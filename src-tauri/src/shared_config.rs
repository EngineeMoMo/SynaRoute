//! Explicit allow-list share format. Never serialize ProviderKey/AppSettings as a template.
//! Imported routes get fresh ids, no endpoint/secret, and remain disabled until configured.
use super::Store;
use crate::error::{AppError, AppResult};
use crate::model::{CategoryType, KeyParams, ModelInfo, ModelMapping, Protocol, ProviderKey};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::OnceLock;

const MAX_BYTES: usize = 1_048_576;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SharedModel {
    pub real_name: String,
    pub context_window: Option<u32>,
    pub max_output_tokens: Option<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SharedMapping {
    pub expected_name: String,
    pub real_name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SharedRoute {
    pub category_id: CategoryType,
    pub protocol: Protocol,
    pub models: Vec<SharedModel>,
    pub mappings: Vec<SharedMapping>,
    pub default_model: Option<String>,
    pub tiers: [Option<String>; 4],
    pub allow_named_model_fallback: bool,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub timeout_ms: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SharedTemplate {
    pub format: String,
    pub version: u32,
    pub routes: Vec<SharedRoute>,
}

impl SharedTemplate {
    fn build(store: &Store) -> Self {
        let mut keys = store.snapshot_config().keys;
        keys.sort_by_key(|k| (k.category_id, k.priority));
        Self {
            format: "synaroute-share".into(),
            version: 1,
            routes: keys
                .iter()
                .map(|k| SharedRoute {
                    category_id: k.category_id,
                    protocol: k.protocol,
                    models: k
                        .models
                        .iter()
                        .map(|m| SharedModel {
                            real_name: m.real_name.clone(),
                            context_window: m.context_window,
                            max_output_tokens: m.max_output_tokens,
                        })
                        .collect(),
                    mappings: k
                        .mappings
                        .iter()
                        .map(|m| SharedMapping {
                            expected_name: m.expected_name.clone(),
                            real_name: m.real_name.clone(),
                        })
                        .collect(),
                    default_model: k.default_model.clone(),
                    tiers: [
                        k.tier_haiku.clone(),
                        k.tier_sonnet.clone(),
                        k.tier_opus.clone(),
                        k.tier_fable.clone(),
                    ],
                    allow_named_model_fallback: k.allow_named_model_fallback,
                    temperature: k.params.temperature,
                    top_p: k.params.top_p,
                    timeout_ms: k.params.timeout_ms,
                })
                .collect(),
        }
    }

    fn validate(&self) -> AppResult<()> {
        if self.format != "synaroute-share" || self.version != 1 {
            return Err(AppError::Invalid(
                "不支持的分享方案格式 / Unsupported template format".into(),
            ));
        }
        if self.routes.is_empty() || self.routes.len() > 200 {
            return Err(AppError::Invalid(
                "分享方案需要 1–200 条线路 / Expected 1–200 routes".into(),
            ));
        }
        for r in &self.routes {
            let names = r
                .models
                .iter()
                .map(|m| &m.real_name)
                .chain(
                    r.mappings
                        .iter()
                        .flat_map(|m| [&m.expected_name, &m.real_name]),
                )
                .chain(r.default_model.iter())
                .chain(r.tiers.iter().flatten());
            if r.models.len() > 200
                || r.mappings.len() > 200
                || names.into_iter().any(|n| {
                    n.trim().is_empty() || n.len() > 256 || n.chars().any(char::is_control)
                })
            {
                return Err(AppError::Invalid(
                    "模型配置过长、为空或含控制字符 / Invalid model configuration".into(),
                ));
            }
            if r.temperature
                .is_some_and(|n| !n.is_finite() || !(0.0..=2.0).contains(&n))
                || r.top_p
                    .is_some_and(|n| !n.is_finite() || !(0.0..=1.0).contains(&n))
                || r.timeout_ms.is_some_and(|n| n == 0 || n > 3_600_000)
            {
                return Err(AppError::Invalid(
                    "请求参数超出范围 / Request parameters out of range".into(),
                ));
            }
        }
        Ok(())
    }

    fn parse(raw: &str) -> AppResult<Self> {
        if raw.len() > MAX_BYTES {
            return Err(AppError::Invalid(
                "分享文件不能超过 1 MiB / Template exceeds 1 MiB".into(),
            ));
        }
        // Do not echo serde errors: untrusted field names could contain credentials.
        let value: Self = serde_json::from_str(raw).map_err(|_| {
            AppError::Invalid(
                "分享文件格式无效或包含未允许字段 / Invalid template or unsupported fields".into(),
            )
        })?;
        value.validate()?;
        Ok(value)
    }

    fn to_keys(&self) -> AppResult<Vec<ProviderKey>> {
        self.routes.iter().enumerate().map(|(i, r)| {
            // Explicit business fields; deserialize defaults only for non-shared metadata.
            let key: ProviderKey = serde_json::from_value(serde_json::json!({
                "id": uuid::Uuid::new_v4().to_string(), "categoryId": r.category_id,
                "name": format!("Shared route {}", i + 1), "vendor": "custom",
                "baseUrl": "", "protocol": r.protocol, "enabled": false,
                "priority": i as i32,
                "models": r.models.iter().map(|m| ModelInfo { real_name: m.real_name.clone(), source: "manual".into(), fetched_at: None, context_window: m.context_window, max_output_tokens: m.max_output_tokens }).collect::<Vec<_>>(),
                "mappings": r.mappings.iter().map(|m| ModelMapping { id: uuid::Uuid::new_v4().to_string(), expected_name: m.expected_name.clone(), real_name: m.real_name.clone(), display_name: None }).collect::<Vec<_>>(),
                "defaultModel": r.default_model, "tierHaiku": r.tiers[0], "tierSonnet": r.tiers[1], "tierOpus": r.tiers[2], "tierFable": r.tiers[3],
                "allowNamedModelFallback": r.allow_named_model_fallback,
                "params": KeyParams { temperature: r.temperature, top_p: r.top_p, timeout_ms: r.timeout_ms }
            }))?;
            Ok(key)
        }).collect()
    }
}

struct Undo {
    path: std::path::PathBuf,
    keys: Vec<ProviderKey>,
}
static UNDO: OnceLock<Mutex<HashMap<String, Undo>>> = OnceLock::new();
fn undo_store() -> &'static Mutex<HashMap<String, Undo>> {
    UNDO.get_or_init(Default::default)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SharedImportResult {
    pub added: usize,
    pub undo_token: String,
}

fn apply(store: &Store, template: &SharedTemplate) -> AppResult<SharedImportResult> {
    template.validate()?;
    let mut keys = template.to_keys()?;
    store.backup_config_before_import()?;
    store.mutate_and_persist(|cfg| {
        for category in CategoryType::ALL {
            let max = cfg
                .keys
                .iter()
                .filter(|k| k.category_id == category)
                .map(|k| k.priority)
                .max()
                .unwrap_or(-1);
            if max > i32::MAX - 201 {
                return Err(AppError::Invalid(
                    "线路优先级超出范围 / Priority overflow".into(),
                ));
            }
            for (i, key) in keys
                .iter_mut()
                .filter(|k| k.category_id == category)
                .enumerate()
            {
                key.priority = max + 1 + i as i32;
            }
        }
        for k in &keys {
            cfg.keys.push(k.clone());
        }
        Ok(())
    })?;
    let token = uuid::Uuid::new_v4().to_string();
    let added = keys.len();
    let mut undo = undo_store().lock();
    // The UI offers undo for the latest import of this store only.
    undo.retain(|_, u| u.path != store.config_path);
    undo.insert(
        token.clone(),
        Undo {
            path: store.config_path.clone(),
            keys,
        },
    );
    Ok(SharedImportResult {
        added,
        undo_token: token,
    })
}

fn undo(store: &Store, token: &str) -> AppResult<()> {
    let mut registry = undo_store().lock();
    let record = registry
        .get(token)
        .filter(|r| r.path == store.config_path)
        .ok_or_else(|| AppError::Invalid("撤销凭据已失效 / Undo is no longer available".into()))?;
    store.mutate_and_persist(|cfg| {
        for old in &record.keys {
            let current = cfg.keys.iter().find(|k| k.id == old.id);
            if current.map(serde_json::to_value).transpose()? != Some(serde_json::to_value(old)?) {
                return Err(AppError::Invalid("导入线路已被修改，未撤销；请在分类页手动管理 / Imported routes changed; nothing removed".into()));
            }
        }
        cfg.keys.retain(|k| !record.keys.iter().any(|old| old.id == k.id));
        Ok(())
    })?;
    registry.remove(token);
    Ok(())
}

#[tauri::command]
pub async fn build_shared_template(
    state: tauri::State<'_, crate::AppState>,
) -> AppResult<SharedTemplate> {
    let value = SharedTemplate::build(&state.store);
    value.validate()?;
    Ok(value)
}

#[tauri::command]
pub async fn preview_shared_template(raw: String) -> AppResult<SharedTemplate> {
    SharedTemplate::parse(&raw)
}

#[tauri::command]
pub async fn save_shared_template(app: tauri::AppHandle, raw: String) -> AppResult<Option<String>> {
    use tauri_plugin_dialog::DialogExt;
    let template = SharedTemplate::parse(&raw)?;
    let data = serde_json::to_vec_pretty(&template)?;
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_title("分享方案 / Share template")
        .set_file_name("synaroute-share.json")
        .add_filter("SynaRoute", &["json"])
        .save_file(move |p| {
            let _ = tx.send(p);
        });
    let Some(path) = rx.await.ok().flatten() else {
        return Ok(None);
    };
    let path = path
        .into_path()
        .map_err(|_| AppError::Invalid("无法解析保存路径 / Invalid save path".into()))?;
    crate::secret::atomic_write(&path, &data)?;
    Ok(Some(path.display().to_string()))
}

#[tauri::command]
pub async fn apply_shared_template(
    state: tauri::State<'_, crate::AppState>,
    raw: String,
) -> AppResult<SharedImportResult> {
    apply(&state.store, &SharedTemplate::parse(&raw)?)
}

#[tauri::command]
pub async fn undo_shared_template(
    state: tauri::State<'_, crate::AppState>,
    token: String,
) -> AppResult<()> {
    undo(&state.store, &token)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::tests::{key, temp_store};
    #[test]
    fn allowlist_excludes_private_metadata_and_roundtrip_is_disabled_and_undoable() {
        let (store, dir) = temp_store("share-allowlist");
        let mut k = key(CategoryType::Codex);
        k.name = "private-account".into();
        k.base_url = "https://private.example/token-secret".into();
        k.headers_json = Some("private-header".into());
        k.icon = Some("private-icon".into());
        store.upsert_key(k).unwrap();
        let t = SharedTemplate::build(&store);
        let raw = serde_json::to_string(&t).unwrap();
        for banned in [
            "private",
            "baseUrl",
            "headersJson",
            "hasSecret",
            "health",
            "settings",
            "vendor",
        ] {
            assert!(!raw.contains(banned), "{banned}");
        }
        let parsed = SharedTemplate::parse(&raw).unwrap();
        let before = serde_json::to_value(store.snapshot_config()).unwrap();
        let result = apply(&store, &parsed).unwrap();
        let cfg = store.snapshot_config();
        assert_eq!(cfg.keys.len(), 2);
        assert!(!cfg.keys[1].enabled && !cfg.keys[1].has_secret && cfg.keys[1].base_url.is_empty());
        assert_ne!(cfg.keys[0].id, cfg.keys[1].id);
        undo(&store, &result.undo_token).unwrap();
        assert_eq!(
            before,
            serde_json::to_value(store.snapshot_config()).unwrap()
        );
        let result = apply(&store, &parsed).unwrap();
        let mut changed = store.snapshot_config().keys[1].clone();
        changed.name = "edited".into();
        store.upsert_key(changed).unwrap();
        assert!(undo(&store, &result.undo_token).is_err());
        assert_eq!(store.snapshot_config().keys.len(), 2);
        drop(store);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn rejects_unknown_fields_versions_and_oversized_input() {
        assert!(SharedTemplate::parse(&" ".repeat(MAX_BYTES + 1)).is_err());
        assert!(
            SharedTemplate::parse(r#"{"format":"synaroute-share","version":2,"routes":[]}"#)
                .is_err()
        );
        assert!(SharedTemplate::parse(
            r#"{"format":"synaroute-share","version":1,"routes":[],"secrets":"no"}"#
        )
        .is_err());
    }
}
