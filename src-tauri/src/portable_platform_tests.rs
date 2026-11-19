use super::{apply_import, build_export, file, parse_and_verify, ImportMode, Store};
use crate::model::{CategoryType, ProviderKey};
use std::{env, path::PathBuf};

#[test]
#[ignore = "requires an explicit exchange directory and runs on each of two operating systems"]
fn exchange_portable_credentials_across_platforms() {
    const PASSWORD: &str = "Portable-Fixture-739!";
    const ID: &str = "cross-platform-acceptance";
    const TOKEN: &str = "synthetic-transfer-739";
    let exchange = PathBuf::from(env::var_os("SYNAROUTE_PROBE_EXCHANGE").expect("exchange directory required"));
    assert!(exchange.is_absolute() && exchange.is_dir());
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("config.json");
    let secrets = directory.path().join("secrets.enc");
    let store = Store::credential_fixture(config.clone(), secrets.clone()).unwrap();
    let platform = env::consts::OS;
    match env::var("SYNAROUTE_PROBE_TRANSFER").unwrap().as_str() {
        "export" => {
            store.upsert_key(ProviderKey {
                id: ID.into(), category_id: CategoryType::Codex, name: "Cross-platform acceptance".into(),
                base_url: "https://fixture.invalid".into(), has_secret: true, ..Default::default()
            }).unwrap();
            store.save_credential(ID, TOKEN).unwrap();
            let (backup, missing) = build_export(&store, env!("CARGO_PKG_VERSION"), Some(PASSWORD)).unwrap();
            assert_eq!(missing, 0);
            let bytes = serde_json::to_vec(&backup).unwrap();
            assert!(!String::from_utf8_lossy(&bytes).contains(TOKEN));
            file::write_new(&exchange.join(format!("{platform}.json")), &bytes).unwrap();
        }
        "import" => {
            let peer = match platform { "windows" => "linux", "linux" => "windows", _ => panic!("unsupported peer") };
            let backup = parse_and_verify(&file::read(exchange.join(format!("{peer}.json"))).unwrap()).unwrap();
            assert!(apply_import(&store, &backup, ImportMode::Merge, Some("Incorrect-Fixture-927!")).is_err());
            assert!(store.get_key(ID).is_none());
            let report = apply_import(&store, &backup, ImportMode::Merge, Some(PASSWORD)).unwrap();
            assert_eq!(report.secrets_imported, 1);
            drop(store);
            let reopened = Store::credential_fixture(config, secrets).unwrap();
            assert!(reopened.get_key(ID).unwrap().has_secret);
            assert_eq!(reopened.secrets.read().get(ID).unwrap().unwrap().as_str(), TOKEN);
        }
        _ => panic!("expected export or import"),
    }
    println!("PORTABLE_ACCEPTANCE platform={platform} mode={}", env::var("SYNAROUTE_PROBE_TRANSFER").unwrap());
}
