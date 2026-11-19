use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{fs, io::{Read, Write}, path::{Path, PathBuf}};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Manifest {
    pub version: String,
    pub files: Vec<Artifact>,
}

#[derive(Deserialize)]
pub(crate) struct Artifact {
    pub path: String,
    pub bytes: u64,
    pub sha256: String,
}

pub(crate) fn manifest() -> Manifest {
    serde_json::from_str(include_str!("../../scripts/laya/rust-bundle.json")).expect("embedded Laya bundle manifest")
}

pub(crate) fn artifact(root: &Path, entry: &Artifact) -> Result<PathBuf, String> {
    let root = root.canonicalize().map_err(|error| error.to_string())?;
    let path = root.join(&entry.path).canonicalize().map_err(|error| format!("{}: {error}", entry.path))?;
    if !path.starts_with(&root) || !path.is_file() {
        return Err(format!("[integrity] invalid bundle path: {}", entry.path));
    }
    Ok(path)
}

pub(crate) fn copy_verified(source: &Path, destination: Option<&Path>, entry: &Artifact) -> Result<(), String> {
    let mut input = fs::File::open(source).map_err(|error| error.to_string())?;
    if input.metadata().map_err(|error| error.to_string())?.len() != entry.bytes {
        return Err(format!("[integrity] invalid file length: {}", entry.path));
    }
    let mut output = destination.map(fs::File::create).transpose().map_err(|error| error.to_string())?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0; 1024 * 1024];
    let mut total = 0u64;
    loop {
        let count = input.read(&mut buffer).map_err(|error| error.to_string())?;
        if count == 0 { break; }
        total += count as u64;
        if total > entry.bytes { return Err(format!("[integrity] file changed: {}", entry.path)); }
        digest.update(&buffer[..count]);
        if let Some(output) = &mut output { output.write_all(&buffer[..count]).map_err(|error| error.to_string())?; }
    }
    if total != entry.bytes || format!("{:x}", digest.finalize()) != entry.sha256 {
        return Err(format!("[integrity] SHA-256 mismatch: {}", entry.path));
    }
    if let Some(output) = output { output.sync_all().map_err(|error| error.to_string())?; }
    Ok(())
}

pub(crate) fn verify_manifest(root: &Path, manifest: &Manifest) -> Result<(), String> {
    for entry in &manifest.files { copy_verified(&artifact(root, entry)?, None, entry)?; }
    Ok(())
}
