//! 大脑聚合成员的**只读**检索工具集。
//!
//! ## 为什么要有它
//!
//! 原先的检索是 [`crate::retrieval::retrieve_detailed`] 在成员调用**之前**跑一次：从 prompt
//! 抽关键词、猜哪些文件相关、把内容整包塞进 prompt。猜错了就是白填几万 token，猜漏了模型只能
//! 盲答。给成员一组按需检索的工具，让它自己一步步挖，比一次性猜准得多。
//!
//! ## 边界：只读，永不写盘、永不执行命令
//!
//! 与既有设计一致——聚合只出主意，落盘由客户端（Claude Code 等）自己做。故这里没有
//! `write_file` / `run_command`。唯一起的子进程是 `rg` 与 `codegraph`，都是只读查询。
//!
//! ## 三道防线（每个涉及路径的工具都必须过）
//!
//! 工具参数来自模型输出，而 prompt 里混着检索到的项目文件内容 —— 即模型可能被内容里的
//! 注入指令诱导。且**工具结果会进上游请求体**：读到什么就等于把什么发给第三方中转商。
//!
//! 1. [`crate::aggregate::is_safe_relative_path`]：字符串级，拒 `..`、绝对路径、盘符、UNC
//! 2. [`crate::aggregate::is_within_work_root`]：canonicalize 后仍须在工作目录内，堵链接逃逸
//! 3. [`crate::retrieval::is_sensitive_path`]：凭据类文件一律拒读
//!
//! 第 3 道比前两道更要紧：前两道只防「读到工作目录**外**」，第 3 道防「读到工作目录**内**的
//! 密钥文件」——`.env` 就在项目里，前两道全部通过。且它要判**两次**：一次按模型给的名字，
//! 一次按解析链接后的真实落点（`notes.md` → `.env` 这种目录内链接能骗过按名字那一次）。
//!
//! ## 也承载 MCP `images` 参数的加载
//!
//! [`load_images`] 放在这里而不是 `mcp`：图片路径同样来自外部输入、同样要过上面那三道防线，
//! 而防线实现（[`resolve_readable`]）就在本模块。放两处必然漂移。

use crate::aggregate::{is_safe_relative_path, is_within_work_root};
use crate::proc::hidden;
use crate::retrieval::is_sensitive_path;
use crate::upstream::{ImagePart, ToolDef, ToolInvocation, ToolResultMsg};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// 单张图片的字节上限。超限**明确报错**而非静默丢弃 —— 静默丢会让用户以为模型看了图，
/// 而模型其实在瞎猜（本项目反复防的「看起来在做但没做」）。
pub const MAX_IMAGE_BYTES: u64 = 5 * 1024 * 1024;
/// 单次调用的图片张数上限。
pub const MAX_IMAGES: usize = 4;

/// 允许的图片格式：**两家协议的交集**（Anthropic 与 OpenAI Chat 都支持这四种）。
/// 只在一家支持的格式会让「换个成员模型就 400」，不如入口直接拒掉并说清楚。
const IMAGE_TYPES: &[(&str, &str)] = &[
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("gif", "image/gif"),
    ("webp", "image/webp"),
];

/// 单次工具结果的字符上限。防一个 `read_file` 把整轮预算吃光。
pub const RESULT_CHAR_CAP: usize = 8000;

pub const READ_FILE: &str = "read_file";
pub const GREP: &str = "grep";
pub const LIST_DIR: &str = "list_dir";
pub const CODEGRAPH_QUERY: &str = "codegraph_query";

/// 与 [`crate::retrieval`] 的 `walk_search` 保持一致的忽略目录（减噪，不是安全控制）。
const IGNORE_DIRS: &[&str] = &[
    "node_modules", "target", "dist", ".git", ".codegraph", "build", ".next", ".venv", "venv",
    "__pycache__", ".idea", ".vscode",
];

/// 一轮聚合内共享的工具执行环境。
///
/// codegraph 只在这里解析**一次**：`crate::codegraph::resolve` 每次都要起子进程探版本，
/// 若放进每次工具调用里，一轮下来会多起十几个进程。
pub struct ToolEnv {
    work_dir: PathBuf,
    /// codegraph CLI 程序名/路径。None = 不可用（此时 `codegraph_query` **不声明**给模型，
    /// 而不是声明了再报错——不给模型一个必然失败的选项）。
    codegraph: Option<String>,
    /// codegraph 不可用的原因，供聚合日志展示（未安装 / 该项目未建索引）。不静默。
    pub codegraph_note: Option<String>,
    /// 单次工具结果的字符上限（可配，默认 [`RESULT_CHAR_CAP`]）。
    ///
    /// 做成字段而不是常量：它直接决定每轮历史的增量，是用户控成本最直接的旋钮。
    /// 调小 → 每次看到的片段更短、可能多调几次；调大 → 单次信息更全但额度涨得快。
    result_cap: usize,
}

impl ToolEnv {
    pub async fn detect(work_dir: &Path) -> Self {
        let indexed = work_dir.join(".codegraph").is_dir();
        let resolved = crate::codegraph::resolve().await;
        let (codegraph, note) = match (resolved, indexed) {
            (Some(r), true) => (Some(r.program), None),
            (Some(_), false) => (
                None,
                Some("codegraph 已安装但该项目未建索引（缺 .codegraph/），未向成员提供 codegraph_query".into()),
            ),
            (None, _) => (
                None,
                Some("codegraph 未安装，未向成员提供 codegraph_query".into()),
            ),
        };
        Self {
            work_dir: work_dir.to_path_buf(),
            codegraph,
            codegraph_note: note,
            result_cap: RESULT_CHAR_CAP,
        }
    }

    /// 设置单次工具结果的字符上限。0 或过小值会让工具变得无用，故 clamp 到 1000~40000。
    pub fn with_result_cap(mut self, cap: u32) -> Self {
        self.result_cap = (cap as usize).clamp(1_000, 40_000);
        self
    }

    /// 仅测试/内部构造：不探测 codegraph。
    #[cfg(test)]
    fn bare(work_dir: &Path) -> Self {
        Self {
            work_dir: work_dir.to_path_buf(),
            codegraph: None,
            codegraph_note: None,
            result_cap: RESULT_CHAR_CAP,
        }
    }
}

/// 向模型声明的工具列表。描述里刻意写明「只读」与路径约束，减少模型试探被拒路径的空转轮次。
pub fn tool_defs(env: &ToolEnv) -> Vec<ToolDef> {
    let mut v = vec![
        ToolDef {
            name: READ_FILE.into(),
            description: "读取工作目录下某个文本文件的内容（带行号）。可选行区间以只取需要的部分。\
                只读，不能写文件。路径必须是工作目录下的相对路径。"
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "工作目录下的相对路径，如 src/main.rs（也接受 file_path / filePath 等等价写法）" },
                    "start_line": { "type": "integer", "description": "起始行（1 起，含）。省略为文件开头" },
                    "end_line": { "type": "integer", "description": "结束行（1 起，含）。省略为文件末尾" }
                },
                "required": ["path"]
            }),
        },
        ToolDef {
            name: GREP.into(),
            description: "在工作目录里按正则搜索内容，返回 `文件:行号: 内容`。\
                用于定位「哪个文件里出现了某个符号/字符串」，再用 read_file 看细节。"
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "正则表达式（ripgrep 语法）" },
                    "glob": { "type": "string", "description": "可选文件过滤，如 *.rs 或 src/**/*.ts" }
                },
                "required": ["pattern"]
            }),
        },
        ToolDef {
            name: LIST_DIR.into(),
            description: "列出工作目录下某个目录的内容，用于先摸清项目结构再决定读哪些文件。".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "相对路径，省略为工作目录根" },
                    "depth": { "type": "integer", "description": "递归深度 1~3，默认 1" }
                }
            }),
        },
    ];
    if env.codegraph.is_some() {
        v.push(ToolDef {
            name: CODEGRAPH_QUERY.into(),
            description: "查项目符号索引：mode=symbols 按名字找符号定义位置（返回文件与行区间）；\
                mode=impact 查改动某符号会影响哪些调用方。只给位置不给正文，拿到行区间后用 \
                read_file 精确取内容。"
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "keyword": { "type": "string", "description": "符号名或关键词" },
                    "mode": { "type": "string", "enum": ["symbols", "impact"], "description": "默认 symbols" },
                    "depth": { "type": "integer", "description": "impact 的追溯深度 1~3，默认 2" }
                },
                "required": ["keyword"]
            }),
        });
    }
    v
}

/// 只读路径的三道防线。返回 canonicalize 后的真实路径，或**给模型看的**拒绝原因。
///
/// 顺序有讲究：先判 `exists` 再判 `is_within_work_root`，因为后者在 canonicalize 失败时
/// fail-closed，对一个根本不存在的路径会报出「疑似链接逃逸」这种误导性原因。
fn resolve_readable(work_dir: &Path, rel: &str) -> Result<PathBuf, String> {
    // ① 字符串级：`..`、绝对路径、盘符、UNC
    if !is_safe_relative_path(rel) {
        return Err(format!(
            "路径 `{rel}` 被拒：只接受工作目录下的相对路径，不允许 `..`、绝对路径或盘符/UNC。"
        ));
    }
    let full = work_dir.join(rel);
    // ③ 敏感文件（按模型给的名字先判一次，快速拒）
    if is_sensitive_path(&full) {
        return Err(format!(
            "路径 `{rel}` 被拒：该文件可能含凭据（.env / 密钥 / 证书 / 密码库类），工具一律不读。\
             请换其他文件。"
        ));
    }
    if !full.exists() {
        return Err(format!("路径 `{rel}` 不存在。可先用 list_dir 确认目录结构。"));
    }
    // ② 解析符号链接后必须仍在工作目录内
    if !is_within_work_root(work_dir, &full) {
        return Err(format!(
            "路径 `{rel}` 被拒：解析符号链接后落在工作目录之外。"
        ));
    }
    let real = full
        .canonicalize()
        .map_err(|e| format!("路径 `{rel}` 无法解析：{e}"))?;
    // ③′ 对**真实落点**再判一次：目录内的链接（notes.md → .env）能骗过按名字那一次。
    if is_sensitive_path(&real) {
        return Err(format!(
            "路径 `{rel}` 被拒：它指向一个可能含凭据的文件。"
        ));
    }
    // ③″ 硬链接别名：canonicalize **不还原硬链接**（硬链接是同一文件内容的另一目录项，
    //     没有「目标」可解析），故良性命名的硬链接（`notes.md` 与 `.env` 同 inode）能骗过
    //     上面按名字、按落点两次判定。枚举该文件在同卷上的**全部**硬链接名，任一叶子名敏感即拒。
    //     仅 Windows 实现（本项目 Windows-only；Unix 为 no-op）。多链接文件极罕见，枚举只在
    //     `number_of_links > 1` 时才发生，正常单链接文件零开销。
    if let Some(alias) = sensitive_hardlink_alias(&real) {
        return Err(format!(
            "路径 `{rel}` 被拒：它与一个可能含凭据的文件（{alias}）是同一份内容的硬链接。"
        ));
    }
    Ok(real)
}

/// 若 `real` 存在任一**敏感命名**的硬链接别名，返回那个敏感名字；否则 `None`。
///
/// 硬链接绕过的本质：`.env` 与 `notes.md` 指向同一 inode 时，读 `notes.md` 等于读 `.env`，
/// 而路径字符串与 canonicalize 结果都是 `notes.md` —— 名字/落点两道判定全部放行。
/// 唯一可靠判据是比对**文件身份**：这里直接枚举同卷上指向同一文件记录的所有路径名
/// （Win32 `FindFirstFileNameW`/`FindNextFileNameW`），逐个过 [`is_sensitive_path`]。
///
/// **保守 fail-open**：枚举 API 本身失败（非 NTFS 卷、权限不足、路径过长）时返回 `None`。
/// 理由：这是名字/落点两道判定**之后**的第三道加固，前两道仍在；且枚举失败通常意味着底层
/// 文件系统根本不支持硬链接、本无此攻击面。宁可放行也不把正常读路径在边缘环境上全拦死。
#[cfg(windows)]
fn sensitive_hardlink_alias(real: &Path) -> Option<String> {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::Foundation::{
        GetLastError, ERROR_HANDLE_EOF, ERROR_MORE_DATA, HANDLE, MAX_PATH,
    };
    use windows::Win32::Storage::FileSystem::{
        FindClose, FindFirstFileNameW, FindNextFileNameW,
    };

    // 单链接文件（绝大多数）直接跳过枚举：只有 number_of_links > 1 才可能有别名。
    // std 的 number_of_links() 是 unstable，这里用 metadata 无法拿到，故不预判、直接枚举——
    // 但 FindFirstFileNameW 对单链接文件也只返回它自己一条，开销极小（一次系统调用）。

    // FindXxxFileNameW 返回的是「卷内相对路径」（如 \path\to\.env），不含盘符。
    // 敏感判定只看**叶子文件名**，卷内相对路径足够取到叶子名，无需拼回盘符。
    let wide: Vec<u16> = real.as_os_str().encode_wide().chain(std::iter::once(0)).collect();

    // 缓冲区长度（字符数）。先给 MAX_PATH，不够时按 ERROR_MORE_DATA 提示的长度重来。
    let mut len: u32 = MAX_PATH;
    let mut buf: Vec<u16> = vec![0u16; len as usize];

    // SAFETY: wide 以 NUL 结尾；buf 容量 = len；handle 成功后必定 FindClose。
    let handle: HANDLE = loop {
        let mut try_len = len;
        let r = unsafe {
            FindFirstFileNameW(PCWSTR(wide.as_ptr()), 0, &mut try_len, PWSTR(buf.as_mut_ptr()))
        };
        match r {
            Ok(h) => break h,
            Err(_) => {
                // 缓冲不足：try_len 被写成所需长度，扩容重试一次。其它错误一律放行。
                if unsafe { GetLastError() } == ERROR_MORE_DATA && try_len > len {
                    len = try_len;
                    buf = vec![0u16; len as usize];
                    continue;
                }
                return None;
            }
        }
    };

    let mut found: Option<String> = None;
    loop {
        // 当前 buf 里是一个卷内相对路径（NUL 结尾）。取叶子名判敏感。
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        let rel_path = std::ffi::OsString::from_wide(&buf[..end]);
        if is_sensitive_path(Path::new(&rel_path)) {
            let leaf = Path::new(&rel_path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| rel_path.to_string_lossy().into_owned());
            found = Some(leaf);
            break;
        }
        // 下一条别名。
        let mut next_len = len;
        let r = unsafe {
            FindNextFileNameW(handle, &mut next_len, PWSTR(buf.as_mut_ptr()))
        };
        if r.is_err() {
            let e = unsafe { GetLastError() };
            if e == ERROR_MORE_DATA && next_len > len {
                len = next_len;
                buf = vec![0u16; len as usize];
                // 重试当前这一条（FindNextFileNameW 缓冲不足时不推进游标）。
                let mut retry_len = len;
                if unsafe {
                    FindNextFileNameW(handle, &mut retry_len, PWSTR(buf.as_mut_ptr()))
                }
                .is_err()
                {
                    break;
                }
                continue;
            }
            // ERROR_HANDLE_EOF = 枚举结束（正常）；其它错误保守停止。
            let _ = ERROR_HANDLE_EOF;
            break;
        }
    }

    // SAFETY: handle 来自成功的 FindFirstFileNameW。
    unsafe {
        let _ = FindClose(handle);
    }
    found
}

/// 非 Windows：no-op。本项目为 Windows-only；Unix 上文件符号链接已由 canonicalize 处理，
/// 硬链接攻击面不在本项目目标范围内。
#[cfg(not(windows))]
fn sensitive_hardlink_alias(_real: &Path) -> Option<String> {
    None
}

/// 加载 MCP `images` 参数指定的图片，编码成可直接进请求体的 [`ImagePart`]。
///
/// 全部校验都是**硬失败**（返回 Err 让整次调用报错），不是跳过：
/// 用户传了图就是想让模型看图，静默丢掉后模型基于残缺输入照常作答，用户拿到一个看起来
/// 正常、实则没看图的答案 —— 那比直接报错糟糕得多。
///
/// 路径同样过 [`resolve_readable`] 的三道防线：图片路径也可能是指向工作目录外的符号链接。
pub fn load_images(work_dir: Option<&str>, rels: &[String]) -> Result<Vec<ImagePart>, String> {
    if rels.is_empty() {
        return Ok(Vec::new());
    }
    let Some(dir) = work_dir.filter(|d| !d.trim().is_empty()) else {
        return Err(
            "传了 images 但没有工作目录：请同时传 cwd（图片路径是相对于项目根目录的）。".into(),
        );
    };
    if rels.len() > MAX_IMAGES {
        return Err(format!(
            "images 最多 {} 张，本次传了 {} 张。请减少张数后重试。",
            MAX_IMAGES,
            rels.len()
        ));
    }
    let root = Path::new(dir);
    let mut out = Vec::with_capacity(rels.len());
    for rel in rels {
        let real = resolve_readable(root, rel)?;
        let ext = real
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
            .unwrap_or_default();
        let Some((_, media_type)) = IMAGE_TYPES.iter().find(|(e, _)| *e == ext) else {
            return Err(format!(
                "`{rel}` 的格式不支持：只接受 {}（两家上游协议共同支持的格式）。",
                IMAGE_TYPES
                    .iter()
                    .map(|(e, _)| *e)
                    .collect::<Vec<_>>()
                    .join(" / ")
            ));
        };
        let size = std::fs::metadata(&real)
            .map_err(|e| format!("`{rel}` 无法读取：{e}"))?
            .len();
        if size > MAX_IMAGE_BYTES {
            return Err(format!(
                "`{rel}` 有 {:.1} MB，超过单张 {:.0} MB 上限。请压缩或裁剪后重试。",
                size as f64 / 1_048_576.0,
                MAX_IMAGE_BYTES as f64 / 1_048_576.0
            ));
        }
        let bytes = std::fs::read(&real).map_err(|e| format!("`{rel}` 读取失败：{e}"))?;
        out.push(ImagePart {
            media_type: (*media_type).to_string(),
            base64: STANDARD.encode(&bytes),
        });
    }
    Ok(out)
}

/// 执行一次工具调用。**永不返回 Err**：失败也是一条 `is_error` 结果回给模型，让它换个方向，
/// 而不是让整个成员失败（一个不存在的文件名不该毁掉一次聚合）。
pub async fn execute(env: &ToolEnv, call: &ToolInvocation) -> ToolResultMsg {
    let result = if call.args.as_object().is_none() {
        // Null/非对象 = 模型吐的 arguments 不是合法 JSON（见 upstream::parse_openai_tool_call）
        Err("工具参数不是合法的 JSON 对象，请检查后重新发起调用。".to_string())
    } else {
        match call.name.as_str() {
            READ_FILE => read_file_tool(env, &call.args),
            LIST_DIR => list_dir_tool(env, &call.args),
            GREP => grep_tool(env, &call.args).await,
            CODEGRAPH_QUERY => codegraph_tool(env, &call.args).await,
            other => Err(format!(
                "未知工具 `{other}`。可用工具：{}",
                tool_defs(env)
                    .iter()
                    .map(|t| t.name.as_str())
                    .collect::<Vec<_>>()
                    .join(" / ")
            )),
        }
    };
    match result {
        Ok(content) => ToolResultMsg {
            id: call.id.clone(),
            content: cap_result(content, env.result_cap),
            is_error: false,
        },
        Err(msg) => ToolResultMsg {
            id: call.id.clone(),
            content: msg,
            is_error: true,
        },
    }
}

/// 为「超出单轮工具调用上限」的那些 call 造一条 is_error 结果，**不执行**。
///
/// 协议要求每个 tool_use 都要有对应结果（缺一条上游 400），所以超限的不能丢，只能回一条不做实事的
/// 错误结果 —— 既保持一一对应，又不起子进程/不读盘/不占结果预算。调用方据此让模型下一轮少调点。
pub fn over_limit_result(call: &ToolInvocation, limit: usize) -> ToolResultMsg {
    ToolResultMsg {
        id: call.id.clone(),
        content: format!(
            "本轮工具调用数超过上限（{limit}），这一条未执行。请一次只调用少量必要的工具，\
             拿到结果后再决定下一步。"
        ),
        is_error: true,
    }
}

/// 单次结果截断。明确告诉模型「被截断了、该怎么缩小范围」——静默截断会让它以为文件就这么长。
fn cap_result(s: String, cap: usize) -> String {
    if s.chars().count() <= cap {
        return s;
    }
    let head: String = s.chars().take(cap).collect();
    format!(
        "{head}\n… [结果已截断至 {cap} 字符。请缩小范围重查：read_file 用 \
         start_line/end_line，grep 用更具体的 pattern 或 glob]"
    )
}

/// 各参数的**别名**：模型常按自己熟悉的工具口径发参数名，我们照收。
///
/// 为什么必须有（实测证据，勿当过度设计）：2026-08-02 的真机日志里，
/// `claude-opus-4-8` 连续 **8 轮**都把路径发成 `file_path`（那是 Claude Code 原生 Read 工具的
/// 参数名，Claude 系模型被训成这么发），每次都被 `req_str` 判「缺少必填参数 `path`」拒掉 ——
/// **报错并没有让它改口**，整个成员的检索能力等于归零，而同一次聚合里发 `path` 的另一个成员
/// 7 次全部成功。
///
/// 判据取舍：与其指望模型读懂错误提示后自纠（实测不会），不如入口直接兼容。
/// 这不是放宽安全边界 —— 取到值之后仍然过完整的三道路径防线。
const ARG_ALIASES: &[(&str, &[&str])] = &[
    // Claude Code 原生 Read/Edit 用 file_path；部分模型习惯 filename / filePath。
    ("path", &["file_path", "filePath", "filename", "file", "target_file"]),
    // rg 口径叫 regex/query；Claude Code 的 Grep 工具本身也用 pattern。
    ("pattern", &["regex", "query", "search", "q"]),
    // Claude Code 的 Grep 用 glob；部分模型发 include/file_pattern。
    ("glob", &["include", "file_pattern", "filter"]),
    ("start_line", &["startLine", "offset", "from_line"]),
    ("end_line", &["endLine", "limit", "to_line"]),
    ("depth", &["max_depth", "maxDepth", "recursive_depth"]),
    ("keyword", &["symbol", "name", "term"]),
    ("mode", &["kind", "type"]),
];

/// 按「正名 + 别名」顺序找出第一个存在的键。返回该键名，供取值与日志用。
fn resolve_key<'a>(args: &Value, key: &'a str) -> Option<&'a str> {
    if args.get(key).is_some_and(|v| !v.is_null()) {
        return Some(key);
    }
    ARG_ALIASES
        .iter()
        .find(|(canon, _)| *canon == key)?
        .1
        .iter()
        .copied()
        .find(|a| args.get(*a).is_some_and(|v| !v.is_null()))
}

/// 取可选的正整数参数（含别名）。
///
/// 数字也接受字符串形态（`"120"`）：模型偶发把整数发成字符串，为此报错纯属自找麻烦。
fn opt_u32(args: &Value, key: &str) -> Option<u32> {
    let k = resolve_key(args, key)?;
    let v = args.get(k)?;
    v.as_u64()
        .or_else(|| v.as_str().and_then(|s| s.trim().parse::<u64>().ok()))
        .map(|n| n.min(u32::MAX as u64) as u32)
}

/// 取必填字符串参数（含别名）。
///
/// 失败提示里**列出接受的别名**：万一模型发的是别名之外的第三种名字，它能从提示里挑一个用，
/// 而不是像日志里那样重复 8 轮同一个错。
fn req_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    resolve_key(args, key)
        .and_then(|k| args.get(k))
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            let aliases = ARG_ALIASES
                .iter()
                .find(|(canon, _)| *canon == key)
                .map(|(_, a)| a.join("` / `"))
                .unwrap_or_default();
            if aliases.is_empty() {
                format!("缺少必填参数 `{key}`（应为非空字符串）。")
            } else {
                format!(
                    "缺少必填参数 `{key}`（应为非空字符串）。也接受这些等价写法：`{aliases}`。\
                     请用其中任意一个重新调用。"
                )
            }
        })
}

/// `read_file` 单次最多读取的字节数。与 walk_grep 的 per-file 2MB、rg 的 --max-filesize 一致。
///
/// 为什么必须有：`BufReader::lines()` 会把**整行**读进内存后才轮到我们判 RESULT_CHAR_CAP，
/// 而 cap 检查在 push 之前——对一个 80MB 单行文件（压缩产物 / 单行 JSON·CSV 导出），
/// 第一行就把 80MB 读进内存（实测峰值约 240MB、阻塞 tokio worker ~2s），且模型可 6 轮反复触发。
/// 用 `Read::take` 从**读取源头**封顶，把这条路的内存与耗时都压到常数级。
/// 2MB ≈ 4 万行代码，任何正常的「读某个函数」区间都够用；真要看更大的，先 grep 定位再小区间读。
const MAX_READ_BYTES: u64 = 2 * 1024 * 1024;

/// `read_file`：按行流式读取，带行号输出。
///
/// 用 `BufReader` 逐行读而非 `read_to_string`：既天然支持行区间，又能在超大文件上提前停下，
/// 不会把一个 500MB 的日志整份读进内存。**从读取源头用 [`MAX_READ_BYTES`] 封顶**，
/// 否则单个超长行会绕过 RESULT_CHAR_CAP（cap 检查在整行已进内存之后）。
fn read_file_tool(env: &ToolEnv, args: &Value) -> Result<String, String> {
    use std::io::{BufRead, BufReader, Read};

    let rel = req_str(args, "path")?;
    let real = resolve_readable(&env.work_dir, rel)?;
    if real.is_dir() {
        return Err(format!("`{rel}` 是目录，请用 list_dir。"));
    }
    let start = opt_u32(args, "start_line").unwrap_or(1).max(1);
    let end = opt_u32(args, "end_line").unwrap_or(u32::MAX);
    if end < start {
        return Err(format!("end_line({end}) 小于 start_line({start})。"));
    }

    let f = std::fs::File::open(&real).map_err(|e| format!("打开 `{rel}` 失败：{e}"))?;
    // 关键：take 封顶读取字节。超过 MAX_READ_BYTES 的部分根本不进内存。
    let mut reader = BufReader::new(f).take(MAX_READ_BYTES);
    let mut out = String::new();
    let mut chars = 0usize;
    let mut lineno = 0u32;
    let mut hit_cap = false;
    // 续读锚点：字符预算耗尽时「下一次该从第几行开始读」。
    //
    // 必须与 `lineno` 分开记：预算在**行边界**耗尽时当前行根本没进 out（下面 `remaining == 0`
    // 那条分支），若按 `lineno + 1` 提示续读，这一行既不在本次结果里、又被续读跳过 → 永久丢失。
    // 行**内**截断时同理：该行只给了前半截，续读也必须回到这一行本身才能拿到剩下的。
    let mut resume_from = 0u32;
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).map_err(|e| {
            format!("`{rel}` 读取失败（可能不是 UTF-8 文本文件，二进制文件无法读取）：{e}")
        })?;
        if n == 0 {
            break; // EOF 或已读满 MAX_READ_BYTES
        }
        let trimmed = line.trim_end_matches(['\n', '\r']);
        lineno += 1;
        if lineno < start {
            continue;
        }
        if lineno > end {
            break;
        }
        let remaining = env.result_cap.saturating_sub(chars);
        if remaining == 0 {
            // 这一行一个字符都没给出去 → 续读必须**从它本身**开始。
            hit_cap = true;
            resume_from = lineno;
            break;
        }
        // 单行也要按剩余预算截断：Take 只封住「读进内存」，但一个 2MB 单行仍会被整条 push 进
        // 结果（cap 检查在 push 之前、每行一次）。这里对当前行只取剩余额度的字符，
        // 使 read_file 自身返回就 ≤ RESULT_CHAR_CAP，而不是依赖 execute() 层的 cap_result 兜底。
        let piece: String = trimmed.chars().take(remaining).collect();
        let line_truncated = piece.chars().count() < trimmed.chars().count();
        chars += piece.chars().count() + 8;
        // 行内被截断时**如实标注**：否则模型会把半行当完整内容用（截在字符串/括号中间尤其危险）。
        let mark = if line_truncated { " …[本行被截断]" } else { "" };
        out.push_str(&format!("{lineno}\t{piece}{mark}\n"));
        if line_truncated {
            hit_cap = true;
            resume_from = lineno; // 该行尾部还没给 → 续读回到这一行
            break;
        }
    }
    // 读到了 MAX_READ_BYTES 边界但还没自然结束区间：告知被截断。
    //
    // `limit() == 0` 单独**不足以**判定「后面还有内容」：文件大小恰好等于 MAX_READ_BYTES 时，
    // 封顶与 EOF 重合、其实已经读全了，仅凭 limit 会误报「只读了前一段」并催模型去做多余的 grep。
    // 故再向底层探读 1 字节：真能读到才是真被截断。
    let byte_capped = if reader.limit() == 0 && !hit_cap {
        let mut probe = [0u8; 1];
        reader
            .into_inner()
            .read(&mut probe)
            .map(|n| n > 0)
            .unwrap_or(false)
    } else {
        false
    };
    if out.is_empty() {
        let end_label = if end == u32::MAX {
            "末尾".to_string()
        } else {
            end.to_string()
        };
        if byte_capped {
            return Err(format!(
                "`{rel}` 前 {} 字节内不含 {start}~{end_label} 行的内容（文件很大）。\
                 请用 grep 先定位，再用更靠前的行区间读。",
                MAX_READ_BYTES
            ));
        }
        return Err(format!(
            "`{rel}` 在 {start}~{end_label} 行区间内没有内容（该文件共 {lineno} 行）。"
        ));
    }
    let mut header = format!("文件 {rel}（行号在前）\n");
    if hit_cap {
        // 报「完整给到第几行」+「从第几行续读」，两者都以 resume_from 为准：
        // 行边界耗尽时 resume_from 那行一个字符都没给，行内截断时它只给了前半截 ——
        // 两种情况下「完整给到」的都是 resume_from - 1，而续读都必须回到 resume_from 本身。
        header.push_str(&format!(
            "[内容较多，只完整给到第 {} 行；继续看请传 start_line={resume_from}]\n",
            resume_from.saturating_sub(1)
        ));
    } else if byte_capped {
        header.push_str(&format!(
            "[文件超过 {} 字节，只读了前一段；如需后文请用 grep 定位后再读]\n",
            MAX_READ_BYTES
        ));
    }
    Ok(header + &out)
}

/// 单次 `list_dir` 的条目上限。超大目录（如未忽略的产物目录）不该刷满整个结果预算。
const LIST_ENTRY_CAP: usize = 400;

/// `list_dir`：迭代式 DFS 列目录。
///
/// 敏感文件仍**列出**但标注不可读：文件名本身不是凭据，而让模型知道「这个存在但读不到」
/// 比让它对一个看不见的文件反复试探更省轮次。
fn list_dir_tool(env: &ToolEnv, args: &Value) -> Result<String, String> {
    let rel = resolve_key(args, "path")
        .and_then(|k| args.get(k))
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .unwrap_or(".");
    let depth = opt_u32(args, "depth").unwrap_or(1).clamp(1, 3) as usize;
    let root = resolve_readable(&env.work_dir, rel)?;
    if !root.is_dir() {
        return Err(format!("`{rel}` 不是目录，请用 read_file。"));
    }

    let mut lines: Vec<String> = Vec::new();
    let mut truncated = false;
    // 迭代式 DFS（与 retrieval::walk_search 同款，避免深递归）。
    let mut stack: Vec<(PathBuf, usize)> = vec![(root.clone(), 0)];
    while let Some((dir, d)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut items: Vec<_> = entries.flatten().collect();
        // 排序让输出稳定（read_dir 顺序随文件系统而变，不稳定的输出会破坏上游的 prompt 缓存）
        items.sort_by_key(|e| e.file_name());
        for entry in items {
            if lines.len() >= LIST_ENTRY_CAP {
                truncated = true;
                break;
            }
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            let Ok(ft) = entry.file_type() else { continue };
            let shown = display_rel(&root, &path, rel);
            if ft.is_dir() {
                if IGNORE_DIRS.contains(&name.as_str()) {
                    lines.push(format!("{shown}/  [已跳过]"));
                    continue;
                }
                lines.push(format!("{shown}/"));
                if d + 1 < depth {
                    stack.push((path, d + 1));
                }
            } else if ft.is_file() {
                let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                if is_sensitive_path(&path) {
                    lines.push(format!("{shown}  ({size} B) [受保护，不可读]"));
                } else {
                    lines.push(format!("{shown}  ({size} B)"));
                }
            }
        }
    }
    lines.sort();
    let mut out = format!("目录 {rel}（深度 {depth}）\n{}", lines.join("\n"));
    if truncated {
        out.push_str(&format!(
            "\n… [条目超过 {LIST_ENTRY_CAP} 个已截断，请给更具体的 path 或减小 depth]"
        ));
    }
    Ok(out)
}

/// 把绝对路径显示成「相对于本次 list 根」的形式，并带上用户给的前缀便于模型直接拿去 read_file。
fn display_rel(root: &Path, path: &Path, rel_prefix: &str) -> String {
    let tail = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/");
    if rel_prefix == "." {
        tail
    } else {
        format!("{}/{tail}", rel_prefix.trim_end_matches('/').replace('\\', "/"))
    }
}

/// 单次 `grep` 返回的匹配行上限。
const MAX_MATCH_LINES: usize = 120;

/// `grep`：优先用 ripgrep，起不来时降级为纯 Rust 字面量遍历（**并明说降级了**）。
///
/// 关键点：rg **搜所有文件**，`.env` 只要命中就会出现在 stdout 里。故命中结果必须逐条过
/// [`is_sensitive_path`] 再交给模型 —— 这一层不是减噪，是防凭据外发。
async fn grep_tool(env: &ToolEnv, args: &Value) -> Result<String, String> {
    let pattern = req_str(args, "pattern")?;
    let glob = resolve_key(args, "glob")
        .and_then(|k| args.get(k))
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty());
    if let Some(g) = glob {
        // glob 同样来自模型，同样按不可信处理：`../**` 会让 rg 走出工作目录。
        if g.contains("..") || Path::new(g).is_absolute() {
            return Err(format!("glob `{g}` 被拒：不允许 `..` 或绝对路径。"));
        }
    }

    let mut cmd = hidden("rg");
    cmd.args([
        "--line-number",
        "--no-heading",
        "--no-messages",
        "--color",
        "never",
        // 每文件最多 5 条：防一个匹配密集的文件占满全部配额，牺牲广度。
        "--max-count",
        "5",
        "--max-filesize",
        "2M",
        "--max-depth",
        "8",
        "--glob",
        "!node_modules",
        "--glob",
        "!target",
        "--glob",
        "!dist",
        "--glob",
        "!.git",
        "--glob",
        "!*.lock",
        "--glob",
        "!*.min.js",
    ]);
    if let Some(g) = glob {
        cmd.args(["--glob", g]);
    }
    // `-e` 显式声明模式，否则以 `-` 开头的正则会被当成 rg 的选项。
    cmd.args(["-e", pattern]).current_dir(&env.work_dir);

    let out = match cmd.output().await {
        Ok(o) => o,
        Err(_) => return walk_grep(&env.work_dir, pattern),
    };
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    match out.status.code() {
        Some(0) => {}
        // rg 约定：1 = 无命中（不是错误）。
        Some(1) => {
            return Ok(format!("`{pattern}` 无命中。"));
        }
        _ => {
            // 2 及其他：多为正则语法错误。原样回给模型让它自己改，比笼统报「搜索失败」有用。
            let why = if stderr.is_empty() {
                "ripgrep 执行失败".to_string()
            } else {
                stderr
            };
            return Err(format!("grep 失败：{why}"));
        }
    }

    let stdout = String::from_utf8_lossy(&out.stdout);
    let (kept, denied, truncated) = filter_rg_hits(&env.work_dir, &stdout);
    if kept.is_empty() && denied == 0 {
        return Ok(format!("`{pattern}` 无命中。"));
    }
    let mut out_s = format!("grep `{pattern}` 命中 {} 行\n{}", kept.len(), kept.join("\n"));
    if denied > 0 {
        out_s.push_str(&format!(
            "\n[另有 {denied} 行命中于可能含凭据的文件，已按安全策略排除]"
        ));
    }
    if truncated {
        out_s.push_str(&format!(
            "\n… [命中过多，只给前 {MAX_MATCH_LINES} 行，请用更具体的 pattern 或 glob]"
        ));
    }
    Ok(out_s)
}

/// 从 rg 的原始 stdout 里筛出可回给模型的命中行。
/// 返回 (可用行, 被安全策略排除的行数, 是否因命中过多而截断)。
///
/// 拆成纯函数不是为了复用，是为了**可验证**：本机不一定装 rg，而「凭据文件的命中不得外发」
/// 这条判据不该只在恰好装了 rg 的机器上才被测到。
fn filter_rg_hits(work_dir: &Path, stdout: &str) -> (Vec<String>, usize, bool) {
    let mut kept: Vec<String> = Vec::new();
    let mut denied = 0usize;
    let mut truncated = false;
    for line in stdout.lines() {
        let Some((path, num, text)) = split_rg_line(line) else {
            continue;
        };
        if is_sensitive_path(&work_dir.join(path)) {
            denied += 1;
            continue;
        }
        if kept.len() >= MAX_MATCH_LINES {
            truncated = true;
            break;
        }
        let shown = path.replace('\\', "/");
        kept.push(format!("{shown}:{num}: {}", text.trim_end()));
    }
    (kept, denied, truncated)
}

/// 解析 rg 的 `路径:行号:内容`。行号段必须是纯数字，否则说明这行不是匹配行（如提示信息）。
fn split_rg_line(line: &str) -> Option<(&str, &str, &str)> {
    let (path, rest) = line.split_once(':')?;
    let (num, text) = rest.split_once(':')?;
    if !num.is_empty() && num.chars().all(|c| c.is_ascii_digit()) {
        Some((path, num, text))
    } else {
        None
    }
}

/// ripgrep 起不来时的兜底：纯 Rust 目录遍历 + **字面量**大小写不敏感匹配。
///
/// 明确在结果头部写出「已降级、不支持正则、glob 被忽略」——静默降级会让模型以为自己的正则
/// 没命中，进而得出错误结论。这是本项目反复防的那类「看起来在做但没做」。
///
/// 直接在 async 上下文里做阻塞 IO：与 [`crate::retrieval`] 的 `walk_search` 同款，靠
/// MAX_SCAN_FILES / MAX_DEPTH 把耗时压在亚秒级。
fn walk_grep(work_dir: &Path, pattern: &str) -> Result<String, String> {
    const MAX_SCAN_FILES: usize = 3000;
    const MAX_DEPTH: usize = 5;
    const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;

    let needle = pattern.to_lowercase();
    let mut kept: Vec<String> = Vec::new();
    let mut scanned = 0usize;
    let mut stack: Vec<(PathBuf, usize)> = vec![(work_dir.to_path_buf(), 0)];
    'outer: while let Some((dir, d)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_dir() {
                if d < MAX_DEPTH && !IGNORE_DIRS.contains(&name.as_str()) && !name.starts_with('.') {
                    stack.push((path, d + 1));
                }
                continue;
            }
            if !ft.is_file() || scanned >= MAX_SCAN_FILES {
                continue;
            }
            if entry.metadata().map(|m| m.len()).unwrap_or(0) > MAX_FILE_BYTES {
                continue;
            }
            scanned += 1;
            if is_sensitive_path(&path) {
                // 先判敏感再读：不读进内存就不可能外发。
                continue;
            }
            // 非 UTF-8（二进制）自然失败，跳过。
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            let rel = path
                .strip_prefix(work_dir)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            let mut per_file = 0usize;
            for (i, line) in content.lines().enumerate() {
                if !line.to_lowercase().contains(&needle) {
                    continue;
                }
                if kept.len() >= MAX_MATCH_LINES {
                    break 'outer;
                }
                kept.push(format!("{rel}:{}: {}", i + 1, line.trim_end()));
                per_file += 1;
                if per_file >= 5 {
                    break;
                }
            }
        }
    }
    let head = "[ripgrep 不可用，已降级为字面量匹配：不支持正则，glob 参数被忽略]\n";
    if kept.is_empty() {
        return Ok(format!("{head}`{pattern}` 无命中。"));
    }
    Ok(format!("{head}命中 {} 行\n{}", kept.len(), kept.join("\n")))
}

/// `codegraph_query`：查符号索引。**只给位置、不给正文** —— 拿到 `文件:起-止` 后再让模型用
/// `read_file` 精确取那段，比这里直接返回源码省得多（也避免同一段内容在历史里出现两次）。
async fn codegraph_tool(env: &ToolEnv, args: &Value) -> Result<String, String> {
    let Some(program) = env.codegraph.as_deref() else {
        // tool_defs 在不可用时不声明该工具，正常走不到这里；模型硬编造工具名时兜底。
        return Err("codegraph 不可用（未安装或该项目未建索引）。请改用 grep + read_file。".into());
    };
    let keyword = req_str(args, "keyword")?;
    let mode = resolve_key(args, "mode")
        .and_then(|k| args.get(k))
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .unwrap_or("symbols");

    match mode {
        "symbols" => {
            let hits = crate::codegraph::query_symbols(program, &env.work_dir, keyword, 8)
                .await
                .map_err(|e| format!("codegraph query 失败：{e}"))?;
            if hits.is_empty() {
                return Ok(format!("codegraph 未找到与 `{keyword}` 相关的符号。"));
            }
            let lines: Vec<String> = hits
                .iter()
                .map(|n| {
                    let end = n
                        .end_line
                        .map(|e| e.to_string())
                        .unwrap_or_else(|| "?".into());
                    let sig = n.signature.clone().unwrap_or_default();
                    format!(
                        "{} {}  —  {}:{}-{}  {}",
                        n.kind, n.name, n.file_path, n.start_line, end, sig
                    )
                })
                .collect();
            Ok(format!(
                "codegraph 符号命中 {} 条（用 read_file + 上面的行区间取正文）\n{}",
                lines.len(),
                lines.join("\n")
            ))
        }
        "impact" => {
            let depth = opt_u32(args, "depth").unwrap_or(2).clamp(1, 3);
            let rel = crate::codegraph::impact(program, &env.work_dir, keyword, depth)
                .await
                .map_err(|e| format!("codegraph impact 失败：{e}"))?;
            if rel.is_empty() {
                return Ok(format!("`{keyword}` 没有查到受影响的调用方。"));
            }
            let lines: Vec<String> = rel
                .iter()
                .take(40)
                .map(|r| {
                    let line = r.start_line.map(|l| l.to_string()).unwrap_or_else(|| "?".into());
                    format!(
                        "{} {}  —  {}:{}",
                        r.kind.clone().unwrap_or_else(|| "symbol".into()),
                        r.name,
                        r.file_path,
                        line
                    )
                })
                .collect();
            Ok(format!(
                "改动 `{keyword}` 会影响 {} 处（深度 {depth}，最多列 40 条）\n{}",
                rel.len(),
                lines.join("\n")
            ))
        }
        other => Err(format!("mode `{other}` 不支持，只能是 symbols 或 impact。")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 建一个隔离的临时工作目录。名字带测试名，避免并行测试互相踩。
    fn work(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "synaroute_agent_tools_{}_{}",
            std::process::id(),
            tag
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn call(name: &str, args: Value) -> ToolInvocation {
        ToolInvocation {
            id: "t1".into(),
            name: name.into(),
            args,
        }
    }

    // ================= 三道防线 =================

    #[test]
    fn defense_1_rejects_escape_shaped_paths() {
        let w = work("d1");
        std::fs::write(w.join("ok.txt"), "hi").unwrap();
        for bad in [
            "../secret.txt",
            "a/../../b.txt",
            "/etc/passwd",
            "C:\\Windows\\win.ini",
            "\\\\server\\share\\x.txt",
        ] {
            let e = resolve_readable(&w, bad).expect_err("逃逸形状的路径必须被拒");
            assert!(
                e.contains("相对路径"),
                "拒绝原因应指明只接受相对路径，实际：{e}"
            );
        }
        // 正常路径不受影响（否则防线把工具废掉了）
        assert!(resolve_readable(&w, "ok.txt").is_ok());
        std::fs::remove_dir_all(&w).ok();
    }

    #[test]
    fn defense_3_rejects_credential_files_inside_work_dir() {
        // 这一道最要紧：.env 就在工作目录**内**，前两道全部通过。
        // 工具结果会进上游请求体，读到就等于把凭据发给第三方中转商。
        let w = work("d3");
        std::fs::create_dir_all(w.join("config")).unwrap();
        for f in [".env", "config/.env.local", "server.pem", "secrets.json"] {
            std::fs::write(w.join(f), "SECRET=abc").unwrap();
            let e = resolve_readable(&w, f).expect_err("凭据类文件必须被拒");
            assert!(e.contains("凭据"), "拒绝原因应点明凭据风险，实际：{e}");
        }
        // 反例：名字吓人但是源码，必须能读（否则代码审查会漏核心实现）
        std::fs::write(w.join("token_service.rs"), "fn f() {}").unwrap();
        assert!(resolve_readable(&w, "token_service.rs").is_ok());
        std::fs::remove_dir_all(&w).ok();
    }

    /// 尝试建一个指向 `target` 的目录链接。返回 false = 本机无权限。
    ///
    /// Windows 走 `mklink /J`（junction，普通权限即可，pnpm 建 node_modules 就用它）。
    /// 参数必须用 `raw_arg` 自己加引号：`Command::arg` 只在含空格时才加引号，而 cmd 的
    /// mklink 对不带引号、且末段以 `.` 开头的路径（如 `...\.env`）会报「无效语法」。
    fn try_dir_link(link: &Path, target: &Path) -> bool {
        #[cfg(windows)]
        {
            mklink(&format!(
                "/c mklink /J \"{}\" \"{}\"",
                link.display(),
                target.display()
            ))
        }
        #[cfg(not(windows))]
        {
            std::os::unix::fs::symlink(target, link).is_ok()
        }
    }

    #[test]
    fn defense_2_rejects_link_dir_escaping_work_root() {
        // 字符串级那道看不见链接：`vendor/x.txt` 里没有 `..`，却能读到工作目录外。
        let w = work("d2");
        let outside = work("d2_outside");
        std::fs::write(outside.join("x.txt"), "outside secret").unwrap();
        // junction 普通权限即可创建，不该跳过（会静默跳过的安全测试等于没有测试）。
        assert!(
            try_dir_link(&w.join("vendor"), &outside),
            "无法创建目录链接，本环境无法验证链接逃逸防线"
        );
        let e = resolve_readable(&w, "vendor/x.txt").expect_err("穿透链接目录必须被拒");
        assert!(
            e.contains("工作目录之外"),
            "拒绝原因应点明落在工作目录外，实际：{e}"
        );
        std::fs::remove_dir_all(&w).ok();
        std::fs::remove_dir_all(&outside).ok();
    }

    #[test]
    fn defense_3_also_checks_resolved_target_not_just_name() {
        // 请求名 benign、真实落点敏感：
        // - 第一道（字符串）：`notes.md` 里没有 `..`，通过
        // - 第二道（落点在 root 内）：目标就在工作目录里，通过
        // - 只有对**真实落点**再判一次敏感，才能挡住
        let w = work("d3b");
        std::fs::write(w.join(".env"), "TOKEN=abc").unwrap();
        let link = w.join("notes.md");
        // 首选最真实的形状：文件符号链接 notes.md → .env。
        let shape = if try_file_link(&link, &w.join(".env")) {
            "文件符号链接"
        } else {
            // Windows 非开发者模式建不出文件链接。退化成 junction 指向一个**名为 .env 的
            // 目录** —— 请求名 benign、canonicalize 后叶子名敏感，走的是同一段判定代码。
            // 刻意不 return 跳过：一条会静默跳过的安全测试等于没有测试。
            std::fs::create_dir_all(w.join("store/.env")).unwrap();
            assert!(
                try_dir_link(&link, &w.join("store/.env")),
                "文件链接与 junction 都建不出来，本环境无法验证该防线"
            );
            "目录 junction"
        };
        let e = resolve_readable(&w, "notes.md").expect_err("指向凭据文件的链接必须被拒");
        assert!(e.contains("凭据"), "{shape} 形状下的拒绝原因不对：{e}");
        std::fs::remove_dir_all(&w).ok();
    }

    #[test]
    fn defense_3_name_check_is_not_redundant_with_target_check() {
        // 反向形状：名字敏感、真实落点无害（`.env` 是个指向普通目录的链接）。
        // 按落点判的那一次会放行，只有**按名字**判的那一次能挡住。
        // 两次判定各自覆盖一个方向，去掉任一个都有洞——故两次都不能省。
        let w = work("d3c");
        std::fs::create_dir_all(w.join("plain")).unwrap();
        std::fs::write(w.join("plain/readme.txt"), "ok").unwrap();
        assert!(
            try_dir_link(&w.join(".env"), &w.join("plain")),
            "无法创建目录链接，本环境无法验证该防线"
        );
        let e = resolve_readable(&w, ".env").expect_err("名字敏感就该拒，不看落点");
        assert!(e.contains("凭据"), "{e}");
        std::fs::remove_dir_all(&w).ok();
    }

    #[test]
    #[cfg(windows)]
    fn defense_3_target_check_catches_windows_name_aliases() {
        // Windows 特异：ADS 语法（`.env::$DATA`）与 8.3 短名（`ENV~1`）能骗过**按名字**那道
        // （字符串既不含 `..` 也不匹配敏感名），但 canonicalize 后叶子名变回 `.env`，
        // 只有**按真实落点**那道能拦住。这条钉死「第二次判定不是冗余」——现有链接测试在建不出
        // 链接的机器上会退化，而这两种形式无需任何特殊权限就能复现。
        let w = work("d3_win");
        std::fs::write(w.join(".env"), "TOKEN=x\n").unwrap();
        // ADS 语法：始终可测（不依赖 8.3 是否启用）
        let e = resolve_readable(&w, ".env::$DATA").expect_err("ADS 别名必须被拒");
        assert!(e.contains("凭据"), "ADS 形式应被按落点那道拦住：{e}");
        std::fs::remove_dir_all(&w).ok();
    }

    /// 尝试建一个**硬链接** `link` → `target`（同一份内容的另一个目录项）。
    /// Windows 上 `mklink /H` 普通权限即可（无需管理员，已实测），故这条安全测试不该被跳过。
    #[cfg(windows)]
    fn try_hard_link(link: &Path, target: &Path) -> bool {
        mklink(&format!(
            "/c mklink /H \"{}\" \"{}\"",
            link.display(),
            target.display()
        ))
    }
    // 非 Windows 版本只被 Windows 专属测试调用，macOS 上 clippy 报 unused。
    #[cfg(not(windows))]
    #[allow(dead_code)]
    fn try_hard_link(link: &Path, target: &Path) -> bool {
        std::fs::hard_link(target, link).is_ok()
    }

    #[test]
    #[cfg(windows)]
    fn defense_3_rejects_benign_named_hardlink_to_credential_file() {
        // 硬链接是三道防线里最后一个洞（上一轮审查记为 P3）：
        // 硬链接**没有「目标」可解析** —— 它就是同一份文件内容的另一个目录项，
        // 故 canonicalize(`notes.md`) 仍然是 `notes.md`，按名字、按落点两次判定全部放行，
        // 而读到的字节和 `.env` 一模一样。
        // 只有枚举「该文件记录的全部硬链接名」才能发现它其实也叫 `.env`。
        let w = work("d3_hardlink");
        std::fs::write(w.join(".env"), "API_KEY=super_secret\n").unwrap();
        assert!(
            try_hard_link(&w.join("notes.md"), &w.join(".env")),
            "无法创建硬链接，本环境无法验证该防线（mklink /H 普通权限即可，不该失败）"
        );
        // 先自证攻击形状成立：两个名字确实是同一份内容。
        assert_eq!(
            std::fs::read_to_string(w.join("notes.md")).unwrap(),
            "API_KEY=super_secret\n",
            "硬链接未生效，测试前提不成立"
        );
        // 前两道判定对 `notes.md` 都会放行（名字无害、canonicalize 后仍是 notes.md），
        // 唯一能拦住的就是硬链接别名枚举。
        let e = resolve_readable(&w, "notes.md")
            .expect_err("指向凭据文件的硬链接必须被拒（去掉别名枚举这条即变红）");
        assert!(
            e.contains("凭据") && e.contains("硬链接"),
            "拒绝原因应点明硬链接与凭据风险，实际：{e}"
        );
        std::fs::remove_dir_all(&w).ok();
    }

    #[test]
    #[cfg(windows)]
    fn hardlink_check_does_not_reject_ordinary_files() {
        // 加固不能把正常读路径拦死（否则防线把工具废掉了，比漏判更早暴露）。
        // 覆盖三种形状：普通单链接文件、名字吓人的源码、两个都无害的互为硬链接的文件。
        let w = work("d3_hardlink_ok");
        std::fs::write(w.join("main.rs"), "fn main() {}\n").unwrap();
        assert!(resolve_readable(&w, "main.rs").is_ok(), "普通文件必须能读");

        // 名字吓人但不在敏感名单里的源码，且是单链接
        std::fs::write(w.join("token_service.rs"), "fn f() {}\n").unwrap();
        assert!(
            resolve_readable(&w, "token_service.rs").is_ok(),
            "源码不该被误伤"
        );

        // 两个无害名字互为硬链接：枚举会看到两条，但都不敏感 → 必须放行
        assert!(
            try_hard_link(&w.join("copy.rs"), &w.join("main.rs")),
            "无法创建硬链接"
        );
        assert!(
            resolve_readable(&w, "copy.rs").is_ok(),
            "无害名字之间的硬链接不该被拒"
        );
        assert!(
            resolve_readable(&w, "main.rs").is_ok(),
            "原文件在有了无害别名后仍应可读"
        );
        std::fs::remove_dir_all(&w).ok();
    }

    #[test]
    #[cfg(windows)]
    fn hardlink_check_also_guards_the_image_loader() {
        // 图片入口走同一个 resolve_readable，故同样受这道防线保护。
        // 形状：`shot.png` 与 `.env` 是硬链接 —— 若放行，凭据内容会被 base64 塞进请求体发给上游。
        let w = work("d3_hardlink_img");
        std::fs::write(w.join(".env"), "TOKEN=abc\n").unwrap();
        assert!(
            try_hard_link(&w.join("shot.png"), &w.join(".env")),
            "无法创建硬链接"
        );
        let dir = w.to_string_lossy().to_string();
        let e = load_images(Some(&dir), &["shot.png".into()])
            .expect_err("指向凭据文件的硬链接图片必须被拒");
        assert!(e.contains("凭据"), "{e}");
        std::fs::remove_dir_all(&w).ok();
    }

    /// 尝试建一个指向文件的符号链接。返回 false = 本机无权限（Windows 需开发者模式）。
    fn try_file_link(link: &Path, target: &Path) -> bool {        #[cfg(windows)]
        {
            mklink(&format!(
                "/c mklink \"{}\" \"{}\"",
                link.display(),
                target.display()
            ))
        }
        #[cfg(not(windows))]
        {
            std::os::unix::fs::symlink(target, link).is_ok()
        }
    }

    /// 用 `cmd` 执行一条 mklink 命令行（已自带引号）。
    #[cfg(windows)]
    fn mklink(raw: &str) -> bool {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("cmd")
            .raw_arg(raw)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    // ================= 工具行为 =================

    #[test]
    fn read_file_range_is_inclusive_and_numbered() {
        let w = work("rf");
        let body: String = (1..=20).map(|i| format!("line{i}\n")).collect();
        std::fs::write(w.join("a.rs"), body).unwrap();
        let env = ToolEnv::bare(&w);
        let out = read_file_tool(&env, &json!({ "path": "a.rs", "start_line": 3, "end_line": 5 }))
            .unwrap();
        // 行号必须带上：模型据此在答案里引用 file:line
        assert!(out.contains("3\tline3"), "{out}");
        assert!(out.contains("5\tline5"), "{out}");
        assert!(!out.contains("line2") && !out.contains("line6"), "区间应含端点且不多取：{out}");
        std::fs::remove_dir_all(&w).ok();
    }

    #[test]
    fn read_file_errors_are_actionable() {        let w = work("rf_err");
        std::fs::create_dir_all(w.join("sub")).unwrap();
        let env = ToolEnv::bare(&w);
        // 不存在 → 提示可以先 list_dir，而不是干巴巴一句失败
        let e = read_file_tool(&env, &json!({ "path": "nope.rs" })).unwrap_err();
        assert!(e.contains("不存在") && e.contains("list_dir"), "{e}");
        // 目录 → 指路到 list_dir
        let e = read_file_tool(&env, &json!({ "path": "sub" })).unwrap_err();
        assert!(e.contains("list_dir"), "{e}");
        // 缺必填参数
        let e = read_file_tool(&env, &json!({})).unwrap_err();
        assert!(e.contains("path"), "{e}");
        // 区间反了
        std::fs::write(w.join("a.rs"), "x\n").unwrap();
        let e = read_file_tool(&env, &json!({ "path": "a.rs", "start_line": 9, "end_line": 2 }))
            .unwrap_err();
        assert!(e.contains("end_line"), "{e}");
        std::fs::remove_dir_all(&w).ok();
    }

    #[test]
    fn read_file_bounds_a_giant_single_line() {
        // 回归护栏：一个 5MB 单行文件（压缩产物/单行 JSON）不得把整行读进内存。
        // MAX_READ_BYTES=2MB 从源头封顶，返回内容仍被 RESULT_CHAR_CAP 收到 8000 字符以内。
        let w = work("rf_giant");
        let giant = "x".repeat(5 * 1024 * 1024);
        std::fs::write(w.join("min.js"), &giant).unwrap();
        let env = ToolEnv::bare(&w);
        let out = read_file_tool(&env, &json!({ "path": "min.js" })).unwrap();
        // 内容正确（前 8000 字符量级），且远小于 5MB —— 证明没有把整行塞进结果
        assert!(out.chars().count() < 20_000, "结果不该接近原文件大小：{}", out.chars().count());
        assert!(out.contains("min.js"), "{}", &out[..out.char_indices().nth(60).map(|(i,_)|i).unwrap_or(out.len())]);
        std::fs::remove_dir_all(&w).ok();
    }

    #[test]
    fn read_file_reports_byte_cap_when_range_past_2mb() {
        // 请求的行区间落在 2MB 之后：不该谎报「没有内容」，而是提示文件过大、先 grep 定位。
        let w = work("rf_past");
        // 每行 100 字节 × 30000 行 = 3MB，请求第 29999 行（在 2MB 之后）
        let body: String = (1..=30_000).map(|i| format!("{:0>98}\n", i)).collect();
        std::fs::write(w.join("big.log"), body).unwrap();
        let env = ToolEnv::bare(&w);
        let e = read_file_tool(&env, &json!({ "path": "big.log", "start_line": 29999 }))
            .unwrap_err();
        assert!(e.contains("grep") && e.contains("文件很大"), "{e}");
        std::fs::remove_dir_all(&w).ok();
    }

    #[test]
    fn read_file_char_cap_at_line_boundary_loses_no_line() {
        // 回归护栏（P2/high）：字符预算恰在**行边界**耗尽时，被挡下的那一行
        // 既不能出现在首屏（否则续读重复它），又必须能被续读拿到（否则永久丢失）。
        // 续读锚点必须是 `lineno` 本身；若退回成 `lineno+1`，header 会声称「完整给到第 N 行」
        // 而第 N 行其实一个字符都没给 —— 本测试随即变红。
        let w = work("rf_boundary");
        // 每行输出计入 92 + 8 = 100 字符；80 行正好累计到 RESULT_CHAR_CAP(8000)，
        // 第 81 行落在行边界上被挡下。行内容各自可辨（LINE0080 / LINE0081…）。
        let body: String = (1..=200)
            .map(|i| format!("LINE{i:04}{}\n", "x".repeat(84)))
            .collect();
        std::fs::write(w.join("big.rs"), body).unwrap();
        let env = ToolEnv::bare(&w);

        let first = read_file_tool(&env, &json!({ "path": "big.rs" })).unwrap();
        assert!(first.contains("start_line="), "首屏应因预算耗尽给出续读锚点：{first:.200}");
        // 从 header 解析续读锚点，不硬编码行号（对 +8 余量的算术改动更鲁棒）。
        let resume: u32 = first
            .split("start_line=")
            .nth(1)
            .and_then(|s| s.split(|c: char| !c.is_ascii_digit()).next())
            .and_then(|s| s.parse().ok())
            .expect("header 里应有 start_line=<数字>");

        // header 声称「只完整给到第 resume-1 行」——那一行必须真的在首屏里（完整）。
        let last_full = format!("LINE{:04}", resume - 1);
        assert!(
            first.contains(&last_full),
            "首屏应完整含第 {} 行（{last_full}）——header 声称给到了它却没给，即丢行",
            resume - 1
        );
        // 续读起点那行不该已出现在首屏（否则续读会重复它）。
        let resume_line = format!("LINE{resume:04}");
        assert!(
            !first.contains(&resume_line),
            "续读起点那行不该出现在首屏：{resume_line}"
        );
        // 关键：从 header 给的锚点续读，必须拿到那一行 —— 无丢失。
        let second =
            read_file_tool(&env, &json!({ "path": "big.rs", "start_line": resume })).unwrap();
        assert!(
            second.contains(&resume_line),
            "续读 start_line={resume} 必须拿到第 {resume} 行（{resume_line}），否则该行永久丢失"
        );
        std::fs::remove_dir_all(&w).ok();
    }

    #[test]
    fn read_file_inline_truncation_resume_keeps_the_tail() {
        // 回归护栏（P2/high，行**内**截断路径）：某行只给了前半截并标注截断时，
        // 续读锚点必须回到**这一行本身**才能拿到尾部；若写成 lineno+1，该行尾部永久丢失。
        let mut body = String::new();
        for i in 1..=79 {
            // "SHORT"(5) + {:03}(3) + 84 = 92 内容字符，输出计入 100。
            body.push_str(&format!("SHORT{i:03}{}\n", "y".repeat(84)));
        }
        // 第 80 行远超剩余额度（100），会被行内截断。头尾各放可辨标记。
        body.push_str(&format!("H80HEAD{}T80TAILEND\n", "a".repeat(200)));
        for i in 81..=90 {
            body.push_str(&format!("AFTER{i:03}\n"));
        }
        let w = work("rf_inline");
        std::fs::write(w.join("big.rs"), body).unwrap();
        let env = ToolEnv::bare(&w);

        let first = read_file_tool(&env, &json!({ "path": "big.rs" })).unwrap();
        assert!(first.contains("本行被截断"), "第 80 行应被行内截断并标注：{first:.200}");
        assert!(first.contains("H80HEAD"), "首屏应含被截断行的头部");
        assert!(!first.contains("T80TAILEND"), "被截断行的尾部不该出现在首屏");
        let resume: u32 = first
            .split("start_line=")
            .nth(1)
            .and_then(|s| s.split(|c: char| !c.is_ascii_digit()).next())
            .and_then(|s| s.parse().ok())
            .expect("header 里应有 start_line=<数字>");
        // 从锚点续读必须能拿到那一行的尾部。锚点错成 lineno+1 时，续读落到下一行、尾部丢失。
        let second =
            read_file_tool(&env, &json!({ "path": "big.rs", "start_line": resume })).unwrap();
        assert!(
            second.contains("T80TAILEND"),
            "续读必须拿回被截断行的尾部，否则该行尾部永久丢失（resume={resume}）"
        );
        std::fs::remove_dir_all(&w).ok();
    }

    #[test]
    fn read_file_at_exactly_2mib_does_not_false_report_truncation() {
        // 回归护栏（P3）：文件大小**恰好等于** MAX_READ_BYTES 时，封顶与真实 EOF 重合，
        // 其实已读全。仅凭 `reader.limit()==0` 会误报「只读了前一段」，催模型做多余 grep。
        // 现按「再探读 1 字节」判定真截断；本测试锁死：恰好 2MiB 不得出现该误报。
        let w = work("rf_exact2m");
        // 每行 63 位零填充 + '\n' = 64 字节；32768 行 × 64 = 2097152 = 2*1024*1024 恰好。
        let body: String = (1..=32_768u32).map(|i| format!("{i:063}\n")).collect();
        assert_eq!(body.len(), 2 * 1024 * 1024, "构造的文件必须恰好 2MiB");
        std::fs::write(w.join("exact.log"), body).unwrap();
        let env = ToolEnv::bare(&w);
        // 请求靠近末尾的一小段：输出很小、不触发 hit_cap，但读到文件真实 EOF。
        let out = read_file_tool(&env, &json!({ "path": "exact.log", "start_line": 32_760 }))
            .unwrap();
        assert!(out.contains("032768"), "末行应被读到：{out:.120}");
        assert!(
            !out.contains("只读了前一段"),
            "文件恰好 2MiB 已读全，不得误报截断：{out:.200}"
        );
        std::fs::remove_dir_all(&w).ok();
    }

    #[test]
    fn list_dir_marks_protected_and_skips_ignored_dirs() {
        let w = work("ld");
        std::fs::write(w.join("main.rs"), "fn main() {}").unwrap();
        std::fs::write(w.join(".env"), "T=1").unwrap();
        std::fs::create_dir_all(w.join("node_modules/pkg")).unwrap();
        std::fs::create_dir_all(w.join("src")).unwrap();
        let env = ToolEnv::bare(&w);
        let out = list_dir_tool(&env, &json!({})).unwrap();
        assert!(out.contains("main.rs"), "{out}");
        assert!(out.contains("src/"), "{out}");
        // 敏感文件仍列出但标注不可读——让模型别去试，省一轮空转
        assert!(out.contains(".env") && out.contains("受保护"), "{out}");
        assert!(out.contains("node_modules/  [已跳过]"), "{out}");
        // 深度 1 不该下钻进 src 之下
        assert!(!out.contains("node_modules/pkg"), "{out}");
        std::fs::remove_dir_all(&w).ok();
    }

    #[test]
    fn walk_grep_never_reads_credential_files() {
        // 这条是 grep 路径的核心安全判据，且不依赖 rg 是否安装（走的是兜底实现）。
        // rg 那条同样过 is_sensitive_path 过滤（见 grep_tool 里对每条命中的判定）。
        let w = work("wg");
        std::fs::write(w.join("app.rs"), "let token = \"NEEDLE_X\";\n").unwrap();
        std::fs::write(w.join(".env"), "API_KEY=NEEDLE_X\n").unwrap();
        std::fs::write(w.join("server.pem"), "NEEDLE_X\n").unwrap();
        let out = walk_grep(&w, "NEEDLE_X").unwrap();
        assert!(out.contains("app.rs"), "正常源码应命中：{out}");
        assert!(!out.contains(".env"), "凭据文件内容不得出现在结果里：{out}");
        assert!(!out.contains("server.pem"), "{out}");
        assert!(out.contains("降级"), "兜底路径必须明说降级了，不能静默：{out}");
        std::fs::remove_dir_all(&w).ok();
    }

    #[test]
    fn filter_rg_hits_drops_credential_files_and_counts_them() {
        // rg 搜**所有**文件，`.env` 命中会直接出现在 stdout 里。这一层不是减噪，是防凭据外发。
        // 用合成的 rg 输出验证，故本机没装 rg 也能测到这条判据。
        let w = work("frh");
        let stdout = concat!(
            "src/app.rs:12:let t = \"NEEDLE\";\n",
            ".env:3:API_KEY=NEEDLE\n",
            "config\\secrets.json:1:{\"k\":\"NEEDLE\"}\n",
            "keys/server.pem:1:NEEDLE\n",
            "src\\lib.rs:7:// NEEDLE\n",
            "这行不是命中格式\n",
        );
        let (kept, denied, truncated) = filter_rg_hits(&w, stdout);
        assert_eq!(denied, 3, "三个凭据类文件的命中都要被排除：{kept:?}");
        assert!(!truncated);
        assert_eq!(
            kept,
            vec![
                "src/app.rs:12: let t = \"NEEDLE\";".to_string(),
                // 反斜杠统一成正斜杠，模型可以直接拿去 read_file
                "src/lib.rs:7: // NEEDLE".to_string(),
            ]
        );
        std::fs::remove_dir_all(&w).ok();
    }

    #[test]
    fn filter_rg_hits_caps_match_count() {
        let w = work("frh2");
        let stdout: String = (1..=MAX_MATCH_LINES + 30)
            .map(|i| format!("src/a.rs:{i}:hit\n"))
            .collect();
        let (kept, _, truncated) = filter_rg_hits(&w, &stdout);
        assert_eq!(kept.len(), MAX_MATCH_LINES);
        assert!(truncated, "超上限必须报出来，否则模型以为就这么多命中");
        std::fs::remove_dir_all(&w).ok();
    }

    #[tokio::test]
    async fn grep_tool_excludes_credential_files() {
        // 覆盖 rg 那条路：rg **搜所有文件**，`.env` 命中会直接出现在 stdout 里，
        // 必须靠逐条 is_sensitive_path 过滤掉。本机没装 rg 时会落到 walk_grep 兜底，
        // 那条同样过滤，故断言两条路径都成立。
        let w = work("gt");
        std::fs::write(w.join("app.rs"), "let t = \"NEEDLE_Y\";\n").unwrap();
        std::fs::write(w.join(".env"), "API_KEY=NEEDLE_Y\n").unwrap();
        let env = ToolEnv::bare(&w);
        let out = grep_tool(&env, &json!({ "pattern": "NEEDLE_Y" })).await.unwrap();
        assert!(out.contains("app.rs"), "正常源码应命中：{out}");
        assert!(
            !out.contains("API_KEY") && !out.contains(".env:"),
            "凭据文件的命中不得回给模型：{out}"
        );
        std::fs::remove_dir_all(&w).ok();
    }

    #[tokio::test]
    async fn grep_rejects_escaping_glob() {
        let w = work("gg");
        let env = ToolEnv::bare(&w);
        let r = grep_tool(&env, &json!({ "pattern": "x", "glob": "../**/*.rs" })).await;
        let e = r.unwrap_err();
        assert!(e.contains("被拒"), "{e}");
        std::fs::remove_dir_all(&w).ok();
    }

    #[test]
    fn split_rg_line_requires_numeric_line_no() {
        assert_eq!(
            split_rg_line("src/a.rs:42:let x = 1;"),
            Some(("src/a.rs", "42", "let x = 1;"))
        );
        // 非匹配行（如 rg 的提示）不该被当成命中
        assert_eq!(split_rg_line("some: note: here"), None);
        assert_eq!(split_rg_line("no colons"), None);
    }

    #[tokio::test]
    async fn execute_never_fails_the_member_on_bad_input() {
        // 失败必须是一条 is_error 结果回给模型（让它换方向），而不是把整个成员打挂。
        let w = work("ex");
        let env = ToolEnv::bare(&w);

        let r = execute(&env, &call("read_file", json!({ "path": "../x" }))).await;
        assert!(r.is_error);
        assert_eq!(r.id, "t1", "结果必须带回原 call id，否则上游认不出对应关系");

        // args 不是对象（模型吐了截断的 arguments，见 upstream::parse_openai_tool_call）
        let r = execute(&env, &call("read_file", Value::Null)).await;
        assert!(r.is_error && r.content.contains("JSON"), "{}", r.content);

        // 编造的工具名 → 报错并列出可用工具
        let r = execute(&env, &call("write_file", json!({ "path": "a" }))).await;
        assert!(r.is_error && r.content.contains("read_file"), "{}", r.content);
        std::fs::remove_dir_all(&w).ok();
    }

    #[test]
    fn cap_result_tells_model_how_to_narrow() {
        let long = "x".repeat(RESULT_CHAR_CAP + 100);
        let out = cap_result(long, RESULT_CHAR_CAP);
        assert!(out.chars().count() < RESULT_CHAR_CAP + 200);
        // 静默截断会让模型以为文件就这么长，据此下错结论
        assert!(out.contains("已截断") && out.contains("start_line"), "{out}");
        assert_eq!(cap_result("短".into(), RESULT_CHAR_CAP), "短");
    }

    #[test]
    fn codegraph_tool_not_declared_when_unavailable() {
        let w = work("td");
        let env = ToolEnv::bare(&w);
        let defs = tool_defs(&env);
        let names: Vec<&str> = defs.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec![READ_FILE, GREP, LIST_DIR]);
        // 不声明的理由：不给模型一个必然失败的选项，省掉一轮空转
        assert!(!names.contains(&CODEGRAPH_QUERY));
        std::fs::remove_dir_all(&w).ok();
    }

    #[test]
    fn tool_schemas_are_object_typed_with_required() {
        let w = work("ts");
        let env = ToolEnv::bare(&w);
        for t in tool_defs(&env) {
            assert_eq!(t.input_schema["type"], "object", "{} 的 schema 形状不对", t.name);
            assert!(!t.description.trim().is_empty(), "{} 缺描述", t.name);
        }
        std::fs::remove_dir_all(&w).ok();
    }

    #[test]
    fn read_file_accepts_claude_code_style_file_path_alias() {
        // 真机回归（2026-08-02 日志实证）：claude-opus-4-8 连续 8 轮把路径发成 `file_path`
        // （Claude Code 原生 Read 工具的参数名），每次都被判「缺少必填参数 path」拒掉，
        // 报错没能让它改口 —— 该成员整轮检索能力归零，而同次聚合里发 `path` 的成员 7 次全成。
        // 故入口必须兼容。去掉 ARG_ALIASES 里的 file_path，这条立刻变红。
        let w = work("rf_alias");
        let body: String = (1..=20).map(|i| format!("line{i}\n")).collect();
        std::fs::create_dir_all(w.join("src/main/java")).unwrap();
        std::fs::write(w.join("src/main/java/CustomThreadPool.java"), &body).unwrap();
        let env = ToolEnv::bare(&w);

        // 日志里那条被拒调用的原样形状
        let out = read_file_tool(
            &env,
            &json!({ "file_path": "src/main/java/CustomThreadPool.java" }),
        )
        .expect("file_path 别名必须被接受（真机上模型就是这么发的）");
        assert!(out.contains("1\tline1"), "{out}");

        // 其余别名同样接受
        for k in ["filePath", "filename", "file", "target_file"] {
            assert!(
                read_file_tool(&env, &json!({ k: "src/main/java/CustomThreadPool.java" })).is_ok(),
                "别名 {k} 应被接受"
            );
        }
        // 正名仍然优先且可用
        assert!(read_file_tool(&env, &json!({ "path": "src/main/java/CustomThreadPool.java" })).is_ok());
        std::fs::remove_dir_all(&w).ok();
    }

    #[test]
    fn alias_resolution_does_not_weaken_path_defenses() {
        // 关键：别名只影响「从哪个键取值」，取到之后三道防线一视同仁。
        // 否则这次兼容就成了绕过凭据防线的后门。
        let w = work("alias_def");
        std::fs::write(w.join(".env"), "TOKEN=abc").unwrap();
        let env = ToolEnv::bare(&w);
        for k in ["path", "file_path", "filePath", "filename"] {
            let e = read_file_tool(&env, &json!({ k: ".env" }))
                .expect_err("凭据文件无论用哪个参数名都必须被拒");
            assert!(e.contains("凭据"), "用 {k} 时的拒绝原因不对：{e}");
            let e = read_file_tool(&env, &json!({ k: "../outside.txt" }))
                .expect_err("逃逸路径无论用哪个参数名都必须被拒");
            assert!(e.contains("相对路径"), "用 {k} 时的拒绝原因不对：{e}");
        }
        std::fs::remove_dir_all(&w).ok();
    }

    #[test]
    fn missing_param_error_lists_accepted_aliases() {
        // 万一模型发的是别名之外的第四种名字，提示里要给出可照抄的选项，
        // 而不是像真机日志那样让它重复 8 轮同一个错。
        let w = work("alias_msg");
        let env = ToolEnv::bare(&w);
        let e = read_file_tool(&env, &json!({ "nonsense_key": "a.rs" })).unwrap_err();
        assert!(e.contains("path"), "{e}");
        assert!(e.contains("file_path"), "提示应列出等价写法供模型照抄：{e}");
        std::fs::remove_dir_all(&w).ok();
    }

    #[test]
    fn numeric_params_accept_string_form_and_aliases() {
        // 模型偶发把整数发成字符串（"120"），为此报错纯属自找麻烦。
        let w = work("num_alias");
        let body: String = (1..=200).map(|i| format!("line{i}\n")).collect();
        std::fs::write(w.join("a.rs"), body).unwrap();
        let env = ToolEnv::bare(&w);
        let out = read_file_tool(
            &env,
            &json!({ "file_path": "a.rs", "startLine": "5", "endLine": 7 }),
        )
        .unwrap();
        assert!(out.contains("5\tline5") && out.contains("7\tline7"), "{out}");
        assert!(!out.contains("line4") && !out.contains("line8"), "区间应精确：{out}");
        std::fs::remove_dir_all(&w).ok();
    }

    #[tokio::test]
    async fn grep_and_list_dir_accept_aliases_too() {
        let w = work("gl_alias");
        std::fs::create_dir_all(w.join("src")).unwrap();
        std::fs::write(w.join("src/app.rs"), "let x = NEEDLE_Z;\n").unwrap();
        let env = ToolEnv::bare(&w);
        // grep：regex/query 是常见别名
        let out = grep_tool(&env, &json!({ "regex": "NEEDLE_Z" })).await.unwrap();
        assert!(out.contains("app.rs"), "grep 的 regex 别名应生效：{out}");
        // list_dir：file_path 当目录名、max_depth 当深度
        let out = list_dir_tool(&env, &json!({ "file_path": "src", "max_depth": 1 })).unwrap();
        assert!(out.contains("app.rs"), "list_dir 别名应生效：{out}");
        std::fs::remove_dir_all(&w).ok();
    }

    // ================= MCP images 参数 =================

    /// 1x1 的假图片字节。格式判定按扩展名（两家协议都按 media_type 走，不嗅探魔数），
    /// 故内容无需是真实 PNG。
    const FAKE: &[u8] = b"\x89PNG\r\n\x1a\nfake";

    #[test]
    fn load_images_maps_media_type_by_extension() {
        let w = work("img_ok");
        std::fs::write(w.join("a.PNG"), FAKE).unwrap();
        std::fs::write(w.join("b.jpg"), FAKE).unwrap();
        std::fs::write(w.join("c.jpeg"), FAKE).unwrap();
        std::fs::write(w.join("d.webp"), FAKE).unwrap();
        std::fs::write(w.join("e.gif"), FAKE).unwrap();
        let dir = w.to_string_lossy().to_string();
        let out = load_images(
            Some(&dir),
            &[
                "a.PNG".into(),
                "b.jpg".into(),
                "c.jpeg".into(),
                "d.webp".into(),
            ],
        )
        .unwrap();
        let types: Vec<&str> = out.iter().map(|i| i.media_type.as_str()).collect();
        // 扩展名大小写不敏感；jpg 与 jpeg 都映射到 image/jpeg
        assert_eq!(
            types,
            vec!["image/png", "image/jpeg", "image/jpeg", "image/webp"]
        );
        assert!(!out[0].base64.is_empty() && !out[0].base64.contains("data:"));
        std::fs::remove_dir_all(&w).ok();
    }

    #[test]
    fn load_images_rejects_over_limit_never_silently_drops() {
        let w = work("img_lim");
        let dir = w.to_string_lossy().to_string();
        for i in 0..MAX_IMAGES + 1 {
            std::fs::write(w.join(format!("{i}.png")), FAKE).unwrap();
        }
        let rels: Vec<String> = (0..MAX_IMAGES + 1).map(|i| format!("{i}.png")).collect();
        // 关键判据：超限**报错**而不是取前 4 张 —— 静默丢会让用户以为模型看了全部图。
        let e = load_images(Some(&dir), &rels).expect_err("超张数必须报错");
        assert!(e.contains(&MAX_IMAGES.to_string()), "{e}");
        std::fs::remove_dir_all(&w).ok();
    }

    #[test]
    fn load_images_rejects_oversized_and_unsupported_and_escaping() {
        let w = work("img_bad");
        let dir = w.to_string_lossy().to_string();

        // 超大：报出实际大小与上限，用户知道要压到多少
        std::fs::write(w.join("big.png"), vec![0u8; (MAX_IMAGE_BYTES + 1) as usize]).unwrap();
        let e = load_images(Some(&dir), &["big.png".into()]).unwrap_err();
        assert!(e.contains("MB") && e.contains("上限"), "{e}");

        // 不支持的格式：说清接受哪些
        std::fs::write(w.join("x.bmp"), FAKE).unwrap();
        let e = load_images(Some(&dir), &["x.bmp".into()]).unwrap_err();
        assert!(e.contains("格式不支持") && e.contains("webp"), "{e}");

        // 路径逃逸：图片路径同样过三道防线
        let e = load_images(Some(&dir), &["../../secret.png".into()]).unwrap_err();
        assert!(e.contains("相对路径"), "{e}");

        // 凭据类命名（.env.png 命中 NAME_PREFIXES）：图片入口也不例外
        std::fs::write(w.join(".env.png"), FAKE).unwrap();
        let e = load_images(Some(&dir), &[".env.png".into()]).unwrap_err();
        assert!(e.contains("凭据"), "{e}");

        std::fs::remove_dir_all(&w).ok();
    }

    #[test]
    fn load_images_requires_cwd_and_short_circuits_on_empty() {
        // 没图 → 直接空，不该因为缺 cwd 就报错（绝大多数调用不传图）
        assert!(load_images(None, &[]).unwrap().is_empty());
        // 传了图但没工作目录 → 明确告诉用户要一起传 cwd
        let e = load_images(None, &["a.png".into()]).unwrap_err();
        assert!(e.contains("cwd"), "{e}");
        let e = load_images(Some("   "), &["a.png".into()]).unwrap_err();
        assert!(e.contains("cwd"), "{e}");
    }
}
