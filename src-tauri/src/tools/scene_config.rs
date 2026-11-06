use super::{claude_cli_settings_path, claude_desktop_dirs, codex, desktop_meta_path, desktop_profile_path, write_without_locking_snapshot, DESKTOP_CONFIG_FILE, DESKTOP_PROFILE_ID};
use crate::{error::{AppError, AppResult}, model::{AppConfig, CategoryType, ProviderKey}, proxy::ProxyManager, store::{routing::{catalog, config::{self, RouteGroup}}, Store}};
use serde::Serialize;
use serde_json::Value;
use std::{fs, io::{Read, Write}, path::{Path, PathBuf}};

const MAX_CONFIG_BYTES: u64 = 16 * 1024 * 1024;

pub struct PreparedFile {
    path: PathBuf,
    before: Option<Vec<u8>>,
    after: Vec<u8>,
}
pub struct PreparedCategory {
    pub category: CategoryType,
    pub status: &'static str,
    files: Vec<PreparedFile>,
    watches: Vec<(PathBuf, Option<Vec<u8>>)>,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncResult {
    pub category_id: CategoryType,
    pub status: String,
    pub message: String,
    pub backup_dir: Option<String>,
}

fn error(message: &str) -> AppError { AppError::ToolConfig(message.into()) }
fn read(path: &Path) -> AppResult<Option<Vec<u8>>> {
    let mut file = match fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(error("客户端模型配置不可读，未改写文件")),
    };
    let mut bytes = Vec::new();
    (&mut file).take(MAX_CONFIG_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_CONFIG_BYTES { return Err(error("客户端配置超过安全读取上限")); }
    Ok(Some(bytes))
}
fn json(bytes: &[u8]) -> AppResult<Value> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| error("客户端 JSON 配置无法解析"))?;
    if !value.is_object() { return Err(error("客户端 JSON 配置顶层必须是对象")); }
    Ok(value)
}
fn toml(bytes: &[u8]) -> AppResult<toml::Value> {
    let text = std::str::from_utf8(bytes).map_err(|_| error("客户端 TOML 配置不是 UTF-8"))?;
    text.parse().map_err(|_| error("客户端 TOML 配置无法解析"))
}

impl PreparedCategory {
    fn empty(category: CategoryType, status: &'static str) -> Self {
        Self { category, status, files: Vec::new(), watches: Vec::new() }
    }
    fn watch(&mut self, path: &Path) -> AppResult<Option<Vec<u8>>> {
        let bytes = read(path)?;
        self.watches.push((path.to_path_buf(), bytes.clone()));
        Ok(bytes)
    }
    fn add(&mut self, path: PathBuf, before: Option<Vec<u8>>, after: Vec<u8>) {
        if before.as_deref() != Some(after.as_slice()) {
            self.files.push(PreparedFile { path, before, after });
        }
    }
    pub fn validate_current(&self) -> AppResult<()> {
        for (path, before) in &self.watches {
            if read(path)? != *before { return Err(error("客户端配置在预览后发生变化，请重新预览")); }
        }
        for file in &self.files {
            if read(&file.path)? != file.before { return Err(error("客户端模型文件在预览后发生变化，请重新预览")); }
        }
        Ok(())
    }
    pub fn paths(&self) -> Vec<String> { self.files.iter().map(|f| f.path.display().to_string()).collect() }
    pub fn describe(&self) -> SyncResult {
        let status = if self.status == "managed" && self.files.is_empty() { "unchanged" } else { self.status };
        SyncResult { category_id: self.category, status: status.into(), backup_dir: None,
            message: match status {
                "stopped" => "代理未运行，只应用本地选项，不改客户端配置",
                "not_managed" => "客户端未由 SynaRoute 接管，未改客户端配置",
                "unchanged" => "客户端模型配置已一致，无需写入",
                _ => "仅更新模型选择或模型清单，不改登录态、部署模式或历史会话",
            }.into() }
    }
    pub fn apply(self, data_dir: &Path) -> SyncResult {
        self.apply_with(data_dir, write_without_locking_snapshot)
    }
    fn apply_with(self, data_dir: &Path, mut write: impl FnMut(&Path, &[u8]) -> AppResult<()>) -> SyncResult {
        let mut outcome = self.describe();
        if self.files.is_empty() { return outcome; }
        let result = (|| -> AppResult<()> {
            self.validate_current()?;
            let backup_dir = data_dir.join("backups/routing-scenes").join(format!("{}-{}", chrono::Utc::now().format("%Y%m%d-%H%M%S"), uuid::Uuid::new_v4()));
            fs::create_dir_all(&backup_dir)?;
            for (index, file) in self.files.iter().enumerate() {
                if let Some(before) = &file.before {
                    let name = format!("{index}-{}", file.path.file_name().unwrap_or_default().to_string_lossy());
                    let mut options = fs::OpenOptions::new();
                    options.write(true).create_new(true);
                    #[cfg(unix)] { use std::os::unix::fs::OpenOptionsExt; options.mode(0o600); }
                    let mut backup = options.open(backup_dir.join(name))?;
                    backup.write_all(before)?;
                    backup.sync_all()?;
                }
            }
            outcome.backup_dir = Some(backup_dir.display().to_string());
            for file in &self.files {
                if read(&file.path)? != file.before { return Err(error("客户端配置发生变化；已完成的模型写入保留，未覆盖外部修改")); }
                write(&file.path, &file.after)?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                outcome.status = "synced".into();
                outcome.message = "模型配置已同步；现有会话可能保留旧选择，模型清单变化需重启客户端".into();
            }
            Err(_) => {
                outcome.status = "failed".into();
                outcome.message = "本地选项已保存，客户端模型配置未全部同步；请检查文件占用和权限后重试，已完成的写入未自动撤销".into();
            }
        }
        outcome
    }
}

fn cli_at(category: CategoryType, path: &Path, endpoint: &str, model: Option<&str>) -> AppResult<PreparedCategory> {
    let mut plan = PreparedCategory::empty(category, "not_managed");
    let Some(bytes) = plan.watch(path)? else { return Ok(plan); };
    let mut doc = json(&bytes)?;
    let Some(env) = doc.get_mut("env").and_then(Value::as_object_mut) else { return Ok(plan); };
    if env.get("ANTHROPIC_BASE_URL").and_then(Value::as_str).map(|s| s.trim_end_matches('/')) != Some(endpoint.trim_end_matches('/')) { return Ok(plan); }
    plan.status = "managed";
    if let Some(model) = model.filter(|s| !s.is_empty()) {
        env.insert("ANTHROPIC_MODEL".into(), Value::String(model.into()));
        doc["model"] = Value::String(model.into());
        plan.add(path.to_path_buf(), Some(bytes), serde_json::to_vec_pretty(&doc)?);
    }
    Ok(plan)
}

pub(in crate::tools) fn codex_model_bytes(path: &Path, model: &str) -> AppResult<Option<Vec<u8>>> {
    if model.trim().is_empty() { return Ok(None); }
    let Some(bytes) = read(path)? else { return Ok(None); };
    let mut doc = toml(&bytes)?;
    if doc.get("model_provider").and_then(toml::Value::as_str) != Some(super::MCP_CLIENT_NAME) { return Ok(None); }
    doc.as_table_mut().ok_or_else(|| error("Codex 配置顶层不是表"))?.insert("model".into(), toml::Value::String(model.trim().into()));
    let after = toml::to_string_pretty(&doc).map_err(|_| error("无法序列化 Codex 配置"))?.into_bytes();
    Ok((after != bytes).then_some(after))
}

fn codex_at(path: &Path, endpoint: &str, models: &[String], groups: &[RouteGroup], keys: &[ProviderKey], model: Option<&str>) -> AppResult<PreparedCategory> {
    let mut plan = PreparedCategory::empty(CategoryType::Codex, "not_managed");
    let Some(bytes) = plan.watch(path)? else { return Ok(plan); };
    let mut doc = toml(&bytes)?;
    if doc.get("model_provider").and_then(toml::Value::as_str) != Some(super::MCP_CLIENT_NAME)
        || doc.get("model_providers").and_then(|p| p.get(super::MCP_CLIENT_NAME)).and_then(|p| p.get("base_url")).and_then(toml::Value::as_str) != Some(codex::expected_base_url(endpoint).as_str()) {
        return Ok(plan);
    }
    let catalog = path.with_file_name(codex::codex_catalog::CATALOG_FILE);
    let pointer = doc.get("model_catalog_json").and_then(toml::Value::as_str);
    let owned_pointer = pointer.map(codex::codex_catalog::pointer_is_ours).unwrap_or(true);
    if !owned_pointer && model.is_some_and(config::is_auto_id) {
        return Err(error("Codex 正在使用自定义模型目录，不能写入自动路由入口；请保留该目录并手动添加模型"));
    }
    plan.status = "managed";
    if owned_pointer && !models.is_empty() {
        let target = pointer.map(PathBuf::from).unwrap_or(catalog);
        let old = plan.watch(&target)?;
        let mut catalog = codex::codex_catalog::build_catalog(models, keys);
        catalog::decorate_codex(&mut catalog, groups, keys);
        plan.add(target.clone(), old, serde_json::to_vec_pretty(&catalog)?);
        doc.as_table_mut().ok_or_else(|| error("Codex 配置顶层不是表"))?.insert("model_catalog_json".into(), toml::Value::String(target.display().to_string()));
    }
    if let Some(model) = model.filter(|s| !s.is_empty()) {
        doc.as_table_mut().ok_or_else(|| error("Codex 配置顶层不是表"))?.insert("model".into(), toml::Value::String(model.into()));
    }
    let after = toml::to_string_pretty(&doc).map_err(|_| error("无法序列化 Codex 配置"))?.into_bytes();
    plan.add(path.to_path_buf(), Some(bytes), after);
    Ok(plan)
}

fn desktop_at(normal: &Path, third: &Path, endpoint: &str, models: &[String], groups: &[RouteGroup], keys: &[ProviderKey]) -> AppResult<PreparedCategory> {
    let mut plan = PreparedCategory::empty(CategoryType::ClaudeDesktop, "not_managed");
    for path in [normal.join(DESKTOP_CONFIG_FILE), third.join(DESKTOP_CONFIG_FILE)] {
        let Some(bytes) = plan.watch(&path)? else { return Ok(plan); };
        if json(&bytes)?["deploymentMode"] != "3p" { return Ok(plan); }
    }
    let Some(meta) = plan.watch(&desktop_meta_path(third))? else { return Ok(plan); };
    if json(&meta)?["appliedId"] != DESKTOP_PROFILE_ID { return Ok(plan); }
    let path = desktop_profile_path(third);
    let Some(bytes) = plan.watch(&path)? else { return Ok(plan); };
    let mut profile = json(&bytes)?;
    if profile["inferenceGatewayBaseUrl"].as_str().map(|s| s.trim_end_matches('/')) != Some(endpoint.trim_end_matches('/')) { return Ok(plan); }
    if models.is_empty() { return Err(error("客户端模型列表为空，保留原配置")); }
    let entries = super::desktop_profile::build_routing_entries(models, keys, groups);
    let generated = super::build_gateway_profile(profile.clone(), endpoint, &entries);
    profile["inferenceModels"] = generated["inferenceModels"].clone();
    plan.status = "managed";
    plan.add(path, Some(bytes), serde_json::to_vec_pretty(&profile)?);
    Ok(plan)
}

pub fn prepare(store: &Store, proxy: &ProxyManager, cfg: &AppConfig, category: CategoryType) -> AppResult<PreparedCategory> {
    let Some(port) = proxy.port_of(category) else { return Ok(PreparedCategory::empty(category, "stopped")); };
    let endpoint = format!("http://127.0.0.1:{port}");
    let mut keys: Vec<_> = cfg.keys.iter().filter(|k| k.category_id == category && k.enabled).cloned().collect();
    keys.sort_by_key(|k| k.priority);
    let routing = cfg.routing.settings()?;
    let groups = catalog::groups(routing, category);
    let models: Vec<_> = catalog::advertised_for(&keys, groups).into_iter().map(|m| m.outward).collect();
    let model = cfg.settings.active_models.get(&category).map(String::as_str);
    let _ = store;
    match category {
        CategoryType::ClaudeCli => cli_at(category, &claude_cli_settings_path()?, &endpoint, model),
        CategoryType::Codex => codex_at(&codex::config_path()?, &endpoint, &models, groups, &keys, model),
        CategoryType::ClaudeDesktop => { let (normal, third) = claude_desktop_dirs()?; desktop_at(&normal, &third, &endpoint, &models, groups, &keys) }
    }
}

pub fn sync_current(store: &Store, proxy: &ProxyManager, categories: &[CategoryType]) -> Vec<SyncResult> {
    let cfg = store.snapshot_config();
    let path = PathBuf::from(store.config_path_display());
    let data_dir = path.parent().unwrap_or(Path::new("."));
    categories.iter().map(|category| match prepare(store, proxy, &cfg, *category) {
        Ok(plan) => plan.apply(data_dir),
        Err(_) => SyncResult { category_id:*category, status:"failed".into(), message:"本地选项已保存，客户端模型配置无法同步；请检查配置和文件权限后重试".into(), backup_dir:None },
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn routing_second_write_failure_preserves_backup_and_retries() {
        let dir = std::env::temp_dir().join(format!("sr-second-write-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.json"); let b = dir.join("b.json");
        fs::write(&a, b"old-a").unwrap(); fs::write(&b, b"old-b").unwrap();
        let make = || PreparedCategory { category: CategoryType::Codex, status: "managed", watches: vec![], files: vec![
            PreparedFile { path: a.clone(), before: Some(fs::read(&a).unwrap()), after: b"new-a".to_vec() },
            PreparedFile { path: b.clone(), before: Some(fs::read(&b).unwrap()), after: b"new-b".to_vec() },
        ] };
        let mut calls = 0;
        let result = make().apply_with(&dir, |path, bytes| {
            calls += 1;
            if calls == 2 { return Err(error("injected second write failure")); }
            write_without_locking_snapshot(path, bytes)
        });
        assert_eq!(calls, 2); assert_eq!(result.status, "failed");
        assert_eq!(fs::read(&a).unwrap(), b"new-a"); assert_eq!(fs::read(&b).unwrap(), b"old-b");
        let backup = PathBuf::from(result.backup_dir.unwrap());
        assert_eq!(fs::read(backup.join("0-a.json")).unwrap(), b"old-a");
        assert_eq!(fs::read(backup.join("1-b.json")).unwrap(), b"old-b");
        assert_eq!(make().apply(&dir).status, "synced");
        assert_eq!(fs::read(&b).unwrap(), b"new-b");
        assert_eq!(fs::read(backup.join("0-a.json")).unwrap(), b"old-a");
        assert!(!super::super::backup_path_for(&a).exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn routing_matrix_backup_failure_and_codex_narrow_write() {
        let dir = std::env::temp_dir().join(format!("sr-scene-matrix-{}", uuid::Uuid::new_v4())); fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        let original = "model_provider = 'synaroute'\nmodel = 'old'\n[model_providers.synaroute]\nbase_url = 'http://127.0.0.1:47101/v1'\nexperimental_bearer_token = 'dummy'\n";
        fs::write(&path, original).unwrap();
        fs::write(dir.join("auth.json"), "untouched-auth").unwrap();
        let plan = codex_at(&path, "http://127.0.0.1:47101", &["new".into()], &[], &[], Some("new")).unwrap();
        assert_eq!(plan.status, "managed");
        let blocked = dir.join("not-a-directory"); fs::write(&blocked, "block").unwrap();
        assert_eq!(plan.apply(&blocked).status, "failed");
        assert_eq!(fs::read_to_string(&path).unwrap(), original, "backup failure must prevent all writes");
        let plan = codex_at(&path, "http://127.0.0.1:47101", &["new".into()], &[], &[], Some("new")).unwrap();
        let result = plan.apply(&dir); assert_eq!(result.status, "synced");
        let doc = toml(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(doc["model"].as_str(), Some("new"));
        assert_eq!(doc["model_providers"]["synaroute"]["experimental_bearer_token"].as_str(), Some("dummy"));
        assert_eq!(fs::read_to_string(dir.join("auth.json")).unwrap(), "untouched-auth");
        assert!(!super::super::backup_path_for(&path).exists(), "model sync must not claim the integration snapshot");
        assert_eq!(codex_at(&path, "http://127.0.0.1:47101", &["new".into()], &[], &[], Some("new")).unwrap().describe().status, "unchanged");
        assert_eq!(codex_at(&path, "http://127.0.0.1:9999", &["new".into()], &[], &[], Some("new")).unwrap().describe().status, "not_managed");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn routing_matrix_partial_sync_can_retry_without_claiming_snapshot() {
        let dir = std::env::temp_dir().join(format!("sr-partial-{}", uuid::Uuid::new_v4())); fs::create_dir_all(&dir).unwrap();
        let first = dir.join("first.json"); let blocked_parent = dir.join("blocked");
        fs::write(&first, b"old").unwrap(); fs::write(&blocked_parent, b"not a directory").unwrap();
        let second = blocked_parent.join("second.json");
        let plan = PreparedCategory { category: CategoryType::Codex, status: "managed", watches: vec![], files: vec![
            PreparedFile { path: first.clone(), before: Some(b"old".to_vec()), after: b"new".to_vec() },
            PreparedFile { path: second.clone(), before: None, after: b"second".to_vec() },
        ] };
        // Reading a child of a regular file can fail before writing; either way no false success.
        let result = plan.apply(&dir); assert_eq!(result.status, "failed");
        fs::remove_file(&blocked_parent).unwrap(); fs::create_dir(&blocked_parent).unwrap();
        let before = fs::read(&first).unwrap();
        let retry = PreparedCategory { category: CategoryType::Codex, status: "managed", watches: vec![], files: vec![
            PreparedFile { path: first.clone(), before: Some(before), after: b"new".to_vec() },
            PreparedFile { path: second.clone(), before: None, after: b"second".to_vec() },
        ] };
        assert_eq!(retry.apply(&dir).status, "synced");
        assert_eq!(fs::read(&first).unwrap(), b"new"); assert_eq!(fs::read(&second).unwrap(), b"second");
        assert!(!super::super::backup_path_for(&first).exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn scene_edits_neither_credentials_nor_integration_snapshots() {
        let dir = std::env::temp_dir().join(format!("sr-scene-file-{}", uuid::Uuid::new_v4())); fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json"); let ep = "http://127.0.0.1:47100";
        fs::write(&path, json!({"env":{"ANTHROPIC_BASE_URL":ep,"ANTHROPIC_AUTH_TOKEN":"fixture-token"},"permissions":{"allow":["Read"]}}).to_string()).unwrap();
        let backup = super::super::backup_path_for(&path); fs::write(&backup, "original snapshot").unwrap();
        let plan = cli_at(CategoryType::ClaudeCli, &path, ep, Some("new-model")).unwrap();
        assert_eq!(plan.paths().len(), 1);
        let result = plan.apply(&dir); assert_eq!(result.status, "synced"); assert!(result.backup_dir.is_some());
        let doc = json(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(doc["env"]["ANTHROPIC_AUTH_TOKEN"], "fixture-token");
        assert_eq!(doc["model"], "new-model"); assert_eq!(doc["permissions"]["allow"][0], "Read");
        assert_eq!(fs::read_to_string(backup).unwrap(), "original snapshot");
        let plan = cli_at(CategoryType::ClaudeCli, &path, ep, Some("another-model")).unwrap();
        fs::write(&path, "external change").unwrap();
        assert_eq!(plan.apply(&dir).status, "failed"); assert_eq!(fs::read_to_string(&path).unwrap(), "external change");
        fs::remove_dir_all(dir).unwrap();
    }
}
