//! Anthropic prompt-caching 的自愈回退。
//!
//! 独立成模块是因为这一簇**零 intra-upstream 依赖**（只用 serde_json 与 std::sync），
//! 是整个 upstream 里耦合最松的一块。

use serde_json::{json, Value};

/// 已知**不支持** `cache_control` 的上游 base_url（进程级记忆）。
///
/// 自愈回退用：某个中转因 `cache_control` 回 400 后，把它的 base_url 记进来，
/// 后续请求直接不带缓存字段,不再每次都先撞一次 400 再重发。
/// 进程级即可——换机/重启后重新探测一次无妨,且避免持久化一个可能随中转升级而变化的判断。
static CACHE_UNSUPPORTED: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
    std::sync::OnceLock::new();

fn cache_unsupported() -> &'static std::sync::Mutex<std::collections::HashSet<String>> {
    CACHE_UNSUPPORTED.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
}

pub(crate) fn cache_known_unsupported(base_url: &str) -> bool {
    cache_unsupported()
        .lock()
        .map(|s| s.contains(base_url))
        .unwrap_or(false)
}

pub(crate) fn mark_cache_unsupported(base_url: &str) {
    if let Ok(mut s) = cache_unsupported().lock() {
        s.insert(base_url.to_string());
    }
}

/// 某个 HTTP 400 的响应体是否**疑似**因 `cache_control` 引起(而非模型名错、参数错等真问题)。
///
/// 判据保守:必须明确提到 cache 相关字样才回退,否则会把「model is required」这类真正的 400
/// 也误当成缓存问题去掉缓存重发——那只会白发一次、真问题依旧,且掩盖了根因。
pub(crate) fn looks_like_cache_rejection(body: &str) -> bool {
    let b = body.to_ascii_lowercase();
    b.contains("cache_control")
        || b.contains("cache control")
        || (b.contains("cache") && (b.contains("unexpected") || b.contains("unsupported") || b.contains("unknown")))
}

/// 给 Anthropic 请求体的**稳定前缀**打缓存断点(5 分钟 TTL)。
///
/// 前缀缓存的顺序是 tools → system → messages,任何字节变动使其后失效。工具循环里:
/// - `tools` 声明整个循环不变 → 断点①钉在 tools 数组**最后一个**元素上,缓存全部工具声明
/// - `messages` 只追加不改写 → 断点②钉在**最后一条消息的最后一个 content 块**上,
///   缓存「到本轮为止的全部历史」;下一轮请求的前缀恰好是这一份,自动命中(0.1x 价)
///
/// 只对 **content 已是数组**的消息打断点:字符串型 content(第一轮的初始问题)若包成数组,
/// 会与后续轮次的字符串形态不一致、破坏前缀逐字节匹配;而工具循环从第二轮起最后一条
/// 必是 tool_result 数组,正是缓存收益最大处,不损失。
///
/// **零信息损失**:只加元数据,模型看到的内容一字不变。
pub(crate) fn inject_anthropic_cache(payload: &mut Value, has_tools: bool) {
    inject_breakpoints(payload, has_tools, false);
}

#[derive(Debug, Default)]
pub(crate) struct CacheInjection(Vec<String>);

fn cache_paths(payload: &Value) -> Vec<String> {
    let mut paths = Vec::new();
    for section in ["tools", "system"] {
        if let Some(blocks) = payload.get(section).and_then(Value::as_array) {
            paths.extend((0..blocks.len()).map(|i| format!("/{section}/{i}")));
        }
    }
    if let Some(messages) = payload.get("messages").and_then(Value::as_array) {
        for (i, msg) in messages.iter().enumerate() {
            if let Some(blocks) = msg.get("content").and_then(Value::as_array) {
                paths.extend((0..blocks.len()).map(|j| format!("/messages/{i}/content/{j}")));
            }
        }
    }
    paths
}

fn inject_breakpoints(payload: &mut Value, has_tools: bool, system: bool) -> CacheInjection {
    let paths = cache_paths(payload);
    let existing = paths.iter().filter(|p| payload.pointer(p).is_some_and(|v| v.get("cache_control").is_some())).count();
    let mut room = 4usize.saturating_sub(existing);
    let mut candidates = Vec::new();
    if has_tools {
        if let Some(path) = paths.iter().rev().find(|p| p.starts_with("/tools/")) {
            candidates.push(path.clone());
        }
    }
    if system {
        if let Some(path) = paths.iter().rev().find(|p| p.starts_with("/system/")) {
            candidates.push(path.clone());
        }
    }
    if let Some(path) = paths.iter().rev().find(|p| p.starts_with("/messages/") && payload.pointer(p).is_some_and(|v| {
        matches!(v.get("type").and_then(Value::as_str), Some("text" | "image" | "tool_result" | "tool_use"))
    })) {
        candidates.push(path.clone());
    }
    let mut added = CacheInjection::default();
    for path in candidates {
        if room == 0 { break; }
        if let Some(obj) = payload.pointer_mut(&path).and_then(Value::as_object_mut) {
            if !obj.contains_key("cache_control") {
                obj.insert("cache_control".into(), json!({"type":"ephemeral"}));
                added.0.push(path);
                room -= 1;
            }
        }
    }
    added
}

/// 只对代理转换生成的请求使用；整段历史一致归一，避免仅末条变形导致前缀漂移。
pub(crate) fn inject_converted_cache(payload: &mut Value) -> CacheInjection {
    if let Some(system) = payload.get_mut("system") {
        if let Some(text) = system.as_str() {
            *system = json!([{"type":"text","text":text}]);
        }
    }
    if let Some(messages) = payload.get_mut("messages").and_then(Value::as_array_mut) {
        for message in messages {
            if let Some(content) = message.get_mut("content") {
                if let Some(text) = content.as_str() {
                    *content = json!([{"type":"text","text":text}]);
                }
            }
        }
    }
    inject_breakpoints(payload, true, true)
}

/// 只撤销本轮增加且未被其他逻辑改写的断点；不认领客户端已有字段。
pub(crate) fn rollback_injected_cache(payload: &mut Value, injection: &CacheInjection) -> usize {
    let mut removed = 0;
    for path in &injection.0 {
        if let Some(obj) = payload.pointer_mut(path).and_then(Value::as_object_mut) {
            if obj.get("cache_control") == Some(&json!({"type":"ephemeral"})) {
                obj.remove("cache_control");
                removed += 1;
            }
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rollback_preserves_caller_fields_and_is_idempotent() {
        let mut p = json!({"system":"stable","tools":[{"name":"user","cache_control":{"type":"ephemeral","ttl":"1h"}},{"name":"ours"}],"messages":[{"role":"user","content":"hello"}]});
        let injection = inject_converted_cache(&mut p);
        assert_eq!(p["messages"][0]["content"][0]["cache_control"]["type"], "ephemeral");
        assert_eq!(rollback_injected_cache(&mut p,&injection),3);
        assert_eq!(p["tools"][0]["cache_control"]["ttl"],"1h");
        assert_eq!(rollback_injected_cache(&mut p,&injection),0);
    }

    #[test]
    fn existing_four_breakpoints_are_preserved_without_adding_more() {
        let mut p = json!({"tools":[{"cache_control":{}},{"cache_control":{}},{"cache_control":{}},{"cache_control":{}}],"system":[{"type":"text","text":"s"}],"messages":[{"content":[{"type":"thinking","thinking":"x"}]}]});
        let original=p.clone();
        let injection=inject_converted_cache(&mut p);
        assert_eq!(p,original);
        assert_eq!(rollback_injected_cache(&mut p,&injection),0);
    }

    #[test]
    fn inject_anthropic_cache_marks_tools_tail_and_last_message_block() {
        // 断点应钉在两处稳定前缀:tools 数组末尾 + 最后一条消息的最后一个 content 块。
        // 这正是工具循环里「到本轮为止的全部历史」的边界,下一轮请求前缀恰好命中。
        let mut payload = json!({
            "model": "claude-opus-4-8",
            "max_tokens": 1024,
            "tools": [
                { "name": "read_file", "description": "d1", "input_schema": {} },
                { "name": "grep", "description": "d2", "input_schema": {} }
            ],
            "messages": [
                { "role": "user", "content": "问题" },
                { "role": "assistant", "content": [ { "type": "tool_use", "id": "c1", "name": "grep", "input": {} } ] },
                { "role": "user", "content": [ { "type": "tool_result", "tool_use_id": "c1", "content": "命中" } ] }
            ]
        });
        inject_anthropic_cache(&mut payload, true);

        // 断点①:tools 的**最后一个**才带,前面的不带(否则浪费断点额度)
        let tools = payload["tools"].as_array().unwrap();
        assert!(tools[0].get("cache_control").is_none(), "非末尾 tool 不该带断点");
        assert_eq!(tools[1]["cache_control"]["type"], "ephemeral", "tools 末尾应打断点");

        // 断点②:最后一条消息的最后一个 content 块
        let msgs = payload["messages"].as_array().unwrap();
        assert!(
            msgs[0].get("content").unwrap().is_string(),
            "第一条是字符串型 content,不该被强行改成数组(会破坏前缀逐字节匹配)"
        );
        let last_block = msgs[2]["content"].as_array().unwrap().last().unwrap();
        assert_eq!(last_block["cache_control"]["type"], "ephemeral", "最后一块应打断点");
    }

    #[test]
    fn inject_cache_is_noop_on_string_content_and_missing_tools() {
        // 收尾轮无 tools + 初始只有字符串 user:不应崩、不应把字符串包成数组。
        let mut p = json!({
            "model": "m", "max_tokens": 10,
            "messages": [ { "role": "user", "content": "只有一条字符串" } ]
        });
        inject_anthropic_cache(&mut p, false); // has_tools=false
        assert!(p.get("tools").is_none());
        assert!(
            p["messages"][0]["content"].is_string(),
            "字符串 content 不带块级断点,保持原样"
        );
    }

    #[test]
    fn cache_rejection_detector_is_conservative() {
        // 只有明确提到 cache 才回退,否则会把真正的 400(model 错等)误当缓存问题白重发一次。
        assert!(looks_like_cache_rejection(
            r#"{"error":{"message":"unexpected field cache_control"}}"#
        ));
        assert!(looks_like_cache_rejection("Unsupported parameter: cache control"));
        assert!(looks_like_cache_rejection(r#"{"error":"unknown cache field"}"#));
        // 真正的 400,与缓存无关 → 不回退
        assert!(!looks_like_cache_rejection(
            r#"{"error":{"message":"model is required"}}"#
        ));
        assert!(!looks_like_cache_rejection(
            r#"{"error":{"message":"max_tokens must be > 0"}}"#
        ));
        assert!(!looks_like_cache_rejection("Failed to parse request body"));
    }

    #[test]
    fn cache_unsupported_endpoint_memory_roundtrips() {
        let url = "https://strict-relay.test/v1";
        assert!(!cache_known_unsupported(url), "初始未知");
        mark_cache_unsupported(url);
        assert!(cache_known_unsupported(url), "标记后应记住");
        // 别的端点不受影响
        assert!(!cache_known_unsupported("https://other.test/v1"));
    }
}
