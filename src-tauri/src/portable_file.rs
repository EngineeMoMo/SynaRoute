use crate::error::{AppError, AppResult};
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use zeroize::Zeroizing;

pub(crate) const MAX_BACKUP_BYTES: usize = 32 * 1024 * 1024;

fn oversized_backup() -> AppError {
    AppError::Invalid(
        "配置备份不能超过 32 MiB，未执行导入或生成副本 / Backup exceeds 32 MiB".into(),
    )
}

pub(crate) fn check_size(size: usize) -> AppResult<()> {
    if size > MAX_BACKUP_BYTES {
        return Err(oversized_backup());
    }
    Ok(())
}

fn read_limited(reader: impl Read, limit: usize) -> AppResult<Zeroizing<Vec<u8>>> {
    let mut raw = Zeroizing::new(Vec::new());
    reader.take(limit as u64 + 1).read_to_end(&mut raw)?;
    if raw.len() > limit {
        return Err(oversized_backup());
    }
    Ok(raw)
}

pub(crate) fn read(path: impl AsRef<Path>) -> AppResult<Zeroizing<Vec<u8>>> {
    let input = File::open(path)?;
    let metadata = input.metadata()?;
    if !metadata.is_file() {
        return Err(AppError::Invalid("请选择普通的 JSON 配置备份文件".into()));
    }
    if metadata.len() > MAX_BACKUP_BYTES as u64 {
        return Err(oversized_backup());
    }
    read_limited(input, MAX_BACKUP_BYTES)
}

fn write_new_with(
    path: &Path,
    prepare: impl FnOnce(&mut File) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut temporary = tempfile::Builder::new()
        .prefix(".synaroute-backup-")
        .suffix(".tmp")
        .tempfile_in(parent)?;
    prepare(temporary.as_file_mut())?;
    temporary
        .persist_noclobber(path)
        .map_err(|error| error.error)?;
    Ok(())
}

pub(crate) fn write_new(path: &Path, data: &[u8]) -> AppResult<()> {
    check_size(data.len())?;
    write_new_with(path, |output| {
        output.write_all(data)?;
        output.sync_all()
    })?;
    Ok(())
}

pub(super) async fn blocking<Output: Send + 'static>(
    operation: impl FnOnce() -> AppResult<Output> + Send + 'static,
) -> AppResult<Output> {
    tauri::async_runtime::spawn_blocking(operation)
        .await
        .map_err(|_| AppError::Other("配置备份后台任务未能完成，请重试".into()))?
}

pub(crate) async fn preview_in_background(
    store: Arc<crate::store::Store>,
    path: PathBuf,
) -> AppResult<super::ImportPreview> {
    blocking(move || crate::service::preview_import(&store, &path)).await
}

pub(crate) async fn import_in_background(
    store: Arc<crate::store::Store>,
    path: String,
    mode: super::ImportMode,
    password: Option<String>,
    allow_legacy_secrets: bool,
) -> AppResult<super::ImportReport> {
    blocking(move || {
        let password = password.map(Zeroizing::new);
        crate::service::apply_import_config(&store, &path, mode, password.as_deref(), allow_legacy_secrets)
    })
    .await
}

#[cfg(test)]
#[path = "portable_file_tests.rs"]
mod tests;
