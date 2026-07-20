//! 最近工作目录检测
//!
//! 从各 AI 编程工具的会话历史中提取用户最近使用的项目路径。
//! - Claude CLI：`~/.claude/projects/<slug>/*.jsonl` 里的 `cwd` 字段
//! - Codex：`~/.codex/sessions/YYYY/MM/DD/*.jsonl` 首行 payload.cwd
//! - Claude 桌面端：`%APPDATA%/Claude/git-worktrees.json` 里登记的 worktree 项目路径
//!   （尽力而为——桌面端会话本体存于二进制 LevelDB，无法可靠解析；仅当用户用过
//!   worktree 功能时此文件才有路径。普通对话不落盘 cwd，检测不到属预期限制。）

use crate::error::AppResult;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentWorkdir {
    /// 绝对路径
    pub path: String,
    /// 来源工具："claude-cli" | "codex" | "claude-desktop"
    pub source: String,
    /// 最近使用时间（毫秒时间戳）
    pub last_used: i64,
}

/// 扫描本机的 Claude CLI 与 Codex 会话历史，返回按 last_used 降序排列的最近工作目录。
/// 同一路径若被多个工具用过，取最新一次。
pub fn scan() -> AppResult<Vec<RecentWorkdir>> {
    let mut seen: HashMap<String, RecentWorkdir> = HashMap::new();

    for w in scan_claude_cli() {
        upsert(&mut seen, w);
    }
    for w in scan_codex() {
        upsert(&mut seen, w);
    }
    for w in scan_claude_desktop() {
        upsert(&mut seen, w);
    }

    let mut list: Vec<RecentWorkdir> = seen.into_values().collect();
    list.sort_by_key(|b| std::cmp::Reverse(b.last_used));
    list.truncate(30);
    Ok(list)
}

fn upsert(map: &mut HashMap<String, RecentWorkdir>, w: RecentWorkdir) {
    match map.get_mut(&w.path) {
        Some(existing) if existing.last_used >= w.last_used => {}
        Some(existing) => *existing = w,
        None => {
            map.insert(w.path.clone(), w);
        }
    }
}

fn mtime_ms(path: &Path) -> i64 {
    path.metadata()
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ---- Claude CLI ----

fn claude_projects_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude").join("projects"))
}

fn scan_claude_cli() -> Vec<RecentWorkdir> {
    let Some(dir) = claude_projects_dir() else {
        return vec![];
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return vec![];
    };
    let mut out = vec![];
    for e in entries.flatten() {
        let p = e.path();
        if !p.is_dir() {
            continue;
        }
        // 该 slug 目录下的最新 jsonl 会话文件
        let Some(latest) = latest_jsonl_in_dir(&p) else {
            continue;
        };
        let last_used = mtime_ms(&latest);
        // 从 jsonl 里读 cwd（覆盖所有行，取最后一个含 cwd 的记录）
        if let Some(cwd) = extract_cwd_from_jsonl(&latest) {
            out.push(RecentWorkdir {
                path: cwd,
                source: "claude-cli".to_string(),
                last_used,
            });
        }
    }
    out
}

fn latest_jsonl_in_dir(dir: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut best: Option<(PathBuf, i64)> = None;
    for e in entries.flatten() {
        let p = e.path();
        if p.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }
        let m = mtime_ms(&p);
        if best.as_ref().map(|(_, bm)| m > *bm).unwrap_or(true) {
            best = Some((p, m));
        }
    }
    best.map(|(p, _)| p)
}

fn extract_cwd_from_jsonl(path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    // 从最后一行向前找，优先返回最近一次 cwd
    for line in content.lines().rev() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
            // Claude CLI: 顶层可能直接有 cwd
            if let Some(cwd) = v.get("cwd").and_then(|c| c.as_str()) {
                return Some(normalize_path(cwd));
            }
            // Codex: payload.cwd
            if let Some(cwd) = v
                .get("payload")
                .and_then(|p| p.get("cwd"))
                .and_then(|c| c.as_str())
            {
                return Some(normalize_path(cwd));
            }
        }
    }
    None
}

fn normalize_path(raw: &str) -> String {
    // 双反斜杠 → 单反斜杠；末尾去斜杠。但要保留 UNC 前缀 \\server\share
    // （否则 \\server\share 会被压成 \server\share，变成当前盘符根相对路径）。
    let trimmed = raw.trim_end_matches(['\\', '/']);
    if let Some(rest) = trimmed.strip_prefix("\\\\") {
        format!("\\\\{}", rest.replace("\\\\", "\\"))
    } else {
        trimmed.replace("\\\\", "\\")
    }
}

// ---- Claude 桌面端 ----
//
// Claude Desktop 不像 CLI/Codex 那样把会话 cwd 落成可解析的 JSONL——会话存在二进制
// LevelDB（IndexedDB）里，不可靠解析。唯一稳定可读的项目路径来源是
// `%APPDATA%/Claude/git-worktrees.json`：用户在 Desktop 里对某项目开 worktree 时，
// 会登记项目路径。有则用（作为补充信号），无则跳过——这是当前能做到的最诚实的接入。
// 时间戳用该文件的 mtime 近似（无法精确到每个项目最后活跃时刻）。

fn claude_desktop_worktrees_file() -> Option<PathBuf> {
    // %APPDATA% = Roaming
    dirs::config_dir().map(|c| c.join("Claude").join("git-worktrees.json"))
}

fn scan_claude_desktop() -> Vec<RecentWorkdir> {
    let Some(file) = claude_desktop_worktrees_file() else {
        return vec![];
    };
    if !file.exists() {
        return vec![];
    }
    let last_used = mtime_ms(&file);
    let Ok(content) = std::fs::read_to_string(&file) else {
        return vec![];
    };
    let Ok(val) = serde_json::from_str::<serde_json::Value>(&content) else {
        return vec![];
    };

    // 结构：{ "worktrees": { <id>: {...} }, "schemaVersion": 2 }
    // worktree 条目的具体形状未文档化——递归扫描任何看起来像绝对路径的字符串值，
    // 提取出来作为项目路径（宽松兼容不同 schema 版本）。
    let mut out = vec![];
    let mut seen = std::collections::HashSet::new();
    if let Some(wts) = val.get("worktrees") {
        collect_path_strings(wts, &mut |p| {
            let norm = normalize_path(&p);
            if looks_like_project_path(&norm) && seen.insert(norm.clone()) {
                out.push(RecentWorkdir {
                    path: norm,
                    source: "claude-desktop".to_string(),
                    last_used,
                });
            }
        });
    }
    out
}

/// 判断字符串是否像一个项目绝对路径（Windows 盘符 或 POSIX 绝对路径）。
fn looks_like_project_path(s: &str) -> bool {
    let bytes = s.as_bytes();
    // Windows: C:\... 或 C:/...
    let win = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/');
    // POSIX 绝对路径
    let posix = s.starts_with('/') && s.len() > 1;
    (win || posix) && Path::new(s).is_dir()
}

/// 递归遍历 JSON，对每个字符串值回调（用于从未知 schema 里捞路径）。
fn collect_path_strings(v: &serde_json::Value, f: &mut impl FnMut(String)) {
    match v {
        serde_json::Value::String(s) => f(s.clone()),
        serde_json::Value::Array(arr) => {
            for item in arr {
                collect_path_strings(item, f);
            }
        }
        serde_json::Value::Object(map) => {
            // key 本身也可能是路径（worktrees 常以路径为 key）
            for (k, val) in map {
                f(k.clone());
                collect_path_strings(val, f);
            }
        }
        _ => {}
    }
}

// ---- Codex ----

fn codex_sessions_dir() -> Option<PathBuf> {
    if let Ok(h) = std::env::var("CODEX_HOME") {
        return Some(PathBuf::from(h).join("sessions"));
    }
    dirs::home_dir().map(|h| h.join(".codex").join("sessions"))
}

fn scan_codex() -> Vec<RecentWorkdir> {
    let Some(root) = codex_sessions_dir() else {
        return vec![];
    };
    // 结构：YYYY/MM/DD/rollout-*.jsonl
    let mut files: Vec<PathBuf> = vec![];
    collect_jsonl_recursive(&root, &mut files, 4);

    // 按 mtime 排序，只取最近 50 个 session（性能上界）
    files.sort_by_key(|p| std::cmp::Reverse(mtime_ms(p)));
    files.truncate(50);

    let mut out = vec![];
    for f in files {
        let last_used = mtime_ms(&f);
        if let Some(cwd) = extract_cwd_from_jsonl(&f) {
            out.push(RecentWorkdir {
                path: cwd,
                source: "codex".to_string(),
                last_used,
            });
        }
    }
    out
}

fn collect_jsonl_recursive(dir: &Path, out: &mut Vec<PathBuf>, depth: u32) {
    if depth == 0 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_jsonl_recursive(&p, out, depth - 1);
        } else if p.extension().and_then(|s| s.to_str()) == Some("jsonl") {
            out.push(p);
        }
    }
}
