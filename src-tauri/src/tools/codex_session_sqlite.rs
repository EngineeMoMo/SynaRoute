//! Codex 会话 SQLite 的写前备份、provider 同步与对称还原。
//! 从父模块抽出只为守住 900 行门；语义与判据仍由父模块测试段覆盖。

use super::{catalog, files, ManifestEntry, SqliteOutcome};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
// sqlite（best-effort：只影响 Desktop 的会话列表，不影响路由）

/// 每个 sqlite 库保留几份写前备份。
pub(super) const DB_BACKUP_KEEP: usize = 3;
/// 每批绑定参数数；SQLite 3.32 之前上限 999。
///
/// 可见性刻意宽于 `pub(super)`：兄弟模块 `ops` / `catalog` 的**生产**代码也按它分批
/// （父模块重导出这一份，故只有一个事实来源）。
pub(in crate::tools) const SQL_CHUNK: usize = 500;
const _: () = assert!(SQL_CHUNK < 999, "旧版 SQLite 绑定参数上限就是 999");

/// 候选库路径：`<sqlite 根>/sqlite/*.db` 优先，回落 `<sqlite 根>/state_5.sqlite`。
///
/// 🔴 **两个都要试**。Codex 正在从单文件迁到 `sqlite/` 目录（本机同时存在两者）。
/// 只改 legacy 那份的表现是：在已迁移的机器上**静默无效** —— 列表照旧显示旧 provider，
/// 而我们报「已同步」。
///
/// 🔴 **sqlite 根不一定等于 `$CODEX_HOME`**：Codex 认 `CODEX_SQLITE_HOME`
/// （二进制里那句 "`CODEX_SQLITE_HOME` is overridden by an exact requirement for
/// sqlite_home"，出自 `core/src/config/requirements.rs`）。不认它的表现同样是静默的 ——
/// 我们对着 `$CODEX_HOME` 下一个陈旧的库写，而 Codex 读的是别处那个。
/// 目录必须真的存在才采纳，否则一个写错的环境变量会让我们连 legacy 库都找不到。
///
/// 判据做成**纯函数**（环境变量的值当入参传进来）：直接在测试里 `set_var` 会污染同进程
/// 并行跑的其它用例 —— 本仓在进程级状态上栽过好几次（`quota_window` 的表、
/// `DENIED_TOTAL` 的计数）。`session_db_paths` 必须经它，有源码级判据钉着。
pub(super) fn sqlite_root_of(env: Option<std::ffi::OsString>, home: &Path) -> PathBuf {
    match env {
        Some(v) if !v.is_empty() && Path::new(&v).is_dir() => PathBuf::from(v),
        _ => home.to_path_buf(),
    }
}

fn sqlite_root(home: &Path) -> PathBuf {
    sqlite_root_of(std::env::var_os("CODEX_SQLITE_HOME"), home)
}

pub(in crate::tools) fn session_db_paths(home: &Path) -> Vec<PathBuf> {
    let root = sqlite_root(home);
    let mut out = Vec::new();
    if let Ok(entries) = fs::read_dir(root.join("sqlite")) {
        for e in entries.flatten() {
            let p = e.path();
            let ext = p.extension().and_then(|s| s.to_str()).unwrap_or_default();
            if p.is_file() && matches!(ext, "db" | "sqlite" | "sqlite3") {
                out.push(p);
            }
        }
        out.sort();
    }
    let legacy = root.join("state_5.sqlite");
    if legacy.is_file() {
        out.push(legacy);
    }
    out
}

pub(super) fn collect_child_thread_ids(home: &Path) -> std::collections::HashSet<String> {
    let mut ids = std::collections::HashSet::new();
    for path in session_db_paths(home) {
        let Ok(conn) = rusqlite::Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) else {
            continue;
        };
        for (table, column) in [
            ("thread_spawn_edges", "child_thread_id"),
            ("agent_job_items", "assigned_thread_id"),
        ] {
            let has_column = conn.query_row(
                "SELECT 1 FROM pragma_table_info(?1) WHERE name=?2 LIMIT 1",
                (table, column), |_| Ok(true),
            ).unwrap_or(false);
            if !has_column { continue }
            let sql = format!("SELECT {column} FROM {table} WHERE COALESCE({column},'') <> ''");
            if let Ok(mut stmt) = conn.prepare(&sql) {
                if let Ok(rows) = stmt.query_map([], |row| row.get::<_, String>(0)) {
                    ids.extend(rows.flatten());
                }
            }
        }
    }
    ids
}

/// 把指定 thread 的 `threads.model_provider` 改成 `target`。
///
/// `ids` = **rollout 那半确实改成功的**那些 thread id。空切片 → 一个字节都不写：那意味着
/// 路由压根没被修好，此时改列表元数据只会制造一个替未完成修复背书的假现场。
pub(super) fn sync_sqlite(
    home: &Path,
    data_dir: &Path,
    target: &str,
    ids: &[String],
    catalog_rows: &[catalog::CatalogRow],
) -> Option<SqliteOutcome> {
    let dbs = session_db_paths(home);
    if dbs.is_empty() {
        return None;
    }
    // cleanup-only 必须运行：已有 guardian 污染行常发生在 rollout 全都已是 target 时。
    let planned_rows = if catalog_rows.is_empty() {
        catalog::plan_from(home, target, ids)
    } else {
        catalog_rows.to_vec()
    };
    let mut out = SqliteOutcome::default();
    for db in dbs {
        let thread_mutation = !ids.is_empty() && threads_would_mutate(&db, target, ids);
        let catalog_mutation = match catalog::would_mutate(&db, target, &planned_rows) {
            Ok(value) => value,
            Err(e) => {
                out.error.get_or_insert(e);
                continue;
            }
        };
        if !thread_mutation && !catalog_mutation {
            continue;
        }
        if let Err(e) = backup_db(&db, data_dir) {
            out.error.get_or_insert(e);
            continue;
        }
        if thread_mutation {
            match set_threads_provider(&db, target, ids) {
                Ok(n) => out.updated += n,
                Err(e) => { out.error.get_or_insert(e); }
            }
        }
        if catalog_mutation {
            catalog::repair_one(&db, target, &planned_rows, &mut out.catalog);
        }
    }
    Some(out)
}

fn threads_would_mutate(db: &Path, target: &str, ids: &[String]) -> bool {
    if ids.is_empty() { return false }
    let Ok(conn) = rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) else {
        return true;
    };
    ids.chunks(SQL_CHUNK).any(|chunk| {
        let holes = std::iter::repeat("?").take(chunk.len()).collect::<Vec<_>>().join(",");
        let target_pos = chunk.len() + 1;
        let sql = format!(
            "SELECT 1 FROM threads WHERE id IN ({holes}) AND model_provider IS NOT ?{target_pos} LIMIT 1"
        );
        let mut params: Vec<&dyn rusqlite::ToSql> = chunk.iter()
            .map(|id| id as &dyn rusqlite::ToSql).collect();
        params.push(&target);
        conn.query_row(&sql, params.as_slice(), |_| Ok(())).is_ok()
    })
}

/// 按清单把 `threads.model_provider` 改回原值。返回 `Some(错误)` 表示有库没处理成功。
///
/// 原值为空串的条目（rollout 里原本没那个字段，或清单是本字段上线前写的）**跳过** ——
/// 我们不知道 sqlite 里当时是什么，猜一个写进去比留着旧值更糟。
///
/// 🔴 **按原值分组、一组一次连接**：清单可能有上百条，而 [`set_threads_provider`] 每次都
/// 要 open 一次库并可能等满 `busy_timeout` —— 逐条来在库被锁时最坏是「条目数 × 1.5 秒」，
/// 那会让「停止代理」这个动作卡上几分钟。
pub(super) fn restore_sqlite(home: &Path, entries: &[ManifestEntry]) -> Option<String> {
    let dbs = session_db_paths(home);
    if dbs.is_empty() {
        return None;
    }
    let mut groups: std::collections::HashMap<&str, Vec<String>> =
        std::collections::HashMap::new();
    for e in entries {
        if e.thread_id.is_empty() || e.original_provider.is_empty() {
            continue;
        }
        groups
            .entry(e.original_provider.as_str())
            .or_default()
            .push(e.thread_id.clone());
    }
    if groups.is_empty() {
        return None;
    }
    let mut first_err = None;
    for db in dbs {
        for (provider, ids) in &groups {
            if let Err(err) = set_threads_provider(&db, provider, ids) {
                first_err.get_or_insert(err);
                break; // 同一个库接着试也是同样的错（多半是锁），换下一个库
            }
        }
    }
    first_err
}

fn backup_identity(db: &Path) -> Result<String, String> {
    let canonical = fs::canonicalize(db).map_err(|e| format!("解析 {} 失败: {e}", db.display()))?;
    let hash = format!("{:x}", Sha256::digest(canonical.to_string_lossy().as_bytes()));
    let stem = db.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    Ok(format!("{stem}-{}", &hash[..12]))
}

fn online_backup(db: &Path, destination: &Path) -> Result<(), String> {
    let source = rusqlite::Connection::open_with_flags(
        db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ).map_err(|e| format!("打开 {} 备份源失败: {e}", db.display()))?;
    let _ = source.busy_timeout(std::time::Duration::from_millis(1500));
    let mut output = rusqlite::Connection::open(destination)
        .map_err(|e| format!("创建 {} 失败: {e}", destination.display()))?;
    let backup = rusqlite::backup::Backup::new(&source, &mut output)
        .map_err(|e| format!("初始化 {} 备份失败: {e}", db.display()))?;
    use rusqlite::backup::StepResult;
    let mut waits = 0;
    loop {
        match backup.step(128).map_err(|e| format!("备份 {} 失败: {e}", db.display()))? {
            StepResult::Done => break,
            StepResult::More => waits = 0,
            StepResult::Busy | StepResult::Locked if waits < 15 => {
                waits += 1;
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            StepResult::Busy | StepResult::Locked => return Err(format!(
                "备份 {} 超时（数据库持续 Busy/Locked）", db.display()
            )),
            _ => return Err(format!("备份 {} 返回未知状态", db.display())),
        }
    }
    Ok(())
}

/// 写前用 SQLite Online Backup 生成单文件一致快照；它会把 WAL 中已提交页一并纳入。
pub(super) fn backup_db(db: &Path, data_dir: &Path) -> Result<(), String> {
    let dir = data_dir.join("backups").join("codex-sqlite");
    fs::create_dir_all(&dir).map_err(|e| format!("建备份目录失败: {e}"))?;
    let identity = backup_identity(db)?;
    let ts = format!("{}-{:04}", chrono::Utc::now().format("%Y%m%dT%H%M%S%.3f"), files::seq() % 10_000);
    let final_path = dir.join(format!("{identity}.{ts}.bak"));
    let temp_path = files::tmp_path_for(&final_path);
    if let Err(error) = online_backup(db, &temp_path) {
        let _ = fs::remove_file(&temp_path);
        return Err(error);
    }
    fs::rename(&temp_path, &final_path).map_err(|e| {
        let _ = fs::remove_file(&temp_path);
        format!("提交 {} 备份失败: {e}", db.display())
    })?;
    prune_backups(&dir, &identity);
    Ok(())
}

/// 每个 canonical 库身份只留最近 [`DB_BACKUP_KEEP`] 份一致快照。
fn prune_backups(dir: &Path, identity: &str) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    let name = |p: &Path| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut mine: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| name(p).starts_with(&format!("{identity}.")))
        .collect();
    mine.sort_by_key(|p| name(p));
    let keep_from = mine.len().saturating_sub(DB_BACKUP_KEEP);
    for p in &mine[..keep_from] {
        let _ = fs::remove_file(p);
    }
}

pub(super) fn set_threads_provider(db: &Path, provider: &str, ids: &[String]) -> Result<usize, String> {
    let conn = rusqlite::Connection::open(db).map_err(|e| format!("{}: {e}", db.display()))?;
    // Codex 在跑时 WAL 是锁着的 —— 等一小会儿而不是立刻放弃：多数锁只持有毫秒级，
    // 而这里的失败代价是「列表元数据不同步」，值得为它等一下。
    let _ = conn.busy_timeout(std::time::Duration::from_millis(1500));
    // 缺表/缺列 → 当作「这个库不管这件事」，不报错。Codex 的 schema 已经改过多次
    // （`state_5` 这个数字本身就是版本号），把它当错误会让接入在一件无关的事上失败。
    let has_col: bool = conn
        .query_row(
            "SELECT 1 FROM pragma_table_info('threads') WHERE name = 'model_provider' LIMIT 1",
            [],
            |_| Ok(true),
        )
        .unwrap_or(false);
    if !has_col {
        return Ok(0);
    }
    // 🔴 **必须分批**：每个 id 是一个绑定参数，而 SQLite 的 `SQLITE_MAX_VARIABLE_NUMBER`
    // 是 32766（3.32 之前只有 999）。重度用户的会话数到那个量级时整条 UPDATE 会报
    // `too many SQL variables` —— 而那是「安静失败」（路由不受影响，只有列表不同步）。
    let mut total = 0usize;
    for chunk in ids.chunks(SQL_CHUNK) {
        // `repeat_n` 要 Rust 1.82，而本仓 MSRV 是 1.77（clippy 的 incompatible_msrv 会拦）。
        let holes = std::iter::repeat("?").take(chunk.len()).collect::<Vec<_>>().join(",");
        let sql = format!(
            "UPDATE threads SET model_provider = ?1 \
             WHERE id IN ({holes}) AND model_provider IS NOT ?1"
        );
        let mut params: Vec<&dyn rusqlite::ToSql> = vec![&provider];
        for id in chunk {
            params.push(id);
        }
        total += conn
            .execute(&sql, params.as_slice())
            .map_err(|e| format!("{}: {e}", db.display()))?;
    }
    Ok(total)
}


