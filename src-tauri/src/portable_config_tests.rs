use super::*;
use crate::model::{AppConfig, BalanceQuery, CategoryType, ProviderKey};
use crate::store::decision::{DecisionConfig, EffortApi, Mode, Provider};
use crate::store::routing::config::{
    AffinityMode, CategoryPolicy, ModelRef, PrivacyConfig, PrivacyMode, RouteGroup, RoutingConfig,
    Scene, SceneRoute, Settings,
};
use std::collections::BTreeMap;

fn fixture() -> (Store, std::path::PathBuf) {
    let directory = std::env::temp_dir().join(format!(
        "synaroute-portable-config-{}",
        uuid::Uuid::new_v4()
    ));
    let store =
        Store::new_at(directory.join("config.json"), directory.join("secrets.enc")).unwrap();
    (store, directory)
}

fn configured() -> Store {
    let (store, _) = fixture();
    store
        .upsert_key(ProviderKey {
            id: "provider".into(),
            category_id: CategoryType::Codex,
            name: "Backup fixture".into(),
            base_url: "https://provider.example".into(),
            has_secret: true,
            balance_query: Some(BalanceQuery {
                api_key_ref: Some("billing-override".into()),
                access_token: Some("private-panel-credential".into()),
                ..Default::default()
            }),
            ..Default::default()
        })
        .unwrap();
    let mut payload = build_export(&store, "test", None).unwrap().0.payload;
    payload.routing = Some(RoutingConfig::Known(Box::new(Settings {
        revision: 7,
        categories: BTreeMap::from([(
            CategoryType::Codex,
            CategoryPolicy {
                affinity: AffinityMode::Session,
                privacy: PrivacyConfig {
                    mode: PrivacyMode::Block,
                    terms_secret: Some("__routing_terms_test".into()),
                    term_count: 1,
                    ..Default::default()
                },
                groups: vec![RouteGroup {
                    id: uuid::Uuid::new_v4().to_string(),
                    name: "Automatic".into(),
                    enabled: true,
                    default_model: "target".into(),
                    judge: Some(ModelRef {
                        key_id: "provider".into(),
                        model: "judge".into(),
                    }),
                    rules: vec![],
                }],
            },
        )]),
        scenes: vec![Scene {
            id: uuid::Uuid::new_v4().to_string(),
            name: "Work".into(),
            routes: vec![SceneRoute {
                category_id: CategoryType::Codex,
                primary_key_id: Some("provider".into()),
                model: Some("target".into()),
                effort: Some("high".into()),
            }],
        }],
        ..Default::default()
    })));
    payload.decision = Some(BTreeMap::from([(
        CategoryType::Codex,
        DecisionConfig {
            mode: Mode::Auto,
            provider: Provider::Openai,
            endpoint: "https://decision.example/v1".into(),
            model: "judge".into(),
            secret_id: "__decision_test".into(),
            targets: BTreeMap::from([("target".into(), EffortApi::Openai)]),
        },
    )]));
    store
        .apply_imported_config(&payload, ImportMode::Merge)
        .unwrap();
    for (reference, value) in [
        ("provider", "private-provider-credential"),
        ("billing-override", "private-billing-credential"),
        ("__decision_test", "private-decision-credential"),
        ("__routing_terms_test", r#"["private-sensitive-term"]"#),
        (crate::proxy::lan_guard::TOKEN_ID, "private-lan-credential"),
        ("__unrelated_internal", "private-unrelated-credential"),
    ] {
        store.save_credential(reference, value).unwrap();
    }
    store
}

#[test]
fn v4_roundtrips_routing_decision_and_only_referenced_secrets() {
    let source = configured();
    let (file, skipped) = build_export(&source, "test", Some("Export-Aa123!")).unwrap();
    assert_eq!(skipped, 0);
    assert_eq!(file.format_version, 4);
    let raw = serde_json::to_vec(&file).unwrap();
    let text = String::from_utf8(raw.clone()).unwrap();
    for forbidden in [
        "private-provider-credential",
        "private-billing-credential",
        "private-panel-credential",
        "private-decision-credential",
        "private-sensitive-term",
        "private-lan-credential",
    ] {
        assert!(
            !text.contains(forbidden),
            "{forbidden} appeared outside encryption"
        );
    }
    assert!(!text.contains("\"accessToken\":"));
    let plaintext: SecretsPlain = serde_json::from_slice(
        &crypto::open_authenticated("Export-Aa123!", file.secrets.as_ref().unwrap(), &associated_data(file.format_version, &file.payload_sha256)).unwrap(),
    )
    .unwrap();
    assert_eq!(plaintext.entries.len(), 5);
    assert!(!plaintext
        .entries
        .contains_key(crate::proxy::lan_guard::TOKEN_ID));
    assert!(!plaintext.entries.contains_key("__unrelated_internal"));
    let parsed = parse_and_verify(&raw).unwrap();
    let (destination, _) = fixture();
    let report = apply_import(
        &destination,
        &parsed,
        ImportMode::Replace,
        Some("Export-Aa123!"),
    )
    .unwrap();
    assert_eq!(report.secrets_imported, 5);
    let actual = destination.snapshot_config();
    let expected = source.snapshot_config();
    assert_eq!(
        serde_json::to_value(&actual.routing).unwrap(),
        serde_json::to_value(&expected.routing).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&actual.decision).unwrap(),
        serde_json::to_value(&expected.decision).unwrap()
    );
    let credentials = destination
        .balance_credentials(&destination.get_key("provider").unwrap())
        .unwrap();
    assert_eq!(credentials.secret.as_str(), "private-billing-credential");
    assert_eq!(
        credentials.config.access_token.as_deref(),
        Some("private-panel-credential")
    );
    assert_eq!(destination.count_orphan_secrets(), 0);
}

#[test]
fn no_secret_export_has_no_plaintext_token_and_reports_missing_auxiliary_credentials() {
    let source = configured();
    let file = build_export(&source, "test", None).unwrap().0;
    assert!(file.secrets.is_none());
    let raw = serde_json::to_string(&file).unwrap();
    assert!(!raw.contains("private-"));
    assert!(!raw.contains("\"accessToken\":"));
    let (destination, _) = fixture();
    let report = apply_import(&destination, &file, ImportMode::Replace, None).unwrap();
    assert!(report
        .warnings
        .iter()
        .any(|warning| warning.contains("4 个余额")));
    assert!(destination
        .balance_credentials(&destination.get_key("provider").unwrap())
        .is_err());
}

#[test]
fn legacy_v2_digest_is_checked_before_defaults_and_replace_resets_missing_policies() {
    let (source, _) = fixture();
    let mut legacy = serde_json::to_value(build_export(&source, "test", None).unwrap().0).unwrap();
    legacy["formatVersion"] = serde_json::json!(2);
    legacy["payload"].as_object_mut().unwrap().remove("routing");
    legacy["payload"]
        .as_object_mut()
        .unwrap()
        .remove("decision");
    legacy["payloadSha256"] = serde_json::json!(digest::canonical(&legacy["payload"]).unwrap());
    let parsed = parse_and_verify(&serde_json::to_vec(&legacy).unwrap()).unwrap();
    assert!(parsed.payload.routing.is_none());
    assert!(parsed.payload.decision.is_none());
    let destination = configured();
    apply_import(&destination, &parsed, ImportMode::Merge, None).unwrap();
    assert!(!destination.snapshot_config().decision.is_empty());
    assert!(!destination
        .snapshot_config()
        .routing
        .settings()
        .unwrap()
        .categories
        .is_empty());
    apply_import(&destination, &parsed, ImportMode::Replace, None).unwrap();
    assert!(destination.snapshot_config().decision.is_empty());
    assert!(destination
        .snapshot_config()
        .routing
        .settings()
        .unwrap()
        .categories
        .is_empty());
    legacy["payload"]["settings"]["theme"] = serde_json::json!("tampered");
    assert!(parse_and_verify(&serde_json::to_vec(&legacy).unwrap()).is_err());
}

#[test]
fn merge_keeps_extra_groups_scenes_and_decision_categories() {
    let source = configured();
    let destination = configured();
    let incoming = build_export(&source, "test", None).unwrap().0;
    let mut existing = build_export(&destination, "test", None).unwrap().0.payload;
    existing
        .decision
        .as_mut()
        .unwrap()
        .insert(CategoryType::ClaudeCli, DecisionConfig::default());
    destination
        .apply_imported_config(&existing, ImportMode::Replace)
        .unwrap();
    apply_import(&destination, &incoming, ImportMode::Merge, None).unwrap();
    let merged = destination.snapshot_config();
    assert_eq!(
        merged.routing.settings().unwrap().categories[&CategoryType::Codex]
            .groups
            .len(),
        2
    );
    assert_eq!(merged.routing.settings().unwrap().scenes.len(), 2);
    assert_eq!(merged.decision.len(), 2);
}

#[test]
fn import_refuses_to_overwrite_or_reference_machine_credentials() {
    let source = configured();
    let mut file = build_export(&source, "test", None).unwrap().0;
    file.payload.keys[0]
        .balance_query
        .as_mut()
        .unwrap()
        .access_token_ref = Some(crate::proxy::lan_guard::TOKEN_ID.into());
    let (destination, _) = fixture();
    assert!(apply_import(&destination, &file, ImportMode::Replace, None).is_err());
    assert!(destination.snapshot_config().keys.is_empty());
}

#[test]
fn replace_snapshots_credentials_even_when_only_existing_ids_are_overwritten() {
    let source = configured();
    let file = build_export(&source, "test", Some("Backup-Aa123!"))
        .unwrap()
        .0;
    let (destination, directory) = fixture();
    destination
        .upsert_key(source.get_key("provider").unwrap())
        .unwrap();
    destination
        .save_credential("provider", "original-provider-secret")
        .unwrap();
    let reference = source
        .get_key("provider")
        .unwrap()
        .balance_query
        .unwrap()
        .access_token_ref
        .unwrap();
    destination
        .save_credential(&reference, "original-panel-secret")
        .unwrap();
    let before = std::fs::read(directory.join("secrets.enc")).unwrap();
    let report = apply_import(
        &destination,
        &file,
        ImportMode::Replace,
        Some("Backup-Aa123!"),
    )
    .unwrap();
    assert_eq!(report.keys_removed, 0);
    assert_eq!(report.secrets_pruned, 0);
    let path = report.secrets_backup_path.unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let vault = crate::secret::SecretStore::load(path.into()).unwrap();
    assert_eq!(
        vault.get("provider").unwrap().unwrap().as_str(),
        "original-provider-secret"
    );
    assert_eq!(
        vault.get(&reference).unwrap().unwrap().as_str(),
        "original-panel-secret"
    );
    assert_eq!(
        destination
            .secrets
            .read()
            .get(&reference)
            .unwrap()
            .unwrap()
            .as_str(),
        "private-panel-credential"
    );
}

#[test]
fn vault_backup_failure_aborts_replace_before_any_configuration_or_credential_change() {
    let source = configured();
    let file = build_export(&source, "test", Some("Backup-Aa123!"))
        .unwrap()
        .0;
    let (destination, directory) = fixture();
    destination
        .upsert_key(ProviderKey {
            id: "local-provider".into(),
            ..Default::default()
        })
        .unwrap();
    destination
        .save_credential("local-provider", "original-provider-secret")
        .unwrap();
    let before = std::fs::read(directory.join("config.json")).unwrap();
    let vault_path = directory.join("secrets.enc");
    std::fs::rename(&vault_path, directory.join("original-secrets.enc")).unwrap();
    std::fs::create_dir(&vault_path).unwrap();
    assert!(apply_import(
        &destination,
        &file,
        ImportMode::Replace,
        Some("Backup-Aa123!")
    )
    .is_err());
    assert_eq!(
        std::fs::read(directory.join("config.json")).unwrap(),
        before
    );
    assert!(destination.get_key("local-provider").is_some());
    assert!(destination.get_key("provider").is_none());
    assert_eq!(
        destination
            .secrets
            .read()
            .get("local-provider")
            .unwrap()
            .unwrap()
            .as_str(),
        "original-provider-secret"
    );
}

#[test]
fn locked_legacy_tokens_are_never_exported_and_migrate_after_unlock() {
    let (source, directory) = fixture();
    source
        .save_credential("legacy", "private-api-legacy")
        .unwrap();
    source
        .secrets
        .write()
        .enable_master_password("Migration-Aa123!")
        .unwrap();
    drop(source);
    let legacy = AppConfig {
        keys: vec![ProviderKey {
            id: "legacy".into(),
            balance_query: Some(BalanceQuery {
                access_token: Some("private-panel-legacy".into()),
                ..Default::default()
            }),
            ..Default::default()
        }],
        ..Default::default()
    };
    std::fs::write(
        directory.join("config.json"),
        serde_json::to_vec(&legacy).unwrap(),
    )
    .unwrap();
    let store =
        Store::new_at(directory.join("config.json"), directory.join("secrets.enc")).unwrap();
    assert!(store.secrets.read().is_locked());
    let file = build_export(&store, "test", None).unwrap().0;
    assert!(!serde_json::to_string(&file)
        .unwrap()
        .contains("private-panel-legacy"));
    assert!(std::fs::read_to_string(directory.join("config.json"))
        .unwrap()
        .contains("private-panel-legacy"));
    store.unlock_secrets("Migration-Aa123!").unwrap();
    assert!(!std::fs::read_to_string(directory.join("config.json"))
        .unwrap()
        .contains("private-panel-legacy"));
    let credentials = store
        .balance_credentials(&store.get_key("legacy").unwrap())
        .unwrap();
    assert_eq!(
        credentials.config.access_token.as_deref(),
        Some("private-panel-legacy")
    );
}

#[test]
fn legacy_plaintext_backup_imports_directly_into_encrypted_storage() {
    let (source, _) = fixture();
    let mut file = build_export(&source, "test", None).unwrap().0;
    file.format_version = 2;
    file.payload.keys.push(ProviderKey {
        id: "legacy-import".into(),
        balance_query: Some(BalanceQuery {
            access_token: Some("private-import-token".into()),
            ..Default::default()
        }),
        ..Default::default()
    });
    file.payload.routing = None;
    file.payload.decision = None;
    file.payload_sha256 = payload_digest(&file.payload).unwrap();
    let parsed = parse_and_verify(&serde_json::to_vec(&file).unwrap()).unwrap();
    let (destination, directory) = fixture();
    apply_import(&destination, &parsed, ImportMode::Merge, None).unwrap();
    assert!(!std::fs::read_to_string(directory.join("config.json"))
        .unwrap()
        .contains("private-import-token"));
    let reference = destination
        .get_key("legacy-import")
        .unwrap()
        .balance_query
        .unwrap()
        .access_token_ref
        .unwrap();
    assert_eq!(
        destination
            .secrets
            .read()
            .get(&reference)
            .unwrap()
            .unwrap()
            .as_str(),
        "private-import-token"
    );
}

#[test]
fn startup_migrates_legacy_tokens_and_failed_encryption_preserves_original_file() {
    for fail in [false, true] {
        let (store, directory) = fixture();
        drop(store);
        let legacy = AppConfig {
            keys: vec![ProviderKey {
                id: "startup-legacy".into(),
                balance_query: Some(BalanceQuery {
                    access_token: Some("private-startup-token".into()),
                    ..Default::default()
                }),
                ..Default::default()
            }],
            ..Default::default()
        };
        let original = serde_json::to_vec(&legacy).unwrap();
        std::fs::write(directory.join("config.json"), &original).unwrap();
        if fail {
            std::fs::create_dir(directory.join("secrets.enc")).unwrap();
        }
        let loaded = Store::new_at(directory.join("config.json"), directory.join("secrets.enc"));
        if fail {
            assert!(loaded.is_err());
            assert_eq!(
                std::fs::read(directory.join("config.json")).unwrap(),
                original
            );
        } else {
            let loaded = loaded.unwrap();
            assert!(!std::fs::read_to_string(directory.join("config.json"))
                .unwrap()
                .contains("private-startup-token"));
            assert!(loaded
                .get_key("startup-legacy")
                .unwrap()
                .balance_query
                .unwrap()
                .access_token_ref
                .is_some());
        }
    }
}
