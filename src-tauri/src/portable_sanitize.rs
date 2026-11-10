use super::ExportPayload;
use crate::error::{AppError, AppResult};
use crate::model::{AppConfig, ProviderKey};
use serde::Serialize;
use serde_json::Value;
use std::path::Path;
use zeroize::Zeroize;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SanitizedBackupOutcome {
    path: String,
    removed_tokens: usize,
}

struct SanitizedBackup {
    data: Vec<u8>,
    removed_tokens: usize,
}

pub(super) fn plaintext_token_count(payload: &ExportPayload) -> usize {
    token_count(&payload.keys)
}

fn token_count(keys: &[ProviderKey]) -> usize {
    keys.iter()
        .filter_map(|key| key.balance_query.as_ref()?.access_token.as_deref())
        .filter(|token| !token.is_empty())
        .count()
}

fn invalid_backup() -> AppError {
    AppError::Invalid("不是有效的 SynaRoute 配置备份，未生成副本".into())
}

fn read_json(raw: &[u8]) -> AppResult<Value> {
    super::file::check_size(raw.len())?;
    serde_json::from_slice(raw).map_err(|_| invalid_backup())
}

fn is_portable(value: &Value) -> bool {
    [
        "formatVersion",
        "payload",
        "payloadSha256",
        "appVersion",
        "exportedAt",
        "secrets",
    ]
    .iter()
    .any(|field| value.get(field).is_some())
}

fn local_config(raw: &[u8], value: &Value) -> AppResult<AppConfig> {
    if !value.get("keys").is_some_and(Value::is_array) {
        return Err(invalid_backup());
    }
    deserialize_local_config(raw, value)
}

fn deserialize_local_config(raw: &[u8], value: &Value) -> AppResult<AppConfig> {
    if !value.is_object() || is_portable(value) {
        return Err(invalid_backup());
    }
    serde_json::from_slice(raw).map_err(|_| invalid_backup())
}

pub(crate) fn parse_local_backup(raw: &[u8]) -> AppResult<(AppConfig, Value)> {
    let value = read_json(raw)?;
    let config = deserialize_local_config(raw, &value)?;
    Ok((config, value))
}

pub(crate) fn preview_backup(
    store: &crate::store::Store,
    raw: &[u8],
) -> AppResult<super::ImportPreview> {
    let value = read_json(raw)?;
    if is_portable(&value) {
        let file = super::parse_and_verify(raw)?;
        return Ok(super::preview_import(store, &file));
    }
    let config = local_config(raw, &value)?;
    Ok(super::ImportPreview {
        format_version: config.config_version,
        app_version: String::new(),
        exported_at: String::new(),
        key_count: config.keys.len(),
        conflicting_keys: 0,
        suspicious_conflicts: Vec::new(),
        local_only_keys: 0,
        vendor_count: config.vendors.len(),
        brain_count: config.brain.len(),
        has_secrets: false,
        plaintext_balance_tokens: token_count(&config.keys),
        local_config: true,
    })
}

fn sanitize_bytes(raw: &[u8]) -> AppResult<SanitizedBackup> {
    let mut file = read_json(raw)?;
    let version = if is_portable(&file) {
        Some(super::parse_and_verify(raw)?.format_version)
    } else {
        local_config(raw, &file)?;
        None
    };
    let payload = if version.is_some() {
        file.get_mut("payload").ok_or_else(invalid_backup)?
    } else {
        &mut file
    };
    let keys = payload
        .get_mut("keys")
        .and_then(Value::as_array_mut)
        .ok_or_else(invalid_backup)?;
    let mut removed_tokens = 0;
    for key in keys {
        let Some(query) = key.get_mut("balanceQuery").and_then(Value::as_object_mut) else {
            continue;
        };
        match query.remove("accessToken") {
            Some(Value::String(mut token)) => {
                removed_tokens += usize::from(!token.is_empty());
                token.zeroize();
            }
            None | Some(Value::Null) => {}
            Some(_) => return Err(invalid_backup()),
        }
    }
    if removed_tokens == 0 {
        return Err(AppError::Invalid(
            "此备份没有需要移除的明文余额面板 Token，未生成副本".into(),
        ));
    }
    if let Some(version) = version {
        let checksum = super::digest::canonical(payload)?;
        file["payloadSha256"] = checksum.into();
        file["formatVersion"] = version.max(2).into();
    }
    let data = serde_json::to_vec_pretty(&file)?;
    super::file::check_size(data.len())?;
    if version.is_some() {
        super::parse_and_verify(&data)?;
    } else {
        local_config(&data, &file)?;
    }
    Ok(SanitizedBackup {
        data,
        removed_tokens,
    })
}

fn write_copy(path: &Path, backup: &SanitizedBackup) -> AppResult<SanitizedBackupOutcome> {
    super::file::write_new(path, &backup.data).map_err(|error| match error {
        AppError::Io(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            AppError::Invalid("脱敏副本不能覆盖原文件或已有文件，请选择新的文件名".into())
        }
        error => error,
    })?;
    Ok(SanitizedBackupOutcome {
        path: path.to_string_lossy().into_owned(),
        removed_tokens: backup.removed_tokens,
    })
}

#[tauri::command]
pub async fn sanitize_legacy_backup(
    app: tauri::AppHandle,
    path: String,
) -> AppResult<Option<SanitizedBackupOutcome>> {
    use tauri_plugin_dialog::DialogExt;
    let backup = super::file::blocking(move || {
        let raw = super::file::read(path)?;
        sanitize_bytes(&raw)
    })
    .await?;
    let (sender, receiver) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_title("另存已移除明文面板 Token 的备份副本")
        .set_file_name(format!(
            "synaroute-config-sanitized-{}.json",
            chrono::Local::now().format("%Y%m%d-%H%M%S")
        ))
        .add_filter("SynaRoute 配置", &["json"])
        .save_file(move |path| {
            let _ = sender.send(path);
        });
    let Some(path) = receiver.await.ok().flatten() else {
        return Ok(None);
    };
    let path = path
        .into_path()
        .map_err(|error| AppError::Invalid(error.to_string()))?;
    super::file::blocking(move || write_copy(&path, &backup).map(Some)).await
}

#[cfg(test)]
#[path = "portable_sanitize_tests.rs"]
mod tests;
