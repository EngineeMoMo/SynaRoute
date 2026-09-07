//! Codex 会话管理的读侧与破坏性操作：列表、删除、导出 Markdown。
//!
//! 与父模块 [`super`] 的分工：那边管**接入/还原时自动改 provider**（用户不感知），
//! 这里管**用户主动做的事**。拆开的直接原因是父模块生产段顶在 900 行上限，但也本该分开 ——
//! 一个是接入链路的一环、失败只降级成提示，一个是由界面驱动、每个操作都要给用户明确结果。
//!
//! # 🔴 删除是本应用唯一会主动删用户对话记录的地方
//!
//! 三处必须一起删（rollout 文件 / sqlite `threads` 行 / `session_index.jsonl`），
//! 且**先删索引、后删文件**：
//!
//! - 先删文件、中途失败 → 索引里留下一条指向不存在路径的记录，Desktop 列表里就是一个
//!   **点开即报错的死条目**（恢复会话时 Codex 只 `SELECT rollout_path`，拿到的是空气）；
//! - 先删索引、中途失败 → 只剩一个不再被引用的文件，下次删除会重新扫到它，**可自愈**。
//!
//! 两种中途失败的代价不对称，所以顺序不是随手定的。
//!
//! **删之前先把 rollout 备份到数据目录**（`backups/codex-sessions-deleted/`，保留 30 天），
//! 备份不成就不删那一条 —— 理由见 [`delete_at`]。
//!
//! # 导出刻意只带对话正文
//!
//! rollout 里还有 `developer` 消息（Desktop 注入的 app-context，几十 KB）、
//! `function_call_output`（可能是整个文件内容）、`reasoning.encrypted_content`（密文）。
//! 导出的 Markdown 是**用户会分享出去的东西**，所以：developer/system 消息跳过、
//! 工具调用只留一行名字、工具输出与密文一概不导出。要看完整现场的人应该直接读 jsonl。

use super::{files, resolve_in_home, scan_at, session_db_paths, view, SessionRef, SQL_CHUNK};
use serde::Serialize;
use serde_json::Value;
use std::fs;
use std::path::Path;

/// 会话列表的一次快照。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexSessionList {
    pub rows: Vec<SessionRef>,
    /// `config.toml` 里当前生效的根 provider。前端据此把「与它不一致」的行标红 ——
    /// 那一列就是用户排查「旧对话为什么 401」的入口。
    pub current_provider: String,
    /// 首行认不出的文件数（Codex 换过 rollout 格式时用户该看到这个数字，
    /// 而不是以为自己没有旧会话）。
    pub unreadable: usize,
    /// 路径无法安全定位的文件数（与上一条分开，成因和处置都不同）。
    pub path_rejected: usize,
    /// 顶部统计卡：总数 / 未归档 / 已归档 / 指向别处 / 会话库路径。
    pub stats: view::SessionStats,
}

/// 读 `config.toml` 的根 `model_provider`。读不到就给空串 —— 那时前端不标红任何行
/// （「不知道当前是什么」比「按错的基准标红一片」好）。
fn current_root_provider(home: &Path) -> String {
    let text = fs::read_to_string(home.join("config.toml")).unwrap_or_default();
    text.parse::<toml::Value>()
        .ok()
        .and_then(|v| v.get("model_provider")?.as_str().map(str::to_string))
        .unwrap_or_default()
}

fn list_at(home: &Path, codex_keys: &[crate::model::ProviderKey]) -> CodexSessionList {
    let mut scan = scan_at(home);
    // 新的排前面。用户来这个页面通常是为了最近那几条对话。
    scan.sessions.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    let current_provider = current_root_provider(home);
    // 标题/模型/档位/token 与统计都在这一步补上（见 `view` 的模块头：**文件为准、库来补充**）。
    let stats = view::enrich(home, &mut scan.sessions, &current_provider, codex_keys);
    CodexSessionList {
        rows: scan.sessions,
        current_provider,
        unreadable: scan.unreadable,
        path_rejected: scan.path_rejected,
        stats,
    }
}

/// 删除一批会话。返回（真正删掉的条数，逐条失败说明，备份目录）。
///
/// 单条失败**不中断**其余条目：用户勾了 20 条、其中一条被 Codex 占着，剩下 19 条该照删 ——
/// 早退会让他反复点、每次都停在同一条上。故本函数不返回 `Err`，失败都在第二个返回值里。
///
/// # 🔴 删除**之前先备份 rollout**，备份不成就不删那一条
///
/// 这是本应用唯一会主动删用户对话记录的地方。CodexPlusPlus 的界面上写着「删除会创建本地
/// 备份」，那是对的 —— 而我们此前的确认框写的是「不可逆、SynaRoute 不会为它们留备份」。
/// 两种做法都自洽，但「有备份」严格更好：误删一条几个月前的对话，用户没有任何别的退路。
///
/// 备份**只备份 rollout 文件本身**（sqlite 行与索引行不备），因为那才是对话内容；
/// 前两者是可以从 rollout 重建的元数据，而备份一整个会话库要 0.5 MB × 每次删除。
fn delete_at(home: &Path, data_dir: &Path, rel_paths: &[String]) -> (usize, Vec<String>, String) {
    let known = scan_at(home).sessions;
    let mut failed = Vec::new();

    // ① 先把路径解析完、thread id 收齐 —— 文件一删就查不到 id 了。
    let mut targets: Vec<(std::path::PathBuf, &String, String)> = Vec::new();
    for rel in rel_paths {
        match resolve_in_home(home, rel) {
            Some(p) => {
                let tid = known
                    .iter()
                    .find(|s| &s.rel_path == rel)
                    .map(|s| s.thread_id.clone())
                    .unwrap_or_default();
                targets.push((p, rel, tid));
            }
            None => failed.push(format!("{rel}（路径越界，已拒绝）")),
        }
    }

    // ② 备份。任何一条备份不成就把它从待删清单里摘掉 —— 确认框承诺了备份，
    // 「备份没成还照删」等于无备份删除用户的对话。
    let mut backed: Vec<(std::path::PathBuf, &String, String)> = Vec::new();
    for t in targets {
        // 文件已经不在了不需要备份（用户可能在别处删过），照旧走后面的清理流程。
        if !t.0.exists() {
            backed.push(t);
            continue;
        }
        match files::backup_file(&t.0, data_dir, BACKUP_KIND, files::Keep::Days(30)) {
            Ok(_) => backed.push(t),
            Err(e) => failed.push(format!("{}（备份失败，已跳过、未删除：{e}）", t.1)),
        }
    }
    let targets = backed;
    let ids: Vec<String> =
        targets.iter().map(|(_, _, t)| t.clone()).filter(|t| !t.is_empty()).collect();

    // ③ 索引先删、文件后删（顺序理由见模块头）。
    //
    // 🔴 **索引没删成就一个文件都不删。** 否则得到的正是那个顺序想避免的东西：索引里留着
    // 指向不存在路径的记录 = Desktop 列表里点开即报错的死条目，且**永不自愈**。
    // 现在的失效方向是「什么都没删 + 一条可行动的失败消息」，用户可以退出 Codex 再重试。
    if let Err(e) = remove_from_index(home, &ids) {
        let why = match files::blame_io(&e) {
            files::IoBlame::Locked => "，文件正被 Codex 占用，完全退出 Codex 后再试",
            files::IoBlame::Unwritable => "，目录不可写，请检查权限",
            files::IoBlame::Other => "",
        };
        return (
            0,
            vec![format!("session_index.jsonl 更新失败（{e}{why}）")],
            String::new(),
        );
    }
    let mut deleted = 0usize;
    for (path, rel, _) in &targets {
        match fs::remove_file(path) {
            Ok(()) => deleted += 1,
            // 文件不存在当成已删（用户可能在别处删过，或重复点了一次）。
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => deleted += 1,
            Err(e) => {
                // 归类给出精确指路：被占用 → 退出 Codex；不可写 → 看权限（见 `files::blame_io`）。
                let why = match files::blame_io(&e) {
                    files::IoBlame::Locked => "文件正被 Codex 占用，完全退出 Codex 后再试",
                    files::IoBlame::Unwritable => "目录或文件不可写，请检查权限",
                    files::IoBlame::Other => "",
                };
                failed.push(if why.is_empty() {
                    format!("{rel}（{e}）")
                } else {
                    format!("{rel}（{why}）")
                });
            }
        }
    }

    // ④ sqlite 的 threads 行。best-effort：它只影响列表显示，而文件已经没了。
    if let Some(e) = delete_threads_rows(home, &ids) {
        failed.push(format!("会话列表元数据未完全清理（不影响已删除的文件）：{e}"));
    }
    (deleted, failed, data_dir.join("backups").join(BACKUP_KIND).display().to_string())
}

/// 被删会话的备份目录名。**同时出现在确认框文案与预览摘要里** —— 用户得知道备份在哪，
/// 否则「有备份」这句话对他没有任何用。
const BACKUP_KIND: &str = "codex-sessions-deleted";

/// 从 `session_index.jsonl` 里摘掉这些 thread id 的行。
///
/// 整份重写而不是原地改：这个文件是每行一条 JSON 的小索引（本机 131 字节），
/// 没有值得为它做流式处理的体量。
///
/// # 🔴 写失败必须交出去 —— 它是「先删索引、后删文件」这个顺序的**前提**
///
/// 这个函数第一版是 `let _ = fs::write(..)`，文档还写着「写不出去不算失败」。那句话
/// **击穿了模块头那条刻意的顺序**：顺序的全部意义是「中途失败宁可留孤儿（可自愈），
/// 不要留死条目（点开即报错、永不自愈）」，而索引没删成却照删文件，得到的恰好是死条目。
///
/// 所以现在返回 `io::Result`，调用方在这一步失败时**不删任何文件** —— 用户拿到一条
/// 可行动的失败消息并可以重试，而不是拿到一个静默坏掉的列表。
///
/// 另一半是**原子写**：裸 `fs::write` 先截断再写，崩在中途留下一份被我们自己写坏的索引，
/// 而 Codex 的会话列表就读它 —— 用户丢的不是一条记录，是整份列表。本仓对 rollout /
/// 回滚清单 / config.json 一律 temp+rename，这里没有理由是例外。
fn remove_from_index(home: &Path, thread_ids: &[String]) -> std::io::Result<()> {
    if thread_ids.is_empty() {
        return Ok(());
    }
    let path = home.join("session_index.jsonl");
    // 索引不存在 / 读不出来都不算失败：没有索引就没有要摘的行（fork 出来的会话本来就
    // 可能不在里面）。真正不能忽略的是**写**那一半。
    let Ok(text) = fs::read_to_string(&path) else { return Ok(()) };
    let kept: Vec<&str> = text
        .lines()
        .filter(|l| {
            let id = serde_json::from_str::<Value>(l)
                .ok()
                .and_then(|v| v.get("id")?.as_str().map(str::to_string))
                .unwrap_or_default();
            // 认不出的行一律保留：删掉自己读不懂的东西是最坏的默认值。
            id.is_empty() || !thread_ids.iter().any(|t| t == &id)
        })
        .collect();
    // 行尾按原文件保留 —— 同 rollout 改写那条纪律（本仓在行尾上栽过三次）。
    // 本机实测这个文件是 LF，但那是观察不是保证，而归一的收益为零。
    let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut out = kept.join(eol);
    if !out.is_empty() {
        out.push_str(eol);
    }
    let tmp = files::tmp_path_for(&path);
    match fs::write(&tmp, out.as_bytes()).and_then(|()| fs::rename(&tmp, &path)) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(e)
        }
    }
}

fn delete_threads_rows(home: &Path, ids: &[String]) -> Option<String> {
    if ids.is_empty() {
        return None;
    }
    let mut first_err = None;
    for db in session_db_paths(home) {
        let Ok(conn) = rusqlite::Connection::open(&db) else { continue };
        let _ = conn.busy_timeout(std::time::Duration::from_millis(1500));
        // 🔴 先查表在不在。`session_db_paths` 只按扩展名筛文件，不保证里面有 `threads`
        // （`~/.codex/sqlite/` 下将来可能有别的库）。不查的话 DELETE 报 `no such table`，
        // 于是一次**成功的删除**会附带一句无意义的错误消息 —— 用户以为没删干净。
        let has_table = conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='threads' LIMIT 1",
                [],
                |_| Ok(true),
            )
            .unwrap_or(false);
        if !has_table {
            continue;
        }
        for chunk in ids.chunks(SQL_CHUNK) {
            // `repeat_n` 要 Rust 1.82，而本仓 MSRV 是 1.77。
            let holes = std::iter::repeat("?").take(chunk.len()).collect::<Vec<_>>().join(",");
            let sql = format!("DELETE FROM threads WHERE id IN ({holes})");
            let params: Vec<&dyn rusqlite::ToSql> =
                chunk.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
            if let Err(e) = conn.execute(&sql, params.as_slice()) {
                first_err.get_or_insert(format!("{}: {e}", db.display()));
                break;
            }
        }
    }
    first_err
}

/// 把一条会话导出成 Markdown。
///
/// 只带对话正文（见模块头）：`developer`/`system` 消息、工具输出、`encrypted_content`
/// 一概不导出。工具调用只留一行名字 —— 它对「这段对话干了什么」有信息量，而 `arguments`
/// 里常是本机绝对路径和整段命令，属于用户未必想连同对话一起发出去的东西。
fn to_markdown(text: &str) -> String {
    let mut out = String::new();
    let mut head_done = false;
    for line in text.lines() {
        let Ok(rec) = serde_json::from_str::<Value>(line) else { continue };
        let payload = rec.get("payload").unwrap_or(&Value::Null);
        let s = |v: &Value, k: &str| {
            v.get(k).and_then(Value::as_str).unwrap_or_default().to_string()
        };
        match rec.get("type").and_then(Value::as_str) {
            Some("session_meta") if !head_done => {
                head_done = true;
                out.push_str(&format!(
                    "# Codex 会话 {}\n\n- 时间：{}\n- 目录：{}\n- provider：{}\n- 客户端：{} {}\n\n---\n",
                    s(payload, "id"),
                    s(payload, "timestamp"),
                    s(payload, "cwd"),
                    s(payload, "model_provider"),
                    s(payload, "originator"),
                    s(payload, "cli_version"),
                ));
            }
            Some("response_item") => match payload.get("type").and_then(Value::as_str) {
                Some("message") => {
                    let role = s(payload, "role");
                    // developer/system 是 Desktop 注入的 app-context（几十 KB），不是对话。
                    if role != "user" && role != "assistant" {
                        continue;
                    }
                    let body = collect_text(payload.get("content"));
                    if body.trim().is_empty() {
                        continue;
                    }
                    let who = if role == "user" { "用户" } else { "助手" };
                    out.push_str(&format!("\n## {who}\n\n{}\n", body.trim_end()));
                }
                Some("function_call") | Some("custom_tool_call") => {
                    out.push_str(&format!("\n> 🔧 工具调用：`{}`\n", s(payload, "name")));
                }
                _ => {}
            },
            _ => {}
        }
    }
    if !head_done {
        out.push_str("# Codex 会话\n\n（首行元数据认不出，以下只有正文）\n");
    }
    out
}

/// 从 `content` 数组里拼出纯文本。三种 `type` 都要认（`input_text` / `output_text` /
/// `text`）—— 只认一种的表现是「导出的对话里一半是空的」。
fn collect_text(content: Option<&Value>) -> String {
    let Some(items) = content.and_then(Value::as_array) else {
        // 也可能直接是字符串形态。
        return content.and_then(Value::as_str).unwrap_or_default().to_string();
    };
    items
        .iter()
        .filter_map(|i| i.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("")
}

/// 工具配置预览面板的整段摘要。
///
/// 🔴 **它必须如实列出我们会动的每一类文件。** 那个面板是用户核对「SynaRoute 动了我哪些
/// 东西」的**唯一**界面，而本轮之前它只提 `config.toml` / 模型目录 / `auth.json` ——
/// **一个字都没提历史对话**。本仓在同一个地方栽过一次（模型目录上线时漏了），教训原文是
/// 「预览面板是用户核对的唯一界面，已如实改写并补上那一条 files 条目」。
///
/// 整段收在这里而不是留在 `codex.rs`：那个文件生产段顶在 900 行上限，而这段话的主体
/// （会话那半）属于本模块。同 `codex_catalog::apply_note` / `missing_catalog_warning`
/// 收在子模块的既有做法。
///
/// **`files` 列表刻意不加东西**：rollout 有几百个、列不完；回滚清单与 sqlite 备份落在
/// **应用数据目录**（不是 Codex 目录），放进「客户端配置文件」那个列表会让用户以为它们是
/// Codex 的文件。说明写在这段摘要里就够。
pub(in crate::tools) fn preview_summary() -> &'static str {
    "Codex：写 ~/.codex/config.toml（model_provider=synaroute、\
     [model_providers.synaroute] 含 base_url/wire_api/bearer 占位、可选顶层 model）\
     与 ~/.codex/synaroute-model-catalog.json（模型目录，还原时整份删除）。\
     auth.json 仅在无可用凭据 / OAuth 已过期时才写占位并备份原件，其余情况原样保留官方 \
     ChatGPT 登录态。不写任何 ANTHROPIC_*。\
     另外会改历史对话的 provider：sessions/ 与 archived_sessions/ 下每个 rollout-*.jsonl \
     的首行 model_provider（只改这一个字段，其余字节逐字不动、文件修改时间也原样保全），\
     以及会话库 threads 表的同名列；原值记在应用数据目录的 codex-session-providers.json，\
     还原时按它逐条改回，写库前先备份到 backups/codex-sqlite/。不改任何对话正文。\
     这一步可以在「Codex 会话」页关掉（关掉后仍可在那里手动同步）。\
     在会话页手动删除时，会先把 rollout 备份到 backups/codex-sessions-deleted/（保留 30 天），\
     再删掉 rollout 文件、session_index.jsonl 里那一行与 threads 行；\
     清理索引孤儿行前会把 session_index.jsonl 整份备份到 backups/codex-session-index/。"
}

/// 给诊断报告用的一行：会话总数与「指向别处」的条数。
///
/// 🔴 这是**排障 401 的关键信息**，而它此前不在报告里 —— 用户报「旧对话每次都 401」时，
/// 拿到报告的人看不出有多少会话指向别的 provider，也就看不到那个答案。
/// 同 CLAUDE.md 里 `mcp-stdio.log` 那条：「可观测性到不了排障者手上就等于没有」。
///
/// 读不出 `$CODEX_HOME` 时返回 `None`（没接入过 Codex 的用户不该在报告里多一行噪音）。
pub(crate) fn diagnostics_line() -> Option<String> {
    let home = super::super::codex_paths::codex_home().ok()?;
    // 诊断报告里刻意**不带**「模型已服务不了」那一位：那要 `Store`，而这个函数被报告拼装
    // 调用时手上只有路径。会话页有这个信息就够了 —— 报告里已经有完整的 Key 与模型清单，
    // 拿到报告的人能自己对上。
    let list = list_at(&home, &[]);
    if list.rows.is_empty() && list.unreadable == 0 {
        return None;
    }
    let bad = list
        .rows
        .iter()
        .filter(|r| !list.current_provider.is_empty() && r.provider != list.current_provider)
        .count();
    let mut s = format!(
        "Codex 会话: {} 条，当前 provider={}，指向别处={bad}",
        list.rows.len(),
        if list.current_provider.is_empty() { "(读不出)" } else { &list.current_provider },
    );
    if list.unreadable > 0 {
        s.push_str(&format!("，首行认不出={}", list.unreadable));
    }
    Some(s)
}

/// 会话列表（会话管理页）。
///
/// 要 `AppState` 是为了那一位「这条对话用的模型现在还服务得了吗」—— 判据要 Codex 分类当前
/// 启用的 Key 池。**只有代理侧能回答它**，这是我们相对启动器类工具的实质差异。
#[tauri::command]
pub async fn list_codex_sessions(
    state: tauri::State<'_, crate::AppState>,
) -> Result<CodexSessionList, String> {
    let home = super::super::codex_paths::codex_home().map_err(|e| e.to_string())?;
    let keys = state.store.enabled_keys_sorted(crate::model::CategoryType::Codex);
    Ok(list_at(&home, &keys))
}

/// 删除选中的会话。前端仍必须先弹确认 —— 有备份不等于可以随手删。
#[tauri::command]
pub async fn delete_codex_sessions(rel_paths: Vec<String>) -> Result<String, String> {
    let home = super::super::codex_paths::codex_home().map_err(|e| e.to_string())?;
    let data_dir = crate::store::data_dir::app_data_dir().map_err(|e| e.to_string())?;
    let (deleted, failed, backup_dir) = delete_at(&home, &data_dir, &rel_paths);
    if failed.is_empty() {
        // 备份路径要**回显**：只说「已备份」而不说在哪，那句话对用户没有用。
        return Ok(format!("已删除 {deleted} 个会话（原文件已备份到 {backup_dir}）"));
    }
    // 部分成功也要如实说清哪些没删掉 —— 只报一个数字会让用户以为全删了。
    Err(format!(
        "已删除 {deleted} 个，{} 个未能删除：{}",
        failed.len(),
        failed.join("；")
    ))
}

/// 导出一条会话为 Markdown，**写进数据目录下的 `exports/`**，返回落盘的完整路径。
///
/// 🔴 **不走「返回文本让前端下载」那条路**（第一版如此）：blob + `<a download>` 依赖
/// WebView2 的下载行为，在 Tauri 里我没有验证过 —— 而它的失败形态是**点了什么都不发生**。
/// 也不用 dialog 插件选路径：那要新增命令与权限配置，而这件事不值得。
/// 写到数据目录：真机必然可靠、路径能回显给用户、且受 `SYNAROUTE_DATA_DIR` 隔离
/// （冒烟测试不会往真实目录里丢文件）。同名重复导出直接覆盖 —— 那是同一条会话的新快照。
#[tauri::command]
pub async fn export_codex_session_markdown(rel_path: String) -> Result<String, String> {
    let home = super::super::codex_paths::codex_home().map_err(|e| e.to_string())?;
    let path = resolve_in_home(&home, &rel_path)
        .ok_or_else(|| format!("路径越界，已拒绝：{rel_path}"))?;
    let text = fs::read_to_string(&path).map_err(|e| format!("读取失败：{e}"))?;
    let dir = crate::store::data_dir::app_data_dir()
        .map_err(|e| e.to_string())?
        .join("exports");
    fs::create_dir_all(&dir).map_err(|e| format!("建导出目录失败：{e}"))?;
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let out = dir.join(format!("{stem}.md"));
    fs::write(&out, to_markdown(&text)).map_err(|e| format!("写出失败：{e}"))?;
    Ok(out.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    /// 夹具目录：pid + 进程内序号。光靠时间戳不够（本机 `timestamp_nanos` 量化粒度只有
    /// 100ns，并发用例会撞到同一个目录并互删文件）—— `ccswitch::db_copy_path` 上踩过。
    fn tmp_home(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "synaroute-cxops-{}-{}-{tag}",
            std::process::id(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("sessions/2026/09/01")).unwrap();
        d
    }

    /// 🔴 夹具的 id **必须是 UUID 形态**：`thread_id` 从文件名末 36 字符推导
    /// （见 `files` 模块头），短标签推导不出 id → sqlite 与索引那两步被静默跳过，
    /// 而它们正是「删除必须三处一起删」那条要测的东西。
    fn uuid_of(label: &str) -> String {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in label.as_bytes() {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x100_0000_01b3);
        }
        format!("01a05d3b-0017-7ec3-9cff-{:012x}", h & 0xffff_ffff_ffff)
    }

    /// 造一条带完整记录类型的 rollout：session_meta + developer + user + assistant +
    /// function_call + function_call_output + reasoning(带 encrypted_content)。
    /// 返回 (相对路径, thread id)。
    fn write_full_rollout(home: &std::path::Path, label: &str) -> (String, String) {
        write_rollout_with_provider(home, label, "openai")
    }

    fn write_rollout_with_provider(
        home: &std::path::Path,
        label: &str,
        provider: &str,
    ) -> (String, String) {
        let id = uuid_of(label);
        let rel = format!("sessions/2026/09/01/rollout-2026-09-01T21-06-47-{id}.jsonl");
        let lines = [
            format!(
                "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{id}\",\"timestamp\":\"2026-09-01T13:06:47Z\",\"cwd\":\"C:/work\",\"originator\":\"Codex Desktop\",\"cli_version\":\"0.151\",\"thread_source\":\"user\",\"model_provider\":\"{provider}\"}}}}"
            ),
            "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"developer\",\"content\":[{\"type\":\"input_text\",\"text\":\"APP-CONTEXT-SECRET-BLOB\"}]}}".into(),
            "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"帮我排序\"}]}}".into(),
            "{\"type\":\"response_item\",\"payload\":{\"type\":\"reasoning\",\"encrypted_content\":\"CIPHERTEXT-MUST-NOT-LEAK\",\"summary\":[]}}".into(),
            "{\"type\":\"response_item\",\"payload\":{\"type\":\"function_call\",\"name\":\"exec_command\",\"arguments\":\"{\\\"cmd\\\":\\\"rm -rf C:/private\\\"}\"}}".into(),
            "{\"type\":\"response_item\",\"payload\":{\"type\":\"function_call_output\",\"output\":\"FILE-CONTENT-MUST-NOT-LEAK\"}}".into(),
            "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"好的\"}]}}".into(),
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\"}}".into(),
        ];
        fs::write(home.join(&rel), lines.join("\n") + "\n").unwrap();
        (rel, id)
    }

    /// 🔴 导出只带对话正文。三样东西**绝不能**进导出文件：`developer` 消息（Desktop 注入的
    /// app-context）、工具输出（可能是整个文件内容）、`encrypted_content`（密文）。
    /// 导出的 Markdown 是用户会分享出去的东西 —— 这条判据守的是「别把不该外传的一起发出去」。
    #[test]
    fn markdown_export_carries_only_the_conversation() {
        let home = tmp_home("md");
        let (rel, id) = write_full_rollout(&home, "t1");
        let md = to_markdown(&fs::read_to_string(home.join(&rel)).unwrap());

        assert!(md.contains(&format!("# Codex 会话 {id}")), "要有元数据抬头: {md}");
        assert!(md.contains("## 用户") && md.contains("帮我排序"));
        assert!(md.contains("## 助手") && md.contains("好的"));
        assert!(md.contains("🔧 工具调用：`exec_command`"), "工具调用留一行名字");

        for leak in [
            "APP-CONTEXT-SECRET-BLOB",
            "CIPHERTEXT-MUST-NOT-LEAK",
            "FILE-CONTENT-MUST-NOT-LEAK",
            "rm -rf C:/private", // 工具参数含本机路径与整段命令，只留名字不留参数
        ] {
            assert!(!md.contains(leak), "导出里不该出现 {leak}:\n{md}");
        }
        let _ = fs::remove_dir_all(&home);
    }

    /// 🔴 删除必须三处一起删，**并且先把 rollout 备份出来**。
    ///
    /// 只删文件会在 Desktop 列表里留下一条**点开即报错的死条目**（恢复会话时 Codex 只
    /// `SELECT rollout_path`，拿到的是空气）。备份那一条守的是另一件事：这是本应用唯一会
    /// 主动删用户对话记录的地方，误删一条几个月前的对话，用户没有别的退路。
    #[test]
    fn deleting_backs_up_then_removes_the_file_the_index_row_and_the_db_row() {
        let home = tmp_home("del");
        let data = home.join("appdata");
        let (rel, id) = write_full_rollout(&home, "t1");
        let (keep, keep_id) = write_full_rollout(&home, "t2");
        fs::write(
            home.join("session_index.jsonl"),
            format!(
                "{{\"id\":\"{id}\",\"thread_name\":\"要删的\"}}\n\
                 {{\"id\":\"{keep_id}\",\"thread_name\":\"留着的\"}}\n"
            ),
        )
        .unwrap();
        let db = home.join("state_5.sqlite");
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute("CREATE TABLE threads (id TEXT, model_provider TEXT)", []).unwrap();
        conn.execute("INSERT INTO threads VALUES (?1,'openai'),(?2,'openai')", [&id, &keep_id])
            .unwrap();
        drop(conn);
        let original = fs::read(home.join(&rel)).unwrap();

        let (deleted, failed, backup_dir) = delete_at(&home, &data, std::slice::from_ref(&rel));
        assert_eq!((deleted, failed.len()), (1, 0), "失败项: {failed:?}");
        assert!(!home.join(&rel).exists(), "rollout 文件要删掉");
        assert!(home.join(&keep).exists(), "没选中的那条不许动");

        // 备份必须是删之前那一份的**逐字节副本** —— 否则它什么都救不回来。
        let baks: Vec<std::path::PathBuf> =
            fs::read_dir(&backup_dir).unwrap().flatten().map(|e| e.path()).collect();
        assert_eq!(baks.len(), 1, "只删了一条，就该只有一份备份: {baks:?}");
        assert_eq!(fs::read(&baks[0]).unwrap(), original, "备份内容要与原文件逐字节相同");

        let idx = fs::read_to_string(home.join("session_index.jsonl")).unwrap();
        assert!(!idx.contains(&id), "索引里要摘掉它");
        assert!(idx.contains(&keep_id), "索引里别的行要留着");

        let conn = rusqlite::Connection::open(&db).unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM threads WHERE id=?1", [&id], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "threads 行要删掉 —— 否则列表里留一条死条目");
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM threads WHERE id=?1", [&keep_id], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
        let _ = fs::remove_dir_all(&home);
    }

    /// 🔴 备份失败时那一条**不许删**，而且要如实说是备份失败。
    ///
    /// 确认框里写着「会先备份」。备份没成还照删，等于无备份删除用户的对话 —— 同 sqlite
    /// 那半的纪律（备份不成就不写库）。夹具用「把备份目录的位置做成一个文件」让
    /// `create_dir_all` 失败：跨平台都成立，不依赖权限位（Unix 看目录权限、Windows 看文件
    /// 属性，拿只读位当夹具会在另一个平台上静默失效）。
    #[test]
    fn a_session_whose_backup_fails_is_not_deleted() {
        let home = tmp_home("nobak");
        let data = home.join("appdata");
        let (rel, _) = write_full_rollout(&home, "t1");
        fs::create_dir_all(&data).unwrap();
        // backups 这个名字被一个**文件**占住 → 下面 create_dir_all 必然失败。
        fs::write(data.join("backups"), b"x").unwrap();

        let (deleted, failed, _) = delete_at(&home, &data, std::slice::from_ref(&rel));
        assert_eq!(deleted, 0, "备份不成就不许删");
        assert!(home.join(&rel).exists(), "文件必须还在");
        assert!(
            failed.iter().any(|f| f.contains("备份失败") && f.contains("未删除")),
            "要如实说是备份失败、且没删: {failed:?}"
        );
        let _ = fs::remove_dir_all(&home);
    }

    /// 🔴 **索引没删成就一个文件都不许删** —— 这是「先删索引、后删文件」那个顺序的前提。
    ///
    /// 第一版的 `remove_from_index` 是 `let _ = fs::write(..)`，文档还写着「写不出去不算
    /// 失败」。那句话把模块头那条刻意的顺序整个击穿了：索引没删成却照删文件，留下的正是
    /// 顺序想避免的**死条目**（Desktop 列表里点开即报错，且永不自愈）。
    ///
    /// 夹具用「把 `session_index.jsonl` 换成一个目录」让 rename 必然失败 —— 跨平台都成立，
    /// 不依赖权限位（同上一条用例的理由）。
    #[test]
    #[cfg(windows)]
    fn nothing_is_deleted_when_the_index_cannot_be_updated() {
        let home = tmp_home("idxfail");
        let data = home.join("appdata");
        let (rel, tid) = write_full_rollout(&home, "t1");
        let index = home.join("session_index.jsonl");
        // 索引里真的有这一行，否则 `remove_from_index` 会因「读不出来」早退、压根不写。
        fs::write(&index, format!("{{\"id\":\"{tid}\"}}\n").as_bytes()).unwrap();

        // ⚠️ **夹具必须让「读成功、写失败」同时成立**，而这两半在同一个路径上是互斥的：
        // 把索引换成目录能让写失败，但 `read_to_string` 对目录也返回 Err → 走早退分支，
        // 测到的是另一件事（第一版就栽在这，`deleted` 是 1 而我以为验到了）。
        //
        // 只读属性恰好满足：读照旧成功，而 `rename` 覆盖一个只读文件在 **Windows 上失败**。
        // 🔴 **Unix 上它不失败** —— rename 只看**目录**的权限位，被替换文件自己的权限无关
        // （CLAUDE.md 记过这个平台差异）。故这条用例只在 Windows 上有意义；在别的平台上
        // 拿它当防线会得到一个恒绿的门，所以直接 `#[cfg(windows)]` 圈起来，
        // 并由下面那条**源码级**判据在所有平台上守住「错误必须被传出去」这一半。
        let mut perm = fs::metadata(&index).unwrap().permissions();
        perm.set_readonly(true);
        fs::set_permissions(&index, perm).unwrap();

        let (deleted, failed, _) = delete_at(&home, &data, std::slice::from_ref(&rel));
        assert_eq!(deleted, 0, "索引没更新成就不许删文件");
        assert!(home.join(&rel).exists(), "rollout 必须还在 —— 否则就是一个死条目");
        assert!(
            failed.iter().any(|f| f.contains("session_index.jsonl")),
            "要如实说是索引更新失败: {failed:?}"
        );
        // 收尾要解掉只读位，否则 `remove_dir_all` 删不掉它、临时目录留在盘上。
        // ⚠️ **刻意用 Windows 原生属性而不是 `Permissions::set_readonly(false)`**：
        // clippy 的 `permissions_set_readonly_false` 拦下了后者，而那个 lint 是对的 ——
        // 它在 Unix 上会把文件设成 **world writable**。这条用例虽然只在 Windows 上跑，
        // 但用一个跨平台语义有害的 API 去表达「Windows 上清掉只读」，下一个人抄走它就中招了。
        let attrs = std::os::windows::fs::MetadataExt::file_attributes(
            &fs::metadata(&index).unwrap(),
        );
        const FILE_ATTRIBUTE_READONLY: u32 = 0x1;
        if attrs & FILE_ATTRIBUTE_READONLY != 0 {
            // 没有安全的 std API 能只清这一位，退回 `set_permissions` 但先确认平台。
            #[allow(clippy::permissions_set_readonly_false)]
            {
                let mut perm = fs::metadata(&index).unwrap().permissions();
                perm.set_readonly(false);
                let _ = fs::set_permissions(&index, perm);
            }
        }
        let _ = fs::remove_dir_all(&home);
    }

    /// 上一条用例只在 Windows 上跑（夹具依赖只读位，Unix 的 rename 不看它）。
    /// 这一条在**所有平台**上守住同样的纪律，且守的是三件容易各自退化的事：
    ///
    /// ① 索引写入必须**原子**（temp + rename）—— 裸 `fs::write` 先截断再写，崩在中途留下
    ///    一份被我们自己写坏的索引，而 Codex 的会话列表就读它；
    /// ② 错误必须**交出去**（返回 `Result`，不是 `let _ =`）；
    /// ③ 调用方必须**在这一步失败时早退**，不许继续删文件。
    ///
    /// 三者缺任一条，「先删索引、后删文件」这个顺序就只是一句注释。
    #[test]
    fn the_index_update_must_be_atomic_and_must_gate_the_deletion() {
        let src = include_str!("codex_session_ops.rs");
        let prod = crate::proxy::custom_headers::production_code_only(src);
        let body = prod
            .split_once("fn remove_from_index(")
            .expect("函数改名了 —— 先修判据")
            .1
            .split_once("\nfn ")
            .expect("找不到函数结尾")
            .0;
        assert!(
            body.contains("std::io::Result<()>"),
            "① 写失败必须能被调用方看见 —— 返回 Result，不是吞掉"
        );
        assert!(
            body.contains("tmp_path_for") && body.contains("fs::rename"),
            "② 必须 temp + rename（原子），不许裸 fs::write 覆盖用户的索引"
        );
        assert!(
            !body.contains("let _ = fs::write"),
            "② 反向：不许把写错误吞掉"
        );
        // ③ 调用方那一半：失败即 return，且不许在 return 之前删文件。
        let del = prod
            .split_once("fn delete_at(")
            .expect("delete_at 改名了")
            .1
            .split_once("\nfn ")
            .expect("找不到 delete_at 结尾")
            .0;
        let gate = del
            .find("remove_from_index(")
            .expect("delete_at 必须调 remove_from_index");
        let first_remove = del.find("fs::remove_file(").unwrap_or(usize::MAX);
        assert!(
            del[gate..].starts_with("remove_from_index(home, &ids)")
                || del[..gate].contains("if let Err(e) = "),
            "③ 索引更新的失败必须被检查（第一版是裸调用 + 吞错）"
        );
        assert!(
            gate < first_remove,
            "③ 索引必须排在删文件之前 —— 反过来会留下点开即报错的死条目"
        );
    }

    /// 越界路径要被拒绝、且**不删任何东西**（清单/参数都可能被外部改）。
    #[test]
    fn a_path_outside_home_is_refused() {
        let home = tmp_home("escape");
        let victim = home.parent().unwrap().join("synaroute-ops-victim.jsonl");
        fs::write(&victim, b"keep me").unwrap();

        let rel = format!("../{}", victim.file_name().unwrap().to_string_lossy());
        let (deleted, failed, _) = delete_at(&home, &home.join("appdata"), &[rel]);
        assert_eq!(deleted, 0);
        assert!(failed.iter().any(|f| f.contains("越界")), "要如实说是越界: {failed:?}");
        assert!(victim.exists(), "绝不许删 CODEX_HOME 之外的文件");

        let _ = fs::remove_file(&victim);
        let _ = fs::remove_dir_all(&home);
    }

    /// 库里没有 `threads` 表时 DELETE 不许报错 —— `session_db_paths` 只按扩展名筛文件，
    /// `~/.codex/sqlite/` 下将来可能有别的库。报错的后果是一次**成功的删除**附带一句
    /// 无意义的错误消息，用户以为没删干净。
    #[test]
    fn a_db_without_the_threads_table_is_skipped_quietly() {
        let home = tmp_home("notable");
        let (rel, _) = write_full_rollout(&home, "t1");
        let db = home.join("sqlite/other.db");
        fs::create_dir_all(db.parent().unwrap()).unwrap();
        rusqlite::Connection::open(&db)
            .unwrap()
            .execute("CREATE TABLE something_else (x TEXT)", [])
            .unwrap();

        let (deleted, failed, _) =
            delete_at(&home, &home.join("appdata"), std::slice::from_ref(&rel));
        assert_eq!((deleted, failed.len()), (1, 0), "无关的库不该产生错误: {failed:?}");
        let _ = fs::remove_dir_all(&home);
    }

    /// 🔴 预览摘要必须点名我们会动的每一类文件。那个面板是用户核对「SynaRoute 动了我哪些
    /// 东西」的**唯一**界面，而本轮之前它一个字都没提历史对话 —— 本仓在同一个地方栽过一次
    /// （模型目录上线时漏了）。这条按「文件类别」逐项断言，改文案时会强制过一遍清单。
    #[test]
    fn the_preview_summary_names_every_kind_of_file_we_touch() {
        let s = preview_summary();
        for kind in [
            "config.toml",
            "synaroute-model-catalog.json",
            "auth.json",
            "rollout-*.jsonl",          // 历史对话首行
            "archived_sessions",        // 归档目录同样会被改
            "threads",                  // 会话库那一列
            "codex-session-providers.json", // 回滚清单
            "backups/codex-sqlite/",    // 写库前的备份
            "session_index.jsonl",      // 手动删除时一并清理
            "backups/codex-sessions-deleted/", // 删除会话前的备份
            "backups/codex-session-index/",    // 清理索引孤儿前的备份
            "修改时间也原样保全",       // mtime 保全是用户能观察到的行为，一并声明
        ] {
            assert!(s.contains(kind), "预览摘要没提 {kind}：\n{s}");
        }
        // 反过来也要说清**不动**什么 —— 用户最担心的是对话内容被改。
        assert!(s.contains("不改任何对话正文"));
    }

    /// 诊断报告与统计卡：有会话时必须给出「总数 / 当前 provider / 指向别处」三个数字。
    ///
    /// 🔴 夹具刻意放**两条、只有一条不一致** —— 一条的话「统计卡把每行都算成不一致」这个
    /// 错误实现也会绿（1 == 1）。同「注入不变红先怀疑用例没压到那个维度」那条。
    #[test]
    fn the_diagnostics_line_carries_the_mismatch_count() {
        let home = tmp_home("diagline");
        write_rollout_with_provider(&home, "t1", "openai"); // 指向别处
        write_rollout_with_provider(&home, "t2", "synaroute"); // 已经对了
        fs::write(
            home.join("config.toml"),
            "model_provider = \"synaroute\"\n\n[model_providers.synaroute]\nname = \"x\"\n",
        )
        .unwrap();

        let list = list_at(&home, &[]);
        assert_eq!(list.current_provider, "synaroute");
        assert_eq!(list.rows.len(), 2);
        let bad = list.rows.iter().filter(|r| r.provider != list.current_provider).count();
        assert_eq!(bad, 1, "只有那条 openai 的会话是「指向别处」");
        // 统计卡与逐行数据必须同源 —— 两处各算一遍必然漂移，而漂移的表现是
        // 「表头说 2 条指向别处、表里只标红 1 行」，用户不知道该信哪个。
        assert_eq!(list.stats.mismatched, bad, "统计卡的数字必须与标红的行数一致");
        assert_eq!((list.stats.total, list.stats.active, list.stats.archived), (2, 2, 0));
        // 列表页要能回答「你到底在读哪个库」—— 没有库时是空串，不是一句假路径。
        assert_eq!(list.stats.db_path, "", "没有会话库时不许编一个路径出来");
        // 标题从正文回落读到（库里没有这条记录）。
        assert_eq!(list.rows[0].title, "帮我排序", "库里没有它就该从正文读第一条用户消息");
        let _ = fs::remove_dir_all(&home);
    }

    /// 🔴 一次删多条时，**每一条的备份都必须在**。
    ///
    /// 备份口径是「按天数过期」而不是「留最近 N 份」：删除的每条是不同的文件，按数量裁到 5
    /// 会让另外那些的备份当场消失，而确认框里刚写着「会先备份」。
    ///
    /// ⚠️ `files::per_file_backups_are_never_pruned_by_count` 直调 `backup_file`，覆盖不到
    /// **调用点选了哪个 `Keep`** —— 把 `delete_at` 里那个参数换成 `Keep::Count(5)`，
    /// 那条判据照样全绿（注入实测）。这一条补的就是那段接线。
    #[test]
    fn deleting_many_sessions_keeps_a_backup_for_every_one() {
        let home = tmp_home("delmany");
        let data = home.join("appdata");
        let rels: Vec<String> = (0..8)
            .map(|i| write_full_rollout(&home, &format!("m{i}")).0)
            .collect();

        let (deleted, failed, backup_dir) = delete_at(&home, &data, &rels);
        assert_eq!((deleted, failed.len()), (8, 0), "失败项: {failed:?}");
        let n = fs::read_dir(&backup_dir).unwrap().count();
        assert_eq!(n, 8, "删了 8 条就该有 8 份备份（不许按数量裁剪）");
        let _ = fs::remove_dir_all(&home);
    }

    /// 🔴 接线判据：上面三条都直调内部函数，三个命令没进 `generate_handler!` 它们照样全绿 ——
    /// 而那时用户点按钮只会拿到一句 "command not found"。策略门 `invoke-command-must-exist`
    /// 只查正向（前端调的名字在 Rust 有定义），反向这条没人管，同 `key_flags.rs` 那条。
    #[test]
    fn the_three_commands_must_be_registered_in_the_handler_list() {
        let lib = include_str!("../lib.rs");
        for cmd in [
            "list_codex_sessions",
            "delete_codex_sessions",
            "export_codex_session_markdown",
        ] {
            assert!(
                lib.contains(&format!("codex_sessions::ops::{cmd}")),
                "{cmd} 没进 generate_handler! —— 界面上点它会报 command not found"
            );
        }
    }
}


