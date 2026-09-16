use serde_json::{json, Value};
use super::super::util::extract_text_content;

pub(super) fn openai_user_content_to_anthropic(content: Option<&Value>) -> Value {
    let Some(Value::Array(parts)) = content else {
        return Value::String(extract_text_content(content));
    };
    let mut blocks = Vec::new();
    for part in parts {
        match part.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(text) = part.get("text").and_then(Value::as_str) {
                    blocks.push(json!({ "type": "text", "text": text }));
                }
            }
            Some("image_url") => {
                let url = part
                    .get("image_url")
                    .and_then(|v| v.get("url").or(Some(v)))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if let Some((media_type, data)) = url
                    .strip_prefix("data:")
                    .and_then(|v| v.split_once(";base64,"))
                {
                    blocks.push(json!({
                        "type": "image",
                        "source": { "type": "base64", "media_type": media_type, "data": data }
                    }));
                } else if url.starts_with("http://") || url.starts_with("https://") {
                    blocks.push(json!({
                        "type": "image",
                        "source": { "type": "url", "url": url }
                    }));
                } else {
                    blocks.push(json!({
                        "type": "text",
                        "text": "[SynaRoute] image_url 无法安全转换，图片未发送。"
                    }));
                }
            }
            _ => {}
        }
    }
    Value::Array(blocks)
}

pub(super) fn content_is_empty(content: &Value) -> bool {
    match content {
        Value::String(s) => s.is_empty(),
        Value::Array(a) => a.is_empty(),
        _ => true,
    }
}

pub(super) fn responses_content_for_chat(content: Option<&Value>) -> Value {
    match content {
        Some(Value::String(s)) => Value::String(s.clone()),
        Some(Value::Array(arr)) => {
            let mut parts = Vec::new();
            for block in arr {
                if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                    parts.push(json!({ "type": "text", "text": text }));
                    continue;
                }
                if block.get("type").and_then(|t| t.as_str()) == Some("input_image") {
                    if let Some(url) = responses_input_image_url(block) {
                        parts.push(json!({
                            "type": "image_url",
                            "image_url": { "url": url }
                        }));
                    } else {
                        parts.push(json!({
                            "type": "text",
                            "text": "[SynaRoute] input_image 无法安全转换，图片未发送。"
                        }));
                    }
                }
            }
            if parts.iter().all(|p| p.get("type").and_then(|t| t.as_str()) == Some("text")) {
                Value::String(
                    parts.iter()
                        .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                        .collect::<Vec<_>>()
                        .join(""),
                )
            } else {
                Value::Array(parts)
            }
        }
        _ => Value::String(String::new()),
    }
}

fn responses_input_image_url(block: &Value) -> Option<String> {
    match block.get("image_url") {
        Some(Value::String(url)) if !url.is_empty() => Some(url.clone()),
        Some(Value::Object(obj)) => obj
            .get("url")
            .and_then(|url| url.as_str())
            .filter(|url| !url.is_empty())
            .map(String::from),
        _ => None,
    }
}
