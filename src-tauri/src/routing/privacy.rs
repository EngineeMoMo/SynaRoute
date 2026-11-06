use super::{config::{invalid, PrivacyConfig, PrivacyMode}, detectors::{self, Scan}};
use crate::{error::AppResult, model::{CategoryType, Protocol}, store::Store};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use zeroize::Zeroizing;

const MAX_TEXT_BYTES: usize = 4 * 1024 * 1024;
const MAX_PREVIEW_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Outcome {
    pub counts: BTreeMap<String, usize>,
    pub unscanned: bool,
}

pub fn terms(store: &Store, config: &PrivacyConfig) -> AppResult<Zeroizing<Vec<String>>> {
    let Some(id) = &config.terms_secret else { return Ok(Zeroizing::new(Vec::new())); };
    let raw = store.secret_for(id)?.ok_or_else(|| invalid("敏感词库不可读取，未发送请求"))?;
    let terms: Vec<String> = serde_json::from_str(&raw).map_err(|_| invalid("敏感词库格式无效，未发送请求"))?;
    if terms.len() != config.term_count || terms.len() > 100 || terms.iter().any(|s| s.chars().count() < 3 || s.len() > 256) {
        return Err(invalid("敏感词库不完整，未发送请求"));
    }
    Ok(Zeroizing::new(terms))
}

fn argument_text<'a>(value: &'a Value, out: &mut Vec<&'a str>) {
    match value {
        Value::String(text) => out.push(text),
        Value::Array(values) => { for value in values { argument_text(value, out); } }
        Value::Object(values) => { for value in values.values() { argument_text(value, out); } }
        _ => {}
    }
}

fn text_values<'a>(value: &'a Value, out: &mut Vec<&'a str>, unscanned: &mut bool) {
    match value {
        Value::String(s) => out.push(s),
        Value::Array(values) => {
            for value in values { text_values(value, out, unscanned); }
        }
        Value::Object(object) => {
            let kind = object.get("type").and_then(Value::as_str).unwrap_or("");
            if matches!(kind, "image" | "image_url" | "input_image" | "input_file" | "file" | "document" | "redacted_thinking" | "item_reference") {
                *unscanned = true;
                return;
            }
            for (key, value) in object {
                // Tool arguments are user data, not protocol envelopes: even name/type/source are text.
                if kind == "tool_use" && key == "input" || key == "arguments" {
                    argument_text(value, out);
                    continue;
                }
                if matches!(key.as_str(), "signature" | "encrypted_content") {
                    if !value.is_null() { *unscanned = true; }
                    continue;
                }
                if matches!(key.as_str(), "id" | "call_id" | "tool_use_id" | "tool_call_id" | "name" | "role" | "type" | "model" | "cache_control" | "source") {
                    continue;
                }
                text_values(value, out, unscanned);
            }
        }
        _ => {}
    }
}

fn collect_text(body: &Value, protocol: Protocol) -> AppResult<(String, bool)> {
    if !body.is_object() { return Err(invalid("请求不是有效的 JSON 对象，无法执行敏感内容检查")); }
    let mut parts = Vec::new();
    let mut unscanned = false;
    let fields: &[&str] = match protocol {
        Protocol::Anthropic => &["system", "messages", "tools"],
        Protocol::OpenaiChat => &["messages", "tools", "functions"],
        Protocol::OpenaiResponses => &["instructions", "input", "tools"],
    };
    for field in fields {
        if let Some(value) = body.get(*field) { text_values(value, &mut parts, &mut unscanned); }
    }
    if body.get("previous_response_id").is_some_and(|v| !v.is_null()) { unscanned = true; }
    let total = parts.iter().try_fold(0usize, |size, s| size.checked_add(s.len()))
        .filter(|size| *size <= MAX_TEXT_BYTES).ok_or_else(|| invalid("文本超过敏感内容检查上限，未发送请求"))?;
    let mut text = String::with_capacity(total);
    // Adjacent text blocks may split a credential; joining without separators keeps that detectable.
    for part in parts { text.push_str(part); }
    Ok((text, unscanned))
}

pub fn scan_request(body: &Value, protocol: Protocol, config: &PrivacyConfig, terms: &[String]) -> AppResult<Outcome> {
    let (text, unscanned) = collect_text(body, protocol)?;
    let text = Zeroizing::new(text);
    let result = detectors::scan(&text, config, terms);
    if result.limited { return Err(invalid("敏感内容命中数量超过检查上限，未发送请求")); }
    Ok(Outcome { counts: result.counts, unscanned })
}

pub fn enforce(store: &Store, category: CategoryType, body: &Value, protocol: Protocol, config: &PrivacyConfig) -> AppResult<Outcome> {
    if config.mode == PrivacyMode::Off { return Ok(Outcome::default()); }
    let checked = terms(store, config).and_then(|terms| scan_request(body, protocol, config, &terms));
    match checked {
        Ok(result) => {
            let blocked = config.mode == PrivacyMode::Block && !result.counts.is_empty();
            store.routing_runtime.update(category, |activity| {
                activity.checked = activity.checked.saturating_add(1);
                activity.blocked = activity.blocked.saturating_add(u64::from(blocked));
                activity.privacy = if blocked { "blocked" } else if !result.counts.is_empty() { "warned" } else if result.unscanned { "partial" } else { "checked" }.into();
                activity.findings = result.counts.clone();
            });
            if blocked { return Err(invalid("敏感内容防护已拦截本次请求，未发送给判断模型或业务上游；请在路由策略中检查命中类型")); }
            Ok(result)
        }
        Err(error) => {
            store.routing_runtime.update(category, |activity| {
                activity.privacy = "scan_failed".into();
                activity.findings.clear();
                activity.blocked = activity.blocked.saturating_add(u64::from(config.mode == PrivacyMode::Block));
            });
            if config.mode == PrivacyMode::Block { Err(error) } else { Ok(Outcome { unscanned: true, ..Default::default() }) }
        }
    }
}

#[tauri::command]
pub async fn preview_sensitive_content(state: tauri::State<'_, crate::AppState>, category_id: CategoryType, text: String, credentials: bool, emails: bool, phones: bool) -> Result<Scan, String> {
    if text.len() > MAX_PREVIEW_BYTES { return Err("试扫文本最多 64 KiB".into()); }
    let store = state.store.clone();
    let text = Zeroizing::new(text);
    tauri::async_runtime::spawn_blocking(move || {
        let policy = super::settings(&store)?.categories.get(&category_id).cloned().unwrap_or_default();
        let config = PrivacyConfig { credentials, emails, phones, ..policy.privacy };
        let terms = terms(&store, &config)?;
        Ok::<_, crate::error::AppError>(detectors::scan(&text, &config, &terms))
    }).await.map_err(|_| "本地试扫失败".to_owned())?.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn tool_argument_keys_are_not_protocol_metadata() {
        for field in ["name", "type", "source", "id", "model", "signature", "encrypted_content"] {
            let body = json!({"messages":[{"role":"assistant","content":[{"type":"tool_use","input":{field:"private-needle"}}]}]});
            let result = scan_request(&body, Protocol::Anthropic, &PrivacyConfig::default(), &["private-needle".into()]).unwrap();
            assert_eq!(result.counts.get("custom"), Some(&1), "argument field {field}");
        }
    }

    #[test]
    fn routing_matrix_privacy_fail_closed_and_warn_preserves_body() {
        let dir = std::env::temp_dir().join(format!("sr-private-matrix-{}", uuid::Uuid::new_v4()));
        let store = Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap();
        let missing = PrivacyConfig { mode: PrivacyMode::Block, terms_secret: Some("__routing_terms_missing".into()), term_count: 1, ..Default::default() };
        let body = json!({"messages":[{"role":"user","content":"ordinary text"}]});
        assert!(enforce(&store, CategoryType::ClaudeCli, &body, Protocol::Anthropic, &missing).is_err());
        let huge = json!({"input":"x".repeat(MAX_TEXT_BYTES + 1)});
        let block = PrivacyConfig { mode: PrivacyMode::Block, ..Default::default() };
        assert!(enforce(&store, CategoryType::Codex, &huge, Protocol::OpenaiResponses, &block).is_err());
        let secret = format!("sk-proj-{}", "a".repeat(32));
        for (protocol, body) in [
            (Protocol::Anthropic, json!({"system":secret,"messages":[]})),
            (Protocol::OpenaiChat, json!({"messages":[{"role":"assistant","tool_calls":[{"function":{"arguments":secret}}]}]})),
            (Protocol::OpenaiResponses, json!({"input":[{"type":"function_call","arguments":secret}]})),
        ] {
            let before = body.clone();
            assert!(enforce(&store, CategoryType::Codex, &body, protocol, &block).is_err());
            let warn = PrivacyConfig { mode: PrivacyMode::Warn, ..Default::default() };
            assert!(!enforce(&store, CategoryType::Codex, &body, protocol, &warn).unwrap().counts.is_empty());
            assert_eq!(body, before);
        }
        drop(store); std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn routing_matrix_locked_terms_and_match_limit() {
        let dir = std::env::temp_dir().join(format!("sr-locked-terms-{}", uuid::Uuid::new_v4()));
        let store = Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap();
        let policy = super::super::config::CategoryPolicy { privacy: PrivacyConfig { mode: PrivacyMode::Block, ..Default::default() }, ..Default::default() };
        super::super::save_policy(&store, CategoryType::Codex, policy, Some(vec!["private-term".into()]), 0).unwrap();
        let config = super::super::settings(&store).unwrap().categories[&CategoryType::Codex].privacy.clone();
        assert!(!std::fs::read_to_string(dir.join("config.json")).unwrap().contains("private-term"));
        store.secrets.write().enable_master_password("Violet!River7-Cobalt9").unwrap();
        store.secrets.write().lock();
        assert!(enforce(&store, CategoryType::Codex, &json!({"input":"ordinary"}), Protocol::OpenaiResponses, &config).is_err());
        let text = (0..300).map(|n| format!("user{n}@example.com ")).collect::<String>();
        let config = PrivacyConfig { mode: PrivacyMode::Block, emails: true, ..Default::default() };
        assert!(scan_request(&json!({"input":text}), Protocol::OpenaiResponses, &config, &[]).is_err());
        drop(store); std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn scans_three_protocols_and_tool_results_without_altering_the_body() {
        let token = format!("sk-proj-{}", "A".repeat(32));
        for (protocol, body) in [
            (Protocol::Anthropic, json!({"messages":[{"role":"user","content":[{"type":"tool_result","content":token}]}]})),
            (Protocol::OpenaiChat, json!({"messages":[{"role":"tool","content":token}]})),
            (Protocol::OpenaiResponses, json!({"input":[{"type":"function_call_output","output":token}]})),
        ] {
            let before = body.clone();
            let result = scan_request(&body, protocol, &PrivacyConfig::default(), &[]).unwrap();
            assert_eq!(result.counts.get("api_key"), Some(&1));
            assert_eq!(body, before);
        }
    }
    #[test]
    fn split_text_is_scanned_but_binary_and_encrypted_fields_are_not_claimed_safe() {
        let body = json!({"messages":[{"role":"user","content":[{"type":"text","text":"sk-proj-"},{"type":"text","text":"abcdefghijklmnop0123456789012345"},{"type":"image","source":{"data":"private image"}}]}]});
        let result = scan_request(&body, Protocol::Anthropic, &PrivacyConfig::default(), &[]).unwrap();
        assert!(result.unscanned);
        assert_eq!(result.counts.get("api_key"), Some(&1));
        assert!(scan_request(&Value::Null, Protocol::Anthropic, &PrivacyConfig::default(), &[]).is_err());
    }
}
