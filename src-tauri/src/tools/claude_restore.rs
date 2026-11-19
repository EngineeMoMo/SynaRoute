use super::{backup_path_for, created_marker_path_for, prerestore_path_for, restore_one};
use crate::error::{AppError, AppResult};
use serde_json::Value;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fs, path::{Path, PathBuf}, sync::{Mutex, MutexGuard}, time::Duration};

#[derive(Serialize, Deserialize)]
struct PendingRestore {
    version: u8,
    original_hash: String,
    staged_hash: String,
    target_hash: Option<String>,
    removed: Vec<String>,
    #[serde(default)]
    applied: bool,
}

fn digest(bytes: &[u8]) -> String { format!("{:x}", Sha256::digest(bytes)) }
fn pending_path(path: &Path) -> PathBuf { path.with_extension("json.synaroute-restore-pending") }

fn read_optional(path: &Path) -> AppResult<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn changed_backup() -> AppError {
    AppError::ToolConfig("Claude restore backup changed; retained recovery journal".into())
}

fn finish_cleanup(path: &Path, pending: &PendingRestore) -> AppResult<bool> {
    let backup = backup_path_for(path);
    if let Some(bytes) = read_optional(&backup)? {
        if Some(digest(&bytes)) != pending.target_hash { return Err(changed_backup()); }
        fs::remove_file(backup)?;
    }
    match fs::remove_file(created_marker_path_for(path)) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    fs::remove_file(pending_path(path))?;
    Ok(true)
}

pub(crate) fn recover_on_launch() {
    let result = super::claude_cli_settings_path().and_then(|path| {
        let _guard = lock();
        recover_locked(&path)
    });
    if let Err(error) = result { tracing::warn!("Claude interrupted restore: {error}"); }
}

pub(super) fn recover_locked(path: &Path) -> AppResult<bool> {
    recover_with_wait(path, || std::thread::sleep(Duration::from_secs(10)))
}

fn recover_with_wait(path: &Path, wait: impl FnOnce()) -> AppResult<bool> {
    let Some(journal) = read_optional(&pending_path(path))? else { return Ok(false) };
    let mut pending: PendingRestore = serde_json::from_slice(&journal)?;
    if pending.version != 1 || pending.removed.iter().any(|key| !HOT_ENV.contains(&key.as_str())) {
        return Err(AppError::ToolConfig("Unsupported Claude restore journal".into()));
    }
    if pending.applied { return finish_cleanup(path, &pending); }
    let backup = backup_path_for(path);
    let target = read_optional(&backup)?;
    let current = read_optional(path)?;
    if target.as_deref().map(digest) != pending.target_hash {
        if target.is_none() && current.as_deref().map(digest) == pending.target_hash {
            return finish_cleanup(path, &pending);
        }
        return Err(changed_backup());
    }
    if let Some(current) = &current {
        if digest(current) == pending.original_hash {
            let temporary = staged(current, target.as_deref()).filter(|(bytes, _)| digest(bytes) == pending.staged_hash)
                .ok_or_else(|| AppError::ToolConfig("Claude restore journal does not match settings".into()))?;
            crate::secret::atomic_write(path, &temporary.0)?;
        }
        wait();
        if read_optional(&backup)? != target { return Err(changed_backup()); }
        match fs::read(path) {
            Ok(current) => {
                let bytes = if digest(&current) == pending.staged_hash { target.clone() }
                    else { Some(clean_current(&current, &pending.removed)?) };
                match bytes {
                    Some(bytes) if bytes == current => {}
                    Some(bytes) => crate::secret::atomic_write(path, &bytes)?,
                    None => fs::remove_file(path)?,
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    pending.applied = true;
    crate::secret::atomic_write(&pending_path(path), &serde_json::to_vec(&pending)?)?;
    finish_cleanup(path, &pending)
}

static WRITES: Mutex<()> = Mutex::new(());
const HOT_ENV: &[&str] = &[
    "ANTHROPIC_BASE_URL", "ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_API_KEY", "ANTHROPIC_CUSTOM_HEADERS",
    "ANTHROPIC_MODEL", "ANTHROPIC_SMALL_FAST_MODEL", "CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY",
    "ANTHROPIC_DEFAULT_OPUS_MODEL", "ANTHROPIC_DEFAULT_SONNET_MODEL", "ANTHROPIC_DEFAULT_HAIKU_MODEL", "ANTHROPIC_DEFAULT_FABLE_MODEL",
];

pub(super) fn lock() -> MutexGuard<'static, ()> { WRITES.lock().unwrap_or_else(|error| error.into_inner()) }

fn staged(current: &[u8], target: Option<&[u8]>) -> Option<(Vec<u8>, Vec<String>)> {
    let current: Value = serde_json::from_slice(current).ok()?;
    let mut target: Value = target.map(serde_json::from_slice).transpose().ok()?.unwrap_or(serde_json::json!({}));
    let current_env = current.get("env")?.as_object()?;
    let target_env = target.get("env").and_then(Value::as_object);
    let removed: Vec<_> = HOT_ENV.iter().filter(|key| current_env.get(**key).and_then(Value::as_str).is_some_and(|value| !value.is_empty())
        && !target_env.is_some_and(|environment| environment.contains_key(**key))).map(|key| key.to_string()).collect();
    if removed.is_empty() { return None; }
    let environment = target.as_object_mut()?.entry("env").or_insert_with(|| serde_json::json!({})).as_object_mut()?;
    for key in &removed { environment.insert(key.clone(), Value::String(String::new())); }
    Some((serde_json::to_vec_pretty(&target).ok()?, removed))
}

fn clean_current(current: &[u8], removed: &[String]) -> AppResult<Vec<u8>> {
    let mut value: Value = serde_json::from_slice(current).map_err(|error| AppError::ToolConfig(format!("Claude settings changed during restore: {error}")))?;
    let mut changed = false;
    if let Some(environment) = value.get_mut("env").and_then(Value::as_object_mut) {
        for key in removed {
            if environment.get(key).and_then(Value::as_str) == Some("") {
                environment.remove(key);
                changed = true;
            }
        }
    }
    if changed { Ok(serde_json::to_vec_pretty(&value)?) } else { Ok(current.to_vec()) }
}

pub(super) fn restore(path: &Path) -> AppResult<bool> {
    let _guard = lock();
    restore_with_wait(path, || std::thread::sleep(Duration::from_secs(10)))
}

fn restore_with_wait(path: &Path, wait: impl FnOnce()) -> AppResult<bool> {
    if pending_path(path).exists() { return recover_with_wait(path, wait); }
    let backup = backup_path_for(path);
    let marker = created_marker_path_for(path);
    if !backup.exists() && !marker.exists() { return Ok(false); }
    let target = if backup.exists() { Some(fs::read(&backup)?) } else { None };
    let current = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error.into()),
    };
    let Some((temporary, removed)) = staged(&current, target.as_deref()) else { return restore_one(path); };
    crate::secret::atomic_write(&prerestore_path_for(path), &current)?;
    let pending = PendingRestore { version: 1, original_hash: digest(&current), staged_hash: digest(&temporary),
        target_hash: target.as_deref().map(digest), removed, applied: false };
    crate::secret::atomic_write(&pending_path(path), &serde_json::to_vec(&pending)?)?;
    crate::secret::atomic_write(path, &temporary)?;
    recover_with_wait(path, wait)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn live_session_observes_empty_then_exact_backup_and_original_scene_survives() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let original = b"{\"permissions\":{\"allow\":[]}}\n";
        fs::write(backup_path_for(&path), original).unwrap();
        let live = serde_json::to_vec(&json!({"env":{"ANTHROPIC_BASE_URL":"http://127.0.0.1:12345","ANTHROPIC_CUSTOM_HEADERS":"X-Fixture: before"}})).unwrap();
        fs::write(&path, &live).unwrap();
        assert!(restore_with_wait(&path, || {
            let temporary: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            assert_eq!(temporary["env"]["ANTHROPIC_BASE_URL"], "");
            assert!(backup_path_for(&path).exists());
        }).unwrap());
        assert_eq!(fs::read(&path).unwrap(), original);
        assert_eq!(fs::read(prerestore_path_for(&path)).unwrap(), live);
        assert!(!backup_path_for(&path).exists());
    }

    #[test]
    fn concurrent_user_edits_and_new_provider_values_survive_cleanup() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        fs::write(created_marker_path_for(&path), "").unwrap();
        fs::write(&path, br#"{"env":{"ANTHROPIC_BASE_URL":"old","ANTHROPIC_AUTH_TOKEN":"fixture"}}"#).unwrap();
        restore_with_wait(&path, || {
            fs::write(&path, br#"{"env":{"ANTHROPIC_BASE_URL":"new","ANTHROPIC_AUTH_TOKEN":"","USER_SETTING":"keep"},"permissions":{"allow":["Read"]}}"#).unwrap();
        }).unwrap();
        let restored: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(restored["env"]["ANTHROPIC_BASE_URL"], "new");
        assert_eq!(restored["env"]["USER_SETTING"], "keep");
        assert!(restored["env"].get("ANTHROPIC_AUTH_TOKEN").is_none());
        assert_eq!(restored["permissions"]["allow"][0], "Read");
    }

    #[test]
    fn explicit_target_values_and_unrelated_environment_are_not_cleared() {
        assert!(staged(br#"{"env":{"ANTHROPIC_BASE_URL":"old","AWS_REGION":"test"}}"#, Some(br#"{"env":{"ANTHROPIC_BASE_URL":"target"}}"#)).is_none());
        assert!(staged(br#"{"env":{"AWS_REGION":"test"}}"#, Some(b"{}")).is_none());
    }

    #[test]
    fn interrupted_cleanup_preserves_exact_later_edits() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let marker = created_marker_path_for(&path);
        fs::write(backup_path_for(&path), b"{}").unwrap();
        fs::write(&path, br#"{"env":{"ANTHROPIC_BASE_URL":"old"}}"#).unwrap();
        let edited = br#"{ "env":{ "ANTHROPIC_BASE_URL":"new" }, "another":true }"#;
        let outcome = restore_with_wait(&path, || {
            fs::write(&path, edited).unwrap();
            fs::create_dir(&marker).unwrap();
        });
        assert!(outcome.is_err());
        assert_eq!(fs::read(&path).unwrap(), edited);
        assert!(!backup_path_for(&path).exists());
        fs::remove_dir(marker).unwrap();
        let later = br#"{ "env": {"ANTHROPIC_BASE_URL":""}, "user": 2 }"#;
        fs::write(&path, later).unwrap();
        assert!(recover_with_wait(&path, || panic!("already restored")).unwrap());
        assert_eq!(fs::read(&path).unwrap(), later);
        assert!(!pending_path(&path).exists());
    }

    #[test]
    fn replacing_the_backup_during_wait_preserves_both_files() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let backup = backup_path_for(&path);
        fs::write(&backup, b"{}").unwrap();
        fs::write(&path, br#"{"env":{"ANTHROPIC_BASE_URL":"old"}}"#).unwrap();
        let edited = b"{\"changed\": true}";
        assert!(restore_with_wait(&path, || {
            fs::write(&backup, edited).unwrap();
            fs::write(&path, edited).unwrap();
        }).is_err());
        assert_eq!(fs::read(backup).unwrap(), edited);
        assert_eq!(fs::read(&path).unwrap(), edited);
        assert!(pending_path(&path).exists());
    }

    #[test]
    fn interrupted_wait_recovers_exact_backup_and_is_idempotent() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let original = b"{\"permissions\":{\"allow\":[]}}\n";
        fs::write(backup_path_for(&path), original).unwrap();
        fs::write(&path, br#"{"env":{"ANTHROPIC_AUTH_TOKEN":"fixture-secret"}}"#).unwrap();
        let interrupted = std::panic::catch_unwind(|| restore_with_wait(&path, || panic!("simulated process exit")));
        assert!(interrupted.is_err());
        assert!(pending_path(&path).exists());
        assert!(!fs::read_to_string(pending_path(&path)).unwrap().contains("fixture-secret"));
        assert!(recover_with_wait(&path, || {}).unwrap());
        assert_eq!(fs::read(&path).unwrap(), original);
        assert!(!recover_with_wait(&path, || {}).unwrap());
    }

    #[test]
    fn recovery_preserves_later_provider_and_rejects_changed_backup() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        fs::write(backup_path_for(&path), b"{}").unwrap();
        fs::write(&path, br#"{"env":{"ANTHROPIC_BASE_URL":"original"}}"#).unwrap();
        let _ = std::panic::catch_unwind(|| restore_with_wait(&path, || panic!("exit")));
        fs::write(backup_path_for(&path), b"{\"changed\":true}").unwrap();
        assert!(recover_with_wait(&path, || {}).is_err());
        assert!(pending_path(&path).exists());
        fs::write(backup_path_for(&path), b"{}").unwrap();
        fs::write(&path, br#"{"env":{"ANTHROPIC_BASE_URL":"new-provider"}}"#).unwrap();
        recover_with_wait(&path, || {}).unwrap();
        let restored: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(restored["env"]["ANTHROPIC_BASE_URL"], "new-provider");
    }
}
