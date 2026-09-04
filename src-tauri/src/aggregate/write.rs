//! 决策者输出 → 落盘：解析 ```file:路径 块、逐条过防线、备份后原子写。
//!
//! # 为什么从 `aggregate.rs` 抽出来
//!
//! 那个文件棘轮余量为 0，而本轮要补的防线比原来那段 `parse_and_apply` 还多。更根本的
//! 理由是：**写是这套功能里唯一不可逆的动作**，它该有自己的模块头把防线一次讲清 ——
//! 原先那几道散在函数体的行间注释里，于是「读路径有凭据黑名单、写路径没有」这个不对称
//! 藏了很久没被发现。
//!
//! # 防线（[`judge`] 是唯一实现，预览与落盘共用它）
//!
//! 模型给的路径不可信：prompt 里混着检索来的项目文件内容，其中任何一行都可能是提示注入。
//! 六道，顺序有意义（便宜的字符串判定在前，要碰文件系统的在后）：
//!
//! 1. [`is_safe_relative_path`]：拒 `..`、绝对路径、盘符、UNC。**与读路径共享同一份**。
//! 2. [`is_safe_write_name`]（写路径专属）：拒 ADS 冒号、Windows 保留设备名、尾随点/空格。
//!    这三种**不逃逸目录**，但会让落点不是那个文件 —— 写 `NUL` 报成功而内容进虚空，
//!    写 `a.txt:hidden` 进的是备用数据流（`type a.txt` 看不到它）。
//!    刻意**不**并进第 1 道：读路径有一条测试（`defense_3_target_check_catches_windows_name_aliases`）
//!    专门钉住「`.env::$DATA` 必须被**按落点**那道拦住」，用来证明第二次判定不是冗余。
//!    把冒号提到第 1 道会让那条测试改为在第 1 道就红 —— 判据还在，但它守的东西没了。
//! 3. [`is_vcs_internal`]：`.git/` 等版本库内部一律拒。`.git/hooks/pre-commit` 是
//!    「提示注入 → 任意代码执行」的直路（git 自己会执行它），而它不含 `..`、不是绝对
//!    路径、也不是链接 —— 前两道全部放行。
//! 4. [`crate::retrieval::is_sensitive_path`]：凭据类（`.env` / 密钥 / 证书 / 密码库）拒写。
//!    **这一道补的是本模块最大的历史缺口**：同一个黑名单在读路径上有 9 个调用点
//!    （`agent_tools` / `retrieval`），写路径**一个都没有** —— 我们拒绝让模型看 `.env`，
//!    却允许它覆盖 `.env`。方向反了，而覆盖比读取更不可逆。
//! 5. [`check_no_link_escape`]：链接逃逸两条（穿透链接目录建子目录 / 目标自身是链接）。
//! 6. [`changed_since`]：目标在 Phase1 之后被改过就拒。用户在「看计划」与「点确认」之间
//!    自己编辑了那个文件，静默覆盖掉是数据丢失，而且他不会知道。
//!
//! # 落盘（[`write_one`]）
//!
//! 备份 → 同目录临时文件 → `rename`。三条都不是可选的：
//!
//! - **备份**：本仓对自己的 `config.toml` 都做 `.bak`，而这里改的是用户的源代码。
//!   语义是「上一次写入前的内容」，故每次都覆盖备份 —— 不做 `.bak` 那种「首写即锁」，
//!   那条纪律为的是保住「接入前」这个唯一时间点，这里用户要的是撤销刚才那一次。
//! - **同目录**临时文件：`%TEMP%` 跨卷会让 `rename` 失败，而原子性全靠它
//!   （同 `codex_sessions` 里那条）。
//! - **`rename`**：裸 `fs::write` 是「截断 + 写」，中途失败（盘满 / 杀软锁文件 / 进程被杀）
//!   会留下半截文件，而那是用户的源代码。
//!
//! # 预览与落盘必须走同一道门
//!
//! [`plan_changes`] 与 [`apply_changes`] 都只经 [`judge`] 取判定。两处各写一份的失效
//! 形态是「预览说会写、实际被拒」或反过来，而后者是用户已经点了确认之后才发生的。
//! 有源码级判据钉住这一点。

use crate::retrieval::is_sensitive_path;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// 覆盖前的原文备份后缀。落在**同目录**，用户一眼能找到（放集中目录他不会去翻）。
const BACKUP_SUFFIX: &str = ".synaroute.bak";

/// 原子写用的临时文件后缀。同目录 —— 跨卷 `rename` 会失败，而原子性全靠 rename。
const TEMP_SUFFIX: &str = ".synaroute.tmp";

/// 版本库内部目录：任一路径组件命中即拒写。
///
/// `.git/hooks/*` 会被 git 在 commit/checkout/merge 时**自动执行**，是提示注入通往任意
/// 代码执行的最短路径；`.git/config` 能改 `core.hooksPath` 把 hook 目录指到别处，
/// 等价于同一件事。其余几家（hg/svn/jj/bzr）同理，一并拒掉而不是只堵 git ——
/// 判据按「版本库内部状态，模型没有正当理由写它」而不是按某一家的实现。
const VCS_DIRS: &[&str] = &[".git", ".hg", ".svn", ".jj", ".bzr"];

/// Windows 保留设备名（不区分大小写，且**带扩展名也算**：`NUL.txt` 仍是设备）。
///
/// 写它们不报错、`fs::write` 返回 `Ok`，内容进虚空 —— 于是我们向用户报告「已写入
/// nul.txt」而磁盘上没有这个文件。失效方向是「谎报成功」，比报错糟。
const RESERVED_DEVICE_NAMES: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// 一条「打算怎么处置这个路径」的预览。用户在真正落盘**之前**看到它。
///
/// 为什么需要它：Phase1 让用户确认的是**计划文本**，而 Phase2 才生成完整文件内容。
/// 原实现里用户点一下「确认执行」就直接落盘 —— 他从未看到将写入的字节，也没有
/// 「将改动这 N 个文件」的清单。这是「用户确认的和实际执行的不是同一个东西」。
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PlannedChange {
    /// 模型给的相对路径（原样回显，**不做归一** —— 用户要能看出模型说的就是这个串）。
    pub path: String,
    /// 将写入的字节数。被拒时仍给出，便于判断模型是不是输出了个空块。
    pub bytes: usize,
    /// 该路径当前是否已存在（覆盖 vs 新建）。覆盖比新建危险，UI 应区分。
    pub exists: bool,
    /// 被拒原因。`Some` = 这一条**不会**被写，且落盘阶段会给出同一句话。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rejected: Option<String>,
}

/// `aggregate_execute`（Phase2a）的返回：决策者原文 + 落盘预览，**一个字节都没写**。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewReport {
    /// 决策者原始输出。Phase2b 要原样回传 —— 落盘时按它重新解析，
    /// 服务端不缓存（多轮并发下缓存必然串台）。
    pub content: String,
    /// 逐条预览。空数组 = 决策者没输出任何 `file:` 块（多半是它给了散文而非文件内容），
    /// 或本轮压根没有工作目录。
    pub changes: Vec<PlannedChange>,
    /// 将要写入哪个目录（回显给用户看，也由前端在 Phase2b 原样回传）。
    /// `None` = 无工作目录，本轮不会写任何文件。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub work_dir: Option<String>,
}

/// [`judge`] 的结论。`Deny` 里那句话会**原样**同时出现在预览与落盘结果里。
enum Verdict {
    Allow { full: PathBuf, exists: bool },
    Deny(String),
}

/// 判断 LLM 给出的相对路径是否安全（禁止逃逸 work_dir）。
/// 拒绝：绝对路径、盘符/UNC 前缀、根、任何 `..` 组件。
///
/// `pub(crate)`：`agent_tools` 的只读工具收到的路径同样来自模型输出，同样不可信，
/// 走同一道字符串级校验（两处各写一份必然漂移）。
pub(crate) fn is_safe_relative_path(path: &str) -> bool {
    use std::path::Component;

    if path.trim().is_empty() {
        return false;
    }

    // ---- 与平台无关的字符串级判定（必须在 `Path` 语义之前）----
    //
    // 为什么不能只靠 `Path::is_absolute()` / `Component::Prefix`：**它们是平台相关的**。
    // 同一个字符串在两个平台上被判成两回事（macOS CI 首跑实测）：
    //
    // | 输入 | Windows | Unix |
    // |---|---|---|
    // | `C:\Windows\win.ini` | 绝对路径 + Prefix → 拒 | 一个普通组件 → **放行** |
    // | `C:/Windows/x` | 绝对路径 + Prefix → 拒 | 组件 `C:`/`Windows`/`x` → **放行** |
    // | `\\server\share\x` | UNC Prefix → 拒 | 一个普通组件 → **放行** |
    //
    // 在 Unix 上这三种不构成逃逸（反斜杠是合法文件名字符，落点仍在工作目录内，
    // 且第 2 道 canonicalize 判定照样成立）。但本函数的**承诺**是「拒 `..`、绝对路径、
    // 盘符、UNC」（见 `agent_tools` 模块注释），承诺在某个平台上不成立就是缺陷——
    // 下一个人会按注释信任它。且同一份模型输出在两平台行为分叉，无谓地增加排障面。
    //
    // 故这三类一律按**字符串形状**拒掉，与运行平台无关。

    // 盘符前缀：`X:` 开头（`C:\x`、`c:/x`、甚至裸 `C:`）
    let b = path.as_bytes();
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        return false;
    }
    // 反斜杠开头：UNC（`\\server\share`）与盘内根（`\Windows`）
    if path.starts_with('\\') {
        return false;
    }
    // 反斜杠也当分隔符做 `..` 判定 —— Unix 的 `Path` 不这么看，
    // 于是 `a\..\..\b` 在 Unix 上是一个组件、逃不掉但也拒不掉。
    if path.split(['/', '\\']).any(|seg| seg == "..") {
        return false;
    }

    // ---- 平台原生判定（Windows 上仍是主力，Unix 上负责 `/` 开头与 `..` 组件）----
    let p = Path::new(path);
    if p.is_absolute() {
        return false;
    }
    for c in p.components() {
        match c {
            Component::ParentDir | Component::Prefix(_) | Component::RootDir => return false,
            _ => {}
        }
    }
    true
}

/// 写路径专属的**名字形态**收窄：落点必须就是那个文件本身。
///
/// 返回 `Err(原因)` 即拒。三类，都不逃逸工作目录，但都让「我写的」与「用户看到的」不是
/// 同一个东西 —— 也就是说危害不是越权，而是**谎报**：
///
/// 1. **ADS 冒号**（`a.txt:hidden`、`a.txt::$DATA`）：Windows 上冒号是备用数据流分隔符，
///    内容写进流里，`type a.txt` / 编辑器 / git 都看不到它，而我们报告「已写入 a.txt」。
///    Unix 上冒号是合法文件名字符、不构成流，但这里仍一律拒 —— 收窄在两个平台都安全
///    （代价是不支持一种少见的文件名），而放行会让同一份模型输出在两平台落点不同。
/// 2. **保留设备名**（`NUL`、`con.txt`、`LPT1`）：写它返回 `Ok` 而磁盘上什么都没有。
///    带扩展名也算 —— Win32 按第一段判设备。
/// 3. **尾随点 / 空格**（`a.txt `、`b.`）：Win32 静默剥掉它们，于是模型说要写 `a.txt `、
///    实际落到 `a.txt`。可被用来绕过一个按精确名字做的检查。
fn is_safe_write_name(path: &str) -> Result<(), String> {
    // 跳过 `.`：`./a.rs` 是第一道明确放行的形态，而 `.` 这一段以点结尾 ——
    // 不排掉它，下面「尾随点」那条会把一个合法路径拒掉。（`..` 已被第一道拦下。）
    for seg in path.split(['/', '\\']).filter(|s| !s.is_empty() && *s != ".") {
        if seg.contains(':') {
            return Err(format!(
                "路径段 `{seg}` 含冒号：Windows 上那是备用数据流（ADS）分隔符，\
                 内容会写进一个用编辑器和 git 都看不到的流里。已拒绝写入。"
            ));
        }
        if seg.ends_with(' ') || seg.ends_with('.') {
            return Err(format!(
                "路径段 `{seg}` 以点或空格结尾：Windows 会静默剥掉它们，\
                 实际落点与这个名字不一致。已拒绝写入。"
            ));
        }
        // 设备名按第一段判（`nul.txt` 仍是设备），大小写无关。
        let stem = seg.split('.').next().unwrap_or(seg).to_ascii_lowercase();
        if RESERVED_DEVICE_NAMES.contains(&stem.as_str()) {
            return Err(format!(
                "`{seg}` 是 Windows 保留设备名：写入会「成功」但内容进虚空，\
                 磁盘上不会出现这个文件。已拒绝写入。"
            ));
        }
    }
    Ok(())
}

/// 任一路径组件命中 [`VCS_DIRS`] 即为版本库内部，返回命中的那一段（进错误消息）。
///
/// 判**每一段**而非只判首段：`sub/project/.git/config` 同样是版本库内部，而 monorepo /
/// submodule 下这个形态很常见。
fn is_vcs_internal(path: &str) -> Option<String> {
    path.split(['/', '\\'])
        .map(|seg| seg.to_ascii_lowercase())
        .find(|lower| VCS_DIRS.contains(&lower.as_str()))
}

/// 规范化后判断 `candidate` 是否仍在 `work_root` 之下（解析符号链接后的真实落点）。
///
/// 任一侧 canonicalize 失败（权限/竞态）时**判为不安全**——宁可拒写一次让用户重试，
/// 也不在无法确认落点时冒险写盘。
///
/// `pub(crate)`：`agent_tools` 的只读工具用它做「解析链接后仍在工作目录内」这道判定。
/// 读路径不需要 [`check_no_link_escape`]（那道为「目标尚不存在的写入」设计），因为读的目标
/// 必然已存在，直接 canonicalize 目标本身即可同时暴露「链接目录」与「目标自身是链接」两种逃逸。
pub(crate) fn is_within_work_root(work_root: &Path, candidate: &Path) -> bool {
    match (work_root.canonicalize(), candidate.canonicalize()) {
        (Ok(root), Ok(c)) => c.starts_with(root),
        _ => false,
    }
}

/// 落盘前的链接逃逸检查。返回 `Err(原因)` 即拒绝写入。
///
/// 两条独立的逃逸路径，必须都堵：
///
/// 1. **穿透链接目录建新子目录**。只校验「父目录存在时的父目录」是不够的：`vendor/` 是指向
///    外部的目录链接时，`vendor/sub/x.txt` 的父目录 `vendor/sub` **不存在**，校验被跳过，
///    紧随其后的 `create_dir_all` 会沿链接把目录建到外面去。故这里向上找到**最近一个已存在的
///    祖先**再校验——链接必然在这个祖先或它之下的某一段里，canonicalize 它就能暴露真实落点。
///    （Windows 上 junction 普通权限即可创建，pnpm 建 `node_modules/*` 就在用，不是刻意构造。）
///
/// 2. **目标文件自身是符号链接**。`fs::write` 跟随链接写入其目标：仓库里带一个
///    `notes.md` → `~/.ssh/config` 的文件链接（git 能 checkout 链接），父目录校验完全通过，
///    却把链接目标整份覆盖。故对已存在的目标用 `symlink_metadata`（不跟随链接）判类型，
///    是链接就拒。
///
/// 判据都取自文件系统而非 LLM 给的字符串——[`is_safe_relative_path`] 那道只看字符串，
/// 看不见链接。
fn check_no_link_escape(work_root: &Path, full_path: &Path) -> Result<(), String> {
    // ① 最近的已存在祖先必须在 root 之内。
    let mut probe = full_path.parent();
    while let Some(dir) = probe {
        if dir.exists() {
            if !is_within_work_root(work_root, dir) {
                return Err("目标解析后落在工作目录之外（疑似链接目录逃逸），已拒绝写入".into());
            }
            break;
        }
        probe = dir.parent();
    }
    // 一路向上都不存在（work_dir 本身都没了）：无法确认落点，fail-closed。
    if probe.is_none() {
        return Err("工作目录不存在或无法解析，已拒绝写入".into());
    }

    // ② 目标本身不得是符号链接（用 symlink_metadata，它不跟随链接）。
    if let Ok(md) = std::fs::symlink_metadata(full_path) {
        if md.file_type().is_symlink() {
            return Err("目标是符号链接，写入会覆盖链接指向的文件，已拒绝写入".into());
        }
    }
    Ok(())
}

/// 判定我们自己的副文件后缀，一律拒写。
///
/// 🔴 **备份是这次改动对用户的核心承诺，而它过得了另外五道防线**：`a.rs.synaroute.bak`
/// 不含冒号、不是设备名、不在版本库内部、扩展名 `bak` 也不在凭据黑名单里。
///
/// 失效链很具体：第一轮落盘覆盖 `a.rs` 并把原文留在 `a.rs.synaroute.bak`；第二轮模型
/// （被注入、或自己判断失误）输出一个 `file:a.rs.synaroute.bak` 块，那份原文就被改掉了。
/// 更糟的是**同一轮**里同时输出 `a.rs` 与 `a.rs.synaroute.bak`：前者写入时备份原文，
/// 后者紧接着覆盖那份备份 —— 用户点「撤销」拿到的是模型写的东西，原文永久丢失。
///
/// `.synaroute.tmp` 一并拒：它是原子写的中间态，模型没有任何正当理由写它，
/// 而放行会让「目标恰好是某次写入的临时文件」这种竞态形态凭空出现。
fn is_our_side_file(rel: &str) -> Option<&'static str> {
    // 按**任意路径段**判而不只判整串结尾：`x.synaroute.bak/y.rs` 这种把备份当目录用的
    // 形态同样要拒（Windows 上文件名不能当目录，但 Unix 上那个名字可以是目录）。
    let segs: Vec<String> = rel
        .split(['/', '\\'])
        .map(|s| s.to_ascii_lowercase())
        .collect();
    [BACKUP_SUFFIX, TEMP_SUFFIX]
        .into_iter()
        .find(|suffix| segs.iter().any(|seg| seg.ends_with(*suffix)))
}

/// 目标在 `since_ms` 之后被改动过吗？`since_ms` 是 Phase1 开始的墙钟毫秒。
///
/// # 为什么需要这道
///
/// 两个 phase 之间用户是自由的：他看着计划，顺手在编辑器里改了同一个文件（很自然 ——
/// 计划正在讨论那个文件）。然后点「确认执行」，而决策者手里的是 Phase1 检索到的**旧内容**，
/// 输出的完整文件里不含他刚写的那几行 → 静默覆盖，且他不会知道自己丢了东西。
///
/// # 判据边界（写在这里免得日后被当成 bug 重查）
///
/// - **拿不到 mtime 一律放行**，不 fail-closed。这道守的是「误伤用户的编辑」，不是安全边界
///   （安全由上面五道负责）；某些文件系统 / 网络盘不给可靠 mtime，在那里 fail-closed 会让
///   整个功能不可用，代价不对称。
/// - mtime 的**粒度**在部分文件系统上是 1~2 秒，故用 `>` 而非 `>=` 并留 1 秒宽容：
///   Phase1 检索**自己**不写文件，所以宽容不会漏掉真实编辑，只会漏掉「确认前那一秒内的编辑」。
/// - `since_ms <= 0` 视为「调用方没给基准」→ 整道跳过（老前端 / 直接调 IPC）。
fn changed_since(full_path: &Path, since_ms: i64) -> bool {
    if since_ms <= 0 {
        return false;
    }
    let Ok(md) = std::fs::metadata(full_path) else {
        return false;
    };
    let Ok(mtime) = md.modified() else {
        return false;
    };
    let Ok(dur) = mtime.duration_since(std::time::UNIX_EPOCH) else {
        return false;
    };
    (dur.as_millis() as i64) > since_ms.saturating_add(1_000)
}

/// **六道防线的唯一实现。** 预览（[`plan_changes`]）与落盘（[`apply_changes`]）都只经它。
///
/// 顺序即成本顺序：三道纯字符串 → 一道字符串黑名单 → 一道碰文件系统 → 一道读 mtime。
fn judge(work_root: &Path, rel: &str, plan_started_ms: i64) -> Verdict {
    // ① 逃逸（与读路径共享）
    if !is_safe_relative_path(rel) {
        return Verdict::Deny("路径越界（含 .. / 绝对路径 / 盘符），已拒绝写入".into());
    }
    // ② 名字形态：落点必须就是这个文件
    if let Err(why) = is_safe_write_name(rel) {
        return Verdict::Deny(why);
    }
    // ③ 版本库内部
    if let Some(seg) = is_vcs_internal(rel) {
        return Verdict::Deny(format!(
            "`{seg}` 是版本库内部目录，一律拒写 —— 其中的 hooks 会被自动执行，\
             是提示注入通往任意代码执行的路径。若确实要改版本库配置，请手工操作。"
        ));
    }
    // ③′ 我们自己的副文件（备份 / 原子写临时文件）
    if let Some(suffix) = is_our_side_file(rel) {
        return Verdict::Deny(format!(
            "`{suffix}` 是 SynaRoute 自己的副文件后缀，一律拒写 —— \
             备份里存的是上一次覆盖前的原文，被改掉就再也撤销不回去了。"
        ));
    }
    let full = work_root.join(rel);
    // ④ 凭据类（与读路径共享同一份黑名单）
    if is_sensitive_path(&full) {
        return Verdict::Deny(
            "该文件可能含凭据（.env / 密钥 / 证书 / 密码库类），一律拒写 —— \
             同一份黑名单也拦着模型去读它。若确实要改，请手工操作。"
                .into(),
        );
    }
    // ⑤ 链接逃逸（文件系统层）
    if let Err(why) = check_no_link_escape(work_root, &full) {
        return Verdict::Deny(why);
    }
    // ⑥ 两个 phase 之间用户自己改过
    if changed_since(&full, plan_started_ms) {
        return Verdict::Deny(
            "该文件在你确认计划之后被改动过（可能是你自己或别的工具改的）。\
             写入会覆盖掉那些改动，已拒绝 —— 请重新生成一次计划。"
                .into(),
        );
    }
    let exists = full.exists();
    Verdict::Allow { full, exists }
}

/// 决策者输出里的一个 ```file:路径 块。
struct FileBlock {
    path: String,
    /// `None` = 围栏没闭合，这一块要按失败报出而不是写半截文件。
    content: Option<String>,
    /// 同一路径的**第二次及以后**出现。整块不处理（原因见 [`DUPLICATE_BLOCK`]）。
    duplicate: bool,
}

/// 解析输出中的 ```file:path\ncontent\n``` 块。
///
/// 围栏解析：用「起始围栏的反引号数量」匹配对应长度的闭合围栏（≥3 个反引号且仅由反引号
/// 构成），使文件内容里出现的三反引号不会提前截断；找不到闭合围栏则该块 `content = None`。
///
/// **同一路径出现多次时，只有第一块被处理**，其余标 `duplicate`（见 [`DUPLICATE_BLOCK`]）。
/// 去重放在这里而不是两个消费者里各写一份 —— 那正是本模块头「预览与落盘必须走同一道门」
/// 要防的形态。
fn parse_blocks(output: &str) -> Vec<FileBlock> {
    let mut out = Vec::new();
    let lines: Vec<&str> = output.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let fence_len = lines[i].chars().take_while(|&c| c == '`').count();
        let after_fence = &lines[i][fence_len..];
        if fence_len < 3 || !after_fence.starts_with("file:") {
            i += 1;
            continue;
        }
        let path = after_fence["file:".len()..].trim().to_string();
        i += 1;
        let start = i;
        let mut closed = false;
        while i < lines.len() {
            let l = lines[i].trim_end();
            if l.len() >= fence_len && !l.is_empty() && l.chars().all(|c| c == '`') {
                closed = true;
                break;
            }
            i += 1;
        }
        if !closed {
            // 不完整输出：报出来但不写（写会得到一个被截断的源文件）。
            // 扫描到此结束 —— 内层循环已把 i 推到末尾，后面没有内容了。
            out.push(FileBlock { path, content: None, duplicate: false });
            break;
        }
        out.push(FileBlock {
            path,
            content: Some(lines[start..i].join("\n")),
            duplicate: false,
        });
        i += 1; // 跳过闭合围栏
    }
    // 去重：按**归一后**的路径比（`./a.rs` 与 `a.rs` 是同一个文件，落点也相同）。
    let mut seen = std::collections::HashSet::new();
    for b in &mut out {
        b.duplicate = !seen.insert(b.path.replace('\\', "/").trim_start_matches("./").to_string());
    }
    out
}

/// Phase2a：解析 + 判定，**一个字节都不写**。给用户看的落盘预览。
pub(crate) fn plan_changes(work_dir: &str, output: &str, plan_started_ms: i64) -> Vec<PlannedChange> {
    let work_root = Path::new(work_dir);
    parse_blocks(output)
        .into_iter()
        .map(|b| {
            // 顺序：重复 → 未闭合 → 六道防线。「整块不处理」的结论最强，排最前。
            if b.duplicate {
                return PlannedChange {
                    path: b.path,
                    bytes: b.content.map(|c| c.len()).unwrap_or(0),
                    exists: false,
                    rejected: Some(DUPLICATE_BLOCK.into()),
                };
            }
            let Some(content) = b.content else {
                return PlannedChange {
                    path: b.path,
                    bytes: 0,
                    exists: false,
                    rejected: Some(UNCLOSED_FENCE.into()),
                };
            };
            let bytes = content.len();
            match judge(work_root, &b.path, plan_started_ms) {
                Verdict::Allow { exists, .. } => PlannedChange {
                    path: b.path,
                    bytes,
                    exists,
                    rejected: None,
                },
                Verdict::Deny(why) => PlannedChange {
                    path: b.path,
                    bytes,
                    exists: false,
                    rejected: Some(why),
                },
            }
        })
        .collect()
}

/// 围栏未闭合的拒绝原因。**一份**：预览与落盘必须给同一句话。
const UNCLOSED_FENCE: &str = "代码块未正确闭合，已跳过（避免写入截断文件）";

/// 同一路径的重复块的拒绝原因。**一份**，理由同 [`UNCLOSED_FENCE`]。
///
/// 🔴 **不去重会让「预览说会写、实际被拒」真的发生**：第一块写入后目标 mtime 变新，
/// 第二块走到 [`changed_since`] 就被判成「你在确认之后改过这个文件」—— 而改它的是我们
/// 自己，那句话把用户指向一个不存在的原因（本仓「指错方向的提示比没有提示更糟」）。
/// 而预览阶段一个字节都没写，两块都会显示成「将写入」。
const DUPLICATE_BLOCK: &str =
    "同一个文件在决策者输出里出现了多次，只采用第一个代码块（其余已跳过，避免同一轮里前后互相覆盖）";

/// Phase2b 的结果：逐条落盘结论 + 备份清单。
pub(crate) struct WriteReport {
    pub changes: Vec<AppliedChange>,
    /// 被覆盖前留下的备份文件（相对工作目录）。空 = 这一轮全是新建文件。
    pub backups: Vec<String>,
}

/// 给路径整体追加一个后缀（`a.rs` → `a.rs.synaroute.bak`）。
///
/// 刻意**不用** `set_extension` —— 那会把 `a.rs` 变成 `a.synaroute.bak`，用户看不出
/// 它是哪个文件的备份，而同目录下 `a.rs` 与 `a.toml` 的备份还会互相覆盖。
fn with_suffix(p: &Path, suffix: &str) -> PathBuf {
    let mut s = p.as_os_str().to_os_string();
    s.push(suffix);
    PathBuf::from(s)
}

/// 备份 → 同目录临时文件 → `rename`。返回备份文件路径（目标原本不存在时为 `None`）。
///
/// 🔴 **备份失败一律中止写入**（fail-closed）：备份是这次改动对用户的核心承诺，
/// 备份没成功还照写等于「无备份覆盖源代码」，而那正是本模块要消除的东西。
/// 磁盘满 / 权限不足 / 杀软锁文件都会走到这条，宁可让这一条失败并如实说明。
fn write_one(full: &Path, content: &str) -> Result<Option<PathBuf>, String> {
    if let Some(parent) = full.parent() {
        // 显式报错而不是 `let _ =`：建不了目录时后面的写入会失败在一句
        // 「系统找不到指定的路径」上，而真因是父目录没建起来。
        std::fs::create_dir_all(parent).map_err(|e| format!("创建父目录失败：{e}"))?;
    }
    let backup = if full.exists() {
        let bak = with_suffix(full, BACKUP_SUFFIX);
        std::fs::copy(full, &bak).map_err(|e| format!("备份原文件失败，已放弃写入：{e}"))?;
        Some(bak)
    } else {
        None
    };
    // 原子写。临时文件必须与目标**同目录** —— `%TEMP%` 跨卷时 `rename` 直接失败，
    // 而原子性全靠这一步（同 codex_sessions 那条）。
    let tmp = with_suffix(full, TEMP_SUFFIX);
    std::fs::write(&tmp, content).map_err(|e| format!("写入临时文件失败：{e}"))?;
    std::fs::rename(&tmp, full).map_err(|e| {
        // 不清理会在用户源码目录里留一个 `.synaroute.tmp` 垃圾文件。
        let _ = std::fs::remove_file(&tmp);
        format!("替换目标文件失败：{e}")
    })?;
    Ok(backup)
}

/// Phase2b：按决策者原文重新解析并**真正落盘**。判定复用 [`judge`]，与预览完全同口径。
///
/// 为什么重新解析而不是接收前端传回来的 changes：那份数据在前端手里待过一趟，
/// 拿它当写入依据等于让前端决定往哪写什么。原文（决策者输出）是唯一可信的输入。
pub(crate) fn apply_changes(work_dir: &str, output: &str, plan_started_ms: i64) -> WriteReport {
    let work_root = Path::new(work_dir);
    let mut changes = Vec::new();
    let mut backups = Vec::new();
    for b in parse_blocks(output) {
        // 与 `plan_changes` 逐字同序：重复 → 未闭合 → 六道防线。有测试钉住两处给同一句话。
        if b.duplicate {
            changes.push(AppliedChange {
                path: b.path,
                success: false,
                error: Some(DUPLICATE_BLOCK.into()),
            });
            continue;
        }
        let Some(content) = b.content else {
            changes.push(AppliedChange {
                path: b.path,
                success: false,
                error: Some(UNCLOSED_FENCE.into()),
            });
            continue;
        };
        let full = match judge(work_root, &b.path, plan_started_ms) {
            Verdict::Allow { full, .. } => full,
            Verdict::Deny(why) => {
                changes.push(AppliedChange {
                    path: b.path,
                    success: false,
                    error: Some(why),
                });
                continue;
            }
        };
        match write_one(&full, &content) {
            Ok(bak) => {
                if let Some(bak) = bak {
                    // 相对路径回显：绝对路径又长又含用户名，而用户要找的是「我项目里哪个文件」。
                    backups.push(
                        bak.strip_prefix(work_root)
                            .unwrap_or(&bak)
                            .to_string_lossy()
                            .replace('\\', "/"),
                    );
                }
                changes.push(AppliedChange {
                    path: b.path,
                    success: true,
                    error: None,
                });
            }
            Err(why) => changes.push(AppliedChange {
                path: b.path,
                success: false,
                error: Some(why),
            }),
        }
    }
    WriteReport { changes, backups }
}

/// 单个文件的修改结果
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppliedChange {
    pub path: String,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试用临时目录（同 `aggregate.rs` 测试段那份：pid + 自增序号，避免并发用例互踩）。
    /// 两份是同形的**测试基建**、不是判据 —— 本模块的判据只有 [`judge`] 一处。
    fn temp_dir(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "synaroute_write_test_{}_{}_{}",
            tag,
            std::process::id(),
            seq
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 落盘一次，只取逐条结论。`plan_started_ms = 0` 明确关掉 mtime 那道
    /// （它有自己的专项用例；这里传 0 才能测其余五道）。
    fn apply(work: &Path, output: &str) -> Vec<AppliedChange> {
        apply_changes(&work.to_string_lossy(), output, 0).changes
    }

    /// 一个块的拒绝原因（断言用）。
    fn why(c: &AppliedChange) -> &str {
        c.error.as_deref().unwrap_or("")
    }

    /// 🔴 同一路径出现多次：**只处理第一块**，且预览与落盘给同一句话。
    ///
    /// 不去重时这是一条真实的「预览说会写、实际被拒」——而模块头恰好承诺了不会发生：
    ///
    /// - 预览阶段一个字节都没写 → 两块都过 [`judge`] → 界面显示「将写入 2 个文件」；
    /// - 落盘阶段第一块写完，目标 mtime 变新 → 第二块撞上 [`changed_since`] → 拒，
    ///   而那句话说的是「你在确认之后改过这个文件」。改它的是**我们自己**，
    ///   于是用户拿到一句指向不存在原因的解释（本仓最忌讳的那类）。
    ///
    /// 另一半同样要钉：磁盘上留下的必须是**第一块**的内容。没有这条断言，「后写的覆盖先写的」
    /// 也能让上面几条通过，而那意味着用户在预览里读到的第一块内容不是最终落盘的东西。
    #[test]
    fn a_path_repeated_in_the_output_is_written_only_once() {
        let work = temp_dir("dup");
        let out = "```file:a.rs\nfirst\n```\n```file:a.rs\nsecond\n```\n";

        let planned = plan_changes(&work.to_string_lossy(), out, 0);
        assert_eq!(planned.len(), 2, "两块都要如实报出来，不能静默合并");
        assert!(planned[0].rejected.is_none(), "第一块应通过");
        assert_eq!(
            planned[1].rejected.as_deref(),
            Some(DUPLICATE_BLOCK),
            "第二块应在**预览阶段**就被标为重复（而不是等到落盘才撞 mtime 那道）"
        );

        let got = apply(&work, out);
        assert!(got[0].success, "第一块应写成功：{}", why(&got[0]));
        assert!(!got[1].success);
        assert_eq!(why(&got[1]), DUPLICATE_BLOCK, "落盘必须与预览给同一句话");
        assert_eq!(
            std::fs::read_to_string(work.join("a.rs")).unwrap(),
            "first",
            "留在磁盘上的必须是第一块 —— 用户在预览里核对的就是它"
        );
        std::fs::remove_dir_all(&work).ok();
    }

    /// 去重按**归一后**的路径比：`./a.rs` 与 `a.rs` 是同一个文件，落点也相同。
    ///
    /// 裸字符串比较会让这两种写法各写一次 —— 后者覆盖前者，而两条都报「成功」。
    #[test]
    fn duplicate_detection_normalises_the_path_first() {
        let work = temp_dir("dupnorm");
        let got = apply(&work, "```file:a.rs\nfirst\n```\n```file:./a.rs\nsecond\n```\n");
        assert!(got[0].success);
        assert_eq!(why(&got[1]), DUPLICATE_BLOCK, "`./a.rs` 与 `a.rs` 是同一个落点");
        assert_eq!(std::fs::read_to_string(work.join("a.rs")).unwrap(), "first");
        std::fs::remove_dir_all(&work).ok();
    }

    #[test]
    fn safe_relative_path_rejects_escapes() {
        // 允许：普通相对路径（含子目录）。
        for p in ["a.rs", "src/main.rs", "src/deep/nested/mod.rs", "./a.rs"] {
            assert!(is_safe_relative_path(p), "{p:?} 应被允许");
        }
        // 拒绝：父目录逃逸、绝对路径、盘符、UNC、根。
        for p in [
            "../secret.txt",
            "src/../../etc/passwd",
            "a/../../b",
            "/etc/passwd",
            "C:\\Windows\\System32\\drivers\\etc\\hosts",
            "C:/Windows/x",
            "\\\\server\\share\\x",
            "",
            "   ",
        ] {
            assert!(
                !is_safe_relative_path(p),
                "{p:?} 必须被拒绝（可写到工作目录之外）"
            );
        }
    }

    #[test]
    fn writes_files_and_creates_parent_dirs() {
        let dir = temp_dir("apply_ok");
        let output = "决策者说明文字（不应被当成文件）\n\
             ```file:src/a.rs\n\
             fn a() {}\n\
             ```\n\
             中间穿插的散文\n\
             ```file:deep/nested/b.txt\n\
             line1\n\
             line2\n\
             ```\n";

        let changes = apply(&dir, output);
        assert_eq!(changes.len(), 2, "应解析出两个文件块: {changes:?}");
        assert!(changes.iter().all(|c| c.success), "均应写入成功: {changes:?}");

        assert_eq!(
            std::fs::read_to_string(dir.join("src/a.rs")).unwrap(),
            "fn a() {}"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("deep/nested/b.txt")).unwrap(),
            "line1\nline2",
            "多级父目录应被自动创建"
        );
        // 新建的文件没有「覆盖前原文」，故不该产生备份。
        let rep = apply_changes(&dir.to_string_lossy(), "", 0);
        assert!(rep.backups.is_empty());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn refuses_paths_escaping_work_dir() {
        // 路径遏制的端到端验证：越界块必须**既不写盘、也不静默**（要报回 success:false）。
        let dir = temp_dir("apply_escape");
        let work = dir.join("proj");
        std::fs::create_dir_all(&work).unwrap();
        let outside = dir.join("pwned.txt");

        let output = format!(
            "```file:../pwned.txt\nHACKED\n```\n\
             ```file:{}\nHACKED\n```\n\
             ```file:ok.rs\nfine\n```\n",
            outside.display()
        );
        let changes = apply(&work, &output);

        let denied: Vec<&AppliedChange> = changes.iter().filter(|c| !c.success).collect();
        assert_eq!(denied.len(), 2, "两个越界块都应被拒: {changes:?}");
        assert!(
            denied.iter().all(|c| why(c).contains("路径越界")),
            "拒绝原因要说清是路径越界: {denied:?}"
        );
        assert!(
            !outside.exists(),
            "工作目录之外的文件绝不能被创建（这是提示注入的直接后果）"
        );
        // 合规块不受影响，仍照常写入。
        assert_eq!(std::fs::read_to_string(work.join("ok.rs")).unwrap(), "fine");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn keeps_inner_triple_backticks_intact() {
        // 围栏按「起始反引号数量」匹配：四反引号起始时，内容里的三反引号不得提前截断，
        // 否则写出的文件被砍半（Markdown/文档类文件必然中招）。
        let dir = temp_dir("apply_fence");
        let output = "````file:README.md\n\
             # Doc\n\
             ```rust\n\
             fn inner() {}\n\
             ```\n\
             tail\n\
             ````\n";

        let changes = apply(&dir, output);
        assert_eq!(changes.len(), 1);
        assert!(changes[0].success, "{changes:?}");
        let written = std::fs::read_to_string(dir.join("README.md")).unwrap();
        assert!(
            written.contains("```rust"),
            "内层三反引号应完整保留: {written:?}"
        );
        assert!(written.contains("fn inner() {}"));
        assert!(
            written.ends_with("tail"),
            "内容不得被内层围栏提前截断: {written:?}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn discards_unclosed_block_instead_of_writing_truncated_file() {
        // 输出被截断（客户端断连 / max_tokens 用尽）时，绝不能把半个文件写上去覆盖用户源码。
        let dir = temp_dir("apply_unclosed");
        let target = dir.join("src/a.rs");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, "ORIGINAL").unwrap();

        let output = "```file:src/a.rs\nfn half_written() {\n";
        let changes = apply(&dir, output);

        assert_eq!(changes.len(), 1);
        assert!(!changes[0].success, "未闭合块必须判失败");
        assert!(
            why(&changes[0]).contains("未正确闭合"),
            "原因要点明未闭合: {changes:?}"
        );
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "ORIGINAL",
            "原文件不得被截断内容覆盖"
        );
        // 也不该留下备份或临时文件 —— 那一条压根没进落盘阶段。
        assert!(!dir.join("src/a.rs.synaroute.bak").exists());
        assert!(!dir.join("src/a.rs.synaroute.tmp").exists());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn ignores_plain_code_blocks_without_file_prefix() {
        // 决策者常在答案里贴普通代码块举例（无 file: 前缀）——不得被误当成待写文件。
        let dir = temp_dir("apply_plain");
        let output = "示例：\n```rust\nfn demo() {}\n```\n以上仅为示例。\n";

        let changes = apply(&dir, output);
        assert!(changes.is_empty(), "普通代码块不该产生写入动作: {changes:?}");
        assert!(!dir.join("rust").exists());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn within_work_root_resolves_links_and_fails_closed() {
        let dir = temp_dir("within_root");
        let work = dir.join("proj");
        let inside = work.join("sub");
        std::fs::create_dir_all(&inside).unwrap();
        let outside = dir.join("outside");
        std::fs::create_dir_all(&outside).unwrap();

        assert!(is_within_work_root(&work, &work), "work_dir 自身在其下");
        assert!(is_within_work_root(&work, &inside), "真实子目录应通过");
        assert!(!is_within_work_root(&work, &outside), "同级目录不在其下");
        assert!(!is_within_work_root(&work, dir.as_path()), "父目录不在其下");
        // fail-closed：路径不存在 → canonicalize 失败 → 判不安全（宁可拒写让用户重试）。
        assert!(
            !is_within_work_root(&work, &work.join("does-not-exist")),
            "无法确认落点时必须判为不安全"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 建一个指向 `target` 的目录链接。Windows 先试 symlink（需特权），回退 junction；
    /// 其他平台用 unix symlink。返回是否建成。
    fn link_dir_for_test(target: &Path, link: &Path) -> bool {
        #[cfg(windows)]
        {
            if std::os::windows::fs::symlink_dir(target, link).is_ok() {
                return true;
            }
            // junction：普通权限可创建，canonicalize 同样解析。
            std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(link)
                .arg(target)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        }
        #[cfg(not(windows))]
        {
            std::os::unix::fs::symlink(target, link).is_ok()
        }
    }

    /// 建一个指向 `target` 文件的符号链接。Windows 需特权，建不成返回 false（测试跳过）。
    fn link_file_for_test(target: &Path, link: &Path) -> bool {
        #[cfg(windows)]
        {
            std::os::windows::fs::symlink_file(target, link).is_ok()
        }
        #[cfg(not(windows))]
        {
            std::os::unix::fs::symlink(target, link).is_ok()
        }
    }

    /// 符号链接逃逸：`link/` 指向工作目录之外时，`link/x` 这个相对路径不含 `..`、非绝对路径，
    /// **组件检查完全放行**，但实际写到了外面。canonicalize 二次校验就是为堵这个洞。
    ///
    /// Windows 上目录符号链接需要特权，故优先用 **junction**（`mklink /J`）——普通权限即可创建，
    /// 且 `canonicalize` 同样会解析它，能真实覆盖这条路径而不是跳过。两种都建不成才跳过。
    #[test]
    fn refuses_symlinked_dir_escaping_work_dir() {
        let dir = temp_dir("apply_symlink");
        let work = dir.join("proj");
        std::fs::create_dir_all(&work).unwrap();
        let outside = dir.join("outside");
        std::fs::create_dir_all(&outside).unwrap();

        let link = work.join("link");
        if !link_dir_for_test(&outside, &link) {
            eprintln!("跳过：当前环境无法创建目录链接（symlink 需特权、junction 也失败）");
            std::fs::remove_dir_all(&dir).ok();
            return;
        }

        // 前置确认：这条路径确实能过组件检查——否则本测试没在验证 canonicalize 那道防线。
        assert!(
            is_safe_relative_path("link/pwned.txt"),
            "该路径不含 ..、非绝对，组件检查必然放行；正是第二道防线的用途所在"
        );

        let changes = apply(&work, "```file:link/pwned.txt\nHACKED\n```\n");
        assert_eq!(changes.len(), 1);
        assert!(
            !changes[0].success,
            "经目录链接落到工作目录之外必须被拒: {changes:?}"
        );
        assert!(
            why(&changes[0]).contains("工作目录之外"),
            "原因要点明真实落点越界: {changes:?}"
        );
        assert!(
            !outside.join("pwned.txt").exists(),
            "链接目标目录里绝不能被写入"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 逃逸路径 ①：**多一级**路径穿透链接目录。
    ///
    /// 原实现只在「父目录已存在」时校验，而 `link/sub/x.txt` 的父目录 `link/sub` 不存在，
    /// 校验被短路跳过，紧随其后的 `create_dir_all` 沿链接把目录建到工作目录之外。
    /// 与 `refuses_symlinked_dir_escaping_work_dir` 的区别就是这一级之差 —— 那条恰好
    /// 命中「父目录存在」，所以旧实现能过；这条不能。
    #[test]
    fn refuses_new_subdir_through_linked_dir() {
        let dir = temp_dir("apply_symlink_deep");
        let work = dir.join("proj");
        std::fs::create_dir_all(&work).unwrap();
        let outside = dir.join("outside");
        std::fs::create_dir_all(&outside).unwrap();

        let link = work.join("vendor");
        if !link_dir_for_test(&outside, &link) {
            eprintln!("跳过：当前环境无法创建目录链接");
            std::fs::remove_dir_all(&dir).ok();
            return;
        }

        // 前置确认：组件检查放行，且父目录确实不存在（旧实现正是在此被短路）。
        assert!(is_safe_relative_path("vendor/sub/pwned.txt"));
        assert!(
            !work.join("vendor/sub").exists(),
            "父目录必须不存在，否则测不到「跳过校验」那条路径"
        );

        let changes = apply(&work, "```file:vendor/sub/pwned.txt\nHACKED\n```\n");
        assert_eq!(changes.len(), 1);
        assert!(
            !changes[0].success,
            "穿透链接目录建新子目录必须被拒: {changes:?}"
        );
        assert!(
            !outside.join("sub").exists(),
            "链接目标目录下绝不能被建出子目录"
        );
        assert!(!outside.join("sub/pwned.txt").exists(), "更不能写入内容");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 逃逸路径 ②：**目标文件自身**是符号链接。
    ///
    /// 父目录校验完全通过（就是 work_root），但 `fs::write` 跟随链接，把链接指向的
    /// 工作目录外文件整份覆盖。仓库里带一个这样的链接即可（git 能 checkout 符号链接）。
    #[test]
    fn refuses_writing_through_symlinked_file() {
        let dir = temp_dir("apply_symlink_file");
        let work = dir.join("proj");
        std::fs::create_dir_all(&work).unwrap();
        let secret = dir.join("secret.conf");
        std::fs::write(&secret, "ORIGINAL").unwrap();

        let link = work.join("notes.md");
        if !link_file_for_test(&secret, &link) {
            eprintln!("跳过：当前环境无法创建文件符号链接（Windows 需特权）");
            std::fs::remove_dir_all(&dir).ok();
            return;
        }

        let changes = apply(&work, "```file:notes.md\nHACKED\n```\n");
        assert_eq!(changes.len(), 1);
        assert!(!changes[0].success, "写入符号链接必须被拒: {changes:?}");
        assert!(
            why(&changes[0]).contains("符号链接"),
            "原因要点明是链接: {changes:?}"
        );
        assert_eq!(
            std::fs::read_to_string(&secret).unwrap(),
            "ORIGINAL",
            "链接指向的工作目录外文件绝不能被改写"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// work_dir 本身不存在时 fail-closed（不能因为「一路向上都没有已存在祖先」而放行）。
    #[test]
    fn refuses_when_work_dir_missing() {
        let dir = temp_dir("apply_no_workdir");
        let work = dir.join("nonexistent-proj");
        // 刻意不创建 work

        let changes = apply(&work, "```file:a.txt\nX\n```\n");
        assert_eq!(changes.len(), 1);
        assert!(!changes[0].success, "工作目录不存在时不得写盘: {changes:?}");
        assert!(!work.exists(), "更不该顺手把工作目录创建出来");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 正常路径不能被上面几道防线误伤：新建多级子目录应当照常成功。
    #[test]
    fn still_creates_nested_dirs_normally() {
        let dir = temp_dir("apply_nested_ok");
        let work = dir.join("proj");
        std::fs::create_dir_all(&work).unwrap();

        let changes = apply(&work, "```file:src/deep/nested/mod.rs\npub fn f() {}\n```\n");
        assert_eq!(changes.len(), 1);
        assert!(changes[0].success, "普通多级新建不得被误拒: {changes:?}");
        assert_eq!(
            std::fs::read_to_string(work.join("src/deep/nested/mod.rs")).unwrap(),
            "pub fn f() {}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    // ─── 本轮新增的四道防线 ──────────────────────────────────────────────────────

    /// 🔴 `.git/hooks/*` 会被 git 自动执行 —— 提示注入通往任意代码执行的最短路径。
    /// 而它不含 `..`、非绝对路径、也不是链接：**前两道防线全部放行**。
    #[test]
    fn refuses_writing_into_version_control_internals() {
        let dir = temp_dir("vcs");
        let work = dir.join("proj");
        std::fs::create_dir_all(work.join(".git/hooks")).unwrap();

        for p in [
            ".git/hooks/pre-commit",
            ".git/config",
            "sub/mod/.git/hooks/post-checkout",
            ".GIT/hooks/pre-push", // 大小写无关：Windows 上 .GIT 就是 .git
            ".hg/hgrc",
            ".svn/entries",
        ] {
            // 前置确认：这些路径能过前两道 —— 否则本用例没在验第三道。
            assert!(is_safe_relative_path(p), "{p} 应能过第一道");
            assert!(is_safe_write_name(p).is_ok(), "{p} 应能过第二道");

            let changes = apply(&work, &format!("```file:{p}\nHACKED\n```\n"));
            assert_eq!(changes.len(), 1, "{p}");
            assert!(!changes[0].success, "{p} 必须被拒: {changes:?}");
            assert!(
                why(&changes[0]).contains("版本库内部"),
                "{p} 的拒绝原因要说清是版本库内部: {changes:?}"
            );
        }
        assert!(
            !work.join(".git/hooks/pre-commit").exists(),
            "hook 文件绝不能被创建出来"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 🔴 读路径拒绝让模型**看** `.env`（`is_sensitive_path` 有 9 个调用点），
    /// 而写路径此前一个都没有 —— 也就是允许它**覆盖** `.env`。方向反了。
    #[test]
    fn refuses_overwriting_credential_files() {
        let dir = temp_dir("cred");
        let work = dir.join("proj");
        std::fs::create_dir_all(work.join("config")).unwrap();
        std::fs::write(work.join(".env"), "TOKEN=real").unwrap();

        for p in [".env", "config/.env.local", "server.pem", "secrets.json"] {
            assert!(is_safe_relative_path(p), "{p} 应能过第一道");
            assert!(is_vcs_internal(p).is_none(), "{p} 不该被第三道拦（那样就测不到第四道）");

            let changes = apply(&work, &format!("```file:{p}\nTOKEN=hacked\n```\n"));
            assert_eq!(changes.len(), 1, "{p}");
            assert!(!changes[0].success, "{p} 必须被拒: {changes:?}");
            assert!(why(&changes[0]).contains("凭据"), "{p}: {changes:?}");
        }
        assert_eq!(
            std::fs::read_to_string(work.join(".env")).unwrap(),
            "TOKEN=real",
            "既有凭据文件的内容一个字节都不能变"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 名字形态那道：三类都**不逃逸目录**，但都让落点不是那个文件（谎报成功）。
    #[test]
    fn refuses_names_whose_landing_spot_is_not_the_file() {
        for (p, needle) in [
            ("notes.md:hidden", "冒号"),
            ("a.txt::$DATA", "冒号"),
            ("src/x.rs:stream", "冒号"),
            ("NUL", "保留设备名"),
            ("nul.txt", "保留设备名"),
            ("logs/LPT1.log", "保留设备名"),
            ("con", "保留设备名"),
            ("a.txt ", "点或空格"),
            ("b.", "点或空格"),
            ("dir /x.rs", "点或空格"),
        ] {
            let e = is_safe_write_name(p).expect_err(&format!("{p:?} 必须被拒"));
            assert!(e.contains(needle), "{p:?} 的原因该提到「{needle}」：{e}");
        }
        // 正常名字不得被误伤 —— 这些里有点、有空格，但都不在结尾。
        for p in [
            "a.rs",
            "src/deep/mod.rs",
            "my file.txt",
            "v1.2.3/notes.md",
            "console.log.js",
            "auxiliary.rs",
            "./a.rs",
        ] {
            assert!(is_safe_write_name(p).is_ok(), "{p:?} 不该被拒");
        }
    }

    /// 🔴 `console.log.js` / `auxiliary.rs` 这类**以设备名开头**的正常文件名不能被误伤。
    ///
    /// 设备判定按「第一段」取 `split('.').next()`，故 `console` ≠ `con`、`auxiliary` ≠ `aux`
    /// —— 判的是整段相等，不是前缀。写成 `starts_with` 会把这两个真实存在的常见文件名拒掉，
    /// 而那种误伤在用户看来就是「SynaRoute 拒绝改我的 console.log.js，没说为什么合理」。
    #[test]
    fn device_name_check_matches_whole_segment_not_prefix() {
        for p in ["console.log.js", "auxiliary.rs", "prnt.txt", "nullable.ts", "com10.txt"] {
            assert!(
                is_safe_write_name(p).is_ok(),
                "{p:?} 只是以设备名开头，不是设备"
            );
        }
        // 反面：真的是设备（带任意扩展名都算）。
        for p in ["CON.txt", "aux", "com9.log"] {
            assert!(is_safe_write_name(p).is_err(), "{p:?} 是设备名");
        }
    }

    /// 覆盖已存在文件 → 必须留下 `.synaroute.bak`，且它的内容是**覆盖前的原文**。
    ///
    /// 本仓对自己的 `config.toml` 都做 `.bak`，而这里改的是用户的源代码 —— 原实现是裸
    /// `fs::write`，覆盖之后原内容再也拿不回来。
    #[test]
    fn backs_up_the_original_before_overwriting() {
        let dir = temp_dir("backup");
        let work = dir.join("proj");
        std::fs::create_dir_all(&work).unwrap();
        let target = work.join("src/a.rs");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, "ORIGINAL CONTENT").unwrap();

        let rep = apply_changes(
            &work.to_string_lossy(),
            "```file:src/a.rs\nNEW CONTENT\n```\n",
            0,
        );
        assert!(rep.changes[0].success, "{:?}", rep.changes);
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "NEW CONTENT",
            "新内容要落盘"
        );
        let bak = work.join("src/a.rs.synaroute.bak");
        assert!(bak.exists(), "必须留下备份");
        assert_eq!(
            std::fs::read_to_string(&bak).unwrap(),
            "ORIGINAL CONTENT",
            "备份里必须是覆盖前的原文"
        );
        // 备份路径要如实报给用户（否则他不知道去哪找），且用相对路径。
        assert_eq!(rep.backups, vec!["src/a.rs.synaroute.bak".to_string()]);
        // 原子写不得留下临时文件。
        assert!(
            !work.join("src/a.rs.synaroute.tmp").exists(),
            "临时文件必须已被 rename 掉"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 🔴 备份**失败**必须中止写入，而不是「照写」。
    ///
    /// 注入方式：把备份路径先占成一个**目录** → `fs::copy` 必然失败。此时原文件必须
    /// 一个字节都没变 —— 「备份失败还照写」等于无备份覆盖用户源码，正是本模块要消除的东西。
    #[test]
    fn a_failed_backup_aborts_the_write() {
        let dir = temp_dir("backup_fail");
        let work = dir.join("proj");
        std::fs::create_dir_all(&work).unwrap();
        let target = work.join("a.rs");
        std::fs::write(&target, "ORIGINAL").unwrap();
        // 让备份目标是个目录：copy 写不进去。
        std::fs::create_dir_all(work.join("a.rs.synaroute.bak")).unwrap();

        let changes = apply(&work, "```file:a.rs\nNEW\n```\n");
        assert!(!changes[0].success, "备份失败时必须判失败: {changes:?}");
        assert!(why(&changes[0]).contains("备份"), "{changes:?}");
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "ORIGINAL",
            "备份没成功就绝不能动原文件"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 🔴 **我们自己的备份文件必须拒写。** 它过得了另外五道防线，而它存的是「上一次覆盖前的
    /// 原文」—— 被模型改掉，用户点撤销拿到的就是模型写的东西，原文永久丢失。
    ///
    /// 最坏形态在同一轮里：先写 `a.rs`（把原文备份出去），紧接着写 `a.rs.synaroute.bak`
    /// （覆盖那份刚生成的备份）。两条都「成功」，而撤销路径已经没了。
    #[test]
    fn refuses_writing_our_own_side_files() {
        let dir = temp_dir("side_files");
        let work = dir.join("proj");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(work.join("a.rs"), "ORIGINAL").unwrap();

        // 先正常落一次盘，得到一份真备份。
        let first = apply(&work, "```file:a.rs\nV2\n```\n");
        assert!(first[0].success);
        let bak = work.join("a.rs.synaroute.bak");
        assert_eq!(std::fs::read_to_string(&bak).unwrap(), "ORIGINAL");

        // 再让模型来写那份备份 —— 必须被拒，且前四道都放行过它（否则测的不是这一道）。
        for p in [
            "a.rs.synaroute.bak",
            "a.rs.synaroute.tmp",
            "A.RS.SYNAROUTE.BAK",
            "sub/x.synaroute.bak/y.rs",
        ] {
            assert!(is_safe_relative_path(p), "{p} 应能过第一道");
            assert!(is_safe_write_name(p).is_ok(), "{p} 应能过第二道");
            assert!(is_vcs_internal(p).is_none(), "{p} 不该被第三道拦");

            let changes = apply(&work, &format!("```file:{p}\nHACKED\n```\n"));
            assert_eq!(changes.len(), 1, "{p}");
            assert!(!changes[0].success, "{p} 必须被拒: {changes:?}");
            assert!(why(&changes[0]).contains("副文件"), "{p}: {changes:?}");
        }
        assert_eq!(
            std::fs::read_to_string(&bak).unwrap(),
            "ORIGINAL",
            "备份里的原文一个字节都不能变 —— 它是撤销的唯一凭据"
        );

        // 同一轮里「写源文件 + 写它的备份」这个最坏形态：源文件照写，备份那条被拒。
        let both = apply(
            &work,
            "```file:a.rs\nV3\n```\n```file:a.rs.synaroute.bak\nHACKED\n```\n",
        );
        assert!(both[0].success, "正常文件不受影响: {both:?}");
        assert!(!both[1].success, "备份那条必须被拒: {both:?}");
        assert_eq!(
            std::fs::read_to_string(&bak).unwrap(),
            "V2",
            "第二次覆盖后备份应是上一版（V2），而不是被模型写成 HACKED"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// mtime 那道：用户在「看计划」与「点确认」之间自己改了同一个文件 → 拒绝覆盖。
    #[test]
    fn refuses_to_overwrite_a_file_the_user_edited_after_planning() {
        let dir = temp_dir("mtime");
        let work = dir.join("proj");
        std::fs::create_dir_all(&work).unwrap();
        let target = work.join("a.rs");
        std::fs::write(&target, "USER EDIT").unwrap();

        // Phase1 开始于「很久以前」，而文件是刚写的 → 判为「确认后被改过」。
        let long_ago = chrono::Utc::now().timestamp_millis() - 600_000;
        let changes = apply_changes(
            &work.to_string_lossy(),
            "```file:a.rs\nMODEL OUTPUT\n```\n",
            long_ago,
        )
        .changes;
        assert!(!changes[0].success, "应拒绝: {changes:?}");
        assert!(why(&changes[0]).contains("确认计划之后被改动过"), "{changes:?}");
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "USER EDIT",
            "用户自己写的内容绝不能被静默覆盖"
        );

        // 反面（同一份夹具）：Phase1 开始于「刚刚」→ 文件不比它新 → 照常写入。
        // 没有这一半，把 changed_since 改成恒 true 也不会红。
        let just_now = chrono::Utc::now().timestamp_millis() + 5_000;
        let ok = apply_changes(
            &work.to_string_lossy(),
            "```file:a.rs\nMODEL OUTPUT\n```\n",
            just_now,
        )
        .changes;
        assert!(ok[0].success, "未被改动过的文件应照常写入: {ok:?}");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// `since_ms <= 0` = 调用方没给基准（老前端 / 直接调 IPC）→ 整道跳过，不误伤。
    #[test]
    fn mtime_guard_is_skipped_without_a_baseline() {
        let dir = temp_dir("mtime_zero");
        let work = dir.join("proj");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(work.join("a.rs"), "OLD").unwrap();

        let changes = apply(&work, "```file:a.rs\nNEW\n```\n");
        assert!(changes[0].success, "没有基准时不该拒: {changes:?}");
        assert_eq!(std::fs::read_to_string(work.join("a.rs")).unwrap(), "NEW");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 🔴 **预览说会写的，落盘就必须真写；预览说被拒的，落盘就必须给同一句话。**
    ///
    /// 用户是看了预览才点的第二次确认。两边判据分叉的失效方向是「预览说会写、实际被拒」
    /// （用户以为改了，其实没改）或反过来（用户以为不会动，结果动了）—— 后者更坏。
    #[test]
    fn the_preview_and_the_write_agree_on_every_block() {
        let dir = temp_dir("agree");
        let work = dir.join("proj");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(work.join("existing.rs"), "OLD").unwrap();
        std::fs::write(work.join(".env"), "TOKEN=real").unwrap();

        // 一份混了各类形态的输出：合规新建 / 合规覆盖 / 凭据 / 版本库 / 越界 / ADS / 未闭合
        let output = "```file:new.rs\nA\n```\n\
             ```file:existing.rs\nB\n```\n\
             ```file:.env\nC\n```\n\
             ```file:.git/hooks/pre-commit\nD\n```\n\
             ```file:../out.txt\nE\n```\n\
             ```file:x.rs:ads\nF\n```\n\
             ```file:trunc.rs\nG\n";

        let w = work.to_string_lossy().to_string();
        let preview = plan_changes(&w, output, 0);
        let applied = apply_changes(&w, output, 0).changes;

        assert_eq!(preview.len(), applied.len(), "块数必须一致");
        assert_eq!(preview.len(), 7, "夹具应覆盖 7 种形态: {preview:?}");
        for (p, a) in preview.iter().zip(applied.iter()) {
            assert_eq!(p.path, a.path, "顺序与路径必须一一对应");
            assert_eq!(
                p.rejected.is_none(),
                a.success,
                "「预览放行」与「落盘成功」必须同进同退：{p:?} vs {a:?}"
            );
            if let Some(ref why_p) = p.rejected {
                assert_eq!(
                    Some(why_p.as_str()),
                    a.error.as_deref(),
                    "拒绝原因必须逐字一致（用户在预览里读到的就是这句）"
                );
            }
        }
        // 预览还要把「覆盖 vs 新建」如实分开 —— 覆盖比新建危险，UI 要能区分。
        let existing = preview.iter().find(|c| c.path == "existing.rs").unwrap();
        assert!(existing.exists, "已存在的文件要标成覆盖: {existing:?}");
        let created = preview.iter().find(|c| c.path == "new.rs").unwrap();
        assert!(!created.exists, "新建的不该标成覆盖: {created:?}");
        assert_eq!(created.bytes, 1, "字节数要如实给（内容是 \"A\"）");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 源码级：**判定只能有一处**，且预览与落盘都必须经过它。
    ///
    /// 上面那条行为用例在「两边都错得一样」时会绿（比如有人把 `judge` 复制成两份、
    /// 又同时改坏两份）。这条钉住结构：`judge` 的定义 1 处、调用恰好 2 处
    /// （`plan_changes` 与 `apply_changes` 各一次），且落盘只能经 `write_one`。
    #[test]
    fn both_the_preview_and_the_write_must_go_through_judge() {
        let src = crate::proxy::custom_headers::production_code_only(include_str!("write.rs"));
        assert_eq!(
            src.matches("fn judge(").count(),
            1,
            "judge 只能有一处定义 —— 复制一份出来，两边就会各自漂"
        );
        assert_eq!(
            src.matches("judge(work_root,").count(),
            2,
            "恰好两个调用点：plan_changes 与 apply_changes。少一个就是有一条路径绕过了防线"
        );
        assert_eq!(
            src.matches("write_one(").count(),
            2,
            "write_one 的定义 1 处 + 调用 1 处；多出来的调用点意味着有一条不过 judge 的写路径"
        );
        // 生产段里不许再出现裸 `fs::write` 到目标路径（原实现就是它：非原子、不备份）。
        // write_one 内部那一次写的是**临时文件**，故判据按「写的是 &tmp」放行。
        for line in src.lines() {
            if line.contains("fs::write(") {
                assert!(
                    line.contains("&tmp"),
                    "落盘只能写临时文件再 rename，这一行是裸写：{line}"
                );
            }
        }
    }
}
