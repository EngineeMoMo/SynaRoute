use super::{config::{PrivacyConfig, RouteGroup}, identity::Identity, runtime::{group_fingerprint, Lease}, Store};
use crate::{error::AppResult, model::{CategoryType, ProviderKey}, upstream::routing_judge::JudgeError};
use std::{sync::Arc, time::{Duration, Instant}};

#[allow(clippy::too_many_arguments)]
pub async fn select(store: &Arc<Store>, category: CategoryType, group: &RouteGroup, identity: Option<&Identity>, lease: Option<&Lease>, keys: &[ProviderKey], private_history: bool, privacy: &PrivacyConfig, deadline: Option<Instant>) -> AppResult<(String, &'static str)> {
    let default = || (group.default_model.clone(), "default");
    let Some(identity) = identity else {
        if private_history { return Err(super::config::invalid("自动入口无法确认服务端续传所属模型；请使用真实模型继续对话")); }
        return Ok((group.default_model.clone(), "identity_missing"));
    };
    let Some(lease) = lease else {
        if private_history { return Err(super::config::invalid("无法确认签名历史的原模型，未发送请求")); }
        return Ok((group.default_model.clone(), "session_unavailable"));
    };
    let state = store.routing_runtime.state(lease);
    if let Some(model) = state.as_ref().and_then(|s| s.model.clone()) { return Ok((model, "turn_reused")); }
    if private_history {
        if let Some(model) = state.as_ref().and_then(|s| s.previous_model.clone()) {
            return Ok((model, "opaque_history"));
        }
        return Err(super::config::invalid("此自动入口尚未观察到原模型，不能安全切换带签名或服务端引用的历史；请选择真实模型"));
    }
    if !identity.new_turn { return Ok((group.default_model.clone(), "turn_not_seen")); }
    let Some(prompt) = identity.prompt.as_deref() else { return Ok((group.default_model.clone(), "no_text")); };
    if group.rules.is_empty() { return Ok(default()); }
    let Some(judge) = &group.judge else { return Ok((group.default_model.clone(), "judge_unavailable")); };
    let Some(key) = keys.iter().find(|k| k.id == judge.key_id && k.enabled && k.models.iter().any(|m| m.real_name == judge.model)) else {
        return Ok((group.default_model.clone(), "judge_unavailable"));
    };
    if !crate::health::is_candidate_for_model(&key.health, Some(&judge.model)) || crate::health::quota_window::active(&key.id)
        || crate::health::balance_gate::is_exhausted(key) || crate::usage_cost::over_budget_ids(store, category).contains(&key.id) {
        return Ok((group.default_model.clone(), "judge_unavailable"));
    }
    let judge_key = super::hash_bytes(&serde_json::to_vec(&(category, &key.id, &key.base_url, &judge.model)).unwrap_or_default());
    {
        let mut cooldowns = store.routing_runtime.cooldowns.lock();
        cooldowns.retain(|_, until| *until > Instant::now());
        if cooldowns.contains_key(&judge_key) { return Ok((group.default_model.clone(), "judge_cooldown")); }
    }
    let cache_key = super::hash_bytes(&serde_json::to_vec(&(&judge_key, group_fingerprint(group), &identity.session, &identity.turn, super::hash_bytes(prompt.as_bytes()))).unwrap_or_default());
    let limit = Duration::from_secs(4).min(deadline.map(|d| d.saturating_duration_since(Instant::now())).unwrap_or(Duration::from_secs(4)));
    if limit.is_zero() { return Ok((group.default_model.clone(), "judge_timeout")); }
    let Some(gate) = store.routing_runtime.judge_gate(&cache_key) else { return Ok((group.default_model.clone(), "judge_busy")); };
    let started = Instant::now();
    let Ok(_guard) = tokio::time::timeout(limit, gate.lock()).await else { return Ok((group.default_model.clone(), "judge_timeout")); };
    if store.routing_runtime.state(lease).is_none() { return Err(super::config::invalid("回合已过期，未发送判断请求")); }
    {
        let mut cooldowns = store.routing_runtime.cooldowns.lock();
        cooldowns.retain(|_, until| *until > Instant::now());
        if cooldowns.contains_key(&judge_key) { return Ok((group.default_model.clone(), "judge_cooldown")); }
    }
    let (rule, reason) = if let Some(cached) = store.routing_runtime.cached(&cache_key) { (cached, "cached") } else {
        let left = limit.saturating_sub(started.elapsed());
        if left.is_zero() { return Ok((group.default_model.clone(), "judge_timeout")); }
        match crate::upstream::routing_judge::classify(store, category, key, &judge.model, prompt, &group.rules, privacy, left).await {
            Ok(rule) => { store.routing_runtime.cache(cache_key, rule.clone()); (rule, "classified") }
            Err(JudgeError::Private) => return Err(super::config::invalid("敏感内容防护已拦截判断输入，未发送判断或业务请求")),
            Err(JudgeError::Unavailable(reason)) => {
                let mut cooldowns = store.routing_runtime.cooldowns.lock();
                if cooldowns.len() < 128 { cooldowns.insert(judge_key, Instant::now() + Duration::from_secs(30)); }
                return Ok((group.default_model.clone(), reason));
            }
        }
    };
    let target = rule.as_ref().and_then(|id| group.rules.iter().find(|r| &r.id == id)).map(|r| &r.model);
    if let Some(model) = target {
        if keys.iter().any(|k| k.enabled && crate::proxy::model_pool::serves_natively(k, model)) {
            return Ok((model.clone(), reason));
        }
        return Ok((group.default_model.clone(), "target_unavailable"));
    }
    Ok((group.default_model.clone(), "no_match"))
}
