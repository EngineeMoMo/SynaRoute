use super::{CachedFile, Stamp};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::{HashMap, HashSet}, fs, path::{Path, PathBuf}, sync::{Arc, Mutex}, time::{Duration, SystemTime}};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

pub(super) struct Watch {
    changed: Arc<Mutex<HashSet<PathBuf>>>,
    _watcher: Option<RecommendedWatcher>,
}

impl Watch {
    pub(super) fn new(home: &Path) -> Self {
        let changed = Arc::new(Mutex::new(HashSet::new()));
        let destination = changed.clone();
        let root = home.to_path_buf();
        let watcher = notify::recommended_watcher(move |event: notify::Result<Event>| {
            let mut changes = destination.lock().unwrap_or_else(|error| error.into_inner());
            match event {
                Ok(event) if !matches!(event.kind, EventKind::Access(_)) => {
                    for path in event.paths {
                        let relative = path.strip_prefix(&root).unwrap_or(&path);
                        let first = relative.components().next().map(|part| part.as_os_str().to_string_lossy());
                        if first.is_some_and(|name| matches!(name.as_ref(), "sessions" | "archived_sessions" | "config.toml" | ".codex-global-state.json" | "session_index.jsonl") || name.starts_with("state_")) {
                            changes.insert(path);
                        }
                    }
                }
                Err(_) => { changes.insert(root.clone()); }
                _ => {}
            }
            if changes.len() > 10_000 { changes.clear(); changes.insert(root.clone()); }
        }).ok().and_then(|mut watcher| watcher.watch(home, RecursiveMode::Recursive).ok().map(|()| watcher));
        Self { changed, _watcher: watcher }
    }

    pub(super) fn changed(&self) -> HashSet<PathBuf> {
        std::mem::take(&mut *self.changed.lock().unwrap_or_else(|error| error.into_inner()))
    }
}

type Snapshot = (Vec<(PathBuf, bool)>, Vec<Stamp>, usize);

pub(super) fn snapshot(home: &Path) -> Snapshot {
    fn walk(root: &Path, depth: usize, archived: bool, snapshot: &mut Snapshot) {
        if depth > 6 { snapshot.2 += 1; return; }
        let entries = match fs::read_dir(root) {
            Ok(entries) => entries,
            Err(error) => { if error.kind() != std::io::ErrorKind::NotFound { snapshot.2 += 1; } return; }
        };
        for entry in entries {
            let Ok(entry) = entry else { snapshot.2 += 1; continue; };
            let Ok(kind) = entry.file_type() else { snapshot.2 += 1; continue; };
            let path = entry.path();
            if kind.is_dir() { walk(&path, depth + 1, archived, snapshot); }
            else if kind.is_file() && entry.file_name().to_string_lossy().starts_with("rollout-") && path.extension().is_some_and(|extension| extension == "jsonl") {
                let Ok(metadata) = entry.metadata() else { snapshot.2 += 1; continue; };
                snapshot.0.push((path.clone(), archived));
                snapshot.1.push(Stamp { path, length: metadata.len(), modified: metadata.modified().ok(), created: metadata.created().ok() });
            }
        }
    }
    let mut snapshot = (Vec::new(), super::identity(home, &[]), 0);
    for name in ["sessions", "archived_sessions"] { walk(&home.join(name), 0, name == "archived_sessions", &mut snapshot); }
    snapshot.1.sort_by(|left, right| left.path.cmp(&right.path));
    snapshot
}

#[derive(Serialize, Deserialize)]
struct DiskIndex {
    version: u8,
    home: PathBuf,
    written: SystemTime,
    files: HashMap<PathBuf, CachedFile>,
}

fn location(home: &Path) -> Option<PathBuf> {
    #[cfg(test)]
    let root = home.join(".synaroute-test-index");
    #[cfg(not(test))]
    let root = crate::store::data_dir::app_data_dir().ok()?.join("codex-session-index");
    Some(root.join(format!("{:x}.json", Sha256::digest(home.as_os_str().to_string_lossy().as_bytes()))))
}

pub(super) fn load(home: &Path) -> HashMap<PathBuf, CachedFile> {
    let load = || -> Option<HashMap<PathBuf, CachedFile>> {
        let path = location(home)?;
        if fs::metadata(&path).ok()?.len() > 64 * 1024 * 1024 { return None; }
        let index: DiskIndex = serde_json::from_slice(&fs::read(path).ok()?).ok()?;
        if index.version != 1 || index.home != home || index.written.elapsed().ok()? > Duration::from_secs(86400) { return None; }
        Some(index.files)
    };
    load().unwrap_or_default()
}

pub(super) fn save(home: &Path, files: &HashMap<PathBuf, CachedFile>) {
    let save = || -> Option<()> {
        let path = location(home)?;
        fs::create_dir_all(path.parent()?).ok()?;
        let index = DiskIndex { version: 1, home: home.into(), written: SystemTime::now(), files: files.clone() };
        let bytes = serde_json::to_vec(&index).ok()?;
        if bytes.len() > 64 * 1024 * 1024 { return None; }
        crate::secret::atomic_write(&path, &bytes).ok()?;
        Some(())
    };
    let _ = save();
}

pub(super) fn parallel_parse(home: &Path, paths: &[(PathBuf, bool)], fingerprints: &HashMap<PathBuf, Stamp>, previous: &HashMap<PathBuf, CachedFile>, changed: &HashSet<PathBuf>) -> Vec<(PathBuf, CachedFile)> {
    let workers = std::thread::available_parallelism().map_or(1, usize::from).min(8).min(paths.len().max(1));
    std::thread::scope(|scope| {
        let handles: Vec<_> = paths.chunks(paths.len().div_ceil(workers).max(1)).map(|chunk| scope.spawn(move || {
            chunk.iter().map(|(path, archived)| {
                let fingerprint = fingerprints.get(path).cloned().unwrap_or_else(|| super::stamp(path.clone()));
                let file = previous.get(path).filter(|file| file.stamp == fingerprint && !changed.iter().any(|change| path.starts_with(change))).cloned().unwrap_or_else(|| {
                    let parsed = super::super::scan_paths(home, vec![(path.clone(), *archived)]);
                    CachedFile { stamp: fingerprint, row: parsed.sessions.into_iter().next(), unreadable: parsed.unreadable, rejected: parsed.path_rejected }
                });
                (path.clone(), file)
            }).collect::<Vec<_>>()
        })).collect();
        handles.into_iter().flat_map(|handle| handle.join().unwrap_or_else(|panic| std::panic::resume_unwind(panic))).collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persisted_index_survives_restart_and_bad_data_falls_back() {
        let directory = tempfile::tempdir().unwrap();
        let home = directory.path().canonicalize().unwrap();
        let path = home.join("sessions/rollout-fixture.jsonl");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, br#"{"type":"session_meta","payload":{"model_provider":"openai"}}"#).unwrap();
        let (paths, stamps, failures) = snapshot(&home);
        assert_eq!(failures, 0);
        let fingerprints = stamps.into_iter().map(|stamp| (stamp.path.clone(), stamp)).collect();
        let rows: HashMap<_, _> = parallel_parse(&home, &paths, &fingerprints, &HashMap::new(), &HashSet::new()).into_iter().collect();
        save(&home, &rows);
        let reloaded = load(&home);
        assert_eq!(reloaded[&path].row.as_ref().unwrap().provider, "openai");
        let changed = HashSet::from([path.clone()]);
        fs::write(&path, br#"{"type":"session_meta","payload":{"model_provider":"custom"}}"#).unwrap();
        let rows = parallel_parse(&home, &paths, &fingerprints, &reloaded, &changed);
        assert_eq!(rows[0].1.row.as_ref().unwrap().provider, "custom");
        fs::write(location(&home).unwrap(), b"{invalid").unwrap();
        assert!(load(&home).is_empty());
    }

    #[test]
    fn watcher_observes_rollout_changes_and_ignores_its_own_cache() {
        let directory = tempfile::tempdir().unwrap();
        let home = directory.path().canonicalize().unwrap();
        fs::create_dir_all(home.join("sessions")).unwrap();
        let watch = Watch::new(&home);
        assert!(watch._watcher.is_some());
        let path = home.join("sessions/rollout-watch.jsonl");
        fs::write(&path, b"fixture").unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        loop {
            if watch.changed().iter().any(|changed| changed == &path || changed == &home.join("sessions")) { break; }
            assert!(std::time::Instant::now() < deadline, "file change was not observed");
            std::thread::sleep(Duration::from_millis(10));
        }
        save(&home, &HashMap::new());
        std::thread::sleep(Duration::from_millis(40));
        assert!(watch.changed().iter().all(|path| !path.starts_with(home.join(".synaroute-test-index"))));
    }
}
