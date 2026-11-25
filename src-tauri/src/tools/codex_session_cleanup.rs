use super::super::{files, sqlite};
use super::IndexAudit;
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use std::collections::BTreeSet;
use std::fs;
use std::io::{self, Write};
use std::path::Path;

fn collect_files(directory: &Path, depth: usize, ids: &mut BTreeSet<String>) -> io::Result<()> {
    if depth > 6 { return Err(io::Error::other("会话目录超过扫描深度")); }
    let metadata = match fs::symlink_metadata(directory) {
        Ok(metadata) => metadata,
        Err(error) if depth == 0 && error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(io::Error::other("会话目录不是普通目录"));
    }
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            collect_files(&entry.path(), depth + 1, ids)?;
        } else if kind.is_symlink() {
            return Err(io::Error::other("会话目录包含未扫描的链接"));
        } else if kind.is_file() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let normalized = name.strip_suffix(".zst").unwrap_or(&name);
            if normalized.starts_with("rollout-") && normalized.ends_with(".jsonl") {
                let id = files::thread_id_from_filename(normalized)
                    .ok_or_else(|| io::Error::other("无法识别现有会话文件的标识"))?;
                ids.insert(id);
            }
        }
    }
    Ok(())
}

fn live_ids(home: &Path) -> Result<BTreeSet<String>, String> {
    let mut ids = BTreeSet::new();
    for directory in files::SESSION_DIRS {
        collect_files(&home.join(directory), 0, &mut ids)
            .map_err(|error| format!("会话文件未完整扫描，未执行索引清理：{error}"))?;
    }
    for path in sqlite::session_db_paths_checked(home)? {
        let read = (|| -> rusqlite::Result<()> {
            let database = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
            database.busy_timeout(std::time::Duration::from_millis(1500))?;
            for (table, column) in [("threads", "id"), ("local_thread_catalog", "thread_id")] {
                let has_table: bool = database.query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
                    [table], |row| row.get(0),
                )?;
                if !has_table { continue; }
                let mut statement = database.prepare(&format!("SELECT {column} FROM {table}"))?;
                for id in statement.query_map([], |row| row.get::<_, Option<String>>(0))? {
                    if let Some(id) = id?.filter(|id| !id.trim().is_empty()) { ids.insert(id); }
                }
            }
            Ok(())
        })();
        read.map_err(|error| format!("会话数据库无法核验，未执行索引清理（{}）：{error}", path.display()))?;
    }
    Ok(ids)
}

fn orphan_id(line: &str, live: &BTreeSet<String>) -> Option<String> {
    let record: Value = serde_json::from_str(line).ok()?;
    let id = record.get("id")?.as_str()?;
    if id.trim().is_empty() || live.contains(id) { return None; }
    uuid::Uuid::parse_str(id).ok()?;
    Some(id.into())
}

pub(super) fn audit(home: &Path) -> Result<IndexAudit, String> {
    let text = match fs::read_to_string(home.join("session_index.jsonl")) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(IndexAudit::default()),
        Err(error) => return Err(format!("无法读取会话索引：{error}")),
    };
    let live = live_ids(home)?;
    let mut report = IndexAudit::default();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        report.total += 1;
        if let Some(id) = orphan_id(line, &live) {
            report.orphans += 1;
            if report.sample.len() < 5 { report.sample.push(id); }
        }
    }
    Ok(report)
}

pub(super) fn prune(home: &Path, data_dir: &Path) -> Result<usize, String> {
    prune_observed(home, data_dir, &mut || Ok(()))
}

fn prune_observed(home: &Path, data_dir: &Path, before_replace: &mut dyn FnMut() -> Result<(), String>) -> Result<usize, String> {
    let path = home.join("session_index.jsonl");
    let text = fs::read_to_string(&path).map_err(|error| format!("读索引失败：{error}"))?;
    let live = live_ids(home)?;
    let mut removed = 0;
    let kept: String = text.split_inclusive('\n').filter(|line| {
        if orphan_id(line, &live).is_none() { return true; }
        removed += 1;
        false
    }).collect();
    if removed == 0 { return Ok(0); }
    files::backup_file(&path, data_dir, "codex-session-index", files::Keep::Count(5))?;
    let temporary = files::tmp_path_for(&path);
    let write = (|| -> Result<(), String> {
        let mut output = fs::OpenOptions::new().write(true).create_new(true).open(&temporary).map_err(|error| error.to_string())?;
        output.write_all(kept.as_bytes()).and_then(|_| output.sync_all()).map_err(|error| error.to_string())?;
        drop(output);
        before_replace()?;
        if live_ids(home)? != live {
            return Err("会话引用在清理期间发生变化，未替换索引，请重新扫描".into());
        }
        if fs::read(&path).map_err(|error| error.to_string())? != text.as_bytes() {
            return Err("会话索引在清理期间发生变化，未覆盖新内容，请重新扫描".into());
        }
        fs::rename(&temporary, &path).map_err(|error| error.to_string())
    })();
    if write.is_err() { let _ = fs::remove_file(&temporary); }
    write?;
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (std::path::PathBuf, String, String) {
        let home = std::env::temp_dir().join(format!("synaroute-cleanup-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(home.join("sessions")).unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let text = format!("{{\"id\":\"{id}\"}}\n");
        fs::write(home.join("session_index.jsonl"), &text).unwrap();
        (home, id, text)
    }

    #[test]
    fn incomplete_scans_and_unreadable_databases_stop_cleanup_without_backup() {
        for reason in ["directory", "depth", "database", "schema", "database-directory"] {
            let (home, _, original) = fixture();
            match reason {
                "directory" => fs::write(home.join("archived_sessions"), b"not a directory").unwrap(),
                "depth" => fs::create_dir_all(home.join("sessions/a/b/c/d/e/f/g")).unwrap(),
                "database" => fs::write(home.join("state_5.sqlite"), b"invalid SQLite").unwrap(),
                "schema" => {
                    let database = Connection::open(home.join("state_5.sqlite")).unwrap();
                    database.execute_batch("CREATE TABLE threads (unknown_id TEXT)").unwrap();
                }
                _ => fs::write(home.join("sqlite"), b"not a directory").unwrap(),
            }
            assert!(audit(&home).is_err(), "{reason}");
            assert!(prune(&home, &home.join("data")).is_err(), "{reason}");
            assert_eq!(fs::read_to_string(home.join("session_index.jsonl")).unwrap(), original);
            assert!(!home.join("data").exists());
            fs::remove_dir_all(home).unwrap();
        }
    }

    #[test]
    fn catalog_references_are_preserved_without_creating_any_database() {
        let (home, id, original) = fixture();
        fs::create_dir(home.join("sqlite")).unwrap();
        let database = Connection::open(home.join("sqlite/catalog.db")).unwrap();
        database.execute_batch("CREATE TABLE local_thread_catalog (thread_id TEXT, host_id TEXT)").unwrap();
        database.execute("INSERT INTO local_thread_catalog VALUES (?1, 'remote')", [&id]).unwrap();
        assert_eq!(audit(&home).unwrap().orphans, 0);
        assert_eq!(prune(&home, &home.join("data")).unwrap(), 0);
        assert_eq!(fs::read_to_string(home.join("session_index.jsonl")).unwrap(), original);
        assert!(!home.join("state_5.sqlite").exists());
        assert!(!home.join("data").exists());
        drop(database);
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn cleanup_preserves_unknown_records_and_exact_surviving_bytes() {
        let (home, id, _) = fixture();
        let kept = "\r\n{\"id\":\"future-format\"}\r\nnot JSON\n{\"id\":\" \"}";
        let original = format!("{{\"id\":\"{id}\"}}\r\n{kept}");
        fs::write(home.join("session_index.jsonl"), &original).unwrap();
        assert_eq!(prune(&home, &home.join("data")).unwrap(), 1);
        assert_eq!(fs::read_to_string(home.join("session_index.jsonl")).unwrap(), kept);
        let backup = fs::read_dir(home.join("data/backups/codex-session-index")).unwrap().next().unwrap().unwrap().path();
        assert_eq!(fs::read_to_string(backup).unwrap(), original);
        assert_eq!(prune(&home, &home.join("data")).unwrap(), 0);
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn new_index_content_during_cleanup_is_never_overwritten() {
        let (home, _, original) = fixture();
        let updated = format!("{original}{{\"id\":\"new-record\"}}\n");
        let result = prune_observed(&home, &home.join("data"), &mut || {
            fs::write(home.join("session_index.jsonl"), &updated).map_err(|error| error.to_string())
        });
        assert!(result.unwrap_err().contains("索引在清理期间发生变化"));
        assert_eq!(fs::read_to_string(home.join("session_index.jsonl")).unwrap(), updated);
        assert_eq!(fs::read_dir(&home).unwrap().count(), 3);
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn new_file_or_database_reference_during_cleanup_aborts() {
        for database_reference in [false, true] {
            let (home, id, original) = fixture();
            let result = prune_observed(&home, &home.join("data"), &mut || {
                if database_reference {
                    let database = Connection::open(home.join("state_5.sqlite")).unwrap();
                    database.execute_batch("CREATE TABLE threads (id TEXT)").unwrap();
                    database.execute("INSERT INTO threads VALUES (?1)", [&id]).unwrap();
                } else {
                    fs::write(home.join(format!("sessions/rollout-{id}.jsonl")), b"in progress").unwrap();
                }
                Ok(())
            });
            assert!(result.unwrap_err().contains("会话引用在清理期间发生变化"));
            assert_eq!(fs::read_to_string(home.join("session_index.jsonl")).unwrap(), original);
            fs::remove_dir_all(home).unwrap();
        }
    }
}
