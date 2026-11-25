
use super::{sse, sse_data, SseDirection, SseTranslator};
use serde_json::{json, Value};

/// 上游错误消息的截断上限。上游可能把整个 HTML 错误页塞进 message。
const MSG_CAP: usize = 500;

pub(crate) fn declares_error(json: &Value) -> bool {
    matches!(json.get("type").and_then(Value::as_str), Some("error" | "response.failed"))
        || json.pointer("/response/status").and_then(Value::as_str) == Some("failed")
        || (json.get("object").and_then(Value::as_str) == Some("response")
            && json.get("status").and_then(Value::as_str) == Some("failed"))
}

/// 这个 SSE 载荷是不是「上游报错」？是则返回可读消息。
///
/// 三家协议的形态各不相同，判据刻意都收在这里（而不是散进六个方向函数）：
/// - Anthropic：`{"type":"error","error":{"type":…,"message":…}}`
/// - Chat：`{"error":{"message":…}}`（顶层 `error` 对象）
/// - Responses：`{"type":"error","message":…}` 或 `{"type":"response.failed","response":{"error":…}}`
///
pub(crate) fn upstream_error_message(json: &Value) -> Option<String> {
    let err = json.get("error");
    let is_err = declares_error(json)
        || err.is_some_and(|value| !value.is_null());
    if !is_err {
        return None;
    }
    // 取消息：从最具体的位置往外退，最后兜底整段 JSON（截断）。
    let raw = err
        .and_then(|e| e.get("message"))
        .and_then(Value::as_str)
        .or_else(|| json.get("message").and_then(Value::as_str))
        .or_else(|| {
            json.get("response")
                .and_then(|r| r.get("error"))
                .and_then(|e| e.get("message"))
                .and_then(Value::as_str)
        })
        .or_else(|| err.and_then(Value::as_str))
        .or_else(|| err.and_then(|e| e.get("type")).and_then(Value::as_str))
        .map(str::to_string)
        .unwrap_or_else(|| json.to_string());
    // 按字符边界截，避免切在多字节中间（同 proxy.rs 的 snippet 那条教训）。
    let msg = if raw.chars().count() > MSG_CAP {
        raw.chars().take(MSG_CAP).collect::<String>() + "…"
    } else {
        raw
    };
    Some(msg)
}

impl SseTranslator {
    /// 命中上游错误就翻成**下游协议**的错误事件，并抑制随后的收尾事件。
    ///
    /// 返回 `None` = 这不是错误，调用方继续走正常的六方向分派。
    pub(super) fn translate_upstream_error(&mut self, json: &Value) -> Option<String> {
        let msg = upstream_error_message(json)?;
        self.status.failed = true;
        let out = match self.dir {
            // 下游 Anthropic：与上游 Anthropic 的流内错误、以及 `stream_idle` 注入的那条
            // 形态完全一致 —— 客户端本就要处理它，零新增契约。
            SseDirection::ChatToAnthropic | SseDirection::ResponsesToAnthropic => sse(
                "error",
                &json!({ "type": "error", "error": { "type": "api_error", "message": msg } }),
            ),
            SseDirection::ResponsesToChat | SseDirection::AnthropicToChat => {
                sse_data(&json!({ "error": { "type": "api_error", "message": msg } }))
            }
            // 下游 Responses：用 `response.failed`（Responses 事件集里的正式成员），
            // 不用裸 `error` —— 后者在 Codex 的解析器里没有对应分支的把握更低。
            //
            // ⚠️ **这一支只做过静态设计、没有真机取证**（Codex 侧解析器行为）。
            SseDirection::ChatToResponses | SseDirection::AnthropicToResponses => sse(
                "response.failed",
                &json!({
                    "type": "response.failed",
                    "response": {
                        "id": self.resp_id, "object": "response", "status": "failed",
                        "model": self.model,
                        "error": { "code": "api_error", "message": msg }
                    }
                }),
            ),
        };
        // 🔴 抑制收尾：见模块头「为什么不需要新字段」。
        self.started = false;
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tr(dir: SseDirection) -> SseTranslator {
        SseTranslator::new(dir)
    }

    /// 三家协议的错误形态都要认出来。
    #[test]
    fn the_three_upstream_error_shapes_are_recognized() {
        // Anthropic
        let m = upstream_error_message(&json!({
            "type": "error", "error": { "type": "overloaded_error", "message": "Overloaded" }
        }));
        assert_eq!(m.as_deref(), Some("Overloaded"));
        // Chat（顶层 error 对象）
        let m = upstream_error_message(&json!({ "error": { "message": "rate limited" } }));
        assert_eq!(m.as_deref(), Some("rate limited"));
        // Responses：顶层 message
        let m = upstream_error_message(&json!({ "type": "error", "message": "bad request" }));
        assert_eq!(m.as_deref(), Some("bad request"));
        // Responses：response.failed
        let m = upstream_error_message(&json!({
            "type": "response.failed",
            "response": { "error": { "message": "model overloaded" } }
        }));
        assert_eq!(m.as_deref(), Some("model overloaded"));
        // 只有 error.type 没有 message 时退到 type
        let m = upstream_error_message(&json!({ "type": "error", "error": { "type": "api_error" } }));
        assert_eq!(m.as_deref(), Some("api_error"));
    }

    /// 🔴 正常增量不许被误判成错误。
    ///
    /// 误判的代价是把一条好的对话当场掐断，比漏判严重得多 —— 有些上游在正常 chunk 里
    /// 带 `"error": null` 占位，那是本判据唯一现实的假阳性来源。
    #[test]
    fn normal_chunks_are_never_mistaken_for_errors() {
        for ok in [
            json!({ "object":"response", "status":"completed", "error":null }),
            json!({ "object":"response", "status":"incomplete", "error":null }),
            json!({ "status":"failed", "message":"ordinary application data" }),
            json!({ "choices": [ { "delta": { "content": "hi" } } ] }),
            json!({ "error": null }),
            json!({ "type": "content_block_delta", "delta": { "text": "hi" } }),
            json!({ "type": "response.output_text.delta", "delta": "hi" }),
            // 「error」只是文本内容的一部分，不是结构
            json!({ "choices": [ { "delta": { "content": "an error occurred" } } ] }),
        ] {
            assert_eq!(upstream_error_message(&ok), None, "误判：{ok}");
        }
    }

    #[test]
    fn reference_compat_failed_response_without_an_error_object_is_recognized() {
        for error in [Value::Null, json!({"message":"capacity exceeded"})] {
            let response = json!({"object":"response", "status":"failed", "error":error});
            assert!(upstream_error_message(&response).is_some());
        }
    }

    /// 每个下游协议都要收到**它自己认得的**错误形态。
    #[test]
    fn each_downstream_protocol_gets_its_own_error_shape() {
        let e = json!({ "type": "error", "error": { "message": "boom" } });

        let mut t = tr(SseDirection::ChatToAnthropic);
        let out = t.translate_upstream_error(&e).expect("该命中");
        assert!(out.starts_with("event: error"), "{out}");
        assert!(out.contains("\"type\":\"error\"") && out.contains("boom"), "{out}");

        let mut t = tr(SseDirection::AnthropicToChat);
        let out = t.translate_upstream_error(&e).expect("该命中");
        assert!(out.starts_with("data: {"), "Chat 流不用具名事件：{out}");
        assert!(out.contains("\"error\"") && out.contains("boom"), "{out}");

        let mut t = tr(SseDirection::AnthropicToResponses);
        let out = t.translate_upstream_error(&e).expect("该命中");
        assert!(out.starts_with("event: response.failed"), "{out}");
        assert!(out.contains("\"status\":\"failed\"") && out.contains("boom"), "{out}");
    }

    /// 🔴 发了错误之后**不许再冲刷收尾事件**。
    ///
    /// 下游先收到「失败」再收到 `response.completed` / `message_stop` 是自相矛盾的，
    /// 而客户端多半以后者为准 —— 于是又变回「成功完成的空回答」，等于没修。
    #[test]
    fn an_error_suppresses_the_completion_flush() {
        let e = json!({ "type": "error", "error": { "message": "boom" } });

        // Responses 下游：先喂一个正常增量把 started 置起来，再报错，随后 finish() 必须为空
        let mut t = tr(SseDirection::AnthropicToResponses);
        let _ = t.push(b"data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\n");
        assert!(!t.translate_upstream_error(&e).unwrap().is_empty());
        assert_eq!(t.finish(), "", "错误之后不许再发 response.completed");

        // Anthropic 下游同理
        let mut t = tr(SseDirection::ChatToAnthropic);
        let _ = t.push(b"data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n");
        assert!(!t.translate_upstream_error(&e).unwrap().is_empty());
        assert_eq!(t.finish(), "", "错误之后不许再发 message_stop");

        let mut t = tr(SseDirection::AnthropicToChat);
        assert!(!t.translate_upstream_error(&e).unwrap().is_empty());
        assert_eq!(t.finish(), "", "错误后不合成正常终态");
    }

    /// 上游把整个 HTML 错误页塞进 message 时要截断（按字符边界，不切裂多字节）。
    #[test]
    fn a_huge_upstream_message_is_capped_on_a_char_boundary() {
        let long = "中".repeat(MSG_CAP + 200);
        let m = upstream_error_message(&json!({ "error": { "message": long } })).unwrap();
        assert_eq!(m.chars().count(), MSG_CAP + 1, "截断后加一个省略号");
        assert!(m.ends_with('…'));
    }

    /// 🔴 接线判据：`process_line` 必须在六方向分派**之前**过这一道。
    ///
    /// 上面几条都直接调 `translate_upstream_error`，把 `sse.rs` 里那一行删掉它们照样全绿,
    /// 而那正是「跨协议下错误被整个丢掉」这个缺陷本身。第 12 次同类接线盲区。
    #[test]
    fn process_line_must_check_for_errors_before_dispatching() {
        let src = std::fs::read_to_string("src/upstream/sse.rs").unwrap();
        let prod = crate::proxy::custom_headers::production_code_only(&src);
        let hook = prod
            .find("self.translate_upstream_error(json)")
            .expect("process_line 必须先过错误翻译这一道");
        let dispatch = prod
            .find("SseDirection::ChatToResponses => Some(self.chat_chunk_to_responses")
            .expect("找不到六方向分派 —— 判据失去参照物，先修判据");
        assert!(hook < dispatch, "错误翻译必须排在六方向分派之前，否则错误会先被当成正常事件吃掉");
    }
}
