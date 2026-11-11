use super::*;
use crate::portable::ImportMode;
use crate::store::Store;
use std::io::ErrorKind;

#[test]
fn bounded_reader_accepts_empty_under_limit_and_exact_limit() {
    for size in [0, 7, 8] {
        let bytes = vec![42; size];
        assert_eq!(read_limited(bytes.as_slice(), 8).unwrap().as_slice(), bytes);
    }
}

#[test]
fn bounded_reader_stops_after_one_extra_byte_even_when_a_stream_keeps_growing() {
    struct GrowingReader<'count> {
        count: &'count mut usize,
    }
    impl Read for GrowingReader<'_> {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            buffer.fill(42);
            *self.count += buffer.len();
            Ok(buffer.len())
        }
    }
    let mut count = 0;
    assert!(read_limited(GrowingReader { count: &mut count }, 8).is_err());
    assert_eq!(count, 9);
}

#[test]
fn file_reader_rejects_oversized_files_and_directories() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("oversized.json");
    File::create(&path)
        .unwrap()
        .set_len(MAX_BACKUP_BYTES as u64 + 1)
        .unwrap();
    assert!(read(&path).unwrap_err().to_string().contains("32 MiB"));
    assert!(read(directory.path()).is_err());
}

#[test]
fn file_reader_accepts_the_exact_limit_and_detects_growth_after_metadata() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("growing.json");
    let output = File::create(&path).unwrap();
    output.set_len(MAX_BACKUP_BYTES as u64).unwrap();
    assert_eq!(read(&path).unwrap().len(), MAX_BACKUP_BYTES);
    let input = File::open(&path).unwrap();
    assert_eq!(input.metadata().unwrap().len(), MAX_BACKUP_BYTES as u64);
    output.set_len(MAX_BACKUP_BYTES as u64 + 1).unwrap();
    assert!(read_limited(input, MAX_BACKUP_BYTES).is_err());
}

#[test]
fn oversized_preview_and_both_import_modes_leave_configuration_and_files_unchanged() {
    let directory = tempfile::tempdir().unwrap();
    let config_path = directory.path().join("config.json");
    let secrets_path = directory.path().join("secrets.enc");
    let store = Store::new_at(config_path.clone(), secrets_path.clone()).unwrap();
    let original = serde_json::to_value(store.snapshot_config()).unwrap();
    let path = directory.path().join("oversized.json");
    File::create(&path)
        .unwrap()
        .set_len(MAX_BACKUP_BYTES as u64 + 1)
        .unwrap();
    assert!(crate::service::preview_import(&store, &path).is_err());
    for mode in [ImportMode::Merge, ImportMode::Replace] {
        assert!(
            crate::service::apply_import_config(&store, path.to_str().unwrap(), mode, None, false)
                .is_err()
        );
        assert_eq!(
            serde_json::to_value(store.snapshot_config()).unwrap(),
            original
        );
    }
    assert!(!config_path.exists());
    assert!(!secrets_path.exists());
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
}

#[test]
fn write_and_sync_failures_leave_neither_target_nor_temporary_files() {
    for stage in ["write", "sync"] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("copy.json");
        let result = write_new_with(&path, |output| {
            output.write_all(if stage == "write" { b"{" } else { b"{}" })?;
            Err(std::io::Error::other(format!("injected {stage} failure")))
        });
        assert!(result.unwrap_err().to_string().contains(stage));
        assert!(!path.exists());
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    }
}

#[test]
fn existing_destination_is_unchanged_and_temporary_file_is_removed() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("copy.json");
    std::fs::write(&path, b"original").unwrap();
    let error = write_new(&path, b"replacement").unwrap_err();
    assert!(matches!(error, AppError::Io(error) if error.kind() == ErrorKind::AlreadyExists));
    assert_eq!(std::fs::read(&path).unwrap(), b"original");
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
}

#[test]
fn destination_created_during_write_cannot_be_overwritten() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("copy.json");
    let error = write_new_with(&path, |output| {
        output.write_all(b"new copy")?;
        std::fs::write(&path, b"concurrent writer")?;
        output.sync_all()
    })
    .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::AlreadyExists);
    assert_eq!(std::fs::read(&path).unwrap(), b"concurrent writer");
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
}

#[test]
fn successful_publication_has_complete_contents_and_no_temporary_file() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("copy.json");
    write_new(&path, br#"{"complete":true}"#).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), br#"{"complete":true}"#);
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
fn oversized_output_is_rejected_before_any_file_is_created() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("copy.json");
    assert!(write_new(&path, &vec![0; MAX_BACKUP_BYTES + 1]).is_err());
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
}

#[test]
fn oversized_export_never_overwrites_an_existing_backup() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::new_at(
        directory.path().join("config.json"),
        directory.path().join("secrets.enc"),
    )
    .unwrap();
    let path = directory.path().join("export.json");
    std::fs::write(&path, b"existing backup").unwrap();
    assert!(
        crate::service::write_export(&store, &path, &vec![0; MAX_BACKUP_BYTES + 1], 0, false)
            .is_err()
    );
    assert_eq!(std::fs::read(path).unwrap(), b"existing backup");
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
}
