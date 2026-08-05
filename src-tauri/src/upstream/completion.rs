//! 一次性文本补全（非流式）。大脑聚合与健康探测的真实补全都走这里。
//!
//! 两个 parse_*_text 提到 pub(super)：session 的 turn 解析器要复用同一套
//! 「SSE 与整包 JSON 都能吃」的解析，两边各写一份必然在某个上游形态上分叉。

use crate::error::{AppError, AppResult};
use crate::model::{Protocol, ProviderKey};
use serde_json::{json, Value};
use std::time::Duration;

use super::client::{
    apply_auth, apply_client_identity, build_client, is_retriable_upstream_error,
    truncate_body, RETRY_BASE_BACKOFF_MS, RETRY_MAX_ATTEMPTS,
};
use super::endpoint::join_endpoint;
use super::usage::record_usage_from_raw;

/// 一次性文本请求（用于聚合成员/决策者/汇总模型调用）。
/// prompt 作为单条 user 消息发送，返回助手文本。
/// `retry` 为 true 时，对临时性上游错误（502/503/504/429/连接失败）自动重试。
///
/// `request_timeout` 为**单次 HTTP 请求**的超时（含上游完整生成时间）。此前硬用
/// `key_timeout`（默认 30s）——那是给代理转发/健康探测的量级；非流式补全要等上游把
/// 整篇内容生成完才返回，30s 必然掐死正常长回答，再叠加 3 次重试 ≈ 91s 全灭，
/// 表象是「error sending request / error decoding response body」而健康探测（max_tokens=1
/// 秒回）始终显示可用。聚合路径应传入 brain 总预算量级的超时。
pub async fn text_completion(
    key: &ProviderKey,
    secret: &str,
    model: &str,
    prompt: &str,
    max_tokens: u32,
    retry: bool,
    request_timeout: Duration,
) -> AppResult<String> {
    let max_attempts = if retry { RETRY_MAX_ATTEMPTS } else { 1 };
    let mut last_err = None;
    for attempt in 1..=max_attempts {
        let client = build_client(key)?;
        let result = match key.protocol {
            Protocol::Anthropic => {
                anthropic_message(&client, key, secret, model, prompt, max_tokens, request_timeout).await
            }
            // 大脑聚合成员探测走 Chat Completions；Responses 上游此处也用 Chat 形态发最小请求
            // （聚合成员探测为尽力而为，非主转发路径）。
            Protocol::OpenaiChat | Protocol::OpenaiResponses => {
                openai_chat(&client, key, secret, model, prompt, max_tokens, request_timeout).await
            }
        };
        match result {
            Ok(text) => return Ok(text),
            Err(e) => {
                // 不可重试错误（鉴权/参数）或已是最后一次：直接返回。
                if attempt >= max_attempts || !is_retriable_upstream_error(&e) {
                    return Err(e);
                }
                last_err = Some(e);
                // 线性退避后重试。
                tokio::time::sleep(std::time::Duration::from_millis(
                    RETRY_BASE_BACKOFF_MS * attempt as u64,
                ))
                .await;
            }
        }
    }
    // 循环必然在上面 return，这里兜底（不应到达）。
    Err(last_err.unwrap_or_else(|| AppError::upstream_msg("未知上游错误")))
}

/// 从原始响应体解析 Anthropic 文本，兼容两种形态：
/// ① 普通 JSON：`{"content":[{"type":"text","text":".."}]}`
/// ② SSE 流：多行 `data: {...}`，逐行取 content_block_delta 的 text 累加。
pub(super) fn parse_anthropic_text(raw: &str) -> Option<String> {
    // 先试普通 JSON
    if let Ok(body) = serde_json::from_str::<Value>(raw) {
        if let Some(text) = anthropic_text_from_json(&body) {
            return Some(text);
        }
    }
    // 再试 SSE：累加 content_block_delta 的 delta.text
    let mut acc = String::new();
    let mut saw_event = false;
    for line in raw.lines() {
        let line = line.trim_start();
        let Some(data) = line.strip_prefix("data:") else { continue };
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        if let Ok(ev) = serde_json::from_str::<Value>(data) {
            saw_event = true;
            if let Some(t) = ev
                .get("delta")
                .and_then(|d| d.get("text"))
                .and_then(|t| t.as_str())
            {
                acc.push_str(t);
            }
        }
    }
    if saw_event {
        Some(acc)
    } else {
        None
    }
}

/// 从已解析的 Anthropic JSON 取 content 里的文本拼接。
fn anthropic_text_from_json(body: &Value) -> Option<String> {
    let arr = body.get("content")?.as_array()?;
    Some(
        arr.iter()
            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join(""),
    )
}

/// 从原始响应体解析 OpenAI 文本，兼容普通 JSON 与 SSE 流（choices[].delta.content）。
pub(super) fn parse_openai_text(raw: &str) -> Option<String> {
    if let Ok(body) = serde_json::from_str::<Value>(raw) {
        if let Some(text) = openai_text_from_json(&body) {
            return Some(text);
        }
    }
    let mut acc = String::new();
    let mut saw_event = false;
    for line in raw.lines() {
        let line = line.trim_start();
        let Some(data) = line.strip_prefix("data:") else { continue };
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        if let Ok(ev) = serde_json::from_str::<Value>(data) {
            saw_event = true;
            if let Some(t) = ev
                .get("choices")
                .and_then(|c| c.as_array())
                .and_then(|arr| arr.first())
                .and_then(|c| c.get("delta"))
                .and_then(|d| d.get("content"))
                .and_then(|c| c.as_str())
            {
                acc.push_str(t);
            }
        }
    }
    if saw_event {
        Some(acc)
    } else {
        None
    }
}

/// 从已解析的 OpenAI JSON 取 choices[0].message.content。
fn openai_text_from_json(body: &Value) -> Option<String> {
    body.get("choices")?
        .as_array()?
        .first()?
        .get("message")?
        .get("content")?
        .as_str()
        .map(|s| s.to_string())
}

async fn anthropic_message(
    client: &reqwest::Client,
    key: &ProviderKey,
    secret: &str,
    model: &str,
    prompt: &str,
    max_tokens: u32,
    request_timeout: Duration,
) -> AppResult<String> {
    let url = join_endpoint(&key.base_url, "/v1/messages");
    let payload = json!({
        "model": model,
        "max_tokens": max_tokens,
        "messages": [{ "role": "user", "content": prompt }]
    });
    // 版本头由 apply_auth 按协议统一添加（见那里的注释），此处不再单独补。
    let mut req = client.post(&url).json(&payload).timeout(request_timeout);
    req = apply_auth(req, key, secret);
    req = apply_client_identity(req, key.protocol);

    let resp = req.send().await?;
    let status = resp.status();
    // 先读文本再解析：上游可能返回 SSE 流（data: {...}）、HTML 错误页或非标准 JSON，
    // 直接 resp.json() 会得到笼统的「error decoding response body」，看不到上游到底返回了啥。
    let raw = resp.text().await?;
    if !status.is_success() {
        // 传**真实状态码**：消息里拼了响应体，绝不能让重试判定去里面猜（见
        // `is_retriable_upstream_error` 的文档：401 + 响应体含「请检查网络连接」曾被误判可重试）。
        return Err(AppError::upstream_http(
            status.as_u16(),
            format!("Anthropic HTTP {status}: {}", truncate_body(&raw)),
        ));
    }
    record_usage_from_raw(&raw);
    // content: [{type:"text", text:"..."}]。兼容普通 JSON 与 SSE 流两种返回形态。
    // 解析失败**不是** HTTP 错误（状态码是 2xx）：status 传 None 会被判为「连接层失败可重试」，
    // 而重试一个格式不对的响应毫无意义。故显式给 status，让它落进「非临时错误」分支。
    let text = parse_anthropic_text(&raw).ok_or_else(|| {
        AppError::upstream_http(
            status.as_u16(),
            format!("Anthropic 响应无法解析: {}", truncate_body(&raw)),
        )
    })?;
    Ok(text)
}

async fn openai_chat(
    client: &reqwest::Client,
    key: &ProviderKey,
    secret: &str,
    model: &str,
    prompt: &str,
    max_tokens: u32,
    request_timeout: Duration,
) -> AppResult<String> {
    let url = join_endpoint(&key.base_url, "/v1/chat/completions");
    let payload = json!({
        "model": model,
        "max_tokens": max_tokens,
        "messages": [{ "role": "user", "content": prompt }]
    });
    let mut req = client.post(&url).json(&payload).timeout(request_timeout);
    req = apply_auth(req, key, secret);
    req = apply_client_identity(req, key.protocol);

    let resp = req.send().await?;
    let status = resp.status();
    let raw = resp.text().await?;
    if !status.is_success() {
        // 传真实状态码，理由同 `anthropic_message`。
        return Err(AppError::upstream_http(
            status.as_u16(),
            format!("OpenAI HTTP {status}: {}", truncate_body(&raw)),
        ));
    }
    record_usage_from_raw(&raw);
    // 解析失败：状态码是 2xx，显式带上以免被当成「连接层失败」而白重试。
    let text = parse_openai_text(&raw).ok_or_else(|| {
        AppError::upstream_http(
            status.as_u16(),
            format!("OpenAI 响应无法解析: {}", truncate_body(&raw)),
        )
    })?;
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_anthropic_plain_json() {
        let raw = r#"{"content":[{"type":"text","text":"你好"},{"type":"text","text":"世界"}]}"#;
        assert_eq!(parse_anthropic_text(raw).as_deref(), Some("你好世界"));
    }

    #[test]
    fn parse_anthropic_sse_stream() {
        // 中转站强制 SSE：逐行 data: 累加 delta.text
        let raw = "event: content_block_delta\n\
                   data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"你\"}}\n\n\
                   data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"好\"}}\n\n\
                   data: [DONE]\n";
        assert_eq!(parse_anthropic_text(raw).as_deref(), Some("你好"));
    }

    #[test]
    fn parse_openai_plain_json() {
        let raw = r#"{"choices":[{"message":{"content":"答案"}}]}"#;
        assert_eq!(parse_openai_text(raw).as_deref(), Some("答案"));
    }

    #[test]
    fn parse_openai_sse_stream() {
        let raw = "data: {\"choices\":[{\"delta\":{\"content\":\"答\"}}]}\n\n\
                   data: {\"choices\":[{\"delta\":{\"content\":\"案\"}}]}\n\n\
                   data: [DONE]\n";
        assert_eq!(parse_openai_text(raw).as_deref(), Some("答案"));
    }

    #[test]
    fn parse_returns_none_on_garbage() {
        // HTML 错误页 / 非 JSON 非 SSE → None，触发上层「响应无法解析」并带原文诊断。
        assert!(parse_anthropic_text("<html>502 Bad Gateway</html>").is_none());
        assert!(parse_openai_text("<html>502 Bad Gateway</html>").is_none());
    }
}
