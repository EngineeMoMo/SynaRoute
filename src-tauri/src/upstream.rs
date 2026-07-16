//! 上游厂商通信 + 协议适配层。
//!
//! MVP 范围（arch-decisions §10）：
//! - 非流式：Anthropic Messages ↔ OpenAI Chat Completions 双向字段转换。
//! - 拉取模型：兼容 Anthropic /v1/models 与 OpenAI /v1/models。
//! - 简单文本请求用于健康检查与聚合成员调用。
//! 流式 SSE 的完整转发在 proxy 模块处理；此处提供非流式的一次性调用。

use crate::error::{AppError, AppResult};
use crate::model::{Protocol, ProviderKey};
use serde_json::{json, Value};
use std::time::Duration;

/// 拉取模型列表（FR-004）。返回真实模型名数组。
pub async fn fetch_models(key: &ProviderKey, secret: &str) -> AppResult<Vec<String>> {
    let client = build_client(key)?;
    let url = join_url(&key.base_url, "/v1/models");

    let mut req = client.get(&url);
    req = apply_auth(req, key, secret);

    let resp = req.send().await?;
    if !resp.status().is_success() {
        return Err(AppError::Upstream(format!(
            "拉取模型失败 HTTP {}",
            resp.status()
        )));
    }
    let body: Value = resp.json().await?;

    // 兼容两种返回结构：{data:[{id}]}（OpenAI）与 {data:[{id/model}]}（Anthropic 亦为 data[].id）
    let mut names = vec![];
    if let Some(arr) = body.get("data").and_then(|d| d.as_array()) {
        for item in arr {
            if let Some(id) = item.get("id").and_then(|v| v.as_str()) {
                names.push(id.to_string());
            }
        }
    }
    Ok(names)
}

/// 一次性文本请求（用于聚合成员/决策者/汇总模型调用）。
/// prompt 作为单条 user 消息发送，返回助手文本。
pub async fn text_completion(
    key: &ProviderKey,
    secret: &str,
    model: &str,
    prompt: &str,
    max_tokens: u32,
) -> AppResult<String> {
    let client = build_client(key)?;
    match key.protocol {
        Protocol::Anthropic => anthropic_message(&client, key, secret, model, prompt, max_tokens).await,
        Protocol::Openai => openai_chat(&client, key, secret, model, prompt, max_tokens).await,
    }
}

async fn anthropic_message(
    client: &reqwest::Client,
    key: &ProviderKey,
    secret: &str,
    model: &str,
    prompt: &str,
    max_tokens: u32,
) -> AppResult<String> {
    let url = join_url(&key.base_url, "/v1/messages");
    let payload = json!({
        "model": model,
        "max_tokens": max_tokens,
        "messages": [{ "role": "user", "content": prompt }]
    });
    let mut req = client.post(&url).json(&payload);
    req = req.header("anthropic-version", "2023-06-01");
    req = apply_auth(req, key, secret);

    let resp = req.send().await?;
    let status = resp.status();
    let body: Value = resp.json().await?;
    if !status.is_success() {
        return Err(AppError::Upstream(format!("Anthropic HTTP {status}: {body}")));
    }
    // content: [{type:"text", text:"..."}]
    let text = body
        .get("content")
        .and_then(|c| c.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default();
    Ok(text)
}

async fn openai_chat(
    client: &reqwest::Client,
    key: &ProviderKey,
    secret: &str,
    model: &str,
    prompt: &str,
    max_tokens: u32,
) -> AppResult<String> {
    let url = join_url(&key.base_url, "/v1/chat/completions");
    let payload = json!({
        "model": model,
        "max_tokens": max_tokens,
        "messages": [{ "role": "user", "content": prompt }]
    });
    let mut req = client.post(&url).json(&payload);
    req = apply_auth(req, key, secret);

    let resp = req.send().await?;
    let status = resp.status();
    let body: Value = resp.json().await?;
    if !status.is_success() {
        return Err(AppError::Upstream(format!("OpenAI HTTP {status}: {body}")));
    }
    let text = body
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|arr| arr.first())
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or_default()
        .to_string();
    Ok(text)
}

/// 轻量健康探测：优先用 /v1/models（成本低）。返回是否可用 + 延迟毫秒。
pub async fn health_probe(key: &ProviderKey, secret: &str) -> (bool, u64) {
    let start = std::time::Instant::now();
    let ok = fetch_models(key, secret).await.is_ok();
    let latency = start.elapsed().as_millis() as u64;
    (ok, latency)
}

// ---- 协议字段转换（proxy 跨协议故障转移时使用）----

/// 将 Anthropic Messages 请求体转为 OpenAI Chat Completions 请求体（MVP：纯文本消息）。
pub fn anthropic_to_openai(body: &Value) -> Value {
    let model = body.get("model").cloned().unwrap_or(Value::Null);
    let max_tokens = body.get("max_tokens").cloned().unwrap_or(json!(4096));
    let mut messages = vec![];
    // system 字段 → system 消息
    if let Some(sys) = body.get("system").and_then(|s| s.as_str()) {
        messages.push(json!({ "role": "system", "content": sys }));
    }
    if let Some(arr) = body.get("messages").and_then(|m| m.as_array()) {
        for m in arr {
            let role = m.get("role").and_then(|r| r.as_str()).unwrap_or("user");
            let content = extract_text_content(m.get("content"));
            messages.push(json!({ "role": role, "content": content }));
        }
    }
    json!({ "model": model, "max_tokens": max_tokens, "messages": messages })
}

/// 将 OpenAI Chat Completions 请求体转为 Anthropic Messages 请求体（MVP：纯文本消息）。
pub fn openai_to_anthropic(body: &Value) -> Value {
    let model = body.get("model").cloned().unwrap_or(Value::Null);
    let max_tokens = body.get("max_tokens").cloned().unwrap_or(json!(4096));
    let mut system = String::new();
    let mut messages = vec![];
    if let Some(arr) = body.get("messages").and_then(|m| m.as_array()) {
        for m in arr {
            let role = m.get("role").and_then(|r| r.as_str()).unwrap_or("user");
            let content = extract_text_content(m.get("content"));
            if role == "system" {
                system.push_str(&content);
            } else {
                messages.push(json!({ "role": role, "content": content }));
            }
        }
    }
    let mut out = json!({ "model": model, "max_tokens": max_tokens, "messages": messages });
    if !system.is_empty() {
        out["system"] = json!(system);
    }
    out
}

/// content 可能是字符串或分块数组，统一抽取为纯文本（MVP 仅文本）。
fn extract_text_content(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

// ---- 内部工具 ----

fn build_client(key: &ProviderKey) -> AppResult<reqwest::Client> {
    let timeout = key.params.timeout_ms.unwrap_or(30_000);
    reqwest::Client::builder()
        .timeout(Duration::from_millis(timeout))
        .build()
        .map_err(|e| AppError::Upstream(e.to_string()))
}

/// 按协议注入鉴权头
fn apply_auth(
    req: reqwest::RequestBuilder,
    key: &ProviderKey,
    secret: &str,
) -> reqwest::RequestBuilder {
    match key.protocol {
        Protocol::Anthropic => req.header("x-api-key", secret),
        Protocol::Openai => req.header("authorization", format!("Bearer {secret}")),
    }
}

fn join_url(base: &str, path: &str) -> String {
    let base = base.trim_end_matches('/');
    // 若 base 已含 /v1，避免重复
    if base.ends_with("/v1") && path.starts_with("/v1/") {
        format!("{}{}", base, &path[3..])
    } else {
        format!("{}{}", base, path)
    }
}
