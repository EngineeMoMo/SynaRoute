//! Codex 的 `local_thread_catalog`（Desktop 会话列表的**索引表**）—— 第三份 provider 副本。
//!
//! # 🔴 这是审查这一轮才发现的一处真缺口
//!
//! provider 在磁盘上有**三份**，而我们此前只写两份：
//!
//! | 位置 | 谁的事实来源 | 我们此前写吗 |
//! |---|---|---|
//! | rollout 首行 `session_meta.payload.model_provider` | **路由**（`thread/resume` 读它） | ✅ |
//! | `state_5.sqlite` 的 `threads.model_provider` | 列表 | ✅ |
//! | `local_thread_catalog.model_provider` | **Desktop 列表** | ❌ |
//!
//! 本机实测（2026-09-06，codex-cli 0.151.0-alpha.7.2）：`local_thread_catalog` **不在**
//! `state_5.sqlite` 里，而在**新布局**的 `sqlite/codex-dev.db` 里；而那个库里**没有**
//! `threads` 表。也就是说两张表在两个不同的库里 —— 只按「有 `threads` 表的库」去写，
//! 这一份永远不会被碰到。
//!
//! 好消息是它**不影响路由**（`thread/resume` 只读 rollout，见父模块模块头），所以此前的
//! 401 修复是完整的。它影响的是「Desktop 的会话列表里那条对话属于哪个 provider」。
//!
//! # 三件事，风险递增，都只在列已存在时做
//!
//! 1. **同步 `model_provider`**（UPDATE）—— 补上上面那张表里缺的第三份；
//! 2. **清 `missing_candidate`**（UPDATE）—— 那一位是 Codex 给「找不到对应 rollout 的条目」
//!    打的标记。我们手上恰好有「磁盘上真的存在」这个事实，所以能确定地清掉它。
//!    CodexPlusPlus 也做同一件事（`SET missing_candidate = 0`）；
//! 3. **精确清掉已确认的内部索引行**（DELETE 只按 local `host_id` + `thread_id`）—— rollout、
//!    `threads`、edge/job 证据表、正文与全局状态一律不碰；冲突与未知证据一律保留。
//!
//! # 🔴 补缺行与清理为什么要格外小心（以及我们收窄了哪些）
//!
//! 这张表带着 Codex 自己的账：`observation_sequence`（每条一个递增序号）、
//! `local_thread_catalog_metadata.catalog_revision`、`local_thread_catalog_sync_state`
//! 的水位线。写错这些不是「少一行」，而可能让 **Codex 自己的扫描器**判断错。故：
//!
//! - **列必须齐**才动手（[`supports_repair`]），少一列就整个跳过 —— schema 改过多次，
//!   而这里宁可不做；
//! - `host_id` 从 `local_thread_catalog_hosts` 里取真实值，取不到就**不做**
//!   （不硬编码 `"local"`：那是猜，而猜错会插出一条 Codex 认不出的行）；
//! - **刻意不写 `local_thread_catalog_sync_state`**。CodexPlusPlus 在「全量同步」时会推进
//!   那张表的水位线（`update_full_sync_state`），我们只做定点补行/清理，不声称完成全量扫描；
//! - 删除必须有正向 child 证据且无 user/fork 冲突；catalog 自身的证据只作用于同一个 DB 路径；
//! - 每个实际 mutation 的库先由父模块做一致 SQLite 快照，备份失败则该库一个字节都不写。
//!
//! # 🔴 「修它就能让会话回到列表」这句话我们没有取证
//!
//! `has_user_event` 在 codex.exe 里只出现在 DDL 与 migration 文本里，**没有**证据表明它
//! 门控列表可见性；CodexPlusPlus 的 README 也只说「切 provider 后旧会话从列表里消失」。
//! 故用户可见文案只说**做了什么**（「补了 N 条索引记录」），不承诺「会话会回来」——
//! 同父模块那条「只说已指向 SynaRoute，不说已恢复可用」。

use super::files;
use rusqlite::types::Value as SqlValue;
use rusqlite::Connection;
use std::collections::HashSet;
use std::path::Path;

/// 一次修复的账。
#[derive(Debug, Default, PartialEq, Eq)]
pub(in crate::tools) struct CatalogReport {
    /// `model_provider` 被改对的行数。
    pub provider_updated: usize,
    /// `missing_candidate` 被清掉的行数。
    pub unmarked_missing: usize,
    /// 补进去的缺行数。
    pub inserted: usize,
    /// 从 local host 的 Desktop 索引精确移除的已确认内部记录数。
    pub removed: usize,
    /// 逐库失败原因（被 WAL 锁住、列不齐等），如实上报、不阻断。
    pub skipped: Vec<String>,
}

impl CatalogReport {
    /// 三项之和。测试段用它做「一个字节都没写」的整体断言。
    #[cfg(test)]
    pub fn total(&self) -> usize {
        self.provider_updated + self.unmarked_missing + self.inserted + self.removed
    }
}

/// 补一行需要的那几列。少任何一列就整个跳过。
fn supports_repair(cols: &HashSet<String>) -> bool {
    [
        "host_id",
        "thread_id",
        "display_title",
        "source_created_at",
        "source_updated_at",
        "cwd",
        "source_kind",
        "model_provider",
        "observation_sequence",
    ]
    .iter()
    .all(|c| cols.contains(*c))
}

fn columns_of(db: &Connection, table: &str) -> HashSet<String> {
    db.prepare(&format!("PRAGMA table_info('{table}')"))
        .and_then(|mut s| {
            s.query_map([], |r| r.get::<_, String>(1))?.collect::<rusqlite::Result<_>>()
        })
        .unwrap_or_default()
}

/// 真实的 `host_id`。**取不到就返回 `None` 表示别动手** —— 不硬编码 `"local"`。
fn host_id(db: &Connection) -> Option<String> {
    let cols = columns_of(db, "local_thread_catalog_hosts");
    if !cols.contains("host_id") {
        return None;
    }
    let sql = if cols.contains("host_kind") {
        "SELECT host_id FROM local_thread_catalog_hosts \
         WHERE LOWER(COALESCE(host_kind,'')) = 'local' ORDER BY host_id LIMIT 1"
    } else {
        "SELECT host_id FROM local_thread_catalog_hosts WHERE host_id = 'local' LIMIT 1"
    };
    db.query_row(sql, [], |r| r.get::<_, String>(0))
        .ok()
        .filter(|h| !h.trim().is_empty())
}

fn catalog_user_ids(db: &Connection, host: &str, cols: &HashSet<String>) -> HashSet<String> {
    if !cols.contains("thread_id") || !cols.contains("thread_source") {
        return HashSet::new();
    }
    db.prepare(
        "SELECT thread_id FROM local_thread_catalog WHERE host_id = ?1 \
         AND LOWER(TRIM(COALESCE(thread_source, ''))) = 'user'",
    )
    .and_then(|mut stmt| {
        stmt.query_map([host], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<_>>()
    })
    .unwrap_or_default()
}

#[derive(Debug, Clone)]
pub(in crate::tools) struct CatalogRow {
    pub thread_id: String,
    pub title: String,
    pub cwd: String,
    /// Unix 秒（catalog 那几列是 `REAL`）。
    pub created_at: f64,
    pub updated_at: f64,
    /// `threads.source`（本机实测是 `"vscode"`）。取不到就不补这一行 —— 见 [`plan_from`]。
    pub source_kind: String,
    pub provider: String,
    /// `threads.thread_source`：`user` = 用户自己的对话，其它值（本机实测
    /// `guardian_review`）是 Codex 内部派生的子代理线程。**只有 `user` 才补进 Desktop 列表**
    /// —— 理由见 [`repair_one`] 里那道门。
    pub thread_source: String,
    /// 文件名/首行已确认是 fork；即使 source 像内部线程也不得清理。
    pub forked: bool,
}

/// 标题的 SQL 表达式：按 catalog 自己的优先级取（`name` 是 Codex 生成的短标题，
/// 见 `view` 模块头）。抽成函数是为了让判据能直接对着生成的 SQL 断言 ——
/// grep 源码里有没有 `NULLIF` 只要一处就过，而这里要的是「每个候选列都套上了」。
///
/// 🔴 **每列都要套 `NULLIF(c,'')`**：`COALESCE` 只跳过 NULL，而 `name` 完全可能是
/// **空串**（Codex 异步生成标题，没生成时就是 `''`）。裸 `COALESCE` 那时返回空串 →
/// 我们往 Desktop 侧栏补一条**没有标题的空行**，用户看到一条点不出名字的记录。
/// `view::thread_info` 一直是带 NULLIF 的，`plan_from` 这一处漏了 —— 同一件事两份实现漂移。
fn title_expr_for(cols: &HashSet<String>) -> String {
    let inner = ["name", "title", "preview", "first_user_message"]
        .iter()
        .filter(|c| cols.contains(**c))
        .map(|c| format!("NULLIF({c}, '')"))
        .collect::<Vec<_>>()
        .join(", ");
    if inner.is_empty() {
        "id".into()
    } else {
        format!("COALESCE({inner}, id)")
    }
}

/// 从各库的 `threads` 表凑出「要补进 catalog 的那些行」。
///
/// 🔴 **必须跨库收集**：本机实测 `local_thread_catalog` 在 `sqlite/codex-dev.db`、而
/// `threads` 在 `state_5.sqlite` —— 两张表在**两个不同的库**里。只在同一个连接里找，
/// 这一步永远拿不到数据（而失效是静默的：一条都补不了，报告说「0 条」）。
///
/// `ids` 是 rollout 那半确实改成功的那些 thread id；只为它们凑行 —— 同 `sync_sqlite` 的口径。
///
/// **`source_kind` 取不到就不补那一行**：它是 NOT NULL，而随便填一个（比如 `"synaroute"`）
/// 会插出一条 Codex 自己认不出来源的记录。宁可少补。
pub(in crate::tools) fn plan_from(home: &Path, target: &str, ids: &[String]) -> Vec<CatalogRow> {
    let want: HashSet<&str> = ids.iter().map(|s| s.as_str()).collect();
    let mut out: Vec<CatalogRow> = Vec::new();
    for db in super::session_db_paths(home) {
        let Ok(conn) = Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        else {
            continue;
        };
        let _ = conn.busy_timeout(std::time::Duration::from_millis(800));
        let cols = columns_of(&conn, "threads");
        if !cols.contains("id") || !cols.contains("source") {
            continue;
        }
        let title_expr = title_expr_for(&cols);
        // 时间戳两种形态都可能在：毫秒列优先（更精确），回落秒列。
        let ts = |ms: &str, s: &str| -> String {
            match (cols.contains(ms), cols.contains(s)) {
                (true, true) => format!("COALESCE({ms} / 1000.0, {s}, 0)"),
                (true, false) => format!("COALESCE({ms} / 1000.0, 0)"),
                (false, true) => format!("COALESCE({s}, 0)"),
                (false, false) => "0".into(),
            }
        };
        let src_expr = if cols.contains("thread_source") {
            "COALESCE(thread_source, '')"
        } else {
            "''"
        };
        let sql = format!(
            "SELECT id, {title_expr}, COALESCE(cwd, ''), {}, {}, COALESCE(source, ''), {src_expr} FROM threads",
            ts("created_at_ms", "created_at"),
            ts("updated_at_ms", "updated_at"),
        );
        let Ok(mut stmt) = conn.prepare(&sql) else { continue };
        let rows = stmt.query_map([], |r| {
            Ok(CatalogRow {
                thread_id: r.get(0)?,
                title: r.get(1)?,
                cwd: files::display_path(&r.get::<_, String>(2)?),
                created_at: r.get(3)?,
                updated_at: r.get(4)?,
                source_kind: r.get(5)?,
                provider: target.to_string(),
                thread_source: r.get(6)?,
                forked: false,
            })
        });
        if let Ok(iter) = rows {
            for row in iter.flatten() {
                if want.contains(row.thread_id.as_str())
                    && !out.iter().any(|x| x.thread_id == row.thread_id)
                {
                    out.push(row);
                }
            }
        }
    }
    out
}

fn thread_source_marks_child(source: &str) -> bool {
    matches!(source.trim().to_ascii_lowercase().as_str(),
        "guardian_review" | "subagent" | "memory_consolidation")
}

fn source_kind_marks_child(source: &str) -> bool {
    let source = source.trim();
    if matches!(source.to_ascii_lowercase().as_str(),
        "guardian_review" | "subagent" | "memory_consolidation") {
        return true;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(source) else { return false };
    let serde_json::Value::Object(object) = value else { return false };
    ["sub_agent", "subagent", "internal"].iter().any(|key| {
        object.get(*key).is_some_and(|value| match value {
            serde_json::Value::Null => false,
            serde_json::Value::Bool(flag) => *flag,
            serde_json::Value::String(text) => !text.trim().is_empty(),
            serde_json::Value::Array(items) => !items.is_empty(),
            serde_json::Value::Object(items) => !items.is_empty(),
            serde_json::Value::Number(_) => true,
        })
    })
}

fn row_has_user_veto(row: &CatalogRow) -> bool {
    row.forked
}

fn row_is_confirmed_child(row: &CatalogRow) -> bool {
    !row.forked
        && !row.thread_source.trim().eq_ignore_ascii_case("user")
        && (thread_source_marks_child(&row.thread_source)
            || source_kind_marks_child(&row.source_kind))
}

pub(in crate::tools) fn cleanup_rows(
    home: &Path,
    target: &str,
    ids: &[String],
    sessions: &[super::SessionRef],
    child_ids: &HashSet<String>,
) -> Vec<CatalogRow> {
    let mut rows = plan_from(home, target, ids);
    merge_rollout_evidence(&mut rows, sessions);
    add_child_ids(&mut rows, child_ids);
    rows.into_iter()
        .filter(|row| child_ids.contains(&row.thread_id) || row_is_confirmed_child(row))
        .collect()
}

pub(in crate::tools) fn add_child_ids(rows: &mut Vec<CatalogRow>, ids: &HashSet<String>) {
    for id in ids {
        if let Some(row) = rows.iter_mut().find(|row| &row.thread_id == id) {
            if !row.thread_source.trim().eq_ignore_ascii_case("user") {
                row.source_kind = "subagent".into();
            }
        } else {
            rows.push(CatalogRow {
                thread_id: id.clone(),
                title: String::new(),
                cwd: String::new(),
                created_at: 0.0,
                updated_at: 0.0,
                source_kind: "subagent".into(),
                provider: String::new(),
                thread_source: String::new(),
                forked: false,
            });
        }
    }
}

/// 先只读判定这个库是否真的需要 mutation；父模块据此决定是否创建备份。
pub(in crate::tools) fn would_mutate(
    db_path: &Path,
    target: &str,
    rows: &[CatalogRow],
) -> Result<bool, String> {
    let conn = Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| format!("{}: {e}", db_path.display()))?;
    let cols = columns_of(&conn, "local_thread_catalog");
    if cols.is_empty() { return Ok(false) }
    let Some(host) = host_id(&conn) else { return Ok(false) };
    for row in rows {
        let exists: bool = conn.query_row(
            "SELECT 1 FROM local_thread_catalog WHERE host_id=?1 AND thread_id=?2 LIMIT 1",
            (&host, &row.thread_id), |_| Ok(true),
        ).unwrap_or(false);
        if row_has_user_veto(row) { continue }
        if exists && row_is_confirmed_child(row) { return Ok(true) }
        if exists && ((cols.contains("model_provider") && row.provider != target)
            || cols.contains("missing_candidate")) {
            let changed: i64 = conn.query_row(
                "SELECT COUNT(*) FROM local_thread_catalog WHERE host_id=?1 AND thread_id=?2 AND \
                 (COALESCE(model_provider,'') <> ?3 OR COALESCE(missing_candidate,0) <> 0)",
                (&host, &row.thread_id, target), |r| r.get(0),
            ).unwrap_or(0);
            if changed > 0 { return Ok(true) }
        }
        if !exists && supports_repair(&cols) && !row.source_kind.is_empty() && worth_listing(row) {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(in crate::tools) fn merge_rollout_evidence(rows: &mut Vec<CatalogRow>, sessions: &[super::SessionRef]) {
    for session in sessions.iter().filter(|session| !session.thread_id.is_empty()) {
        let rollout = CatalogRow {
            thread_id: session.thread_id.clone(),
            title: session.title.clone(),
            cwd: session.cwd.clone(),
            created_at: 0.0,
            updated_at: 0.0,
            source_kind: String::new(),
            provider: session.provider.clone(),
            thread_source: session.thread_source.clone(),
            forked: session.forked,
        };
        if let Some(row) = rows.iter_mut().find(|row| row.thread_id == session.thread_id) {
            row.forked |= rollout.forked;
            let current_user = row.thread_source.trim().eq_ignore_ascii_case("user");
            let rollout_user = rollout.thread_source.trim().eq_ignore_ascii_case("user");
            if current_user || rollout_user {
                row.thread_source = "user".into();
            } else if row.thread_source.trim().is_empty()
                || thread_source_marks_child(&rollout.thread_source)
            {
                row.thread_source = rollout.thread_source;
            }
        } else {
            rows.push(rollout);
        }
    }
}

/// 对一个库做 provider/missing 更新、精确内部行清理与缺行补齐；结构改动与 revision 同事务。
pub(in crate::tools) fn repair_one(
    db_path: &Path,
    target: &str,
    rows: &[CatalogRow],
    report: &mut CatalogReport,
) {
    let Ok(mut conn) = Connection::open(db_path) else {
        report.skipped.push(format!("{}: 打不开", db_path.display()));
        return;
    };
    let _ = conn.busy_timeout(std::time::Duration::from_millis(1500));
    let cols = columns_of(&conn, "local_thread_catalog");
    if cols.is_empty() {
        // 这个库不管这件事（本机的 `state_5.sqlite` 就是这种）—— 不是错误。
        return;
    }
    let Some(host) = host_id(&conn) else { return };
    let catalog_users = catalog_user_ids(&conn, &host, &cols);

    // 只按 fork 否决**更新**（provider / missing 都是无损的）。刻意不把 `row_is_confirmed_child`
    // 也排除掉：那些行随后会被删掉，更新它们无害；而**被 catalog 自身 user 标记救下来**的
    // 冲突行必须照常同步 —— 排除它会让第三份 provider 副本在那些行上永远是旧值（写这条
    // 判据的用例当场抓到了这一点）。
    let mutable_ids: Vec<&str> = rows.iter().filter(|row| !row_has_user_veto(row))
        .map(|row| row.thread_id.as_str()).collect();

    // ① provider：只改 local host，避免同库里的远端 catalog 副本被连带改写。
    if cols.contains("model_provider") && cols.contains("thread_id") {
        for chunk in mutable_ids.chunks(super::SQL_CHUNK) {
            let holes = std::iter::repeat("?").take(chunk.len()).collect::<Vec<_>>().join(",");
            let sql = format!(
                "UPDATE local_thread_catalog SET model_provider = ?1 \
                 WHERE host_id = ?2 AND thread_id IN ({holes}) AND model_provider IS NOT ?1"
            );
            let mut params: Vec<&dyn rusqlite::ToSql> = vec![&target, &host];
            for id in chunk {
                params.push(id);
            }
            match conn.execute(&sql, params.as_slice()) {
                Ok(n) => report.provider_updated += n,
                Err(e) => report.skipped.push(format!("{}: provider 未同步（{e}）", db_path.display())),
            }
        }
    }

    // ② `missing_candidate`：我们手上有「rollout 真的在磁盘上」这个事实，所以能确定地清掉它。
    if cols.contains("missing_candidate") && cols.contains("thread_id") {
        for chunk in mutable_ids.chunks(super::SQL_CHUNK) {
            let holes = std::iter::repeat("?").take(chunk.len()).collect::<Vec<_>>().join(",");
            let sql = format!(
                "UPDATE local_thread_catalog SET missing_candidate = 0 \
                 WHERE host_id = ?1 AND thread_id IN ({holes}) AND COALESCE(missing_candidate, 0) <> 0"
            );
            let mut params: Vec<&dyn rusqlite::ToSql> = vec![&host];
            params.extend(chunk.iter().map(|s| s as &dyn rusqlite::ToSql));
            match conn.execute(&sql, params.as_slice()) {
                Ok(n) => report.unmarked_missing += n,
                Err(e) => report
                    .skipped
                    .push(format!("{}: missing 标记未清（{e}）", db_path.display())),
            }
        }
    }

    // ③ 删除与插入同属 structural changes，必须与 revision 在同一事务内提交/回滚。
    if !cols.contains("host_id") || !cols.contains("thread_id") { return }
    let existing: HashSet<String> = conn
        .prepare("SELECT thread_id FROM local_thread_catalog WHERE host_id = ?1")
        .and_then(|mut s| {
            s.query_map([&host], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<_>>()
        })
        .unwrap_or_default();
    let removable: Vec<&CatalogRow> = rows
        .iter()
        .filter(|row| {
            existing.contains(&row.thread_id)
                && !catalog_users.contains(&row.thread_id)
                && row_is_confirmed_child(row)
        })
        .collect();
    let missing: Vec<&CatalogRow> = if supports_repair(&cols) {
        rows.iter()
            .filter(|r| !existing.contains(&r.thread_id) && !r.source_kind.is_empty() && worth_listing(r))
            .collect()
    } else {
        Vec::new()
    };
    if missing.is_empty() && removable.is_empty() {
        return;
    }
    let mut seq: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(observation_sequence), 0) FROM local_thread_catalog WHERE host_id = ?1",
            [&host],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let insert_cols = insert_columns(&cols);
    let holes = std::iter::repeat("?").take(insert_cols.len()).collect::<Vec<_>>().join(", ");
    let sql = format!(
        "INSERT OR IGNORE INTO local_thread_catalog ({}) VALUES ({holes})",
        insert_cols.join(", ")
    );
    let tx = match conn.transaction() {
        Ok(t) => t,
        Err(e) => {
            report.skipped.push(format!("{}: 开事务失败（{e}）", db_path.display()));
            return;
        }
    };
    let mut inserted = 0usize;
    let mut removed = 0usize;
    for row in removable {
        match tx.execute(
            "DELETE FROM local_thread_catalog WHERE host_id = ?1 AND thread_id = ?2",
            (&host, &row.thread_id),
        ) {
            Ok(n) => removed += n,
            Err(e) => {
                report.skipped.push(format!("{}: 内部索引未清理（{e}）", db_path.display()));
                return;
            }
        }
    }
    for row in missing {
        seq += 1;
        let vals = insert_values(&insert_cols, &host, row, seq);
        match tx.execute(&sql, rusqlite::params_from_iter(vals)) {
            Ok(n) => inserted += n,
            Err(e) => {
                report.skipped.push(format!("{}: 补行失败（{e}）", db_path.display()));
                return;
            }
        }
    }
    let structural = inserted + removed;
    if structural > 0 {
        let metadata = columns_of(&tx, "local_thread_catalog_metadata");
        if !metadata.contains("catalog_revision") {
            report.skipped.push(format!("{}: 缺 catalog_revision，结构变更已回滚", db_path.display()));
            return;
        }
        match tx.execute(
            "UPDATE local_thread_catalog_metadata SET catalog_revision = catalog_revision + ?1",
            [structural as i64],
        ) {
            Ok(1) => {}
            Ok(n) => {
                report.skipped.push(format!("{}: 版本号异常影响 {n} 行，结构变更已回滚", db_path.display()));
                return;
            }
            Err(e) => {
                report.skipped.push(format!("{}: 版本号未推进（{e}），结构变更已回滚", db_path.display()));
                return;
            }
        }
    }
    match tx.commit() {
        Ok(()) => {
            report.inserted += inserted;
            report.removed += removed;
        }
        Err(e) => report.skipped.push(format!("{}: 提交失败（{e}）", db_path.display())),
    }
}

/// 这一条值得出现在 **Desktop 的会话列表**里吗。
///
/// 🔴 **用户 2026-09-10 实报的缺陷本体**：Codex Desktop 的项目侧栏被十几条一模一样的
/// 「The following is the Codex agent history whose request action you are as…」刷满，
/// 真正的对话被挤在中间认不出来。那些是 Codex 自己派生的 `guardian_review` 子代理线程，
/// 而它们的 `threads.name` 是 **NULL** → `plan_from` 的 COALESCE 回落到 `title`，
/// 那一列存的就是那段注入文本。
///
/// **Codex 自己刻意不把它们放进 catalog。** 本机取证（2026-09-10）：`threads` 有 3 行、
/// 其中 1 行是 guardian，而 `local_thread_catalog` 只有 **2 行**（两条 `user`）——
/// 且 `local_thread_catalog_sync_state.initial_build_complete = 1`、
/// `observation_sequence` 已到 37，也就是它**扫过了、然后决定不收**。
/// 我们去补它等于把 Codex 刻意排除的东西塞回用户眼前。
///
/// 删除只认正向 child 证据（已知 thread_source / truthy structured source / edge/job），
/// 明确 user、fork 或任何冲突都保留。标题只用于防止旧 schema 的未知来源被**插入**，绝不作为 DELETE 证据。
fn worth_listing(r: &CatalogRow) -> bool {
    if row_is_confirmed_child(r) || r.forked {
        return false;
    }
    !super::view::is_injected_prompt(&r.title)
}

/// 要插哪些列：必填的九列 + 库里恰好有的那几个可选列。
fn insert_columns(cols: &HashSet<String>) -> Vec<&'static str> {
    let mut names = vec![
        "host_id",
        "thread_id",
        "display_title",
        "source_created_at",
        "source_updated_at",
        "cwd",
        "source_kind",
        "model_provider",
        "observation_sequence",
    ];
    // `source_recency_at` 是 NOT NULL 且有默认值 0；带上它列表排序才对得上。
    for opt in ["missing_candidate", "source_recency_at", "pending_observed_title", "thread_source"] {
        if cols.contains(opt) {
            names.push(opt);
        }
    }
    names
}

fn insert_values(cols: &[&str], host: &str, r: &CatalogRow, seq: i64) -> Vec<SqlValue> {
    cols.iter()
        .map(|c| match *c {
            "host_id" => SqlValue::Text(host.to_string()),
            "thread_id" => SqlValue::Text(r.thread_id.clone()),
            "display_title" => SqlValue::Text(r.title.clone()),
            "source_created_at" => SqlValue::Real(r.created_at),
            "source_updated_at" => SqlValue::Real(r.updated_at),
            "source_recency_at" => SqlValue::Real(r.updated_at),
            "cwd" => SqlValue::Text(r.cwd.clone()),
            "source_kind" => SqlValue::Text(r.source_kind.clone()),
            "model_provider" => SqlValue::Text(r.provider.clone()),
            "thread_source" => SqlValue::Text(r.thread_source.clone()),
            "observation_sequence" => SqlValue::Integer(seq),
            // 我们补的行**不是**「观察到的标题待定」，标题是现成的。
            "missing_candidate" | "pending_observed_title" => SqlValue::Integer(0),
            _ => SqlValue::Null,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "sr-cat-{tag}-{}-{}",
            std::process::id(),
            files::seq()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// 造一个「新布局」的 catalog 库：本机实测那 17 列（含 `host_id` / `observation_sequence`
    /// / `missing_candidate` / `source_recency_at`）。
    fn catalog_db(dir: &Path) -> std::path::PathBuf {
        let p = dir.join("codex-dev.db");
        let c = Connection::open(&p).unwrap();
        c.execute_batch(
            "CREATE TABLE local_thread_catalog (
                host_id TEXT NOT NULL, thread_id TEXT NOT NULL, display_title TEXT NOT NULL,
                source_created_at REAL NOT NULL, source_updated_at REAL NOT NULL, cwd TEXT,
                source_kind TEXT NOT NULL, source_detail TEXT, model_provider TEXT,
                git_branch TEXT, observation_sequence INTEGER NOT NULL,
                missing_candidate INTEGER NOT NULL DEFAULT 0, thread_source TEXT,
                source_recency_at REAL NOT NULL DEFAULT 0,
                pending_observed_title INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (host_id, thread_id));
             CREATE TABLE local_thread_catalog_hosts (host_id TEXT, host_kind TEXT NOT NULL);
             INSERT INTO local_thread_catalog_hosts VALUES ('local', 'local');
             CREATE TABLE local_thread_catalog_metadata (id INTEGER, catalog_revision INTEGER NOT NULL DEFAULT 0);
             INSERT INTO local_thread_catalog_metadata VALUES (1, 7);",
        )
        .unwrap();
        p
    }

    fn row(id: &str) -> CatalogRow {
        CatalogRow {
            thread_id: id.into(),
            title: "实现 Java 快速排序".into(),
            cwd: "C:\\work".into(),
            created_at: 1788268413.0,
            updated_at: 1788270772.0,
            source_kind: "vscode".into(),
            provider: "synaroute".into(),
            thread_source: "user".into(),
            forked: false,
        }
    }

    /// 内部派生线程（本机实测的 `guardian_review`）：`thread_source` 非 user，
    /// 且标题就是 Codex 注入给子代理的那段英文。
    fn guardian_row(id: &str) -> CatalogRow {
        CatalogRow {
            thread_source: "guardian_review".into(),
            title: "The following is the Codex agent history whose request action you are asked to review"
                .into(),
            ..row(id)
        }
    }

    /// 🔴 **第三份 provider 副本必须一起改。**
    ///
    /// 本机实测 `local_thread_catalog` 在 `sqlite/codex-dev.db`、`threads` 在
    /// `state_5.sqlite` —— 只写「有 `threads` 表的库」时这一份永远是旧值，
    /// 而 Desktop 的会话列表读的就是它。
    #[test]
    fn the_third_provider_copy_is_synced_too() {
        let dir = tmp("prov");
        let db = catalog_db(&dir);
        let c = Connection::open(&db).unwrap();
        c.execute(
            "INSERT INTO local_thread_catalog (host_id,thread_id,display_title,source_created_at,
             source_updated_at,cwd,source_kind,model_provider,observation_sequence)
             VALUES ('local','t1','旧标题',1.0,2.0,'C:\\work','vscode','openai',3)",
            [],
        )
        .unwrap();
        drop(c);

        let mut rep = CatalogReport::default();
        repair_one(&db, "synaroute", &[row("t1")], &mut rep);
        assert_eq!(rep.provider_updated, 1, "那一份没被改对：{rep:?}");
        assert_eq!(rep.inserted, 0, "已经有这一行了，不该再插");

        let c = Connection::open(&db).unwrap();
        let got: String = c
            .query_row("SELECT model_provider FROM local_thread_catalog WHERE thread_id='t1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(got, "synaroute");
        // 幂等：再跑一次不该有任何改动（`IS NOT ?1` 那道条件）。
        let mut again = CatalogReport::default();
        repair_one(&db, "synaroute", &[row("t1")], &mut again);
        assert_eq!(again.total(), 0, "第二次不该再写任何东西：{again:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 🔴 **A8-6：catalog 行**自己**标着 `user` 时，一律不许删。**
    ///
    /// 与 `catalog_cleanup_obeys_user_fork_unknown_and_host_vetoes` 覆盖的**不是**同一条路：
    /// 那条走的是「rollout 说 user」（由 `merge_rollout_evidence` 把 `thread_source` 抬成
    /// user）。这里是另一半 —— **rollout 说内部、而 catalog 自己说 user**。
    ///
    /// 现实成因：rollout 已被用户删掉/归档、或 Codex 改过 rollout 的来源标记，而 Desktop
    /// 索引里那一行仍如实记着「这是用户的对话」。此时按 rollout 的证据删，删掉的是**用户
    /// 真实对话**在列表里的条目 —— 不可自愈（我们不重建 catalog 行），而用户只会看到
    /// 「我的对话从侧栏消失了」。
    ///
    /// 冲突时**一律保留**，与本模块「删除只认正向 child 证据、冲突与未知一律保留」同口径。
    #[test]
    fn a_catalog_row_marked_user_is_never_removed_even_if_the_plan_says_internal() {
        let dir = tmp("userveto");
        let db = catalog_db(&dir);
        let c = Connection::open(&db).unwrap();
        // catalog 自己记着 user；而下面传进去的计划行说它是 guardian_review。
        c.execute(
            "INSERT INTO local_thread_catalog (host_id,thread_id,display_title,source_created_at,
             source_updated_at,cwd,source_kind,model_provider,observation_sequence,thread_source)
             VALUES ('local','t1','用户自己的对话',1.0,2.0,'C:\\w','vscode','openai',3,'user')",
            [],
        )
        .unwrap();
        drop(c);

        let mut rep = CatalogReport::default();
        repair_one(&db, "synaroute", &[guardian_row("t1")], &mut rep);
        assert_eq!(rep.removed, 0, "catalog 说 user 就不许删：{rep:?}");

        let c = Connection::open(&db).unwrap();
        let left: i64 = c
            .query_row("SELECT COUNT(*) FROM local_thread_catalog WHERE thread_id='t1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(left, 1, "那一行必须还在");
        // 否决只针对**删除**：provider 这类无损更新照旧要做（否则第三份副本永远是旧值）。
        let provider: String = c
            .query_row("SELECT model_provider FROM local_thread_catalog WHERE thread_id='t1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(provider, "synaroute", "user 否决不该顺带掐掉 provider 同步");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `missing_candidate` 只在**我们确知 rollout 存在**时清。
    #[test]
    fn a_stale_missing_flag_is_cleared() {
        let dir = tmp("miss");
        let db = catalog_db(&dir);
        let c = Connection::open(&db).unwrap();
        c.execute(
            "INSERT INTO local_thread_catalog (host_id,thread_id,display_title,source_created_at,
             source_updated_at,cwd,source_kind,model_provider,observation_sequence,missing_candidate)
             VALUES ('local','t1','x',1.0,2.0,'C:\\w','vscode','synaroute',3,1)",
            [],
        )
        .unwrap();
        drop(c);
        let mut rep = CatalogReport::default();
        repair_one(&db, "synaroute", &[row("t1")], &mut rep);
        assert_eq!(rep.unmarked_missing, 1, "失效标记没清掉：{rep:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 补缺行：`observation_sequence` 必须**接着现有最大值往上排**，`catalog_revision` 要推进。
    ///
    /// 那两个数是 Codex 自己的账。序号重复会让它的扫描器把两条记录看成同一次观察；
    /// 不推 revision 则 Desktop 可能不刷新（补了行、界面看不到）。
    #[test]
    fn a_missing_row_is_inserted_with_the_next_observation_sequence() {
        let dir = tmp("ins");
        let db = catalog_db(&dir);
        let c = Connection::open(&db).unwrap();
        c.execute(
            "INSERT INTO local_thread_catalog (host_id,thread_id,display_title,source_created_at,
             source_updated_at,cwd,source_kind,model_provider,observation_sequence)
             VALUES ('local','old','x',1.0,2.0,'C:\\w','vscode','synaroute',25)",
            [],
        )
        .unwrap();
        drop(c);

        let mut rep = CatalogReport::default();
        repair_one(&db, "synaroute", &[row("old"), row("new1"), row("new2")], &mut rep);
        assert_eq!(rep.inserted, 2, "两条缺行都要补上：{rep:?}");

        let c = Connection::open(&db).unwrap();
        let seqs: Vec<i64> = c
            .prepare("SELECT observation_sequence FROM local_thread_catalog WHERE thread_id LIKE 'new%' ORDER BY observation_sequence")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(seqs, vec![26, 27], "序号必须接着 25 往上排，且逐条递增");
        let rev: i64 = c
            .query_row("SELECT catalog_revision FROM local_thread_catalog_metadata", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rev, 9, "补了 2 行，revision 要从 7 推到 9");
        let title: String = c
            .query_row("SELECT display_title FROM local_thread_catalog WHERE thread_id='new1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(title, "实现 Java 快速排序", "标题要带过去，否则列表里是一串 uuid");
        let inserted_source: String = c.query_row(
            "SELECT thread_source FROM local_thread_catalog WHERE thread_id='new1'", [], |r| r.get(0)
        ).unwrap();
        assert_eq!(inserted_source, "user", "catalog 有该列时必须写入来源");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// revision 失败必须回滚 destructive structural mutation，并且报告只计 committed 行。
    #[test]
    fn a_revision_failure_rolls_back_structural_changes() {
        let dir = tmp("revfail");
        // 复用标准夹具（列必须齐，否则 `supports_repair` 那道门会让我们一行都不补、
        // 压根走不到版本号那一段 —— 第一版自建 schema 就是这么失败的）。
        let db = catalog_db(&dir);
        let c = Connection::open(&db).unwrap();
        c.execute(
            "INSERT INTO local_thread_catalog (host_id,thread_id,display_title,source_created_at,
             source_updated_at,cwd,source_kind,model_provider,observation_sequence)
             VALUES ('local','old','x',1.0,2.0,'C:\\w','vscode','synaroute',25)",
            [],
        )
        .unwrap();
        // 把 metadata 换成**视图**：`PRAGMA table_info` 照样报出 catalog_revision（那道门
        // 通过），而 SQLite 拒绝对视图 UPDATE → 恰好只让这一步失败。
        c.execute_batch(
            "DROP TABLE local_thread_catalog_metadata;
             CREATE TABLE rev_backing (id INTEGER, catalog_revision INTEGER NOT NULL DEFAULT 0);
             INSERT INTO rev_backing VALUES (1, 7);
             CREATE VIEW local_thread_catalog_metadata AS SELECT * FROM rev_backing;",
        )
        .unwrap();
        drop(c);

        let mut rep = CatalogReport::default();
        repair_one(&db, "synaroute", &[row("old"), row("new1"), row("new2")], &mut rep);
        assert_eq!(rep.inserted, 0, "revision 失败时结构改动必须回滚：{rep:?}");
        assert!(rep.skipped.iter().any(|s| s.contains("版本号")), "失败必须上报: {rep:?}");
        let count: i64 = Connection::open(&db).unwrap().query_row(
            "SELECT COUNT(*) FROM local_thread_catalog WHERE thread_id LIKE 'new%'", [], |r| r.get(0)
        ).unwrap();
        assert_eq!(count, 0, "未推进 revision 的新行不得 commit");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 🔴 **列不齐 / 取不到真实 `host_id` 一律不补行。**
    ///
    /// schema 改过多次，而插一条 Codex 认不出的记录比不插糟得多。`host_id` 尤其不许硬编码
    /// 成 `"local"` —— 那是猜。两种情形下 UPDATE 那两步仍然照做（它们只依赖列存在）。
    #[test]
    fn insertion_is_skipped_when_the_schema_is_unfamiliar() {
        // ① 少一列（没有 observation_sequence）
        let dir = tmp("nocol");
        let db = dir.join("x.db");
        let c = Connection::open(&db).unwrap();
        c.execute_batch(
            "CREATE TABLE local_thread_catalog (host_id TEXT, thread_id TEXT, model_provider TEXT, missing_candidate INTEGER DEFAULT 0);
             INSERT INTO local_thread_catalog VALUES ('local','t1','openai',1);
             CREATE TABLE local_thread_catalog_hosts (host_id TEXT, host_kind TEXT);
             INSERT INTO local_thread_catalog_hosts VALUES ('local','local');",
        )
        .unwrap();
        drop(c);
        let mut rep = CatalogReport::default();
        repair_one(&db, "synaroute", &[row("t1"), row("t2")], &mut rep);
        assert_eq!(rep.inserted, 0, "列不齐时一行都不许补");
        assert_eq!(rep.provider_updated, 1, "但 UPDATE 那两步照做");
        assert_eq!(rep.unmarked_missing, 1);
        // 🔴 判据必须是「**干净地**跳过」，不能只看 `inserted == 0`：把那道门删掉之后
        // INSERT 会引用不存在的列 → SQL 报错 → `inserted` 仍是 0，于是只看它的断言照样绿
        // （注入实测确认）。要求 `skipped` 为空，才把那道门的贡献变成机械可观测的。
        assert!(rep.skipped.is_empty(), "列不齐该是干净跳过，不是试着插然后失败：{rep:?}");

        // ② 列齐了，但 hosts 表里没有 local 那一行 → 拿不到真实 host_id
        let dir2 = tmp("nohost");
        let db2 = catalog_db(&dir2);
        Connection::open(&db2)
            .unwrap()
            .execute("DELETE FROM local_thread_catalog_hosts", [])
            .unwrap();
        let mut rep2 = CatalogReport::default();
        repair_one(&db2, "synaroute", &[row("t1")], &mut rep2);
        assert_eq!(rep2.inserted, 0, "取不到 host_id 就不许猜一个插进去");
        assert!(rep2.skipped.is_empty(), "这一支同样该是干净跳过：{rep2:?}");

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&dir2);
    }

    /// 没有这张表的库（本机的 `state_5.sqlite` 就是）**不是错误**，一个字都不该报。
    #[test]
    fn a_database_without_the_catalog_table_is_not_an_error() {
        let dir = tmp("notable");
        let db = dir.join("state_5.sqlite");
        Connection::open(&db)
            .unwrap()
            .execute("CREATE TABLE threads (id TEXT PRIMARY KEY)", [])
            .unwrap();
        let mut rep = CatalogReport::default();
        repair_one(&db, "synaroute", &[row("t1")], &mut rep);
        assert_eq!(rep.total(), 0);
        assert!(rep.skipped.is_empty(), "这不该被报成跳过：{rep:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 已有内部条目必须被精确移除；报告必须说明不影响路由、rollout 与正文。
    #[test]
    fn existing_guardian_rows_are_removed_and_disclosed() {
        let dir = tmp("existing_guardian");
        let db = catalog_db(&dir);
        let c = Connection::open(&db).unwrap();
        c.execute(
            "INSERT INTO local_thread_catalog (host_id,thread_id,display_title,source_created_at,
             source_updated_at,cwd,source_kind,model_provider,observation_sequence,thread_source)
             VALUES ('local','guardian','title',1,2,'','vscode','synaroute',3,'guardian_review')",
            [],
        ).unwrap();
        drop(c);
        let mut report = CatalogReport::default();
        repair_one(&db, "synaroute", &[guardian_row("guardian")], &mut report);
        assert_eq!(report.removed, 1, "已有 guardian 行必须被清掉：{report:?}");
        assert_eq!(Connection::open(&db).unwrap().query_row(
            "SELECT COUNT(*) FROM local_thread_catalog", [], |r| r.get::<_, i64>(0)
        ).unwrap(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 结构化 source 的 false/null/empty/malformed 不构成删除证据，truthy 标记才构成。
    #[test]
    fn structured_source_requires_a_truthy_known_child_marker() {
        for safe in ["", "{}", "not-json", r#"{"internal":false}"#, r#"{"subagent":null}"#,
            r#"{"sub_agent":""}"#, r#"{"internal":[]}"#] {
            assert!(!source_kind_marks_child(safe), "{safe:?} 不是正向证据");
        }
        for child in [r#"{"internal":true}"#, r#"{"subagent":{"kind":"review"}}"#,
            r#"{"sub_agent":"guardian"}"#, "guardian_review", "subagent", "memory_consolidation"] {
            assert!(source_kind_marks_child(child), "{child:?} 应是正向证据");
        }
    }

    /// 🔴 **Codex 内部派生的线程不许补进 Desktop 列表**（用户 2026-09-10 实报的缺陷本体）。
    ///
    /// 症状：Codex Desktop 的项目侧栏被十几条一模一样的「The following is the Codex agent
    /// history whose request action you are as…」刷满，真正的对话被挤在中间认不出来。
    ///
    /// 两道门各自单独验（[`worth_listing`] 的文档写了为什么不是冗余）：
    /// - `thread_source != "user"` → 不补，**即使标题是干净的**；
    /// - 老 schema 没有那一列（`thread_source` 为空串）时，靠**标题形态**兜住。
    ///
    /// 同一次调用里的正常行必须照旧补上 —— 否则这道门就变成「把整个功能关掉」。
    #[test]
    fn codex_internal_threads_never_reach_the_desktop_list() {
        let dir = tmp("guardian");
        let db = catalog_db(&dir);
        // 三条一起喂：正常的 / 内部派生的 / 老 schema 下只能靠标题认出来的。
        let mut old_schema = guardian_row("t3");
        old_schema.thread_source = String::new(); // 那一列不存在时读出来就是空串
        let mut clean_but_derived = row("t4");
        clean_but_derived.thread_source = "guardian_review".into(); // 标题干净，仅来源可疑

        let mut rep = CatalogReport::default();
        repair_one(
            &db,
            "synaroute",
            &[row("t1"), guardian_row("t2"), old_schema, clean_but_derived],
            &mut rep,
        );
        assert_eq!(rep.inserted, 1, "只有那条 user 会话该被补进去：{rep:?}");

        let c = Connection::open(&db).unwrap();
        let ids: Vec<String> = c
            .prepare("SELECT thread_id FROM local_thread_catalog ORDER BY thread_id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(ids, vec!["t1".to_string()], "侧栏里只该多这一条");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 🔴 **标题列是空串时不许当成「有标题」。**
    ///
    /// `COALESCE` 只跳过 NULL。而 `threads.name` 完全可能是**空串**（Codex 异步生成标题，
    /// 没生成时就是 `''`）—— 裸 `COALESCE` 那时返回空串，我们就往侧栏补一条**没有标题的
    /// 空行**。`view::thread_info` 一直带 `NULLIF`，`plan_from` 这一处漏了。
    ///
    /// 判据**直接对着生成出来的 SQL 断言**，不是 grep 源码里有没有 `NULLIF` 那个词 ——
    /// 后者只要有一处就过，而这里要的是「每个候选列都套上了」。
    #[test]
    fn an_empty_name_must_not_win_over_the_real_title() {
        let cols: HashSet<String> = ["id", "source", "name", "title", "preview", "first_user_message"]
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        let sql = title_expr_for(&cols);
        for c in ["name", "title", "preview", "first_user_message"] {
            assert!(
                sql.contains(&format!("NULLIF({c}, '')")),
                "候选列 {c} 没套 NULLIF —— 它是空串时会赢过后面真正的标题。生成的是: {sql}"
            );
        }
        // 一列都没有时回落到 id（否则 catalog 的 NOT NULL 那一列会插失败）。
        assert_eq!(title_expr_for(&HashSet::new()), "id");
    }

    /// 🔴 **`source_kind` 取不到就不补那一行。**
    ///
    /// 它是 NOT NULL，随便填一个（比如我们自己的名字）会插出一条 Codex 认不出来源的记录。
    #[test]
    fn a_row_without_a_known_source_kind_is_not_inserted() {
        let dir = tmp("nokind");
        let db = catalog_db(&dir);
        let mut r = row("t1");
        r.source_kind = String::new();
        let mut rep = CatalogReport::default();
        repair_one(&db, "synaroute", &[r], &mut rep);
        assert_eq!(rep.inserted, 0, "来源未知就宁可少补");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 🔴 **只允许按 local host + thread id 精确 DELETE，且永不写 sync_state。**
    #[test]
    fn destructive_sql_is_scoped_to_the_local_catalog_row() {
        let prod = crate::proxy::custom_headers::production_code_only(include_str!(
            "codex_session_catalog.rs"
        ));
        assert!(!prod.contains("local_thread_catalog_sync_state"), "不许写 full-sync 水位线");
        let deletes: Vec<&str> = prod.lines().filter(|line| line.contains("DELETE FROM")).collect();
        assert_eq!(deletes.len(), 1, "只能有一条经审查的 DELETE：{deletes:?}");
        assert!(deletes[0].contains("local_thread_catalog"));
        assert!(prod.contains("WHERE host_id = ?1 AND thread_id = ?2"), "DELETE 必须双重精确限定");
        for forbidden in ["DELETE FROM threads", "DELETE FROM thread_spawn_edges",
            "DELETE FROM agent_job_items", "DELETE FROM local_thread_catalog_sync_state"] {
            assert!(!prod.contains(forbidden), "禁止破坏支持表：{forbidden}");
        }
        assert!(prod.contains("UPDATE local_thread_catalog SET model_provider"));
        assert!(prod.contains("SET missing_candidate = 0"));
        assert!(prod.contains("INSERT OR IGNORE INTO local_thread_catalog"));
    }

    /// 🔴 **接线判据**：`sync_sqlite` 必须真的调 `repair_one`，且计划必须**跨库**收集。
    ///
    /// 上面全部用例都直调 `repair_one`。把 `sync_sqlite` 里那一行摘掉它们照样全绿 ——
    /// 而那正是缺陷本体（第三份副本永远是旧值，且完全静默）。第 19 次同类盲区。
    ///
    /// 「跨库」那一半同样要钉：`plan_from` 若只在当前连接里找 `threads`，本机形态下
    /// 一条数据都拿不到（两张表在两个库里），报告会说「0 条」而没人知道为什么。
    #[test]
    fn the_repair_must_be_wired_into_the_sqlite_sync() {
        // 🔴 判据跟着代码搬：`sync_sqlite` 已从 `codex_sessions.rs` 移到
        // `codex_session_sqlite.rs`（为守 900 行门）。扫错文件的表现是**假红**，
        // 而假红的代价是下一个人把搬家撤回去。同 CLAUDE.md「搬代码时判据要跟着搬」。
        let prod = crate::proxy::custom_headers::production_code_only(include_str!(
            "codex_session_sqlite.rs"
        ));
        assert!(prod.contains("catalog::repair_one("), "sync_sqlite 里没接上这一步");
        assert!(prod.contains("catalog::plan_from("), "没有跨库收集 threads 那一步");
        let sessions = crate::proxy::custom_headers::production_code_only(include_str!("codex_sessions.rs"));
        assert!(sessions.contains("catalog::merge_rollout_evidence("), "sync_to_at 必须把 scan.sessions 交给 cleanup");
        assert!(sessions.contains("sqlite::collect_child_thread_ids("), "edge/job 正向证据必须接入");
        let me = crate::proxy::custom_headers::production_code_only(include_str!(
            "codex_session_catalog.rs"
        ));
        assert!(
            me.contains("for db in super::session_db_paths(home)"),
            "plan_from 必须遍历所有库 —— 本机的 threads 与 catalog 在两个不同的库里"
        );
    }
}
