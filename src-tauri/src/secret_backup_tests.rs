use super::*;

#[test]
fn repeated_vault_snapshots_preserve_each_encrypted_generation() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("secrets.enc");
    let mut secrets = SecretStore::load(path.clone()).unwrap();
    let mut generations = Vec::new();
    for index in 0..8 {
        let credential = format!("backup-test-credential-{index}");
        secrets.set("provider", &credential).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let backup = secrets
            .backup_before_rewrite("import-replace")
            .unwrap()
            .unwrap();
        assert_ne!(backup, path);
        assert!(!String::from_utf8_lossy(&bytes).contains(&credential));
        generations.push((backup, bytes, credential));
    }
    let mut paths = std::collections::HashSet::new();
    for (backup, expected, credential) in generations {
        assert!(paths.insert(backup.clone()));
        assert_eq!(std::fs::read(&backup).unwrap(), expected);
        let restored = SecretStore::load(backup).unwrap();
        assert_eq!(
            restored.get("provider").unwrap().unwrap().as_str(),
            credential
        );
    }
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 9);
}

#[test]
fn only_a_missing_vault_is_treated_as_having_nothing_to_back_up() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("secrets.enc");
    let secrets = SecretStore::load(path.clone()).unwrap();
    assert!(secrets
        .backup_before_rewrite("master-on")
        .unwrap()
        .is_none());
    std::fs::create_dir(&path).unwrap();
    assert!(secrets.backup_before_rewrite("master-on").is_err());
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
}

#[test]
fn oversized_vault_refuses_destructive_rewrite_without_creating_a_backup() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("secrets.enc");
    let mut secrets = SecretStore::load(path.clone()).unwrap();
    std::fs::File::create(&path)
        .unwrap()
        .set_len(file::MAX_BACKUP_BYTES as u64 + 1)
        .unwrap();
    assert!(secrets.enable_master_password("Migration-Aa123!").is_err());
    assert!(!secrets.is_master_mode());
    assert_eq!(
        std::fs::metadata(&path).unwrap().len(),
        file::MAX_BACKUP_BYTES as u64 + 1
    );
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
}

#[test]
fn corrupt_vault_recovery_copies_never_overwrite_prior_recovery_material() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("secrets.enc");
    let secrets = SecretStore::load(path.clone()).unwrap();
    let mut copies = Vec::new();
    for index in 0..4 {
        let raw = format!("corrupt-but-recoverable-ciphertext-{index}");
        std::fs::write(&path, &raw).unwrap();
        let backup = secrets.backup_before_rewrite("corrupt").unwrap().unwrap();
        copies.push((backup, raw));
    }
    for (backup, raw) in copies {
        assert_eq!(std::fs::read_to_string(backup).unwrap(), raw);
    }
}
