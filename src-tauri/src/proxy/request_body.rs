use serde_json::Value;
use std::ops::{Deref, DerefMut};

pub(super) enum AttemptBody<'request> {
    Borrowed { value: &'request mut Value, saved: Vec<(&'static str, Option<Value>)> },
    Owned(Value),
}

impl<'request> AttemptBody<'request> {
    pub(super) fn new(value: &'request mut Value, same_protocol: bool) -> Self {
        if !same_protocol { return Self::Owned(value.clone()); }
        let saved = ["model", "reasoning", "temperature", "top_p", "thinking", "max_tokens", "_pending_effort"]
            .into_iter().map(|field| (field, value.get(field).cloned())).collect();
        Self::Borrowed { value, saved }
    }

    pub(super) fn convert(&mut self, from: crate::model::Protocol, to: crate::model::Protocol) {
        if let Self::Owned(value) = self {
            *value = crate::upstream::convert_request_owned(std::mem::take(value), from, to);
        }
    }

    pub(super) fn detach(&mut self) {
        if matches!(self, Self::Borrowed { .. }) {
            *self = Self::Owned((**self).clone());
        }
    }
}

impl Deref for AttemptBody<'_> {
    type Target = Value;
    fn deref(&self) -> &Value {
        match self { Self::Borrowed { value, .. } => value, Self::Owned(value) => value }
    }
}

impl DerefMut for AttemptBody<'_> {
    fn deref_mut(&mut self) -> &mut Value {
        match self { Self::Borrowed { value, .. } => value, Self::Owned(value) => value }
    }
}

impl serde::Serialize for AttemptBody<'_> {
    fn serialize<Serializer: serde::Serializer>(&self, serializer: Serializer) -> Result<Serializer::Ok, Serializer::Error> {
        (**self).serialize(serializer)
    }
}

impl Drop for AttemptBody<'_> {
    fn drop(&mut self) {
        if let Self::Borrowed { value, saved } = self {
            if let Some(object) = value.as_object_mut() {
                for (field, original) in saved.drain(..) {
                    match original {
                        Some(value) => { object.insert(field.into(), value); }
                        None => { object.remove(field); }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn success_borrows_large_fields_and_restores_key_specific_parameters() {
        let mut original = json!({"model":"client", "messages":[{"content":"x".repeat(1024 * 1024)}], "temperature":0.4});
        let address = original["messages"][0]["content"].as_str().unwrap().as_ptr();
        {
            let mut attempt = AttemptBody::new(&mut original, true);
            assert_eq!(attempt["messages"][0]["content"].as_str().unwrap().as_ptr(), address);
            attempt["model"] = json!("upstream");
            attempt["temperature"] = json!(0.8);
            attempt["max_tokens"] = json!(100);
        }
        assert_eq!(original["model"], "client");
        assert_eq!(original["temperature"], 0.4);
        assert!(original.get("max_tokens").is_none());
        assert_eq!(original["messages"][0]["content"].as_str().unwrap().as_ptr(), address);
    }

    #[test]
    fn error_rectification_cannot_pollute_the_next_key() {
        let mut original = json!({"model":"client", "messages":[{"content":[{"type":"image", "data":"picture"}]}]});
        let expected = original.clone();
        {
            let mut attempt = AttemptBody::new(&mut original, true);
            attempt["model"] = json!("upstream");
            attempt.detach();
            attempt["messages"] = json!([]);
            assert_eq!(attempt["model"], "upstream");
        }
        assert_eq!(original, expected);
        {
            let mut translated = AttemptBody::new(&mut original, false);
            translated["messages"] = json!([]);
        }
        assert_eq!(original, expected);
    }

    #[test]
    fn all_attempt_parameters_are_restored_with_absent_and_null_values_distinct() {
        let mut original = json!({"model":"client", "reasoning":{"summary":"auto"},
            "temperature":null, "top_p":0.7, "thinking":{"type":"enabled", "budget_tokens":1024},
            "_pending_effort":"high", "messages":[]});
        let expected = original.clone();
        {
            let mut attempt = AttemptBody::new(&mut original, true);
            for field in ["model", "reasoning", "temperature", "top_p", "thinking", "max_tokens", "_pending_effort"] {
                attempt[field] = json!("modified");
            }
        }
        assert_eq!(original, expected);
    }

    #[test]
    #[ignore = "performance report: npm run perf"]
    fn measure_request_body_reuse() {
        let mut original = json!({"model":"client", "messages":[{"content":"x".repeat(1024 * 1024)}]});
        let samples = 300;
        let started = std::time::Instant::now();
        for _ in 0..samples {
            let mut cloned = std::hint::black_box(original.clone());
            cloned["model"] = json!("upstream");
            std::hint::black_box(&cloned);
        }
        let clone_us = started.elapsed().as_micros();
        let started = std::time::Instant::now();
        for _ in 0..samples {
            let mut attempt = AttemptBody::new(&mut original, true);
            attempt["model"] = json!("upstream");
            std::hint::black_box(&*attempt);
        }
        println!("SYNAROUTE_PERF {}", json!({"name":"request_body_reuse", "samples":samples,
            "clone_total_us":clone_us, "reuse_total_us":started.elapsed().as_micros(), "content_bytes":1024 * 1024}));
    }
}
