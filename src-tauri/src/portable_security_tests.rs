use super::*;
use crate::model::{BalanceQuery, CategoryType, ProviderKey};
use crate::store::decision::{DecisionConfig, Provider};

fn fixture() -> (tempfile::TempDir, Store) {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::new_at(
        directory.path().join("config.json"),
        directory.path().join("secrets.enc"),
    )
    .unwrap();
    (directory, store)
}

fn configured() -> (tempfile::TempDir, Store) {
    let (directory, store) = fixture();
    store
        .upsert_key(ProviderKey {
            id: "provider".into(),
            name: "Trusted provider".into(),
            base_url: "https://trusted.example/v1".into(),
            has_secret: true,
            ..Default::default()
        })
        .unwrap();
    store
        .save_credential("provider", "synthetic-credential")
        .unwrap();
    (directory, store)
}

fn verify(file: &ExportFile) -> ExportFile {
    parse_and_verify(&serde_json::to_vec(file).unwrap()).unwrap()
}

#[test]
fn encrypted_backup_authenticates_endpoints_and_resists_downgrade() {
    let (_source_directory, source) = configured();
    let (mut file, _) = build_export(&source, "test", Some("test-backup-password")).unwrap();
    let ciphertext = serde_json::to_vec(&file.secrets).unwrap();
    file.payload.keys[0].base_url = "https://untrusted.example".into();
    file.payload_sha256 = payload_digest(&file.payload).unwrap();
    for version in [4, 3, 2] {
        let (directory, target) = fixture();
        file.format_version = version;
        assert_eq!(serde_json::to_vec(&file.secrets).unwrap(), ciphertext);
        let before = serde_json::to_value(target.snapshot_config()).unwrap();
        let result = apply_import_with_consent(
            &target,
            &verify(&file),
            ImportMode::Replace,
            Some("test-backup-password"),
            true,
        );
        assert!(result.is_err(), "accepted tampered version {version}");
        assert_eq!(
            serde_json::to_value(target.snapshot_config()).unwrap(),
            before
        );
        assert!(target.secret_for("provider").unwrap().is_none());
        assert!(!std::fs::read_dir(directory.path())
            .unwrap()
            .flatten()
            .any(|entry| entry.file_name().to_string_lossy().contains("pre-import")));
    }
}

#[test]
fn legacy_encrypted_backups_require_explicit_consent() {
    let (_directory, source) = configured();
    let (mut file, _) = build_export(&source, "test", None).unwrap();
    file.format_version = 3;
    file.secrets = Some(
        crypto::seal(
            "legacy-password",
            &serde_json::to_vec(&SecretsPlain {
                entries: std::collections::HashMap::from([(
                    "provider".into(),
                    "synthetic-credential".into(),
                )]),
            })
            .unwrap(),
        )
        .unwrap(),
    );
    let (_target_directory, target) = fixture();
    assert!(preview_import(&target, &file).unauthenticated_secrets);
    assert!(apply_import(
        &target,
        &verify(&file),
        ImportMode::Merge,
        Some("legacy-password")
    )
    .is_err());
    assert!(target.get_key("provider").is_none());
    let report = apply_import_with_consent(
        &target,
        &verify(&file),
        ImportMode::Merge,
        Some("legacy-password"),
        true,
    )
    .unwrap();
    assert_eq!(report.secrets_imported, 1);
    assert!(report
        .warnings
        .iter()
        .any(|warning| warning.contains("未认证")));
}

#[test]
fn changed_origin_cannot_reuse_local_credentials_even_when_locked() {
    for mode in [ImportMode::Merge, ImportMode::Replace] {
        let (directory, store) = configured();
        let (mut file, _) = build_export(&store, "test", None).unwrap();
        file.payload.keys[0].base_url = "https://untrusted.example".into();
        file.payload.keys[0].has_secret = false;
        file.payload_sha256 = payload_digest(&file.payload).unwrap();
        assert_eq!(preview_import(&store, &file).suspicious_conflicts.len(), 1);
        for locked in [false, true] {
            if locked {
                store
                    .secrets
                    .write()
                    .enable_master_password("Vault-T9!mQ4#xR8")
                    .unwrap();
                store.secrets.write().lock();
            }
            let before_config = std::fs::read(directory.path().join("config.json")).unwrap();
            let before_vault = std::fs::read(directory.path().join("secrets.enc")).unwrap();
            let error = apply_import(&store, &verify(&file), mode, None).unwrap_err();
            assert!(error.to_string().contains("本机凭证"), "{error}");
            assert_eq!(
                std::fs::read(directory.path().join("config.json")).unwrap(),
                before_config
            );
            assert_eq!(
                std::fs::read(directory.path().join("secrets.enc")).unwrap(),
                before_vault
            );
        }
    }
}

#[test]
fn same_destination_restores_configuration_without_losing_local_secret() {
    let (_directory, store) = configured();
    let (mut file, _) = build_export(&store, "test", None).unwrap();
    file.payload.keys[0].name = "Renamed provider".into();
    file.payload.keys[0].has_secret = false;
    file.payload_sha256 = payload_digest(&file.payload).unwrap();
    apply_import(&store, &verify(&file), ImportMode::Merge, None).unwrap();
    assert!(store.get_key("provider").unwrap().has_secret);
    assert_eq!(
        store.secret_for("provider").unwrap().unwrap().as_str(),
        "synthetic-credential"
    );
}

#[test]
fn imported_balance_and_decision_references_cannot_borrow_unrelated_credentials() {
    let (_directory, store) = configured();
    store
        .save_credential("billing", "synthetic-billing-credential")
        .unwrap();
    store
        .save_credential("__balance_access_test", "synthetic-panel-credential")
        .unwrap();
    store
        .save_credential("__decision_test", "synthetic-decision-credential")
        .unwrap();
    let original = build_export(&store, "test", None).unwrap().0;
    for change in 0..4 {
        let mut file = original.clone();
        match change {
            0 => {
                file.payload.keys[0].balance_query = Some(BalanceQuery {
                    url: "https://untrusted.example/balance".into(),
                    ..Default::default()
                })
            }
            1 => {
                file.payload.keys[0].balance_query = Some(BalanceQuery {
                    api_key_ref: Some("billing".into()),
                    ..Default::default()
                })
            }
            2 => {
                file.payload.keys[0].balance_query = Some(BalanceQuery {
                    access_token_ref: Some("__balance_access_test".into()),
                    ..Default::default()
                })
            }
            _ => {
                file.payload.decision = Some(std::collections::BTreeMap::from([(
                    CategoryType::Codex,
                    DecisionConfig {
                        endpoint: "https://untrusted.example".into(),
                        provider: Provider::Openai,
                        secret_id: "__decision_test".into(),
                        ..Default::default()
                    },
                )]))
            }
        }
        file.payload_sha256 = payload_digest(&file.payload).unwrap();
        assert!(apply_import(&store, &verify(&file), ImportMode::Merge, None).is_err());
        assert!(store.get_key("provider").unwrap().balance_query.is_none());
    }
}

#[test]
fn orphaned_key_secrets_cannot_be_adopted_by_imports() {
    let (_directory, store) = fixture();
    store
        .save_credential("provider", "synthetic-orphan-credential")
        .unwrap();
    let (_source_directory, source) = configured();
    let file = build_export(&source, "test", None).unwrap().0;
    assert!(apply_import(&store, &file, ImportMode::Merge, None).is_err());
    assert!(store.get_key("provider").is_none());
}
