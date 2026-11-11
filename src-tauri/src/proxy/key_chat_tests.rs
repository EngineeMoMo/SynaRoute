use super::*;
use http_body_util::{BodyExt, Full};
use hyper::{body::Incoming, service::service_fn, Request, Response};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct Captured {
    path: String,
    headers: hyper::HeaderMap,
    body: Value,
}

async fn mock(
    status: u16,
    body: Value,
) -> (
    String,
    oneshot::Receiver<Captured>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (sender, receiver) = oneshot::channel();
    let sender = Arc::new(parking_lot::Mutex::new(Some(sender)));
    let task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let service = service_fn(move |request: Request<Incoming>| {
            let sender = sender.clone();
            let body = body.clone();
            async move {
                let (parts, incoming) = request.into_parts();
                let raw = incoming.collect().await.unwrap().to_bytes();
                sender
                    .lock()
                    .take()
                    .unwrap()
                    .send(Captured {
                        path: parts.uri.path().into(),
                        headers: parts.headers,
                        body: serde_json::from_slice(&raw).unwrap(),
                    })
                    .ok();
                Ok::<_, std::convert::Infallible>(
                    Response::builder()
                        .status(status)
                        .header("Content-Type", "application/json")
                        .body(Full::new(bytes::Bytes::from(body.to_string())))
                        .unwrap(),
                )
            }
        });
        let _ = hyper::server::conn::http1::Builder::new()
            .keep_alive(false)
            .serve_connection(TokioIo::new(stream), service)
            .await;
    });
    (url, receiver, task)
}

fn store(url: &str, protocol: Protocol) -> (tempfile::TempDir, Arc<Store>, ChatRequest) {
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(
        Store::new_at(
            directory.path().join("config.json"),
            directory.path().join("secrets.enc"),
        )
        .unwrap(),
    );
    let mut settings = store.get_settings();
    settings.log_dir = Some(directory.path().join("logs").to_string_lossy().into_owned());
    store
        .save_settings(crate::model::UserPrefs::from(&settings))
        .unwrap();
    store
        .upsert_key(ProviderKey {
            id: "selected".into(),
            category_id: CategoryType::Codex,
            protocol,
            name: "Selected key".into(),
            base_url: url.into(),
            enabled: false,
            has_secret: true,
            headers_json: Some(r#"{"X-Tenant":"synthetic-header-credential"}"#.into()),
            ..Default::default()
        })
        .unwrap();
    store
        .save_credential("selected", "synthetic-selected-credential")
        .unwrap();
    let request = ChatRequest {
        key_id: "selected".into(),
        base_url: url.into(),
        protocol,
        model: "test-model".into(),
        messages: vec![
            Message {
                role: Role::User,
                content: "synthetic-first-prompt".into(),
            },
            Message {
                role: Role::Assistant,
                content: "synthetic-previous-answer".into(),
            },
            Message {
                role: Role::User,
                content: "synthetic-follow-up".into(),
            },
        ],
    };
    (directory, store, request)
}

#[tokio::test]
async fn all_protocols_preserve_multiturn_history_and_use_only_selected_key() {
    let _serial = SERIAL.lock().await;
    for protocol in [
        Protocol::Anthropic,
        Protocol::OpenaiChat,
        Protocol::OpenaiResponses,
    ] {
        let reply_text =
            "synthetic-reply synthetic-selected-credential synthetic-header-credential";
        let (path, reply) = match protocol {
            Protocol::Anthropic => (
                "/v1/messages",
                json!({"type":"message", "role":"assistant", "content":[{"type":"text","text":reply_text}],"stop_reason":"end_turn","usage":{"input_tokens":3,"output_tokens":4}}),
            ),
            Protocol::OpenaiChat => (
                "/v1/chat/completions",
                json!({"choices":[{"message":{"role":"assistant","content":reply_text},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":4}}),
            ),
            Protocol::OpenaiResponses => (
                "/v1/responses",
                json!({"id":"resp_test","object":"response","status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":reply_text}]}],"usage":{"input_tokens":3,"output_tokens":4}}),
            ),
        };
        let (url, received, server) = mock(200, reply).await;
        let (_directory, store, request) = store(&url, protocol);
        let before = serde_json::to_value(store.snapshot_config()).unwrap();
        let outcome = run(store.clone(), request, |started| {
            assert_eq!(started.category_id, CategoryType::Codex);
            assert!(!started.request_id.is_empty());
            Ok(())
        })
        .await
        .unwrap();
        let captured = received.await.unwrap();
        assert_eq!(captured.path, path);
        assert_eq!(
            captured.headers[protocol.auth_scheme().header_name()],
            protocol
                .auth_scheme()
                .header_value("synthetic-selected-credential")
        );
        assert_eq!(captured.headers["X-Tenant"], "synthetic-header-credential");
        let messages = if protocol == Protocol::OpenaiResponses {
            &captured.body["input"]
        } else {
            &captured.body["messages"]
        };
        assert_eq!(messages.as_array().unwrap().len(), 3);
        assert_eq!(messages[1]["role"], "assistant");
        assert!(messages.to_string().contains("synthetic-follow-up"));
        if protocol.is_openai() {
            assert_eq!(captured.body["store"], false);
        }
        assert_eq!(outcome.key_id, "selected");
        assert_eq!(outcome.model, "test-model");
        assert!(!outcome.text.contains("synthetic-selected-credential"));
        assert!(!outcome.text.contains("synthetic-header-credential"));
        assert!(outcome.text.contains("[REDACTED]"));
        assert_eq!(outcome.usage.unwrap().input, 3);
        assert_eq!(outcome.usage.unwrap().output, 4);
        assert_eq!(
            serde_json::to_value(store.snapshot_config()).unwrap(),
            before
        );
        assert!(store.active_requests.list(CategoryType::Codex).is_empty());
        let logs = serde_json::to_string(&store.list_all_events()).unwrap();
        for private in [
            "synthetic-first-prompt",
            "synthetic-follow-up",
            "synthetic-reply",
            "synthetic-selected-credential",
        ] {
            assert!(!logs.contains(private));
        }
        assert_eq!(store.token_usage_by_key()[0].key_id, "selected");
        server.await.unwrap();
    }
}

#[tokio::test]
async fn cancellation_before_start_ack_does_not_send_a_request() {
    let _serial = SERIAL.lock().await;
    let (url, mut received, server) = mock(200, json!({})).await;
    let (_directory, store, request) = store(&url, Protocol::OpenaiChat);
    let outcome = run(store.clone(), request, |started| {
        assert_eq!(
            store
                .active_requests
                .stop(started.category_id, &started.request_id),
            super::super::active_requests::StopResult::Requested
        );
        Ok(())
    })
    .await;
    assert!(outcome.unwrap_err().to_string().contains("停止"));
    assert!(received.try_recv().is_err());
    assert!(store.active_requests.list(CategoryType::Codex).is_empty());
    server.abort();
}

#[tokio::test]
async fn cancellation_drops_an_inflight_upstream_request() {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
    let _serial = SERIAL.lock().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (accepted_sender, accepted_receiver) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buffer = [0; 8192];
        let mut reader = BufReader::new(&mut stream);
        let mut length = None;
        loop {
            let mut line = String::new();
            assert!(reader.read_line(&mut line).await.unwrap() > 0);
            if line == "\r\n" {
                break;
            }
            if let Some((name, value)) = line.split_once(':') {
                if name.eq_ignore_ascii_case("content-length") {
                    length = Some(value.trim().parse::<usize>().unwrap());
                }
            }
        }
        let mut body = vec![0; length.expect("JSON request has a content length")];
        reader.read_exact(&mut body).await.unwrap();
        drop(reader);
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 100000\r\n\r\n{").await.unwrap();
        accepted_sender.send(()).unwrap();
        let closed = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut buffer))
            .await
            .unwrap();
        assert!(matches!(closed, Ok(0) | Err(_)));
    });
    let (_directory, store, request) = store(&url, Protocol::OpenaiChat);
    let worker_store = store.clone();
    let (started_sender, started_receiver) = oneshot::channel();
    let worker = tokio::spawn(async move {
        run(worker_store, request, |started| {
            started_sender.send(started).ok();
            Ok(())
        })
        .await
    });
    let started = started_receiver.await.unwrap();
    accepted_receiver.await.unwrap();
    store
        .active_requests
        .stop(started.category_id, &started.request_id);
    assert!(tokio::time::timeout(Duration::from_secs(2), worker)
        .await
        .unwrap()
        .unwrap()
        .is_err());
    assert!(store.active_requests.list(CategoryType::Codex).is_empty());
    server.await.unwrap();
}

#[tokio::test]
async fn errors_are_redacted_and_do_not_fall_back_to_another_key() {
    let _serial = SERIAL.lock().await;
    let (url, received, server) = mock(401, json!({"error":{"message":"synthetic-selected-credential synthetic-header-credential rejected"}})).await;
    let (other_url, mut other_received, other_server) = mock(200, json!({})).await;
    let (_directory, store, request) = store(&url, Protocol::OpenaiChat);
    store
        .upsert_key(ProviderKey {
            id: "other".into(),
            base_url: other_url,
            enabled: true,
            ..Default::default()
        })
        .unwrap();
    store
        .save_credential("other", "synthetic-other-credential")
        .unwrap();
    let error = run(store, request, |_| Ok(()))
        .await
        .unwrap_err()
        .to_string();
    received.await.unwrap();
    assert!(error.contains("401"));
    assert!(!error.contains("synthetic-selected-credential"));
    assert!(!error.contains("synthetic-header-credential"));
    assert!(other_received.try_recv().is_err());
    other_server.abort();
    server.await.unwrap();
}

#[tokio::test]
async fn changed_config_and_oversized_history_are_rejected_before_network() {
    let _serial = SERIAL.lock().await;
    let (url, mut received, server) = mock(200, json!({})).await;
    for change in 0..4 {
        let (_directory, store, mut request) = store(&url, Protocol::OpenaiChat);
        match change {
            0 => request.base_url = "https://changed.example".into(),
            1 => request.messages[0].content = "a".repeat(MAX_MESSAGE_BYTES + 1),
            2 => request.messages[0].role = Role::Assistant,
            _ => store.remove_credential("selected").unwrap(),
        }
        assert!(run(store.clone(), request, |_| panic!(
            "invalid request must not begin"
        ))
        .await
        .is_err());
        assert!(store.active_requests.list(CategoryType::Codex).is_empty());
    }
    assert!(received.try_recv().is_err());
    server.abort();
}
