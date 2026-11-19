#[path = "laya_artifacts.rs"]
pub(crate) mod artifacts;
#[path = "laya_bundle.rs"]
pub(crate) mod bundle;

use sha2::{Digest, Sha256};
use std::{fs, io::Write, path::{Path, PathBuf}, sync::LazyLock};

pub(crate) fn worker(root: &Path) -> Result<PathBuf, String> {
    static BINARY: &[u8] = include_bytes!(env!("SYNAROUTE_LAYA_WORKER"));
    static NAME: LazyLock<String> = LazyLock::new(|| format!("worker-{:x}.exe", Sha256::digest(BINARY)));
    fs::create_dir_all(root).map_err(|error| error.to_string())?;
    let executable = root.join(&*NAME);
    if executable.metadata().is_ok_and(|metadata| metadata.len() == BINARY.len() as u64)
        && fs::read(&executable).is_ok_and(|bytes| bytes == BINARY) {
        return Ok(executable);
    }
    let mut temp = tempfile::NamedTempFile::new_in(root).map_err(|error| error.to_string())?;
    temp.write_all(BINARY).and_then(|()| temp.as_file().sync_all()).map_err(|error| error.to_string())?;
    temp.persist(&executable).map_err(|error| error.to_string())?;
    Ok(executable)
}
