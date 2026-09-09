//! 会话页的写侧：**手动**同步、同步目标发现、索引孤儿清理、本页偏好。
//!
//! # 为什么需要「手动同步」这个入口
//!
//! 自动同步只在**接入**（点「启动」）时跑。于是接入那一刻 Codex 正开着 → rollout 被独占 →
//! N 条会话被跳过，而此前唯一的补救是「先停止再启动」整个代理 —— 为了改几行首行元数据
//! 去重启整条转发链路，代价与目的完全不成比例。CodexPlusPlus 的会话页有一个「立刻修复
//! 历史会话」按钮，那是对的。
//!
//! # 同步目标为什么要给用户选
//!
//! 自动同步的目标恒为 `synaroute`。但用户有两个正当需求是它覆盖不到的：
//!
//! - **把旧对话指回官方**（`openai`）而不做完整还原 —— 还原会把 `config.toml` 一起交还；
//! - 机器上有 cc-switch 或手配的 provider 时，把某批会话指到那个 id 上。
//!
//! 候选来源与 CodexPlusPlus / `Dailin521/codex-provider-sync` 一致：`config.toml` 的根
//! `model_provider` 与全部 `[model_providers.<id>]`、rollout 首行里出现过的、会话库里出现
//! 过的。**当前生效的那个排第一**，其余按 id 排序。
//!
//! 🔴 **它不改 `config.toml`。** 这一页只动历史会话的归属，不是「切换 provider」——
//! 那是接入/还原的事。混起来会让用户以为在这里选 `openai` 就等于切回官方登录了。
//!
//! # 偏好落在自己的小文件里，不进 `AppSettings`
//!
//! `codex-session-prefs.json`（数据目录，受 `SYNAROUTE_DATA_DIR` 隔离）。理由有三条：
//! ① `AppSettings` 会被前端整份 `saveSettings` 覆盖，漏一个键的失效方向是「开关自己弹回去」
//! （`userPrefsParity` 那条判据就是为此建的），而这两位与全局偏好无关；
//! ② `model.rs` / `SettingsPage.tsx` 棘轮余量都是 0；
//! ③ 🔴 **绝不能塞进回滚清单** `codex-session-providers.json` —— 那份文件在还原成功后会被
//! **删掉**，偏好会跟着消失，而它是我们唯一的原值凭据，多一个用途就多一条搞坏它的路。

use super::{files, SyncReport};
use crate::error::AppResult;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

const PREFS_FILE: &str = "codex-session-prefs.json";

/// 内置 provider。Codex 里它永远存在（所以指向它的旧会话不报「provider 不存在」而是
/// **静默**打 `api.openai.com`），故它一定要在候选里 —— 「指回官方」是最常见的手动目标。
const BUILTIN_PROVIDER: &str = "openai";

/// 进程内互斥：同一时刻只允许一趟会话同步。
///
/// 🔴 **不做 CodexPlusPlus 那样的锁文件。** 它必须跨进程（launcher 与 manager 是两个
/// 可执行文件，各自都会同步），而我们只有一个进程 —— 一个锁文件带来的是「上次崩溃留下
/// 陈旧锁、此后永远同步不了」这类问题，而它要防的东西在这里压根不存在。
///
/// 有了「手动同步」这个入口之后，接入那一趟与用户点按钮那一趟**确实可能同时跑**，
/// 而两趟都会读改同一批 rollout 与同一份清单 —— 清单那半的竞态方向最坏（「首记即锁」
/// 依赖读-改-写是原子的）。
static SYNC_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// 本页偏好。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionPrefs {
    /// 关掉「接入时自动同步历史会话」。默认 `false`（= 开着，与本功能上线时的行为一致）。
    ///
    /// 存在的理由：这是我们**唯一**会主动改用户对话文件的自动动作。同时用 cc-switch 的人
    /// 有正当理由不让我们碰。关掉之后手动按钮仍然可用 —— 语义是「不自动，但我要时能做」。
    #[serde(default)]
    pub auto_sync_disabled: bool,
    /// 上次在下拉里选过的目标，只用于回填选中项。
    #[serde(default)]
    pub last_target: String,
}

fn prefs_path(data_dir: &Path) -> std::path::PathBuf {
    data_dir.join(PREFS_FILE)
}

/// 读偏好。**任何失败都回落默认值**：这是一份纯偏好文件，读不出来的正确行为是按默认走，
/// 而不是让接入失败在一个偏好上。
pub(in crate::tools) fn read_prefs_in(data_dir: &Path) -> SessionPrefs {
    std::fs::read_to_string(prefs_path(data_dir))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn write_prefs_in(data_dir: &Path, p: &SessionPrefs) -> AppResult<()> {
    std::fs::create_dir_all(data_dir)
        .map_err(|e| crate::error::AppError::ToolConfig(format!("创建数据目录失败: {e}")))?;
    let path = prefs_path(data_dir);
    let tmp = files::tmp_path_for(&path);
    let text = serde_json::to_string_pretty(p)?;
    std::fs::write(&tmp, text)
        .and_then(|_| std::fs::rename(&tmp, &path))
        .map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            crate::error::AppError::ToolConfig(format!("写会话页偏好失败: {e}"))
        })
}

/// 接入路径要不要自动同步。
pub(in crate::tools) fn auto_sync_enabled() -> bool {
    match crate::store::data_dir::app_data_dir() {
        Ok(d) => !read_prefs_in(&d).auto_sync_disabled,
        // 数据目录都拿不到时按「开着」走：默认值该偏向修复那一侧。
        Err(_) => true,
    }
}

/// 取锁之后再同步。两个调用方（接入、手动按钮）都必须走它。
///
/// 锁被别的线程 poison 过（那说明另一趟同步 panic 了）时**照样继续** ——
/// 数据一致性靠清单与原子替换保证，不靠这把锁；把一次同步拒掉换不来任何安全。
pub(in crate::tools) fn locked_sync(
    home: &Path,
    data_dir: &Path,
    target: &str,
) -> AppResult<SyncReport> {
    let _guard = SYNC_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    super::sync_to_at(home, data_dir, target)
}

/// 还原是同一批 rollout + 同一份清单的**第三个写者**，必须与两条同步路径共用这把锁。
/// 少这一环会出现「同步改回 target 后，还原紧接着删掉清单」的不可回滚交错。
pub(in crate::tools) fn locked_restore(
    home: &Path,
    data_dir: &Path,
) -> AppResult<Option<String>> {
    let _guard = SYNC_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    super::restore_at(home, data_dir)
}

// ---- 同步目标发现 ----

/// 一个候选目标。`sources` 是它**在哪儿出现过**，用来让用户判断该选哪个。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TargetOption {
    pub id: String,
    /// `config` / `rollout` / `sqlite`，已排序去重。
    pub sources: Vec<String>,
    pub is_current: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetList {
    /// `config.toml` 的根 `model_provider`。空串 = 读不出来。
    pub current: String,
    pub targets: Vec<TargetOption>,
    /// 我们自己接入时会用的那个 id，前端据此把它标成推荐项。
    pub ours: String,
    pub prefs: SessionPrefs,
}

/// provider id 的形态判据。
///
/// 收窄到「字母数字 + `_-.`」是因为这个值会被**写进用户的 rollout 首行**。带控制字符或
/// 引号的串会毁掉那一行 JSON，而毁掉的是用户的对话文件。同 CodexPlusPlus 的
/// `is_valid_explicit_provider_id`。
pub(in crate::tools) fn is_valid_provider_id(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= 64
        && v.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

/// 从 `config.toml` 读根 provider 与全部 `[model_providers.<id>]`。
fn config_providers(home: &Path) -> (String, Vec<String>) {
    let text = std::fs::read_to_string(home.join("config.toml")).unwrap_or_default();
    let parsed = text.parse::<toml::Value>().ok();
    let current = parsed
        .as_ref()
        .and_then(|v| v.get("model_provider")?.as_str().map(str::to_string))
        .unwrap_or_default();
    let declared = parsed
        .as_ref()
        .and_then(|v| v.get("model_providers")?.as_table())
        .map(|t| t.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    (current, declared)
}

/// 会话库里出现过的 provider id。只读、失败即空 —— 这一层只是给下拉多几个候选。
fn sqlite_providers(home: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for db in super::session_db_paths(home) {
        let Ok(conn) = rusqlite::Connection::open_with_flags(
            &db,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        ) else {
            continue;
        };
        let _ = conn.busy_timeout(std::time::Duration::from_millis(600));
        // 语句与连接的生存期都收在这个块里 —— `let Ok(..) = .. else { continue }` 会让
        // 借用活过 `conn`（实测 E0597）。
        let got: Vec<String> = match conn.prepare(
            "SELECT DISTINCT model_provider FROM threads WHERE COALESCE(model_provider,'') <> ''",
        ) {
            Ok(mut stmt) => match stmt.query_map([], |r| r.get::<_, String>(0)) {
                Ok(rows) => rows.flatten().collect(),
                Err(_) => Vec::new(),
            },
            Err(_) => Vec::new(),
        };
        out.extend(got);
    }
    out
}

pub(in crate::tools) fn discover_targets(home: &Path, data_dir: &Path) -> TargetList {
    let (current, declared) = config_providers(home);
    // BTreeMap/BTreeSet：id 与 sources 都要稳定顺序，否则下拉每次刷新都在跳。
    let mut map: BTreeMap<String, BTreeSet<&'static str>> = BTreeMap::new();
    let add = |ids: Vec<String>, src: &'static str, map: &mut BTreeMap<_, BTreeSet<_>>| {
        for id in ids {
            if is_valid_provider_id(&id) {
                map.entry(id).or_default().insert(src);
            }
        }
    };
    add(vec![current.clone(), BUILTIN_PROVIDER.to_string()], "config", &mut map);
    add(declared, "config", &mut map);
    add(
        super::scan_at(home).sessions.into_iter().map(|s| s.provider).collect(),
        "rollout",
        &mut map,
    );
    add(sqlite_providers(home), "sqlite", &mut map);

    let mut targets: Vec<TargetOption> = map
        .into_iter()
        .map(|(id, srcs)| TargetOption {
            is_current: id == current,
            sources: srcs.into_iter().map(str::to_string).collect(),
            id,
        })
        .collect();
    // 当前生效的排第一（同参照实现）。其余已经因 BTreeMap 而按 id 有序。
    targets.sort_by_key(|t| !t.is_current);
    TargetList {
        current,
        targets,
        ours: super::super::MCP_CLIENT_NAME.to_string(),
        prefs: read_prefs_in(data_dir),
    }
}

// ---- 索引孤儿清理 ----

/// `session_index.jsonl` 里指向已不存在的 rollout 的行数与内容。
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexAudit {
    pub total: usize,
    /// 指向已不存在的会话的行数。
    pub orphans: usize,
    /// 前几条孤儿的 id，给用户核对用（不全列 —— 几百条 id 对他毫无意义）。
    pub sample: Vec<String>,
}

/// 最多回显几条孤儿 id。
const ORPHAN_SAMPLE: usize = 5;

/// 审计索引。**只读**。
///
/// 孤儿的来路：用户在别处手删过 rollout、别的工具（cc-switch / CodexPlusPlus）删过、
/// 或某次删除中途失败。它们的表现是 Codex 会话列表里**点开即报错的死条目**
/// （恢复会话时只 `SELECT rollout_path`，拿到的是空气）。
///
/// 🔴 **判据是「这个 id 在磁盘上有没有对应的 rollout」，不是「文件存不存在」**：索引行里
/// 只有 id，没有路径。而 id 要从**文件名**推导（见 [`files::thread_id_from_filename`]）——
/// 用首行的 `payload.id` 会让 fork 子会话把父会话的 id 也算成「还在」，那方向是安全的
/// （少删），但会让清理永远清不掉父会话真正的孤儿行。
pub(in crate::tools) fn audit_index(home: &Path) -> IndexAudit {
    let live: BTreeSet<String> =
        super::scan_at(home).sessions.into_iter().map(|s| s.thread_id).collect();
    let Ok(text) = std::fs::read_to_string(home.join("session_index.jsonl")) else {
        return IndexAudit::default();
    };
    let mut audit = IndexAudit::default();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        audit.total += 1;
        let id = serde_json::from_str::<Value>(line)
            .ok()
            .and_then(|v| v.get("id")?.as_str().map(str::to_string))
            .unwrap_or_default();
        // 认不出 id 的行一律不算孤儿 —— 删掉自己读不懂的东西是最坏的默认值。
        if id.is_empty() || live.contains(&id) {
            continue;
        }
        audit.orphans += 1;
        if audit.sample.len() < ORPHAN_SAMPLE {
            audit.sample.push(id);
        }
    }
    audit
}

/// 删掉孤儿行。返回真正删掉的行数。
///
/// **改写前先把整份索引备份到数据目录**：这个文件很小（本机 131 字节），而判据读的是
/// 「磁盘上有没有对应文件」—— 万一 `CODEX_HOME` 在清理那一刻指向了别处（用户刚改过环境
/// 变量、或我们解析错了），全部行都会被判成孤儿。备份是这种情形唯一的退路。
/// 备份失败就**不清理**（同 sqlite 那半的纪律：宁可不做，也不留一个无法回退的改动）。
pub(in crate::tools) fn prune_index(home: &Path, data_dir: &Path) -> Result<usize, String> {
    let path = home.join("session_index.jsonl");
    let text = std::fs::read_to_string(&path).map_err(|e| format!("读索引失败: {e}"))?;
    let live: BTreeSet<String> =
        super::scan_at(home).sessions.into_iter().map(|s| s.thread_id).collect();
    let mut removed = 0usize;
    let kept: Vec<&str> = text
        .lines()
        .filter(|l| {
            if l.trim().is_empty() {
                return false;
            }
            let id = serde_json::from_str::<Value>(l)
                .ok()
                .and_then(|v| v.get("id")?.as_str().map(str::to_string))
                .unwrap_or_default();
            if id.is_empty() || live.contains(&id) {
                return true;
            }
            removed += 1;
            false
        })
        .collect();
    if removed == 0 {
        return Ok(0);
    }
    files::backup_file(&path, data_dir, "codex-session-index", files::Keep::Count(5))?;
    // 行尾按原文件保留（本仓在行尾上栽过三次）。
    let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut out = kept.join(eol);
    if !out.is_empty() {
        out.push_str(eol);
    }
    let tmp = files::tmp_path_for(&path);
    std::fs::write(&tmp, out)
        .and_then(|_| std::fs::rename(&tmp, &path))
        .map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("写索引失败: {e}")
        })?;
    Ok(removed)
}

// ---- 命令 ----

/// 同步目标下拉的数据源。
#[tauri::command]
pub async fn list_codex_provider_targets() -> Result<TargetList, String> {
    let home = super::super::codex_paths::codex_home().map_err(|e| e.to_string())?;
    let data_dir = crate::store::data_dir::app_data_dir().map_err(|e| e.to_string())?;
    Ok(discover_targets(&home, &data_dir))
}

fn target_is_known(list: &TargetList, target: &str) -> bool {
    list.targets.iter().any(|o| o.id == target)
}

/// 手动把历史会话同步到 `target`。
///
/// 目标形态在这里**必须再校验一次**：命令是前端调的，而它的值最终会被写进用户的 rollout
/// 首行。前端校验只是体验，后端校验才是防线。
#[tauri::command]
pub async fn sync_codex_sessions(target: String) -> Result<String, String> {
    let target = target.trim().to_string();
    if !is_valid_provider_id(&target) {
        return Err(format!("provider id 形态不合法（只允许字母数字与 _ - .）：{target:?}"));
    }
    let home = super::super::codex_paths::codex_home().map_err(|e| e.to_string())?;
    let data_dir = crate::store::data_dir::app_data_dir().map_err(|e| e.to_string())?;
    // 形态合法还不够：必须是后端刚发现并展示给用户的候选。否则直接调 IPC 能把任意
    // `valid-looking` id 写进全部 rollout，而 config.toml 压根没声明它。
    let known = discover_targets(&home, &data_dir);
    if !target_is_known(&known, &target) {
        return Err(format!("provider id 不在当前可选目标中，未修改任何会话：{target}"));
    }
    let report = locked_sync(&home, &data_dir, &target).map_err(|e| e.to_string())?;
    // 选过就记下来，下次打开这一页回填它。写失败不影响本次同步的结论。
    let mut prefs = read_prefs_in(&data_dir);
    prefs.last_target = target.clone();
    let _ = write_prefs_in(&data_dir, &prefs);
    // 一条都没动也要如实说 —— 「已同步 0 条」比一句「完成」有信息量得多。
    Ok(super::describe(&report).unwrap_or_else(|| {
        format!("全部 {} 个历史对话已经指向 {target}，无需改动", report.already_ok)
    }))
}

/// 保存本页偏好（目前只有「接入时自动同步」这一位）。
#[tauri::command]
pub async fn set_codex_session_auto_sync(enabled: bool) -> Result<(), String> {
    let data_dir = crate::store::data_dir::app_data_dir().map_err(|e| e.to_string())?;
    let mut prefs = read_prefs_in(&data_dir);
    prefs.auto_sync_disabled = !enabled;
    write_prefs_in(&data_dir, &prefs).map_err(|e| e.to_string())
}

/// 审计 `session_index.jsonl`（只读）。
#[tauri::command]
pub async fn audit_codex_session_index() -> Result<IndexAudit, String> {
    let home = super::super::codex_paths::codex_home().map_err(|e| e.to_string())?;
    Ok(audit_index(&home))
}

/// 清掉索引里的孤儿行。
#[tauri::command]
pub async fn prune_codex_session_index() -> Result<String, String> {
    let home = super::super::codex_paths::codex_home().map_err(|e| e.to_string())?;
    let data_dir = crate::store::data_dir::app_data_dir().map_err(|e| e.to_string())?;
    let n = prune_index(&home, &data_dir)?;
    Ok(if n == 0 {
        "没有需要清理的条目".to_string()
    } else {
        format!("已清掉 {n} 条指向已删除会话的索引行（原索引已备份到数据目录）")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    fn tmp_home(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "synaroute-cxsync-{}-{}-{tag}",
            std::process::id(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("sessions/2026/09/01")).unwrap();
        fs::create_dir_all(d.join("data")).unwrap();
        d
    }

    fn write_rollout(home: &Path, id: &str, provider: &str) -> String {
        let rel = format!("sessions/2026/09/01/rollout-2026-09-01T21-06-47-{id}.jsonl");
        fs::write(
            home.join(&rel),
            format!(
                "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{id}\",\"model_provider\":\"{provider}\"}}}}\n{{\"x\":1}}\n"
            ),
        )
        .unwrap();
        rel
    }

    const ID_A: &str = "01a05d14-4e5b-7773-b425-25ae029f078f";
    const ID_B: &str = "01a05d3b-0017-7ec3-9cff-23ea32293b1b";

    /// 候选必须合并三个来源，且**当前生效的那个排第一**。
    #[test]
    fn targets_merge_config_rollout_and_database_with_current_first() {
        let home = tmp_home("targets");
        write_rollout(&home, ID_A, "legacy-relay");
        fs::write(
            home.join("config.toml"),
            "model_provider = \"synaroute\"\n\n[model_providers.synaroute]\nname = \"x\"\n\n[model_providers.ccswitch]\nname = \"y\"\n",
        )
        .unwrap();
        let db = home.join("state_5.sqlite");
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute("CREATE TABLE threads (id TEXT, model_provider TEXT)", []).unwrap();
        conn.execute("INSERT INTO threads VALUES ('x','from-db')", []).unwrap();
        drop(conn);

        let list = discover_targets(&home, &home.join("data"));
        assert_eq!(list.current, "synaroute");
        assert_eq!(list.targets.first().map(|t| t.id.as_str()), Some("synaroute"), "当前的排第一");
        let ids: Vec<&str> = list.targets.iter().map(|t| t.id.as_str()).collect();
        for want in ["synaroute", "ccswitch", "openai", "legacy-relay", "from-db"] {
            assert!(ids.contains(&want), "候选里缺 {want}：{ids:?}");
        }
        // 来源标记要如实合并。
        let syn = list.targets.iter().find(|t| t.id == "synaroute").unwrap();
        assert_eq!(syn.sources, vec!["config"]);
        let relay = list.targets.iter().find(|t| t.id == "legacy-relay").unwrap();
        assert_eq!(relay.sources, vec!["rollout"]);
        // `openai` 永远在候选里 —— 「指回官方」是最常见的手动目标。
        assert!(ids.contains(&"openai"));
        let _ = fs::remove_dir_all(&home);
    }

    /// 🔴 形态不合法的 provider id 一律拒绝 —— 这个值会被写进用户的 rollout 首行，
    /// 带引号或控制字符会毁掉那一行 JSON。
    #[test]
    fn a_malformed_provider_id_is_refused() {
        for bad in ["", " ", "a b", "a\"b", "a\nb", "../x", "a/b", &"x".repeat(65)] {
            assert!(!is_valid_provider_id(bad), "{bad:?} 应被拒绝");
        }
        for ok in ["openai", "synaroute", "cc-switch", "my_relay", "v1.2"] {
            assert!(is_valid_provider_id(ok), "{ok:?} 应被接受");
        }
    }

    /// 索引孤儿：指向已不存在会话的行要被认出并清掉，**认不出 id 的行必须留着**，
    /// 且清理前必须留下备份。
    #[test]
    fn orphan_index_rows_are_pruned_and_the_original_is_backed_up() {
        let home = tmp_home("orphan");
        let data = home.join("data");
        write_rollout(&home, ID_A, "synaroute");
        fs::write(
            home.join("session_index.jsonl"),
            format!(
                "{{\"id\":\"{ID_A}\",\"thread_name\":\"活着的\"}}\n\
                 {{\"id\":\"{ID_B}\",\"thread_name\":\"文件已经没了\"}}\n\
                 这一行不是 JSON\n"
            ),
        )
        .unwrap();

        let audit = audit_index(&home);
        assert_eq!((audit.total, audit.orphans), (3, 1), "只有 B 是孤儿");
        assert_eq!(audit.sample, vec![ID_B.to_string()]);

        let n = prune_index(&home, &data).unwrap();
        assert_eq!(n, 1);
        let idx = fs::read_to_string(home.join("session_index.jsonl")).unwrap();
        assert!(idx.contains(ID_A), "活着的那条要留着");
        assert!(!idx.contains(ID_B), "孤儿要删掉");
        assert!(idx.contains("这一行不是 JSON"), "读不懂的行一律保留");
        let backups = fs::read_dir(data.join("backups").join("codex-session-index")).unwrap();
        assert_eq!(backups.count(), 1, "清理前必须留一份备份");

        // 幂等：没有孤儿时一个字节都不写、也不再产生备份。
        assert_eq!(prune_index(&home, &data).unwrap(), 0);
        let backups = fs::read_dir(data.join("backups").join("codex-session-index")).unwrap();
        assert_eq!(backups.count(), 1, "没孤儿就不该再备份");
        let _ = fs::remove_dir_all(&home);
    }

    /// 偏好读写：默认值必须是「自动同步开着」，读不出文件时也一样。
    #[test]
    fn prefs_default_to_auto_sync_on() {
        let home = tmp_home("prefs");
        let data = home.join("data");
        assert!(!read_prefs_in(&data).auto_sync_disabled, "默认必须偏向修复那一侧");

        let p = SessionPrefs { auto_sync_disabled: true, last_target: "openai".into() };
        write_prefs_in(&data, &p).unwrap();
        let back = read_prefs_in(&data);
        assert!(back.auto_sync_disabled);
        assert_eq!(back.last_target, "openai");

        // 文件被写坏时回落默认值，而不是让接入失败在一个偏好上。
        fs::write(data.join(PREFS_FILE), "{ 坏 json").unwrap();
        assert!(!read_prefs_in(&data).auto_sync_disabled);
        let _ = fs::remove_dir_all(&home);
    }

    /// 🔴 偏好**绝不能**落进回滚清单文件：那份文件在还原成功后会被删掉。
    ///
    /// 这条钉的是文件名而不是行为 —— 行为上的失效（还原一次偏好就丢了）要跑一整趟
    /// 同步+还原才看得出来，而那时症状是「开关自己弹回去」，没人会联想到还原。
    #[test]
    fn prefs_live_in_their_own_file() {
        assert_ne!(PREFS_FILE, super::super::MANIFEST_FILE);
        let src = include_str!("codex_session_sync.rs");
        let prod = crate::proxy::custom_headers::production_code_only(src);
        assert!(
            !prod.contains("MANIFEST_FILE"),
            "偏好不许碰回滚清单 —— 那份文件在还原成功后会被删掉，偏好会跟着消失"
        );
    }

    /// 🔴 接线判据：五个命令都必须进 `generate_handler!`。
    ///
    /// 策略门 `invoke-command-must-exist` 只查正向（前端调的名字在 Rust 有定义），
    /// 反向（写了命令却没注册）**只在用户点到那个按钮时才炸**，同 `key_flags.rs` 那条。
    #[test]
    fn the_sync_commands_must_be_registered_in_the_handler_list() {
        let lib = include_str!("../lib.rs");
        for cmd in [
            "list_codex_provider_targets",
            "sync_codex_sessions",
            "set_codex_session_auto_sync",
            "audit_codex_session_index",
            "prune_codex_session_index",
        ] {
            assert!(
                lib.contains(&format!("codex_sessions::sync::{cmd}")),
                "{cmd} 没进 generate_handler! —— 界面上点它会报 command not found"
            );
        }
    }

    /// 🔴 接线判据：接入路径必须**先问过偏好**再同步。
    ///
    /// 上面那条 `prefs_default_to_auto_sync_on` 只证明读写正确 —— 把 `append_sync_note` 里
    /// 那个判断摘掉，它照样全绿，而那正是「关掉了开关却照样改用户会话文件」这个缺陷本体。
    #[test]
    fn the_apply_path_must_consult_the_auto_sync_preference() {
        let src = include_str!("codex_sessions.rs");
        let prod = crate::proxy::custom_headers::production_code_only(src);
        assert!(
            prod.contains("sync::auto_sync_enabled()"),
            "codex_sessions.rs 的接入路径必须调 sync::auto_sync_enabled()"
        );
    }
}
