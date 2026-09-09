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
use std::fs;
use std::path::{Path, PathBuf};

/// 回滚清单文件名（落在应用数据目录，受 `SYNAROUTE_DATA_DIR` 隔离）。
const MANIFEST_FILE: &str = "codex-session-providers.json";

#[path = "codex_session_ops.rs"] pub(crate) mod ops;
#[path = "codex_session_fs.rs"] pub(in crate::tools) mod files;
#[path = "codex_session_history.rs"] pub(in crate::tools) mod history; // 顺 history_base 链拼完整历史（fork 只存分叉点之后的轮次）；理由见该文件模块头
#[path = "codex_session_view.rs"] pub(in crate::tools) mod view;
#[path = "codex_session_catalog.rs"] mod catalog; // Desktop 列表索引 local_thread_catalog（第三份 provider 副本）；理由见该文件模块头
#[path = "codex_session_sqlite.rs"] mod sqlite;
#[path = "codex_session_sync.rs"] pub(crate) mod sync;

// 文件层的原语都在 [`files`] 里。这里再导出一次，让 [`ops`]/[`view`]/[`sync`] 与本模块
// 用同一个名字 —— 各处 `files::` 前缀写法不一致时，「到底哪份实现」会变成一个要查的问题。
pub(in crate::tools) use files::{
    collect_rollouts, read_first_line, rel_of, resolve_in_home, split_eol,
};

// 🔴 `SQL_CHUNK` 的重导出**不能**带 `#[cfg(test)]`：`ops` 与 `catalog` 两个兄弟模块的
// **生产**代码按 `super::SQL_CHUNK` 用它。带 cfg 的表现极具迷惑性 —— `cargo test --lib`
// 全绿（测试构型下这行存在），而 `cargo build` / `cargo check` **压根编译不过**，
// 也就是说整套测试都证明不了产物能构建出来。抓住它的只有 clippy/check，不是任何用例。
pub(in crate::tools) use sqlite::SQL_CHUNK;
#[cfg(test)]
use sqlite::{set_threads_provider, sqlite_root_of, DB_BACKUP_KEEP};

/// 一条会话的首行元数据。**只从 rollout 首行读**，不碰正文。
///
/// 展示用的补充字段（标题 / 模型 / 档位 / token）由 [`view`] 从会话库或正文补上 ——
/// 那些是「让用户认出这是哪条对话」用的，与路由正确性无关，故刻意不混进这一层。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionRef {
    /// 相对 `$CODEX_HOME` 的路径。清单里存的也是它 —— 存绝对路径会在换机器或改
    /// `CODEX_HOME` 之后认领到别人的文件上去。
    pub rel_path: String,
    /// 🔴 从**文件名**推导（见 [`files::thread_id_from_filename`]），不是首行的
    /// `payload.id` —— fork 子会话的首行记着**父**会话的 id，拿它去 DELETE 会删错行。
    /// 推导不出时是空串：那时 sqlite 与索引那两步一律跳过（宁可少做，不能做错）。
    pub thread_id: String,
    /// 首行里记的 provider。缺失时是空串（Codex 早期版本没这个字段），
    /// 空串**参与同步**：它同样会让 `thread/resume` 拿不到我们的 provider。
    pub provider: String,
    pub archived: bool,
    pub cwd: String,
    pub timestamp: String,
    pub bytes: u64,
    /// 这条 thread 的来源：`user` = 用户自己的对话，其它值（如 `guardian_review`）是
    /// Codex 内部派生出来的。**不隐藏、只标注** —— 隐藏用户的数据比多显示一行更糟，
    /// 而不标注的话「我明明只开了 3 个对话，这里怎么有 5 条」无从解释。
    pub thread_source: String,
    /// fork 出来的子会话：文件名带两个 UUID，且首行的 `payload.id` 是父会话的 id。
    pub forked: bool,
    /// 展示用标题（由 [`view`] 填，扫描层留空）。
    #[serde(default)]
    pub title: String,
    /// 这条对话用的模型与思考档位（由 [`view`] 从会话库补，取不到就留空）。
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub effort: String,
    #[serde(default)]
    pub tokens: u64,
    /// 🔴 **这条对话用的模型，我们现在已经服务不了了。**
    ///
    /// 只有代理侧能回答这个问题 —— CodexPlusPlus 之类的启动器没有 Key 池，永远给不出它。
    /// 用处很具体：用户点开一条旧对话报错，成因可能不是 provider（那一列是绿的），而是他
    /// 后来删掉了服务 `glm-5.3` 的那条 Key、或改了模型映射。没有这一位的话，那条对话在
    /// 界面上「一切正常」而实际打开必然降级或失败。
    ///
    /// **只在确知时为真**（同 `balance_gate` 的「查不到 ≠ 为零」）：没记模型名、或 Codex
    /// 分类压根没有启用的 Key 时一律 `false` —— 后者会把整张表刷红，而那只说明用户还没配
    /// Codex，不说明这些对话有问题。
    #[serde(default)]
    pub model_unserviceable: bool,
    /// Desktop 项目侧栏里这条对话归属的项目名。**空串 = 未归入任何项目**。
    ///
    /// 只读自 `.codex-global-state.json`（见 [`view`] 里那个函数的文档：那个文件我们绝不写）。
    /// 它回答的是一个用户真会问的问题：「这条对话为什么不在我的项目侧栏里」——
    /// 而那与 provider 正交，看 provider 那一列永远看不出来。
    #[serde(default)]
    pub project: String,
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
    /// 用户正在看的同步目标。**报告必须从这里派生文案**，不能写死 SynaRoute ——
    /// 手动入口允许选 openai / 自定义 provider，写死会确认一件与实际相反的事。
    pub target: String,
    /// 真正改过的会话数。
    pub changed: usize,
    /// 本来就已经是目标 provider、一个字节都没动的会话数。
    pub already_ok: usize,
    /// 改写失败而跳过的会话数。成因归类在 [`SyncReport::blame`] 里 ——
    /// Windows 把「被占用」与「不可写」都映射成 `PermissionDenied`，只有 raw OS error
    /// 32/33 能把它们分开（见 [`files::blame_from_text`]）。
    pub skipped: usize,
    /// 跳过项的成因（取第一条的归类）。它决定 [`describe`] 给的是精确指路还是条件句。
    pub blame: Option<files::IoBlame>,
    pub unreadable: usize,
    /// 路径遏制拒掉的会话数。**刻意与 `unreadable` 分开计数**：一个是路径推导出了问题、
    /// 一个是 Codex 改了 rollout 格式，合成一个数字会让用户拿到指错方向的解释。
    pub path_rejected: usize,
    /// 改动过的会话里有多少条正文带 `encrypted_content`。**只对这些条目成立**
    /// （没改过的文件我们不会去流一遍，见 [`files::RewriteInfo`]）。
    pub encrypted: usize,
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
    /// Desktop 列表索引那一份的账（provider 同步 / 清 missing 标记 / 补缺行）。
    pub catalog: catalog::CatalogReport,
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
    /// 接入前 config.toml 的根 provider。接入期间**新建**的会话没有逐条原值，
    /// 停止时按这一个真实快照交还；旧清单缺字段时从 config.toml 的 .bak 再取。
    #[serde(default)]
    fallback_provider: String,
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

/// 从接入前的 config.toml 快照读根 provider。文件合法但没显式值 = Codex 内置 `openai`；
/// 快照不存在/损坏则返回 None，**不猜**（还原用户会话时猜错比少改一条糟）。
fn fallback_provider_from_backup(home: &Path) -> Option<String> {
    let config = home.join("config.toml");
    let backup = super::backup_path_for(&config);
    if backup.is_file() {
        let text = fs::read_to_string(backup).ok()?;
        let doc = text.parse::<toml::Value>().ok()?;
        return Some(
            doc.get("model_provider")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or("openai")
                .to_string(),
        );
    }
    // 接入前 config.toml 不存在时 `backup_and_write_bytes` 落 created marker；Codex 在
    // 缺 config 的形态下用内置 openai。它是可证实的默认，不是猜一个自定义 id。
    super::super::created_marker_path_for(&config)
        .is_file()
        .then(|| "openai".to_string())
}

/// 该往清单里记哪个原值。
///
/// 🔴 **绝不能把我们自己的 provider id 记成「原值」。** 手动同步允许把目标选成任意
/// 已发现的 provider，于是这个序列完全正常：接入（会话被改成 `synaroute`）→ 期间新建
/// 若干会话（Codex 按 config 写下 `synaroute`）→ 用户在会话页手动同步到 `openai`。
/// 这一步里那些会话的当前值就是 `synaroute`，裸记下去之后「还原」会把它们改回
/// `synaroute` —— 而 `restore_one` 同时把 `config.toml` 从 `.bak` 整份交还，
/// `[model_providers.synaroute]` 那张表**已经不在了**。
///
/// 后果是本仓模块头矩阵里最坏的那一行：`model_provider="synaroute"` 但表缺失 →
/// `Error: Model provider \`synaroute\` not found`，**一个请求都不发**。也就是说
/// 「还原」这个动作亲手把用户的对话变成打不开的。
///
/// 正确的原值是接入前的根 provider（[`Manifest::fallback_provider`]）。取不到时记空串 ——
/// 空串在还原侧是「把字段整个摘掉」（见 [`rewrite_first_line`]），Codex 对缺字段有默认
/// 行为（回落 config 的根 provider，而那时它已经是用户自己的了）。宁可交给那个默认，
/// 也不写一个我们知道会失效的 id。
fn original_to_record(current: &str, fallback: &str) -> String {
    if current == super::MCP_CLIENT_NAME {
        return fallback.to_string();
    }
    current.to_string()
}

/// 旧版清单没有逐条记录「本来就是 target」的会话；只把**清单写成之后新建**的未记录会话
/// 当成 born-as-target。时间取不到就 fail closed，不拿一个同名旧会话冒充新会话。
fn born_after_sync(session: &SessionRef, synced_at: &str) -> bool {
    let Ok(session_ts) = chrono::DateTime::parse_from_rfc3339(&session.timestamp) else {
        return false;
    };
    let Ok(sync_ts) = chrono::DateTime::parse_from_rfc3339(synced_at) else {
        return false;
    };
    session_ts >= sync_ts
}


// 扫描（只读首行）

/// 解析首行，只在它确实是 `session_meta` 时返回。
///
/// 🔴 **判定与改写都只针对首行。** 取证方式：本机 5 个 rollout 的首行逐个 dump 过，
/// `type` 全部是 `session_meta`（含 2 个 fork 子会话与 1 个 guardian 子代理会话）。
/// ⚠️ **依据不是 `ordinal == 0`** —— 那 5 个里一个是 `ordinal: 53`（带 `history_base`
/// 的续接会话）、一个压根没有 `ordinal` 字段。原注释拿它当依据是错的，而它声称核对过，
/// 属本仓最贵的那类过时注释；行为一直是对的（只看第一行、只认 `type`）。
///
/// CodexPlusPlus 遍历全文找 session_meta，那要把几十 MB 整个读进内存；而万一 Codex 日后
/// 挪走它，我们的表现是**首行认不出 → 跳过并计数**，不是静默改错行 —— 失效方向安全。
///
/// 返回的第一项是首行里的 `payload.id`，**只用于判断这是不是 fork 子会话**（子会话那里它
/// 是父的 id）。真正的 thread id 由 [`files::thread_id_from_filename`] 从文件名推导。
fn parse_meta(line: &str) -> Option<(String, String, String, String, String)> {
    let (body, _) = split_eol(line);
    let rec: Value = serde_json::from_str(body.trim()).ok()?;
    if rec.get("type").and_then(Value::as_str) != Some("session_meta") {
        return None;
    }
    let p = rec.get("payload")?;
    let s = |k: &str| p.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
    let id = if p.get("id").is_some() { s("id") } else { s("session_id") };
    Some((id, s("model_provider"), s("cwd"), s("timestamp"), s("thread_source")))
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
        let Some((meta_id, provider, cwd, timestamp, thread_source)) = parse_meta(&line) else {
            report.unreadable += 1;
            continue;
        };
        let Some(rel_path) = rel_of(home, &path) else {
            report.path_rejected += 1;
            continue;
        };
        // 🔴 id 取文件名末尾那个 UUID。取不到 → 空串（sqlite/索引那两步跳过），
        // 而不是回落到 `meta_id` —— 回落正是 fork 子会话会删错父会话行的那条路。
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let thread_id = files::thread_id_from_filename(&name).unwrap_or_default();
        report.sessions.push(SessionRef {
            // 首行的 id 与文件名末尾的 id 不同 = 这是从那条会话 fork 出来的分支。
            forked: !thread_id.is_empty() && !meta_id.is_empty() && thread_id != meta_id,
            rel_path,
            thread_id,
            provider,
            archived,
            cwd,
            timestamp,
            bytes,
            thread_source,
            title: String::new(),
            model: String::new(),
            effort: String::new(),
            tokens: 0,
            model_unserviceable: false,
            project: String::new(),
        });
    }
    report
}


// 改写单个 rollout（流式，内存 O(1)）

/// 把首行的 `model_provider` 改成 `want`（`None` = 把字段整个摘掉）。
///
/// 返回 `Ok(Some(原值, 副产物))` 表示确实改了，`Ok(None)` 表示无需改动（**一个字节都没写**）
/// —— 后者必须真的不写：照写一遍相同内容会让「已经对了」这个绝大多数情况变成每次接入都
/// 全量重写几百个文件。
///
/// ⚠️ **判据不能只看 mtime**：[`files::replace_first_line`] 现在会把 mtime 原样恢复，
/// 于是「写了」与「没写」在 mtime 上长得一样。用例改为断言返回值与**首行字节**
/// （`serde_json` 默认按字母排序键，真实改写必然改变字节形态）。
fn rewrite_first_line(
    path: &Path,
    want: Option<&str>,
) -> AppResult<Option<(String, files::RewriteInfo)>> {
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
    let info = files::replace_first_line(path, &first, &next_first)?;
    Ok(Some((original, info)))
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
    let tmp = files::tmp_path_for(&path);
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
        target: target.to_string(),
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
    // 清单第一次落盘时锁定「接入前根 provider」。接入期间新建的会话本来就写 target，
    // 不会进逐条 entries；停止时只有这份快照能如实交还。旧清单留空，restore 再尝试读 .bak。
    let manifest_was_empty = manifest.target.is_empty() && manifest.entries.is_empty();
    let mut manifest_changed = false;
    if manifest.fallback_provider.is_empty() {
        if let Some(provider) = fallback_provider_from_backup(home) {
            manifest.fallback_provider = provider;
            manifest_changed = true;
        }
    }
    let mut newly_recorded = false;
    for (_, s) in &todo {
        if !manifest.entries.iter().any(|e| e.rel_path == s.rel_path) {
            manifest.entries.push(ManifestEntry {
                rel_path: s.rel_path.clone(),
                original_provider: original_to_record(&s.provider, &manifest.fallback_provider),
                thread_id: s.thread_id.clone(),
            });
            newly_recorded = true;
        }
    }
    if newly_recorded || manifest_changed || manifest_was_empty {
        // 初次同步即使一条旧会话都没有，也必须落一份清单：此后在接入期间新建的会话
        // 一出生就是 target、永远不会进 todo；没有这个空清单，停止时完全认不出它们。
        if manifest_was_empty || manifest.target != target {
            manifest.target = target.to_string();
            manifest.synced_at = chrono::Utc::now().to_rfc3339();
        }
        write_manifest(data_dir, &manifest)?;
    }

    // ③ 再改文件，并记下**确实改成功的** thread id。
    let mut done_ids: Vec<String> = Vec::new();
    for (path, s) in &todo {
        match rewrite_first_line(path, Some(target)) {
            Ok(Some((_, info))) => {
                report.changed += 1;
                if info.had_encrypted_content {
                    report.encrypted += 1;
                }
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
            Err(e) => {
                report.skipped += 1;
                // 只留第一条的归类：够给方向了，而逐条列举会让提示变成一篇日志。
                report.blame.get_or_insert(files::blame_from_text(&e.to_string()));
            }
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
    let mut manifest = match read_manifest_state(data_dir) {
        ManifestState::Loaded(m) => m,
        ManifestState::Missing => return Ok(None),
        // 与同步侧同一条纪律：坏清单不是「没有清单」。这里报错而不是静默什么都不改 ——
        // 后者会让用户以为已经还原干净了，而他的旧对话仍指着一个即将停掉的端口。
        ManifestState::Corrupt(p) => return Err(corrupt_err(&p)),
    };
    // 兼容旧清单：上线 fallback_provider 之前的文件从 config.toml 首写即锁的 .bak 恢复。
    if manifest.fallback_provider.is_empty() {
        manifest.fallback_provider = fallback_provider_from_backup(home).unwrap_or_default();
    }
    let known: std::collections::HashSet<String> =
        manifest.entries.iter().map(|e| e.rel_path.clone()).collect();
    // 接入期间新建的会话一出生就指向 target，sync_to_at 因 already_ok 早退、从未把它写进清单。
    // 仅补「清单落盘之后出生 + 当前仍等于 target + 不在清单」这一窄形态；旧的手配会话不猜。
    let born: Vec<SessionRef> = if manifest.fallback_provider.is_empty() {
        Vec::new()
    } else {
        scan_at(home)
            .sessions
            .into_iter()
            .filter(|s| {
                !known.contains(s.rel_path.as_str())
                    && s.provider == manifest.target
                    && born_after_sync(s, &manifest.synced_at)
            })
            .collect()
    };
    // born-as-target 也先写进清单，再改文件：若中途失败或 sqlite 被锁，下一次停止还能精确重试。
    // 这与同步侧「清单必须在改文件之前落盘」是同一条纪律，不能只存在内存里。
    if !born.is_empty() {
        manifest.entries.extend(born.into_iter().map(|s| ManifestEntry {
            rel_path: s.rel_path,
            original_provider: manifest.fallback_provider.clone(),
            thread_id: s.thread_id,
        }));
        write_manifest(data_dir, &manifest)?;
    }
    let restore_entries = manifest.entries.clone();
    let mut restored = 0usize;
    let mut failed = 0usize;
    let mut gone = 0usize;

    for e in &restore_entries {
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
    let db_note = restore_sqlite(home, &restore_entries);

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


// SQLite 实现抽到子模块；对兄弟模块重导出唯一的路径发现函数。
use sqlite::{restore_sqlite, sync_sqlite};
pub(in crate::tools) use sqlite::session_db_paths;

// 对外入口（解析真实 `$CODEX_HOME` 与数据目录）

/// 接入时调用：同步历史会话，返回一句给用户看的说明（`None` = 没什么可说的）。
pub(in crate::tools) fn sync_to(target: &str) -> AppResult<Option<String>> {
    let home = super::codex_paths::codex_home()?;
    let data_dir = crate::store::data_dir::app_data_dir()?;
    Ok(describe(&sync::locked_sync(&home, &data_dir, target)?))
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
    // 用户在会话页关掉了「接入时自动同步」→ 一个字节都不动。这是我们唯一会自动改用户
    // 对话文件的动作，给它一个关得掉的开关是应该的（同时用 cc-switch 的人有正当理由不让
    // 我们碰）。**刻意不落提示**：关掉它的人不需要每次接入都被告知一次。
    if !sync::auto_sync_enabled() {
        return applied;
    }
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
    sync::locked_restore(&home, &data_dir)
}

/// 把报告写成一句话。
///
/// 🔴 **只说「已指向」，不说「已恢复可用」** —— 见模块头那条已知边界：跨账号的
/// `encrypted_content` 可能仍然解不开。承诺一件我们保证不了的事，代价是用户按它排除掉
/// 真正的方向。
fn describe(r: &SyncReport) -> Option<String> {
    let mut parts = Vec::new();
    if r.changed > 0 {
        let target = if r.target.is_empty() { "目标 provider" } else { &r.target };
        parts.push(format!(
            "已把 {} 个历史对话指向 {target}（重启 Codex 后生效；若某条旧对话仍报错，\
             那是它的推理内容由原账号加密所致，新建对话不受影响）",
            r.changed
        ));
    }
    if r.encrypted > 0 {
        // 只说「可能」：我们能确定的只有「这些文件里有 encrypted_content」，续聊到底会不会
        // 失败取决于上游账号，而那是我们看不到的。给出条数是为了让用户能对上号 ——
        // 上面那句条件句在没有数字时无从核实，他只会反复怀疑代理。
        parts.push(format!(
            "其中 {} 条带有原账号加密的推理内容，续聊或压缩时可能报解密失败（新建对话不受影响）",
            r.encrypted
        ));
    }
    if r.skipped > 0 {
        // 归类得出来就给精确指路，认不出才退回条件句 —— Windows 把两种成因都映射成
        // `PermissionDenied`，只有 raw OS error 32/33 能分开（见 `files::blame_from_text`）。
        let why = match r.blame {
            Some(files::IoBlame::Locked) => {
                "（文件正被 Codex 占用）—— 完全退出 Codex 后再点一次接入即可"
            }
            Some(files::IoBlame::Unwritable) => {
                "（其所在目录或文件不可写）—— 退出 Codex 没用，请检查 CODEX_HOME 的权限与是否只读卷"
            }
            _ => "（文件被占用，或其所在目录不可写）—— 若 Codex 正在运行，请完全退出后再点一次接入",
        };
        parts.push(format!("{} 个对话未能同步{why}", r.skipped));
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
        // Desktop 列表索引那一份的账。**只报做了什么，不承诺「会话会回到列表」** ——
        // `has_user_event` 门控可见性这件事我们没有取证（见 `catalog` 模块头）。
        if db.catalog.inserted > 0 || db.catalog.unmarked_missing > 0 {
            parts.push(format!(
                "另修正了 Desktop 会话列表索引（补 {} 条记录、清 {} 个失效标记）",
                db.catalog.inserted, db.catalog.unmarked_missing
            ));
        }
    }
    (!parts.is_empty()).then(|| parts.join("；"))
}

#[cfg(test)]
mod tests {
    use super::*;
    // 测试段自己要的 IO trait：生产段搬走 `replace_first_line` 之后不再用它们（见 `files`）。
    use std::io::{BufReader, Write as _};

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

    /// 把短标签扩成 UUID 形态。
    ///
    /// 🔴 **夹具的文件名必须以合法 UUID 结尾**：`thread_id` 现在从文件名末 36 个字符推导
    /// （见 [`files`] 模块头 —— fork 子会话的首行 `payload.id` 是**父**的 id）。用 `t1`
    /// 这种短标签的话推导不出 id，于是 sqlite 与索引那两步会被静默跳过，
    /// 而那两步恰恰是好几条用例要测的东西。已经是 36 字符的原样返回（真二进制那条用例
    /// 要拿同一个 id 去调 app-server）。
    fn uuid_of(label: &str) -> String {
        if label.len() == 36 {
            return label.to_string();
        }
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in label.as_bytes() {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x100_0000_01b3);
        }
        format!("01a05d14-4e5b-7773-b425-{:012x}", h & 0xffff_ffff_ffff)
    }

    /// 造一条 rollout。`eol` 让行尾成为可测维度（Codex 在 Windows 上写的是 `\n`，
    /// 但用户的文件经过别的工具处理后可能变成 `\r\n`，而我们不能替他归一）。
    fn write_rollout(home: &Path, sub: &str, id: &str, provider: &str, eol: &str) -> PathBuf {
        let id = uuid_of(id);
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
        assert_eq!(original.map(|(p, _)| p).as_deref(), Some("openai"), "必须如实返回原值");

        let after = fs::read(&path).unwrap();
        let first_end = after.iter().position(|b| *b == b'\n').unwrap();
        let first = String::from_utf8(after[..=first_end].to_vec()).unwrap();
        assert!(first.contains("\"model_provider\":\"synaroute\""));
        assert!(first.ends_with("\r\n"), "行尾必须保留 CRLF，实际: {first:?}");
        assert_eq!(&after[first_end + 1..], tail_before, "首行之后必须逐字节不变");
        let _ = fs::remove_dir_all(&home);
    }

    /// ② 已经指向我们的会话**一个字节都不许写**。
    ///
    /// ⚠️ **判据不能只看 mtime 了**：[`files::replace_first_line`] 现在会把 mtime 原样恢复
    /// （否则一次接入会让用户几百个历史会话文件全部显示为刚修改），于是「写了」与「没写」
    /// 在 mtime 上长得一模一样 —— 原先那条断言从此对本用例要防的缺陷完全无效。
    ///
    /// 替代判据两条，缺一不可：
    /// ① 返回值必须是 `None`（早退没了就会变成 `Some`）；
    /// ② **文件字节逐字不变** —— `serde_json` 默认用 `BTreeMap`，重新序列化会把键**按字母
    ///    排序**，所以哪怕语义相同，一次真实改写也必然改变首行的字节形态。这条比 mtime
    ///    更难被绕过（连「先备份 mtime 再照写一遍」都躲不过它）。
    /// mtime 那条断言保留，但它现在守的是**保全**、不再是「有没有写」。
    #[test]
    fn a_session_that_already_points_at_us_is_not_touched_at_all() {
        let home = tmp_home("noop");
        let path = write_rollout(&home, "sessions/2026/09/01", "t1", "synaroute", "\n");
        let before = fs::read(&path).unwrap();
        let mtime_before = fs::metadata(&path).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));

        assert!(
            rewrite_first_line(&path, Some("synaroute")).unwrap().is_none(),
            "无需改动时必须早退"
        );
        assert_eq!(fs::read(&path).unwrap(), before, "无需改动时必须一个字节都不写");
        assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), mtime_before);
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

    /// 🔴 接入期间**新建**的会话一出生就写 target，因 `already_ok` 从来进不了逐条清单；
    /// 停止时必须按接入前 config 快照交还。反面一起钉：接入前就手配成 target 的旧会话
    /// 不能被误认成 born-as-target（时间戳判据存在的全部理由）。
    #[test]
    fn sessions_born_while_applied_restore_to_the_pre_apply_provider() {
        let home = tmp_home("born_target");
        let data = home.join("appdata");
        let config = home.join("config.toml");
        fs::write(super::super::backup_path_for(&config), "model_provider = \"openai\"\n").unwrap();

        let old = write_rollout(&home, "sessions/2026/09/01", "old", "openai", "\n");
        let manual = write_rollout(&home, "sessions/2026/09/01", "manual", "synaroute", "\n");
        assert_eq!(sync_to_at(&home, &data, "synaroute").unwrap().changed, 1);

        let born = write_rollout(&home, "sessions/2026/09/02", "born", "synaroute", "\n");
        let text = fs::read_to_string(&born).unwrap();
        fs::write(&born, text.replace("2026-09-01T13:06:47.003Z", "2999-09-02T00:00:00Z")).unwrap();

        let note = restore_at(&home, &data).unwrap().expect("旧会话与接入期间新会话都该还原");
        assert!(note.contains('2'), "应还原 old + born 两条：{note}");
        assert_eq!(provider_of(&old), "openai");
        assert_eq!(provider_of(&born), "openai", "接入期间新建的会话不能留在已删除的 provider 上");
        assert_eq!(provider_of(&manual), "synaroute", "接入前就指向 target 的手配旧会话不许猜着改");
        let _ = fs::remove_dir_all(&home);
    }

    /// 🔴 **手动同步到别的目标之后，还原不许把会话留在我们自己那个已被删掉的 provider 上。**
    ///
    /// 会话页那个目标下拉允许选**任意**已发现的 provider，于是这个序列完全正常：
    /// 接入（会话改成 `synaroute`）→ 期间新建会话（Codex 按 config 写 `synaroute`）→
    /// 用户手动同步到 `openai`（「我想让旧对话回官方」）。第三步里那条新会话的当前值
    /// 就是 `synaroute`，裸记进清单之后「还原」会把它改回 `synaroute` ——
    /// 而 `restore_one` 同时把 `config.toml` 从 `.bak` 整份交还，那张表已经不在了。
    ///
    /// 后果是 `codex.rs` 模块头矩阵里最坏的那一行：`Error: Model provider \`synaroute\`
    /// not found`，**一个请求都不发**。也就是「还原」亲手把用户的对话变成打不开的。
    ///
    /// 判据同时钉反面：真正来自别处的原值（这里的 `my-relay`）必须原样保住 ——
    /// 一律替换成 fallback 会把用户自己配的 provider 也冲掉。
    #[test]
    fn restoring_after_a_manual_sync_elsewhere_never_leaves_our_own_provider_behind() {
        let home = tmp_home("manual_elsewhere");
        let data = home.join("appdata");
        let config = home.join("config.toml");
        fs::write(super::super::backup_path_for(&config), "model_provider = \"openai\"\n").unwrap();

        // ① 接入：把一条历史会话改成我们。
        let old = write_rollout(&home, "sessions/2026/09/01", "old", "openai", "\n");
        assert_eq!(sync_to_at(&home, &data, "synaroute").unwrap().changed, 1);

        // ② 接入期间新建一条（Codex 按 config 写下我们的 id），另有一条真来自别处。
        let born = write_rollout(&home, "sessions/2026/09/02", "born", "synaroute", "\n");
        let relay = write_rollout(&home, "sessions/2026/09/02", "relay", "my-relay", "\n");

        // ③ 用户在会话页手动同步到 openai —— 此时 born 的当前值正是 `synaroute`。
        sync_to_at(&home, &data, "openai").unwrap();
        let m = read_manifest(&data).expect("清单必须在");
        let of = |tag: &str| {
            let want = uuid_of(tag);
            m.entries
                .iter()
                .find(|e| e.rel_path.contains(&want))
                .map(|e| e.original_provider.clone())
                .unwrap_or_else(|| panic!("清单里没有 {tag}：{:?}", m.entries))
        };
        assert_ne!(
            of("born"),
            "synaroute",
            "🔴 绝不能把我们自己的 id 记成原值 —— 还原后那张表已经不在，Codex 一个请求都不发"
        );
        assert_eq!(of("born"), "openai", "正确的原值是接入前的根 provider");
        assert_eq!(of("relay"), "my-relay", "反面：真来自别处的原值必须原样保住");

        // ④ 停止 → 还原。三条都不许留在 `synaroute` 上。
        restore_at(&home, &data).unwrap();
        for (tag, path) in [("old", &old), ("born", &born), ("relay", &relay)] {
            assert_ne!(
                provider_of(path),
                "synaroute",
                "{tag} 还原后仍指向我们（config 里那张表已被交还掉）"
            );
        }
        assert_eq!(provider_of(&relay), "my-relay");
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
        let rec = m.entries.iter().find(|e| e.rel_path.contains(&uuid_of("t1"))).unwrap();
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
        c.execute("INSERT INTO threads VALUES (?1, ?2)", [&uuid_of("t1"), &provider.to_string()])
            .unwrap();
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
        provider_of_id(path, &uuid_of("t1"))
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
        // 判据跟着代码搬到 `codex_session_sqlite.rs`（理由同 catalog 那条）。
        let src = crate::proxy::custom_headers::production_code_only(include_str!(
            "codex_session_sqlite.rs"
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
            provider_of_id(main, &uuid_of("t1")),
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
                fallback_provider: String::new(),
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
            Some(SqliteOutcome { updated: 2, error: None, ..Default::default() }),
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

    /// 🔴 `CODEX_SQLITE_HOME` 必须被认。
    ///
    /// Codex 自己认这个变量（二进制里那句 "`CODEX_SQLITE_HOME` is overridden by an exact
    /// requirement for sqlite_home"，出自 `core/src/config/requirements.rs`）。不认它的表现
    /// 是**静默的**：我们对着 `$CODEX_HOME` 下那个陈旧的库写，而 Codex 读的是别处那个 ——
    /// 于是「列表里的 provider 怎么改都不变」，而 rollout 那半明明成功了。
    ///
    /// 空串与「指向不存在的目录」都必须回落 `home`：CI 里 `env: { X: "" }` 是常见写法，
    /// 而一个写错的路径若被采纳，我们连 legacy 库都会找不到（同 `SYNAROUTE_DATA_DIR`
    /// 那条「空串必须视为未设置」的教训）。
    #[test]
    fn the_sqlite_home_override_is_honored_but_only_when_it_exists() {
        let home = tmp_home("sqhome");
        let other = home.join("elsewhere");
        fs::create_dir_all(&other).unwrap();

        assert_eq!(sqlite_root_of(None, &home), home, "没设就用 CODEX_HOME");
        assert_eq!(sqlite_root_of(Some("".into()), &home), home, "空串必须视为未设置");
        assert_eq!(
            sqlite_root_of(Some(home.join("nope").into_os_string()), &home),
            home,
            "指向不存在的目录时必须回落，否则连 legacy 库都找不到"
        );
        assert_eq!(sqlite_root_of(Some(other.clone().into_os_string()), &home), other);

        // 接线：`session_db_paths` 必须经 `sqlite_root`，否则上面全绿而功能没生效。
        // 判据跟着代码搬到 `codex_session_sqlite.rs`。
        let src = crate::proxy::custom_headers::production_code_only(include_str!(
            "codex_session_sqlite.rs"
        ));
        assert!(src.contains("let root = sqlite_root(home);"), "库路径必须从 sqlite_root 起算");
        let _ = fs::remove_dir_all(&home);
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
        //
        // 🔴 三支各测一次。原先只有一句「覆盖两种成因的条件句」，那是因为 Windows 把
        // 「被占用」与「不可写」都映射成 `PermissionDenied`、分不开。现在
        // [`files::blame_from_text`] 能按 raw OS error 32/33 分开它们，于是：
        // 归类得出来 → 给**精确**指路；认不出 → 才退回条件句。
        // 精确那两支必须**互不相同**，否则「归类」这件事对用户毫无价值。
        let locked = describe(&SyncReport {
            skipped: 2,
            blame: Some(files::IoBlame::Locked),
            ..Default::default()
        })
        .unwrap();
        assert!(locked.contains("退出 Codex"), "被占用要说退出 Codex: {locked}");
        assert!(!locked.contains("只读卷"), "已经确定是占用，别再提无关的权限方向");

        let unwritable = describe(&SyncReport {
            skipped: 2,
            blame: Some(files::IoBlame::Unwritable),
            ..Default::default()
        })
        .unwrap();
        assert!(unwritable.contains("权限"), "不可写要指向权限: {unwritable}");
        assert!(
            unwritable.contains("退出 Codex 没用"),
            "🔴 必须明说退出 Codex 无效 —— 只读卷上那句是无效指路，用户照做后一字不变"
        );

        // 认不出成因时退回条件句，两种情形都要提到。
        let unknown = describe(&SyncReport { skipped: 2, ..Default::default() }).unwrap();
        assert!(unknown.contains("退出") && unknown.contains("不可写"), "{unknown}");

        // encrypted_content 的条数要单独说，且**只说「可能」** —— 会不会真的失败取决于
        // 上游账号，那是我们看不到的。
        let enc = describe(&SyncReport { changed: 2, encrypted: 2, ..Default::default() }).unwrap();
        assert!(enc.contains("可能"), "不许把「可能解不开」说成一定失败: {enc}");

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
                ..Default::default()
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
        let mut out = BufReader::new(child.stdout.take().unwrap());

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






