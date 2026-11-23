use super::{catalog, config::{AffinityMode, PrivacyMode}, identity::{self, Hints}, intent, privacy, runtime::Lease, Store};
use crate::{model::{CategoryType, Protocol, ProviderKey}, proxy::ResBody};
use hyper::{Response, StatusCode};
use serde_json::Value;
use std::{sync::Arc, time::Instant};

#[derive(Clone, Default)]
pub struct Decision { pub model: String, pub lease: Option<Lease>, pub keep_key: bool, continuation: Option<super::runtime::Binding> }

fn reject(status: StatusCode, message: &str) -> Box<Response<ResBody>> {
    let mut response = Response::new(crate::proxy::full_body(bytes::Bytes::from(serde_json::json!({"type":"error","error":{"type":"routing_policy_error","message":message}}).to_string())));
    *response.status_mut() = status;
    response.headers_mut().insert("content-type", hyper::header::HeaderValue::from_static("application/json"));
    Box::new(response)
}

#[allow(clippy::too_many_arguments)]
pub async fn prepare(store: &Arc<Store>, category: CategoryType, body: &Value, protocol: Protocol, hints: &Hints, baseline: &str, deadline: Option<Instant>) -> Result<Decision, Box<Response<ResBody>>> {
    // Reject before candidate failure accounting: a locked vault is not an upstream failure.
    if store.secrets.read().is_locked() {
        return Err(reject(StatusCode::SERVICE_UNAVAILABLE, "密钥库已用主口令加密但尚未解锁：请打开 SynaRoute 主窗口输入主口令解锁后重试。"));
    }
    let settings = super::settings(store).map_err(|_| reject(StatusCode::SERVICE_UNAVAILABLE, "路由策略无法加载，请打开 SynaRoute 检查配置；请求未发送"))?;
    let policy = settings.categories.get(&category).cloned().unwrap_or_default();
    let automatic = super::config::is_auto_id(baseline);
    if policy.affinity == AffinityMode::Off && policy.privacy.mode == PrivacyMode::Off && !automatic {
        return Ok(Decision { model:baseline.into(), lease:None, keep_key:false, continuation:None });
    }
    privacy::enforce(store, category, body, protocol, &policy.privacy)
        .map_err(|_| reject(StatusCode::UNPROCESSABLE_ENTITY, "敏感内容防护已拦截请求或无法完成检查，未发送给任何模型；请到 SynaRoute 路由策略查看状态"))?;
    let identity = identity::identify(hints, body, category, protocol);
    let keys = store.enabled_keys_sorted(category);
    let upstream = super::upstream_identity::Snapshot::capture(store, category, &keys)
        .map_err(|_| reject(StatusCode::SERVICE_UNAVAILABLE, "无法确认上游身份，请解锁密钥库并重试"))?;
    let fingerprint = upstream.fingerprint(&catalog::fingerprint(&keys, store.get_settings().active_models.get(&category), settings.revision, baseline));
    let previous = body.get("previous_response_id").filter(|value| !value.is_null());
    let continuation = if protocol == Protocol::OpenaiResponses {
        identity.as_ref().and_then(|identity| previous.and_then(Value::as_str)
            .and_then(|id| store.routing_runtime.response_for(category, &identity.session, id, &fingerprint)))
    } else { None };
    if automatic && previous.is_some() && continuation.is_none() {
        return Err(reject(StatusCode::UNPROCESSABLE_ENTITY, "自动路由尚未观察到此会话的响应引用，未发送请求；请选择原模型继续"));
    }
    let mut lease = if automatic || policy.affinity != AffinityMode::Off {
        identity.as_ref().and_then(|id| store.routing_runtime.begin(category, &id.session, &id.turn,
            continuation.as_ref().map_or(id.position, |response| response.position.saturating_add(1)), &fingerprint, policy.affinity))
    } else { None };
    if let Some(lease) = &mut lease { lease.upstream = Some(upstream); }
    let (model, reason) = if automatic {
        let group = catalog::resolve_group(&policy.groups, baseline).ok_or_else(|| reject(StatusCode::NOT_FOUND, "自动路由入口已关闭或删除，请刷新客户端模型列表或选择真实模型"))?;
        if let Some(response) = &continuation {
            if lease.is_none() { return Err(reject(StatusCode::UNPROCESSABLE_ENTITY, "续传所属回合已过期，未发送请求")); }
            (response.model.clone(), "response_reference")
        } else { intent::select(store, category, group, identity.as_ref(), lease.as_ref(), &keys, identity::opaque_history(body), &policy.privacy, deadline).await
            .map_err(|_| reject(StatusCode::UNPROCESSABLE_ENTITY, "自动路由无法安全处理本次输入；请检查路由策略，或选择原有真实模型继续"))?
        }
    } else { (baseline.to_owned(), "manual") };
    if deadline.is_some_and(|d| d <= Instant::now()) {
        return Err(reject(StatusCode::SERVICE_UNAVAILABLE, "路由判断已耗尽本次故障转移预算，未开始新的上游请求"));
    }
    let fresh = super::settings(store).map_err(|_| reject(StatusCode::SERVICE_UNAVAILABLE, "路由配置无法确认，请刷新后重试"))?;
    let current_keys = store.enabled_keys_sorted(category);
    let current_upstream = super::upstream_identity::Snapshot::capture(store, category, &current_keys)
        .map_err(|_| reject(StatusCode::CONFLICT, "上游身份无法确认，请重试"))?;
    if current_upstream.fingerprint(&catalog::fingerprint(&current_keys, store.get_settings().active_models.get(&category), fresh.revision, baseline)) != fingerprint {
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
    Ok(Decision { model, lease, keep_key: policy.affinity != AffinityMode::Off, continuation: continuation.map(|response| response.binding) })
}

impl Decision {
    pub fn rank(&self, store: &Store, candidates: &mut Vec<ProviderKey>) {
        if let Some(binding) = &self.continuation {
            candidates.retain(|key| key.id == binding.key_id && key.resolve_model(&self.model) == binding.real_model);
            return;
        }
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
    pub fn observe_response(&self, store: &Store, key: &ProviderKey, real: &str, protocol: Protocol, body: &Value) {
        if protocol != Protocol::OpenaiResponses || key.protocol != protocol { return; }
        if !matches!(body["status"].as_str(), Some("completed" | "incomplete")) { return; }
        if let (Some(lease), Some(id)) = (&self.lease, body["id"].as_str()) {
            store.routing_runtime.observe_response(lease, id, &key.id, real);
        }
    }
    pub fn failed_candidate(&self, store: &Store, key: &ProviderKey) {
        if let Some(lease) = &self.lease { store.routing_runtime.forget_key(lease, &key.id); }
    }
}

pub fn stream_end(store: &Arc<Store>, lease: &Option<Lease>, errored: bool) {
    if errored { if let Some(lease) = lease { store.routing_runtime.failed(lease); } }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;

    #[tokio::test]
    async fn boxed_rejection_preserves_http_contract() {
        let response = *reject(StatusCode::CONFLICT, "fixture rejection");
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(response.headers()["content-type"], "application/json");
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["error"]["type"], "routing_policy_error");
        assert_eq!(body["error"]["message"], "fixture rejection");
    }
}
