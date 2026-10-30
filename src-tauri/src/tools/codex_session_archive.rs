use super::{scan_at, session_db_paths, sqlite};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveFailure {
    pub rel_path: String,
    pub error: String,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveResult {
    pub updated: Vec<String>,
    pub unchanged: Vec<String>,
    pub failed: Vec<ArchiveFailure>,
    pub warnings: Vec<String>,
    pub backup_dir: String,
}

fn update_database(
    database: &Path,
    data_dir: &Path,
    ids: &BTreeSet<String>,
    archived: bool,
) -> Result<Vec<(String, bool)>, String> {
    let mut connection = Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_WRITE)
        .map_err(|error| error.to_string())?;
    connection.busy_timeout(std::time::Duration::from_millis(800))
        .map_err(|error| error.to_string())?;
    let columns: BTreeSet<String> = connection.prepare("PRAGMA table_info(threads)")
        .and_then(|mut statement| statement.query_map([], |row| row.get(1))?.collect())
        .map_err(|error| error.to_string())?;
    if !columns.contains("id") {
        return Ok(Vec::new());
    }
    if !columns.contains("archived") {
        return Err("会话库缺少 archived 列，未执行写入".into());
    }
    let mut matches = Vec::new();
    {
        let mut statement = connection.prepare("SELECT COALESCE(archived, 0) FROM threads WHERE id = ?1")
            .map_err(|error| error.to_string())?;
        for id in ids {
            if let Some(current) = statement.query_row([id], |row| row.get::<_, bool>(0))
                .optional().map_err(|error| error.to_string())? {
                matches.push((id.clone(), current != archived));
            }
        }
    }
    if !matches.iter().any(|(_, changed)| *changed) {
        return Ok(matches);
    }
    sqlite::backup_db(database, data_dir)?;
    let transaction = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|error| error.to_string())?;
    let now = chrono::Utc::now();
    for (id, changed) in &mut matches {
        if !*changed {
            continue;
        }
        let affected = transaction.execute("UPDATE threads SET archived = ?1 WHERE id = ?2 AND COALESCE(archived, 0) <> ?1",
            rusqlite::params![archived, id.as_str()]).map_err(|error| error.to_string())?;
        *changed = affected > 0;
        if affected == 0 {
            continue;
        }
        for (column, value) in [("archived_at", now.timestamp()), ("archived_at_ms", now.timestamp_millis())] {
            if columns.contains(column) {
                transaction.execute(&format!("UPDATE threads SET {column} = ?1 WHERE id = ?2"),
                    rusqlite::params![archived.then_some(value), id.as_str()])
                    .map_err(|error| error.to_string())?;
            }
        }
    }
    transaction.commit().map_err(|error| error.to_string())?;
    Ok(matches)
}

fn archive_at(home: &Path, data_dir: &Path, rel_paths: &[String], archived: bool) -> ArchiveResult {
    let known: HashMap<_, _> = scan_at(home).sessions.into_iter()
        .map(|session| (session.rel_path, session.thread_id)).collect();
    let mut result = ArchiveResult {
        backup_dir: data_dir.join("backups/codex-sqlite").display().to_string(),
        ..ArchiveResult::default()
    };
    let mut selected = BTreeMap::new();
    for rel_path in rel_paths.iter().collect::<BTreeSet<_>>() {
        match known.get(rel_path).filter(|id| !id.is_empty()) {
            Some(id) => { selected.insert(rel_path.clone(), id.clone()); }
            None => result.failed.push(ArchiveFailure {
                rel_path: rel_path.clone(), error: "会话已不存在或无法安全识别，请刷新列表后重试".into(),
            }),
        }
    }
    if selected.is_empty() {
        return result;
    }
    let ids = selected.values().cloned().collect();
    let mut outcomes = HashMap::<String, bool>::new();
    for database in session_db_paths(home) {
        match update_database(&database, data_dir, &ids, archived) {
            Ok(rows) => {
                for (id, changed) in rows {
                    outcomes.entry(id).and_modify(|previous| *previous |= changed).or_insert(changed);
                }
            }
            Err(error) => result.warnings.push(format!("{}：{error}", database.display())),
        }
    }
    for (rel_path, id) in selected {
        match outcomes.get(&id) {
            Some(true) => result.updated.push(rel_path),
            Some(false) => result.unchanged.push(rel_path),
            None => result.failed.push(ArchiveFailure {
                rel_path, error: "未找到可更新的会话数据库记录；请检查会话库提示后重试".into(),
            }),
        }
    }
    result
}

#[tauri::command]
pub async fn set_codex_sessions_archived(rel_paths: Vec<String>, archived: bool) -> Result<ArchiveResult, String> {
    let home = super::super::codex_paths::codex_home().map_err(|error| error.to_string())?;
    let data_dir = crate::store::data_dir::app_data_dir().map_err(|error| error.to_string())?;
    tauri::async_runtime::spawn_blocking(move || {
        super::guard::ensure_stopped()?;
        let _guard = super::guard::acquire(&home)?;
        Ok(archive_at(&home, &data_dir, &rel_paths, archived))
    }).await.map_err(|error| error.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct Fixture {
        home: std::path::PathBuf,
        data: std::path::PathBuf,
        rel: String,
        id: String,
    }

    impl Fixture {
        fn new() -> Self {
            let home = std::env::temp_dir().join(format!("synaroute-archive-{}", uuid::Uuid::new_v4()));
            let id = uuid::Uuid::new_v4().to_string();
            let rel = format!("sessions/2026/10/30/rollout-2026-10-30T10-00-00-{id}.jsonl");
            fs::create_dir_all(home.join("sessions/2026/10/30")).unwrap();
            fs::write(home.join(&rel), format!("{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{id}\",\"thread_source\":\"user\"}}}}\nKEEP BODY\n")).unwrap();
            let connection = Connection::open(home.join("state_5.sqlite")).unwrap();
            connection.execute_batch("CREATE TABLE threads (id TEXT PRIMARY KEY, archived INTEGER NOT NULL DEFAULT 0, archived_at INTEGER, archived_at_ms INTEGER, title TEXT);").unwrap();
            connection.execute("INSERT INTO threads(id, title) VALUES (?1, 'keep title')", [&id]).unwrap();
            Self { data: home.join("data"), home, rel, id }
        }

        fn connection(&self) -> Connection {
            Connection::open(self.home.join("state_5.sqlite")).unwrap()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.home);
        }
    }

    #[test]
    fn archive_and_restore_back_up_database_and_never_move_or_change_rollout() {
        let fixture = Fixture::new();
        let original = fs::read(fixture.home.join(&fixture.rel)).unwrap();
        let result = archive_at(&fixture.home, &fixture.data, std::slice::from_ref(&fixture.rel), true);
        assert_eq!(result.updated, vec![fixture.rel.clone()]);
        assert!(result.failed.is_empty());
        assert!(result.warnings.is_empty());
        let connection = fixture.connection();
        let (archived, seconds, millis, title): (bool, i64, i64, String) = connection.query_row(
            "SELECT archived, archived_at, archived_at_ms, title FROM threads WHERE id = ?1", [&fixture.id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))).unwrap();
        assert!(archived);
        assert_eq!(millis / 1000, seconds);
        assert_eq!(title, "keep title");
        let backup = fs::read_dir(&result.backup_dir).unwrap().next().unwrap().unwrap().path();
        let snapshot = Connection::open(backup).unwrap();
        assert!(!snapshot.query_row("SELECT archived FROM threads", [], |row| row.get::<_, bool>(0)).unwrap());
        let restored = archive_at(&fixture.home, &fixture.data, std::slice::from_ref(&fixture.rel), false);
        assert_eq!(restored.updated, vec![fixture.rel.clone()]);
        assert!(connection.query_row("SELECT archived = 0 AND archived_at IS NULL AND archived_at_ms IS NULL FROM threads", [], |row| row.get::<_, bool>(0)).unwrap());
        assert_eq!(fs::read(fixture.home.join(&fixture.rel)).unwrap(), original);
        assert!(!fixture.home.join("archived_sessions").exists());
    }

    #[test]
    fn repeated_archive_is_noop_and_does_not_create_another_backup() {
        let fixture = Fixture::new();
        let paths = vec![fixture.rel.clone(), fixture.rel.clone()];
        let first = archive_at(&fixture.home, &fixture.data, &paths, true);
        assert_eq!(first.updated.len(), 1);
        let second = archive_at(&fixture.home, &fixture.data, &paths, true);
        assert!(second.updated.is_empty());
        assert_eq!(second.unchanged, vec![fixture.rel.clone()]);
        assert_eq!(fs::read_dir(first.backup_dir).unwrap().count(), 1);
    }

    #[test]
    fn failed_backup_leaves_database_and_rollout_intact() {
        let fixture = Fixture::new();
        fs::write(&fixture.data, "not a directory").unwrap();
        let result = archive_at(&fixture.home, &fixture.data, std::slice::from_ref(&fixture.rel), true);
        assert!(result.updated.is_empty());
        assert_eq!(result.failed.len(), 1);
        assert!(!result.warnings.is_empty());
        assert!(!fixture.connection().query_row("SELECT archived FROM threads", [], |row| row.get::<_, bool>(0)).unwrap());
        assert!(fixture.home.join(&fixture.rel).is_file());
    }

    #[test]
    fn rejects_unscanned_paths_and_reports_sessions_missing_from_database() {
        let fixture = Fixture::new();
        fixture.connection().execute("DELETE FROM threads", []).unwrap();
        let paths = vec![fixture.rel.clone(), "../outside.jsonl".into(), "config.toml".into()];
        let result = archive_at(&fixture.home, &fixture.data, &paths, true);
        assert!(result.updated.is_empty());
        assert_eq!(result.failed.len(), 3);
        assert!(!fixture.data.exists());
    }

    #[test]
    fn failed_timestamp_update_rolls_back_the_archive_change() {
        let fixture = Fixture::new();
        fixture.connection().execute_batch("CREATE TRIGGER reject_archived_time BEFORE UPDATE OF archived_at ON threads BEGIN SELECT RAISE(ABORT, 'blocked timestamp'); END;").unwrap();
        let result = archive_at(&fixture.home, &fixture.data, std::slice::from_ref(&fixture.rel), true);
        assert!(result.updated.is_empty());
        assert_eq!(result.failed.len(), 1);
        assert!(result.warnings.iter().any(|warning| warning.contains("blocked timestamp")));
        assert!(!fixture.connection().query_row("SELECT archived FROM threads", [], |row| row.get::<_, bool>(0)).unwrap());
    }

    #[test]
    fn a_failed_session_does_not_hide_successful_sessions_in_the_same_batch() {
        let fixture = Fixture::new();
        let paths = vec![fixture.rel.clone(), "sessions/missing.jsonl".into()];
        let result = archive_at(&fixture.home, &fixture.data, &paths, true);
        assert_eq!(result.updated, vec![fixture.rel.clone()]);
        assert_eq!(result.failed.len(), 1);
        assert_eq!(result.failed[0].rel_path, "sessions/missing.jsonl");
    }
}
