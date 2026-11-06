pub mod config;
pub mod runtime;
pub mod catalog;
pub mod identity;
pub mod detectors;
pub mod privacy;
pub mod scenes;
pub mod intent;
pub mod policy;
#[cfg(test)] mod matrix_tests;
#[cfg(test)] mod affinity_http_tests;

use super::Store;
use crate::{error::AppResult, model::{CategoryType, ProviderKey}};
use config::{CategoryPolicy, RoutingConfig, Scene, Settings};
use serde::Serialize;
use sha2::{Digest, Sha256};

pub fn hash_bytes(bytes: &[u8]) -> String { format!("{:x}", Sha256::digest(bytes)) }

pub fn settings(store: &Store) -> AppResult<Settings> {
    if store.config_degraded.load(std::sync::atomic::Ordering::Acquire) {
        return Err(config::invalid("应用配置未能加载，路由策略无法确认；请求未发送"));
    }
    let cfg = store.config.read();
    let settings = cfg.routing.settings()?;
    if settings.scenes.len() > 32 { return Err(config::invalid("场景数量超过上限")); }
    for policy in settings.categories.values() { policy.validate()?; }
    Ok(settings.clone())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyOption { pub id: String, pub name: String, pub enabled: bool, pub models: Vec<String> }
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CategoryView {
    pub category_id: CategoryType,
    pub policy: CategoryPolicy,
    pub keys: Vec<KeyOption>,
    pub models: Vec<String>,
    pub current: config::SceneRoute,
    pub issues: Vec<String>,
    pub activity: Option<runtime::Activity>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct View {
    pub revision: u64,
    pub categories: Vec<CategoryView>,
    pub scenes: Vec<Scene>,
    pub vault_locked: bool,
}

pub fn view(store: &Store) -> AppResult<View> {
    let _edit = store.routing_runtime.edit.lock();
    let cfg = store.snapshot_config();
    let settings = settings(store)?;
    let categories = CategoryType::ALL.into_iter().map(|category_id| {
        let policy = settings.categories.get(&category_id).cloned().unwrap_or_default();
        let mut keys: Vec<ProviderKey> = cfg.keys.iter().filter(|k| k.category_id == category_id).cloned().collect();
        keys.sort_by_key(|k| k.priority);
        let current = scenes::current(&cfg, category_id);
        let enabled: Vec<_> = keys.iter().filter(|k| k.enabled).cloned().collect();
        let models = crate::proxy::discoverable_models(&enabled);
        let issues = policy.issues(&keys);
        let activity = store.routing_runtime.activities.lock().get(&category_id).cloned();
        CategoryView { category_id, policy, models, current, issues, activity, keys: keys.into_iter().map(|k| KeyOption {
            id:k.id, name:k.name, enabled:k.enabled, models:k.models.into_iter().map(|m| m.real_name).collect(),
        }).collect() }
    }).collect();
    Ok(View { revision:settings.revision, scenes:settings.scenes, categories, vault_locked: store.secrets.read().is_locked() })
}

pub fn save_policy(store: &Store, category: CategoryType, mut policy: CategoryPolicy, terms: Option<Vec<String>>, revision: u64) -> AppResult<()> {
    let _edit = store.routing_runtime.edit.lock();
    let current = settings(store)?;
    if current.revision != revision { return Err(config::invalid("路由策略已变化，请刷新后重试")); }
    let previous = current.categories.get(&category).cloned().unwrap_or_default();
    policy.privacy.terms_secret = previous.privacy.terms_secret.clone();
    policy.privacy.term_count = previous.privacy.term_count;
    let mut new_secret = None;
    if let Some(mut terms) = terms {
        if terms.len() > 100 || terms.iter().any(|s| s.trim().chars().count() < 3 || s.len() > 256 || s.contains(['\r', '\n', '\0'])) {
            return Err(config::invalid("敏感词最多 100 个，每个至少 3 个字符且不超过 256 字节，不含换行"));
        }
        for term in &mut terms { *term = term.trim().to_owned(); }
        terms.sort(); terms.dedup();
        policy.privacy.term_count = terms.len();
        policy.privacy.terms_secret = if terms.is_empty() { None } else {
            let id = format!("{}{}", config::TERMS_PREFIX, uuid::Uuid::new_v4());
            let encoded = zeroize::Zeroizing::new(serde_json::to_string(&terms)?);
            store.secrets.write().set(&id, &encoded)?;
            new_secret = Some(id.clone()); Some(id)
        };
        use zeroize::Zeroize;
        terms.zeroize();
    }
    let result = (|| {
        policy.validate()?;
        let keys = store.list_keys(category);
        let issues = policy.issues(&keys);
        if !issues.is_empty() && policy.groups != previous.groups { return Err(config::invalid(&issues.join("；"))); }
        store.mutate_and_persist(|cfg| {
            let mut settings = cfg.routing.settings()?.clone();
            if settings.revision != revision { return Err(config::invalid("路由策略已变化，请刷新后重试")); }
            settings.categories.insert(category, policy.clone());
            settings.revision += 1;
            cfg.routing = RoutingConfig::Known(Box::new(settings));
            Ok(())
        })
    })();
    if result.is_err() {
        if let Some(id) = new_secret { let _ = store.secrets.write().remove(&id); }
    } else {
        if previous.privacy.terms_secret != policy.privacy.terms_secret {
            if let Some(id) = previous.privacy.terms_secret { let _ = store.secrets.write().remove(&id); }
        }
        store.routing_runtime.reset(category);
    }
    result
}

#[tauri::command]
pub async fn routing_policies(state: tauri::State<'_, crate::AppState>) -> Result<View, String> {
    let store = state.store.clone();
    tauri::async_runtime::spawn_blocking(move || view(&store).map_err(|e| e.to_string())).await.map_err(|_| "无法读取路由策略".to_owned())?
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveResult { pub view: View, pub sync: Vec<crate::tools::scene_config::SyncResult> }
#[tauri::command]
pub async fn save_routing_policy(app: tauri::AppHandle, state: tauri::State<'_, crate::AppState>, category_id: CategoryType, policy: CategoryPolicy, terms: Option<Vec<String>>, revision: u64) -> Result<SaveResult, String> {
    let store = state.store.clone(); let proxy = state.proxy.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        let _edit = store.routing_runtime.edit.lock();
        save_policy(&store, category_id, policy, terms, revision)?;
        let sync = crate::tools::scene_config::sync_current(&store, &proxy, &[category_id]);
        Ok::<_, crate::error::AppError>(SaveResult { view: view(&store)?, sync })
    }).await.map_err(|_| "保存路由策略失败".to_owned())?.map_err(|e| e.to_string())?;
    let _ = crate::rebuild_tray(&app);
    Ok(result)
}
#[tauri::command]
pub fn clear_routing_sessions(state: tauri::State<'_, crate::AppState>, category_id: CategoryType) {
    state.store.routing_runtime.reset(category_id);
    crate::events::emit(crate::events::Topic::Settings, Some(category_id));
}
#[tauri::command]
pub fn routing_models(state: tauri::State<'_, crate::AppState>, category_id: CategoryType) -> Vec<String> {
    catalog::models(&state.store, category_id)
}
