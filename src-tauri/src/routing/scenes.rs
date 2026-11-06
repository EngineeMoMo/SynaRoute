use super::{config::{invalid, Scene, SceneRoute, RoutingConfig}, runtime::PendingScene, hash_bytes, Store};
use crate::{error::AppResult, model::{AppConfig, CategoryType}, proxy::ProxyManager, tools::scene_config::{self, SyncResult}};
use serde::Serialize;
use std::{collections::HashSet, path::PathBuf, time::{Duration, Instant}};

pub fn current(cfg: &AppConfig, category_id: CategoryType) -> SceneRoute {
    SceneRoute {
        category_id,
        primary_key_id: cfg.keys.iter().filter(|k| k.category_id == category_id && k.enabled).min_by_key(|k| k.priority).map(|k| k.id.clone()),
        model: cfg.settings.active_models.get(&category_id).filter(|s| !s.trim().is_empty()).cloned(),
        effort: (category_id == CategoryType::Codex).then(|| cfg.settings.active_efforts.get(&category_id).cloned()).flatten(),
    }
}

fn fingerprint(cfg: &AppConfig) -> String {
    let categories: Vec<_> = CategoryType::ALL.into_iter().map(|cat| {
        let keys: Vec<_> = cfg.keys.iter().filter(|k| k.category_id == cat).cloned().collect();
        super::catalog::fingerprint(&keys, cfg.settings.active_models.get(&cat), cfg.routing.settings().map(|s| s.revision).unwrap_or(u64::MAX), "scene")
    }).collect();
    hash_bytes(&serde_json::to_vec(&(categories, &cfg.settings.active_efforts)).unwrap_or_default())
}

fn transform(cfg: &mut AppConfig, routes: &[SceneRoute]) -> AppResult<()> {
    let routing = cfg.routing.settings()?.clone();
    for route in routes {
        let enabled: Vec<_> = cfg.keys.iter().filter(|k| k.category_id == route.category_id && k.enabled).cloned().collect();
        if let Some(primary) = &route.primary_key_id {
            if !enabled.iter().any(|k| &k.id == primary) { return Err(invalid("场景中的主 Key 已删除、停用或不属于该分类")); }
        }
        if let Some(model) = &route.model {
            let available = super::catalog::advertised_for(&enabled, super::catalog::groups(&routing, route.category_id));
            if !available.iter().any(|m| &m.outward == model) { return Err(invalid("场景中的模型或自动入口已不可用，请编辑场景")); }
        }
    }
    for route in routes {
        if let Some(id) = &route.primary_key_id { crate::store::key_order::promote_in(cfg, route.category_id, id)?; }
        match &route.model { Some(model) => { cfg.settings.active_models.insert(route.category_id, model.clone()); }, None => { cfg.settings.active_models.remove(&route.category_id); } }
        if route.category_id == CategoryType::Codex {
            match &route.effort { Some(effort) if !effort.is_empty() => { cfg.settings.active_efforts.insert(route.category_id, effort.clone()); }, _ => { cfg.settings.active_efforts.remove(&route.category_id); } }
        }
    }
    Ok(())
}

pub fn save(store: &Store, mut scene: Scene, revision: u64) -> AppResult<()> {
    let _edit = store.routing_runtime.edit.lock();
    if scene.id.is_empty() { scene.id = uuid::Uuid::new_v4().to_string(); }
    scene.name = scene.name.trim().to_owned();
    scene.validate()?;
    let current = store.snapshot_config();
    let mut check = current.clone(); transform(&mut check, &scene.routes)?;
    store.mutate_and_persist(|cfg| {
        let mut settings = cfg.routing.settings()?.clone();
        if settings.revision != revision { return Err(invalid("场景或策略已变化，请刷新后重试")); }
        if let Some(previous) = settings.scenes.iter_mut().find(|s| s.id == scene.id) { *previous = scene; }
        else {
            if settings.scenes.len() >= 32 { return Err(invalid("最多保存 32 个场景")); }
            settings.scenes.push(scene);
        }
        settings.revision += 1;
        cfg.routing = RoutingConfig::Known(Box::new(settings));
        Ok(())
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Change { pub before: SceneRoute, pub after: SceneRoute, pub client: SyncResult, pub paths: Vec<String> }
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Preview { pub token: String, pub name: String, pub changes: Vec<Change>, pub expires_in_seconds: u64 }

pub fn preview(store: &Store, proxy: &ProxyManager, scene_id: &str) -> AppResult<Preview> {
    let _edit = store.routing_runtime.edit.lock();
    let cfg = store.snapshot_config();
    let scene = cfg.routing.settings()?.scenes.iter().find(|s| s.id == scene_id).cloned().ok_or_else(|| invalid("场景不存在"))?;
    scene.validate()?;
    let mut target = cfg.clone(); transform(&mut target, &scene.routes)?;
    let mut files = Vec::new(); let mut changes = Vec::new();
    for route in &scene.routes {
        let plan = scene_config::prepare(store, proxy, &target, route.category_id)?;
        changes.push(Change { before:current(&cfg, route.category_id), after:current(&target, route.category_id), paths:plan.paths(), client:plan.describe() });
        files.push(plan);
    }
    let token = uuid::Uuid::new_v4().to_string();
    let mut previews = store.routing_runtime.previews.lock();
    previews.retain(|_, p| p.expires > Instant::now());
    if previews.len() >= 8 { return Err(invalid("预览过多，请关闭旧预览后重试")); }
    previews.insert(token.clone(), PendingScene { scene_id:scene.id, fingerprint:fingerprint(&cfg), routes:scene.routes, files, expires:Instant::now() + Duration::from_secs(600) });
    Ok(Preview { token, name:scene.name, changes, expires_in_seconds:600 })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Applied { pub view: super::View, pub sync: Vec<SyncResult>, pub changed: bool, pub backup_path: Option<String> }

pub fn apply(store: &Store, proxy: &ProxyManager, token: &str) -> AppResult<Applied> {
    let _edit = store.routing_runtime.edit.lock();
    let plan = store.routing_runtime.previews.lock().remove(token).ok_or_else(|| invalid("预览不存在或已使用，请重新预览"))?;
    let before = store.snapshot_config();
    if plan.expires <= Instant::now() || plan.fingerprint != fingerprint(&before) { return Err(invalid("配置在预览后已变化或预览过期，请重新预览")); }
    if !before.routing.settings()?.scenes.iter().any(|s| s.id == plan.scene_id && s.routes == plan.routes) { return Err(invalid("场景已经变更，请重新预览")); }
    for files in &plan.files {
        files.validate_current()?;
        if files.status == "stopped" && proxy.is_running(files.category) || files.status != "stopped" && !proxy.is_running(files.category) { return Err(invalid("代理状态在预览后变化，请重新预览")); }
    }
    let categories: Vec<_> = plan.routes.iter().map(|r| r.category_id).collect();
    let mut target = before.clone(); transform(&mut target, &plan.routes)?;
    let changed = fingerprint(&target) != fingerprint(&before);
    let cfg_path = PathBuf::from(store.config_path_display());
    let backup = if changed {
        let name = format!("config.pre-scene-{}-{}.json", chrono::Utc::now().format("%Y%m%d-%H%M%S"), uuid::Uuid::new_v4());
        let path = cfg_path.with_file_name(name);
        crate::secret::atomic_write(&path, &serde_json::to_vec_pretty(&before)?)?;
        Some(path.display().to_string())
    } else { None };
    if changed {
        store.mutate_and_persist(|cfg| {
            if fingerprint(cfg) != plan.fingerprint { return Err(invalid("配置已经变化，请重新预览")); }
            transform(cfg, &plan.routes)
        })?;
    }
    let mut sync = Vec::new();
    for files in plan.files {
        sync.push(files.apply(cfg_path.parent().ok_or_else(|| invalid("数据目录不可用"))?));
    }
    for category in categories { store.routing_runtime.reset(category); }
    crate::events::emit(crate::events::Topic::Settings, None);
    Ok(Applied { view:super::view(store)?, sync, changed, backup_path:backup })
}

#[tauri::command]
pub async fn save_routing_scene(state: tauri::State<'_, crate::AppState>, scene: Scene, revision: u64) -> Result<super::View, String> {
    let store = state.store.clone();
    tauri::async_runtime::spawn_blocking(move || { save(&store, scene, revision)?; super::view(&store) }).await.map_err(|_| "场景保存失败".to_owned())?.map_err(|e| e.to_string())
}
#[tauri::command]
pub async fn delete_routing_scene(state: tauri::State<'_, crate::AppState>, scene_id: String, revision: u64) -> Result<super::View, String> {
    let store = state.store.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _edit = store.routing_runtime.edit.lock();
        store.mutate_and_persist(|cfg| {
            let mut settings = cfg.routing.settings()?.clone();
            if settings.revision != revision { return Err(invalid("场景已经变化，请刷新")); }
            let old = settings.scenes.len(); settings.scenes.retain(|s| s.id != scene_id);
            if old == settings.scenes.len() { return Err(invalid("场景不存在")); }
            settings.revision += 1; cfg.routing = RoutingConfig::Known(Box::new(settings)); Ok(())
        })?;
        super::view(&store)
    }).await.map_err(|_| "场景删除失败".to_owned())?.map_err(|e| e.to_string())
}
#[tauri::command]
pub async fn preview_routing_scene(state: tauri::State<'_, crate::AppState>, scene_id: String) -> Result<Preview, String> {
    let store = state.store.clone(); let proxy = state.proxy.clone();
    tauri::async_runtime::spawn_blocking(move || preview(&store, &proxy, &scene_id)).await.map_err(|_| "场景预览失败".to_owned())?.map_err(|e| e.to_string())
}
#[tauri::command]
pub async fn apply_routing_scene(app: tauri::AppHandle, state: tauri::State<'_, crate::AppState>, token: String) -> Result<Applied, String> {
    let store = state.store.clone(); let proxy = state.proxy.clone();
    let result = tauri::async_runtime::spawn_blocking(move || apply(&store, &proxy, &token)).await.map_err(|_| "场景应用失败".to_owned())?.map_err(|e| e.to_string())?;
    let _ = crate::rebuild_tray(&app); Ok(result)
}
#[tauri::command]
pub async fn sync_routing_clients(state: tauri::State<'_, crate::AppState>, categories: Vec<CategoryType>) -> Result<Vec<SyncResult>, String> {
    if categories.len() > 3 || categories.iter().collect::<HashSet<_>>().len() != categories.len() { return Err("分类列表无效".into()); }
    let store = state.store.clone(); let proxy = state.proxy.clone();
    tauri::async_runtime::spawn_blocking(move || { let _edit = store.routing_runtime.edit.lock(); scene_config::sync_current(&store, &proxy, &categories) }).await.map_err(|_| "模型同步失败".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ModelInfo, ProviderKey};
    use std::sync::Arc;
    #[test]
    fn routing_matrix_multicategory_validation_is_atomic() {
        let mut cfg = AppConfig::default();
        cfg.keys.push(ProviderKey { id: "cli-key".into(), category_id: CategoryType::ClaudeCli, enabled: true, ..Default::default() });
        let before = serde_json::to_value(&cfg).unwrap();
        let routes = vec![
            SceneRoute { category_id: CategoryType::ClaudeCli, primary_key_id: Some("cli-key".into()), model: None, effort: None },
            SceneRoute { category_id: CategoryType::Codex, primary_key_id: Some("missing".into()), model: None, effort: Some("high".into()) },
        ];
        assert!(transform(&mut cfg, &routes).is_err());
        assert_eq!(serde_json::to_value(&cfg).unwrap(), before);
    }

    #[test]
    fn scenes_validate_all_categories_before_mutation_and_reject_stale_previews() {
        let dir = std::env::temp_dir().join(format!("sr-scene-{}", uuid::Uuid::new_v4()));
        let store = Arc::new(Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap());
        let key = ProviderKey { id:"scene-one".into(), enabled:true, models:vec![ModelInfo { real_name:"m".into(), source:"manual".into(), fetched_at:None, context_window:None, max_output_tokens:None }], ..Default::default() };
        store.upsert_key(key.clone()).unwrap();
        let scene = Scene { id:uuid::Uuid::new_v4().to_string(), name:"Work".into(), routes:vec![SceneRoute { category_id:CategoryType::ClaudeCli, primary_key_id:Some(key.id), model:Some("m".into()), effort:None }] };
        save(&store, scene.clone(), 0).unwrap();
        let proxy = ProxyManager::new(store.clone());
        let p = preview(&store, &proxy, &scene.id).unwrap();
        store.routing_runtime.previews.lock().get_mut(&p.token).unwrap().expires = Instant::now() - Duration::from_secs(1);
        assert!(apply(&store, &proxy, &p.token).is_err(), "expired previews must never apply");
        assert!(store.active_model_of(CategoryType::ClaudeCli).is_none());
        let p = preview(&store, &proxy, &scene.id).unwrap();
        store.set_active_model(CategoryType::ClaudeCli, "another").unwrap();
        assert!(apply(&store, &proxy, &p.token).is_err());
        let p = preview(&store, &proxy, &scene.id).unwrap();
        let applied = apply(&store, &proxy, &p.token).unwrap();
        assert!(applied.changed); assert!(applied.backup_path.is_some());
        assert_eq!(store.active_model_of(CategoryType::ClaudeCli).as_deref(), Some("m"));
        assert_eq!(applied.sync[0].status, "stopped"); assert!(!proxy.is_running(CategoryType::ClaudeCli));
        assert!(apply(&store, &proxy, &p.token).is_err());
        let p = preview(&store, &proxy, &scene.id).unwrap();
        assert!(!apply(&store, &proxy, &p.token).unwrap().changed);
        let mut invalid_scene = scene.clone(); invalid_scene.routes.push(SceneRoute { category_id:CategoryType::Codex, primary_key_id:Some("wrong-category".into()), model:None, effort:None });
        assert!(save(&store, invalid_scene, 1).is_err());
        drop(proxy); drop(store); std::fs::remove_dir_all(dir).ok();
    }
}
