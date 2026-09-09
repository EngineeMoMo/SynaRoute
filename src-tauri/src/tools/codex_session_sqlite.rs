//! Codex 会话 SQLite 的写前备份、provider 同步与对称还原。
//! 从父模块抽出只为守住 900 行门；语义与判据仍由父模块测试段覆盖。

use super::{catalog, files, ManifestEntry, SqliteOutcome};
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

/// 把指定 thread 的 `threads.model_provider` 改成 `target`。
///
/// `ids` = **rollout 那半确实改成功的**那些 thread id。空切片 → 一个字节都不写：那意味着
/// 路由压根没被修好，此时改列表元数据只会制造一个替未完成修复背书的假现场。
pub(super) fn sync_sqlite(
    home: &Path,
    data_dir: &Path,
    target: &str,
    ids: &[String],
) -> Option<SqliteOutcome> {
    let dbs = session_db_paths(home);
    if dbs.is_empty() {
        return None;
    }
    if ids.is_empty() {
        return Some(SqliteOutcome::default());
    }
    // Desktop 列表那份索引（`local_thread_catalog`）要的展示信息 —— 它在**另一个库**里，
    // 而那个库没有 `threads` 表，所以数据只能从这一侧带过去。取不到就只做 UPDATE 那两步。
    let catalog_rows = catalog::plan_from(home, target, ids);
    let mut out = SqliteOutcome::default();
    for db in dbs {
        // 备份在**写之前**、且只在真要写的时候做（`ids` 非空已经保证了这一点）。
        if let Err(e) = backup_db(&db, data_dir) {
            out.error.get_or_insert(e);
            continue; // 备份不成就不写这个库 —— 宁可不同步，也不留一个无法回退的改动
        }
        match set_threads_provider(&db, target, ids) {
            Ok(n) => out.updated += n,
            Err(e) => {
                out.error.get_or_insert(e);
            }
        }
        // 第三份 provider 副本 + Desktop 列表索引的缺行。理由见 `catalog` 模块头。
        catalog::repair_one(&db, target, &catalog_rows, &mut out.catalog);
    }
    Some(out)
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

/// 写之前把库文件备份到 `<data_dir>/backups/codex-sqlite/`，只留最近 [`DB_BACKUP_KEEP`] 份。
///
/// 🔴 **`-wal` 也要备份**：WAL 模式下主文件可能不含最新数据，只拷主文件会得到一个旧快照。
/// 缺 `-wal` 不是错误（Codex 关闭时会 checkpoint 掉它）。
fn backup_db(db: &Path, data_dir: &Path) -> Result<(), String> {
    let dir = data_dir.join("backups").join("codex-sqlite");
    fs::create_dir_all(&dir).map_err(|e| format!("建备份目录失败: {e}"))?;
    let stem = db.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    // 🔴 秒级时间戳不够：用户连点两下「启动」时两次备份会同名、**后一次覆盖前一次**，
    // 于是「保留 3 份」实际只有 1 份。加毫秒 + 进程内自增序号（同 `tmp_path_for`）。
    let ts = format!(
        "{}-{:04}",
        chrono::Utc::now().format("%Y%m%dT%H%M%S%.3f"),
        files::seq() % 10_000
    );
    fs::copy(db, dir.join(format!("{stem}.{ts}.bak")))
        .map_err(|e| format!("备份 {} 失败: {e}", db.display()))?;
    let wal = PathBuf::from(format!("{}-wal", db.to_string_lossy()));
    if wal.is_file() {
        fs::copy(&wal, dir.join(format!("{stem}-wal.{ts}.bak")))
            .map_err(|e| format!("备份 {} 失败: {e}", wal.display()))?;
    }
    prune_backups(&dir, &stem);
    Ok(())
}

/// 每个库名只留最近 [`DB_BACKUP_KEEP`] 份（同 `log_rotate` 那条：加了保留就必须同时加清理）。
/// 排序键刻意把 `-wal` 挪到末位 —— 取证见 `tests::old_db_backups_are_pruned` 的文档。
fn prune_backups(dir: &Path, stem: &str) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    let name = |p: &Path| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut mine: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| name(p).starts_with(stem))
        .collect();
    // 键 = (时间戳段, is_wal)：时间戳格式定长故字典序即时间序（不依赖 mtime —— 备份刚写完
    // 可能同秒，而排错方向会删掉最新那份）；is_wal 排在后面让同一轮的两个文件相邻、主文件在前。
    mine.sort_by_key(|p| {
        let n = name(p);
        let wal = n.starts_with(&format!("{stem}-wal."));
        (n.trim_start_matches(stem).trim_start_matches("-wal").to_string(), wal)
    });
    let keep_from = mine.len().saturating_sub(DB_BACKUP_KEEP * 2); // 主文件 + wal
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


