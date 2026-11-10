use super::*;
use crate::model::{AppConfig, BalanceQuery, ProviderKey};
use crate::portable::{self, ImportMode};
use serde_json::Value;
use std::path::{Path, PathBuf};

fn fixture() -> (Store, tempfile::TempDir) {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::new_at(
        directory.path().join("config.json"),
        directory.path().join("secrets.enc"),
    )
    .unwrap();
    (store, directory)
}

fn legacy_bytes() -> Vec<u8> {
    let config = AppConfig {
        keys: vec![ProviderKey {
            id: "legacy-provider".into(),
            balance_query: Some(BalanceQuery {
                access_token: Some("legacy-rollback-token".into()),
                ..Default::default()
            }),
            ..Default::default()
        }],
        ..Default::default()
    };
    let mut value = serde_json::to_value(config).unwrap();
    value["extension"] = serde_json::json!({"preserve": [1, 2, 3]});
    value["keys"][0]["balanceQuery"]["extension"] = "preserve-query-field".into();
    serde_json::to_vec_pretty(&value).unwrap()
}

fn backups(directory: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("config.pre-import-")
        })
        .collect()
}

#[test]
fn locked_local_legacy_tokens_block_clean_replace_before_any_mutation() {
    let (store, directory) = fixture();
    store
        .save_credential("legacy-provider", "legacy-api-credential")
        .unwrap();
    store
        .secrets
        .write()
        .enable_master_password("Migration-Aa123!")
        .unwrap();
    let config_path = directory.path().join("config.json");
    let secrets_path = directory.path().join("secrets.enc");
    let raw = legacy_bytes();
    std::fs::write(&config_path, &raw).unwrap();
    drop(store);
    let store = Store::new_at(config_path.clone(), secrets_path.clone()).unwrap();
    assert!(store.secrets.read().is_locked());
    let original = serde_json::to_value(store.snapshot_config()).unwrap();
    let original_secrets = std::fs::read(&secrets_path).unwrap();
    let (source, _source_directory) = fixture();
    let incoming = portable::build_export(&source, "test", None).unwrap().0;
    let error = portable::apply_import(&store, &incoming, ImportMode::Replace, None).unwrap_err();
    assert!(error.to_string().contains("请先解锁"));
    assert_eq!(std::fs::read(&config_path).unwrap(), raw);
    assert_eq!(std::fs::read(&secrets_path).unwrap(), original_secrets);
    assert_eq!(
        serde_json::to_value(store.snapshot_config()).unwrap(),
        original
    );
    assert!(backups(directory.path()).is_empty());
    store.unlock_secrets("Migration-Aa123!").unwrap();
    let report = portable::apply_import(&store, &incoming, ImportMode::Replace, None).unwrap();
    let backup = std::fs::read(report.backup_path.unwrap()).unwrap();
    let config: AppConfig = serde_json::from_slice(&backup).unwrap();
    let reference = config.keys[0]
        .balance_query
        .as_ref()
        .unwrap()
        .access_token_ref
        .as_ref()
        .unwrap();
    assert!(!String::from_utf8(backup)
        .unwrap()
        .contains("legacy-rollback-token"));
    let vault_backup =
        crate::secret::SecretStore::load(report.secrets_backup_path.unwrap().into()).unwrap();
    let mut vault_backup = vault_backup;
    vault_backup.unlock("Migration-Aa123!").unwrap();
    assert_eq!(
        vault_backup.get(reference).unwrap().unwrap().as_str(),
        "legacy-rollback-token"
    );
}

#[test]
fn backup_encrypts_disk_only_legacy_tokens_preserving_unknown_fields_and_original() {
    let (store, directory) = fixture();
    let raw = legacy_bytes();
    let config_path = directory.path().join("config.json");
    std::fs::write(&config_path, &raw).unwrap();
    assert!(store.config.read().keys.is_empty());
    let path = store.backup_config_before_import().unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let actual: Value = serde_json::from_slice(&bytes).unwrap();
    let reference = actual["keys"][0]["balanceQuery"]["accessTokenRef"]
        .as_str()
        .unwrap();
    assert_eq!(
        store
            .secrets
            .read()
            .get(reference)
            .unwrap()
            .unwrap()
            .as_str(),
        "legacy-rollback-token"
    );
    assert!(reference.starts_with(super::super::balance::ACCESS_PREFIX));
    let mut expected: Value = serde_json::from_slice(&raw).unwrap();
    let query = expected["keys"][0]["balanceQuery"].as_object_mut().unwrap();
    query.remove("accessToken");
    query.insert("accessTokenRef".into(), reference.into());
    assert_eq!(actual, expected);
    assert_eq!(std::fs::read(&config_path).unwrap(), raw);
    assert!(!String::from_utf8(bytes)
        .unwrap()
        .contains("legacy-rollback-token"));
    assert!(
        !std::fs::read_to_string(directory.path().join("secrets.enc"))
            .unwrap()
            .contains("legacy-rollback-token")
    );
    drop(store);
    std::fs::copy(&path, &config_path).unwrap();
    let restored = Store::new_at(config_path, directory.path().join("secrets.enc")).unwrap();
    let restored_key = restored.get_key("legacy-provider").unwrap();
    assert_eq!(
        restored_key
            .balance_query
            .unwrap()
            .access_token_ref
            .as_deref(),
        Some(reference)
    );
    assert_eq!(
        restored
            .secrets
            .read()
            .get(reference)
            .unwrap()
            .unwrap()
            .as_str(),
        "legacy-rollback-token"
    );
}

#[test]
fn back_to_back_backups_do_not_overwrite_each_other_and_preserve_clean_disk_bytes() {
    let (store, directory) = fixture();
    let raw = br#"{ "keys": [], "extension": [1,2] }"#;
    serde_json::from_slice::<AppConfig>(raw).unwrap();
    std::fs::write(directory.path().join("config.json"), raw).unwrap();
    let mut paths = std::collections::HashSet::new();
    for _ in 0..8 {
        let path = store.backup_config_before_import().unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), raw);
        assert!(paths.insert(path));
    }
    assert_eq!(backups(directory.path()).len(), 8);
}

#[test]
fn valid_configurations_with_omitted_default_fields_are_backed_up_without_rewriting() {
    let (store, directory) = fixture();
    for raw in [
        br#"{}"#.as_slice(),
        br#"{"keys":[]}"#,
        br#"{"configVersion":2}"#,
    ] {
        serde_json::from_slice::<AppConfig>(raw).unwrap();
        std::fs::write(directory.path().join("config.json"), raw).unwrap();
        let backup = store.backup_config_before_import().unwrap();
        assert_eq!(std::fs::read(backup).unwrap(), raw);
    }
}

#[test]
fn missing_configuration_uses_a_valid_memory_snapshot_without_creating_the_source() {
    let (store, directory) = fixture();
    let expected = serde_json::to_value(&*store.config.read()).unwrap();
    let path = store.backup_config_before_import().unwrap();
    let actual: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(actual, expected);
    assert!(!directory.path().join("config.json").exists());
}

#[test]
fn unreadable_corrupt_and_oversized_configurations_never_fall_back_to_memory() {
    for failure in ["directory", "invalid", "oversized"] {
        let (store, directory) = fixture();
        let path = directory.path().join("config.json");
        match failure {
            "directory" => std::fs::create_dir(&path).unwrap(),
            "invalid" => std::fs::write(&path, b"{broken").unwrap(),
            _ => std::fs::File::create(&path)
                .unwrap()
                .set_len(file::MAX_BACKUP_BYTES as u64 + 1)
                .unwrap(),
        }
        assert!(store.backup_config_before_import().is_err());
        assert!(backups(directory.path()).is_empty());
        assert!(store.config.read().keys.is_empty());
    }
}

#[test]
fn degraded_configuration_refuses_backups_even_after_external_repair() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.json");
    std::fs::write(&path, b"{broken").unwrap();
    let store = Store::new_at(path.clone(), directory.path().join("secrets.enc")).unwrap();
    std::fs::write(&path, serde_json::to_vec(&AppConfig::default()).unwrap()).unwrap();
    assert!(store.backup_config_before_import().is_err());
    assert!(backups(directory.path()).is_empty());
}

#[test]
fn credential_persistence_failure_preserves_legacy_tokens_and_creates_no_backup() {
    let (store, directory) = fixture();
    std::fs::create_dir(directory.path().join("secrets.enc")).unwrap();
    let raw = legacy_bytes();
    let path = directory.path().join("config.json");
    std::fs::write(&path, &raw).unwrap();
    assert!(store.backup_config_before_import().is_err());
    assert_eq!(std::fs::read(&path).unwrap(), raw);
    assert!(backups(directory.path()).is_empty());
}

#[test]
fn degraded_vault_cannot_create_a_legacy_token_backup() {
    let directory = tempfile::tempdir().unwrap();
    let config_path = directory.path().join("config.json");
    let secrets_path = directory.path().join("secrets.enc");
    std::fs::write(&secrets_path, b"unreadable-vault").unwrap();
    let store = Store::new_at(config_path.clone(), secrets_path.clone()).unwrap();
    assert!(store.secrets.read().is_degraded());
    let raw = legacy_bytes();
    std::fs::write(&config_path, &raw).unwrap();
    assert!(store.backup_config_before_import().is_err());
    assert_eq!(std::fs::read(&config_path).unwrap(), raw);
    assert_eq!(std::fs::read(&secrets_path).unwrap(), b"unreadable-vault");
    assert!(backups(directory.path()).is_empty());
}
