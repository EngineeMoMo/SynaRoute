use super::{catalog, config::{AffinityMode, PrivacyMode}, identity::{self, Hints}, intent, privacy, runtime::Lease, Store};
use crate::{model::{CategoryType, Protocol, ProviderKey}, proxy::ResBody};
use hyper::{Response, StatusCode};
use serde_json::Value;
use std::{sync::Arc, time::Instant};

#[derive(Clone, Default)]
pub struct Decision { pub model: String, pub lease: Option<Lease>, pub keep_key: bool }

fn reject(status: StatusCode, message: &str) -> Response<ResBody> {
    let mut response = Response::new(crate::proxy::full_body(bytes::Bytes::from(serde_json::json!({"type":"error","error":{"type":"routing_policy_error","message":message}}).to_string())));
    *response.status_mut() = status;
    response.headers_mut().insert("content-type", hyper::header::HeaderValue::from_static("application/json"));
    response
}

#[allow(clippy::too_many_arguments)]
pub async fn prepare(store: &Arc<Store>, category: CategoryType, body: &Value, protocol: Protocol, hints: &Hints, baseline: &str, deadline: Option<Instant>) -> Result<Decision, Response<ResBody>> {
    // Reject before candidate failure accounting: a locked vault is not an upstream failure.
    if store.secrets.read().is_locked() {
        return Err(reject(StatusCode::SERVICE_UNAVAILABLE, "密钥库已用主口令加密但尚未解锁：请打开 SynaRoute 主窗口输入主口令解锁后重试。"));
    }
    let settings = super::settings(store).map_err(|_| reject(StatusCode::SERVICE_UNAVAILABLE, "路由策略无法加载，请打开 SynaRoute 检查配置；请求未发送"))?;
    let policy = settings.categories.get(&category).cloned().unwrap_or_default();
    let automatic = super::config::is_auto_id(baseline);
    if policy.affinity == AffinityMode::Off && policy.privacy.mode == PrivacyMode::Off && !automatic {
        return Ok(Decision { model:baseline.into(), lease:None, keep_key:false });
    }
    privacy::enforce(store, category, body, protocol, &policy.privacy)
        .map_err(|_| reject(StatusCode::UNPROCESSABLE_ENTITY, "敏感内容防护已拦截请求或无法完成检查，未发送给任何模型；请到 SynaRoute 路由策略查看状态"))?;
    let identity = identity::identify(hints, body, category, protocol);
    let keys = store.enabled_keys_sorted(category);
    let fingerprint = catalog::fingerprint(&keys, store.get_settings().active_models.get(&category), settings.revision, baseline);
    let lease = if automatic || policy.affinity != AffinityMode::Off {
        identity.as_ref().and_then(|id| store.routing_runtime.begin(category, &id.session, &id.turn, id.position, &fingerprint, policy.affinity))
    } else { None };
    let (model, reason) = if automatic {
        let group = catalog::resolve_group(&policy.groups, baseline).ok_or_else(|| reject(StatusCode::NOT_FOUND, "自动路由入口已关闭或删除，请刷新客户端模型列表或选择真实模型"))?;
        intent::select(store, category, group, identity.as_ref(), lease.as_ref(), &keys, identity::opaque_history(body), &policy.privacy, deadline).await
            .map_err(|_| reject(StatusCode::UNPROCESSABLE_ENTITY, "自动路由无法安全处理本次输入；请检查路由策略，或选择原有真实模型继续"))?
    } else { (baseline.to_owned(), "manual") };
    if deadline.is_some_and(|d| d <= Instant::now()) {
        return Err(reject(StatusCode::SERVICE_UNAVAILABLE, "路由判断已耗尽本次故障转移预算，未开始新的上游请求"));
    }
    let fresh = super::settings(store).map_err(|_| reject(StatusCode::SERVICE_UNAVAILABLE, "路由配置无法确认，请刷新后重试"))?;
    let current_keys = store.enabled_keys_sorted(category);
    if catalog::fingerprint(&current_keys, store.get_settings().active_models.get(&category), fresh.revision, baseline) != fingerprint {
        return Err(reject(StatusCode::CONFLICT, "判断期间路由配置已变化，未开始新的上游请求，请重试"));
    }
    let model = match &lease {
        Some(lease) => store.routing_runtime.choose(lease, &model)
            .ok_or_else(|| reject(StatusCode::CONFLICT, "该回合的路由状态已过期，请重试"))?,
        None => model,
    };
    store.routing_runtime.update(category, |activity| {
        activity.identity = identity.as_ref().map(|id| id.source).unwrap_or("unidentified").into();
        activity.selection = reason.into(); activity.selected_model = Some(model.clone()); activity.kept_key = None;
    });
    Ok(Decision { model, lease, keep_key: policy.affinity != AffinityMode::Off })
}

impl Decision {
    pub fn rank(&self, store: &Store, candidates: &mut [ProviderKey]) {
        if !self.keep_key { return; }
        let Some(lease) = &self.lease else { return; };
        let Some(binding) = store.routing_runtime.state(lease).and_then(|s| s.binding) else { return; };
        let Some(index) = candidates.iter().position(|k| k.id == binding.key_id && k.resolve_model(&self.model) == binding.real_model) else { return; };
        if index == 0 { return; }
        let Some(first) = candidates.first() else { return; };
        let key = &candidates[index];
        if !crate::health::is_candidate_for_model(&key.health, Some(&binding.real_model)) || crate::health::quota_window::active(&key.id) { return; }
        let over = crate::usage_cost::over_budget_ids(store, lease.category);
        if !crate::proxy::model_pool::same_routing_class(first, key, &self.model, &over) { return; }
        candidates[..=index].rotate_right(1);
        store.routing_runtime.update(lease.category, |activity| activity.kept_key = Some(binding.key_id));
    }
    pub fn served(&self, store: &Store, key: &ProviderKey, real: &str) {
        if let Some(lease) = &self.lease { store.routing_runtime.served(lease, &key.id, real); }
    }
    pub fn failed_candidate(&self, store: &Store, key: &ProviderKey) {
        if let Some(lease) = &self.lease { store.routing_runtime.forget_key(lease, &key.id); }
    }
}

pub fn stream_end(store: &Arc<Store>, lease: &Option<Lease>, errored: bool) {
    if errored { if let Some(lease) = lease { store.routing_runtime.failed(lease); } }
}
