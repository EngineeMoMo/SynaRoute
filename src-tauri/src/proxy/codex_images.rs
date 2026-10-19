use crate::model::{CategoryType, Protocol};
use crate::store::Store;
use base64::Engine;
use bytes::{Bytes, BytesMut};
use futures_util::{Stream, StreamExt};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;

const MAX_EVENT_BYTES: usize = 32 * 1024 * 1024;
const MAX_IMAGES: usize = 64;

pub(super) fn stream<S, E>(
    upstream: S,
    category: CategoryType,
    protocol: Protocol,
    content_type: &str,
    content_encoding: Option<&str>,
    store: &Store,
) -> impl Stream<Item = io::Result<Bytes>> + Send + 'static
where
    S: Stream<Item = Result<Bytes, E>> + Send + 'static,
    E: std::fmt::Display + Send + 'static,
{
    let directory = enabled(category, protocol, content_type, content_encoding)
        .then(|| PathBuf::from(store.config_path_display()).with_file_name("generated-images"));
    relay(upstream, directory, MAX_EVENT_BYTES)
}

fn enabled(
    category: CategoryType,
    protocol: Protocol,
    content_type: &str,
    content_encoding: Option<&str>,
) -> bool {
    category == CategoryType::Codex
        && protocol == Protocol::OpenaiResponses
        && content_type
            .split(';')
            .next()
            .is_some_and(|value| value.trim().eq_ignore_ascii_case("text/event-stream"))
        && content_encoding.map_or(true, |value| value.eq_ignore_ascii_case("identity"))
}

struct Relay<S> {
    upstream: Pin<Box<S>>,
    pending: BytesMut,
    scanned: usize,
    bridge: Option<ImageBridge>,
    ended: bool,
    error: Option<io::Error>,
    limit: usize,
}

fn relay<S, E>(
    upstream: S,
    directory: Option<PathBuf>,
    limit: usize,
) -> impl Stream<Item = io::Result<Bytes>> + Send
where
    S: Stream<Item = Result<Bytes, E>> + Send + 'static,
    E: std::fmt::Display + Send + 'static,
{
    let state = Relay {
        upstream: Box::pin(upstream),
        pending: BytesMut::new(),
        scanned: 0,
        bridge: directory.map(ImageBridge::new),
        ended: false,
        error: None,
        limit,
    };
    futures_util::stream::unfold(state, |mut state| async move {
        loop {
            if state.bridge.is_none() {
                if !state.pending.is_empty() {
                    return Some((Ok(state.pending.split().freeze()), state));
                }
            } else if let Some(end) = event_end(&state.pending, state.scanned) {
                let event = state.pending.split_to(end).freeze();
                state.scanned = 0;
                if end > state.limit {
                    return Some((Ok(event), state));
                }
                let image_event = event
                    .windows(b"image_generation_call".len())
                    .any(|window| window == b"image_generation_call");
                let terminal_event = state.bridge.as_ref().is_some_and(|bridge| {
                    !bridge.messages.is_empty()
                        && [
                            b"response.completed".as_slice(),
                            b"response.incomplete".as_slice(),
                        ]
                        .iter()
                        .any(|kind| event.windows(kind.len()).any(|window| window == *kind))
                });
                if !image_event && !terminal_event {
                    return Some((Ok(event), state));
                }
                let mut bridge = state.bridge.take().expect("image bridge exists");
                let original = event.clone();
                match tokio::task::spawn_blocking(move || {
                    let output = bridge.rewrite_event(event);
                    (bridge, output)
                })
                .await
                {
                    Ok((bridge, output)) => {
                        state.bridge = Some(bridge);
                        return Some((Ok(output), state));
                    }
                    Err(error) => {
                        tracing::warn!(%error, "Codex 图片兼容处理失败，保留原始事件");
                        return Some((Ok(original), state));
                    }
                }
            }
            if state.pending.len() > state.limit {
                tracing::warn!("Codex SSE 事件过大，图片兼容处理退回原样透传");
                state.bridge = None;
                continue;
            }
            if state.ended {
                if !state.pending.is_empty() {
                    return Some((Ok(state.pending.split().freeze()), state));
                }
                return state.error.take().map(|error| (Err(error), state));
            }
            state.scanned = state.pending.len().saturating_sub(3);
            match state.upstream.next().await {
                Some(Ok(bytes)) if state.bridge.is_none() => return Some((Ok(bytes), state)),
                Some(Ok(bytes)) => state.pending.extend_from_slice(&bytes),
                Some(Err(error)) => {
                    state.error = Some(io::Error::other(error.to_string()));
                    state.ended = true;
                }
                None => state.ended = true,
            }
        }
    })
}

fn event_end(bytes: &[u8], start: usize) -> Option<usize> {
    for index in start..bytes.len() {
        if bytes[index..].starts_with(b"\n\n") {
            return Some(index + 2);
        }
        if bytes[index..].starts_with(b"\r\n\r\n") {
            return Some(index + 4);
        }
    }
    None
}

struct ImageBridge {
    directory: PathBuf,
    messages: HashMap<String, Value>,
    order: Vec<String>,
    emitted: HashSet<String>,
}

impl ImageBridge {
    fn new(directory: PathBuf) -> Self {
        Self {
            directory,
            messages: HashMap::new(),
            order: Vec::new(),
            emitted: HashSet::new(),
        }
    }

    fn rewrite_event(&mut self, original: Bytes) -> Bytes {
        let Ok(text) = std::str::from_utf8(&original) else {
            return original;
        };
        let data = text
            .lines()
            .filter_map(|line| line.strip_prefix("data:"))
            .map(|line| line.strip_prefix(' ').unwrap_or(line))
            .collect::<Vec<_>>()
            .join("\n");
        let Ok(mut event) = serde_json::from_str::<Value>(&data) else {
            return original;
        };
        let mut prefix = Vec::new();
        match event.get("type").and_then(Value::as_str) {
            Some("response.output_item.done") => {
                if let Some(item) = event.get("item") {
                    self.message(item);
                }
                return original;
            }
            Some("response.completed" | "response.incomplete") => {
                let mut sequence = event.get("sequence_number").and_then(Value::as_u64);
                if let Some(output) = event.pointer("/response/output").and_then(Value::as_array) {
                    for item in output {
                        self.message(item);
                    }
                }
                let Some(response) = event.get_mut("response").and_then(Value::as_object_mut)
                else {
                    return original;
                };
                let Some(output) = response
                    .entry("output")
                    .or_insert_with(|| json!([]))
                    .as_array_mut()
                else {
                    return original;
                };
                for id in &self.order {
                    if !self.emitted.insert(id.clone()) {
                        continue;
                    }
                    let message = self.messages[id].clone();
                    let mut done = json!({
                        "type": "response.output_item.done",
                        "output_index": output.len(),
                        "item": message,
                    });
                    output.push(message);
                    if let Some(number) = sequence.as_mut() {
                        done["sequence_number"] = json!(*number);
                        *number = number.saturating_add(1);
                    }
                    prefix.push(done);
                }
                if let Some(sequence) = sequence {
                    event["sequence_number"] = json!(sequence);
                }
            }
            _ => {}
        }
        if prefix.is_empty() {
            return original;
        }
        let mut output = String::new();
        for event in prefix {
            output.push_str("event: response.output_item.done\ndata: ");
            output.push_str(&event.to_string());
            output.push_str("\n\n");
        }
        let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
        let mut replaced = false;
        for line in text.lines() {
            if line.starts_with("data:") {
                if replaced {
                    continue;
                }
                output.push_str("data: ");
                output.push_str(&event.to_string());
                replaced = true;
            } else {
                output.push_str(line);
            }
            output.push_str(newline);
        }
        Bytes::from(output)
    }

    fn message(&mut self, item: &Value) -> Option<(String, Value)> {
        if item.get("type")?.as_str()? != "image_generation_call" {
            return None;
        }
        let id = item.get("id")?.as_str()?;
        if id.len() > 4096 {
            return None;
        }
        if let Some(message) = self.messages.get(id) {
            return Some((id.to_string(), message.clone()));
        }
        if item.get("status").and_then(Value::as_str) != Some("completed")
            || self.messages.len() >= MAX_IMAGES
        {
            return None;
        }
        let result = item.get("result").and_then(Value::as_str).unwrap_or("");
        let text = match save_image(&self.directory, result) {
            Ok(path) => {
                let path = path.to_string_lossy().replace('\\', "/");
                format!("![生成的图片](<{path}>)")
            }
            Err(error) => {
                tracing::warn!(%error, "Codex 生成图片无法保存");
                format!("图片已返回，但无法保存为可显示的本地图片：{error}")
            }
        };
        let digest = format!("{:x}", Sha256::digest(id.as_bytes()));
        let message = json!({
            "type": "message",
            "id": format!("msg_synaroute_image_{}", &digest[..32]),
            "role": "assistant",
            "status": "completed",
            "phase": "final_answer",
            "content": [{ "type": "output_text", "text": text, "annotations": [] }],
        });
        self.messages.insert(id.to_string(), message.clone());
        self.order.push(id.to_string());
        Some((id.to_string(), message))
    }
}

fn save_image(directory: &Path, result: &str) -> Result<PathBuf, String> {
    let encoded = if result.starts_with("data:image/") {
        let (header, data) = result.split_once(',').ok_or("图片 data URL 不完整")?;
        if !header.ends_with(";base64") {
            return Err("图片 data URL 不是 Base64 编码".into());
        }
        data
    } else {
        result
    };
    if encoded.is_empty() || encoded.len() > MAX_EVENT_BYTES {
        return Err("图片数据为空或超过大小限制".into());
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| "图片 Base64 数据无效")?;
    let extension = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        "png"
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        "jpg"
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        "webp"
    } else {
        return Err("图片不是受支持的 PNG、JPEG 或 WebP 格式".into());
    };
    let path = directory.join(format!("{:x}.{extension}", Sha256::digest(&bytes)));
    crate::secret::atomic_write(&path, &bytes).map_err(|error| error.to_string())?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Wl6VtcAAAAASUVORK5CYII=";

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!("synaroute-image-{}", uuid::Uuid::new_v4())))
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn image(id: &str) -> Value {
        json!({ "type": "image_generation_call", "id": id, "status": "completed", "result": PNG })
    }

    fn event(value: Value) -> Bytes {
        Bytes::from(format!(
            "event: {}\ndata: {value}\n\n",
            value["type"].as_str().unwrap()
        ))
    }

    fn parse_events(bytes: &[u8]) -> Vec<Value> {
        std::str::from_utf8(bytes)
            .unwrap()
            .lines()
            .filter_map(|line| line.strip_prefix("data: "))
            .filter_map(|data| serde_json::from_str(data).ok())
            .collect()
    }

    async fn collect(
        chunks: Vec<io::Result<Bytes>>,
        directory: Option<PathBuf>,
        limit: usize,
    ) -> Vec<io::Result<Bytes>> {
        relay(futures_util::stream::iter(chunks), directory, limit)
            .collect()
            .await
    }

    #[tokio::test]
    async fn completed_image_becomes_a_displayable_message_across_every_chunk_boundary() {
        let fixture = Fixture::new();
        let input = event(
            json!({ "type": "response.output_item.done", "output_index": 1, "item": image("ig_first") }),
        );
        let completed = event(
            json!({ "type": "response.completed", "response": { "output": [image("ig_first")] } }),
        );
        let input = Bytes::from([input.as_ref(), completed.as_ref()].concat());
        for split in 0..=input.len() {
            let chunks = collect(
                vec![Ok(input.slice(..split)), Ok(input.slice(split..))],
                Some(fixture.0.clone()),
                MAX_EVENT_BYTES,
            )
            .await;
            let output: Vec<u8> = chunks
                .into_iter()
                .flat_map(|chunk| chunk.unwrap())
                .collect();
            let output = parse_events(&output);
            assert_eq!(output.len(), 3);
            assert_eq!(output[0]["item"], image("ig_first"));
            assert_eq!(output[1]["item"]["type"], "message");
            let text = output[1]["item"]["content"][0]["text"].as_str().unwrap();
            assert!(text.starts_with("![生成的图片](<"));
            assert!(!text.contains(PNG));
            assert_eq!(output[1]["output_index"], 1);
            assert_eq!(output[2]["response"]["output"][0], image("ig_first"));
            assert_eq!(output[1]["item"], output[2]["response"]["output"][1]);
        }
        let saved: Vec<_> = std::fs::read_dir(&fixture.0).unwrap().collect();
        assert_eq!(saved.len(), 1);
        assert_eq!(
            std::fs::read(saved[0].as_ref().unwrap().path()).unwrap(),
            base64::engine::general_purpose::STANDARD
                .decode(PNG)
                .unwrap()
        );
    }

    #[tokio::test]
    async fn completion_only_images_emit_messages_once_and_keep_sequence_numbers() {
        let fixture = Fixture::new();
        let done = event(
            json!({ "type": "response.output_item.done", "item": image("ig_first"), "output_index": 0 }),
        );
        let completed = event(
            json!({ "type": "response.completed", "sequence_number": 9, "response": { "output": [image("ig_first"), image("ig_second")], "usage": { "output_tokens": 3 } } }),
        );
        let result = collect(
            vec![Ok(done), Ok(completed)],
            Some(fixture.0.clone()),
            MAX_EVENT_BYTES,
        )
        .await;
        let output: Vec<u8> = result
            .into_iter()
            .flat_map(|chunk| chunk.unwrap())
            .collect();
        let events = parse_events(&output);
        assert_eq!(events.len(), 4);
        assert_eq!(events[1]["type"], "response.output_item.done");
        assert_eq!(events[1]["output_index"], 2);
        assert_eq!(events[1]["sequence_number"], 9);
        assert_eq!(events[2]["sequence_number"], 10);
        assert_eq!(events[3]["sequence_number"], 11);
        assert_eq!(events[3]["response"]["usage"]["output_tokens"], 3);
        assert_eq!(events[0]["item"], events[3]["response"]["output"][0]);
        assert_eq!(events[1]["item"], events[3]["response"]["output"][2]);
        assert_eq!(events[2]["item"], events[3]["response"]["output"][3]);
    }

    #[tokio::test]
    async fn images_are_displayed_even_when_the_final_output_omits_them() {
        let fixture = Fixture::new();
        let done = event(
            json!({ "type": "response.output_item.done", "item": image("ig_first"), "output_index": 0 }),
        );
        let completed =
            event(json!({ "type": "response.completed", "response": { "output": [] } }));
        let result = collect(
            vec![Ok(done.clone()), Ok(completed)],
            Some(fixture.0.clone()),
            MAX_EVENT_BYTES,
        )
        .await;
        assert_eq!(result[0].as_ref().unwrap(), &done);
        let output: Vec<u8> = result
            .into_iter()
            .flat_map(|chunk| chunk.unwrap())
            .collect();
        let events = parse_events(&output);
        assert_eq!(events.len(), 3);
        assert_eq!(events[1]["item"]["type"], "message");
        assert_eq!(events[1]["output_index"], 0);
        assert_eq!(events[1]["item"], events[2]["response"]["output"][0]);
    }

    #[test]
    fn modified_crlf_events_retain_sse_ids_and_non_data_fields() {
        let fixture = Fixture::new();
        let mut bridge = ImageBridge::new(fixture.0.clone());
        let original = format!("id: event-7\r\nretry: 1000\r\nevent: response.completed\r\ndata: {{\"type\":\"response.completed\",\r\ndata: \"response\":{{\"output\":[{}]}}}}\r\n\r\n", image("ig_crlf"));
        let output = bridge.rewrite_event(Bytes::from(original));
        let text = std::str::from_utf8(&output).unwrap();
        assert!(text.contains("id: event-7\r\nretry: 1000\r\n"));
        assert!(text.ends_with("\r\n\r\n"));
        let events = parse_events(&output);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0]["item"]["type"], "message");
    }

    #[tokio::test]
    async fn ordinary_events_crlf_unicode_and_incomplete_tails_remain_byte_exact() {
        let fixture = Fixture::new();
        let input = Bytes::from_static(b": heartbeat\r\n\r\nevent: response.output_text.delta\r\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"hello\"}\r\n\r\ndata: [DONE]\n\nunfinished");
        let mut chunks: Vec<_> = input
            .iter()
            .map(|byte| Ok(Bytes::copy_from_slice(&[*byte])))
            .collect();
        chunks.push(Ok(Bytes::from("小猫")));
        let result = collect(chunks, Some(fixture.0.clone()), MAX_EVENT_BYTES).await;
        let output: Vec<u8> = result
            .into_iter()
            .flat_map(|chunk| chunk.unwrap())
            .collect();
        assert_eq!(output, [input.as_ref(), "小猫".as_bytes()].concat());
        assert!(!fixture.0.exists());
    }

    #[tokio::test]
    async fn disabled_and_oversized_streams_pass_through_without_writing_files() {
        let fixture = Fixture::new();
        let input =
            event(json!({ "type": "response.output_item.done", "item": image("ig_first") }));
        for directory in [None, Some(fixture.0.clone())] {
            let result = collect(
                vec![Ok(input.slice(..40)), Ok(input.slice(40..))],
                directory,
                20,
            )
            .await;
            let output: Vec<u8> = result
                .into_iter()
                .flat_map(|chunk| chunk.unwrap())
                .collect();
            assert_eq!(output, input);
            assert!(!fixture.0.exists());
        }
    }

    #[tokio::test]
    async fn upstream_failure_follows_pending_bytes_instead_of_losing_them() {
        let fixture = Fixture::new();
        let result = collect(
            vec![
                Ok(Bytes::from("data: partial")),
                Err(io::Error::other("disconnected")),
            ],
            Some(fixture.0.clone()),
            MAX_EVENT_BYTES,
        )
        .await;
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].as_ref().unwrap(), "data: partial");
        assert!(result[1]
            .as_ref()
            .unwrap_err()
            .to_string()
            .contains("disconnected"));
    }

    #[test]
    fn invalid_images_and_unwritable_directories_produce_visible_errors() {
        let fixture = Fixture::new();
        let mut bridge = ImageBridge::new(fixture.0.clone());
        let mut bad = image("ig_invalid");
        bad["result"] = json!("not base64");
        let (_, message) = bridge.message(&bad).unwrap();
        let text = message["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("Base64"));
        assert!(!text.contains("!["));
        assert!(!fixture.0.exists());
        std::fs::write(&fixture.0, "not a directory").unwrap();
        let (_, message) = bridge.message(&image("ig_unwritable")).unwrap();
        assert!(message["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("无法保存"));
        std::fs::remove_file(&fixture.0).unwrap();
    }

    #[test]
    fn file_names_cannot_be_controlled_by_upstream_ids_and_data_urls_are_supported() {
        let fixture = Fixture::new();
        let mut bridge = ImageBridge::new(fixture.0.clone());
        let mut item = image("../../escape");
        item["result"] = json!(format!("data:image/png;base64,{PNG}"));
        let (_, message) = bridge.message(&item).unwrap();
        assert!(!message["id"].as_str().unwrap().contains(".."));
        assert!(message["id"].as_str().unwrap().len() <= 64);
        let saved = std::fs::read_dir(&fixture.0)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert_eq!(saved.parent(), Some(fixture.0.as_path()));
        assert_eq!(saved.extension().unwrap(), "png");
    }

    #[test]
    fn compatibility_is_limited_to_uncompressed_codex_responses_sse() {
        assert!(enabled(
            CategoryType::Codex,
            Protocol::OpenaiResponses,
            "text/event-stream; charset=utf-8",
            None
        ));
        for (category, protocol, content_type, encoding) in [
            (
                CategoryType::ClaudeCli,
                Protocol::OpenaiResponses,
                "text/event-stream",
                None,
            ),
            (
                CategoryType::Codex,
                Protocol::Anthropic,
                "text/event-stream",
                None,
            ),
            (
                CategoryType::Codex,
                Protocol::OpenaiResponses,
                "application/json",
                None,
            ),
            (
                CategoryType::Codex,
                Protocol::OpenaiResponses,
                "text/event-stream",
                Some("gzip"),
            ),
        ] {
            assert!(!enabled(category, protocol, content_type, encoding));
        }
        let source = include_str!("../proxy.rs");
        assert!(source.contains("codex_images::stream(crate::upstream::guard_stream_idle(resp.bytes_stream(), permit), category, downstream, &content_type, upstream_content_encoding.as_deref(), store)"));
    }
}
