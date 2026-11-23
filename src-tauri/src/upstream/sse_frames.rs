use crate::model::Protocol;
use serde_json::{json, Value};

pub(crate) const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

#[derive(Default)]
pub(crate) struct Decoder {
    line: Vec<u8>,
    data: String,
    event: String,
    bytes: usize,
    after_cr: bool,
    started: bool,
    pub failed: bool,
    pub oversized: bool,
}

impl Decoder {
    pub fn push(&mut self, chunk: &[u8]) -> Vec<Value> {
        let mut events = Vec::new();
        for &byte in chunk {
            if self.failed { break; }
            if byte == b'\n' && self.after_cr { self.after_cr = false; continue; }
            self.after_cr = byte == b'\r';
            if byte == b'\n' || byte == b'\r' {
                let raw = std::mem::take(&mut self.line);
                match std::str::from_utf8(&raw) {
                    Ok(line) => self.line(line, &mut events),
                    Err(_) => self.failed = true,
                }
            } else if self.line.len().saturating_add(self.bytes) >= MAX_FRAME_BYTES {
                self.failed = true;
                self.oversized = true;
            } else { self.line.push(byte); }
        }
        if self.failed { self.line.clear(); self.data.clear(); self.event.clear(); }
        events
    }

    fn line(&mut self, line: &str, events: &mut Vec<Value>) {
        let line = if !self.started { self.started = true; line.strip_prefix('\u{feff}').unwrap_or(line) } else { line };
        if line.is_empty() {
            if !self.data.is_empty() {
                let mut value = if self.data.trim() == "[DONE]" {
                    json!({"type":"synaroute.chat.done"})
                } else {
                    match serde_json::from_str::<Value>(&self.data) {
                        Ok(value) if value.is_object() => value,
                        Ok(value) if self.event == "error" || self.event == "response.failed" => json!({"error":value}),
                        _ => { self.failed = true; return; }
                    }
                };
                if !self.event.is_empty() && self.event != "message" { value["type"] = json!(self.event); }
                events.push(value);
            }
            self.data.clear(); self.event.clear(); self.bytes = 0;
            return;
        }
        self.bytes = self.bytes.saturating_add(line.len() + 1);
        if line.starts_with(':') { return; }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "data" => { if !self.data.is_empty() { self.data.push('\n'); } self.data.push_str(value); }
            "event" => self.event = value.into(),
            _ => {}
        }
    }

    pub fn pending(&self) -> bool { !self.line.is_empty() || !self.data.is_empty() || !self.event.is_empty() }
}

#[derive(Default)]
pub(crate) struct Status {
    pub terminal: bool,
    pub failed: bool,
}

impl Status {
    pub fn observe(&mut self, protocol: Protocol, event: &Value) {
        if super::sse::sse_error::upstream_error_message(event).is_some()
            || event.pointer("/response/status").and_then(Value::as_str) == Some("failed") {
            self.failed = true;
        }
        let kind = event.get("type").and_then(Value::as_str).unwrap_or("");
        self.terminal |= match protocol {
            Protocol::OpenaiChat => kind == "synaroute.chat.done" || event.get("choices")
                .and_then(Value::as_array).is_some_and(|choices| choices.iter().any(|choice|
                    choice.get("finish_reason").and_then(Value::as_str).is_some_and(|reason| !reason.is_empty()))),
            Protocol::Anthropic => kind == "message_stop",
            Protocol::OpenaiResponses => matches!(kind, "response.completed" | "response.incomplete" | "response.done"),
        };
    }
}

pub(crate) struct Monitor {
    decoder: Decoder,
    status: Status,
    protocol: Protocol,
    usage: crate::model::TokenUsage,
    response_id: Option<String>,
}

impl Monitor {
    pub fn new(protocol: Protocol) -> Self { Self { decoder: Decoder::default(), status: Status::default(), protocol, usage: Default::default(), response_id: None } }
    pub fn push(&mut self, bytes: &[u8]) -> usize {
        if self.failed(false) { return 0; }
        let mut consumed = 0;
        for line in bytes.split_inclusive(|byte| *byte == b'\n' || *byte == b'\r') {
            for event in self.decoder.push(line) {
                self.status.observe(self.protocol, &event);
                if self.protocol == Protocol::OpenaiResponses && matches!(event["type"].as_str(), Some("response.completed" | "response.incomplete" | "response.done")) {
                    self.response_id = event.pointer("/response/id").and_then(Value::as_str).filter(|id| id.len() <= 256).map(str::to_owned);
                }
                if let Some(usage) = super::extract_usage(&event) {
                    self.usage.input = self.usage.input.max(usage.input);
                    self.usage.output = self.usage.output.max(usage.output);
                    self.usage.cache_read = self.usage.cache_read.max(usage.cache_read);
                    self.usage.cache_creation = self.usage.cache_creation.max(usage.cache_creation);
                }
            }
            consumed += line.len();
            if self.failed(false) { break; }
        }
        consumed
    }
    pub fn failed(&self, eof: bool) -> bool {
        self.decoder.failed || self.status.failed || (eof && (!self.status.terminal || self.decoder.pending()))
    }
    pub fn explicit_error(&self) -> bool { self.status.failed }
    pub fn response_id(&self) -> Option<&str> { (!self.failed(false)).then_some(self.response_id.as_deref()).flatten() }
    pub fn usage(&self) -> Option<crate::model::TokenUsage> { (!self.usage.is_empty()).then_some(self.usage) }
}

pub(crate) fn failure(protocol: Protocol) -> String {
    let message = "Upstream SSE stream is invalid or ended without a valid completion event";
    match protocol {
        Protocol::Anthropic => format!("event: error\ndata: {}\n\n", json!({"type":"error","error":{"type":"api_error","message":message}})),
        Protocol::OpenaiChat => format!("data: {}\n\n", json!({"error":{"type":"api_error","message":message}})),
        Protocol::OpenaiResponses => format!("event: response.failed\ndata: {}\n\n", json!({"type":"response.failed","response":{"object":"response","status":"failed","error":{"code":"api_error","message":message}}})),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_names_multiline_and_utf8_survive_all_boundaries() {
        let wire = "event: error\r\ndata: {\r\ndata: \"message\":\"错误\"}\r\n\r\n";
        for width in 1..=wire.len() {
            let mut decoder = Decoder::default();
            let events: Vec<_> = wire.as_bytes().chunks(width).flat_map(|chunk| decoder.push(chunk)).collect();
            assert!(!decoder.failed);
            assert_eq!(events, vec![json!({"type":"error","message":"错误"})]);
        }
    }

    #[test]
    fn failure_remains_visible_beyond_the_tail_window() {
        let mut monitor = Monitor::new(Protocol::OpenaiResponses);
        monitor.push(b"event: error\ndata: {\"message\":\"failed\"}\n\n");
        monitor.push(&b": heartbeat\n\n".repeat(20_000));
        monitor.push(b"data: {\"type\":\"response.completed\"}\n\n");
        assert!(monitor.failed(true));
    }

    #[test]
    fn incomplete_frames_and_missing_terminal_are_failures() {
        for wire in ["data: {\"type\":\"response.output_text.delta\",\"delta\":\"partial\"}\n\n", "data: {\"type\":\"response.completed\"}"] {
            let mut monitor = Monitor::new(Protocol::OpenaiResponses);
            monitor.push(wire.as_bytes());
            assert!(monitor.failed(true));
        }
        let mut monitor = Monitor::new(Protocol::OpenaiResponses);
        monitor.push(b"data: {\"type\":\"response.incomplete\"}\n\n");
        assert!(!monitor.failed(true));
    }

    #[test]
    fn multiline_frame_size_is_bounded() {
        let mut decoder = Decoder::default();
        for _ in 0..(MAX_FRAME_BYTES / 1024 + 2) { decoder.push(format!("data: {}\n", "x".repeat(1024)).as_bytes()); }
        assert!(decoder.failed);
    }

    #[test]
    fn string_errors_cut_later_success_and_usage_outlives_window() {
        let mut monitor = Monitor::new(Protocol::OpenaiResponses);
        monitor.push(b"data: {\"usage\":{\"input_tokens\":42,\"output_tokens\":2}}\n\n");
        monitor.push(&b": heartbeat\n\n".repeat(20_000));
        let failed = b"event: error\ndata: \"failure\"\n\n";
        let mut combined = failed.to_vec();
        combined.extend_from_slice(b"event: response.completed\ndata: {}\n\n");
        assert_eq!(monitor.push(&combined), failed.len());
        assert_eq!(monitor.push(b"data: [DONE]\n\n"), 0);
        assert!(monitor.failed(true));
        assert_eq!(monitor.usage().unwrap().input, 42);
        assert_eq!(monitor.usage().unwrap().output, 2);
    }
}
