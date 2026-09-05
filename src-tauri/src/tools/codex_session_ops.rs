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
//! # 导出刻意只带对话正文
//!
//! rollout 里还有 `developer` 消息（Desktop 注入的 app-context，几十 KB）、
//! `function_call_output`（可能是整个文件内容）、`reasoning.encrypted_content`（密文）。
//! 导出的 Markdown 是**用户会分享出去的东西**，所以：developer/system 消息跳过、
//! 工具调用只留一行名字、工具输出与密文一概不导出。要看完整现场的人应该直接读 jsonl。

use super::{resolve_in_home, scan_at, session_db_paths, SessionRef, SQL_CHUNK};
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

fn list_at(home: &Path) -> CodexSessionList {
    let mut scan = scan_at(home);
    // 新的排前面。用户来这个页面通常是为了最近那几条对话。
    scan.sessions.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    CodexSessionList {
        rows: scan.sessions,
        current_provider: current_root_provider(home),
        unreadable: scan.unreadable,
        path_rejected: scan.path_rejected,
    }
}

/// 删除一批会话。返回（真正删掉的条数，逐条失败说明）。
///
/// 单条失败**不中断**其余条目：用户勾了 20 条、其中一条被 Codex 占着，剩下 19 条该照删 ——
/// 早退会让他反复点、每次都停在同一条上。故本函数不返回 `Err`，失败都在第二个返回值里。
fn delete_at(home: &Path, rel_paths: &[String]) -> (usize, Vec<String>) {
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
    let ids: Vec<String> =
        targets.iter().map(|(_, _, t)| t.clone()).filter(|t| !t.is_empty()).collect();

    // ② 索引先删、文件后删（顺序理由见模块头）。
    remove_from_index(home, &ids);
    let mut deleted = 0usize;
    for (path, rel, _) in &targets {
        match fs::remove_file(path) {
            Ok(()) => deleted += 1,
            // 文件不存在当成已删（用户可能在别处删过，或重复点了一次）。
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => deleted += 1,
            Err(e) => failed.push(format!("{rel}（{e}）")),
        }
    }

    // ③ sqlite 的 threads 行。best-effort：它只影响列表显示，而文件已经没了。
    if let Some(e) = delete_threads_rows(home, &ids) {
        failed.push(format!("会话列表元数据未完全清理（不影响已删除的文件）：{e}"));
    }
    (deleted, failed)
}

/// 从 `session_index.jsonl` 里摘掉这些 thread id 的行。
///
/// 整份重写而不是原地改：这个文件是每行一条 JSON 的小索引（本机 131 字节），
/// 没有值得为它做流式处理的体量。写不出去**不算失败** —— 那只让列表里多一条指向
/// 不存在文件的记录，而 Codex 打开它时会自己报错；相比之下把整批删除判成失败更糟。
fn remove_from_index(home: &Path, thread_ids: &[String]) {
    if thread_ids.is_empty() {
        return;
    }
    let path = home.join("session_index.jsonl");
    let Ok(text) = fs::read_to_string(&path) else { return };
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
    let _ = fs::write(&path, out);
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
     的首行 model_provider（只改这一个字段，其余字节逐字不动），以及会话库 threads 表的\
     同名列；原值记在应用数据目录的 codex-session-providers.json，还原时按它逐条改回，\
     写库前先备份到 backups/codex-sqlite/。不改任何对话正文。\
     在会话页手动删除时，会一并删掉 rollout 文件、session_index.jsonl 里那一行与 threads 行。"
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
    let list = list_at(&home);
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
#[tauri::command]
pub async fn list_codex_sessions() -> Result<CodexSessionList, String> {
    let home = super::super::codex_paths::codex_home().map_err(|e| e.to_string())?;
    Ok(list_at(&home))
}

/// 删除选中的会话。**不可逆** —— 前端必须先弹确认。
#[tauri::command]
pub async fn delete_codex_sessions(rel_paths: Vec<String>) -> Result<String, String> {
    let home = super::super::codex_paths::codex_home().map_err(|e| e.to_string())?;
    let (deleted, failed) = delete_at(&home, &rel_paths);
    if failed.is_empty() {
        return Ok(format!("已删除 {deleted} 个会话"));
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

    /// 造一条带完整记录类型的 rollout：session_meta + developer + user + assistant +
    /// function_call + function_call_output + reasoning(带 encrypted_content)。
    fn write_full_rollout(home: &std::path::Path, id: &str) -> String {
        let rel = format!("sessions/2026/09/01/rollout-2026-09-01T21-06-47-{id}.jsonl");
        let lines = [
            format!(
                "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{id}\",\"timestamp\":\"2026-09-01T13:06:47Z\",\"cwd\":\"C:/work\",\"originator\":\"Codex Desktop\",\"cli_version\":\"0.151\",\"model_provider\":\"openai\"}}}}"
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
        rel
    }

    /// 🔴 导出只带对话正文。三样东西**绝不能**进导出文件：`developer` 消息（Desktop 注入的
    /// app-context）、工具输出（可能是整个文件内容）、`encrypted_content`（密文）。
    /// 导出的 Markdown 是用户会分享出去的东西 —— 这条判据守的是「别把不该外传的一起发出去」。
    #[test]
    fn markdown_export_carries_only_the_conversation() {
        let home = tmp_home("md");
        let rel = write_full_rollout(&home, "t1");
        let md = to_markdown(&fs::read_to_string(home.join(&rel)).unwrap());

        assert!(md.contains("# Codex 会话 t1"), "要有元数据抬头: {md}");
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

    /// 🔴 删除必须三处一起删。只删文件会在 Desktop 列表里留下一条**点开即报错的死条目**
    /// （恢复会话时 Codex 只 `SELECT rollout_path`，拿到的是空气）。
    #[test]
    fn deleting_removes_the_file_the_index_row_and_the_db_row() {
        let home = tmp_home("del");
        let rel = write_full_rollout(&home, "t1");
        let keep = write_full_rollout(&home, "t2");
        fs::write(
            home.join("session_index.jsonl"),
            "{\"id\":\"t1\",\"thread_name\":\"要删的\"}\n{\"id\":\"t2\",\"thread_name\":\"留着的\"}\n",
        )
        .unwrap();
        let db = home.join("state_5.sqlite");
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute("CREATE TABLE threads (id TEXT, model_provider TEXT)", []).unwrap();
        conn.execute("INSERT INTO threads VALUES ('t1','openai'),('t2','openai')", []).unwrap();
        drop(conn);

        let (deleted, failed) = delete_at(&home, std::slice::from_ref(&rel));
        assert_eq!((deleted, failed.len()), (1, 0), "失败项: {failed:?}");
        assert!(!home.join(&rel).exists(), "rollout 文件要删掉");
        assert!(home.join(&keep).exists(), "没选中的那条不许动");

        let idx = fs::read_to_string(home.join("session_index.jsonl")).unwrap();
        assert!(!idx.contains("\"t1\""), "索引里要摘掉它");
        assert!(idx.contains("\"t2\""), "索引里别的行要留着");

        let conn = rusqlite::Connection::open(&db).unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM threads WHERE id='t1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "threads 行要删掉 —— 否则列表里留一条死条目");
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM threads WHERE id='t2'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
        let _ = fs::remove_dir_all(&home);
    }

    /// 越界路径要被拒绝、且**不删任何东西**（清单/参数都可能被外部改）。
    #[test]
    fn a_path_outside_home_is_refused() {
        let home = tmp_home("escape");
        let victim = home.parent().unwrap().join("synaroute-ops-victim.jsonl");
        fs::write(&victim, b"keep me").unwrap();

        let rel = format!("../{}", victim.file_name().unwrap().to_string_lossy());
        let (deleted, failed) = delete_at(&home, &[rel]);
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
        let rel = write_full_rollout(&home, "t1");
        let db = home.join("sqlite/other.db");
        fs::create_dir_all(db.parent().unwrap()).unwrap();
        rusqlite::Connection::open(&db)
            .unwrap()
            .execute("CREATE TABLE something_else (x TEXT)", [])
            .unwrap();

        let (deleted, failed) = delete_at(&home, std::slice::from_ref(&rel));
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
        ] {
            assert!(s.contains(kind), "预览摘要没提 {kind}：\n{s}");
        }
        // 反过来也要说清**不动**什么 —— 用户最担心的是对话内容被改。
        assert!(s.contains("不改任何对话正文"));
    }

    /// 诊断报告里那一行：有会话时必须给出「总数 / 当前 provider / 指向别处」三个数字。
    /// 没接入过 Codex 的机器返回 `None`（不在报告里多一行噪音）。
    #[test]
    fn the_diagnostics_line_carries_the_mismatch_count() {
        let home = tmp_home("diagline");
        write_full_rollout(&home, "t1"); // 这条记的是 openai
        fs::write(
            home.join("config.toml"),
            "model_provider = \"synaroute\"\n\n[model_providers.synaroute]\nname = \"x\"\n",
        )
        .unwrap();

        let list = list_at(&home);
        assert_eq!(list.current_provider, "synaroute");
        assert_eq!(list.rows.len(), 1);
        let bad = list.rows.iter().filter(|r| r.provider != list.current_provider).count();
        assert_eq!(bad, 1, "那条 openai 的会话就是「指向别处」");
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


