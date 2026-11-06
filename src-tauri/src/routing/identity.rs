use crate::model::{CategoryType, Protocol};
use hyper::{HeaderMap, Request};
use serde_json::Value;
use std::net::IpAddr;

#[derive(Clone, Copy)]
pub struct VerifiedPeer(pub IpAddr);

#[derive(Clone, Default)]
pub struct Hints {
    peer: Option<IpAddr>,
    explicit: Option<String>,
    branch: Option<String>,
    claude: Option<String>,
    codex: Option<String>,
}

fn identifier(value: &str) -> Option<String> {
    (!value.is_empty() && value.len() <= 128 && value.bytes().all(|b| b.is_ascii_alphanumeric() || b"_-.".contains(&b)))
        .then(|| value.to_owned())
}
fn header(headers: &HeaderMap, key: &str) -> Option<String> {
    let mut values = headers.get_all(key).iter();
    let value = values.next()?.to_str().ok()?;
    if values.next().is_some() { return None; }
    identifier(value)
}

impl Hints {
    pub fn from_request<B>(req: &Request<B>) -> Self {
        Self {
            peer: req.extensions().get::<VerifiedPeer>().map(|p| p.0),
            explicit: header(req.headers(), "x-synaroute-session-id"),
            branch: header(req.headers(), "x-synaroute-branch-id"),
            claude: header(req.headers(), "x-claude-code-session-id"),
            codex: header(req.headers(), "thread-id").or_else(|| header(req.headers(), "session-id")),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Identity {
    pub session: String,
    pub turn: String,
    pub position: usize,
    pub new_turn: bool,
    pub prompt: Option<String>,
    pub source: &'static str,
}

fn user_text(content: &Value) -> Option<String> {
    if let Some(text) = content.as_str() { return Some(text.to_owned()); }
    let blocks = content.as_array()?;
    if blocks.iter().any(|b| b["type"] == "tool_result") { return None; }
    let parts: Vec<&str> = blocks.iter().filter(|b| matches!(b["type"].as_str(), Some("text" | "input_text")))
        .filter_map(|b| b["text"].as_str()).collect();
    if parts.is_empty() { return None; }
    Some(parts.join("\n"))
}

fn ignored_input(text: &str) -> bool {
    let text = text.trim_start();
    ["<environment_context>", "<turn_aborted>", "<system-reminder>", "<local-command-caveat>"]
        .iter().any(|prefix| text.starts_with(prefix))
}

fn protocol_name(protocol: Protocol) -> &'static str {
    match protocol { Protocol::Anthropic => "anthropic", Protocol::OpenaiChat => "chat", Protocol::OpenaiResponses => "responses" }
}

pub fn identify(hints: &Hints, body: &Value, category: CategoryType, protocol: Protocol) -> Option<Identity> {
    let peer = hints.peer?;
    let metadata = body.get("metadata").and_then(|m| m.get("user_id")).and_then(Value::as_str)
        .and_then(|s| (s.len() <= 4096).then_some(s))
        .and_then(|s| serde_json::from_str::<Value>(s).ok());
    let (id, source) = if let Some(id) = hints.explicit.clone() { (id, "explicit") }
    else if category == CategoryType::Codex { (hints.codex.clone()?, "native") }
    else {
        let payload_id = metadata.as_ref().and_then(|m| m["session_id"].as_str()).and_then(identifier);
        if let (Some(a), Some(b)) = (&hints.claude, &payload_id) { if a != b { return None; } }
        (hints.claude.clone().or(payload_id)?, "native")
    };
    let owned;
    let messages = if let Some(messages) = body["messages"].as_array() { messages }
        else if let Some(input) = body["input"].as_array() { input }
        else {
            let input = body["input"].as_str()?;
            owned = vec![serde_json::json!({"role":"user","content":input})]; &owned
        };
    let mut first = None;
    let mut latest = None;
    let mut user_count = 0;
    for (index, message) in messages.iter().enumerate() {
        if message["role"] != "user" { continue; }
        let Some(text) = user_text(&message["content"]) else { continue; };
        if ignored_input(&text) { continue; }
        user_count += 1;
        first.get_or_insert(message);
        latest = Some((index, message, text));
    }
    let (index, message, prompt) = latest?;
    let peer = match peer {
        IpAddr::V6(v) => v.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(IpAddr::V6(v)),
        value => value,
    };
    let peer_scope = if peer.is_loopback() { "loopback".to_owned() } else { peer.to_string() };
    // A native ID is required; the prefix only partitions branches sharing that ID.
    let first = super::hash_bytes(&serde_json::to_vec(first?).ok()?);
    let session = super::hash_bytes(&serde_json::to_vec(&(category.as_str(), protocol_name(protocol), peer_scope, id, &hints.branch, first)).ok()?);
    let turn = super::hash_bytes(&serde_json::to_vec(&(user_count, message)).ok()?);
    Some(Identity { session, turn, position: user_count, new_turn: index + 1 == messages.len(),
        prompt: (!prompt.trim().is_empty() && prompt.len() <= 24_000).then_some(prompt), source })
}

pub fn opaque_history(body: &Value) -> bool {
    fn has_opaque(value: &Value) -> bool {
        match value {
            Value::Object(obj) => obj.get("encrypted_content").is_some_and(|v| !v.is_null())
                || obj.get("signature").is_some_and(|v| !v.is_null())
                || obj.get("type").and_then(Value::as_str) == Some("item_reference")
                || obj.values().any(has_opaque),
            Value::Array(values) => values.iter().any(has_opaque),
            _ => false,
        }
    }
    body.get("previous_response_id").is_some_and(|v| !v.is_null()) || has_opaque(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn hints(category: CategoryType, id: &str) -> Hints {
        Hints { peer: Some("127.0.0.1".parse().unwrap()), claude: (category != CategoryType::Codex).then(|| id.into()), codex: (category == CategoryType::Codex).then(|| id.into()), ..Default::default() }
    }
    #[test]
    fn native_identity_matches_fixture_headers_and_isolates_sessions() {
        let body = json!({"messages":[{"role":"user","content":"same text"}]});
        let a = identify(&hints(CategoryType::ClaudeCli, "session-a"), &body, CategoryType::ClaudeCli, Protocol::Anthropic).unwrap();
        let b = identify(&hints(CategoryType::ClaudeCli, "session-b"), &body, CategoryType::ClaudeCli, Protocol::Anthropic).unwrap();
        assert_ne!(a.session, b.session);
        let mut remote = hints(CategoryType::ClaudeCli, "session-a"); remote.peer = Some("192.168.0.5".parse().unwrap());
        assert_ne!(a.session, identify(&remote, &body, CategoryType::ClaudeCli, Protocol::Anthropic).unwrap().session);
        assert!(identify(&Hints::default(), &body, CategoryType::ClaudeCli, Protocol::Anthropic).is_none());
    }
    #[test]
    fn routing_matrix_identity_partitions_branch_category_protocol_and_peer() {
        let body = json!({"messages":[{"role":"user","content":"same text"}]});
        let base = hints(CategoryType::ClaudeCli, "s");
        let identify_at = |h: &Hints, category, protocol| identify(h, &body, category, protocol).unwrap();
        let original = identify_at(&base, CategoryType::ClaudeCli, Protocol::Anthropic);
        let mut branch = base.clone(); branch.branch = Some("branch-a".into());
        assert_ne!(original.session, identify_at(&branch, CategoryType::ClaudeCli, Protocol::Anthropic).session);
        assert_ne!(original.session, identify_at(&base, CategoryType::ClaudeDesktop, Protocol::Anthropic).session);
        assert_ne!(original.session, identify_at(&base, CategoryType::ClaudeCli, Protocol::OpenaiChat).session);
        let mut mapped = base.clone(); mapped.peer = Some("::ffff:127.0.0.1".parse().unwrap());
        assert_eq!(original.session, identify_at(&mapped, CategoryType::ClaudeCli, Protocol::Anthropic).session);
        let mut request = Request::builder().header("x-synaroute-session-id", "explicit").header("x-synaroute-branch-id", "b").body(()).unwrap();
        request.extensions_mut().insert(VerifiedPeer("127.0.0.1".parse().unwrap()));
        let parsed = Hints::from_request(&request);
        assert_eq!(identify_at(&parsed, CategoryType::Codex, Protocol::OpenaiResponses).source, "explicit");
        request.headers_mut().append("x-synaroute-session-id", "other".parse().unwrap());
        assert!(identify(&Hints::from_request(&request), &body, CategoryType::Codex, Protocol::OpenaiResponses).is_none());
    }

    #[test]
    fn tool_results_keep_the_turn_and_new_user_input_advances_it() {
        for protocol in [Protocol::Anthropic, Protocol::OpenaiChat, Protocol::OpenaiResponses] {
            let mut messages = vec![json!({"role":"user","content":"write tests"})];
            let body = |messages: &Vec<Value>| if protocol == Protocol::OpenaiResponses { json!({"input":messages}) } else { json!({"messages":messages}) };
            let h = hints(CategoryType::Codex, "thread");
            let first = identify(&h, &body(&messages), CategoryType::Codex, protocol).unwrap();
            messages.push(json!({"role":"assistant","content":"working"}));
            messages.push(if protocol == Protocol::Anthropic { json!({"role":"user","content":[{"type":"tool_result","tool_use_id":"t","content":"result"}]}) } else { json!({"type":"function_call_output","role":"tool","output":"result"}) });
            let next = identify(&h, &body(&messages), CategoryType::Codex, protocol).unwrap();
            assert_eq!(first.session, next.session); assert_eq!(first.turn, next.turn); assert!(!next.new_turn);
            messages.push(json!({"role":"user","content":"write tests"}));
            let later = identify(&h, &body(&messages), CategoryType::Codex, protocol).unwrap();
            assert_eq!(later.session, first.session); assert_ne!(later.turn, first.turn); assert_eq!(later.position, 2);
        }
    }
}
