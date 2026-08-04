//! upstream 内部的小工具：与协议无关、被 convert / sse / session 多处共用。
//!
//! 单独成文件是因为它们**没有任何 intra-upstream 依赖**（只用 uuid 与 serde_json），
//! 放哪个职责模块里都会给那个模块凭空添一条无关依赖。

use serde_json::Value;

/// 轻量随机 id 片段（无需强随机性，仅用于响应缺 id 时兜底）。
pub(super) fn uuid_like() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// content 可能是字符串或分块数组，统一抽取为纯文本（MVP 仅文本）。
pub(super) fn extract_text_content(content: Option<&Value>) -> String {
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
