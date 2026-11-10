use super::Store;
use crate::error::{AppError, AppResult};
use crate::portable::{file, sanitize};
use zeroize::{Zeroize, Zeroizing};

impl Store {
    pub fn backup_config_before_import(&self) -> AppResult<String> {
        let _editing = self.routing_runtime.edit.lock();
        if self
            .config_degraded
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return Err(AppError::Other(
                "本机配置读取异常，已停止导入；请先修复 config.json 后重新启动".into(),
            ));
        }
        let _persisting = self
            .persist_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let raw = match file::read(&self.config_path) {
            Ok(raw) => raw,
            Err(AppError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                Zeroizing::new(serde_json::to_vec_pretty(&*self.config.read())?)
            }
            Err(error) => return Err(error),
        };
        let (mut config, mut value) = sanitize::parse_local_backup(&raw)?;
        let has_legacy_tokens = config.keys.iter().any(|key| {
            key.balance_query
                .as_ref()
                .is_some_and(|query| query.access_token.is_some())
        });
        let data = if has_legacy_tokens {
            let mut secrets = self.secrets.write();
            if secrets.is_locked() || secrets.is_degraded() {
                return Err(AppError::Invalid(
                    "本机旧配置仍含余额面板 Token，密钥库尚未解锁或读取异常，无法安全生成回滚备份。请先解锁或修复密钥库后重试；未执行导入。".into(),
                ));
            }
            for (index, key) in config.keys.iter_mut().enumerate() {
                if !super::balance::protect_key(key, &mut secrets)? {
                    continue;
                }
                let query = value["keys"][index]["balanceQuery"]
                    .as_object_mut()
                    .ok_or_else(|| AppError::Invalid("本机余额配置格式无效，未执行导入".into()))?;
                if let Some(serde_json::Value::String(mut token)) = query.remove("accessToken") {
                    token.zeroize();
                }
                match key
                    .balance_query
                    .as_ref()
                    .and_then(|query| query.access_token_ref.as_ref())
                {
                    Some(reference) => {
                        query.insert("accessTokenRef".into(), reference.clone().into());
                    }
                    None => {
                        query.remove("accessTokenRef");
                    }
                }
            }
            Zeroizing::new(serde_json::to_vec_pretty(&value)?)
        } else {
            raw
        };
        let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
        let nonce = uuid::Uuid::new_v4();
        let backup = self
            .config_path
            .with_file_name(format!("config.pre-import-{stamp}-{nonce}.json"));
        file::write_new(&backup, &data)?;
        Ok(backup.display().to_string())
    }
}

#[cfg(test)]
#[path = "store_backup_tests.rs"]
mod tests;
