use super::{DecisionConfig, Store};
use crate::model::{CategoryType, TokenUsage};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub(crate) fn key_id(config: &DecisionConfig) -> String {
    let mut digest = Sha256::new();
    digest.update(config.endpoint.as_bytes());
    digest.update([0]);
    digest.update(config.secret_id.as_bytes());
    format!("decision:{:x}", digest.finalize())
}

pub(crate) fn name(key_id: &str) -> Option<String> {
    key_id.strip_prefix("decision:").filter(|suffix| suffix.len() == 64 && suffix.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .map(|_| "动态思考判断服务 / Decision judge".into())
}

pub(crate) fn configured_slots(store: &Store) -> Vec<(CategoryType, String)> {
    store.config.read().decision.iter().filter(|(_, config)| !config.endpoint.is_empty() && !config.model.is_empty())
        .map(|(category, config)| (*category, key_id(config))).collect()
}

pub(super) struct RequestUsage<'store> {
    store: &'store Store,
    category: CategoryType,
    key_id: String,
    model: String,
    usage: Option<TokenUsage>,
    received: bool,
}

impl<'store> RequestUsage<'store> {
    pub fn new(store: &'store Store, category: CategoryType, config: &DecisionConfig) -> Self {
        Self { store, category, key_id: key_id(config), model: config.model.clone(), usage: None, received: false }
    }

    pub fn observe(&mut self, body: &Value) {
        self.received = true;
        self.usage = crate::upstream::extract_usage(body);
        if let Some(model) = body.get("model").and_then(Value::as_str).filter(|model| !model.is_empty() && model.len() <= 200) {
            self.model = model.into();
        }
    }
}

impl Drop for RequestUsage<'_> {
    fn drop(&mut self) {
        let detail = if self.usage.is_some() {
            "动态思考判断用量 / Decision judge usage"
        } else if self.received {
            "动态思考判断服务未返回可计费用量，成本未知 / Decision judge returned no billable usage; cost unknown"
        } else {
            "动态思考判断请求未完整返回，用量和成本未知 / Decision judge request incomplete; usage and cost unknown"
        };
        self.store.append_event_model(self.category, "system", Some(&self.key_id), detail, None,
            Some(format!("decision-usage:{}:{}", self.key_id, self.model)), self.usage.take(), &self.model);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn usage_survives_uncertain_answers_and_unknown_is_not_zero() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::new_at(directory.path().join("config.json"), directory.path().join("secrets.enc")).unwrap();
        let config = DecisionConfig { endpoint: "https://judge.invalid/v1/chat/completions".into(), model: "configured-model".into(), ..Default::default() };
        {
            let mut request = RequestUsage::new(&store, CategoryType::Codex, &config);
            request.observe(&json!({"model":"actual-model", "choices":[], "usage":{"prompt_tokens":100,"completion_tokens":4,"prompt_tokens_details":{"cached_tokens":25}}}));
        }
        let rows = store.token_usage_by_model();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].key_id, key_id(&config));
        assert_eq!(rows[0].model, "actual-model");
        assert_eq!(rows[0].usage.input, 75);
        assert_eq!(rows[0].usage.cache_read, 25);
        drop(RequestUsage::new(&store, CategoryType::Codex, &config));
        assert_eq!(store.token_usage_by_model()[0].usage.output, 4);
        assert!(name(&key_id(&config)).is_some());
        assert!(!key_id(&config).contains("judge.invalid"));
        let costs = crate::usage_cost::rows(&store);
        let row = costs.iter().find(|row| row.key_id == key_id(&config)).unwrap();
        assert!(!row.key_deleted);
        assert!(row.cost_nano.is_none());
        assert!(row.key_name.as_deref().unwrap().contains("Decision judge"));
        assert!(store.flush_usage_if_dirty());
        drop(store);
        let reopened = Store::new_at(directory.path().join("config.json"), directory.path().join("secrets.enc")).unwrap();
        assert_eq!(reopened.token_usage_by_model()[0].usage.input, 75);
    }
}
