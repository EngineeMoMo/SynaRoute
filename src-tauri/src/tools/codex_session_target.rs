use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::Path;

fn read_config(home: &Path) -> Result<Vec<u8>, String> {
    match std::fs::read(home.join("config.toml")) {
        Ok(bytes) => Ok(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(format!("无法读取当前 provider 配置：{error}")),
    }
}

pub(super) fn configured(home: &Path) -> Result<HashSet<String>, String> {
    let bytes = read_config(home)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| "provider 配置不是 UTF-8".to_string())?;
    let parsed = text.parse::<toml::Value>().map_err(|_| "config.toml 无法解析，请先修复配置".to_string())?;
    let mut available = HashSet::from(["openai".to_string()]);
    if let Some(providers) = parsed.get("model_providers").and_then(toml::Value::as_table) {
        available.extend(providers.iter().filter(|(_, value)| value.is_table()).map(|(id, _)| id.clone()));
    }
    Ok(available)
}

pub(super) struct TargetSnapshot {
    digest: Vec<u8>,
}

impl TargetSnapshot {
    pub(super) fn capture(home: &Path, target: &str) -> Result<Self, String> {
        if !super::sync::is_valid_provider_id(target) {
            return Err("provider 名称不合法".into());
        }
        let before = read_config(home)?;
        if !configured(home)?.contains(target) {
            return Err(format!("目标 {target} 已不在当前 model_providers 配置中；历史记录中的名称不能作为可用目标"));
        }
        let snapshot = Self { digest: Sha256::digest(before).to_vec() };
        snapshot.verify(home)?;
        Ok(snapshot)
    }

    pub(super) fn verify(&self, home: &Path) -> Result<(), String> {
        if Sha256::digest(read_config(home)?).as_slice() != self.digest {
            return Err("provider 配置在操作期间发生变化，已停止后续写入；请刷新目标后重试。已完成的部分可通过原回滚清单还原".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_provider_names_and_non_table_entries_are_not_resolvable() {
        let home = std::env::temp_dir().join(format!("synaroute-target-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&home).unwrap();
        assert!(TargetSnapshot::capture(&home, "openai").is_ok());
        assert!(TargetSnapshot::capture(&home, "stale").is_err());
        std::fs::write(home.join("config.toml"), "model_provider = 'stale'\n[model_providers]\nstale = 'not a table'\n[model_providers.relay]\nname = 'Relay'\n").unwrap();
        assert!(TargetSnapshot::capture(&home, "stale").is_err());
        let snapshot = TargetSnapshot::capture(&home, "relay").unwrap();
        std::fs::write(home.join("config.toml"), "[model_providers.relay]\nname = 'Changed'\n").unwrap();
        assert!(snapshot.verify(&home).is_err());
        std::fs::write(home.join("config.toml"), "broken = [").unwrap();
        assert!(TargetSnapshot::capture(&home, "openai").is_err());
        std::fs::remove_dir_all(home).unwrap();
    }
}
