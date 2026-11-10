use super::SecretStore;
use crate::error::{AppError, AppResult};
use crate::portable::file;
use std::path::PathBuf;

impl SecretStore {
    pub(crate) fn backup_before_rewrite(&self, tag: &str) -> AppResult<Option<PathBuf>> {
        let failure = |error: AppError| {
            AppError::Other(format!(
                "整份重写密钥库前的备份失败（用途：{tag}），已放弃该操作（避免写坏后无从恢复）: {error}"
            ))
        };
        let data = match file::read(&self.path) {
            Ok(data) => data,
            Err(AppError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(None);
            }
            Err(error) => return Err(failure(error)),
        };
        let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
        let nonce = uuid::Uuid::new_v4();
        let backup = self
            .path
            .with_extension(format!("enc.{tag}-{stamp}-{nonce}"));
        file::write_new(&backup, &data).map_err(failure)?;
        tracing::info!("密钥库整份重写前已备份到 {backup:?}（用途：{tag}）");
        Ok(Some(backup))
    }
}

#[cfg(test)]
#[path = "secret_backup_tests.rs"]
mod tests;
