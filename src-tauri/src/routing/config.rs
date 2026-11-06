use crate::model::{CategoryType, ProviderKey};
use crate::error::{AppError, AppResult};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};

pub const AUTO_PREFIX: &str = "claude-synaroute-auto-";
pub const TERMS_PREFIX: &str = "__routing_terms_";
pub const MAX_GROUPS: usize = 16;
pub const MAX_RULES: usize = 16;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RoutingConfig {
    Known(Box<Settings>),
    Unsupported(serde_json::Value),
}
impl Default for RoutingConfig {
    fn default() -> Self { Self::Known(Box::default()) }
}
impl RoutingConfig {
    pub fn settings(&self) -> AppResult<&Settings> {
        match self {
            Self::Known(settings) if settings.version == 1 => Ok(settings),
            _ => Err(invalid("路由策略格式不受支持；请检查配置，未发送请求")),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct Settings {
    pub version: u32,
    pub revision: u64,
    pub categories: BTreeMap<CategoryType, CategoryPolicy>,
    pub scenes: Vec<Scene>,
}
impl Default for Settings {
    fn default() -> Self {
        Self { version: 1, revision: 0, categories: BTreeMap::new(), scenes: Vec::new() }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AffinityMode { #[default] Off, Turn, Session }
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrivacyMode { #[default] Off, Warn, Block }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct PrivacyConfig {
    pub mode: PrivacyMode,
    pub credentials: bool,
    pub emails: bool,
    pub phones: bool,
    pub terms_secret: Option<String>,
    pub term_count: usize,
}
impl Default for PrivacyConfig {
    fn default() -> Self {
        Self { mode: PrivacyMode::Off, credentials: true, emails: false, phones: false, terms_secret: None, term_count: 0 }
    }
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct CategoryPolicy {
    pub affinity: AffinityMode,
    pub privacy: PrivacyConfig,
    pub groups: Vec<RouteGroup>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelRef { pub key_id: String, pub model: String }
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IntentRule { pub id: String, pub intent: String, pub model: String }
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RouteGroup {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub default_model: String,
    pub judge: Option<ModelRef>,
    pub rules: Vec<IntentRule>,
}
impl RouteGroup {
    pub fn model_id(&self) -> String { format!("{AUTO_PREFIX}{}", self.id) }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SceneRoute {
    pub category_id: CategoryType,
    pub primary_key_id: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Scene { pub id: String, pub name: String, pub routes: Vec<SceneRoute> }

pub fn invalid(message: &str) -> AppError { AppError::Invalid(message.into()) }
pub fn valid_text(value: &str, max: usize) -> bool {
    !value.trim().is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}
pub fn valid_id(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok() && value.len() <= 36
}
pub fn is_auto_id(model: &str) -> bool {
    model.strip_prefix(AUTO_PREFIX).is_some_and(valid_id)
}
pub fn valid_effort(value: &str) -> bool {
    matches!(value, "" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max" | "ultra")
}

impl CategoryPolicy {
    pub fn validate(&self) -> AppResult<()> {
        let p = &self.privacy;
        if self.groups.len() > MAX_GROUPS || p.term_count > 100
            || p.terms_secret.as_ref().is_some_and(|s| !s.starts_with(TERMS_PREFIX) || s.len() > 100)
            || (p.term_count > 0) != p.terms_secret.is_some() {
            return Err(invalid("路由策略或敏感词引用无效"));
        }
        let mut ids = HashSet::new();
        for group in &self.groups {
            if !valid_id(&group.id) || !ids.insert(&group.id) || !valid_text(&group.name, 120)
                || !valid_text(&group.default_model, 256) || is_auto_id(&group.default_model)
                || group.rules.len() > MAX_RULES || group.enabled && group.judge.is_none() {
                return Err(invalid("自动路由组无效：检查名称、默认模型、判断模型和规则数量"));
            }
            if let Some(judge) = &group.judge {
                if !valid_text(&judge.key_id, 128) || !valid_text(&judge.model, 256) || is_auto_id(&judge.model) {
                    return Err(invalid("判断模型必须是现有 Key 上的真实模型，不能使用自动路由组"));
                }
            }
            let mut rules = HashSet::new();
            for rule in &group.rules {
                if !valid_text(&rule.id, 64) || !rules.insert(&rule.id)
                    || !valid_text(&rule.intent, 512) || !valid_text(&rule.model, 256) || is_auto_id(&rule.model) {
                    return Err(invalid("意图规则无效：每条需要唯一 ID、意图说明和真实模型"));
                }
            }
        }
        Ok(())
    }

    pub fn issues(&self, keys: &[ProviderKey]) -> Vec<String> {
        let enabled: Vec<_> = keys.iter().filter(|k| k.enabled).cloned().collect();
        let models = crate::proxy::discoverable_models(&enabled);
        let mut problems = Vec::new();
        for group in &self.groups {
            if models.iter().any(|m| m == &group.model_id()) {
                problems.push(format!("自动入口与真实模型重名：{}", group.name));
            }
            if !models.contains(&group.default_model) {
                problems.push(format!("组「{}」的默认模型当前不可服务", group.name));
            }
            if group.rules.iter().any(|r| !models.contains(&r.model)) {
                problems.push(format!("组「{}」有目标模型当前不可服务", group.name));
            }
            if group.enabled && !group.judge.as_ref().is_some_and(|j| enabled.iter().any(|k|
                k.id == j.key_id && k.models.iter().any(|m| m.real_name == j.model))) {
                problems.push(format!("组「{}」的判断 Key 或真实模型当前不可用", group.name));
            }
        }
        problems
    }
}

impl Scene {
    pub fn validate(&self) -> AppResult<()> {
        let mut categories = HashSet::new();
        if !valid_id(&self.id) || !valid_text(&self.name, 120) || self.routes.is_empty() || self.routes.len() > 3 {
            return Err(invalid("场景名称、ID 或分类范围无效"));
        }
        for row in &self.routes {
            if !categories.insert(row.category_id)
                || row.primary_key_id.as_ref().is_some_and(|s| !valid_text(s, 128))
                || row.model.as_ref().is_some_and(|s| !valid_text(s, 256))
                || row.effort.as_ref().is_some_and(|s| !valid_effort(s))
                || row.category_id != CategoryType::Codex && row.effort.is_some() {
                return Err(invalid("场景分类重复或模型/档位无效"));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_are_off_and_unknown_policy_survives_serialization() {
        let config = crate::model::AppConfig::default();
        assert!(config.routing.settings().unwrap().categories.is_empty());
        assert_eq!(CategoryPolicy::default().privacy.mode, PrivacyMode::Off);
        let unknown = serde_json::json!({"version": 99, "privacy": "block", "future": true});
        let config: RoutingConfig = serde_json::from_value(unknown.clone()).unwrap();
        assert!(config.settings().is_err());
        assert_eq!(serde_json::to_value(config).unwrap(), unknown);
        let invalid = serde_json::json!({"version": 1, "categories":{"codex":{"affinity":"typo"}}});
        let config: RoutingConfig = serde_json::from_value(invalid.clone()).unwrap();
        assert!(config.settings().is_err());
        assert_eq!(serde_json::to_value(config).unwrap(), invalid);
    }
    #[test]
    fn virtual_ids_are_native_desktop_compatible() {
        let group = RouteGroup { id: uuid::Uuid::new_v4().to_string(), name: "Auto".into(), enabled: false, default_model:"real".into(), judge: None, rules: vec![] };
        assert!(is_auto_id(&group.model_id()));
        assert!(crate::model::is_desktop_acceptable_model_id(&group.model_id()));
        assert!(!is_auto_id("claude-synaroute-auto-not-an-id"));
        let mut policy = CategoryPolicy::default();
        policy.groups.push(group.clone());
        assert!(policy.validate().is_ok());
        policy.groups.push(group);
        assert!(policy.validate().is_err());
    }
}
