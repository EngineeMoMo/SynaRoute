use serde::{Deserialize, Serialize};
use std::{fs, path::{Path, PathBuf}};
use super::artifacts::{artifact, copy_verified, manifest, verify_manifest, Manifest};
#[cfg(test)]
use super::artifacts::Artifact;
#[cfg(test)]
use sha2::{Digest, Sha256};

#[derive(Deserialize, Serialize)]
struct Selection {
    active: String,
    previous: Option<String>,
}

fn selection(root: &Path) -> Result<Option<Selection>, String> {
    let bytes = match fs::read(root.join("selection.json")) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    let selected: Selection = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    let valid = |name: &str| name.strip_prefix("bundle-").is_some_and(|id| id.len() == 16 && id.bytes().all(|byte| byte.is_ascii_alphanumeric()));
    if !valid(&selected.active) || selected.previous.as_deref().is_some_and(|name| !valid(name) || name == selected.active) {
        return Err("[integrity] invalid bundle selection".into());
    }
    Ok(Some(selected))
}

fn activate(root: &Path, selection: &Selection) -> Result<(), String> {
    let mut pointer = tempfile::NamedTempFile::new_in(root).map_err(|error| error.to_string())?;
    serde_json::to_writer(&mut pointer, selection).map_err(|error| error.to_string())?;
    pointer.as_file().sync_all().map_err(|error| error.to_string())?;
    pointer.persist(root.join("selection.json")).map_err(|error| error.to_string())?;
    Ok(())
}

pub(crate) fn current(root: &Path) -> Result<PathBuf, String> {
    selection(root)?.map(|selected| root.join(selected.active)).ok_or_else(|| "[install] Rust Laya bundle has not been imported".into())
}

pub(crate) fn can_rollback(root: &Path) -> bool {
    selection(root).ok().flatten().and_then(|selected| selected.previous).is_some_and(|name| root.join(name).is_dir())
}

fn rollback_manifest(root: &Path, manifest: &Manifest) -> Result<(), String> {
    let selected = selection(root)?.ok_or("No installed bundle")?;
    let previous = selected.previous.ok_or("No previous bundle")?;
    verify_manifest(&root.join(&previous), manifest)?;
    activate(root, &Selection { active: previous, previous: Some(selected.active) })
}

pub(crate) fn rollback(root: &Path) -> Result<(), String> {
    rollback_manifest(root, &manifest())
}

pub(crate) fn installed(root: &Path) -> bool {
    current(root).is_ok_and(|directory| manifest().files.iter().all(|entry|
        directory.join(&entry.path).metadata().is_ok_and(|metadata| metadata.is_file() && metadata.len() == entry.bytes)))
}

pub(crate) fn install(source: &Path, root: &Path) -> Result<String, String> {
    install_manifest(source, root, &manifest())
}

fn install_manifest(source: &Path, root: &Path, manifest: &Manifest) -> Result<String, String> {
    fs::create_dir_all(root).map_err(|error| error.to_string())?;
    let previous = selection(root)?;
    let stage = tempfile::Builder::new().prefix("bundle-").rand_bytes(16).tempdir_in(root).map_err(|error| error.to_string())?;
    for entry in &manifest.files {
        let source = artifact(source, entry)?;
        let target = stage.path().join(&entry.path);
        fs::create_dir_all(target.parent().ok_or("invalid bundle path")?).map_err(|error| error.to_string())?;
        copy_verified(&source, Some(&target), entry)?;
    }
    let active = stage.path().file_name().and_then(|name| name.to_str()).ok_or("invalid staging directory")?.to_string();
    activate(root, &Selection { active, previous: previous.as_ref().map(|selected| selected.active.clone()) })?;
    let _ = stage.keep();
    if let Some(obsolete) = previous.and_then(|selected| selected.previous) {
        let directory = root.join(&obsolete);
        if directory.canonicalize().ok().zip(root.canonicalize().ok()).is_some_and(|(actual, root)| actual.parent() == Some(root.as_path()) && actual.file_name() == Some(obsolete.as_ref())) {
            if let Err(error) = fs::remove_dir_all(directory) { tracing::warn!("Could not prune old Laya bundle: {error}"); }
        }
    }
    Ok(manifest.version.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_rejects_corruption_and_keeps_the_selected_version() {
        let source = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        let manifest = Manifest { version: "fixture".into(), files: vec![Artifact { path: "model.bin".into(), bytes: 5,
            sha256: format!("{:x}", Sha256::digest(b"valid")) }] };
        fs::write(source.path().join("model.bin"), b"valid").unwrap();
        install_manifest(source.path(), target.path(), &manifest).unwrap();
        fs::write(source.path().join("model.bin"), b"wrong").unwrap();
        assert!(install_manifest(source.path(), target.path(), &manifest).unwrap_err().contains("SHA-256"));
        let first = current(target.path()).unwrap();
        assert_eq!(fs::read(first.join("model.bin")).unwrap(), b"valid");
        assert_eq!(fs::read_dir(target.path()).unwrap().count(), 2);
        assert!(!can_rollback(target.path()));
        fs::write(source.path().join("model.bin"), b"valid").unwrap();
        install_manifest(source.path(), target.path(), &manifest).unwrap();
        let second = current(target.path()).unwrap();
        assert_ne!(first, second);
        rollback_manifest(target.path(), &manifest).unwrap();
        assert_eq!(current(target.path()).unwrap(), first);
        fs::write(second.join("model.bin"), b"wrong").unwrap();
        assert!(rollback_manifest(target.path(), &manifest).is_err());
        assert_eq!(current(target.path()).unwrap(), first);
        install_manifest(source.path(), target.path(), &manifest).unwrap();
        assert!(!second.exists());
        assert!(first.is_dir());
    }
}
