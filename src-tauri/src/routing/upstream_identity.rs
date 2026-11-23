use super::{runtime::Lease, Store};
use crate::{error::{AppError, AppResult}, model::{CategoryType, ProviderKey}};
use std::{collections::BTreeMap, sync::Arc};
use zeroize::Zeroizing;

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub revision: u64,
    identities: BTreeMap<String, String>,
}

fn identity(key: &ProviderKey, secret: Option<&str>) -> String {
    let bytes = Zeroizing::new(serde_json::to_vec(&(
        &key.id, &key.base_url, key.protocol, crate::proxy::custom_headers::headers_for(key), secret,
    )).expect("upstream identity is serializable"));
    super::hash_bytes(&bytes)
}

pub fn changed(previous: &ProviderKey, next: &ProviderKey) -> bool {
    previous.category_id != next.category_id || identity(previous, None) != identity(next, None)
}

impl Snapshot {
    pub fn capture(store: &Store, category: CategoryType, keys: &[ProviderKey]) -> AppResult<Arc<Self>> {
        let _edit = store.routing_runtime.edit.lock();
        let secrets = store.secrets.read();
        let identities = keys.iter().map(|key| {
            let secret = secrets.get(&key.id)?;
            Ok((key.id.clone(), identity(key, secret.as_deref().map(String::as_str))))
        }).collect::<AppResult<_>>()?;
        Ok(Arc::new(Self { revision: store.routing_runtime.upstream_revision(category), identities }))
    }

    pub fn fingerprint(&self, config: &str) -> String {
        super::hash_bytes(&serde_json::to_vec(&(config, self.revision, &self.identities)).expect("routing identity is serializable"))
    }
}

pub fn secret_for_request(store: &Store, key: &ProviderKey, lease: Option<&Lease>) -> AppResult<Option<Zeroizing<String>>> {
    let Some(snapshot) = lease.and_then(|lease| lease.upstream.as_ref()) else { return store.secret_for(&key.id); };
    let _edit = store.routing_runtime.edit.lock();
    let current = store.get_key(&key.id);
    let secrets = store.secrets.read();
    let secret = secrets.get(&key.id)?;
    if snapshot.revision != store.routing_runtime.upstream_revision(key.category_id)
        || current.as_ref().is_none_or(|current| changed(current, key) || !current.enabled)
        || snapshot.identities.get(&key.id) != Some(&identity(key, secret.as_deref().map(String::as_str))) {
        return Err(AppError::Invalid("上游身份已变化，旧请求未发送；请使用当前配置重新发起请求".into()));
    }
    Ok(secret)
}

#[cfg(test)]
#[path = "upstream_identity_tests.rs"]
mod tests;
