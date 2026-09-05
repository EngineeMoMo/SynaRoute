//! Codex 历史会话的 provider 同步：让**切换 provider 之前**创建的对话仍然路由到我们。
//!
//! # 为什么需要它（2026-09-03 实测，codex-cli 0.151.0-alpha.7.2）
//!
//! 每条 thread **自带一个 provider 身份**，写在 rollout 首行的
//! `session_meta.payload.model_provider`；app-server 的 `thread/resume` 用它**覆盖
//! `config.toml` 的根 `model_provider`**。于是官方登录期创建的对话记着 `openai`
//! （内置 provider、永远存在 → 不报 `Model provider not found` 而是**静默**打
//! `api.openai.com`）→ 拿 `auth.json` 里我们的占位符 → **401**。
//!
//! 用户看到的形态是：**旧对话每次都 401，新建对话完全正常**，而 `config.toml` 完好、
//! [`super::drift_state`] 判 `Intact`、一个字都不报。这是占位符外发的**第三条**路径
//! （另两条见 codex.rs 模块头）。
//!
//! **四组实测的对照表**（含「不传参也走会话里记的那个」这条决定性的一行）在测试段
//! `the_real_codex_binary_resumes_with_the_provider_we_wrote` 的文档里 —— 它就是那张表的
//! 可执行版本。协议侧印证：`Thread` 与 `ThreadResumeResponse` 的 `modelProvider` 都是
//! **required**；权威定义可自己导：`codex.exe app-server generate-json-schema --out <dir>`。
//!
//! 🔴 **别把第三方工具的说法当成这条的解释。** `Dailin521/codex-provider-sync` 与
//! CodexPlusPlus 改的就是这两处，但它们的 README 只说「切 provider 后旧会话**从列表里
//! 消失**（可见性）」—— 打错上游这个后果它们没写，而那才是 401 的来源。改的地方对、
//! 描述不全。故本模块的判据按**路由正确性**设计，不按可见性。
//!
//! # rollout 是权威，sqlite 只影响列表侧
//!
//! 恢复会话时 Codex 只 `SELECT rollout_path FROM threads WHERE id = ?`（二进制里那条
//! SQL），provider 是从 rollout 文件读出来的 —— 实测夹具**根本没有 sqlite 记录**也照样
//! 生效。`threads.model_provider` 管的是 Desktop 的会话列表，故 sqlite 那半是 best-effort、
//! [`sync_to_at`] 的成功**不依赖它可写**。看到 sqlite 跳过就以为整个功能没生效，是误读。
//!
//! # 🔴 已知边界：本模块只保证「路由到对的上游」，不保证旧对话一定能继续
//!
//! 跨账号恢复时 Responses 的 reasoning 带 `encrypted_content`、由签发它的账号加密，换上游
//! 后可能解不开（与 `THINKING_SIGNATURE_INVALID` 同族）。故用户可见文案**只说「已把 N 个
//! 旧对话指向 SynaRoute」**，绝不说「已恢复可用」—— 那是我们手里的信息支撑不了的承诺。

use crate::error::{AppError, AppResult};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// 回滚清单文件名（落在应用数据目录，受 `SYNAROUTE_DATA_DIR` 隔离）。
const MANIFEST_FILE: &str = "codex-session-providers.json";

/// 会话目录名。`archived_sessions` 不能漏 —— 漏了的表现是「归档里的旧会话仍 401」，
/// 而用户不会想到「已归档」与「能不能用」有关系。
const SESSION_DIRS: [&str; 2] = ["sessions", "archived_sessions"];

/// 递归深度上限。Codex 的布局是 `sessions/YYYY/MM/DD/`，3 层足够；给到 6 层是留余量。
///
/// ⚠️ 它防的是**异常深的真实目录树**，不是符号链接环 —— [`walk`] 用的
/// `DirEntry::file_type()` 按 std 的语义**不跟随符号链接**，指向目录的链接
/// `is_dir()` / `is_file()` 双false、直接落进 `_` 被跳过，那种环压根形成不了。
/// （这条原先写的是「防链接环」，代码审查时按 std 文档核出来是错的。）
const MAX_DEPTH: usize = 6;
#[path = "codex_session_ops.rs"] pub(crate) mod ops;

/// 每个 sqlite 库保留几份写前备份。3 份足够回退一次误操作，而不至于让频繁启停把
/// 备份目录堆成无界增长（每份约 0.5 MB）。
const DB_BACKUP_KEEP: usize = 3;

/// `IN (?,…)` 一批最多放几个 id。SQLite 的绑定参数上限是 32766（3.32 之前 999），
/// 取 500 留足余量，且远小于任一版本的下限。
const SQL_CHUNK: usize = 500;

/// 上限由**编译器**保证，不是靠测试：把 `SQL_CHUNK` 调过 999 直接编译不过。
/// 写成 `const` 断言而不是 `#[test]` 是刻意的 —— 硬保证不该降级成软保证。
const _: () = assert!(SQL_CHUNK < 999, "3.32 之前的 SQLite 绑定参数上限就是 999");

/// 一条会话的首行元数据。**只从 rollout 首行读**，不碰正文。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionRef {
    /// 相对 `$CODEX_HOME` 的路径。清单里存的也是它 —— 存绝对路径会在换机器或改
    /// `CODEX_HOME` 之后认领到别人的文件上去。
    pub rel_path: String,
    pub thread_id: String,
    /// 首行里记的 provider。缺失时是空串（Codex 早期版本没这个字段），
    /// 空串**参与同步**：它同样会让 `thread/resume` 拿不到我们的 provider。
    pub provider: String,
    pub archived: bool,
    pub cwd: String,
    pub timestamp: String,
    pub bytes: u64,
}

/// 扫描结果。`unreadable` 单独计数而不是静默丢掉：Codex 的 rollout 格式已经变过几次
/// （本轮实测中 `history_mode: "full"` 就被新版拒绝过），全部无法解析时用户该知道
/// 「同步了 0 条」是因为格式变了，而不是因为没有旧会话。
#[derive(Debug, Default)]
pub(in crate::tools) struct ScanReport {
    pub sessions: Vec<SessionRef>,
    /// 首行不是 `session_meta` / JSON 坏 / 读不出来。
    pub unreadable: usize,
    /// 路径推导不出「相对 `$CODEX_HOME`」的形态（见 [`rel_of`]）。与 `unreadable` 分开：
    /// 两者的成因与处置完全不同。
    pub path_rejected: usize,
}

/// 同步结果。
#[derive(Debug, Default)]
pub(in crate::tools) struct SyncReport {
    /// 真正改过的会话数。
    pub changed: usize,
    /// 本来就已经是目标 provider、一个字节都没动的会话数。
    pub already_ok: usize,
    /// 改写失败而跳过的会话数（文件被占用 / 目录不可写，两者在 Windows 上都映射成
    /// `PermissionDenied`，分不开 —— 故文案写成覆盖两种情形的条件句，见 [`describe`]）。
    pub skipped: usize,
    pub unreadable: usize,
    /// 路径遏制拒掉的会话数。**刻意与 `unreadable` 分开计数**：一个是路径推导出了问题、
    /// 一个是 Codex 改了 rollout 格式，合成一个数字会让用户拿到指错方向的解释。
    pub path_rejected: usize,
    /// sqlite 那半的结果。`None` = 没找到库（正常，新装机器可能还没有）。
    pub sqlite: Option<SqliteOutcome>,
}

/// sqlite 那半的结果。
///
/// 🔴 **`updated` 与 `error` 必须并存**，不能像第一版那样「只要改动行数非零就报成功」：
/// 本机同时有两个库，Codex 在跑时可能只锁住其中一个 —— 于是「一个失败一个成功」被汇总成
/// 纯成功，用户看不到有一半没同步。同「两条丢日志路径不能合成一个数字」那条。
#[derive(Debug, Default, PartialEq, Eq)]
pub(in crate::tools) struct SqliteOutcome {
    pub updated: usize,
    /// 任一库失败即有值（只留第一条，够给方向了）。
    pub error: Option<String>,
}

/// 回滚清单。**精确记原值**，不是「回滚成 openai」那种猜测。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    /// 写清单时的目标 provider，仅用于人读与排障。
    target: String,
    synced_at: String,
    entries: Vec<ManifestEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManifestEntry {
    rel_path: String,
    /// 我们动它**之前**那一行里的值。空串表示原本没有这个字段 —— 还原时要把字段
    /// 整个摘掉，而不是写一个空串进去（那是两种不同的形态，Codex 对前者有默认行为）。
    original_provider: String,
    /// 该会话的 thread id，用于**还原 sqlite 那半**（`threads.model_provider`）。
    /// `#[serde(default)]` 让本字段上线前写出的旧清单仍能读 —— 那些条目的 sqlite
    /// 回滚会被跳过，而不是让整份清单解析失败（后者会让回滚凭据整个消失）。
    #[serde(default)]
    thread_id: String,
}


// 扫描（只读首行）

/// 读文件的第一行，**行尾原样保留** —— 改写时要用它把新首行拼回去，归一成 `\n` 会让整份
/// 文件的行尾与 Codex 自己写的不一致（本仓在行尾上栽过三次，症状都是「看起来改对了、
/// 实际匹配不上」）。用 `read_line` 而不是读全文：首行实测 50 KB 量级
/// （`base_instructions` 内嵌在里面），而 rollout 整体可以到几十 MB。
fn read_first_line(path: &Path) -> io::Result<String> {
    let mut line = String::new();
    BufReader::new(File::open(path)?).read_line(&mut line)?;
    Ok(line)
}

/// 把一行拆成「正文」与「行尾」。
fn split_eol(line: &str) -> (&str, &str) {
    if let Some(body) = line.strip_suffix("\r\n") {
        (body, "\r\n")
    } else if let Some(body) = line.strip_suffix('\n') {
        (body, "\n")
    } else {
        (line, "")
    }
}

/// 解析首行，只在它确实是 `session_meta` 时返回。
///
/// 🔴 **判定与改写都只针对首行**，依据是 Codex 自己写的 `"ordinal":0`（本机 4 个 rollout
/// 逐个核过，含一个 fork 出来的子会话）。CodexPlusPlus 遍历全文找 session_meta，那要把
/// 几十 MB 整个读进内存；而万一 Codex 日后挪走它，我们的表现是**首行认不出 → 跳过并
/// 计数**，不是静默改错行 —— 失效方向是安全的那一侧。
fn parse_meta(line: &str) -> Option<(String, String, String, String)> {
    let (body, _) = split_eol(line);
    let rec: Value = serde_json::from_str(body.trim()).ok()?;
    if rec.get("type").and_then(Value::as_str) != Some("session_meta") {
        return None;
    }
    let p = rec.get("payload")?;
    let s = |k: &str| p.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
    // `id` 与 `session_id` 在实测样本里同值；`id` 是 CodexPlusPlus 取的那个，跟它一致。
    let id = if p.get("id").is_some() { s("id") } else { s("session_id") };
    Some((id, s("model_provider"), s("cwd"), s("timestamp")))
}

/// 递归收集 `sessions/` 与 `archived_sessions/` 下的 `rollout-*.jsonl`。
///
/// 目录不存在**不是错误**：干净安装的机器上 `archived_sessions` 常常没有，
/// 而把它当错误会让整次接入失败在一件无关紧要的事上。
fn collect_rollouts(home: &Path) -> Vec<(PathBuf, bool)> {
    let mut out = Vec::new();
    for dir in SESSION_DIRS {
        let archived = dir == "archived_sessions";
        walk(&home.join(dir), 0, archived, &mut out);
    }
    out
}

fn walk(dir: &Path, depth: usize, archived: bool, out: &mut Vec<(PathBuf, bool)>) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        match entry.file_type() {
            Ok(t) if t.is_dir() => walk(&path, depth + 1, archived, out),
            Ok(t) if t.is_file() => {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with("rollout-") && name.ends_with(".jsonl") {
                    out.push((path, archived));
                }
            }
            _ => {}
        }
    }
}

/// 扫描全部会话的首行元数据。
pub(in crate::tools) fn scan_at(home: &Path) -> ScanReport {
    let mut report = ScanReport::default();
    for (path, archived) in collect_rollouts(home) {
        let bytes = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let Ok(line) = read_first_line(&path) else {
            report.unreadable += 1;
            continue;
        };
        let Some((thread_id, provider, cwd, timestamp)) = parse_meta(&line) else {
            report.unreadable += 1;
            continue;
        };
        let Some(rel_path) = rel_of(home, &path) else {
            report.path_rejected += 1;
            continue;
        };
        report.sessions.push(SessionRef {
            rel_path,
            thread_id,
            provider,
            archived,
            cwd,
            timestamp,
            bytes,
        });
    }
    report
}

/// 相对 `$CODEX_HOME` 的路径，统一用 `/` 分隔。`None` = 推导不出相对形态。
///
/// 🔴 **统一分隔符是为了清单能跨平台读**：`strip_prefix` 保留宿主分隔符，Windows 写出的
/// 清单在 macOS 上会被当成一个完整文件名（`file_name()` 在 Unix 上只认 `/`）——
/// `pointer_is_ours` 就是这么在 macOS CI 上连红三个版本的。
///
/// 🔴 **失败必须返回 `None`，不许兜底成绝对路径**：那会同时丢掉「换机器不认领别人的
/// 文件」与 [`resolve_in_home`] 那道遏制（它必然拒绝绝对路径 → 整批会话被算成越界，
/// 而用户看到的解释指向错误方向）。走到这里说明 [`walk`] 的前缀假设被破坏了。
fn rel_of(home: &Path, path: &Path) -> Option<String> {
    Some(
        path.strip_prefix(home)
            .ok()?
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/"),
    )
}

/// 把清单里的相对路径解析回绝对路径，并**确认它仍在 `home` 之内**。
///
/// 清单是 `%APPDATA%\SynaRoute\` 下的普通 JSON，被别人手改之后 `rel_path` 可以写成
/// `../..`，而我们拿它去**改写文件**。危害受限（目标首行必须是合法 `session_meta`），
/// 但本仓对同类原语一贯设遏制（`aggregate.rs` 修过两条路径穿透），成本近乎为零。
///
/// **刻意不用 `canonicalize`**：它解析符号链接、且对不存在的路径直接失败，而「文件已被
/// 用户删掉」是还原时的正常情形。三道门的**实际**覆盖面与直觉不符（注入实测）：逐段
/// `Normal` 那道是唯一挡得住 `..` 的（`Path::starts_with` 按 component 比较、不规范化），
/// `is_absolute` 与前缀检查对绝对路径互为冗余 —— 别以为去掉第二道还有东西兜着。
fn resolve_in_home(home: &Path, rel: &str) -> Option<PathBuf> {
    // 空串必须先挡掉：`Path::new("").components()` 是空迭代器，下面那条 `all()` 对空集
    // 恒真、`home.join("")` 又恰好等于 home 自身，于是它会通过全部三道门，把
    // **`$CODEX_HOME` 目录本身**当成一个 rollout 交出去（写这条判据时当场抓到的）。
    if rel.trim().is_empty() {
        return None;
    }
    let p = Path::new(rel);
    if p.is_absolute() {
        return None;
    }
    // 逐段只接受普通名字。`.` 也拒掉：同一个文件两种写法会让「首记即锁」认不出是同一条。
    if !p.components().all(|c| matches!(c, std::path::Component::Normal(_))) {
        return None;
    }
    let joined = home.join(p);
    joined.starts_with(home).then_some(joined)
}


// 改写单个 rollout（流式，内存 O(1)）

/// 临时文件的进程内序号。光靠 `pid + 时间戳`不够 —— 本机实测 `timestamp_nanos` 的量化
/// 粒度只有 100ns，同进程并发调用会拿到完全相同的路径，一个的 rename 会顶掉另一个正在写
/// 的临时文件（`ccswitch::db_copy_path` 上踩过：8 线程 16 万采样里 88% 撞名）。
/// 也被 [`backup_db`] 与 [`write_manifest`] 复用。
static TMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// 与目标文件**同目录**的临时文件路径 —— `std::env::temp_dir()` 可能在别的卷上，而
/// `fs::rename` 跨卷会失败，原子性全靠它。
fn tmp_path_for(path: &Path) -> PathBuf {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    path.with_file_name(format!(
        ".{name}.synaroute-{}-{}.tmp",
        std::process::id(),
        TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ))
}

/// 把首行的 `model_provider` 改成 `want`（`None` = 把字段整个摘掉）。
///
/// 返回 `Ok(Some(原值))` 表示确实改了，`Ok(None)` 表示无需改动（**一个字节都没写**）——
/// 后者必须真的不写：照写一遍相同内容会让 mtime 变、惊动 Codex 的文件监听，而且「已经
/// 对了」这个绝大多数情况会变成每次接入都全量重写几百个文件。判据用 mtime 断言它。
fn rewrite_first_line(path: &Path, want: Option<&str>) -> AppResult<Option<String>> {
    let first = read_first_line(path).map_err(|e| AppError::ToolConfig(format!("{e}")))?;
    let (body, eol) = split_eol(&first);
    let mut rec: Value = serde_json::from_str(body.trim())
        .map_err(|e| AppError::ToolConfig(format!("首行不是合法 JSON: {e}")))?;
    if rec.get("type").and_then(Value::as_str) != Some("session_meta") {
        return Err(AppError::ToolConfig("首行不是 session_meta".into()));
    }
    let payload = rec
        .get_mut("payload")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| AppError::ToolConfig("session_meta 缺 payload".into()))?;

    let original = payload
        .get("model_provider")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    match want {
        Some(w) if original == w => return Ok(None),
        Some(w) => {
            payload.insert("model_provider".into(), Value::String(w.to_string()));
        }
        // 还原到「原本没有这个字段」的形态。写空串是另一回事 —— Codex 对缺字段有默认
        // 行为，对空串没有，两者不能混。
        None => {
            if !payload.contains_key("model_provider") {
                return Ok(None);
            }
            payload.remove("model_provider");
        }
    }

    let next_first = format!("{}{eol}", serde_json::to_string(&rec)?);
    replace_first_line(path, &first, &next_first)?;
    Ok(Some(original))
}

/// 用 `next_first` 换掉文件的首行，其余字节**逐字节照搬**。写临时文件后 rename，
/// 于是任何中途失败都不会留下半个文件（这比对几百 MB 的会话做整份备份现实得多）。
fn replace_first_line(path: &Path, first: &str, next_first: &str) -> AppResult<()> {
    let tmp = tmp_path_for(path);
    let copy = (|| -> io::Result<()> {
        let mut src = File::open(path)?;
        // 按**字节**偏移跳过首行：`String::len()` 就是 UTF-8 字节数，而 rollout 是 JSONL
        // （必然 UTF-8）。带 BOM 的文件在上一步就因 JSON 解析失败被挡掉了。
        src.seek(SeekFrom::Start(first.len() as u64))?;
        let mut dst = File::create(&tmp)?;
        dst.write_all(next_first.as_bytes())?;
        io::copy(&mut src, &mut dst)?;
        // 先落盘再 rename：崩在这之间留下的是一个孤立的 .tmp（下次扫描不认它，文件名不以
        // rollout- 开头），而不是一个内容不完整的 rollout。
        dst.sync_all()
    })();
    if let Err(e) = copy {
        let _ = fs::remove_file(&tmp);
        return Err(AppError::ToolConfig(format!(
            "改写 {} 失败: {e}",
            path.display()
        )));
    }
    fs::rename(&tmp, path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        AppError::ToolConfig(format!("替换 {} 失败: {e}", path.display()))
    })
}

// 清单

fn manifest_path_in(data_dir: &Path) -> PathBuf {
    data_dir.join(MANIFEST_FILE)
}

/// 清单的三种状态。**「坏了」不能和「没有」共用一个 `None`** —— 那正是第二轮代码审查
/// 抓出的静默数据丢失：坏清单被当成「没有」→ 下一次接入以空清单起步 → 首记即锁失效 →
/// 覆盖写回 → 原值永久消失，而用户什么提示都收不到。
enum ManifestState {
    Missing,
    /// 文件在但读不出/解析不了。带上路径，好让用户能自己去看那份文件。
    Corrupt(PathBuf),
    Loaded(Manifest),
}

fn read_manifest_state(data_dir: &Path) -> ManifestState {
    let path = manifest_path_in(data_dir);
    if !path.exists() {
        return ManifestState::Missing;
    }
    match fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok()) {
        Some(m) => ManifestState::Loaded(m),
        None => ManifestState::Corrupt(path),
    }
}

fn corrupt_err(path: &Path) -> AppError {
    AppError::ToolConfig(format!(
        "历史会话的回滚清单读不出来（{}）—— 为免把原值覆盖掉，本次不动任何会话文件。\
         请检查或删除该文件后重试；删掉它意味着放弃「还原时把旧对话改回原 provider」的能力。",
        path.display()
    ))
}

/// 原子写清单：临时文件 + rename。
///
/// 🔴 **不能用裸 `fs::write`**：它先截断再写，中途崩溃/断电/盘满会留下一份**被我们自己
/// 写坏的 JSON**，而那正是 [`ManifestState::Corrupt`] 要防的输入。同 `store.rs` 对
/// `config.json` 的做法。临时文件与目标同目录 —— 跨卷 rename 会失败，而原子性全靠它。
fn write_manifest(data_dir: &Path, m: &Manifest) -> AppResult<()> {
    fs::create_dir_all(data_dir)
        .map_err(|e| AppError::ToolConfig(format!("创建数据目录失败: {e}")))?;
    let text = serde_json::to_string_pretty(m)?;
    let path = manifest_path_in(data_dir);
    let tmp = path.with_file_name(format!(
        ".{MANIFEST_FILE}.{}-{}.tmp",
        std::process::id(),
        TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let write = fs::write(&tmp, text).and_then(|_| fs::rename(&tmp, &path));
    write.map_err(|e| {
        let _ = fs::remove_file(&tmp);
        AppError::ToolConfig(format!("写会话回滚清单失败: {e}"))
    })
}

// 同步

/// 把所有 provider ≠ `target` 的历史会话改成 `target`，并把**原值**记进清单。
///
/// **同步所有 provider、不只 `openai`**：用户那些自定义 provider 在我们接入后未必还在
/// `config.toml` 里，那种形态 Codex **启动即硬报错**、一个请求都不发。改了有清单可精确
/// 回滚，不改则没有退路。
///
/// # 🔴 两条顺序纪律，各对应一条数据丢失级链路（取证与反例见测试段同名判据）
///
/// - **清单「首记即锁」**（同 `.bak`）：每个 `rel_path` 的原值只在第一次见到时记录。
///   反过来做 → 第二次接入没有新条目 → 清单被覆盖成空 → 还原时一个都改不回来。
/// - **清单必须在改文件之前落盘**：反过来做 → 盘满/权限受限时文件已改而清单从未写出 →
///   用户点停止时读不到凭据、按设计什么都不改 → 旧对话永久指向一个已停掉的端口。
///   反向的两种失败都安全：清单写失败则一个文件没动；清单写成功而某文件改失败，还原时
///   那条发现已是原值、返回 `Ok(None)`。
pub(in crate::tools) fn sync_to_at(
    home: &Path,
    data_dir: &Path,
    target: &str,
) -> AppResult<SyncReport> {
    let scan = scan_at(home);
    let mut report = SyncReport {
        unreadable: scan.unreadable,
        path_rejected: scan.path_rejected,
        ..SyncReport::default()
    };

    // ① 先算出「要动哪些」，原值直接取扫描时读到的那个 —— 不必等改写返回。
    let mut todo: Vec<(PathBuf, &SessionRef)> = Vec::new();
    for s in &scan.sessions {
        if s.provider == target {
            report.already_ok += 1;
            continue;
        }
        match resolve_in_home(home, &s.rel_path) {
            Some(path) => todo.push((path, s)),
            // 扫描自产的 rel_path 不该越界。真越界了单独计数、单独措辞 —— 与「首行认不出」
            // 合成一个数字会让用户拿到指错方向的解释（一个是路径推导出了问题，一个是
            // Codex 改了 rollout 格式，两者的处置完全不同）。
            None => report.path_rejected += 1,
        }
    }

    // ② 清单先落盘。首记即锁：已有条目一律不动。
    let mut manifest = match read_manifest_state(data_dir) {
        ManifestState::Loaded(m) => m,
        ManifestState::Missing => Manifest::default(),
        // 🔴 坏清单**不许**当成「没有」：那会以空清单起步、把原值整份覆盖掉。
        // 宁可这次不同步 —— 用户还能去看那份文件，而覆盖之后什么都没了。
        ManifestState::Corrupt(p) => return Err(corrupt_err(&p)),
    };
    let mut newly_recorded = false;
    for (_, s) in &todo {
        if !manifest.entries.iter().any(|e| e.rel_path == s.rel_path) {
            manifest.entries.push(ManifestEntry {
                rel_path: s.rel_path.clone(),
                original_provider: s.provider.clone(),
                thread_id: s.thread_id.clone(),
            });
            newly_recorded = true;
        }
    }
    if newly_recorded {
        manifest.target = target.to_string();
        manifest.synced_at = chrono::Utc::now().to_rfc3339();
        write_manifest(data_dir, &manifest)?;
    }

    // ③ 再改文件，并记下**确实改成功的** thread id。
    let mut done_ids: Vec<String> = Vec::new();
    for (path, s) in &todo {
        match rewrite_first_line(path, Some(target)) {
            Ok(Some(_)) => {
                report.changed += 1;
                if !s.thread_id.is_empty() {
                    done_ids.push(s.thread_id.clone());
                }
            }
            // 扫描时读到了、改写时又说无需改动：并发下有可能（Codex 自己刚改过）。
            // 这种情形 sqlite 那半也该跟上，故一样收进 done_ids。
            Ok(None) => {
                report.already_ok += 1;
                if !s.thread_id.is_empty() {
                    done_ids.push(s.thread_id.clone());
                }
            }
            Err(_) => report.skipped += 1,
        }
    }

    // ④ sqlite 只更新上面真的改成功的那些 thread。**不能无条件全表 UPDATE**：Codex 正在
    // 运行时 rollout 全被独占（`changed == 0`），而 sqlite 未必同时被锁 → 列表会显示这些
    // 对话已属 synaroute 而打开旧对话照旧 401，**替一个没完成的修复背书**。
    report.sqlite = sync_sqlite(home, data_dir, target, &done_ids);
    Ok(report)
}


// 还原

/// 按清单把会话的 provider 改回**原值**。返回 `Some(说明)` 表示确实动了。
///
/// 🔴 **清单缺失 → 什么都不改。** 绝不能退化成「全部改成 config 当前的 provider」：用户
/// 可能有我们从没动过的会话（cc-switch 的、手配的），那样会把它们一起改掉。同 `restore_one`
/// 在 `!backup.exists()` 时判「无需还原」—— 没有凭据就不猜。清单**坏了**是另一回事，
/// 走 [`corrupt_err`]。
///
/// ⚠️ 那道 `Missing` 早退与「空清单天然无操作」行为重叠（注入实测：换成 `unwrap_or_default`
/// 照样绿）。门保留：它表达语义边界，不该依赖另一处的副作用成立。真正能让判据变红的注入是
/// 「没有清单就自己扫一份出来」。
pub(in crate::tools) fn restore_at(home: &Path, data_dir: &Path) -> AppResult<Option<String>> {
    let manifest = match read_manifest_state(data_dir) {
        ManifestState::Loaded(m) => m,
        ManifestState::Missing => return Ok(None),
        // 与同步侧同一条纪律：坏清单不是「没有清单」。这里报错而不是静默什么都不改 ——
        // 后者会让用户以为已经还原干净了，而他的旧对话仍指着一个即将停掉的端口。
        ManifestState::Corrupt(p) => return Err(corrupt_err(&p)),
    };
    let mut restored = 0usize;
    let mut failed = 0usize;
    let mut gone = 0usize;

    for e in &manifest.entries {
        // 路径遏制：清单被手改成 `../..` 时不许我们去改 `$CODEX_HOME` 之外的文件。
        // 算作 failed 而不是静默跳过 —— 清单留着，排障时能看到这条没处理成功。
        let Some(path) = resolve_in_home(home, &e.rel_path) else {
            failed += 1;
            continue;
        };
        if !path.exists() {
            // 用户后来删掉了那个会话。不是错误 —— 它已经不需要还原了。
            gone += 1;
            continue;
        }
        let want = (!e.original_provider.is_empty()).then_some(e.original_provider.as_str());
        match rewrite_first_line(&path, want) {
            Ok(Some(_)) => restored += 1,
            Ok(None) => gone += 1,
            Err(_) => failed += 1,
        }
    }

    // sqlite 那半也要对称回滚 —— 只做 rollout 不做 sqlite 的话，还原之后 Desktop 的
    // 会话列表会长期标着 synaroute 而实际路由已回到官方，且**永不自愈**（除非用户再
    // 接入一次）。同步那半刻意写了 sqlite，这一半漏掉就是我们自己造的不一致。
    let db_note = restore_sqlite(home, &manifest.entries);

    // 全部处理完才删清单（同 `restore_one` 还原成功后删 `.bak`）。有失败就留着 ——
    // 用户退出 Codex 后再点一次停止就能补完。
    //
    // 🔴 **`db_note` 也要算进来**：`failed` 只统计 rollout 那半，于是「rollout 全还原成功、
    // sqlite 被锁」时清单照样被删 → 重试路径被永久切掉、元数据再也回不去。
    if failed == 0 && db_note.is_none() {
        let _ = fs::remove_file(manifest_path_in(data_dir));
    }
    if restored == 0 && failed == 0 {
        // sqlite 那半的错误也要有出口 —— 否则「rollout 都已被用户删掉、而列表元数据没能
        // 还原」这个组合会一个字都不说。
        return Ok(db_note.map(|e| format!("会话列表元数据未完全还原（不影响路由）：{e}")));
    }
    let mut note = format!("已把 {restored} 个历史会话的 provider 改回原值");
    if gone > 0 {
        note.push_str(&format!("（{gone} 个已不需要处理）"));
    }
    if failed > 0 {
        note.push_str(&format!(
            "；{failed} 个未能还原（文件被 Codex 占用、目录不可写，或清单条目已越界）\
             —— 若 Codex 正在运行，完全退出后再点一次「停止」即可补完，回滚清单已保留"
        ));
    }
    if let Some(e) = db_note {
        note.push_str(&format!("；会话列表元数据未完全还原（不影响路由）：{e}"));
    }
    Ok(Some(note))
}


// sqlite（best-effort：只影响 Desktop 的会话列表，不影响路由）

/// 候选库路径：`$CODEX_HOME/sqlite/*.db` 优先，回落 `state_5.sqlite`。
///
/// 🔴 **两个都要试**。Codex 正在从单文件迁到 `sqlite/` 目录（本机同时存在两者）。
/// 只改 legacy 那份的表现是：在已迁移的机器上**静默无效** —— 列表照旧显示旧 provider，
/// 而我们报「已同步」。
fn session_db_paths(home: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = fs::read_dir(home.join("sqlite")) {
        for e in entries.flatten() {
            let p = e.path();
            let ext = p.extension().and_then(|s| s.to_str()).unwrap_or_default();
            if p.is_file() && matches!(ext, "db" | "sqlite" | "sqlite3") {
                out.push(p);
            }
        }
        out.sort();
    }
    let legacy = home.join("state_5.sqlite");
    if legacy.is_file() {
        out.push(legacy);
    }
    out
}

/// 把指定 thread 的 `threads.model_provider` 改成 `target`。
///
/// `ids` = **rollout 那半确实改成功的**那些 thread id。空切片 → 一个字节都不写：那意味着
/// 路由压根没被修好，此时改列表元数据只会制造一个替未完成修复背书的假现场。
fn sync_sqlite(
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
fn restore_sqlite(home: &Path, entries: &[ManifestEntry]) -> Option<String> {
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
        TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % 10_000
    );
    fs::copy(db, dir.join(format!("{stem}.{ts}.bak")))
        .map_err(|e| format!("备份 {} 失败: {e}", db.display()))?;
    let wal = PathBuf::from(format!("{}-wal", db.to_string_lossy()));
    if wal.is_file() {
        let _ = fs::copy(&wal, dir.join(format!("{stem}-wal.{ts}.bak")));
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

fn set_threads_provider(db: &Path, provider: &str, ids: &[String]) -> Result<usize, String> {
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


// 对外入口（解析真实 `$CODEX_HOME` 与数据目录）

/// 接入时调用：同步历史会话，返回一句给用户看的说明（`None` = 没什么可说的）。
pub(in crate::tools) fn sync_to(target: &str) -> AppResult<Option<String>> {
    let home = super::codex_paths::codex_home()?;
    let data_dir = crate::store::data_dir::app_data_dir()?;
    Ok(describe(&sync_to_at(&home, &data_dir, target)?))
}

/// 把会话同步的结果并进接入成功的那条提示。
///
/// # 🔴 为什么挂在 `with_rollback` **之外**
///
/// 会话同步失败（Codex 占着文件、清单写不出去）时这次接入本身是**成功的** —— config 与
/// 模型目录都已写对，新建对话立刻可用。放进 `with_rollback` 会把一次成功的接入整个回滚，
/// 用户从「旧对话不能用」变成「全都不能用」，方向正好反了。反过来的顺序同样刻意：
/// **config 写失败时不该已经动过用户的会话文件**（同 `select_model` 那条）。
///
/// 返回 `String` 而不是 `AppResult`：这一层的任何失败都只降级成提示里的一句话。
pub(in crate::tools) fn append_sync_note(applied: String) -> String {
    match sync_to(super::MCP_CLIENT_NAME) {
        Ok(Some(note)) => format!("{applied}；{note}"),
        Ok(None) => applied,
        Err(e) => format!("{applied}；历史对话同步未完成：{e}"),
    }
}

/// 还原时调用。
pub(in crate::tools) fn restore_from_manifest() -> AppResult<Option<String>> {
    let home = super::codex_paths::codex_home()?;
    let data_dir = crate::store::data_dir::app_data_dir()?;
    restore_at(&home, &data_dir)
}

/// 把报告写成一句话。
///
/// 🔴 **只说「已指向」，不说「已恢复可用」** —— 见模块头那条已知边界：跨账号的
/// `encrypted_content` 可能仍然解不开。承诺一件我们保证不了的事，代价是用户按它排除掉
/// 真正的方向。
fn describe(r: &SyncReport) -> Option<String> {    let mut parts = Vec::new();
    if r.changed > 0 {
        parts.push(format!(
            "已把 {} 个历史对话指向 SynaRoute（重启 Codex 后生效；若某条旧对话仍报错，\
             那是它的推理内容由原账号加密所致，新建对话不受影响）",
            r.changed
        ));
    }
    if r.skipped > 0 {
        // 🔴 **条件句**：Windows 把「文件被独占」与「目录不可写」都映射成 `PermissionDenied`，
        // 分不开。只说「退出 Codex 再试」在只读卷上是无效指路 —— 用户照做后一字不变。
        parts.push(format!(
            "{} 个对话未能同步（文件被占用，或其所在目录不可写）\
             —— 若 Codex 正在运行，请完全退出 Codex 后再点一次接入",
            r.skipped
        ));
    }
    if r.unreadable > 0 {
        parts.push(format!("{} 个会话文件的首行无法解析，已跳过", r.unreadable));
    }
    if r.path_rejected > 0 {
        // 与上一条分开措辞：这里是路径本身出了问题，不是 rollout 的格式变了。
        parts.push(format!(
            "{} 个会话因路径无法安全定位而跳过（检查 CODEX_HOME 下是否有符号链接）",
            r.path_rejected
        ));
    }
    if let Some(db) = &r.sqlite {
        if let Some(e) = &db.error {
            // 只影响列表元数据，所以措辞刻意不像故障 —— 免得用户以为路由没修好。
            parts.push(format!("会话列表元数据未完全同步（不影响路由）：{e}"));
        }
    }
    (!parts.is_empty()).then(|| parts.join("；"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 夹具目录的进程内序号 —— 同 `tmp_path_for` 的理由：`timestamp_nanos` 在本机的量化
    /// 粒度只有 100ns，并发跑的两条用例会拿到同一个目录并互删对方的文件。本仓在
    /// `ccswitch::db_copy_path` 与 `codex_catalog` 的夹具上各踩过一次（后者是全量跑偶发红）。
    static FIXTURE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    fn tmp_home(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "synaroute-cxsess-{}-{}-{tag}",
            std::process::id(),
            FIXTURE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("sessions/2026/09/01")).unwrap();
        dir
    }

    /// 造一条 rollout。`eol` 让行尾成为可测维度（Codex 在 Windows 上写的是 `\n`，
    /// 但用户的文件经过别的工具处理后可能变成 `\r\n`，而我们不能替他归一）。
    fn write_rollout(home: &Path, sub: &str, id: &str, provider: &str, eol: &str) -> PathBuf {
        let dir = home.join(sub);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("rollout-2026-09-01T21-06-47-{id}.jsonl"));
        let meta = serde_json::json!({
            "timestamp": "2026-09-01T13:06:54.261Z",
            "ordinal": 0,
            "type": "session_meta",
            "payload": {
                "session_id": id, "id": id,
                "timestamp": "2026-09-01T13:06:47.003Z",
                "cwd": "C:/work/demo",
                "originator": "Codex Desktop",
                "cli_version": "0.151.0-alpha.7.2",
                "model_provider": provider,
                "history_mode": "legacy",
            }
        });
        let body = serde_json::json!({ "ordinal": 1, "type": "response_item" });
        fs::write(
            &path,
            format!("{meta}{eol}{body}{eol}"),
        )
        .unwrap();
        path
    }

    /// ① 首行的 provider 被改成目标值，**其余字节逐字节不变**（含行尾）。
    #[test]
    fn the_first_line_provider_is_rewritten_and_the_tail_is_byte_identical() {
        let home = tmp_home("rewrite");
        let path = write_rollout(&home, "sessions/2026/09/01", "t1", "openai", "\r\n");
        let before = fs::read(&path).unwrap();
        let tail_before = &before[before.iter().position(|b| *b == b'\n').unwrap() + 1..];

        let original = rewrite_first_line(&path, Some("synaroute")).unwrap();
        assert_eq!(original.as_deref(), Some("openai"), "必须如实返回原值");

        let after = fs::read(&path).unwrap();
        let first_end = after.iter().position(|b| *b == b'\n').unwrap();
        let first = String::from_utf8(after[..=first_end].to_vec()).unwrap();
        assert!(first.contains("\"model_provider\":\"synaroute\""));
        assert!(first.ends_with("\r\n"), "行尾必须保留 CRLF，实际: {first:?}");
        assert_eq!(&after[first_end + 1..], tail_before, "首行之后必须逐字节不变");
        let _ = fs::remove_dir_all(&home);
    }

    /// ② 已经指向我们的会话**一个字节都不许写** —— 判据用 mtime，因为只比内容的话
    /// 「重写成相同内容」也会绿，而那会让每次接入都全量重写几百个文件、并惊动 Codex
    /// 自己的文件监听。
    #[test]
    fn a_session_that_already_points_at_us_is_not_touched_at_all() {
        let home = tmp_home("noop");
        let path = write_rollout(&home, "sessions/2026/09/01", "t1", "synaroute", "\n");
        let mtime_before = fs::metadata(&path).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));

        assert_eq!(rewrite_first_line(&path, Some("synaroute")).unwrap(), None);
        assert_eq!(
            fs::metadata(&path).unwrap().modified().unwrap(),
            mtime_before,
            "无需改动时必须一个字节都不写"
        );
        let _ = fs::remove_dir_all(&home);
    }

    /// ③ 清单回滚的是**精确原值**，不是「一律改回 openai」那种猜测。
    ///
    /// 夹具刻意给两条不同的原值（`openai` 与 cc-switch 那类自定义 id）—— 只用一条的话
    /// 「硬编码回 openai」这个错误实现也会绿。
    #[test]
    fn the_manifest_restores_the_exact_original_not_a_guess() {
        let home = tmp_home("roundtrip");
        let data = home.join("appdata");
        let a = write_rollout(&home, "sessions/2026/09/01", "ta", "openai", "\n");
        let b = write_rollout(&home, "sessions/2026/09/01", "tb", "my-relay", "\n");

        let r = sync_to_at(&home, &data, "synaroute").unwrap();
        assert_eq!(r.changed, 2);
        for p in [&a, &b] {
            assert!(fs::read_to_string(p).unwrap().contains("\"synaroute\""));
        }

        let note = restore_at(&home, &data).unwrap().expect("应报告已还原");
        assert!(note.contains('2'), "应说明还原了 2 条: {note}");
        assert_eq!(provider_of(&a), "openai");
        assert_eq!(provider_of(&b), "my-relay", "必须回到它自己的原值，不是 openai");
        assert!(
            !manifest_path_in(&data).exists(),
            "全部成功后清单要删掉（同 restore_one 删 .bak）"
        );
        let _ = fs::remove_dir_all(&home);
    }

    fn provider_of(path: &Path) -> String {
        let line = read_first_line(path).unwrap();
        parse_meta(&line).unwrap().1
    }

    /// ④ 没有清单 → **一个文件都不许改**。
    ///
    /// 🔴 不能退化成「全部改成 config 当前的 provider」：用户可能有我们从没动过的会话
    /// （cc-switch 的、手配的），那样会把它们一起改掉。同 `restore_one` 在 `!backup.exists()`
    /// 时判「无需还原」—— 没有凭据就不猜。
    #[test]
    fn without_a_manifest_nothing_is_touched() {
        let home = tmp_home("nomanifest");
        let data = home.join("appdata");
        let p = write_rollout(&home, "sessions/2026/09/01", "t1", "synaroute", "\n");
        let before = fs::read(&p).unwrap();

        assert_eq!(restore_at(&home, &data).unwrap(), None);
        assert_eq!(fs::read(&p).unwrap(), before, "无清单时不许动任何文件");
        let _ = fs::remove_dir_all(&home);
    }

    /// ⑤ `archived_sessions` 也要覆盖 —— 漏掉它的表现是「归档里的旧对话仍 401」，
    /// 而用户不会想到「已归档」与「能不能用」有关系。
    #[test]
    fn archived_sessions_are_covered_too() {
        let home = tmp_home("archived");
        let data = home.join("appdata");
        let arch = write_rollout(&home, "archived_sessions/2026/08/30", "tz", "openai", "\n");

        let r = sync_to_at(&home, &data, "synaroute").unwrap();
        assert_eq!(r.changed, 1, "归档目录里的会话必须被同步");
        assert_eq!(provider_of(&arch), "synaroute");
        assert!(
            scan_at(&home).sessions.iter().any(|s| s.archived),
            "扫描结果要标出 archived —— 列表页据此分组"
        );
        let _ = fs::remove_dir_all(&home);
    }

    /// ⑥ 首行认不出 → 跳过并**计数**，不 panic、不静默丢掉。
    ///
    /// Codex 的 rollout 格式已经变过几次（本轮实测中 `history_mode: "full"` 就被新版拒过）。
    /// 全部认不出时用户该知道「同步了 0 条」是因为格式变了，而不是因为没有旧对话。
    #[test]
    fn an_unparsable_first_line_is_skipped_and_counted() {
        let home = tmp_home("unreadable");
        let data = home.join("appdata");
        let dir = home.join("sessions/2026/09/01");
        fs::write(dir.join("rollout-2026-09-01T00-00-00-bad1.jsonl"), "{not json\n").unwrap();
        // 合法 JSON 但不是 session_meta —— 万一 Codex 把首行换成别的记录类型，
        // 我们的表现必须是「认不出就别碰」，而不是往一条不相干的记录里插字段。
        fs::write(
            dir.join("rollout-2026-09-01T00-00-01-bad2.jsonl"),
            "{\"type\":\"response_item\"}\n",
        )
        .unwrap();
        let ok = write_rollout(&home, "sessions/2026/09/01", "good", "openai", "\n");

        let scan = scan_at(&home);
        assert_eq!(scan.unreadable, 2);
        assert_eq!(scan.sessions.len(), 1);

        let r = sync_to_at(&home, &data, "synaroute").unwrap();
        assert_eq!((r.changed, r.unreadable), (1, 2), "坏文件不阻断好文件");
        assert_eq!(provider_of(&ok), "synaroute");
        let _ = fs::remove_dir_all(&home);
    }

    /// ⑦ 🔴 **清单首记即锁**，与 `.bak` 同一条纪律。
    ///
    /// 反过来做（每次接入覆盖清单）是一条**数据丢失级**链路：第二次接入时那些文件已经是
    /// target 了 → 没有新条目 → 清单被覆盖成空 → 还原时一个都改不回来，用户的旧对话
    /// 永久指向一个已经停掉的代理端口。
    #[test]
    fn a_second_sync_must_not_overwrite_the_recorded_originals() {
        let home = tmp_home("firstwins");
        let data = home.join("appdata");
        let p = write_rollout(&home, "sessions/2026/09/01", "t1", "openai", "\n");

        assert_eq!(sync_to_at(&home, &data, "synaroute").unwrap().changed, 1);
        // 第二次接入：文件已经是 synaroute，无事可做。
        let second = sync_to_at(&home, &data, "synaroute").unwrap();
        assert_eq!((second.changed, second.already_ok), (0, 1));

        let m = read_manifest(&data).expect("清单不许在第二次接入后消失");
        assert_eq!(m.entries.len(), 1);
        assert_eq!(m.entries[0].original_provider, "openai", "原值必须还是第一次记的");

        restore_at(&home, &data).unwrap();
        assert_eq!(provider_of(&p), "openai");
        let _ = fs::remove_dir_all(&home);
    }

    /// ⑦之二 清单必须保住**接入前**那个值，哪怕中途 provider 变过好几手。
    ///
    /// ⚠️ 这条是注入实测补出来的：上一条用例里第二次接入 `changed == 0`，于是
    /// 「首记即锁」那道门**压根没被执行到** —— 把它改成 `if true` 照样全绿。
    /// 同 CLAUDE.md 那条「注入不变红时先怀疑用例没压到那个分支」。
    ///
    /// 复现的真实序列：接入 → 用户中途手改/用 cc-switch 切到别的 provider → 再接入。
    /// 此时第二次接入**有**新条目要写，两个缺陷才会现形：
    /// ① 不查重 → 清单里同一个路径两条，还原时后写的赢 → 回到中途那个值；
    /// ② 不合并磁盘上的旧清单 → 第一次记的原值被整份覆盖掉。
    /// 两者的后果一样：**用户再也回不到接入前的状态**。
    #[test]
    fn the_manifest_keeps_the_pre_apply_value_across_provider_churn() {
        let home = tmp_home("churn");
        let data = home.join("appdata");
        let t1 = write_rollout(&home, "sessions/2026/09/01", "t1", "openai", "\n");

        assert_eq!(sync_to_at(&home, &data, "synaroute").unwrap().changed, 1);
        // 用户中途切走用了别的 provider，那条会话被 Codex 记成 my-relay。
        rewrite_first_line(&t1, Some("my-relay")).unwrap();
        // 再新建一条，好让第二次接入确实有东西要写（否则压不到那道门）。
        write_rollout(&home, "sessions/2026/09/01", "t2", "openai", "\n");

        assert_eq!(sync_to_at(&home, &data, "synaroute").unwrap().changed, 2);
        let m = read_manifest(&data).unwrap();
        assert_eq!(m.entries.len(), 2, "两个路径各一条，不许重复记");
        let rec = m.entries.iter().find(|e| e.rel_path.contains("t1")).unwrap();
        assert_eq!(
            rec.original_provider, "openai",
            "必须是接入前那个值，不是中途那手 my-relay"
        );

        restore_at(&home, &data).unwrap();
        assert_eq!(provider_of(&t1), "openai", "还原要回到接入前，不是中途状态");
        let _ = fs::remove_dir_all(&home);
    }

    /// ⑧ 原本**没有** `model_provider` 字段的会话，还原后必须回到「没有这个字段」，
    /// 而不是留一个空串。
    ///
    /// 两者不是一回事：Codex 对缺字段有默认行为（用 config 的根 provider），对空串没有。
    /// 写空串等于把一个能工作的老会话改成一个未验证的形态。
    #[test]
    fn a_session_without_the_field_gets_the_field_removed_again() {
        let home = tmp_home("nofield");
        let data = home.join("appdata");
        let dir = home.join("sessions/2026/09/01");
        let path = dir.join("rollout-2026-09-01T00-00-00-old.jsonl");
        fs::write(
            &path,
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"old\"}}\n{\"ordinal\":1}\n",
        )
        .unwrap();

        assert_eq!(sync_to_at(&home, &data, "synaroute").unwrap().changed, 1);
        assert_eq!(provider_of(&path), "synaroute");

        restore_at(&home, &data).unwrap();
        let line = read_first_line(&path).unwrap();
        assert!(
            !line.contains("model_provider"),
            "原本没有这个字段，还原后不该留一个空串: {line}"
        );
        let _ = fs::remove_dir_all(&home);
    }

    fn make_threads_db(path: &Path, provider: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let c = rusqlite::Connection::open(path).unwrap();
        c.execute("CREATE TABLE threads (id TEXT, model_provider TEXT)", []).unwrap();
        c.execute("INSERT INTO threads VALUES ('t1', ?1)", [provider]).unwrap();
    }

    /// 测试里读清单的便捷壳：只在「确实加载出来」时给值，其余状态一律 panic ——
    /// 用例要断言的是内容，把 Missing/Corrupt 悄悄当成 `None` 会让判据变空洞。
    fn read_manifest(data_dir: &Path) -> Option<Manifest> {
        match read_manifest_state(data_dir) {
            ManifestState::Loaded(m) => Some(m),
            ManifestState::Missing => None,
            ManifestState::Corrupt(p) => panic!("清单损坏: {}", p.display()),
        }
    }

    fn provider_in_db(path: &Path) -> String {
        provider_of_id(path, "t1")
    }

    fn provider_of_id(db: &Path, id: &str) -> String {
        rusqlite::Connection::open(db)
            .unwrap()
            .query_row("SELECT model_provider FROM threads WHERE id=?1", [id], |r| r.get(0))
            .unwrap()
    }

    /// ⑫ 🔴 **清单写不出去时，一个会话文件都不许被改。**
    ///
    /// 代码审查抓出的第一版顺序错误：先改完全部文件、再写清单。`%APPDATA%` 盘满或权限
    /// 受限时，文件已被改成 target 而清单从未落盘 → 用户点停止时 [`restore_at`] 读不到
    /// 凭据、按设计什么都不改 → **那些旧对话永久指向一个已经停掉的代理端口**。
    ///
    /// 夹具用「把 data_dir 那个路径做成一个文件」来让 `create_dir_all` 失败 ——
    /// 跨平台都成立，不依赖权限位（Unix 的 rename 看目录权限、Windows 看文件属性，
    /// 拿只读位当夹具会在另一个平台上静默失效，本仓在 `pointer_is_ours` 上栽过一次）。
    #[test]
    fn a_manifest_that_cannot_be_written_aborts_before_touching_any_file() {
        let home = tmp_home("manifestfail");
        let blocked = home.join("appdata-is-a-file");
        fs::write(&blocked, b"not a directory").unwrap();
        let path = write_rollout(&home, "sessions/2026/09/01", "t1", "openai", "\n");
        let before = fs::read(&path).unwrap();

        let err = sync_to_at(&home, &blocked, "synaroute").unwrap_err();
        assert!(err.to_string().contains("清单") || err.to_string().contains("数据目录"));
        assert_eq!(
            fs::read(&path).unwrap(),
            before,
            "清单落盘失败时必须一个字节都还没改 —— 否则回滚凭据永久丢失"
        );
        let _ = fs::remove_dir_all(&home);
    }

    /// ⑬ 🔴 **sqlite 只更新 rollout 那半确实处理过的 thread，不是全表 UPDATE。**
    ///
    /// 第一版无条件 `UPDATE threads SET model_provider = ?`。于是 Codex 正在运行、
    /// rollout 全被独占时（changed == 0），列表照样被改成 synaroute —— 而打开旧对话仍
    /// 401。**列表替一个没完成的修复背了书**，用户据此排除掉「同步没生效」这个真方向。
    ///
    /// 判据用一条**孤立行**（sqlite 里有、磁盘上没有对应 rollout）：全表 UPDATE 会连它
    /// 一起改，按 id 收窄则不会。
    #[test]
    fn sqlite_only_touches_threads_whose_rollout_was_handled() {
        let home = tmp_home("sqlnarrow");
        let data = home.join("appdata");
        write_rollout(&home, "sessions/2026/09/01", "t1", "openai", "\n");
        let db = home.join("state_5.sqlite");
        make_threads_db(&db, "openai");
        rusqlite::Connection::open(&db)
            .unwrap()
            .execute("INSERT INTO threads VALUES ('orphan', 'openai')", [])
            .unwrap();

        let r = sync_to_at(&home, &data, "synaroute").unwrap();
        assert_eq!((r.changed, r.sqlite.as_ref().unwrap().updated), (1, 1));
        assert_eq!(provider_in_db(&db), "synaroute", "有 rollout 的那条要改");
        assert_eq!(
            provider_of_id(&db, "orphan"),
            "openai",
            "没有对应 rollout 的孤立行不许被改 —— 那是全表 UPDATE 的指纹"
        );
        let _ = fs::remove_dir_all(&home);
    }

    /// ⑬之二 `done_ids` 为空（rollout 那半一条都没成功）时，一个字节都不许写。
    #[test]
    fn an_empty_done_list_leaves_sqlite_untouched() {
        let home = tmp_home("sqlempty");
        let data = home.join("appdata");
        let db = home.join("state_5.sqlite");
        make_threads_db(&db, "openai");

        let out = sync_sqlite(&home, &data, "synaroute", &[]).unwrap();
        assert_eq!(out, SqliteOutcome::default());
        assert_eq!(provider_in_db(&db), "openai");
        assert!(!data.join("backups").exists(), "没东西要写时连备份都不该产生");
        let _ = fs::remove_dir_all(&home);
    }

    /// 把文件设成只读，用来制造「库写不进去」。
    ///
    /// `Permissions::set_readonly` 是 std 的**跨平台** API（Windows 设只读属性、Unix 清写位），
    /// 比 `PermissionsExt` 那套 Unix-only 的手法适合当夹具 —— 本仓有过「夹具只在开发机
    /// 平台上成立」的教训。
    fn set_readonly(path: &Path, ro: bool) {
        let mut p = fs::metadata(path).unwrap().permissions();
        p.set_readonly(ro);
        fs::set_permissions(path, p).unwrap();
    }

    /// ⑭ 🔴 **多库时「一个成功一个失败」必须如实上报，不能汇总成纯成功。**
    ///
    /// 第一版只在 `total == 0` 时返回错误，于是本机这种「同时有 sqlite/*.db 与
    /// state_5.sqlite」的机器上，Codex 只锁住其中一个时用户看不到有一半没同步。
    /// 同 CLAUDE.md 里「两条丢日志路径不能合成一个数字」那条：把部分成功呈现成成功，
    /// 排障的人会拿它当答案。
    #[test]
    fn a_partly_failed_sqlite_sync_is_reported_not_swallowed() {
        let home = tmp_home("sqlpartial");
        let data = home.join("appdata");
        write_rollout(&home, "sessions/2026/09/01", "t1", "openai", "\n");
        let good = home.join("sqlite/good.db");
        let bad = home.join("state_5.sqlite");
        make_threads_db(&good, "openai");
        make_threads_db(&bad, "openai");
        set_readonly(&bad, true);

        let out = sync_to_at(&home, &data, "synaroute").unwrap().sqlite.unwrap();
        assert_eq!(out.updated, 1, "可写的那个库要改成功");
        assert!(out.error.is_some(), "另一个库的失败不许被吞掉");
        assert_eq!(provider_in_db(&good), "synaroute");

        set_readonly(&bad, false); // Windows 上只读文件删不掉
        let _ = fs::remove_dir_all(&home);
    }

    /// ⑮ 🔴 **还原要对称回滚 sqlite。**
    ///
    /// 同步那半刻意写了 `threads.model_provider`（为的是 Desktop 的会话列表），还原那半
    /// 漏掉它就是我们自己造的长期不一致：config 与 rollout 都已回到官方，而列表仍标着
    /// synaroute，且**永不自愈**（除非用户再接入一次）。
    #[test]
    fn restoring_also_puts_the_sqlite_provider_back() {
        let home = tmp_home("sqlrestore");
        let data = home.join("appdata");
        let roll = write_rollout(&home, "sessions/2026/09/01", "t1", "openai", "\n");
        let db = home.join("state_5.sqlite");
        make_threads_db(&db, "openai");

        sync_to_at(&home, &data, "synaroute").unwrap();
        assert_eq!(provider_in_db(&db), "synaroute");

        restore_at(&home, &data).unwrap().expect("应报告已还原");
        assert_eq!(provider_of(&roll), "openai");
        assert_eq!(provider_in_db(&db), "openai", "sqlite 那半也必须回到原值");
        let _ = fs::remove_dir_all(&home);
    }

    /// ⑱ 🔴 **坏清单不是「没有清单」。**
    ///
    /// 第二轮代码审查抓出的静默数据丢失：`read_manifest` 把解析失败和文件不存在归成同一个
    /// `None` → 同步侧 `unwrap_or_default()` 以空清单起步 → 首记即锁失效 → 覆盖写回 →
    /// **原值永久消失且用户毫无提示**；还原侧则静默什么都不改。
    ///
    /// 触发不需要外力：`write_manifest` 原先用裸 `fs::write`（先截断再写），在它写入中途
    /// 崩溃/断电/盘满就会留下一份被我们自己写坏的 JSON。
    #[test]
    fn a_corrupt_manifest_is_never_treated_as_absent() {
        let home = tmp_home("corrupt");
        let data = home.join("appdata");
        let roll = write_rollout(&home, "sessions/2026/09/01", "t1", "openai", "\n");
        let before = fs::read(&roll).unwrap();
        fs::create_dir_all(&data).unwrap();
        fs::write(manifest_path_in(&data), b"{\"entries\": [ truncated").unwrap();

        let err = sync_to_at(&home, &data, "synaroute").unwrap_err();
        assert!(err.to_string().contains("回滚清单读不出来"), "要如实说清单坏了: {err}");
        assert_eq!(fs::read(&roll).unwrap(), before, "坏清单时不许动任何会话文件");
        assert!(
            fs::read_to_string(manifest_path_in(&data)).unwrap().contains("truncated"),
            "坏清单必须留在盘上 —— 覆盖掉它就等于把用户最后的线索也删了"
        );

        let err = restore_at(&home, &data).unwrap_err();
        assert!(err.to_string().contains("回滚清单读不出来"));
        let _ = fs::remove_dir_all(&home);
    }

    /// ⑱之二 清单是原子写的：中途失败不会留下半份 JSON，成功后不留 `.tmp`。
    #[test]
    fn the_manifest_is_written_atomically() {
        let home = tmp_home("atomic");
        let data = home.join("appdata");
        write_manifest(&data, &Manifest::default()).unwrap();
        let leftovers: Vec<String> = fs::read_dir(&data)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "不该留下临时文件: {leftovers:?}");
        // 源码级：写清单的那条路径必须经 rename，不许退回裸 fs::write（那会留下截断文件，
        // 正是上一条判据的输入）。
        let src = crate::proxy::custom_headers::production_code_only(include_str!(
            "codex_sessions.rs"
        ));
        let at = src.find("fn write_manifest").expect("函数改名了，请同步本判据");
        let end = src[at..].find("\n}").map(|i| at + i).unwrap_or(src.len());
        assert!(
            src[at..end].contains("fs::rename("),
            "write_manifest 必须走临时文件 + rename"
        );
        let _ = fs::remove_dir_all(&home);
    }

    /// ⑲ 🔴 **sqlite 回滚失败时清单必须留着。**
    ///
    /// 删清单的条件原先只看 `failed`（rollout 那半），于是「rollout 全还原成功、sqlite 被
    /// 锁」时清单照样被删 → 重试路径被永久切掉、`threads.model_provider` 停在 synaroute
    /// 再也回不去。那正好泄漏了本轮刚加的对称回滚。
    #[test]
    fn the_manifest_survives_a_failed_sqlite_rollback() {
        let home = tmp_home("dbfailkeep");
        let data = home.join("appdata");
        let roll = write_rollout(&home, "sessions/2026/09/01", "t1", "openai", "\n");
        let db = home.join("state_5.sqlite");
        make_threads_db(&db, "openai");
        sync_to_at(&home, &data, "synaroute").unwrap();

        set_readonly(&db, true);
        let note = restore_at(&home, &data).unwrap().unwrap();
        assert_eq!(provider_of(&roll), "openai", "rollout 那半照旧要还原");
        assert!(note.contains("列表元数据"), "要如实报出 sqlite 没还原成功");
        assert!(
            manifest_path_in(&data).exists(),
            "sqlite 还没回滚成功时清单必须留着，否则重试路径被永久切掉"
        );

        set_readonly(&db, false);
        let _ = fs::remove_dir_all(&home);
    }

    /// ⑳ 超过一批（`SQL_CHUNK`）的 id 必须全部被更新 —— 分批本身不能把结果改错。
    ///
    /// ⚠️ **这条压不到它真正要防的那个边界**：每个 id 是一个绑定参数，而 bundled 的 SQLite
    /// 上限是 32766（3.32 之前才是 999）。要让「不分批」真的失败得造三万多行，成本不合理 ——
    /// 注入实测确认：把 `chunks(SQL_CHUNK)` 换成一批全发，505 个参数照样成功、判据仍绿。
    /// 故那一半靠下面的**源码级判据**钉形态，同 `only_v6_must_be_set_explicitly` 的分工。
    #[test]
    fn ids_beyond_one_sql_batch_are_all_updated() {
        let home = tmp_home("chunk");
        let db = home.join("state_5.sqlite");
        make_threads_db(&db, "openai");
        let conn = rusqlite::Connection::open(&db).unwrap();
        let n = SQL_CHUNK + 5;
        let ids: Vec<String> = (0..n).map(|i| format!("id{i}")).collect();
        for id in &ids {
            conn.execute("INSERT INTO threads VALUES (?1, 'openai')", [id]).unwrap();
        }
        drop(conn);

        let updated = set_threads_provider(&db, "synaroute", &ids).unwrap();
        assert_eq!(updated, n, "跨批的 id 一个都不能漏");
        assert_eq!(provider_of_id(&db, "id0"), "synaroute");
        assert_eq!(provider_of_id(&db, &format!("id{}", n - 1)), "synaroute");
        let _ = fs::remove_dir_all(&home);
    }

    /// ⑳之二 源码级：`IN (?,…)` 必须分批。
    ///
    /// 不分批的失效形态是 `too many SQL variables` —— 而它「安静」（路由不受影响、只有
    /// Desktop 的会话列表不同步），所以更需要机械判据而不是靠人记得。
    ///
    /// **批大小的上限不在这里测** —— 它由 `SQL_CHUNK` 旁边那条 `const _: () = assert!(…)`
    /// 在**编译期**钉住（调过 999 直接编译不过）。这里只管「分批这件事还在做」。
    #[test]
    fn the_id_list_must_be_chunked_below_the_most_conservative_sqlite_limit() {
        let src = crate::proxy::custom_headers::production_code_only(include_str!(
            "codex_sessions.rs"
        ));
        let at = src.find("fn set_threads_provider").expect("函数改名了，请同步本判据");
        let end = src[at..].find("\n}").map(|i| at + i).unwrap_or(src.len());
        assert!(
            src[at..end].contains("ids.chunks(SQL_CHUNK)"),
            "必须按 SQL_CHUNK 分批 —— 一次性展开全部 id 会在会话数上万时整条 UPDATE 失败"
        );
    }

    /// ㉑ 推导不出相对路径时返回 `None`，不许兜底成绝对路径。
    ///
    /// 兜底会同时丢掉两条性质：清单跨机器不认领别人的文件、以及 `resolve_in_home` 那道
    /// 遏制（它必然拒绝绝对路径 → 整批会话被算成越界，而用户看到的解释指向错误方向）。
    #[test]
    fn a_path_outside_home_has_no_relative_form() {
        #[cfg(windows)]
        let (home, outside) = (Path::new("C:\\codex"), Path::new("D:\\elsewhere\\x.jsonl"));
        #[cfg(not(windows))]
        let (home, outside) = (Path::new("/codex"), Path::new("/elsewhere/x.jsonl"));

        assert_eq!(rel_of(home, outside), None, "越界路径不该有相对形态");
        assert_eq!(
            rel_of(home, &home.join("sessions").join("a.jsonl")).as_deref(),
            Some("sessions/a.jsonl"),
            "正常路径要归一成 / 分隔"
        );
    }

    /// ⑯ 写库之前必须先备份，且备份内容是**动手之前**那一份。
    ///
    /// 计划里承诺过这一步，第一版漏掉了（代码审查抓出）。叠加上「还原不回滚 sqlite」那条
    /// 之后，用户一旦想回到接入前的元数据状态就没有任何凭据 —— 反向 UPDATE 也做不到，
    /// 因为原值已经被覆盖且没记在任何地方。
    #[test]
    fn the_db_is_backed_up_with_its_pre_write_content() {
        let home = tmp_home("dbbackup");
        let data = home.join("appdata");
        write_rollout(&home, "sessions/2026/09/01", "t1", "openai", "\n");
        let db = home.join("state_5.sqlite");
        make_threads_db(&db, "openai");

        sync_to_at(&home, &data, "synaroute").unwrap();
        let dir = data.join("backups").join("codex-sqlite");
        let baks: Vec<PathBuf> = fs::read_dir(&dir).unwrap().flatten().map(|e| e.path()).collect();
        let main = baks
            .iter()
            .find(|p| p.file_name().unwrap().to_string_lossy().starts_with("state_5.sqlite."))
            .expect("主库应有一份备份");
        assert_eq!(
            provider_of_id(main, "t1"),
            "openai",
            "备份必须是写之前那一份，否则它什么都救不回来"
        );
        let _ = fs::remove_dir_all(&home);
    }

    /// ⑯之二 备份数量有上限 —— 加了保留就必须同时加清理（同 `log_rotate` 那条：
    /// 只做滚动不做清理时，「上限看着在工作、实际只管住第一个文件」）。
    ///
    /// ⚠️ 轮数取 `KEEP*2 + 3`：`prune_backups` 的窗口是 `KEEP*2`（按「主文件 + wal」估），
    /// 而这个夹具**每轮产 2 个文件**（主 + wal）。第一版的夹具没有 wal、每轮只产 1 个，
    /// 只跑 `KEEP+3 = 6` 轮时恰好等于窗口 → **不裁剪也过**，注入实测仍绿。判据必须压过边界。
    ///
    /// # 🔴 为什么排序键要把 `-wal` 挪到末位（2026-09-04 审查发现的真缺陷）
    ///
    /// 备份的文件名是 `{stem}.{ts}.bak` 与 `{stem}-wal.{ts}.bak`。裸 `mine.sort()` 比的是
    /// 整个文件名，而 `-`(0x2D) **小于** `.`(0x2E) —— 于是**全部** wal 备份排在**全部**主文件
    /// 之前，两类被分成了两段而不是按轮次交错。窗口 `KEEP*2` 从前面删，实际语义就变成
    /// 「先把所有 wal 删光，wal 不够了才删最旧的主文件」：
    ///
    /// - 跑 5 轮 → 10 个文件，删前 4 个全是 wal → 留下 **5 份主文件 + 1 份 wal**
    ///   （而不是宣称的「3 份」）；
    /// - 更要紧的是**配对被拆散**：留下的旧主文件没有它的 wal，而 WAL 模式下主文件可能是
    ///   旧快照 —— 备份 wal 的全部理由就是这个。原注释还写着「方向安全」，那句话在有 wal
    ///   的库上不成立（这类「声称分析过」的注释比没有注释更贵）。
    ///
    /// 键 `(时间戳段, is_wal)` 让同一轮的两个文件相邻、且主文件在前：窗口边界落在一对中间时
    /// 丢掉的是主文件、留下一个无害的孤儿 wal，而不是反过来。
    #[test]
    fn old_db_backups_are_pruned() {
        let home = tmp_home("dbprune");
        let data = home.join("appdata");
        let roll = write_rollout(&home, "sessions/2026/09/01", "t1", "openai", "\n");
        let db = home.join("state_5.sqlite");
        make_threads_db(&db, "openai");

        let rounds = DB_BACKUP_KEEP * 2 + 3;
        // 每轮把 rollout 拨回 openai，好让下一轮真的有东西要写（也就真的会备份）。
        for _ in 0..rounds {
            // WAL 侧写文件必须在**每轮备份之前**在场，否则本判据压不到「主/wal 配对」这一维。
            // 每轮重写：rusqlite 打开非 WAL 模式的库时会把这个不合法的 `-wal` 清掉，
            // 只在循环外写一次的话第 2 轮起就没有 wal 备份了（第一版实测如此）。
            fs::write(home.join("state_5.sqlite-wal"), b"fake-wal").unwrap();
            rewrite_first_line(&roll, Some("openai")).unwrap();
            sync_to_at(&home, &data, "synaroute").unwrap();
        }
        let n = fs::read_dir(data.join("backups").join("codex-sqlite"))
            .unwrap()
            .flatten()
            .count();
        assert!(n > 1, "备份不该互相覆盖（秒级时间戳会让同一秒内的几次备份同名）");
        assert!(
            n <= DB_BACKUP_KEEP * 2,
            "备份数应被裁到上限，实际 {n} 份（跑了 {rounds} 轮；无界增长会把数据目录堆满）"
        );
        // 🔴 留下的每一份主文件都必须还有它配对的 wal —— 裸 `sort()` 会先删光所有 wal，
        // 留下一堆没有 wal 的主文件备份，而那时主文件可能只是个旧快照（见上方文档）。
        let names: Vec<String> = fs::read_dir(data.join("backups").join("codex-sqlite"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        for main in names.iter().filter(|n| n.starts_with("state_5.sqlite.")) {
            let want = main.replace("state_5.sqlite.", "state_5.sqlite-wal.");
            assert!(
                names.contains(&want),
                "主库备份 {main} 没有配对的 wal（现有：{names:?}）—— 用它恢复会拿到旧快照"
            );
        }
        let _ = fs::remove_dir_all(&home);
    }

    /// ⑰ 🔴 **清单里越界的 rel_path 必须被拒绝。**
    ///
    /// 清单是 `%APPDATA%\SynaRoute\` 下的普通 JSON，被别的程序或用户手改之后可以写成
    /// `../..`，而我们拿它去**改写文件**。危害受限（目标首行必须是合法 `session_meta`），
    /// 但本仓对同类原语一贯设遏制 —— `aggregate.rs` 修过两条路径穿透。
    #[test]
    fn a_manifest_entry_that_escapes_codex_home_is_refused() {
        let home = tmp_home("escape");
        let data = home.join("appdata");
        // 受害文件放在 home 之外，且长得像一个合法 rollout（否则「没被改」可能只是因为
        // 它压根不是 session_meta —— 那样判据就测不到遏制这一层）。
        let outside = home.parent().unwrap().join("synaroute-escape-victim.jsonl");
        fs::write(
            &outside,
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"v\",\"model_provider\":\"victim\"}}\n",
        )
        .unwrap();
        let before = fs::read(&outside).unwrap();

        fs::create_dir_all(&data).unwrap();
        let rel = format!("../{}", outside.file_name().unwrap().to_string_lossy());
        write_manifest(
            &data,
            &Manifest {
                target: "synaroute".into(),
                synced_at: "x".into(),
                entries: vec![ManifestEntry {
                    rel_path: rel,
                    original_provider: "openai".into(),
                    thread_id: "v".into(),
                }],
            },
        )
        .unwrap();

        let note = restore_at(&home, &data).unwrap().expect("越界条目要如实报出来");
        assert!(note.contains("越界") || note.contains("未能还原"));
        assert_eq!(fs::read(&outside).unwrap(), before, "绝不许改 CODEX_HOME 之外的文件");
        assert!(
            manifest_path_in(&data).exists(),
            "有未处理成功的条目时清单要留着，排障才看得到"
        );
        let _ = fs::remove_file(&outside);
        let _ = fs::remove_dir_all(&home);
    }

    /// ⑰之二 遏制判据本身的边界（纯函数，逐形态穷举）。
    #[test]
    fn resolve_in_home_refuses_everything_that_leaves_the_root() {
        #[cfg(windows)]
        let home = Path::new("C:\\codex");
        #[cfg(not(windows))]
        let home = Path::new("/codex");

        assert!(resolve_in_home(home, "sessions/2026/a.jsonl").is_some());
        for bad in [
            "../outside.jsonl",
            "sessions/../../outside.jsonl",
            "./a.jsonl", // `.` 一并拒掉：同一个文件两种写法会让「首记即锁」认不出是同一条
            "",
        ] {
            assert!(resolve_in_home(home, bad).is_none(), "{bad:?} 应被拒绝");
        }
        // 绝对路径：按平台给，因为 `is_absolute()` 走宿主语义（本仓在 macOS CI 上
        // 为此连红三个版本）。
        #[cfg(windows)]
        assert!(resolve_in_home(home, "C:\\Windows\\x.jsonl").is_none());
        #[cfg(not(windows))]
        assert!(resolve_in_home(home, "/etc/x.jsonl").is_none());
    }

    /// ⑨ **没有 sqlite 库时 rollout 那半照样成功。** 这是模块头那条「rollout 是权威」的
    /// 可执行形态 —— 实测中夹具根本没有 sqlite 记录，`thread/resume` 依然按 rollout 路由。
    #[test]
    fn a_missing_session_db_does_not_fail_the_rollout_half() {
        let home = tmp_home("nodb");
        let data = home.join("appdata");
        let p = write_rollout(&home, "sessions/2026/09/01", "t1", "openai", "\n");

        let r = sync_to_at(&home, &data, "synaroute").unwrap();
        assert_eq!(r.changed, 1);
        assert!(r.sqlite.is_none(), "没找到库应报 None，而不是失败");
        assert_eq!(provider_of(&p), "synaroute");
        let _ = fs::remove_dir_all(&home);
    }

    /// ⑩ 新布局 `sqlite/*.db` 与 legacy `state_5.sqlite` **两个都要写**。
    ///
    /// 🔴 只写 legacy 的表现是：在已迁到 `sqlite/` 目录的机器上**静默无效** —— 列表照旧
    /// 显示旧 provider，而我们报「已同步」。Codex 正在迁移，本机同时存在两者。
    #[test]
    fn both_the_new_and_legacy_session_dbs_are_updated() {
        let home = tmp_home("bothdb");
        let data = home.join("appdata");
        write_rollout(&home, "sessions/2026/09/01", "t1", "openai", "\n");
        let fresh = home.join("sqlite/codex-dev.db");
        let legacy = home.join("state_5.sqlite");
        make_threads_db(&fresh, "openai");
        make_threads_db(&legacy, "openai");

        let r = sync_to_at(&home, &data, "synaroute").unwrap();
        assert_eq!(
            r.sqlite,
            Some(SqliteOutcome { updated: 2, error: None }),
            "两个库各一行都该被改到"
        );
        assert_eq!(provider_in_db(&fresh), "synaroute");
        assert_eq!(provider_in_db(&legacy), "synaroute", "legacy 那份不能漏");
        let _ = fs::remove_dir_all(&home);
    }

    /// ⑪ 库在但没有 `model_provider` 列（Codex 改了 schema）→ 跳过，不报错。
    #[test]
    fn a_session_db_without_the_column_is_skipped_quietly() {
        let home = tmp_home("nocol");
        let data = home.join("appdata");
        write_rollout(&home, "sessions/2026/09/01", "t1", "openai", "\n");
        let db = home.join("state_5.sqlite");
        let c = rusqlite::Connection::open(&db).unwrap();
        c.execute("CREATE TABLE threads (id TEXT)", []).unwrap();
        drop(c);

        let r = sync_to_at(&home, &data, "synaroute").unwrap();
        assert_eq!(r.changed, 1, "rollout 那半不受影响");
        assert_eq!(
            r.sqlite,
            Some(SqliteOutcome::default()),
            "缺列应当是「0 行、无错误」而不是失败"
        );
        let _ = fs::remove_dir_all(&home);
    }

    /// 🔴 **接线判据：上面 11 条全都直调函数，把两处调用点摘掉它们照样全绿** ——
    /// 而「接入不同步 / 还原不回滚」正是缺陷本体，且失效完全静默。
    ///
    /// 本仓已在同一类盲区上栽过 13 次（`route_meta` 的每个出口、`lan_guard` 的 peer、
    /// `log_rotate` 的写线程、`mcp::handle_http` 的 path、`model_choice` 的转发路径……）。
    /// 教训一律是同一句：**单元覆盖了组件 ≠ 覆盖了调用它的那条线**。
    ///
    /// 用 `production_code_only` 而不是 `production_slice`：本模块与 `codex_catalog` 的
    /// 文档注释里都写着 `codex_sessions::restore_from_manifest`，不剥注释的话注释会替代码
    /// 满足断言。本仓已三次栽在这上面（`data-dir-env-name-must-match` /
    /// `userPrefsParity` / `only_v6_must_be_set_explicitly`）。
    #[test]
    fn the_apply_and_restore_paths_must_actually_call_into_this_module() {
        let prod = crate::proxy::custom_headers::production_code_only;

        let codex = prod(include_str!("codex.rs"));
        let at = codex.find("pub(super) fn apply(").expect("apply 改名了，请同步本判据");
        let end = codex[at..].find("\n}").map(|i| at + i).unwrap_or(codex.len());
        assert!(
            codex[at..end].contains("codex_sessions::append_sync_note("),
            "接入路径必须同步历史对话的 provider —— 不然「旧对话 401、新建正常」这个缺陷原样存在"
        );

        let catalog = prod(include_str!("codex_catalog.rs"));
        let at = catalog
            .find("pub(in crate::tools) fn restore_side_files")
            .expect("restore_side_files 改名了，请同步本判据");
        let end = catalog[at..].find("\n}").map(|i| at + i).unwrap_or(catalog.len());
        assert!(
            catalog[at..end].contains("codex_sessions::restore_from_manifest("),
            "还原路径必须回滚会话 provider —— 否则还原后旧对话指向一个已经停掉的代理端口"
        );
    }

    /// 路径必须经 `codex_paths::codex_home()`，不许自己拼 `.codex`。
    ///
    /// 与 `codex_paths.rs` 里那条判据同源：设了 `CODEX_HOME` 的机器上写错目录的表现是
    /// 「SynaRoute 说同步了 N 条、Codex 读的是另一份」—— 静默且极难归因。
    #[test]
    fn the_entrypoints_must_resolve_codex_home_through_the_single_source() {
        let src = crate::proxy::custom_headers::production_code_only(include_str!(
            "codex_sessions.rs"
        ));
        let calls = src.matches("codex_paths::codex_home()").count();
        assert_eq!(calls, 2, "sync_to 与 restore_from_manifest 各一次");
        assert!(
            !src.contains("home_dir()"),
            "不许直接拼 dirs::home_dir() —— 那是跨机器 401 的成因"
        );
    }

    /// 用户可见文案的判据：**只说「已指向」，不说「已恢复可用」**。
    ///
    /// 跨账号恢复时 reasoning 的 `encrypted_content` 可能解不开，所以「旧对话能继续」
    /// 是我们手里的信息支撑不了的承诺。本仓记过一次同类：某处告警写「其它 Key 会自动接管」，
    /// 而 791 毫秒后被系统自己的另一条消息否证 —— 用户据此排除了真正的方向。
    #[test]
    fn the_user_facing_note_never_promises_that_old_chats_will_work() {
        let note = describe(&SyncReport { changed: 3, ..Default::default() }).unwrap();
        assert!(note.contains('3'));
        assert!(note.contains("重启"), "必须说要重启 Codex —— 模型目录是 startup only");
        assert!(note.contains("加密"), "必须如实交代 encrypted_content 这条边界");
        assert!(!note.contains("已恢复"), "不许承诺旧对话恢复可用");

        // 被占用时要给**可行动**的出路，而不是只报一个数字。
        // 未能同步时要给**覆盖两种成因**的条件句，而不是只说「退出 Codex」——
        // 只读卷/权限受限时那句话是无效指路，用户照做后一字不变。
        let skipped = describe(&SyncReport { skipped: 2, ..Default::default() }).unwrap();
        assert!(skipped.contains("退出 Codex"));
        assert!(skipped.contains("不可写"), "必须同时给出「目录不可写」这一支");

        // 全都已经对了 → 一个字都不说。否则每次接入都多一行无信息量的话，
        // 而那种噪音会把真正要看的提示挤掉。
        assert_eq!(describe(&SyncReport { already_ok: 5, ..Default::default() }), None);

        // 路径越界与「首行认不出」必须给不同的解释：一个查符号链接、一个是 Codex 换了
        // rollout 格式。合成一句会把用户送去查错的东西。
        let bad_path = describe(&SyncReport { path_rejected: 1, ..Default::default() }).unwrap();
        assert!(bad_path.contains("符号链接"), "路径类问题要指向路径: {bad_path}");
        assert!(!bad_path.contains("首行"), "不许套用「首行无法解析」那句解释");

        // sqlite 那半失败时的措辞刻意不像故障 —— 免得用户以为路由没修好。
        let db = describe(&SyncReport {
            sqlite: Some(SqliteOutcome {
                updated: 0,
                error: Some("locked".into()),
            }),
            ..Default::default()
        })
        .unwrap();
        assert!(db.contains("不影响路由"));
    }

    /// 用**真实 codex 二进制**验证「改完 rollout 首行，Codex 就按新值恢复会话」。
    ///
    /// # 四组实测的对照表（模块头指到这里）
    ///
    /// | # | 入口 | rollout 首行记的 | 传 `modelProvider` 参数 | 请求实际打到 |
    /// |---|---|---|---|---|
    /// | 1 | `codex exec resume` | stale | — | **config 的**（CLI 那条路不受影响） |
    /// | 2 | app-server `thread/resume` | stale | `"stale"` | stale |
    /// | 3 | app-server `thread/resume` | stale | **不传** | **stale**（自己从 rollout 读的） |
    /// | 4 | app-server `thread/resume` | 改成 current | 不传 | **current** ✅ 改文件即修复 |
    ///
    /// 第 3 行是这个模块存在的理由：**不需要客户端传参**，app-server 自己读会话快照，
    /// 所以我们改 `config.toml` 再正确也管不到旧会话。第 4 行是正向验证，也就是本测试的
    /// 第 2 步。第 1 行解释了为什么 CLI 用户从来没报过这个问题。
    ///
    /// 上面那些判据只证明「我们写对了文件」，这一条证明**Codex 真的按它路由** ——
    /// 同 `codex_catalog` 里 `catalog_is_accepted_by_the_real_codex_binary` 的分工。
    ///
    /// 它读 `thread/resume` 响应里的 `modelProvider`：那个字段在协议里是 **required**，
    /// 且本轮已用两个本地探针核对过「它等于请求实际打到的那个上游」，所以拿它当判据
    /// 不需要再起 HTTP 服务。
    ///
    /// 🔴 **对照组不能省**：第 1 步先断言「不改就走 stale」。没有它，「改完走 current」
    /// 也可能只是因为 Codex 一直读 config（那样这个模块就是白做的）。
    ///
    /// 跑法（`codex.exe` 在 WindowsApps 下带 ACL、要先 `cp` 出来，约 313 MB）：
    /// ```text
    /// SYNAROUTE_CODEX_PROBE=<path>/codex.exe cargo test --lib codex_sessions -- --ignored
    /// ```
    #[test]
    #[ignore = "需要真实 codex 二进制，见函数文档"]
    fn the_real_codex_binary_resumes_with_the_provider_we_wrote() {
        let exe = std::env::var("SYNAROUTE_CODEX_PROBE")
            .expect("请把 SYNAROUTE_CODEX_PROBE 指向 codex.exe");
        let home = tmp_home("realbin");
        let data = home.join("appdata");
        let sid = "01a05d14-4e5b-7773-b425-0000000000ff";
        fs::write(
            home.join("config.toml"),
            "model = \"probe-model\"\nmodel_provider = \"current\"\n\n\
             [model_providers.current]\nname = \"current\"\n\
             base_url = \"http://127.0.0.1:1/v1\"\nwire_api = \"responses\"\n\n\
             [model_providers.stale]\nname = \"stale\"\n\
             base_url = \"http://127.0.0.1:2/v1\"\nwire_api = \"responses\"\n",
        )
        .unwrap();
        write_rollout(&home, "sessions/2026/09/01", sid, "stale", "\n");

        assert_eq!(
            resume_provider(&exe, &home, sid),
            "stale",
            "对照组：会话自带的 provider 必须压过 config 的根 provider —— \
             它不成立的话这个模块就没有存在理由"
        );

        assert_eq!(sync_to_at(&home, &data, "current").unwrap().changed, 1);
        assert_eq!(
            resume_provider(&exe, &home, sid),
            "current",
            "同步之后 Codex 必须按我们写的值恢复会话"
        );
        let _ = fs::remove_dir_all(&home);
    }

    /// 起一次 `codex app-server`，`thread/resume` 之后读回它认定的 `modelProvider`。
    fn resume_provider(exe: &str, home: &Path, sid: &str) -> String {
        use std::io::BufRead as _;
        use std::process::{Command, Stdio};

        let mut child = Command::new(exe)
            .arg("app-server")
            .env("CODEX_HOME", home)
            .env("RUST_LOG", "error")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("起不来 app-server");
        let mut stdin = child.stdin.take().unwrap();
        let mut out = io::BufReader::new(child.stdout.take().unwrap());

        let send = |w: &mut std::process::ChildStdin, s: &str| {
            writeln!(w, "{s}").unwrap();
            w.flush().unwrap();
        };
        send(
            &mut stdin,
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"clientInfo":{"name":"probe","version":"1.0.0"}}}"#,
        );
        // 必须等 initialize 的响应再发别的：app-server 在握手完成前会拒绝其它方法。
        let mut provider = String::new();
        let mut line = String::new();
        let mut sent_resume = false;
        // 上限而不是无限循环：app-server 会持续推通知，读不到目标时要能自己结束。
        for _ in 0..400 {
            line.clear();
            if out.read_line(&mut line).unwrap_or(0) == 0 {
                break;
            }
            let Ok(msg) = serde_json::from_str::<Value>(line.trim()) else { continue };
            let id = msg.get("id").and_then(Value::as_i64);
            if id == Some(1) && !sent_resume {
                send(&mut stdin, "{\"jsonrpc\":\"2.0\",\"method\":\"initialized\",\"params\":{}}");
                send(
                    &mut stdin,
                    &format!(
                        "{{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"thread/resume\",\
                         \"params\":{{\"threadId\":\"{sid}\",\"approvalPolicy\":\"never\"}}}}"
                    ),
                );
                sent_resume = true;
            } else if id == Some(2) {
                if let Some(e) = msg.get("error") {
                    panic!("thread/resume 失败: {e}");
                }
                provider = msg
                    .get("result")
                    .and_then(|r| r.get("modelProvider"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                break;
            }
        }
        let _ = child.kill();
        // kill 之后必须 wait：否则留下僵尸进程（clippy::zombie_processes 会红）。
        // 探针每跑一次起一个 313 MB 的 app-server，攒着不回收会把开发机拖垮。
        let _ = child.wait();
        assert!(!provider.is_empty(), "没读到 thread/resume 的 modelProvider");
        provider
    }
}






