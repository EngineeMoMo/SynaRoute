use super::{
    backup_path_for, claude_cli_settings_path, claude_desktop_dirs, codex, created_marker_path_for,
    desktop_meta_path, desktop_profile_path, DESKTOP_CONFIG_FILE, DESKTOP_PROFILE_ID,
};
use crate::{model::CategoryType, proxy::ProxyManager, store::Store};
use serde::Serialize;
use serde_json::Value;
use std::{fs, path::Path};

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionState {
    Configured,
    NotConfigured,
    NeedsAttention,
    Unknown,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionCheck {
    state: ConnectionState,
    reason: &'static str,
    client_model: Option<String>,
}

impl ConnectionCheck {
    fn new(state: ConnectionState, reason: &'static str) -> Self {
        Self {
            state,
            reason,
            client_model: None,
        }
    }
}

fn read_optional(path: &Path) -> Result<Option<String>, &'static str> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err("unreadable"),
    }
}

fn read_json(path: &Path) -> Result<Option<Value>, &'static str> {
    read_optional(path)?
        .map(|text| {
            let value: Value = serde_json::from_str(&text).map_err(|_| "invalid_json")?;
            if !value.is_object() {
                return Err("invalid_json");
            }
            Ok(value)
        })
        .transpose()
}

fn text(value: &Value, name: &str) -> Option<String> {
    value
        .get(name)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn cli_at(path: &Path, endpoint: &str) -> Result<ConnectionCheck, &'static str> {
    let Some(doc) = read_json(path)? else {
        return Ok(ConnectionCheck::new(
            ConnectionState::NotConfigured,
            "missing",
        ));
    };
    if doc.get("env").is_some_and(|env| !env.is_object()) {
        return Err("invalid_json");
    }
    let env = &doc["env"];
    let mut check = match text(env, "ANTHROPIC_BASE_URL") {
        None => ConnectionCheck::new(ConnectionState::NotConfigured, "not_pointing"),
        Some(url) if url.trim_end_matches('/') == endpoint.trim_end_matches('/') => {
            if text(env, "ANTHROPIC_AUTH_TOKEN")
                .or_else(|| text(env, "ANTHROPIC_API_KEY"))
                .is_some()
            {
                ConnectionCheck::new(ConnectionState::Configured, "matches")
            } else {
                ConnectionCheck::new(ConnectionState::NeedsAttention, "credentials_missing")
            }
        }
        Some(_) => ConnectionCheck::new(ConnectionState::NeedsAttention, "endpoint_mismatch"),
    };
    check.client_model = text(env, "ANTHROPIC_MODEL").or_else(|| text(&doc, "model"));
    Ok(check)
}

fn codex_at(cfg: &Path, auth: &Path, endpoint: &str) -> Result<ConnectionCheck, &'static str> {
    let raw = read_optional(cfg)?;
    let doc = raw
        .as_deref()
        .map(|s| s.parse::<toml::Value>().map_err(|_| "invalid_toml"))
        .transpose()?;
    // Unreadable credentials are not evidence that the placeholder is absent.
    let _ = read_json(auth)?;
    let selected = doc
        .as_ref()
        .and_then(|d| d.get("model_provider"))
        .and_then(toml::Value::as_str);
    let applied = selected == Some(super::MCP_CLIENT_NAME)
        || backup_path_for(cfg).exists()
        || created_marker_path_for(cfg).exists();
    let (state, reason) = match codex::drift_state(cfg, auth, endpoint, applied) {
        codex::DriftState::Intact => (ConnectionState::Configured, "matches"),
        codex::DriftState::NotApplied => (ConnectionState::NotConfigured, "not_pointing"),
        codex::DriftState::ProfileShadowed { .. } => {
            (ConnectionState::NeedsAttention, "profile_override")
        }
        codex::DriftState::LegacyProfileKeyBreaksLoad { .. } => {
            (ConnectionState::NeedsAttention, "legacy_profile")
        }
        codex::DriftState::CatalogFileMissing { .. } => {
            (ConnectionState::NeedsAttention, "catalog_missing")
        }
        codex::DriftState::FellBackToOfficial {
            placeholder_armed: true,
        } => (ConnectionState::NeedsAttention, "placeholder_armed"),
        codex::DriftState::SelectionDangling => {
            (ConnectionState::NeedsAttention, "provider_missing")
        }
        codex::DriftState::OurTablePointsElsewhere { .. } => {
            (ConnectionState::NeedsAttention, "provider_mismatch")
        }
        codex::DriftState::SelectedElsewhere { .. }
        | codex::DriftState::FellBackToOfficial {
            placeholder_armed: false,
        } => (ConnectionState::NeedsAttention, "not_pointing"),
    };
    Ok(ConnectionCheck {
        state,
        reason,
        client_model: doc
            .as_ref()
            .and_then(|d| d.get("model"))
            .and_then(toml::Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned),
    })
}

fn desktop_at(
    normal: &Path,
    threep: &Path,
    endpoint: &str,
) -> Result<ConnectionCheck, &'static str> {
    let normal = read_json(&normal.join(DESKTOP_CONFIG_FILE))?;
    let third = read_json(&threep.join(DESKTOP_CONFIG_FILE))?;
    let profile = read_json(&desktop_profile_path(threep))?;
    let meta = read_json(&desktop_meta_path(threep))?;
    let ours = meta
        .as_ref()
        .and_then(|v| v.get("appliedId"))
        .and_then(Value::as_str)
        == Some(DESKTOP_PROFILE_ID);
    if !ours {
        return Ok(ConnectionCheck::new(
            if profile.is_some() {
                ConnectionState::NeedsAttention
            } else {
                ConnectionState::NotConfigured
            },
            "desktop_not_selected",
        ));
    }
    let deployed = [&normal, &third].iter().all(|d| {
        d.as_ref()
            .and_then(|v| v.get("deploymentMode"))
            .and_then(Value::as_str)
            == Some("3p")
    });
    if !deployed {
        return Ok(ConnectionCheck::new(
            ConnectionState::NeedsAttention,
            "deployment_mode",
        ));
    }
    let Some(profile) = profile else {
        return Ok(ConnectionCheck::new(
            ConnectionState::NeedsAttention,
            "profile_missing",
        ));
    };
    if text(&profile, "inferenceGatewayBaseUrl")
        .as_deref()
        .map(|s| s.trim_end_matches('/'))
        != Some(endpoint.trim_end_matches('/'))
    {
        return Ok(ConnectionCheck::new(
            ConnectionState::NeedsAttention,
            "endpoint_mismatch",
        ));
    }
    if text(&profile, "inferenceProvider").as_deref() != Some("gateway")
        || text(&profile, "inferenceGatewayAuthScheme").as_deref() != Some("bearer")
        || text(&profile, "inferenceGatewayApiKey").is_none()
    {
        return Ok(ConnectionCheck::new(
            ConnectionState::NeedsAttention,
            "credentials_missing",
        ));
    }
    Ok(ConnectionCheck::new(ConnectionState::Configured, "matches"))
}

fn connection(category: CategoryType, endpoint: &str) -> ConnectionCheck {
    let result = match category {
        CategoryType::ClaudeCli => claude_cli_settings_path()
            .map_err(|_| "path_unavailable")
            .and_then(|p| cli_at(&p, endpoint)),
        CategoryType::Codex => codex::config_path()
            .and_then(|cfg| Ok((cfg, codex::auth_path()?)))
            .map_err(|_| "path_unavailable")
            .and_then(|(cfg, auth)| codex_at(&cfg, &auth, endpoint)),
        CategoryType::ClaudeDesktop => claude_desktop_dirs()
            .map_err(|_| "path_unavailable")
            .and_then(|(normal, third)| desktop_at(&normal, &third, endpoint)),
    };
    result.unwrap_or_else(|reason| ConnectionCheck::new(ConnectionState::Unknown, reason))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CategoryOverview {
    category_id: CategoryType,
    running: bool,
    endpoint: String,
    key_count: usize,
    enabled_count: usize,
    primary_key: Option<String>,
    configured_model: Option<String>,
    configured_effort: Option<String>,
    connection: ConnectionCheck,
    latest: Option<crate::diagnostics::route_profiles::LatestAttempt>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutingOverview {
    captured_at: i64,
    vault_locked: bool,
    categories: Vec<CategoryOverview>,
}

pub(crate) fn build(store: &Store, proxy: &ProxyManager) -> RoutingOverview {
    build_with(store, proxy, connection)
}

fn build_with(
    store: &Store,
    proxy: &ProxyManager,
    check: impl Fn(CategoryType, &str) -> ConnectionCheck,
) -> RoutingOverview {
    // list_keys also refreshes externally edited configuration before reading settings.
    let keys: Vec<_> = CategoryType::ALL
        .into_iter()
        .map(|cat| (cat, store.list_keys(cat)))
        .collect();
    let settings = store.get_settings();
    let latest = store.route_profiles.latest();
    let categories = keys
        .into_iter()
        .map(|(category_id, keys)| {
            let endpoint = crate::service::expected_endpoint(store, proxy, category_id);
            let enabled = store.enabled_keys_sorted(category_id);
            CategoryOverview {
                category_id,
                running: proxy.is_running(category_id),
                connection: check(category_id, &endpoint),
                endpoint,
                key_count: keys.len(),
                enabled_count: enabled.len(),
                primary_key: enabled.first().map(|k| k.name.clone()),
                configured_model: settings
                    .active_models
                    .get(&category_id)
                    .filter(|s| !s.trim().is_empty())
                    .cloned(),
                configured_effort: settings
                    .active_efforts
                    .get(&category_id)
                    .filter(|s| !s.trim().is_empty())
                    .cloned(),
                latest: latest
                    .iter()
                    .find(|a| a.category_id == category_id)
                    .cloned(),
            }
        })
        .collect();
    RoutingOverview {
        captured_at: chrono::Utc::now().timestamp_millis(),
        vault_locked: store.secrets.read().is_locked(),
        categories,
    }
}

#[tauri::command]
pub async fn routing_overview(
    state: tauri::State<'_, crate::AppState>,
) -> Result<RoutingOverview, String> {
    let store = state.store.clone();
    let proxy = state.proxy.clone();
    tauri::async_runtime::spawn_blocking(move || build(&store, &proxy))
        .await
        .map_err(|_| "无法读取路由总览 / Unable to read routing overview".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ProviderKey;
    use serde_json::json;
    use std::{path::PathBuf, sync::Arc};

    fn dir() -> PathBuf {
        let path = std::env::temp_dir().join(format!("sr-overview-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        path
    }
    fn write_json(path: &Path, value: Value) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
    }

    #[test]
    fn cli_checks_the_endpoint_and_never_returns_credentials() {
        let dir = dir();
        let path = dir.join("settings.json");
        let ep = "http://127.0.0.1:47100";
        assert_eq!(
            cli_at(&path, ep).unwrap().state,
            ConnectionState::NotConfigured
        );
        write_json(
            &path,
            json!({"env":{"ANTHROPIC_BASE_URL":ep,"ANTHROPIC_AUTH_TOKEN":"private-token","ANTHROPIC_MODEL":"client-model"}}),
        );
        let before = fs::read(&path).unwrap();
        let check = cli_at(&path, ep).unwrap();
        assert_eq!(check.state, ConnectionState::Configured);
        assert_eq!(check.client_model.as_deref(), Some("client-model"));
        assert!(!serde_json::to_string(&check)
            .unwrap()
            .contains("private-token"));
        assert_eq!(fs::read(&path).unwrap(), before);
        assert_eq!(
            cli_at(&path, "http://127.0.0.1:47101").unwrap().state,
            ConnectionState::NeedsAttention
        );
        write_json(&path, json!({"env":{"ANTHROPIC_BASE_URL":ep}}));
        assert_eq!(cli_at(&path, ep).unwrap().reason, "credentials_missing");
        fs::write(&path, "{").unwrap();
        assert_eq!(cli_at(&path, ep).unwrap_err(), "invalid_json");
        write_json(&path, json!([]));
        assert_eq!(cli_at(&path, ep).unwrap_err(), "invalid_json");
        write_json(&path, json!({"env":"invalid"}));
        assert_eq!(cli_at(&path, ep).unwrap_err(), "invalid_json");
        assert_eq!(read_optional(&dir).unwrap_err(), "unreadable");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn codex_reuses_drift_checks_even_without_backup_markers() {
        let dir = dir();
        let cfg = dir.join("config.toml");
        let auth = dir.join("auth.json");
        let ep = "http://127.0.0.1:47101";
        let config = format!("model_provider = 'synaroute'\nmodel = 'client-model'\n[model_providers.synaroute]\nbase_url = '{ep}/v1'\nexperimental_bearer_token = '{}'\nrequires_openai_auth = true\n", super::super::PROXY_PLACEHOLDER);
        fs::write(&cfg, &config).unwrap();
        let check = codex_at(&cfg, &auth, ep).unwrap();
        assert_eq!(check.state, ConnectionState::Configured);
        assert_eq!(check.client_model.as_deref(), Some("client-model"));
        assert_eq!(
            codex_at(&cfg, &auth, "http://127.0.0.1:47999")
                .unwrap()
                .state,
            ConnectionState::NeedsAttention
        );
        fs::write(
            &cfg,
            config.replace(
                "requires_openai_auth = true",
                "requires_openai_auth = false",
            ),
        )
        .unwrap();
        assert_eq!(
            codex_at(&cfg, &auth, ep).unwrap().state,
            ConnectionState::NeedsAttention
        );
        fs::write(&cfg, "model_provider='openai'").unwrap();
        assert_eq!(
            codex_at(&cfg, &auth, ep).unwrap().state,
            ConnectionState::NotConfigured
        );
        write_json(
            &auth,
            json!({"OPENAI_API_KEY":codex::CODEX_AUTH_PLACEHOLDER}),
        );
        assert_eq!(
            codex_at(&cfg, &auth, ep).unwrap().reason,
            "placeholder_armed"
        );
        fs::write(&cfg, "invalid=[").unwrap();
        assert_eq!(codex_at(&cfg, &auth, ep).unwrap_err(), "invalid_toml");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn desktop_requires_selected_profile_deployment_and_current_endpoint() {
        let dir = dir();
        let normal = dir.join("normal");
        let third = dir.join("third");
        let ep = "http://127.0.0.1:47102";
        assert_eq!(
            desktop_at(&normal, &third, ep).unwrap().state,
            ConnectionState::NotConfigured
        );
        write_json(
            &normal.join(DESKTOP_CONFIG_FILE),
            json!({"deploymentMode":"3p"}),
        );
        write_json(
            &third.join(DESKTOP_CONFIG_FILE),
            json!({"deploymentMode":"3p"}),
        );
        write_json(
            &desktop_meta_path(&third),
            json!({"appliedId":DESKTOP_PROFILE_ID}),
        );
        write_json(
            &desktop_profile_path(&third),
            json!({"inferenceGatewayBaseUrl":ep,"inferenceGatewayApiKey":"private-token","inferenceGatewayAuthScheme":"bearer","inferenceProvider":"gateway"}),
        );
        let check = desktop_at(&normal, &third, ep).unwrap();
        assert_eq!(check.state, ConnectionState::Configured);
        assert!(!serde_json::to_string(&check)
            .unwrap()
            .contains("private-token"));
        assert_eq!(
            desktop_at(&normal, &third, "http://127.0.0.1:47999")
                .unwrap()
                .reason,
            "endpoint_mismatch"
        );
        write_json(
            &normal.join(DESKTOP_CONFIG_FILE),
            json!({"deploymentMode":"1p"}),
        );
        assert_eq!(
            desktop_at(&normal, &third, ep).unwrap().reason,
            "deployment_mode"
        );
        write_json(
            &desktop_meta_path(&third),
            json!({"appliedId":"someone-else"}),
        );
        assert_eq!(
            desktop_at(&normal, &third, ep).unwrap().reason,
            "desktop_not_selected"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn overview_keeps_configuration_and_actual_attempts_separate() {
        let dir = dir();
        let store =
            Arc::new(Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap());
        let primary = ProviderKey {
            id: "overview-primary".into(),
            name: "Primary".into(),
            enabled: true,
            priority: 0,
            ..Default::default()
        };
        let backup = ProviderKey {
            id: "overview-backup".into(),
            name: "Backup".into(),
            priority: 1,
            ..primary.clone()
        };
        store.upsert_key(primary).unwrap();
        store.upsert_key(backup.clone()).unwrap();
        store
            .set_proxy_running_categories(&[CategoryType::ClaudeCli])
            .unwrap();
        store
            .route_profiles
            .record(&backup, "alias", "actual-model", true, true, Some(200), 24);
        let proxy = ProxyManager::new(store.clone());
        let snapshot = build_with(&store, &proxy, |cat, _| {
            ConnectionCheck::new(
                if cat == CategoryType::Codex {
                    ConnectionState::Unknown
                } else {
                    ConnectionState::Configured
                },
                "matches",
            )
        });
        assert_eq!(snapshot.categories.len(), 3);
        let cli = &snapshot.categories[0];
        assert!(
            !cli.running,
            "saved autostart intent is not a listening proxy"
        );
        assert_eq!(cli.primary_key.as_deref(), Some("Primary"));
        assert_eq!(cli.latest.as_ref().unwrap().key_name, "Backup");
        assert_eq!(cli.latest.as_ref().unwrap().real_model, "actual-model");
        assert_eq!(snapshot.categories[1].key_count, 0);
        assert!(snapshot.categories[1].latest.is_none());
        assert_eq!(
            snapshot.categories[2].connection.state,
            ConnectionState::Unknown
        );
        assert!(!snapshot.vault_locked);
        let serialized = serde_json::to_value(&snapshot).unwrap();
        let object = serialized.as_object().unwrap();
        let mut fields: Vec<_> = object.keys().map(String::as_str).collect();
        fields.sort();
        assert_eq!(fields, ["capturedAt", "categories", "vaultLocked"]);
        let mut row_fields: Vec<_> = serialized["categories"][0]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        row_fields.sort();
        assert_eq!(
            row_fields,
            [
                "categoryId",
                "configuredEffort",
                "configuredModel",
                "connection",
                "enabledCount",
                "endpoint",
                "keyCount",
                "latest",
                "primaryKey",
                "running"
            ]
        );
        drop(proxy);
        drop(store);
        fs::remove_dir_all(dir).ok();
    }
}
