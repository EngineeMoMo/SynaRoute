use crate::model::ProviderKey;
use serde_json::Value;
use zeroize::Zeroizing;

pub(crate) struct ErrorRedactor {
    secrets: Zeroizing<Vec<String>>,
}

impl ErrorRedactor {
    pub(crate) fn new(key: &ProviderKey, secret: &str) -> Self {
        let mut secrets: Vec<String> = crate::proxy::custom_headers::headers_for(key)
            .into_iter().map(|(_, value)| value).collect();
        secrets.push(secret.into());
        secrets.retain(|value| !value.is_empty());
        secrets.sort_by_key(|value| std::cmp::Reverse(value.len()));
        Self { secrets: Zeroizing::new(secrets) }
    }

    pub(crate) fn message(&self, text: &str) -> String {
        self.secrets.iter().fold(text.to_owned(), |text, secret| crate::diagnostics::redact_secret(&text, secret))
    }

    pub(crate) fn body(&self, bytes: &[u8]) -> String {
        match serde_json::from_slice::<Value>(bytes) {
            Ok(mut value) => { self.value(&mut value); value.to_string() }
            Err(_) => {
                let text = String::from_utf8_lossy(bytes);
                if text.contains('\\') || text.trim_start().starts_with(['{', '[', '"']) {
                    "Upstream returned an invalid error body".into()
                } else { self.message(&text) }
            }
        }
    }

    pub(crate) fn value(&self, value: &mut Value) {
        match value {
            Value::String(text) => *text = self.message(text),
            Value::Array(values) => { for value in values { self.value(value); } }
            Value::Object(values) => {
                *values = std::mem::take(values).into_iter().map(|(key, mut value)| {
                    self.value(&mut value);
                    (self.message(&key), value)
                }).collect();
            }
            Value::Number(number) if self.secrets.iter().any(|secret| *secret == number.to_string()) => {
                *value = Value::String("***".into());
            }
            _ => {}
        }
    }
}

pub(crate) fn body(key: &ProviderKey, secret: &str, text: &str) -> String {
    ErrorRedactor::new(key, secret).body(text.as_bytes())
}

#[cfg(test)]
#[path = "error_redaction_tests.rs"]
mod tests;
