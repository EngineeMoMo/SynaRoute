use super::*;
use crate::model::{BalanceQuery, ProviderKey};
use crate::portable::{self, ExportFile, ImportMode};
use crate::store::Store;
use sha2::{Digest, Sha256};

fn legacy_backup(format_version: u32) -> Vec<u8> {
    let payload = ExportPayload {
        keys: vec![ProviderKey {
            id: "legacy-provider".into(),
            name: "Legacy backup fixture".into(),
            balance_query: Some(BalanceQuery {
                access_token: Some("legacy-panel-credential".into()),
                ..Default::default()
            }),
            ..Default::default()
        }],
        brain: Vec::new(),
        vendors: Vec::new(),
        settings: Default::default(),
        routing: None,
        decision: None,
    };
    let payload_sha256 = if format_version == 1 {
        format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&payload).unwrap())
        )
    } else {
        portable::digest::canonical(&payload).unwrap()
    };
    serde_json::to_vec_pretty(&ExportFile {
        format_version,
        app_version: "0.1.81".into(),
        exported_at: "2026-11-10T00:00:00Z".into(),
        payload_sha256,
        payload,
        secrets: None,
    })
    .unwrap()
}

fn encode(mut file: Value) -> Vec<u8> {
    file["payloadSha256"] = portable::digest::canonical(&file["payload"])
        .unwrap()
        .into();
    serde_json::to_vec_pretty(&file).unwrap()
}

fn fixture() -> (Store, std::path::PathBuf) {
    let directory = std::env::temp_dir().join(format!(
        "synaroute-portable-sanitize-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let store =
        Store::new_at(directory.join("config.json"), directory.join("secrets.enc")).unwrap();
    (store, directory)
}

#[test]
fn sanitizes_every_supported_format_and_recomputes_a_valid_digest() {
    for version in [1, 2, 3] {
        let raw = legacy_backup(version);
        let original = portable::parse_and_verify(&raw).unwrap();
        if version == 1 {
            assert_ne!(
                original.payload_sha256,
                portable::digest::canonical(&original.payload).unwrap()
            );
        }
        let backup = sanitize_bytes(&raw).unwrap();
        let sanitized = portable::parse_and_verify(&backup.data).unwrap();
        assert_eq!(backup.removed_tokens, 1);
        assert_eq!(sanitized.format_version, version.max(2));
        assert_eq!(sanitized.app_version, original.app_version);
        assert_eq!(sanitized.exported_at, original.exported_at);
        assert_eq!(plaintext_token_count(&sanitized.payload), 0);
        assert!(sanitized.secrets.is_none());
        let text = String::from_utf8(backup.data).unwrap();
        assert!(!text.contains("legacy-panel-credential"));
        assert!(!text.contains("\"accessToken\""));
    }
}

#[test]
fn preserves_unknown_configuration_references_and_encrypted_secrets() {
    let mut original: Value = serde_json::from_slice(&legacy_backup(2)).unwrap();
    original["payload"]["extension"] = serde_json::json!({"nested": ["保留", 42]});
    original["payload"]["keys"][0]["balanceQuery"]["accessTokenRef"] =
        "__balance_access_existing".into();
    let envelope = crate::crypto::seal("Backup-Aa123!", b"existing encrypted credentials").unwrap();
    original["secrets"] = serde_json::to_value(envelope).unwrap();
    let backup = sanitize_bytes(&encode(original.clone())).unwrap();
    let actual: Value = serde_json::from_slice(&backup.data).unwrap();
    let mut expected = original;
    expected["payload"]["keys"][0]["balanceQuery"]
        .as_object_mut()
        .unwrap()
        .remove("accessToken");
    expected["payloadSha256"] = actual["payloadSha256"].clone();
    assert_eq!(actual, expected);
    assert_eq!(
        crate::crypto::open(
            "Backup-Aa123!",
            portable::parse_and_verify(&backup.data)
                .unwrap()
                .secrets
                .as_ref()
                .unwrap()
        )
        .unwrap(),
        b"existing encrypted credentials"
    );
}

#[test]
fn preview_reports_plaintext_even_when_there_is_no_encrypted_section() {
    let (store, _) = fixture();
    let file = portable::parse_and_verify(&legacy_backup(2)).unwrap();
    let preview = portable::preview_import(&store, &file);
    assert!(!preview.has_secrets);
    assert_eq!(preview.plaintext_balance_tokens, 1);
    let serialized = serde_json::to_string(&preview).unwrap();
    assert!(serialized.contains("\"plaintextBalanceTokens\":1"));
    assert!(!serialized.contains("legacy-panel-credential"));
    assert!(store.snapshot_config().keys.is_empty());
}

#[test]
fn ignores_empty_tokens_and_reference_only_credentials() {
    let (store, _) = fixture();
    for token in [None, Some(String::new())] {
        let mut file = portable::parse_and_verify(&legacy_backup(2)).unwrap();
        let query = file.payload.keys[0].balance_query.as_mut().unwrap();
        query.access_token = token;
        query.access_token_ref = Some("__balance_access_existing".into());
        assert_eq!(plaintext_token_count(&file.payload), 0);
        assert_eq!(
            portable::preview_import(&store, &file).plaintext_balance_tokens,
            0
        );
    }
}

#[test]
fn import_warns_that_the_original_backup_remains_sensitive() {
    let (store, _) = fixture();
    let file = portable::parse_and_verify(&legacy_backup(2)).unwrap();
    let report = portable::apply_import(&store, &file, ImportMode::Merge, None).unwrap();
    assert!(report.warnings.iter().any(|warning| {
        warning.contains("明文余额面板 Token") && warning.contains("不会清理原文件")
    }));
    assert!(!serde_json::to_string(&report)
        .unwrap()
        .contains("legacy-panel-credential"));
    assert!(!serde_json::to_string(&store.snapshot_config())
        .unwrap()
        .contains("legacy-panel-credential"));
}

#[test]
fn rejects_corrupt_future_duplicate_and_malformed_backups() {
    let original: Value = serde_json::from_slice(&legacy_backup(2)).unwrap();
    let mut corrupt = original.clone();
    corrupt["payloadSha256"] = "incorrect".into();
    assert!(sanitize_bytes(&serde_json::to_vec(&corrupt).unwrap()).is_err());
    let mut future = original.clone();
    future["formatVersion"] = 4.into();
    assert!(sanitize_bytes(&encode(future)).is_err());
    let duplicate = String::from_utf8(legacy_backup(2)).unwrap().replace(
        "\"accessToken\": \"legacy-panel-credential\"",
        "\"accessToken\": \"legacy-panel-credential\", \"accessToken\": \"duplicate\"",
    );
    assert!(sanitize_bytes(duplicate.as_bytes()).is_err());
    let mut malformed = original;
    malformed["payload"]["keys"][0]["balanceQuery"]["accessToken"] = true.into();
    assert!(sanitize_bytes(&encode(malformed)).is_err());
}

#[test]
fn rejects_duplicate_metadata_before_rewriting_the_backup() {
    let duplicate = String::from_utf8(legacy_backup(2)).unwrap().replace(
        "\"appVersion\": \"0.1.81\"",
        "\"appVersion\": \"0.1.81\", \"appVersion\": \"0.1.80\"",
    );
    assert!(portable::parse_and_verify(duplicate.as_bytes()).is_err());
    assert!(sanitize_bytes(duplicate.as_bytes()).is_err());
}

#[test]
fn refuses_to_claim_that_an_already_clean_backup_was_sanitized() {
    let backup = sanitize_bytes(&legacy_backup(2)).unwrap();
    let error = sanitize_bytes(&backup.data).err().unwrap().to_string();
    assert!(error.contains("没有需要移除"));
}

#[test]
fn counts_and_removes_tokens_from_every_key() {
    let mut file: Value = serde_json::from_slice(&legacy_backup(3)).unwrap();
    let mut second = file["payload"]["keys"][0].clone();
    second["id"] = "second-provider".into();
    second["balanceQuery"]["accessToken"] = "second-panel-credential".into();
    file["payload"]["keys"].as_array_mut().unwrap().push(second);
    let backup = sanitize_bytes(&encode(file)).unwrap();
    assert_eq!(backup.removed_tokens, 2);
    assert_eq!(
        plaintext_token_count(&portable::parse_and_verify(&backup.data).unwrap().payload),
        0
    );
}

#[test]
fn sanitizing_does_not_unlock_or_modify_the_local_secret_vault() {
    let (store, directory) = fixture();
    store
        .save_credential("local-provider", "local-api-credential")
        .unwrap();
    store
        .secrets
        .write()
        .enable_master_password("Migration-Aa123!")
        .unwrap();
    store.secrets.write().lock();
    let original_vault = std::fs::read(directory.join("secrets.enc")).unwrap();
    let original_config = serde_json::to_value(store.snapshot_config()).unwrap();
    let backup = sanitize_bytes(&legacy_backup(2)).unwrap();
    write_copy(&directory.join("sanitized.json"), &backup).unwrap();
    assert!(store.secrets.read().is_locked());
    assert_eq!(
        std::fs::read(directory.join("secrets.enc")).unwrap(),
        original_vault
    );
    assert_eq!(
        serde_json::to_value(store.snapshot_config()).unwrap(),
        original_config
    );
}

#[test]
fn creates_only_new_files_and_leaves_the_source_and_local_configuration_untouched() {
    let (store, directory) = fixture();
    let source = directory.join("legacy.json");
    let destination = directory.join("sanitized.json");
    let raw = legacy_backup(2);
    std::fs::write(&source, &raw).unwrap();
    let backup = sanitize_bytes(&std::fs::read(&source).unwrap()).unwrap();
    assert!(write_copy(&source, &backup).is_err());
    assert_eq!(std::fs::read(&source).unwrap(), raw);
    let outcome = write_copy(&destination, &backup).unwrap();
    assert_eq!(outcome.removed_tokens, 1);
    assert_eq!(outcome.path, destination.to_string_lossy());
    let written = std::fs::read(&destination).unwrap();
    assert!(portable::parse_and_verify(&written).is_ok());
    assert!(write_copy(&destination, &backup).is_err());
    assert_eq!(std::fs::read(&destination).unwrap(), written);
    assert_eq!(std::fs::read(&source).unwrap(), raw);
    assert!(store.snapshot_config().keys.is_empty());
}

fn raw_legacy_backup() -> Vec<u8> {
    let portable: Value = serde_json::from_slice(&legacy_backup(2)).unwrap();
    let mut config = portable["payload"].clone();
    config["configVersion"] = 2.into();
    config["extension"] = serde_json::json!({"preserve": "extra configuration"});
    config["keys"][0]["balanceQuery"]["accessTokenRef"] = "__balance_access_previous".into();
    serde_json::to_vec_pretty(&config).unwrap()
}

#[test]
fn raw_rollback_cleanup_preserves_format_references_and_unknown_configuration() {
    let raw = raw_legacy_backup();
    let backup = sanitize_bytes(&raw).unwrap();
    assert_eq!(backup.removed_tokens, 1);
    let actual: Value = serde_json::from_slice(&backup.data).unwrap();
    let mut expected: Value = serde_json::from_slice(&raw).unwrap();
    expected["keys"][0]["balanceQuery"]
        .as_object_mut()
        .unwrap()
        .remove("accessToken");
    assert_eq!(actual, expected);
    assert!(actual.get("formatVersion").is_none());
    assert!(actual.get("payloadSha256").is_none());
    assert!(parse_local_backup(&backup.data).is_ok());
    assert!(portable::parse_and_verify(&backup.data).is_err());
    assert!(sanitize_bytes(&backup.data).is_err());
}

#[test]
fn raw_rollback_preview_and_copy_work_without_unlocking_or_importing() {
    let (store, directory) = fixture();
    store
        .save_credential("local-provider", "local-api-credential")
        .unwrap();
    store
        .secrets
        .write()
        .enable_master_password("Migration-Aa123!")
        .unwrap();
    store.secrets.write().lock();
    let vault = std::fs::read(directory.join("secrets.enc")).unwrap();
    let raw = raw_legacy_backup();
    let source = directory.join("config.pre-import-legacy.json");
    std::fs::write(&source, &raw).unwrap();
    let preview = crate::service::preview_import(&store, &source).unwrap();
    assert!(preview.local_config);
    assert_eq!(preview.plaintext_balance_tokens, 1);
    assert_eq!(preview.key_count, 1);
    assert!(preview.app_version.is_empty());
    let serialized = serde_json::to_string(&preview).unwrap();
    assert!(!serialized.contains("legacy-panel-credential"));
    let copy = sanitize_bytes(&super::super::file::read(&source).unwrap()).unwrap();
    write_copy(&directory.join("sanitized-rollback.json"), &copy).unwrap();
    assert!(crate::service::apply_import_config(
        &store,
        source.to_str().unwrap(),
        ImportMode::Replace,
        None
    )
    .is_err());
    assert_eq!(std::fs::read(&source).unwrap(), raw);
    assert_eq!(std::fs::read(directory.join("secrets.enc")).unwrap(), vault);
    assert!(store.secrets.read().is_locked());
    assert!(store.snapshot_config().keys.is_empty());
}

#[test]
fn malformed_exports_cannot_bypass_validation_as_local_backups() {
    let raw = raw_legacy_backup();
    let (store, _) = fixture();
    for field in ["formatVersion", "payload", "payloadSha256", "secrets"] {
        let mut value: Value = serde_json::from_slice(&raw).unwrap();
        value[field] = Value::Null;
        let bytes = serde_json::to_vec(&value).unwrap();
        assert!(sanitize_bytes(&bytes).is_err());
        assert!(preview_backup(&store, &bytes).is_err());
    }
    for bytes in [
        br#"{}"#.as_slice(),
        br#"{"keys":[],"settings":null}"#,
        br#"[]"#,
    ] {
        assert!(sanitize_bytes(bytes).is_err());
        assert!(preview_backup(&store, bytes).is_err());
    }
    let duplicate = String::from_utf8(raw).unwrap().replace(
        "\"accessToken\": \"legacy-panel-credential\"",
        "\"accessToken\": \"legacy-panel-credential\", \"accessToken\": \"duplicate-token\"",
    );
    assert!(sanitize_bytes(duplicate.as_bytes()).is_err());
    assert!(preview_backup(&store, duplicate.as_bytes()).is_err());
}

#[test]
fn oversized_cleanup_and_direct_portable_parsing_fail_before_json_processing() {
    let raw = vec![b' '; super::super::file::MAX_BACKUP_BYTES + 1];
    let error = sanitize_bytes(&raw).err().unwrap().to_string();
    assert!(error.contains("32 MiB"));
    assert!(portable::parse_and_verify(&raw)
        .unwrap_err()
        .to_string()
        .contains("32 MiB"));
}
