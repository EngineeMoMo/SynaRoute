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
//! 3. **补缺行**（INSERT OR IGNORE）—— 磁盘上有 rollout、`threads` 里也有记录，而 catalog
//!    里没有它。这一条是「会话从 Desktop 列表里消失」的修法。
//!
//! # 🔴 补缺行为什么要格外小心（以及我们收窄了哪些）
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
//!   那张表的水位线（`update_full_sync_state`），我们**只做定点补行**，不声称自己完成了一轮
//!   全量对账 —— 推进水位线可能让 Codex 跳过它本该自己做的扫描，那是拿一个未取证的写入去换
//!   一点整洁；
//! - **刻意不删任何行**。它的 `repair_..._filtered` 会 `DELETE` 掉「被标成非根」的条目，
//!   而那依赖 `thread_spawn_edges` 的一套推导。删用户列表里的条目属于「影响范围超出修复」；
//! - 写之前照旧**备份整库**（父模块的 `backup_db` 已经在做），备份失败就不写那个库。
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
    /// 逐库失败原因（被 WAL 锁住、列不齐等），如实上报、不阻断。
    pub skipped: Vec<String>,
}

impl CatalogReport {
    /// 三项之和。测试段用它做「一个字节都没写」的整体断言。
    #[cfg(test)]
    pub fn total(&self) -> usize {
        self.provider_updated + self.unmarked_missing + self.inserted
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

/// 一条要补进 catalog 的记录。字段全部来自我们已经读到的东西（rollout 首行 + `threads`）。
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
        // 标题按 catalog 自己的优先级取（`name` 是 Codex 生成的短标题，见 `view` 模块头）。
        let title = ["name", "title", "preview", "first_user_message"]
            .iter()
            .filter(|c| cols.contains(**c))
            .map(|c| (*c).to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let title_expr = if title.is_empty() { "id".into() } else { format!("COALESCE({title}, id)") };
        // 时间戳两种形态都可能在：毫秒列优先（更精确），回落秒列。
        let ts = |ms: &str, s: &str| -> String {
            match (cols.contains(ms), cols.contains(s)) {
                (true, true) => format!("COALESCE({ms} / 1000.0, {s}, 0)"),
                (true, false) => format!("COALESCE({ms} / 1000.0, 0)"),
                (false, true) => format!("COALESCE({s}, 0)"),
                (false, false) => "0".into(),
            }
        };
        let sql = format!(
            "SELECT id, {title_expr}, COALESCE(cwd, ''), {}, {}, COALESCE(source, '') FROM threads",
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

/// 对一个库做三件事。任何一步失败都只计入 `skipped`，不阻断其余库。
///
/// `rows` 是「磁盘上真的存在、且我们知道其 provider」的会话；`target` 是同步目标。
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
    let ids: Vec<&str> = rows.iter().map(|r| r.thread_id.as_str()).collect();

    // ① provider：把这一份也改对。
    if cols.contains("model_provider") && cols.contains("thread_id") {
        for chunk in ids.chunks(super::SQL_CHUNK) {
            let holes = std::iter::repeat("?").take(chunk.len()).collect::<Vec<_>>().join(",");
            let sql = format!(
                "UPDATE local_thread_catalog SET model_provider = ?1 \
                 WHERE thread_id IN ({holes}) AND model_provider IS NOT ?1"
            );
            let mut params: Vec<&dyn rusqlite::ToSql> = vec![&target];
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
        for chunk in ids.chunks(super::SQL_CHUNK) {
            let holes = std::iter::repeat("?").take(chunk.len()).collect::<Vec<_>>().join(",");
            let sql = format!(
                "UPDATE local_thread_catalog SET missing_candidate = 0 \
                 WHERE thread_id IN ({holes}) AND COALESCE(missing_candidate, 0) <> 0"
            );
            let params: Vec<&dyn rusqlite::ToSql> =
                chunk.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
            match conn.execute(&sql, params.as_slice()) {
                Ok(n) => report.unmarked_missing += n,
                Err(e) => report
                    .skipped
                    .push(format!("{}: missing 标记未清（{e}）", db_path.display())),
            }
        }
    }

    // ③ 补缺行。列不齐 / 取不到 host_id 一律不做（见模块头）。
    if !supports_repair(&cols) {
        return;
    }
    let Some(host) = host_id(&conn) else { return };
    let existing: HashSet<String> = conn
        .prepare("SELECT thread_id FROM local_thread_catalog WHERE host_id = ?1")
        .and_then(|mut s| {
            s.query_map([&host], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<_>>()
        })
        .unwrap_or_default();
    let missing: Vec<&CatalogRow> = rows
        .iter()
        .filter(|r| !existing.contains(&r.thread_id) && !r.source_kind.is_empty())
        .collect();
    if missing.is_empty() {
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
    for row in missing {
        seq += 1;
        let vals = insert_values(&insert_cols, &host, row, seq);
        match tx.execute(&sql, rusqlite::params_from_iter(vals)) {
            Ok(n) => inserted += n,
            Err(e) => {
                report.skipped.push(format!("{}: 补行失败（{e}）", db_path.display()));
                break;
            }
        }
    }
    // `catalog_revision` 是 Desktop 判「索引变过了」的依据。补了行不推它，界面可能不刷新。
    // **刻意只推这一个**，不碰 `local_thread_catalog_sync_state` 的水位线（理由见模块头）。
    if inserted > 0 && columns_of(&tx, "local_thread_catalog_metadata").contains("catalog_revision")
    {
        // 🔴 **失败要留痕，不能 `let _ =` 吞掉** —— 上面那句注释说的就是这次失败的后果
        // （「补了行不推它，界面可能不刷新」）。吞掉它的表现是：我们报「已补 N 条」，
        // 而 Desktop 列表一点变化都没有，用户手里没有任何线索解释这个矛盾。
        // 刻意**不**因此回滚：补进去的行本身是对的，少推一个版本号的代价只是「可能要重启
        // Codex 才看得到」，而回滚会把一次有效的修复整个丢掉。
        if let Err(e) = tx.execute(
            "UPDATE local_thread_catalog_metadata SET catalog_revision = catalog_revision + ?1",
            [inserted as i64],
        ) {
            report
                .skipped
                .push(format!("{}: 版本号未推进（{e}）—— 可能要重启 Codex 才看到新条目", db_path.display()));
        }
    }
    match tx.commit() {
        Ok(()) => report.inserted += inserted,
        Err(e) => report.skipped.push(format!("{}: 提交失败（{e}）", db_path.display())),
    }
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
    for opt in ["missing_candidate", "source_recency_at", "pending_observed_title"] {
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
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 🔴 **版本号推不动要留痕，不能吞掉。**
    ///
    /// `catalog_revision` 是 Desktop 判「索引变过了」的依据。推不动时的表现是：我们报
    /// 「已补 N 条」，而 Desktop 列表一点变化都没有 —— 用户手里没有任何线索解释这个矛盾。
    /// 第一版是 `let _ = tx.execute(..)`，注入实测**照样全绿**（8 条用例都没碰这一维）。
    ///
    /// 夹具用**视图**：`columns_of` 走 `PRAGMA table_info`，对视图照样报出列名 → 那道门通过；
    /// 而 SQLite 拒绝对视图 UPDATE → 恰好只让这一步失败。用只读文件之类做不到这么精确
    /// （会连 INSERT 一起挡掉，那时 `inserted` 是 0、压根走不到这一段）。
    ///
    /// **刻意不因此回滚**：补进去的行本身是对的，少推一个版本号只是「可能要重启 Codex 才
    /// 看得到」，而回滚会把一次有效的修复整个丢掉 —— 所以这条同时断言 `inserted` 仍然是 2。
    #[test]
    fn a_revision_bump_that_fails_is_reported_not_swallowed() {
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
        assert_eq!(rep.inserted, 2, "补行本身是对的，不许因为推不动版本号而回滚：{rep:?}");
        assert!(
            rep.skipped.iter().any(|s| s.contains("版本号")),
            "推不动版本号必须留痕 —— 否则「已补 N 条」与「列表没变化」这个矛盾无解: {rep:?}"
        );
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

    /// 🔴 **刻意不推 `local_thread_catalog_sync_state` 的水位线，也不删任何行。**
    ///
    /// CodexPlusPlus 在「全量同步」时会做这两件事。我们只做定点补行 —— 声称完成了一轮全量
    /// 对账可能让 Codex 跳过它本该自己做的扫描，而删条目属于「影响范围超出修复」。
    /// 判据钉形态：生产段里不许出现那张表，也不许有 DELETE。
    #[test]
    fn we_neither_advance_the_watermark_nor_delete_rows() {
        let prod = crate::proxy::custom_headers::production_code_only(include_str!(
            "codex_session_catalog.rs"
        ));
        assert!(
            !prod.contains("local_thread_catalog_sync_state"),
            "不许推那张表的水位线（理由见本测试文档）"
        );
        assert!(!prod.contains("DELETE FROM"), "不许删用户列表里的条目");
        // 正向：确实在做那三件事，否则上面两条是空洞的绿。
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
        let prod = crate::proxy::custom_headers::production_code_only(include_str!(
            "codex_sessions.rs"
        ));
        assert!(prod.contains("catalog::repair_one("), "sync_sqlite 里没接上这一步");
        assert!(prod.contains("catalog::plan_from("), "没有跨库收集那一步");
        let me = crate::proxy::custom_headers::production_code_only(include_str!(
            "codex_session_catalog.rs"
        ));
        assert!(
            me.contains("for db in super::session_db_paths(home)"),
            "plan_from 必须遍历所有库 —— 本机的 threads 与 catalog 在两个不同的库里"
        );
    }
}
