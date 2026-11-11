use super::active_requests::{End, Phase};
use crate::error::{AppError, AppResult};
use crate::model::{CategoryType, Protocol, ProviderKey, TokenUsage};
use crate::store::Store;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::{Duration, Instant};

const MAX_MESSAGES: usize = 63;
const MAX_MESSAGE_BYTES: usize = 32 * 1024;
const MAX_HISTORY_BYTES: usize = 128 * 1024;
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
static CONCURRENT_TESTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Role {
    User,
    Assistant,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct Message {
    pub role: Role,
    pub content: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ChatRequest {
    pub key_id: String,
    pub base_url: String,
    pub protocol: Protocol,
    pub model: String,
    pub messages: Vec<Message>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Started {
    pub request_id: String,
    pub category_id: CategoryType,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ChatReply {
    pub key_id: String,
    pub model: String,
    pub protocol: Protocol,
    pub text: String,
    pub latency_ms: u64,
    pub usage: Option<TokenUsage>,
    pub truncated: bool,
}

fn validate(request: &ChatRequest, key: &ProviderKey) -> AppResult<String> {
    if request.base_url != key.base_url || request.protocol != key.protocol {
        return Err(AppError::Invalid("Key 接口配置已变化，请关闭测试窗口后重新打开，避免将会话发往其他接口。 / Key endpoint changed; reopen the test window.".into()));
    }
    let model = key.resolve_model(request.model.trim());
    if model.trim().is_empty() || model.len() > 200 || model.chars().any(char::is_control) {
        return Err(AppError::Invalid(
            "请输入有效的测试模型名。 / Enter a valid model name.".into(),
        ));
    }
    let mut size = 0usize;
    if request.messages.is_empty()
        || request.messages.len() > MAX_MESSAGES
        || request.messages.len() % 2 != 1
    {
        return Err(AppError::Invalid("会话需以用户消息开始和结束，最多 31 轮历史；请清空后重试。 / Invalid conversation or history limit reached.".into()));
    }
    for (index, message) in request.messages.iter().enumerate() {
        let expected = if index % 2 == 0 {
            Role::User
        } else {
            Role::Assistant
        };
        size = size.saturating_add(message.content.len());
        if message.role != expected
            || message.content.trim().is_empty()
            || message.content.len() > MAX_MESSAGE_BYTES
            || size > MAX_HISTORY_BYTES
        {
            return Err(AppError::Invalid("会话顺序无效或内容过长：每条最多 32 KiB，总计最多 128 KiB。 / Invalid or oversized conversation.".into()));
        }
    }
    Ok(model)
}

fn redact(text: &str, key: &ProviderKey, secret: &str) -> String {
    let mut safe = if secret.is_empty() {
        text.to_owned()
    } else {
        text.replace(secret, "[REDACTED]")
    };
    for (_, value) in super::custom_headers::headers_for(key) {
        if !value.is_empty() {
            safe = safe.replace(&value, "[REDACTED]");
        }
    }
    safe
}

async fn send(
    key: &ProviderKey,
    secret: &str,
    model: &str,
    messages: &[Message],
    control: &super::active_requests::Control,
) -> AppResult<ChatReply> {
    let mut body =
        json!({"model": model, "messages": messages, "stream": false, "max_tokens": 2048});
    super::apply_key_params(&mut body, &key.params, key.protocol);
    let mut body = crate::upstream::convert_request_owned(body, Protocol::OpenaiChat, key.protocol);
    if key.protocol.is_openai() {
        body["store"] = json!(false);
    }
    let endpoint = match key.protocol {
        Protocol::Anthropic => "/v1/messages",
        Protocol::OpenaiChat => "/v1/chat/completions",
        Protocol::OpenaiResponses => "/v1/responses",
    };
    let url = crate::upstream::join_endpoint(&key.base_url, endpoint);
    let timeout =
        crate::upstream::key_timeout(key).clamp(Duration::from_secs(1), Duration::from_secs(120));
    let client = crate::upstream::decoding_client_for(&url);
    let request = client.post(url).json(&body).timeout(timeout);
    let request = crate::upstream::apply_client_identity(request, key.protocol);
    let request = super::apply_upstream_headers(request, key, secret, &[], model);
    control.phase(Phase::Waiting);
    let response = request.send().await.map_err(|error| {
        AppError::upstream_msg(redact(&error.without_url().to_string(), key, secret))
    })?;
    let status = response.status();
    let raw =
        crate::upstream::response_body::read_limited(response, MAX_RESPONSE_BYTES, |length| {
            control.received(length)
        })
        .await
        .map_err(|error| AppError::upstream_msg(redact(&error.to_string(), key, secret)))?;
    let parsed: Value = serde_json::from_slice(&raw).map_err(|_| {
        AppError::upstream_http(
            status.as_u16(),
            "上游未返回有效 JSON；单 Key 测试需要非流式文本响应。 / Expected a JSON response.",
        )
    })?;
    let failure = crate::upstream::upstream_error_message(&parsed);
    if !status.is_success() || failure.is_some() {
        let message = parsed["error"]["message"]
            .as_str()
            .or_else(|| parsed["error"].as_str())
            .or_else(|| parsed["response"]["error"]["message"].as_str())
            .or_else(|| parsed["message"].as_str())
            .unwrap_or("Upstream request failed");
        let message: String = redact(message, key, secret).chars().take(500).collect();
        return Err(AppError::upstream_http(
            status.as_u16(),
            format!("HTTP {status}: {message}"),
        ));
    }
    let chat = crate::upstream::convert_response_ext(
        &parsed,
        key.protocol,
        Protocol::OpenaiChat,
        &Default::default(),
        &Default::default(),
        &[],
    );
    let message = &chat["choices"][0]["message"];
    let text = message["content"]
        .as_str()
        .or_else(|| message["refusal"].as_str())
        .unwrap_or_default();
    if text.trim().is_empty() {
        return Err(AppError::upstream_msg(
            "上游没有返回文本内容。 / The upstream returned no text.",
        ));
    }
    Ok(ChatReply {
        key_id: key.id.clone(),
        model: model.into(),
        protocol: key.protocol,
        text: redact(text, key, secret),
        latency_ms: 0,
        usage: crate::upstream::extract_usage(&parsed),
        truncated: crate::upstream::is_truncated_response(&parsed),
    })
}

pub(super) async fn run(
    store: Arc<Store>,
    request: ChatRequest,
    on_started: impl FnOnce(Started) -> AppResult<()>,
) -> AppResult<ChatReply> {
    let _permit = CONCURRENT_TESTS.try_acquire().map_err(|_| {
        AppError::Invalid(
            "最多同时进行 4 个会话测试。 / At most four concurrent conversation tests.".into(),
        )
    })?;
    let key = store
        .get_key(&request.key_id)
        .ok_or_else(|| AppError::NotFound("测试 Key 已删除。 / Key was deleted.".into()))?;
    let model = validate(&request, &key)?;
    if store.category_has_budget(key.category_id)
        && crate::usage_cost::over_budget_ids(&store, key.category_id).contains(&key.id)
    {
        return Err(AppError::Invalid(
            "此 Key 已达到花费预算，请调整预算后测试。 / Key budget exhausted.".into(),
        ));
    }
    let secret = store
        .secret_for(&key.id)?
        .filter(|secret| !secret.is_empty())
        .ok_or_else(|| {
            AppError::Invalid(
                "此 Key 尚未配置密钥，请先保存密钥。 / Configure a credential first.".into(),
            )
        })?;
    let request_id = uuid::Uuid::new_v4().to_string();
    let guard = store.active_requests.begin(
        store.clone(),
        &request_id,
        key.category_id,
        &request.model,
        false,
    );
    let control = guard.control();
    control.attempt(&key, &request.model, 1);
    on_started(Started {
        request_id,
        category_id: key.category_id,
    })?;
    let started = Instant::now();
    let stopped = || {
        AppError::Invalid("会话测试已停止；上游可能仍会计费。 / Test stopped; the provider may still bill this request.".into())
    };
    let result = tokio::select! { biased;
        _ = control.cancelled() => Err(stopped()),
        result = send(&key, &secret, &model, &request.messages, &control) => result,
    };
    if control.finish(if result.is_ok() {
        End::Complete
    } else {
        End::UpstreamError
    }) == End::Operator
    {
        return Err(stopped());
    }
    let elapsed = started.elapsed().as_millis() as u64;
    let usage = result.as_ref().ok().and_then(|reply| reply.usage);
    store.append_event_model(
        key.category_id,
        "request",
        Some(&key.id),
        &format!(
            "单 Key 会话测试 · {} · {elapsed}ms",
            if result.is_ok() { "成功" } else { "失败" }
        ),
        None,
        None,
        usage,
        &model,
    );
    result.map(|mut reply| {
        reply.latency_ms = elapsed;
        reply
    })
}

#[tauri::command]
pub(crate) async fn test_key_conversation(
    state: tauri::State<'_, crate::AppState>,
    request: ChatRequest,
    on_started: tauri::ipc::Channel<Started>,
) -> AppResult<ChatReply> {
    run(state.store.clone(), request, |started| {
        on_started.send(started).map_err(|_| {
            AppError::Invalid("测试窗口已关闭，未发起请求。 / Test window closed.".into())
        })
    })
    .await
}

#[cfg(test)]
#[path = "key_chat_tests.rs"]
mod tests;
