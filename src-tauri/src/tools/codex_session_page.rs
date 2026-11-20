use super::{files, ops::CodexSessionList, view, ScanReport, SessionRef};
use crate::model::ProviderKey;
use serde::{Deserialize, Serialize};
use std::{collections::{BTreeMap, HashMap}, fs, path::{Path, PathBuf}, sync::{Arc, Mutex, OnceLock}, time::{Duration, Instant, SystemTime}};
#[path = "codex_session_index.rs"] mod storage;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PageQuery {
    pub query: String,
    pub archive: String,
    pub source: String,
    pub only_mismatched: bool,
    pub only_model_gone: bool,
    pub sort: String,
    pub grouping: String,
    pub page: usize,
    pub page_size: usize,
    pub refresh: bool,
    pub selected: Vec<String>,
}

impl Default for PageQuery {
    fn default() -> Self {
        Self { query: String::new(), archive: "active".into(), source: "user".into(), only_mismatched: false,
            only_model_gone: false, sort: "updated".into(), grouping: "project".into(), page: 0, page_size: 50, refresh: false, selected: Vec::new() }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PageInfo {
    pub filtered_total: usize,
    pub page: usize,
    pub page_size: usize,
    pub provider_counts: BTreeMap<String, usize>,
    pub retained_selection: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Stamp { path: PathBuf, length: u64, modified: Option<SystemTime>, created: Option<SystemTime> }

fn stamp(path: PathBuf) -> Stamp {
    let metadata = fs::metadata(&path).ok();
    Stamp { path, length: metadata.as_ref().map_or(0, fs::Metadata::len),
        modified: metadata.as_ref().and_then(|metadata| metadata.modified().ok()),
        created: metadata.and_then(|metadata| metadata.created().ok()) }
}

fn identity(home: &Path, paths: &[(PathBuf, bool)]) -> Vec<Stamp> {
    let mut files: Vec<_> = paths.iter().map(|(path, _)| path.clone()).collect();
    files.extend(["config.toml", ".codex-global-state.json", "session_index.jsonl"].map(|name| home.join(name)));
    for database in super::session_db_paths(home) {
        files.push(database.clone());
        files.push(PathBuf::from(format!("{}-wal", database.display())));
        files.push(PathBuf::from(format!("{}-shm", database.display())));
    }
    files.sort();
    files.into_iter().map(stamp).collect()
}

#[derive(Clone, Serialize, Deserialize)]
struct CachedFile { stamp: Stamp, row: Option<SessionRef>, unreadable: usize, rejected: usize }
struct Index {
    at: Instant,
    identity: Vec<Stamp>,
    keys: String,
    files: HashMap<PathBuf, CachedFile>,
    snapshot: CodexSessionList,
}

struct HomeState { index: Option<Index>, watch: storage::Watch }
type HomeIndex = Arc<Mutex<HomeState>>;
static INDEXES: OnceLock<Mutex<HashMap<PathBuf, HomeIndex>>> = OnceLock::new();

pub(super) fn list(home: &Path, keys: &[ProviderKey], query: &PageQuery) -> Result<CodexSessionList, String> {
    list_with_snapshot(home, keys, query, storage::snapshot)
}

fn list_with_snapshot(home: &Path, keys: &[ProviderKey], query: &PageQuery, mut read_snapshot: impl FnMut(&Path) -> storage::Snapshot) -> Result<CodexSessionList, String> {
    if query.query.len() > 4096 || query.selected.len() > 100_000 || !matches!(query.archive.as_str(), "all" | "active" | "archived")
        || !matches!(query.source.as_str(), "all" | "user" | "internal") || !matches!(query.sort.as_str(), "updated" | "created" | "title")
        || !matches!(query.grouping.as_str(), "project" | "time") {
        return Err("无效的会话查询 / Invalid session query".into());
    }
    let home = match home.canonicalize() {
        Ok(home) => home,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(select(&CodexSessionList {
            rows: Vec::new(), page: None, current_provider: String::new(), unreadable: 0, path_rejected: 0, stats: Default::default(),
        }, query)),
        Err(error) => return Err(error.to_string()),
    };
    let keys_signature = serde_json::to_string(keys).map_err(|error| error.to_string())?;
    let entry = {
        let mut indexes = INDEXES.get_or_init(Default::default).lock().map_err(|_| "Session index unavailable")?;
        if indexes.len() >= 4 && !indexes.contains_key(&home) { indexes.clear(); }
        indexes.entry(home.clone()).or_insert_with(|| Arc::new(Mutex::new(HomeState { index: None, watch: storage::Watch::new(&home) }))).clone()
    };
    let mut state = entry.lock().map_err(|_| "Session index unavailable")?;
    let changed = state.watch.changed();
    let persisted = if state.index.is_none() && !query.refresh { storage::load(&home) } else { HashMap::new() };
    for attempt in 0..2 {
        let (paths, before, scan_failures) = read_snapshot(&home);
        let cached = state.index.as_ref();
        let expired = query.refresh || cached.is_none_or(|index| index.at.elapsed() >= Duration::from_secs(30));
        if !expired && changed.is_empty() && scan_failures == 0 && cached.is_some_and(|index| index.identity == before && index.keys == keys_signature && index.snapshot.stats.metadata_failures == 0) {
            return Ok(select(&cached.unwrap().snapshot, query));
        }
        let mut scan = ScanReport::default();
        let mut next_files = HashMap::new();
        let fingerprints: HashMap<_, _> = before.iter().map(|stamp| (stamp.path.clone(), stamp.clone())).collect();
        let parsed_files = storage::parallel_parse(&home, &paths, &fingerprints, cached.filter(|_| !expired).map(|index| &index.files).unwrap_or(&persisted), &changed);
        for (path, file) in parsed_files {
            scan.unreadable += file.unreadable;
            scan.path_rejected += file.rejected;
            if let Some(row) = &file.row { scan.sessions.push(row.clone()); }
            next_files.insert(path, file);
        }
        let current_provider = fs::read_to_string(home.join("config.toml")).ok().and_then(|text| text.parse::<toml::Value>().ok())
            .and_then(|value| value.get("model_provider")?.as_str().map(str::to_owned)).unwrap_or_default();
        let mut stats = view::enrich(&home, &mut scan.sessions, &current_provider, keys);
        let (_, after, after_failures) = read_snapshot(&home);
        stats.metadata_failures += scan_failures.max(after_failures);
        stats.indexing_changed = before != after;
        if stats.indexing_changed && attempt == 0 { continue; }
        let snapshot = CodexSessionList { rows: scan.sessions, page: None, current_provider, unreadable: scan.unreadable, path_rejected: scan.path_rejected, stats };
        let result = select(&snapshot, query);
        if snapshot.stats.indexing_changed { return Ok(result); }
        if stats_safe(&snapshot) { storage::save(&home, &next_files); }
        state.index = Some(Index { at: Instant::now(), identity: after, keys: keys_signature.clone(), files: next_files, snapshot });
        return Ok(result);
    }
    Err("Session index unavailable".into())
}

fn stats_safe(snapshot: &CodexSessionList) -> bool { snapshot.stats.metadata_failures == 0 && snapshot.unreadable == 0 && snapshot.path_rejected == 0 }

fn time(row: &SessionRef, sort: &str) -> i64 {
    let text = if sort == "created" || row.updated_at.is_empty() { &row.timestamp } else { &row.updated_at };
    chrono::DateTime::parse_from_rfc3339(text).map_or(0, |time| time.timestamp_millis())
}

fn project(row: &SessionRef) -> String {
    let displayed = files::display_path(&row.cwd).replace('\\', "/");
    let path = if displayed == "/" { displayed } else { displayed.trim_end_matches('/').to_string() };
    if path.is_empty() { format!("project:{}", row.project) }
    else if path.as_bytes().get(1) == Some(&b':') || path.starts_with("//") { path.to_lowercase() }
    else { path }
}

fn select(snapshot: &CodexSessionList, query: &PageQuery) -> CodexSessionList {
    let terms: Vec<_> = query.query.split_whitespace().map(str::to_lowercase).collect();
    let mut rows: Vec<_> = snapshot.rows.iter().filter(|row| {
        (query.archive == "all" || row.archived == (query.archive == "archived"))
            && (query.source == "all" || row.internal == (query.source == "internal"))
            && (!query.only_mismatched || (!snapshot.current_provider.is_empty() && row.provider != snapshot.current_provider))
            && (!query.only_model_gone || row.model_unserviceable)
            && {
                let text = [&row.title, &row.cwd, &row.provider, &row.model, &row.project, &row.thread_id, &row.rel_path].map(String::as_str).join(" ").to_lowercase();
                terms.iter().all(|term| text.contains(term))
            }
    }).collect();
    rows.sort_by(|left, right| {
        (if query.sort == "title" { left.title.to_lowercase().cmp(&right.title.to_lowercase()) } else { time(right, &query.sort).cmp(&time(left, &query.sort)) })
            .then_with(|| left.rel_path.cmp(&right.rel_path))
    });
    let today = chrono::Local::now().date_naive();
    let group_key = |row: &SessionRef| {
        if query.grouping == "project" { return project(row); }
        let elapsed = chrono::DateTime::from_timestamp_millis(time(row, "updated")).filter(|_| time(row, "updated") != 0)
            .map(|time| today.signed_duration_since(time.with_timezone(&chrono::Local).date_naive()).num_days());
        match elapsed { None => "unknown", Some(days) if days <= 0 => "today", Some(1) => "yesterday", Some(days) if days <= 6 => "week", _ => "older" }.into()
    };
    let mut group_order = HashMap::new();
    for row in &rows {
        let index = group_order.len();
        group_order.entry(group_key(row)).or_insert(index);
    }
    rows.sort_by_key(|row| group_order[&group_key(row)]);
    let page_size = query.page_size.clamp(1, 200);
    let total = rows.len();
    let page = query.page.min(total.saturating_sub(1) / page_size);
    let mut provider_counts = BTreeMap::new();
    for row in &snapshot.rows { *provider_counts.entry(row.provider.clone()).or_default() += 1; }
    let paths: std::collections::HashSet<_> = snapshot.rows.iter().map(|row| row.rel_path.as_str()).collect();
    let retained_selection = query.selected.iter().filter(|path| paths.contains(path.as_str())).cloned().collect();
    CodexSessionList { rows: rows.into_iter().skip(page * page_size).take(page_size).cloned().collect(),
        page: Some(PageInfo { filtered_total: total, page, page_size, provider_counts, retained_selection }),
        stats: snapshot.stats.clone(), current_provider: snapshot.current_provider.clone(), unreadable: snapshot.unreadable, path_rejected: snapshot.path_rejected }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(home: &Path, index: usize) -> PathBuf {
        let path = home.join("sessions").join(format!("rollout-2026-11-17-00000000-0000-4000-8000-{index:012}.jsonl"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, format!("{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"00000000-0000-4000-8000-{index:012}\",\"model_provider\":\"openai\",\"cwd\":\"D:/project\",\"timestamp\":\"2026-11-17T12:00:00Z\"}}}}\n")).unwrap();
        path
    }
    #[test]
    fn pages_cover_rollout_only_sessions_and_refresh_on_changes() {
        let directory = tempfile::tempdir().unwrap();
        let paths: Vec<_> = (0..125).map(|index| fixture(directory.path(), index)).collect();
        let mut query = PageQuery { archive: "all".into(), source: "all".into(), ..Default::default() };
        let first = list(directory.path(), &[], &query).unwrap();
        assert_eq!(first.rows.len(), 50);
        assert_eq!(first.page.unwrap().filtered_total, 125);
        query.page = 2;
        assert_eq!(list(directory.path(), &[], &query).unwrap().rows.len(), 25);
        fs::remove_file(&paths[0]).unwrap();
        query.query = "00000000-0000-4000-8000-000000000000".into();
        assert!(list(directory.path(), &[], &query).unwrap().rows.is_empty());
        query.query.clear();
        query.page = usize::MAX;
        assert_eq!(list(directory.path(), &[], &query).unwrap().page.unwrap().page, 2);
        let other = tempfile::tempdir().unwrap();
        fixture(other.path(), 0);
        assert_eq!(list(other.path(), &[], &query).unwrap().rows.len(), 1);
        query.selected = vec![files::rel_of(directory.path(), &paths[0]).unwrap(), files::rel_of(directory.path(), &paths[124]).unwrap()];
        assert_eq!(list(directory.path(), &[], &query).unwrap().page.unwrap().retained_selection.len(), 1);
    }

    #[test]
    fn wal_changes_and_database_read_failures_are_visible() {
        let directory = tempfile::tempdir().unwrap();
        fixture(directory.path(), 1);
        let database = directory.path().join("state_5.sqlite");
        let connection = rusqlite::Connection::open(&database).unwrap();
        connection.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE threads(id TEXT, title TEXT, archived INTEGER); INSERT INTO threads VALUES ('00000000-0000-4000-8000-000000000001','Before',0);").unwrap();
        let query = PageQuery { source: "all".into(), ..Default::default() };
        assert_eq!(list(directory.path(), &[], &query).unwrap().rows[0].title, "Before");
        connection.execute("UPDATE threads SET title='After', archived=1", []).unwrap();
        assert!(list(directory.path(), &[], &query).unwrap().rows.is_empty());
        let query = PageQuery { archive: "all".into(), ..query };
        assert_eq!(list(directory.path(), &[], &query).unwrap().rows[0].title, "After");
        drop(connection);
        fs::write(&database, "invalid database").unwrap();
        let fallback = list(directory.path(), &[], &query).unwrap();
        assert_eq!(fallback.rows.len(), 1);
        assert!(fallback.stats.metadata_failures > 0);
    }

    #[test]
    fn continuous_writes_return_rows_without_caching_an_unstable_snapshot() {
        use std::io::Write;

        for source in ["rollout", "wal", "config", "index", "projects"] {
            let directory = tempfile::tempdir().unwrap();
            let home = directory.path().canonicalize().unwrap();
            let path = fixture(&home, 1);
            let connection = if source == "wal" {
                let connection = rusqlite::Connection::open(home.join("state_5.sqlite")).unwrap();
                connection.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE threads(id TEXT, title TEXT); INSERT INTO threads VALUES ('00000000-0000-4000-8000-000000000001','Initial');").unwrap();
                Some(connection)
            } else { None };
            let query = PageQuery { source: "all".into(), refresh: true, ..Default::default() };
            let mut reads = 0;
            let result = list_with_snapshot(&home, &[], &query, |home| {
                reads += 1;
                if reads % 2 == 0 {
                    match source {
                        "rollout" => fs::OpenOptions::new().append(true).open(&path).unwrap().write_all(b"{}\n").unwrap(),
                        "wal" => { connection.as_ref().unwrap().execute("UPDATE threads SET title=?1", [format!("Update {reads}")]).unwrap(); }
                        "config" => fs::write(home.join("config.toml"), format!("model_provider = 'provider_{reads}'\n")).unwrap(),
                        "index" => fs::OpenOptions::new().create(true).append(true).open(home.join("session_index.jsonl")).unwrap().write_all(b"{}\n").unwrap(),
                        _ => fs::write(home.join(".codex-global-state.json"), format!("{{\"revision\":{reads}}}")).unwrap(),
                    }
                }
                storage::snapshot(home)
            }).unwrap();
            assert_eq!(reads, 4, "{source}: expected one retry");
            assert_eq!(result.rows.len(), 1, "{source}");
            assert!(result.stats.indexing_changed, "{source}");
            assert_eq!(result.stats.metadata_failures, 0, "{source}");
            assert_eq!(result.page.unwrap().filtered_total, 1);
            assert!(storage::load(&home).is_empty(), "{source}: unstable snapshot persisted");
            let entry = INDEXES.get().unwrap().lock().unwrap()[&home].clone();
            assert!(entry.lock().unwrap().index.is_none(), "{source}: unstable snapshot cached");

            let stable = list(&home, &[], &query).unwrap();
            assert!(!stable.stats.indexing_changed, "{source}");
            assert_eq!(stable.rows.len(), 1);
            assert_eq!(stable.rows[0].bytes, fs::metadata(&path).unwrap().len());
            if source == "wal" { assert_eq!(stable.rows[0].title, "Update 4"); }
            if source == "config" { assert_eq!(stable.current_provider, "provider_4"); }
            let saved = storage::load(&home)[&path].stamp.clone();
            let cached_at = entry.lock().unwrap().index.as_ref().unwrap().at;
            let mut reads = 0;
            let busy = list_with_snapshot(&home, &[], &query, |home| {
                reads += 1;
                if reads % 2 == 0 {
                    fs::OpenOptions::new().append(true).open(&path).unwrap().write_all(b"{}\n").unwrap();
                }
                storage::snapshot(home)
            }).unwrap();
            assert!(busy.stats.indexing_changed);
            assert_eq!(storage::load(&home)[&path].stamp, saved);
            assert_eq!(entry.lock().unwrap().index.as_ref().unwrap().at, cached_at);
            let updated = list(&home, &[], &PageQuery { refresh: false, ..query }).unwrap();
            assert!(!updated.stats.indexing_changed);
            assert_eq!(updated.rows[0].bytes, fs::metadata(&path).unwrap().len());
        }
    }

    #[test]
    fn a_single_concurrent_change_recovers_on_retry() {
        use std::io::Write;

        let directory = tempfile::tempdir().unwrap();
        let path = fixture(directory.path(), 1);
        let mut reads = 0;
        let result = list_with_snapshot(directory.path(), &[], &PageQuery::default(), |home| {
            reads += 1;
            if reads == 2 {
                fs::OpenOptions::new().append(true).open(&path).unwrap().write_all(b"{}\n").unwrap();
            }
            storage::snapshot(home)
        }).unwrap();
        assert_eq!(reads, 4);
        assert!(!result.stats.indexing_changed);
        assert_eq!(result.rows[0].bytes, fs::metadata(&path).unwrap().len());
        let home = directory.path().canonicalize().unwrap();
        let saved = storage::load(&home);
        assert_eq!(saved[&home.join(path.strip_prefix(directory.path()).unwrap())].row.as_ref().unwrap().bytes, result.rows[0].bytes);
    }

    #[test]
    fn full_refresh_reloads_same_length_same_mtime_provider_rewrite() {
        let directory = tempfile::tempdir().unwrap();
        let path = fixture(directory.path(), 2);
        let mut query = PageQuery::default();
        assert_eq!(list(directory.path(), &[], &query).unwrap().rows[0].provider, "openai");
        let modified = fs::metadata(&path).unwrap().modified().unwrap();
        let text = fs::read_to_string(&path).unwrap().replace("openai", "custom");
        fs::write(&path, text).unwrap();
        fs::OpenOptions::new().write(true).open(&path).unwrap().set_times(fs::FileTimes::new().set_modified(modified)).unwrap();
        query.refresh = true;
        assert_eq!(list(directory.path(), &[], &query).unwrap().rows[0].provider, "custom");
        assert!(list(&directory.path().join("missing"), &[], &query).unwrap().rows.is_empty());
    }
    #[test]
    #[ignore = "writes comparative timing evidence"]
    fn compare_full_list_and_paged_index() {
        let directory = tempfile::tempdir().unwrap();
        for index in 0..2000 { fixture(directory.path(), index); }
        let query = PageQuery { source: "all".into(), ..Default::default() };
        let start = Instant::now();
        let full = super::super::ops::list_at(directory.path(), &[]);
        let full_ms = start.elapsed().as_secs_f64() * 1000.0;
        let start = Instant::now();
        let cold = list(directory.path(), &[], &query).unwrap();
        let cold_ms = start.elapsed().as_secs_f64() * 1000.0;
        let start = Instant::now();
        let warm = list(directory.path(), &[], &query).unwrap();
        let warm_ms = start.elapsed().as_secs_f64() * 1000.0;
        INDEXES.get().unwrap().lock().unwrap().remove(&directory.path().canonicalize().unwrap());
        let start = Instant::now();
        let reopened = list(directory.path(), &[], &query).unwrap();
        let reopened_ms = start.elapsed().as_secs_f64() * 1000.0;
        println!("{}", serde_json::json!({"sessions":2000,"full_ms":full_ms,"cold_ms":cold_ms,"warm_ms":warm_ms,"reopened_ms":reopened_ms,"full_bytes":serde_json::to_vec(&full).unwrap().len(),"page_bytes":serde_json::to_vec(&warm).unwrap().len()}));
        assert_eq!(cold.rows.len(), 50);
        assert_eq!(warm.rows.len(), 50);
        assert_eq!(reopened.rows.len(), 50);
    }
}
