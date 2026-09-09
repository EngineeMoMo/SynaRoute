//! codegraph CLI 适配层 —— 可执行定位、三态检测、符号级查询。
//!
//! 为什么单独成模块：codegraph 是**可选**外部工具，装了就能拿到 AST 级的符号边界与跨文件调用边
//! （比 grep 精确一个量级），没装则整体降级到 grep/遍历。把它的所有假设收在这一处，
//! 便于上游改版时只改这里。
//!
//! ## 实测事实（codegraph v1.5.0，2026-07-30 实机验证，勿凭文档改）
//!
//! 1. **退出码恒为 0**：`version`、未初始化的 `status`、未初始化的 `query` 全部 exit=0，
//!    失败信息只在 stdout 里（如 `✗ CodeGraph not initialized`）。故**判据必须是「stdout 能否
//!    解析成期望的 JSON」**，不能看 `status.success()`——旧实现正是栽在这，然后静默返回空。
//! 2. **没有 `search` 子命令**：等价命令是 `query <search>`。旧实现调 `search --json` 从未生效过。
//! 3. `-p, --path <path>` 可显式指定项目，比依赖进程 `current_dir` 可靠。
//! 4. JSON 形状（README 未文档化，由实机 dump 确认）：
//!    - `query --json` → `[{ node: {...}, score: f64 }]`
//!    - `callers|callees --json` → `{ symbol, callers|callees: [{name, kind, filePath, startLine}] }`
//!    - `impact --json --depth N` → `{ symbol, depth, nodeCount, edgeCount, affected: [同上] }`
//!    - `node` 对象含 `kind/name/qualifiedName/filePath/language/startLine/endLine/signature/
//!      returnType/visibility/docstring`——**`startLine`/`endLine` 是符号级切片的关键**，
//!      有它就不必自己做花括号配对来找方法体边界。

use crate::proc::hidden;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::process::Command;

/// 单次 codegraph 调用的墙钟上限。索引 47 文件仅 961ms，查询是毫秒级；
/// 给到 20s 足够覆盖大仓库冷启动，又不会在工具卡死时拖垮整轮聚合。
const CLI_TIMEOUT: Duration = Duration::from_secs(20);

/// `codegraph init` 的墙钟上限。实测 47 文件 961ms，但大仓库（十万行级）可能到分钟级，
/// 故给 10 分钟；超时只中止本次建索引，不影响其他功能。
const INIT_TIMEOUT: Duration = Duration::from_secs(600);

/// codegraph 在项目根建的索引目录名。**「已索引」的唯一判据就是它在不在。**
/// 抽成常量是为了让 `init_project` 的成功判据与 `detect` 的状态判据共用一份 ——
/// 各写一个字面量的话，上游哪天改了目录名，两处只会有一处被修。
const INDEX_DIR: &str = ".codegraph";

/// codegraph 可用性状态 —— 每态对应**不同的用户动作**，故这些区分不能合并。
///
/// 🔴 **`IndexUnknown` 与 `NotIndexed` 必须分开，合并过一次、是用户报的缺陷本体**：
/// 旧实现里 `detect(None)` 返回 `NotIndexed`，也就是把「**我没查**」谎报成「我查了，没有」。
/// 三个后果同时发生且互相印证，用户完全无从判断哪个是真的：徽标写「未索引」（而项目
/// 可能早就索引过了）、说明写「已安装但当前项目未建索引」（假话）、「建索引」按钮被禁用
/// 并配一句「请先设置工作目录」（而目录就显示在同一屏的上方）。
/// 判据：**只有在真的判不出目录时才允许说「请先设置工作目录」** —— 否则那句话把用户
/// 送去做一个他已经做过的操作，即本仓「指错方向的提示比没有提示更糟」那条。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "state")]
pub enum CodegraphState {
    /// 未安装（PATH 与所有已知安装位置都找不到可执行）。
    NotInstalled,
    /// 找到了可执行，但它位于**非当前 node 版本**的 npm 全局目录里（nvm 升级后的孤岛），
    /// PATH 调不到。此时 SynaRoute 仍可用绝对路径调用，但要提示用户重装以免终端里用不了。
    Stranded { path: String, version: String },
    /// 可执行就绪，但**没有可判定的项目目录**（既没勾自动跟随、也没填工作目录，
    /// 或勾了自动跟随而会话历史里一个项目都扫不到）。索引状态**未知**，不是「没有」。
    IndexUnknown { path: String, version: String },
    /// 可执行就绪，但目标项目没有 `.codegraph/` 索引 —— 需要跑一次 `init`。
    ///
    /// `dir` 是**实际判定的**项目目录：UI 必须显示它。不显示的代价很具体 ——
    /// 自动跟随下目录由会话历史决定，用户看到「未索引」时第一个问题就是「哪个项目」。
    NotIndexed { path: String, version: String, dir: String },
    /// 就绪：可执行 + 项目已索引。`dir` 同上。
    Ready {
        path: String,
        version: String,
        dir: String,
        nodes: Option<u64>,
        edges: Option<u64>,
    },
}

/// 已解析的 codegraph 可执行位置（含它是否来自 PATH）。
#[derive(Debug, Clone)]
pub struct Resolved {
    /// 调用时用的程序名或绝对路径。
    pub program: String,
    pub version: String,
    /// 是否从 PATH 直接解析到（false = 从已知安装位置捞到的孤岛）。
    pub on_path: bool,
}

/// `query --json` 的单条结果。
#[derive(Debug, Clone, Deserialize)]
pub struct QueryHit {
    pub node: SymbolNode,
    /// codegraph 自身的相关度分数。当前排序直接沿用它返回的顺序，故不读取；
    /// 保留字段是为了将来做跨关键词的合并排序时不必改结构。
    #[serde(default)]
    #[allow(dead_code)]
    pub score: f64,
}

/// codegraph 的符号节点。只声明我们真正用到的字段，其余忽略（上游加字段不会破坏解析）。
///
/// 几个字段当前未被读取但刻意保留：它们是 codegraph 真实返回的内容，
/// 后续做「契约视图」（按 visibility 过滤私有符号、用 docstring 替代方法体省 token）时直接可用，
/// 删了将来还得重新对一遍字段名。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct SymbolNode {
    pub kind: String,
    pub name: String,
    #[serde(default)]
    pub qualified_name: Option<String>,
    pub file_path: String,
    #[serde(default)]
    pub language: Option<String>,
    /// 1-based 起始行（含）。符号级切片的依据。
    pub start_line: u32,
    /// 1-based 结束行（含）。
    #[serde(default)]
    pub end_line: Option<u32>,
    #[serde(default)]
    pub signature: Option<String>,
    #[serde(default)]
    pub return_type: Option<String>,
    #[serde(default)]
    pub visibility: Option<String>,
    #[serde(default)]
    pub docstring: Option<String>,
}

/// `callers|callees|impact` 里的邻接节点（字段比 query 的 node 少，无 endLine）。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelatedNode {
    pub name: String,
    #[serde(default)]
    pub kind: Option<String>,
    pub file_path: String,
    #[serde(default)]
    pub start_line: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ImpactOut {
    #[serde(default)]
    affected: Vec<RelatedNode>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CallersOut {
    #[serde(default)]
    callers: Vec<RelatedNode>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CalleesOut {
    #[serde(default)]
    callees: Vec<RelatedNode>,
}

/// 候选可执行名（Windows 上 npm 装出的是 .cmd 壳脚本，`Command::new` 需要显式带扩展名才能找到）。
#[cfg(windows)]
const EXE_NAMES: &[&str] = &["codegraph.cmd", "codegraph.exe", "codegraph"];
#[cfg(not(windows))]
const EXE_NAMES: &[&str] = &["codegraph"];

/// 定位 codegraph 可执行。
///
/// 顺序：PATH → npm 全局 prefix → nvm 各版本的 node_global → 常见 npm 全局目录。
///
/// 为什么不能只试 PATH（2026-07-30 实机踩到）：用户用 nvm 从 node 20 升到 22 后，
/// npm 全局包留在 `F:\nvm\v20.20.0\node_global`（不在任何 PATH 里），而 PATH 指向
/// `F:\nodejs\node_global`——`codegraph` 变成谁都调不到的孤岛。这类环境漂移每次升 node
/// 都会复发，故必须能从已知位置捞回来，并把**绝对路径**记下来用，不再依赖 PATH。
pub async fn resolve() -> Option<Resolved> {
    // 1. PATH
    for name in EXE_NAMES {
        if let Some(v) = probe_version(name).await {
            return Some(Resolved { program: name.to_string(), version: v, on_path: true });
        }
    }
    // 2. 已知安装位置
    for dir in candidate_dirs().await {
        for name in EXE_NAMES {
            let p = dir.join(name);
            if !p.is_file() {
                continue;
            }
            let prog = p.to_string_lossy().to_string();
            if let Some(v) = probe_version(&prog).await {
                return Some(Resolved { program: prog, version: v, on_path: false });
            }
        }
    }
    None
}

/// 枚举可能装着 codegraph 的目录（npm 全局 bin 目录的常见形态）。
async fn candidate_dirs() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();

    // npm config get prefix（最权威，但 npm 自身可能不在 PATH，故失败不致命）
    if let Ok(o) = timeout_output(hidden(npm_program()).args(["config", "get", "prefix"])).await
    {
        let p = String::from_utf8_lossy(&o.stdout).trim().to_string();
        if !p.is_empty() && p != "undefined" {
            let base = PathBuf::from(&p);
            out.push(base.clone());
            out.push(base.join("bin")); // POSIX 布局
        }
    }

    // nvm 多版本目录：<nvm_root>/v*/node_global 与 <nvm_root>/v*/（Windows nvm4w 布局）
    for env_key in ["NVM_HOME", "NVM_SYMLINK", "NVM_DIR"] {
        if let Ok(root) = std::env::var(env_key) {
            let root = PathBuf::from(root);
            push_nvm_versions(&root, &mut out);
            if let Some(parent) = root.parent() {
                push_nvm_versions(parent, &mut out);
            }
        }
    }

    // 常见用户级 npm 全局目录
    if let Some(home) = dirs::home_dir() {
        out.push(home.join(".npm-global").join("bin"));
        out.push(home.join(".npm-global"));
    }
    if let Some(appdata) = dirs::config_dir() {
        out.push(appdata.join("npm")); // %APPDATA%\npm
    }

    out.dedup();
    out
}

/// 把 `<root>/v*/node_global` 与 `<root>/v*` 加入候选（nvm-windows 的 npm 全局布局）。
fn push_nvm_versions(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if !p.is_dir() {
            continue;
        }
        let name = e.file_name().to_string_lossy().to_string();
        if !name.starts_with('v') {
            continue;
        }
        out.push(p.join("node_global"));
        out.push(p);
    }
}

#[cfg(windows)]
fn npm_program() -> &'static str {
    "npm.cmd"
}
#[cfg(not(windows))]
fn npm_program() -> &'static str {
    "npm"
}

/// 跑 `<prog> version` 拿版本号。
///
/// 判据是**stdout 里能否找到形如 `1.5.0` 的版本串**，而非退出码——codegraph 的退出码恒为 0
/// （实测），且当程序压根不存在时 `Command` 会在启动阶段就 Err，两种失败都能区分开。
async fn probe_version(program: &str) -> Option<String> {
    let out = timeout_output(hidden(program).arg("version")).await.ok()?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    parse_version(&text)
}

/// 从输出里抓第一个 `x.y[.z]` 版本串。不用正则依赖，手写扫描。
fn parse_version(text: &str) -> Option<String> {
    for token in text.split(|c: char| c.is_whitespace() || c == 'v' || c == 'V') {
        let t = token.trim().trim_start_matches('v');
        let mut parts = t.split('.');
        let a = parts.next()?;
        let b = parts.next().unwrap_or("");
        if a.is_empty() || b.is_empty() {
            continue;
        }
        if a.chars().all(|c| c.is_ascii_digit()) && b.chars().all(|c| c.is_ascii_digit()) {
            return Some(t.trim_end_matches(|c: char| !c.is_ascii_digit()).to_string());
        }
    }
    None
}

/// 为项目建立索引（`codegraph init <path>`）。
///
/// 该命令会在项目根创建 `.codegraph/`（SQLite + FTS5，纯本地不出网）。实测 47 文件 / 1254 节点 /
/// 4870 边耗时 961ms；大仓库可能到分钟级，故给足 `INIT_TIMEOUT`。
///
/// 与查询同理：**退出码不可信**，用「`.codegraph/` 目录是否出现」作为成功判据，
/// 并把 stdout 摘要一并返回供日志展示。
pub async fn init_project(work_dir: &Path) -> Result<String, String> {
    let r = resolve().await.ok_or_else(|| {
        "未找到 codegraph 可执行文件；请先安装（npm i -g @colbymchenry/codegraph）".to_string()
    })?;
    let dir = work_dir.to_string_lossy().to_string();
    let out = match tokio::time::timeout(
        INIT_TIMEOUT,
        hidden(&r.program).args(["init", &dir]).output(),
    )
    .await
    {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return Err(format!("启动 codegraph init 失败: {e}")),
        Err(_) => return Err("codegraph init 超时（超过 10 分钟）".into()),
    };
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    if !work_dir.join(INDEX_DIR).is_dir() {
        let head: String = text.trim().chars().take(300).collect();
        return Err(format!("init 未生成 {INDEX_DIR}/ 目录；输出: {head}"));
    }
    // 从输出里抓 "N nodes, M edges" 摘要（有则带上，无则给通用成功文案）
    let summary = text
        .lines()
        .find(|l| l.contains("nodes") && l.contains("edges"))
        .map(|l| l.trim().trim_start_matches(['●', '│', ' ']).to_string())
        .unwrap_or_else(|| "索引已建立".to_string());
    Ok(summary)
}

/// 带超时地执行命令并收集输出。超时视为失败（Err），避免卡死拖垮整轮聚合。
async fn timeout_output(cmd: &mut Command) -> std::io::Result<std::process::Output> {
    match tokio::time::timeout(CLI_TIMEOUT, cmd.output()).await {
        Ok(r) => r,
        Err(_) => Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "codegraph CLI 超时",
        )),
    }
}

/// 跑一条 codegraph 子命令并把 stdout 解析成 `T`。
///
/// **失败判据是「能否解析成 T」，不是退出码**——codegraph 退出码恒为 0，失败信息只在 stdout
/// （如 `✗ CodeGraph not initialized`）。解析失败时返回带 stdout 摘要的 Err，供调用方落日志；
/// 旧实现在此静默返回空，导致「集成从未生效」看起来像「没有命中」。
async fn run_json<T: serde::de::DeserializeOwned>(
    program: &str,
    args: &[&str],
) -> Result<T, String> {
    let out = timeout_output(hidden(program).args(args))
        .await
        .map_err(|e| format!("启动失败: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str::<T>(stdout.trim()).map_err(|e| {
        // 只截前 200 字符：codegraph 的错误是单行人话，够定位；避免把整份 JSON 灌进日志。
        let head: String = stdout.trim().chars().take(200).collect();
        format!("解析 JSON 失败({e}); stdout 摘要: {head}")
    })
}

/// 判定 codegraph 对某项目的可用状态。IO 只有两件事：定位可执行、看 `.codegraph/` 在不在。
/// 状态**决策**本身在 [`classify`] 里，因为那才是能在任何机器上验的部分。
pub async fn detect(work_dir: Option<&Path>) -> CodegraphState {
    let resolved = resolve().await;
    let indexed = work_dir.map(|d| d.join(INDEX_DIR).is_dir());
    classify(resolved, work_dir, indexed)
}

/// 由「可执行解析结果 + 目录 + 该目录是否已索引」得出状态。**纯函数，不碰 IO。**
///
/// 🔴 **抽出来不是为了好看，是为了判据能在没装 codegraph 的机器上真的压到分支。**
/// 第一版把决策留在 `detect` 里、用例直接调 `detect(None)`，而开发机没装 codegraph →
/// 提前返回 `NotInstalled` → **注入（把 `IndexUnknown` 改回 `NotIndexed`）照样全绿**，
/// 也就是缺陷的后端那一半压根没有判据守着。同本仓「注入不变红时先怀疑用例没压到那个分支」。
///
/// 🔴 **`indexed == None`（没给目录）必须是 [`CodegraphState::IndexUnknown`]，不许是
/// `NotIndexed`** —— 我们此时没看过 `.codegraph/`，报「未索引」就是谎报。见枚举上的论证。
fn classify(
    resolved: Option<Resolved>,
    work_dir: Option<&Path>,
    indexed: Option<bool>,
) -> CodegraphState {
    let Some(r) = resolved else {
        return CodegraphState::NotInstalled;
    };
    if !r.on_path {
        // 孤岛：SynaRoute 能用绝对路径调，但用户终端里调不到，需提示重装。
        return CodegraphState::Stranded { path: r.program, version: r.version };
    }
    let (Some(dir), Some(has_index)) = (work_dir, indexed) else {
        return CodegraphState::IndexUnknown { path: r.program, version: r.version };
    };
    let dir = dir.to_string_lossy().to_string();
    if !has_index {
        return CodegraphState::NotIndexed { path: r.program, version: r.version, dir };
    }
    CodegraphState::Ready {
        path: r.program,
        version: r.version,
        dir,
        nodes: None,
        edges: None,
    }
}

/// 查询与关键词相关的种子符号（`query <kw> --json -p <dir> -l <limit>`）。
///
/// 返回按 codegraph 自身 score 降序的符号列表。任何失败都返回 Err 并附原因，由调用方落日志。
pub async fn query_symbols(
    program: &str,
    work_dir: &Path,
    keyword: &str,
    limit: usize,
) -> Result<Vec<SymbolNode>, String> {
    let dir = work_dir.to_string_lossy().to_string();
    let lim = limit.to_string();
    let hits: Vec<QueryHit> = run_json(
        program,
        &["query", keyword, "--json", "-p", &dir, "-l", &lim],
    )
    .await?;
    Ok(hits.into_iter().map(|h| h.node).collect())
}

/// 取某符号的影响面（`impact <sym> --json -p <dir> --depth N`）——跨文件调用链。
/// 实测能从 upstream.rs 追到 proxy.rs 的调用方，是「全链路审查」的核心数据源。
pub async fn impact(
    program: &str,
    work_dir: &Path,
    symbol: &str,
    depth: u32,
) -> Result<Vec<RelatedNode>, String> {
    let dir = work_dir.to_string_lossy().to_string();
    let d = depth.to_string();
    let out: ImpactOut = run_json(
        program,
        &["impact", symbol, "--json", "-p", &dir, "--depth", &d],
    )
    .await?;
    Ok(out.affected)
}

/// 取直接调用方（`callers <sym> --json -p <dir> -l N`）。
///
/// 当前检索主路径用 `impact`（一次调用即覆盖多跳，CLI 次数更省）。此函数供
/// 「只看直接调用方」的场景与单测验证 JSON 形状，保留以免将来重新对字段名。
#[allow(dead_code)]
pub async fn callers(
    program: &str,
    work_dir: &Path,
    symbol: &str,
    limit: usize,
) -> Result<Vec<RelatedNode>, String> {
    let dir = work_dir.to_string_lossy().to_string();
    let lim = limit.to_string();
    let out: CallersOut = run_json(
        program,
        &["callers", symbol, "--json", "-p", &dir, "-l", &lim],
    )
    .await?;
    Ok(out.callers)
}

/// 取直接被调方（`callees <sym> --json -p <dir> -l N`）。见 [`callers`] 的保留理由。
#[allow(dead_code)]
pub async fn callees(
    program: &str,
    work_dir: &Path,
    symbol: &str,
    limit: usize,
) -> Result<Vec<RelatedNode>, String> {
    let dir = work_dir.to_string_lossy().to_string();
    let lim = limit.to_string();
    let out: CalleesOut = run_json(
        program,
        &["callees", symbol, "--json", "-p", &dir, "-l", &lim],
    )
    .await?;
    Ok(out.callees)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- 版本解析（可用性检测的判据，不能看退出码）----

    #[test]
    fn parses_real_version_output() {
        // 实机 `codegraph version` 输出就是裸版本号一行。
        assert_eq!(parse_version("1.5.0\n").as_deref(), Some("1.5.0"));
        // 带 v 前缀 / 前后噪声也要能抓到。
        assert_eq!(parse_version("codegraph v2.10.3").as_deref(), Some("2.10.3"));
        assert_eq!(parse_version("  0.9\n").as_deref(), Some("0.9"));
    }

    #[test]
    fn rejects_non_version_output() {
        // 关键：程序不存在 / 输出人话时不能误判成"已安装"。
        assert!(parse_version("").is_none());
        assert!(parse_version("'codegraph' 不是内部或外部命令").is_none());
        assert!(parse_version("command not found").is_none());
    }

    // ---- JSON 形状（字段名来自实机 dump，README 未文档化）----
    //
    // 这几条钉住的是**上游 JSON 契约**：一旦 codegraph 改字段名，这里先红，
    // 而不是在运行时静默退化成"没有命中"（旧实现正是这个失败模式）。

    #[test]
    fn deserializes_query_json_from_real_capture() {
        // 摘自实机 `codegraph query "collect_declared_tools" --json`（截去长 docstring）。
        let raw = r#"[
          {
            "node": {
              "id": "function:baf22b500c13123640b883f5efe31e09",
              "kind": "function",
              "name": "collect_declared_tools",
              "qualifiedName": "collect_declared_tools",
              "filePath": "src-tauri/src/upstream.rs",
              "language": "rust",
              "startLine": 673,
              "endLine": 694,
              "startColumn": 0,
              "endColumn": 1,
              "docstring": "收集本次 Responses 请求声明的全部工具",
              "signature": "(body: &Value) -> Vec<Value>",
              "visibility": "public",
              "isExported": false,
              "returnType": "Vec",
              "updatedAt": 1785390701509
            },
            "score": 99.12986889573463
          }
        ]"#;
        let hits: Vec<QueryHit> = serde_json::from_str(raw).expect("真实 query JSON 应可解析");
        assert_eq!(hits.len(), 1);
        let n = &hits[0].node;
        assert_eq!(n.name, "collect_declared_tools");
        assert_eq!(n.file_path, "src-tauri/src/upstream.rs");
        // startLine/endLine 是符号级切片的依据，必须解析出来
        assert_eq!(n.start_line, 673);
        assert_eq!(n.end_line, Some(694));
        assert_eq!(n.signature.as_deref(), Some("(body: &Value) -> Vec<Value>"));
    }

    #[test]
    fn deserializes_impact_json_from_real_capture() {
        // 摘自实机 `codegraph impact ... --json --depth 2`：affected 里含跨文件节点
        // （upstream.rs 的符号追到 proxy.rs 的调用方），这是"全链路"的数据来源。
        let raw = r#"{
          "symbol": "collect_declared_tools",
          "depth": 2,
          "nodeCount": 26,
          "edgeCount": 29,
          "affected": [
            { "name": "collect_search_tools", "kind": "function",
              "filePath": "src-tauri/src/upstream.rs", "startLine": 720 },
            { "name": "try_stream_to_key", "kind": "function",
              "filePath": "src-tauri/src/proxy.rs", "startLine": 652 }
          ]
        }"#;
        let out: ImpactOut = serde_json::from_str(raw).expect("真实 impact JSON 应可解析");
        assert_eq!(out.affected.len(), 2);
        assert_eq!(out.affected[1].file_path, "src-tauri/src/proxy.rs");
        assert_eq!(out.affected[1].start_line, Some(652));
    }

    #[test]
    fn deserializes_callers_json_from_real_capture() {
        let raw = r#"{
          "symbol": "collect_declared_tools",
          "callers": [
            { "name": "responses_to_chat", "kind": "function",
              "filePath": "src-tauri/src/upstream.rs", "startLine": 1343 }
          ]
        }"#;
        let out: CallersOut = serde_json::from_str(raw).expect("真实 callers JSON 应可解析");
        assert_eq!(out.callers.len(), 1);
        assert_eq!(out.callers[0].name, "responses_to_chat");
    }

    #[test]
    fn tolerates_missing_optional_fields() {
        // impact/callers 的节点没有 endLine/signature；query 的节点可能缺 docstring。
        // 缺字段必须走 serde default 而非解析失败——否则一个可选字段缺失就让整条链路退化。
        let raw = r#"[{"node":{"kind":"method","name":"f","filePath":"a.java","startLine":10}}]"#;
        let hits: Vec<QueryHit> = serde_json::from_str(raw).expect("缺可选字段也应解析");
        assert_eq!(hits[0].node.end_line, None);
        assert_eq!(hits[0].node.signature, None);
        assert_eq!(hits[0].score, 0.0, "score 缺失走 default");
    }

    #[test]
    fn rejects_error_text_as_json() {
        // codegraph 未初始化时 stdout 是 `✗ CodeGraph not initialized` 且**退出码仍为 0**。
        // 解析必须失败（从而被上报成诊断），而不是当成空结果静默吞掉。
        let err_text = "✗ CodeGraph not initialized in E:\\proj";
        assert!(
            serde_json::from_str::<Vec<QueryHit>>(err_text).is_err(),
            "错误文本不得被当成合法空结果"
        );
    }

    // ---- 三态语义 ----

    #[test]
    fn state_serializes_with_tag_for_frontend() {
        // 前端按 `state` 判分支（未装/孤岛/未知/未索引/就绪各对应不同 UI），故必须带 tag。
        let s = serde_json::to_value(CodegraphState::NotInstalled).unwrap();
        assert_eq!(s["state"], "notInstalled");
        let s = serde_json::to_value(CodegraphState::NotIndexed {
            path: "codegraph".into(),
            version: "1.5.0".into(),
            dir: "C:\\workCode\\proj".into(),
        })
        .unwrap();
        assert_eq!(s["state"], "notIndexed");
        assert_eq!(s["version"], "1.5.0");
        // dir 必须过 IPC：UI 要显示「是哪个项目没索引」。
        assert_eq!(s["dir"], "C:\\workCode\\proj");
        let s = serde_json::to_value(CodegraphState::Stranded {
            path: "F:\\nvm\\v20.20.0\\node_global\\codegraph.cmd".into(),
            version: "1.5.0".into(),
        })
        .unwrap();
        assert_eq!(s["state"], "stranded");
        assert!(s["path"].as_str().unwrap().contains("v20.20.0"));
    }

    #[test]
    fn index_unknown_is_a_distinct_tag_from_not_indexed() {
        // 🔴 这两个 tag 合并过一次，就是用户报的缺陷：自动跟随开着、项目明明已索引，
        // 界面却报「未索引」+ 禁用建索引按钮 + 「请先设置工作目录」（而目录就在上方显示着）。
        // 前端靠 tag 分流，两者同名则那三条 UI 判据无从区分 —— 故这条钉住它们不同名。
        let unknown = serde_json::to_value(CodegraphState::IndexUnknown {
            path: "codegraph".into(),
            version: "1.5.0".into(),
        })
        .unwrap();
        assert_eq!(unknown["state"], "indexUnknown");
        let not_indexed = serde_json::to_value(CodegraphState::NotIndexed {
            path: "codegraph".into(),
            version: "1.5.0".into(),
            dir: "C:\\p".into(),
        })
        .unwrap();
        assert_ne!(
            unknown["state"], not_indexed["state"],
            "「没查」与「查了没有」必须是两个 tag，否则 UI 只能对其中一种说对话"
        );
        // 未知态**不许**带 dir：带了就意味着我们其实知道目录，那就该去判索引而不是报未知。
        assert!(
            unknown.get("dir").is_none(),
            "IndexUnknown 不该有 dir —— 有目录就该判索引"
        );
    }

    /// 装了 codegraph 的机器上才有的 `Resolved`，测试里直接造。
    fn on_path() -> Option<Resolved> {
        Some(Resolved { program: "codegraph".into(), version: "1.5.0".into(), on_path: true })
    }

    #[test]
    fn no_directory_means_unknown_not_unindexed() {
        // 🔴 缺陷的**后端那一半**：`detect(None)` 曾返回 NotIndexed，把「我没查」谎报成
        // 「我查了，没有」。前端据此显示未索引徽标 + 禁用按钮 + 「请先设置工作目录」。
        //
        // 这条**必须**走 classify 而不是 detect：开发机没装 codegraph，`detect` 会在
        // resolve 那一步就返回 NotInstalled，于是注入照样全绿（第一版实测如此）。
        let st = classify(on_path(), None, None);
        assert_eq!(
            st,
            CodegraphState::IndexUnknown { path: "codegraph".into(), version: "1.5.0".into() },
            "没给目录时我们没看过 .codegraph/，只能报「未知」"
        );
        assert!(
            !matches!(st, CodegraphState::NotIndexed { .. }),
            "报「未索引」= 谎报，且会让 UI 说出那三句错话"
        );
    }

    #[test]
    fn unindexed_and_ready_both_name_the_project() {
        // 「哪个项目」是用户看到状态后的第一个问题 —— 自动跟随下目录由会话历史决定，
        // 界面上不显示的话他无从判断这个结论说的是不是他关心的那个仓库。
        let dir = Path::new("C:\\workCode\\reviewCode\\jztac-server");
        let st = classify(on_path(), Some(dir), Some(false));
        match st {
            CodegraphState::NotIndexed { dir: d, .. } => {
                assert_eq!(d, "C:\\workCode\\reviewCode\\jztac-server")
            }
            other => panic!("没索引的目录该报 NotIndexed，实得 {other:?}"),
        }
        let st = classify(on_path(), Some(dir), Some(true));
        match st {
            CodegraphState::Ready { dir: d, .. } => {
                assert_eq!(d, "C:\\workCode\\reviewCode\\jztac-server")
            }
            other => panic!("已索引的目录该报 Ready，实得 {other:?}"),
        }
    }

    #[test]
    fn executable_problems_outrank_the_index_question() {
        // 没装 / 孤岛时索引状态无意义（调不到 codegraph，索引在不在都用不上），
        // 故这两支必须先返回，且**不许**因为「给了目录」就跳过它们。
        let dir = Path::new("C:\\p");
        assert_eq!(classify(None, Some(dir), Some(true)), CodegraphState::NotInstalled);
        let stranded = Some(Resolved {
            program: "F:\\nvm\\v20.20.0\\node_global\\codegraph.cmd".into(),
            version: "1.5.0".into(),
            on_path: false,
        });
        assert!(matches!(
            classify(stranded, Some(dir), Some(true)),
            CodegraphState::Stranded { .. }
        ));
    }

    #[tokio::test]
    async fn detect_delegates_to_classify_and_reads_the_index_dir() {
        // detect 的那两件 IO 要真的接上 classify。空临时目录 = 没有 .codegraph/，
        // 故装了 codegraph 的机器必须报 NotIndexed；没装的机器只能是 NotInstalled/Stranded
        // —— 无论哪种，**都不许是 Ready**（那意味着我们没看目录就宣布就绪）。
        //
        // ⚠️ **本条在没装 codegraph 的机器上压不到那个判断**（resolve 就返回 None →
        // NotInstalled，`indexed` 是什么都不影响结论）。注入实测确认：把 `indexed` 改成恒
        // `true`，本条照样绿。真正守住那一行的是下面那条源码级判据 —— 别把这条读成有覆盖。
        let dir = std::env::temp_dir().join(format!(
            "cg_probe_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        let st = detect(Some(&dir)).await;
        assert!(
            !matches!(st, CodegraphState::Ready { .. }),
            "空目录里没有 .codegraph/，不该报就绪；实得 {st:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `detect` 必须真的去问文件系统「`.codegraph/` 在不在」。
    ///
    /// [`classify`] 是纯函数，所以「索引到底存不存在」这一位的**真实性**只能由 `detect` 保证，
    /// 而上面那条行为用例在没装 codegraph 的机器上压不到它（注入实测：改成恒 `true` 仍绿）。
    /// 失效方向最坏：**未索引的项目被报成就绪**，检索静默退化回按文件匹配而界面说符号级可用。
    ///
    /// 用共享的 `production_code_only` 剥注释 —— 本仓已五次栽在「注释里的字面量满足了断言」，
    /// 而本模块的文档里到处都是 `.codegraph/` 字样，不剥必然自我满足。
    #[test]
    fn detect_must_consult_the_filesystem_for_the_index_dir() {
        let src =
            crate::proxy::custom_headers::production_code_only(include_str!("codegraph.rs"));
        let body = src
            .split_once("pub async fn detect(")
            .and_then(|(_, rest)| rest.split_once("\nfn classify("))
            .map(|(b, _)| b.to_string())
            .expect("detect/classify 改名或换序了 —— 先修判据");
        // 接受常量名或裸字面量两种写法 —— 判据钉的是「问了文件系统」这个**性质**，
        // 不是某一种拼法。钉拼法的判据会在有人改进实现时制造假红（本仓为此撤回过一次改进）。
        let names_the_dir = body.contains("INDEX_DIR") || body.contains(INDEX_DIR);
        assert!(
            names_the_dir && body.contains("is_dir()"),
            "detect 必须用 is_dir() 检查 {INDEX_DIR}/；恒真/恒假地喂给 classify 会让\
             「未索引」被报成「就绪」，而那是静默的"
        );
        // 正向断言：剥完还得剩下东西，否则锚点一变判据就静默退化成什么都没查。
        assert!(body.len() > 80, "取到的 detect 函数体太短，锚点可能已失效");
    }
}
