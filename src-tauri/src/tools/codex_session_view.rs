//! 会话列表的**展示层**：标题、模型、档位、token、统计。
//!
//! # 为什么单独一层
//!
//! 父模块 [`super`] 只读 rollout 首行，那是**路由正确性**要的全部信息。但首行里没有
//! 「这是哪条对话」—— 用户看到的是一堆时间戳与同一个工作目录，认不出哪条是哪条。
//! CodexPlusPlus 的会话页之所以可读，是因为它整个列表**直接读 `state_5.sqlite`**
//! （它的界面上就写着「读取 Codex 本地 state_5.sqlite」）。
//!
//! 🔴 **我们刻意不照抄那个数据源**，而是「文件为准 + 库来补充」：
//!
//! - 本机实测磁盘上有 **5** 个 rollout，而 `threads` 表只有 **3** 行 —— fork 出来的子会话
//!   压根没有 `threads` 记录。以库为准的列表会把它们整个漏掉，而它们**也带 provider**、
//!   也会被 `thread/resume` 用到；
//! - Codex 在跑时 sqlite 常被 WAL 锁着，以库为准的列表那时会变成空的，
//!   而「Codex 正开着」恰恰是用户来这一页的典型时刻；
//! - provider 的事实来源是 rollout（见父模块模块头），列表与同步该看同一份东西。
//!
//! 所以库只用来补三样**纯展示**的东西：标题、模型/档位、token 数。取不到就留空 ——
//! 那时回落到从正文里读第一条用户消息。
//!
//! # 标题的取值顺序：`name` → `title` → `first_user_message` → 正文
//!
//! 实测 `threads` 里这几列同时存在，而 **`name` 是 Codex 自己生成的短标题**
//! （「实现 Java 快速排序」），`title` / `first_user_message` / `preview` 都是**原始的
//! 第一条用户消息**（「使用java写一个快速排序」）。CodexPlusPlus 显示的是后者。
//! 优先 `name` 是因为那一列本来就是为「让人认出这条对话」而存在的。

use super::{files, SessionRef};
use serde::Serialize;
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::Path;

/// 标题最长显示多少字符。
///
/// 上限存在的理由不是省地方：本机就有一条会话的第一条用户消息是**整段被审查的对话历史**
/// （guardian 子代理，`first_user_message` 以 "The following is the Codex agent history…"
/// 开头，长达数千字符）。不截断的话那一行会把整张表挤变形。
const TITLE_MAX_CHARS: usize = 60;

/// 从正文里找第一条用户消息时最多读多少字节。
///
/// 首行本身就有 50 KB 量级（`base_instructions` 内嵌在里面，已被跳过不计），随后可能还有
/// Desktop 注入的 `developer` 消息。512 KB 足够越过它们；读不到就留空标题 ——
/// 为一行展示文字去读几十 MB 是本末倒置，而列表要对几百条会话都做这件事。
const BODY_SCAN_BYTES: u64 = 512 * 1024;

/// 会话库里那几列纯展示信息。
#[derive(Debug, Default, Clone)]
pub(in crate::tools) struct ThreadInfo {
    pub title: String,
    pub model: String,
    pub effort: String,
    pub tokens: u64,
}

/// 列表页顶部那张统计卡。
///
/// CodexPlusPlus 的会话页顶部就是这四个数字 + 库路径。它们的用处很具体：**「总数」与我们
/// 报的「已同步 N 条」能对上账**，而 `dbPath` 回答「你到底在读哪个库」—— 后者在
/// `CODEX_SQLITE_HOME` 被设过、或机器上同时有新旧两个库时是唯一能定性的信息。
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStats {
    pub total: usize,
    pub active: usize,
    pub archived: usize,
    /// provider 与当前生效的根 provider 不一致的条数（当前 provider 读不出时恒为 0）。
    pub mismatched: usize,
    /// 我们实际会读写的会话库。空串 = 一个都没找到（新装机器的正常形态）。
    pub db_path: String,
    /// 会话库里能匹配上的条目数。**与 `total` 的差额就是「库里没有记录的会话」**
    /// （fork 子会话属于这一类），而那个差额正是「为什么别的工具少列几条」的答案。
    pub in_db: usize,
    /// 用的模型现在已经服务不了的条数（见 [`super::SessionRef::model_unserviceable`]）。
    /// 这一位**只有代理侧能给** —— 启动器类工具没有 Key 池。
    pub model_gone: usize,
}

/// 标题回落（读正文）的进程内缓存。
///
/// 键是 `(相对路径, mtime, 大小)` —— 三者都不变就认为标题不会变。这一趟对**每条**库里没有
/// 记录的会话都要开一次文件，而列表页是可以反复刷新的（用户点「刷新」、同步完自动刷、
/// 切页面回来再刷）。几百条会话时不缓存就是每次刷新几百次 open+read。
///
/// 上限存在的理由与 `lan_guard` 的 `SEEN` 一样：它是进程级只增不减的表，没有上限就是无界
/// 增长。到顶直接整份清空（不做 LRU）—— 会话总数在实际形态下远小于上限，走到清空说明
/// 有人的 `CODEX_HOME` 里有上万个 rollout，那时重建一次的代价可以接受。
///
/// ⚠️ **这一条没有机械判据**（注入实测：把清空那三行删掉，用例照样全绿）。要验它得造
/// 4096 个夹具文件，成本与收益不成比例。**别把这句话读成「有测试在守」** —— 同
/// `only_v6_must_be_set_explicitly` 那条的处置：守不住的就写明守不住，而不是假装有防线。
const TITLE_CACHE_MAX: usize = 4096;

/// 键 = (相对路径, mtime 毫秒, 字节数)。
type TitleCache = HashMap<(String, u64, u64), String>;

static TITLE_CACHE: std::sync::Mutex<Option<TitleCache>> = std::sync::Mutex::new(None);

/// 缓存键里的 mtime 用**毫秒**（`SystemTime` 不能直接当 HashMap 的键）。取不到就用 0 ——
/// 那时缓存仍然按「路径 + 大小」命中，代价是极端情况下标题可能过时一次。
fn cache_key(home: &Path, rel: &str) -> (String, u64, u64) {
    let meta = std::fs::metadata(home.join(rel));
    let (ms, len) = meta
        .map(|m| {
            let ms = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            (ms, m.len())
        })
        .unwrap_or((0, 0));
    (rel.to_string(), ms, len)
}

fn cached_title(home: &Path, rel: &str) -> Option<String> {
    let key = cache_key(home, rel);
    let mut guard = TITLE_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let map = guard.get_or_insert_with(TitleCache::new);
    if let Some(hit) = map.get(&key) {
        return Some(hit.clone());
    }
    let path = files::resolve_in_home(home, rel)?;
    let title = clip(&first_user_message(&path).unwrap_or_default());
    if map.len() >= TITLE_CACHE_MAX {
        map.clear();
    }
    map.insert(key, title.clone());
    Some(title)
}

/// Desktop 的项目侧栏归属：`.codex-global-state.json` 里那两个键。
///
/// # 🔴 只读，绝不写这个文件
///
/// 它是 Electron 的持久化 atom store —— 本机实测 **24 个顶层键**，装着窗口位置、
/// onboarding 标记、prompt 历史（`prompt-history` 里就是用户逐字打过的每一句话）。
/// Desktop 在运行时把它整份持有在内存里并按自己的节奏整份写回。我们去写它 = 最后写的人赢，
/// 也就是**可能把 Desktop 更新的状态整片抹掉**（用户会看到 prompt 历史莫名消失）。
///
/// 那是用户的数据、且不是我们负责的东西。CodexPlusPlus 会写它；我们刻意只读 ——
/// 读到的信息已经能回答用户会问的那个问题：「这条对话为什么不在我的项目侧栏里」。
///
/// 判据：本模块生产段里不许出现对这个文件的写入（见测试段那条形态判据）。
fn project_names(home: &Path) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Ok(text) = std::fs::read_to_string(home.join(".codex-global-state.json")) else {
        return out;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        return out;
    };
    // id → 项目名
    let mut by_id: HashMap<&str, &str> = HashMap::new();
    if let Some(projects) = v.get("local-projects").and_then(|p| p.as_object()) {
        for (id, p) in projects {
            if let Some(name) = p.get("name").and_then(|n| n.as_str()) {
                by_id.insert(id.as_str(), name);
            }
        }
    }
    // thread → 项目名
    if let Some(assign) = v.get("thread-project-assignments").and_then(|a| a.as_object()) {
        for (thread, a) in assign {
            if let Some(name) = a
                .get("projectId")
                .and_then(|i| i.as_str())
                .and_then(|i| by_id.get(i))
            {
                out.insert(thread.clone(), (*name).to_string());
            }
        }
    }
    out
}

/// 把库里的展示信息补进扫描结果，并算出统计。
///
/// 顺序刻意是「先库、再正文兜底」：库那一趟是一次 SQL、几乎免费，正文那一趟要开文件。
///
/// `codex_keys` 是 Codex 分类当前**启用**的 Key（判「这条对话用的模型还服务得了吗」）。
/// 空切片 = 不做这个判定，一条都不标（见 [`SessionRef::model_unserviceable`]）。
pub(in crate::tools) fn enrich(
    home: &Path,
    rows: &mut [SessionRef],
    current_provider: &str,
    codex_keys: &[crate::model::ProviderKey],
) -> SessionStats {
    let info = thread_info(home);
    let projects = project_names(home);
    let mut stats = SessionStats {
        total: rows.len(),
        db_path: super::session_db_paths(home)
            .first()
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
        ..SessionStats::default()
    };
    for r in rows.iter_mut() {
        if r.archived {
            stats.archived += 1;
        } else {
            stats.active += 1;
        }
        // 🔴 统计卡的这一位与表格里「哪些行标红」必须**同源**：两处各算一遍必然漂移，
        // 而漂移的表现是「表头说 3 条指向别处、表里只标红 1 行」，用户不知道该信哪个。
        // 前端拿的就是这个数字（`data.stats.mismatched`），不自己再 filter 一遍。
        if !current_provider.is_empty() && r.provider != current_provider {
            stats.mismatched += 1;
        }
        r.cwd = files::display_path(&r.cwd);
        // Desktop 项目侧栏的归属（只读，见 project_names）。空串 = 未归入任何项目 ——
        // 那正是「这条对话为什么不在我的项目侧栏里」的答案。
        if let Some(p) = projects.get(&r.thread_id) {
            r.project = p.clone();
        }
        if let Some(i) = info.get(&r.thread_id) {
            stats.in_db += 1;
            r.title = clip(&i.title);
            r.model = i.model.clone();
            r.effort = i.effort.clone();
            r.tokens = i.tokens;
        }
        if r.title.is_empty() {
            // 库里没有它（fork 子会话），或那几列都是空的 —— 回落到读正文。
            r.title = cached_title(home, &r.rel_path).unwrap_or_default();
        }
        // 只在「有模型名 + 确实有启用的 Key」时才敢下这个结论。
        if !r.model.is_empty() && !codex_keys.is_empty() {
            r.model_unserviceable =
                !codex_keys.iter().any(|k| crate::proxy::model_pool::may_serve(k, &r.model));
            if r.model_unserviceable {
                stats.model_gone += 1;
            }
        }
    }
    stats
}

/// 按 `TITLE_MAX_CHARS` 截断，并把换行压成空格。
///
/// 压换行是必须的：第一条用户消息经常是多行的，原样塞进表格单元格会让行高炸开。
/// **按字符而不是字节**截断 —— 中文标题在字节边界上切会得到一个非法 UTF-8 片段。
fn clip(s: &str) -> String {
    let one_line = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= TITLE_MAX_CHARS {
        return one_line;
    }
    let head: String = one_line.chars().take(TITLE_MAX_CHARS).collect();
    format!("{head}…")
}

/// 读会话库里的展示信息。任何失败都返回目前收集到的那部分 —— 这一层**不许影响列表能不能
/// 出来**：库被 Codex 锁着是常态，那时用户看到的应该是「没有标题的完整列表」，
/// 而不是一个错误页。
fn thread_info(home: &Path) -> HashMap<String, ThreadInfo> {
    let mut out: HashMap<String, ThreadInfo> = HashMap::new();
    for db in super::session_db_paths(home) {
        let Ok(conn) = rusqlite::Connection::open_with_flags(
            &db,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        ) else {
            continue;
        };
        let _ = conn.busy_timeout(std::time::Duration::from_millis(800));
        // 列是**按名字探测**的，不是写死一串 SELECT：Codex 的 schema 改过多次（`state_5`
        // 里那个数字就是版本号），少一列就整条查询报错的话，这一层会在某次 Codex 升级后
        // 静默变成「所有标题都没了」。
        let cols: Vec<String> = conn
            .prepare("PRAGMA table_info('threads')")
            .and_then(|mut s| {
                s.query_map([], |r| r.get::<_, String>(1))?.collect::<rusqlite::Result<_>>()
            })
            .unwrap_or_default();
        let has = |c: &str| cols.iter().any(|x| x == c);
        if !has("id") {
            continue;
        }
        // 标题的取值顺序见模块头。`COALESCE` 里只放**确实存在**的列。
        let title_expr = ["name", "title", "first_user_message", "preview"]
            .iter()
            .filter(|c| has(c))
            .map(|c| format!("NULLIF({c}, '')"))
            .collect::<Vec<_>>()
            .join(", ");
        let title_expr =
            if title_expr.is_empty() { "''".to_string() } else { format!("COALESCE({title_expr}, '')") };
        let pick = |c: &str| if has(c) { c.to_string() } else { "''".to_string() };
        let sql = format!(
            "SELECT id, {title_expr}, {}, {}, {} FROM threads",
            pick("model"),
            pick("reasoning_effort"),
            if has("tokens_used") { "tokens_used" } else { "0" },
        );
        let Ok(mut stmt) = conn.prepare(&sql) else { continue };
        let Ok(rows) = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                ThreadInfo {
                    title: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    model: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    effort: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                    tokens: r.get::<_, Option<i64>>(4)?.unwrap_or(0).max(0) as u64,
                },
            ))
        }) else {
            continue;
        };
        for (id, info) in rows.flatten() {
            // 多个库时**先到的赢**（`session_db_paths` 把新布局排在 legacy 之前）——
            // legacy 库里可能是迁移前的旧快照，用它覆盖新库会让标题倒退。
            out.entry(id).or_insert(info);
        }
    }
    out
}

/// 从 rollout 正文里读第一条用户消息。
///
/// **跳过首行**（那是 `session_meta`，含 50 KB 的 `base_instructions`），逐行找
/// `response_item` + `payload.type == "message"` + `role == "user"`。`developer` / `system`
/// 一律不算 —— 那是 Desktop 注入的 app-context，拿它当标题等于每条会话都叫同一个名字。
fn first_user_message(path: &Path) -> Option<String> {
    let f = std::fs::File::open(path).ok()?;
    let mut reader = BufReader::new(f);
    let mut line = String::new();
    // 首行单独吃掉（不解析）。
    reader.read_line(&mut line).ok()?;
    let mut budget = BODY_SCAN_BYTES;
    loop {
        line.clear();
        let n = reader.read_line(&mut line).ok()?;
        if n == 0 {
            return None;
        }
        budget = budget.saturating_sub(n as u64);
        if let Some(text) = user_text(&line) {
            return Some(text);
        }
        if budget == 0 {
            // 读够了还没找到就放弃：这只是一行展示文字，不值得为它把几十 MB 读完。
            return None;
        }
    }
}

fn user_text(line: &str) -> Option<String> {
    let rec: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
    if rec.get("type").and_then(|v| v.as_str()) != Some("response_item") {
        return None;
    }
    let p = rec.get("payload")?;
    if p.get("type").and_then(|v| v.as_str()) != Some("message")
        || p.get("role").and_then(|v| v.as_str()) != Some("user")
    {
        return None;
    }
    let text = match p.get("content") {
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .filter_map(|i| i.get("text").and_then(|v| v.as_str()))
            .collect::<Vec<_>>()
            .join(" "),
        Some(serde_json::Value::String(s)) => s.clone(),
        _ => String::new(),
    };
    (!text.trim().is_empty()).then(|| text.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    fn tmp_home(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "synaroute-cxview-{}-{}-{tag}",
            std::process::id(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("sessions/2026/09/01")).unwrap();
        d
    }

    /// 造一条 rollout。`extra` 用来插额外的正文行。
    fn write_rollout(home: &Path, name: &str, provider: &str, extra: &str) -> String {
        let rel = format!("sessions/2026/09/01/{name}");
        let mut text = format!(
            "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"01a05d14-4e5b-7773-b425-25ae029f078f\",\"timestamp\":\"2026-09-01T13:06:47Z\",\"cwd\":\"\\\\\\\\?\\\\C:\\\\work\",\"thread_source\":\"user\",\"model_provider\":\"{provider}\"}}}}\n"
        );
        text.push_str(extra);
        fs::write(home.join(&rel), text).unwrap();
        rel
    }

    const USER_LINE: &str = "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"使用java写一个快速排序\"}]}}\n";
    const DEV_LINE: &str = "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"developer\",\"content\":[{\"type\":\"input_text\",\"text\":\"APP-CONTEXT\"}]}}\n";

    /// 🔴 标题优先取库里的 `name`（Codex 自己生成的短标题），而不是原始第一条消息。
    ///
    /// 实测那两列同时存在且值不同：`name` = 「实现 Java 快速排序」，
    /// `title`/`first_user_message` = 「使用java写一个快速排序」。CodexPlusPlus 显示后者。
    #[test]
    fn the_generated_name_wins_over_the_raw_first_message() {
        let home = tmp_home("title");
        let rel = write_rollout(
            &home,
            "rollout-2026-09-01T21-06-47-01a05d14-4e5b-7773-b425-25ae029f078f.jsonl",
            "synaroute",
            USER_LINE,
        );
        let db = home.join("state_5.sqlite");
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute(
            "CREATE TABLE threads (id TEXT, name TEXT, title TEXT, first_user_message TEXT, model TEXT, reasoning_effort TEXT, tokens_used INTEGER)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO threads VALUES ('01a05d14-4e5b-7773-b425-25ae029f078f','实现 Java 快速排序','使用java写一个快速排序','使用java写一个快速排序','glm-5.3','xhigh',2283034)",
            [],
        )
        .unwrap();
        drop(conn);

        let mut rows = super::super::scan_at(&home).sessions;
        assert_eq!(rows.len(), 1);
        let stats = enrich(&home, &mut rows, "synaroute", &[]);
        assert_eq!(rows[0].title, "实现 Java 快速排序");
        assert_eq!(rows[0].model, "glm-5.3");
        assert_eq!(rows[0].effort, "xhigh");
        assert_eq!(rows[0].tokens, 2_283_034);
        // cwd 的扩展长度前缀只在展示层剥掉。
        assert_eq!(rows[0].cwd, r"C:\work", "\\\\?\\ 前缀要归一，否则同一目录出现两种写法");
        assert_eq!((stats.total, stats.in_db, stats.mismatched), (1, 1, 0));
        assert!(stats.db_path.ends_with("state_5.sqlite"), "要报出实际读的库: {}", stats.db_path);
        let _ = fs::remove_file(home.join(&rel));
        let _ = fs::remove_dir_all(&home);
    }

    /// 🔴 库里没有记录的会话（fork 子会话就是这一类）必须**仍然出现在列表里**，
    /// 标题回落到正文的第一条用户消息。
    ///
    /// 这是我们与 CodexPlusPlus 的关键差异：它整个列表读 sqlite，于是本机 5 个 rollout
    /// 只列出 3 条。漏掉的那些**同样带 provider**、同样会被 `thread/resume` 用到。
    /// `in_db` 与 `total` 的差额就是这个数字。
    #[test]
    fn a_session_missing_from_the_database_still_gets_a_title_from_its_body() {
        let home = tmp_home("fallback");
        write_rollout(
            &home,
            // 两个 UUID = fork 子会话，末尾那个才是它自己的 id。
            "rollout-2026-09-01T21-08-35-01a05d14-4e5b-7773-b425-25ae029f078f_01a05d15-f502-7363-867c-b9ab2203be6f.jsonl",
            "openai",
            &format!("{DEV_LINE}{USER_LINE}"),
        );
        let mut rows = super::super::scan_at(&home).sessions;
        let stats = enrich(&home, &mut rows, "synaroute", &[]);

        assert_eq!(rows.len(), 1);
        assert!(rows[0].forked, "文件名带两个 UUID = fork 子会话");
        assert_eq!(
            rows[0].title, "使用java写一个快速排序",
            "库里没有它就该从正文读第一条**用户**消息（developer 那条不算）"
        );
        assert_eq!((stats.total, stats.in_db, stats.mismatched), (1, 0, 1));
        let _ = fs::remove_dir_all(&home);
    }

    /// 超长标题必须截断，而且**按字符截**。
    ///
    /// 本机真有这样一条：guardian 子代理的第一条用户消息是整段被审查的对话历史
    /// （"The following is the Codex agent history…"）。不截断会把整张表挤变形；
    /// 按字节截会在中文中间切出非法 UTF-8。
    #[test]
    fn an_overlong_title_is_clipped_by_characters() {
        let long = "中".repeat(200);
        let out = clip(&long);
        assert_eq!(out.chars().count(), TITLE_MAX_CHARS + 1, "截断后再加一个省略号");
        assert!(out.ends_with('…'));
        // 多行压成一行 —— 第一条用户消息经常是多行的。
        assert_eq!(clip("a\nb  c\n"), "a b c");
        assert_eq!(clip("short"), "short");
    }

    /// 库缺列 / 库压根打不开时，列表要照样出来（只是没有标题）。
    ///
    /// 「Codex 正开着、库被 WAL 锁住」恰恰是用户来这一页的典型时刻，那时候把列表变成
    /// 一个错误页是最坏的行为。
    #[test]
    fn a_database_without_the_columns_does_not_break_the_list() {
        let home = tmp_home("nocol");
        write_rollout(
            &home,
            "rollout-2026-09-01T21-06-47-01a05d14-4e5b-7773-b425-25ae029f078f.jsonl",
            "openai",
            "",
        );
        let db = home.join("state_5.sqlite");
        let conn = rusqlite::Connection::open(&db).unwrap();
        // 只有 id 一列 —— 模拟 Codex 改过 schema。
        conn.execute("CREATE TABLE threads (id TEXT)", []).unwrap();
        conn.execute("INSERT INTO threads VALUES ('01a05d14-4e5b-7773-b425-25ae029f078f')", [])
            .unwrap();
        drop(conn);

        let mut rows = super::super::scan_at(&home).sessions;
        let stats = enrich(&home, &mut rows, "synaroute", &[]);
        assert_eq!(rows.len(), 1, "缺列不许让列表变空");
        assert_eq!(rows[0].title, "", "没有正文也没有标题列 → 留空，不报错");
        assert_eq!(stats.in_db, 1);
        let _ = fs::remove_dir_all(&home);
    }

    /// 🔴 「这条对话用的模型现在还服务得了吗」—— **只有代理侧能回答的那一位**。
    ///
    /// 用处很具体：用户点开一条旧对话报错，而 provider 那一列是绿的。真成因可能是他后来删掉
    /// 了服务 `glm-5.3` 的那条 Key，或改了模型映射。没有这一位，那条对话在界面上「一切正常」
    /// 而实际打开必然降级或失败 —— 启动器类工具（CodexPlusPlus / codex-provider-sync）
    /// 永远给不出它，因为它们没有 Key 池。
    ///
    /// 三个边界一起钉住（都对应一种误报）：
    /// ① 有 Key 且认识那个模型 → 不标；
    /// ② 有 Key 但没人认识 → 标；
    /// ③ **压根没有启用的 Key → 一条都不标**。③ 最要紧：那只说明用户还没配 Codex，
    ///    而按「没人能服务」判会把整张表刷红 —— 同 `balance_gate` 的「查不到 ≠ 为零」。
    #[test]
    fn a_model_no_key_can_serve_is_flagged_but_only_when_we_actually_know() {
        let home = tmp_home("gone");
        write_rollout(
            &home,
            "rollout-2026-09-01T21-06-47-01a05d14-4e5b-7773-b425-25ae029f078f.jsonl",
            "synaroute",
            USER_LINE,
        );
        let db = home.join("state_5.sqlite");
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute("CREATE TABLE threads (id TEXT, name TEXT, model TEXT)", []).unwrap();
        conn.execute(
            "INSERT INTO threads VALUES ('01a05d14-4e5b-7773-b425-25ae029f078f','旧对话','glm-5.3')",
            [],
        )
        .unwrap();
        drop(conn);

        let mut serving = crate::service::tests::key(crate::model::CategoryType::Codex);
        serving.models = vec![crate::model::ModelInfo {
            real_name: "glm-5.3".into(),
            source: "manual".into(),
            fetched_at: None,
            context_window: None,
            max_output_tokens: None,
        }];
        let mut elsewhere = serving.clone();
        elsewhere.id = "k2".into();
        elsewhere.models = vec![crate::model::ModelInfo {
            real_name: "gpt-5.6".into(),
            source: "manual".into(),
            fetched_at: None,
            context_window: None,
            max_output_tokens: None,
        }];

        let run = |keys: &[crate::model::ProviderKey]| {
            let mut rows = super::super::scan_at(&home).sessions;
            let stats = enrich(&home, &mut rows, "synaroute", keys);
            (rows[0].model_unserviceable, stats.model_gone)
        };

        assert_eq!(run(&[serving.clone()]), (false, 0), "① 有 Key 认识它 → 不许标");
        assert_eq!(run(&[elsewhere.clone()]), (true, 1), "② 有 Key 但没人认识 → 要标");
        assert_eq!(
            run(&[]),
            (false, 0),
            "③ 没有启用的 Key 时一条都不许标 —— 那只说明用户还没配 Codex"
        );
        let _ = fs::remove_dir_all(&home);
    }

    /// 标题回落有进程内缓存，**键里必须带 mtime**。
    ///
    /// 列表页是可以反复刷新的（点「刷新」、同步完自动刷、切页面回来再刷），而回落那一趟对
    /// **每条**库里没有记录的会话都要开一次文件。两个方向一起钉：
    ///
    /// ① mtime 与大小都不变 → 必须命中缓存（判据是「悄悄换掉正文、把 mtime 拨回去，
    ///    仍拿到旧标题」—— 那只可能来自缓存）；
    /// ② mtime 变了 → 必须失效。缺这一半的表现是**用户继续对话之后标题永远停在旧的那条**，
    ///    而那种陈旧是静默的。
    #[test]
    fn the_title_fallback_is_cached_and_keyed_on_mtime() {
        let home = tmp_home("cache");
        let rel = write_rollout(
            &home,
            "rollout-2026-09-01T21-06-47-01a0aaaa-4e5b-7773-b425-25ae029f0777.jsonl",
            "synaroute",
            USER_LINE,
        );
        let path = home.join(&rel);
        let first = || {
            let mut rows = super::super::scan_at(&home).sessions;
            enrich(&home, &mut rows, "synaroute", &[]);
            rows[0].title.clone()
        };
        assert_eq!(first(), "使用java写一个快速排序");

        // 换成**等长**的另一段正文，再把 mtime 拨回原值 → 键完全相同 → 必须命中缓存。
        let mtime = fs::metadata(&path).unwrap().modified().unwrap();
        let text = fs::read_to_string(&path).unwrap();
        let swapped = text.replace("使用java写一个快速排序", "使用RUST写一个快速排序");
        assert_eq!(swapped.len(), text.len(), "夹具必须等长，否则大小变了就不是同一个键");
        fs::write(&path, &swapped).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(mtime))
            .unwrap();
        assert_eq!(first(), "使用java写一个快速排序", "① mtime 与大小都没变 → 必须命中缓存");

        // 把 mtime 往后拨 → 键变了 → 必须重新读，拿到新正文。
        let later = mtime + std::time::Duration::from_secs(60);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(later))
            .unwrap();
        assert_eq!(
            first(),
            "使用RUST写一个快速排序",
            "② mtime 变了必须失效 —— 否则用户继续对话后标题永远停在旧的那条"
        );
        let _ = fs::remove_dir_all(&home);
    }

    /// 🔴 项目归属只读、**绝不写** `.codex-global-state.json`。
    ///
    /// 那是 Electron 的 atom store（本机 24 个顶层键：窗口位置、onboarding 标记、
    /// **prompt 历史**…），Desktop 运行时整份持在内存里并按自己的节奏整份写回。
    /// 我们去写它就是「最后写的人赢」→ 可能把 Desktop 更新的状态整片抹掉，
    /// 用户会看到自己打过的 prompt 历史莫名消失。CodexPlusPlus 写它，我们刻意不写。
    ///
    /// 判据两半：① 读得出来（否则这一列恒空，功能等于没做）；
    /// ② 生产段里**不许**出现对那个文件的写入。
    #[test]
    fn project_assignment_is_read_but_never_written() {
        let home = tmp_home("proj");
        write_rollout(
            &home,
            "rollout-2026-09-01T21-06-47-01a05d14-4e5b-7773-b425-25ae029f078f.jsonl",
            "synaroute",
            USER_LINE,
        );
        fs::write(
            home.join(".codex-global-state.json"),
            r#"{
              "local-projects": {
                "419d94b9": { "id": "419d94b9", "name": "demo3", "rootPaths": ["C:\\work"] }
              },
              "thread-project-assignments": {
                "01a05d14-4e5b-7773-b425-25ae029f078f": { "projectKind": "local", "projectId": "419d94b9" },
                "unknown-thread": { "projectKind": "local", "projectId": "no-such-project" }
              },
              "prompt-history": { "global": ["用户逐字打过的话"] }
            }"#,
        )
        .unwrap();

        let mut rows = super::super::scan_at(&home).sessions;
        enrich(&home, &mut rows, "synaroute", &[]);
        assert_eq!(rows[0].project, "demo3", "项目名没读出来 —— 这一列会恒空");

        // 指向不存在项目的条目不许凭空造名字（那会显示一个用户认不出的字符串）。
        let names = project_names(&home);
        assert_eq!(names.get("unknown-thread"), None);

        // ② 形态判据：不许写那个文件。
        let prod = crate::proxy::custom_headers::production_code_only(include_str!(
            "codex_session_view.rs"
        ));
        assert!(prod.contains(".codex-global-state.json"), "读那一半必须在");
        for bad in ["fs::write", "File::create", "write_all"] {
            assert!(
                !prod.contains(bad),
                "本模块生产段出现了 `{bad}` —— 那个文件我们只读不写（理由见本测试文档）"
            );
        }
        let _ = fs::remove_dir_all(&home);
    }

    /// 文件不存在 / 是坏 JSON 时必须安静退化成「没有项目信息」，不能让列表出不来。
    #[test]
    fn a_missing_or_broken_global_state_is_not_an_error() {
        let home = tmp_home("nostate");
        assert!(project_names(&home).is_empty());
        fs::write(home.join(".codex-global-state.json"), b"{ not json").unwrap();
        assert!(project_names(&home).is_empty());
        let _ = fs::remove_dir_all(&home);
    }

    /// 正文扫描有字节上限：越过上限还没找到用户消息就放弃，不把几十 MB 读完。
    #[test]
    fn the_body_scan_gives_up_after_its_budget() {
        let home = tmp_home("budget");
        // 一条超过预算的 developer 消息，之后才是用户消息 —— 应当扫不到。
        let filler = format!(
            "{{\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"developer\",\"content\":[{{\"text\":\"{}\"}}]}}}}\n",
            "x".repeat(BODY_SCAN_BYTES as usize + 16)
        );
        let rel = write_rollout(
            &home,
            "rollout-2026-09-01T21-06-47-01a05d14-4e5b-7773-b425-25ae029f078f.jsonl",
            "openai",
            &format!("{filler}{USER_LINE}"),
        );
        assert_eq!(first_user_message(&home.join(&rel)), None, "超预算就该放弃");
        let _ = fs::remove_dir_all(&home);
    }
}
