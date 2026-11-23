use crate::{model::{Protocol, ProviderKey}, upstream::sse_frames::{Decoder, Status, MAX_FRAME_BYTES}};
use crate::upstream::error_redaction::ErrorRedactor;
use zeroize::Zeroizing;

pub(super) fn error_bytes(status: u16, bytes: bytes::Bytes, key: &ProviderKey, secret: &str) -> bytes::Bytes {
    if status >= 400 || crate::upstream::body_is_upstream_error(&bytes).is_some() {
        return bytes::Bytes::from(Redactor::new(key, secret).body(&bytes));
    }
    bytes
}

pub(super) struct Redactor {
    frame: Zeroizing<Vec<u8>>,
    line_bytes: usize,
    after_cr: bool,
    credentials: ErrorRedactor,
    protocol: Protocol,
    closed: bool,
}

impl Redactor {
    pub fn pending(&self) -> bool { !self.frame.is_empty() }

    pub fn message(&self, text: &str) -> String {
        self.credentials.message(text)
    }

    pub fn body(&self, bytes: &[u8]) -> String {
        self.credentials.body(bytes)
    }

    pub fn finish(&mut self) -> Vec<u8> {
        if !self.pending() { return Vec::new(); }
        self.frame.clear();
        self.closed = true;
        crate::upstream::sse_frames::failure(self.protocol).into_bytes()
    }

    pub fn new(key: &ProviderKey, secret: &str) -> Self {
        Self { frame: Zeroizing::new(Vec::new()), line_bytes: 0, after_cr: false,
            credentials: ErrorRedactor::new(key, secret), protocol: key.protocol, closed: false }
    }

    pub fn push(&mut self, bytes: &[u8]) -> Vec<u8> {
        let mut output = Vec::new();
        for &byte in bytes {
            if self.closed { break; }
            if self.after_cr && byte == b'\n' {
                self.after_cr = false;
                if self.frame.is_empty() { output.push(byte); } else { self.frame.push(byte); }
                continue;
            }
            if self.frame.len() >= MAX_FRAME_BYTES {
                self.closed = true;
                self.frame.clear();
                output.extend_from_slice(crate::upstream::sse_frames::failure(self.protocol).as_bytes());
                break;
            }
            self.frame.push(byte);
            self.after_cr = byte == b'\r';
            if byte == b'\r' || byte == b'\n' {
                if self.line_bytes == 0 { self.flush(&mut output); }
                self.line_bytes = 0;
            } else { self.line_bytes += 1; }
        }
        output
    }

    fn flush(&mut self, output: &mut Vec<u8>) {
        let mut decoder = Decoder::default();
        let events = decoder.push(&self.frame);
        if decoder.failed {
            self.closed = true;
            output.extend_from_slice(crate::upstream::sse_frames::failure(self.protocol).as_bytes());
        } else {
            let mut status = Status::default();
            for event in &events { status.observe(self.protocol, event); }
            if status.failed {
                self.closed = true;
                for mut event in events {
                    self.credentials.value(&mut event);
                    let kind = event["type"].as_str().filter(|kind| !kind.contains(['\r', '\n'])).unwrap_or("error");
                    output.extend_from_slice(format!("event: {kind}\ndata: {event}\n\n").as_bytes());
                }
            } else { output.extend_from_slice(&self.frame); }
        }
        self.frame.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    #[test]
    fn http_and_soft_errors_redact_decoded_api_and_custom_header_credentials() {
        let key = ProviderKey { headers_json:Some(r#"{"x-fixture":"header-secret"}"#.into()), ..Default::default() };
        for status in [200, 401, 500] {
            let bytes = bytes::Bytes::from_static(br#"{"error":{"message":"private\u002dsecret header\u002dsecret","private\u002dsecret":"header-secret"}}"#);
            let body = error_bytes(status, bytes, &key, "private-secret");
            let parsed: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(parsed["error"]["message"], "*** ***");
            assert_eq!(parsed["error"]["***"], "***");
        }
        let normal = bytes::Bytes::from_static(br#"{ "text": "private-secret header-secret" }"#);
        assert_eq!(error_bytes(200, normal.clone(), &key, "private-secret"), normal);
        assert_eq!(Redactor::new(&key, "private-secret").body(b"private-secret header-secret"), "*** ***");
    }

    #[test]
    fn errors_are_redacted_across_every_chunk_boundary_and_protocol() {
        let secret = "private-fixture-secret";
        for (protocol, event, data) in [
            (Protocol::Anthropic, "error", json!({"type":"error","error":{"message":secret}})),
            (Protocol::OpenaiChat, "message", json!({"error":{"message":secret}})),
            (Protocol::OpenaiResponses, "response.failed", json!({"response":{"status":"failed","error":{"message":secret}}})),
        ] {
            let key = ProviderKey { protocol, ..Default::default() };
            let wire = format!("event: {event}\r\ndata: {data}\r\n\r\n");
            for width in 1..=wire.len() {
                let mut redactor = Redactor::new(&key, secret);
                let output: Vec<u8> = wire.as_bytes().chunks(width).flat_map(|chunk| redactor.push(chunk)).collect();
                let text = String::from_utf8(output).unwrap();
                assert!(!text.contains(secret));
                assert!(text.contains("***"));
                for downstream in [Protocol::Anthropic, Protocol::OpenaiChat, Protocol::OpenaiResponses] {
                    if let Some(direction) = crate::upstream::sse_direction(downstream, protocol) {
                        let mut translator = crate::upstream::SseTranslator::new(direction);
                        let translated = translator.push(text.as_bytes());
                        assert!(!translated.contains(secret));
                        assert!(translator.failed());
                    }
                }
            }
        }
    }

    #[test]
    fn decoded_errors_and_custom_headers_are_redacted_but_normal_frames_are_unchanged() {
        let key = ProviderKey { headers_json: Some(r#"{"x-fixture":"header-secret"}"#.into()), ..Default::default() };
        let mut redactor = Redactor::new(&key, "private-secret");
        let normal = b": ping\r\n\r\ndata: {\"choices\":[{\"delta\":{\"content\":\"private-secret\"}}]}\r\n\r\n";
        let output: Vec<u8> = normal.chunks(1).flat_map(|chunk| redactor.push(chunk)).collect();
        assert_eq!(output, normal);
        let error = br#"data: {"error":{"message":"private\u002dsecret header-secret"}}"#;
        let mut output = redactor.push(error);
        output.extend(redactor.push(b"\n\n"));
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("*** ***"), "{text}");
    }

    #[test]
    fn incomplete_and_oversized_errors_never_release_partial_credentials() {
        let mut redactor = Redactor::new(&ProviderKey::default(), "private-secret");
        assert!(redactor.push(b"event: error\ndata: {\"message\":\"private-secret").is_empty());
        let output = redactor.push(&vec![b'x'; MAX_FRAME_BYTES]);
        assert!(!String::from_utf8_lossy(&output).contains("private-secret"));
        assert!(redactor.closed);
        assert!(redactor.push(b"\n\n").is_empty());
    }

    #[test]
    fn incomplete_error_at_eof_is_generic_and_completed_streams_stay_unchanged() {
        let mut redactor = Redactor::new(&ProviderKey::default(), "private-secret");
        assert!(redactor.push(b"event: error\ndata: {\"message\":\"private-secret").is_empty());
        let error = String::from_utf8(redactor.finish()).unwrap();
        assert!(!error.contains("private-secret"));
        assert!(error.contains("error"));
        assert!(redactor.finish().is_empty());
        let mut redactor = Redactor::new(&ProviderKey::default(), "private-secret");
        let done = b"event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
        assert_eq!(redactor.push(done), done);
        assert!(redactor.finish().is_empty());
    }
}
