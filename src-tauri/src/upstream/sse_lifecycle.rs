use super::{SseDirection, SseTranslator};
use crate::model::Protocol;
use serde_json::json;

impl SseTranslator {
    pub(crate) fn failed(&self) -> bool {
        self.status.failed || self.decoder.failed || self.limit_hit.is_some()
    }

    pub(super) fn upstream_protocol(&self) -> Protocol {
        match self.dir {
            SseDirection::ChatToResponses | SseDirection::ChatToAnthropic => Protocol::OpenaiChat,
            SseDirection::AnthropicToChat | SseDirection::AnthropicToResponses => Protocol::Anthropic,
            SseDirection::ResponsesToChat | SseDirection::ResponsesToAnthropic => Protocol::OpenaiResponses,
        }
    }

    pub fn finish(&mut self) -> String {
        if self.finished { return String::new(); }
        self.finished = true;
        if self.failed() { self.ending.clear(); return String::new(); }
        if !self.status.terminal || self.decoder.pending() {
            self.status.failed = true;
            self.ending.clear();
            return self.translate_upstream_error(&json!({"type":"error","message":"Upstream stream ended without a valid completion event"})).unwrap_or_default();
        }
        let mut out = std::mem::take(&mut self.ending);
        out.push_str(&match self.dir {
            SseDirection::ChatToResponses | SseDirection::AnthropicToResponses => self.emit_responses_completed(None),
            SseDirection::ChatToAnthropic | SseDirection::ResponsesToAnthropic => self.emit_anthropic_stop(),
            SseDirection::ResponsesToChat | SseDirection::AnthropicToChat => "data: [DONE]\n\n".to_string(),
        });
        out
    }
}
