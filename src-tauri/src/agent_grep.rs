//! `grep` 工具的命中行筛选与体积上限。
//!
//! `#[path]` 挂在 [`super::agent_tools`] 下：那个文件棘轮冻结在 949、余量为 0，
//! 而这一族自洽 —— 只回答「一条 rg 命中该不该给模型、给多少」。
//!
//! # 两道上限，管的是不同维度
//!
//! - **行数**（[`MAX_MATCH_LINES`]）：早就有，防「一次检索把几千行灌进上下文」。
//! - **行宽**（[`MAX_MATCH_CHARS`]）：本轮补的。`--max-filesize 2M` 限制的是**输入文件**，
//!   而单行可以就是那 2 MB —— minified JS、单行 JSON、base64 内联资源都是这个形态。
//!   120 行 × 2 MB 会整段进模型上下文、进工具结果日志、进聚合的每一轮重发历史。
//!   而 `cap_result` 在**结果拼好之后**才截，挡不住峰值内存。
//!
//! # 🔴 筛选不是减噪，是防凭据外发
//!
//! rg **搜所有文件**，`.env` 只要命中就会出现在 stdout 里。故每条命中都要过
//! [`super::is_sensitive_path`] —— 这一层去掉的话，一次 `grep KEY` 就能把用户的密钥
//! 原样交给模型。判据拆成纯函数是为了**可验证**：本机不一定装 rg，而这条判据不该只在
//! 恰好装了 rg 的机器上才被测到。

use super::is_sensitive_read_path;
use std::path::Path;

/// 单次 `grep` 返回的匹配行上限。
pub(super) const MAX_MATCH_LINES: usize = 120;

/// 单行回显的字符上限。
pub(super) const MAX_MATCH_CHARS: usize = 400;

/// 按字符（不是字节）截短过宽的行。
///
/// **必须按字符边界**：切在多字节中间会产出乱码 —— 同 `sse_error` 的 `MSG_CAP` 那条教训。
/// 结尾的省略号是**必需的**：不标出来，模型会以为那一行就这么长，据此得出错误结论。
pub(super) fn cap_match_text(text: &str) -> String {
    if text.chars().count() <= MAX_MATCH_CHARS {
        return text.to_string();
    }
    text.chars().take(MAX_MATCH_CHARS).collect::<String>() + "…"
}

/// 从 rg 的原始 stdout 里筛出可回给模型的命中行。
/// 返回 (可用行, 被安全策略排除的行数, 是否因命中过多而截断)。
pub(super) fn filter_rg_hits(work_dir: &Path, stdout: &str) -> (Vec<String>, usize, bool) {
    let mut kept: Vec<String> = Vec::new();
    let mut denied = 0usize;
    let mut truncated = false;
    for line in stdout.lines() {
        let Some((path, num, text)) = split_rg_line(line) else {
            continue;
        };
        if is_sensitive_read_path(&work_dir.join(path)) {
            denied += 1;
            continue;
        }
        if kept.len() >= MAX_MATCH_LINES {
            truncated = true;
            break;
        }
        let shown = path.replace('\\', "/");
        kept.push(format!("{shown}:{num}: {}", cap_match_text(text.trim_end())));
    }
    (kept, denied, truncated)
}

/// 解析 rg 的 `路径:行号:内容`。行号段必须是纯数字，否则说明这行不是匹配行（如提示信息）。
pub(super) fn split_rg_line(line: &str) -> Option<(&str, &str, &str)> {
    let (path, rest) = line.split_once(':')?;
    let (num, text) = rest.split_once(':')?;
    if !num.is_empty() && num.chars().all(|c| c.is_ascii_digit()) {
        Some((path, num, text))
    } else {
        None
    }
}
