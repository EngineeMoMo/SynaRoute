//! 文件知识检索模块 — 从用户配置的工作目录中检索与 prompt 相关的文件。
//!
//! 检索策略：
//! 1. 从 prompt 提取关键词
//! 2. grep 搜索工作目录中匹配的文件
//! 3. 如果目标项目有 .codegraph/ 索引，用 codegraph CLI 查询符号关系
//! 4. 合并去重，按相关度排序
//! 5. 按 max_tokens 限制裁剪内容

use crate::error::AppResult;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio::process::Command;

/// 检索到的单个文件
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetrievedFile {
    pub path: String,
    pub content: String,
    pub relevance: f32,
    pub source: String, // "grep" | "codegraph"
}

/// 估算 token 数（简单按字符数 / 4 估算，中文按字符数 / 2）
fn estimate_tokens(text: &str) -> u32 {
    let chars = text.chars().count();
    let cjk_count = text.chars().filter(|c| *c > '\u{4E00}' && *c < '\u{9FFF}').count();
    let ascii_count = chars - cjk_count;
    ((ascii_count as f64 / 4.0) + (cjk_count as f64 / 1.5)) as u32
}

/// 从 prompt 中提取关键词（简单分词 + 去停用词）
fn extract_keywords(prompt: &str) -> Vec<String> {
    let stop_words: &[&str] = &[
        "的", "了", "是", "在", "我", "有", "和", "就", "不", "人", "都", "一", "这", "中",
        "他", "会", "着", "没", "看", "好", "自", "也", "把", "那", "她", "你", "对", "说",
        "the", "a", "an", "is", "are", "was", "were", "be", "been", "being", "have", "has",
        "had", "do", "does", "did", "will", "would", "could", "should", "may", "might",
        "shall", "can", "need", "to", "of", "in", "for", "on", "with", "at", "by", "from",
        "it", "this", "that", "and", "or", "but", "if", "as", "not", "no", "so", "up",
        "请", "帮", "我要", "需要", "如何", "怎么", "什么", "哪个", "为什么",
    ];

    let mut keywords = Vec::new();
    // 按空白和常见标点分割
    for word in prompt.split(|c: char| c.is_whitespace() || c.is_ascii_punctuation() || matches!(c, '\u{3000}'..='\u{303F}' | '\u{FF01}'..='\u{FF5E}' | '\u{2000}'..='\u{206F}')) {
        let w = word.trim().to_lowercase();
        if w.len() < 2 {
            continue;
        }
        if stop_words.contains(&w.as_str()) {
            continue;
        }
        // 保留看起来像标识符的词（含下划线、驼峰等）
        if w.contains('_') || w.contains('.') || w.chars().any(|c| c.is_uppercase()) || w.len() >= 3 {
            keywords.push(word.trim().to_string());
        }
    }
    // 去重保序
    let mut seen = std::collections::HashSet::new();
    keywords.retain(|k| seen.insert(k.clone()));
    // 最多取 10 个关键词
    keywords.truncate(10);
    keywords
}

/// 用 ripgrep 搜索工作目录中包含关键词的文件。
///
/// 返回 `None` 表示 `rg` 未安装 / 无法启动（据此触发纯 Rust 遍历兜底）；
/// 返回 `Some(vec)`（可能为空）表示 rg 正常执行过——空只代表没命中，不需兜底。
async fn grep_search(
    work_dir: &Path,
    keywords: &[String],
    max_files: usize,
) -> Option<Vec<(PathBuf, f32)>> {
    let mut file_hits: HashMap<PathBuf, u32> = HashMap::new();
    let mut rg_ran = false;

    for kw in keywords {
        let output = Command::new("rg")
            .args([
                "--files-with-matches",
                "--no-messages",
                "--max-count", "1",
                "--max-depth", "5",
                "--glob", "!node_modules",
                "--glob", "!target",
                "--glob", "!dist",
                "--glob", "!.git",
                "--glob", "!*.lock",
                "--glob", "!*.min.js",
                kw,
            ])
            .current_dir(work_dir)
            .output()
            .await;

        match output {
            Ok(out) => {
                rg_ran = true;
                let stdout = String::from_utf8_lossy(&out.stdout);
                for line in stdout.lines() {
                    let path = work_dir.join(line.trim());
                    *file_hits.entry(path).or_insert(0) += 1;
                }
            }
            Err(_) => {
                // 无法启动 rg（未安装等）：立即放弃，交给兜底。
                return None;
            }
        }
    }

    if !rg_ran {
        return None;
    }

    // 按命中关键词数排序
    let total_kw = keywords.len().max(1) as f32;
    let mut results: Vec<(PathBuf, f32)> = file_hits
        .into_iter()
        .map(|(path, hits)| (path, hits as f32 / total_kw))
        .collect();
    results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    results.truncate(max_files);
    Some(results)
}

/// 纯 Rust 目录遍历兜底：当 `rg` 不可用时，手动递归工作目录、读取文本文件、
/// 按关键词命中数打分。为控成本：限制深度、跳过常见忽略目录、只看文本类扩展名、
/// 限制扫描文件总数上限。
fn walk_search(work_dir: &Path, keywords: &[String], max_files: usize) -> Vec<(PathBuf, f32)> {
    // 忽略目录（与 rg 的 glob 保持一致）
    const IGNORE_DIRS: &[&str] = &[
        "node_modules", "target", "dist", ".git", ".codegraph", "build",
        ".next", ".venv", "venv", "__pycache__", ".idea", ".vscode",
    ];
    // 只扫描这些扩展名的文本文件（避免读二进制/大资源）
    const TEXT_EXT: &[&str] = &[
        "rs", "ts", "tsx", "js", "jsx", "mjs", "cjs", "py", "go", "java", "kt",
        "c", "h", "cpp", "hpp", "cc", "cs", "rb", "php", "swift", "scala",
        "json", "toml", "yaml", "yml", "md", "txt", "html", "css", "scss",
        "vue", "svelte", "sql", "sh", "xml", "gradle", "properties",
    ];
    const MAX_SCAN_FILES: usize = 3000; // 扫描文件总数上限，防超大仓库拖垮
    const MAX_DEPTH: usize = 5;

    let lowered: Vec<String> = keywords.iter().map(|k| k.to_lowercase()).collect();
    let total_kw = keywords.len().max(1) as f32;

    let mut file_hits: Vec<(PathBuf, f32)> = Vec::new();
    let mut scanned = 0usize;

    // 迭代式 DFS（避免递归深栈）
    let mut stack: Vec<(PathBuf, usize)> = vec![(work_dir.to_path_buf(), 0)];
    while let Some((dir, depth)) = stack.pop() {
        if depth > MAX_DEPTH || scanned >= MAX_SCAN_FILES {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_dir() {
                let name = entry.file_name().to_string_lossy().to_string();
                if IGNORE_DIRS.contains(&name.as_str()) || name.starts_with('.') {
                    continue;
                }
                stack.push((path, depth + 1));
                continue;
            }
            if !ft.is_file() {
                continue;
            }
            // 扩展名过滤
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_lowercase())
                .unwrap_or_default();
            if !TEXT_EXT.contains(&ext.as_str()) {
                continue;
            }
            scanned += 1;
            if scanned >= MAX_SCAN_FILES {
                break;
            }
            // 只读前 256KB 判匹配，避免大文件
            let Ok(content) = read_head(&path, 256 * 1024) else {
                continue;
            };
            let lc = content.to_lowercase();
            let hits = lowered.iter().filter(|kw| lc.contains(kw.as_str())).count();
            if hits > 0 {
                file_hits.push((path, hits as f32 / total_kw));
            }
        }
    }

    file_hits.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    file_hits.truncate(max_files);
    file_hits
}

/// 读取文件前 N 字节并按 UTF-8 有效边界安全截断（避免中途切断多字节字符）。
fn read_head(path: &Path, max_bytes: usize) -> std::io::Result<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut buf = vec![0u8; max_bytes];
    let n = f.read(&mut buf)?;
    buf.truncate(n);
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// 尝试用 codegraph CLI 查询（如果目标项目有 .codegraph/ 索引）
async fn codegraph_search(work_dir: &Path, keywords: &[String], max_files: usize) -> Vec<(PathBuf, f32)> {
    let codegraph_dir = work_dir.join(".codegraph");
    if !codegraph_dir.exists() {
        return vec![];
    }

    let mut file_hits: HashMap<PathBuf, u32> = HashMap::new();

    for kw in keywords.iter().take(5) {
        // 尝试 codegraph search 命令
        let output = Command::new("codegraph")
            .args(["search", "--json", kw])
            .current_dir(work_dir)
            .output()
            .await;

        if let Ok(out) = output {
            if out.status.success() {
                let stdout = String::from_utf8_lossy(&out.stdout);
                // 解析 JSON 输出中的文件路径
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(&stdout) {
                    if let Some(arr) = val.as_array() {
                        for item in arr {
                            if let Some(file) = item.get("file").and_then(|f| f.as_str()) {
                                let path = work_dir.join(file);
                                *file_hits.entry(path).or_insert(0) += 1;
                            }
                        }
                    }
                }
            }
        }
    }

    let total_kw = keywords.len().max(1) as f32;
    let mut results: Vec<(PathBuf, f32)> = file_hits
        .into_iter()
        .map(|(path, hits)| (path, hits as f32 / total_kw))
        .collect();
    results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    results.truncate(max_files);
    results
}

/// 取字符串前 `max_chars` 个字符（按字符而非字节，避免切断多字节 UTF-8 导致 panic）。
fn take_chars(s: &str, max_chars: usize) -> String {
    s.chars().take(max_chars).collect()
}

/// 读取文件内容并裁剪到合理大小（按字符计，中文安全）。
fn read_file_capped(path: &Path, max_chars: usize) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    if content.chars().count() <= max_chars {
        Some(content)
    } else {
        Some(format!(
            "{}\n\n... [文件被截断，共 {} 字符]",
            take_chars(&content, max_chars),
            content.chars().count()
        ))
    }
}

/// 主入口：从工作目录检索与 prompt 相关的文件
pub async fn retrieve(
    work_dir: &str,
    prompt: &str,
    max_tokens: u32,
) -> AppResult<Vec<RetrievedFile>> {
    let work_path = Path::new(work_dir);
    if !work_path.exists() {
        return Ok(vec![]);
    }

    let keywords = extract_keywords(prompt);
    if keywords.is_empty() {
        return Ok(vec![]);
    }

    // 并行执行两种检索
    let (grep_opt, cg_results) = tokio::join!(
        grep_search(work_path, &keywords, 15),
        codegraph_search(work_path, &keywords, 10),
    );

    // grep_search 返回 None 表示 rg 未安装 → 用纯 Rust 遍历兜底，保证核心检索不因缺 rg 而失效。
    let grep_results = match grep_opt {
        Some(results) => results,
        None => walk_search(work_path, &keywords, 15),
    };

    // 合并去重（以路径为 key，取较高的相关度）
    let mut merged: HashMap<PathBuf, (f32, &str)> = HashMap::new();
    for (path, score) in &grep_results {
        merged.insert(path.clone(), (*score, "grep"));
    }
    for (path, score) in &cg_results {
        let entry = merged.entry(path.clone()).or_insert((0.0, "codegraph"));
        if *score > entry.0 {
            *entry = (*score, "codegraph");
        } else {
            // codegraph 补充分数
            entry.0 = (entry.0 + score * 0.5).min(1.0);
        }
    }

    // 按相关度排序
    let mut sorted: Vec<(PathBuf, f32, &str)> = merged
        .into_iter()
        .map(|(p, (s, src))| (p, s, src))
        .collect();
    sorted.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    // 按 token 限制读取文件内容
    let mut results = Vec::new();
    let mut total_tokens: u32 = 0;
    let per_file_cap = 8000; // 每个文件最多 8000 字符

    for (path, relevance, source) in sorted {
        if total_tokens >= max_tokens {
            break;
        }
        let Some(content) = read_file_capped(&path, per_file_cap) else {
            continue;
        };
        let tokens = estimate_tokens(&content);
        if total_tokens + tokens > max_tokens {
            // 裁剪最后一个文件以适配（按字符计，中文安全）
            let remaining = max_tokens - total_tokens;
            let chars_budget = (remaining as usize) * 3; // 粗略反推
            let trimmed = if content.chars().count() > chars_budget {
                format!("{}\n... [截断]", take_chars(&content, chars_budget))
            } else {
                content
            };
            let rel_path = path
                .strip_prefix(work_path)
                .unwrap_or(&path)
                .to_string_lossy()
                .to_string();
            results.push(RetrievedFile {
                path: rel_path,
                content: trimmed,
                relevance,
                source: source.to_string(),
            });
            break;
        }
        total_tokens += tokens;
        let rel_path = path
            .strip_prefix(work_path)
            .unwrap_or(&path)
            .to_string_lossy()
            .to_string();
        results.push(RetrievedFile {
            path: rel_path,
            content,
            relevance,
            source: source.to_string(),
        });
    }

    Ok(results)
}
