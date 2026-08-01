//! 文件知识检索模块 — 从用户配置的工作目录中检索与 prompt 相关的文件。
//!
//! 检索策略：
//! 1. 从 prompt 提取关键词
//! 2. grep 搜索工作目录中匹配的文件
//! 3. 如果目标项目有 .codegraph/ 索引，用 codegraph CLI 查询符号关系
//! 4. 合并去重，按相关度排序
//! 5. 按 max_tokens 限制裁剪内容

use crate::error::AppResult;
use crate::proc::hidden;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

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
        // 走 proc::hidden 而非 Command::new：Windows 上 rg 是控制台程序，
        // 直接起会闪一个黑窗口（见 crate::proc 模块注释）。
        let output = hidden("rg")
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

/// 敏感文件排除 —— **恒定生效，无开关**。
///
/// 检索结果会被拼进 prompt 发给每个聚合成员（多个第三方上游），故凭据类文件一旦命中关键词就会
/// 外发。旧实现零过滤：`walk_search` 的扩展名白名单含 `json/yaml/yml/toml/properties`，而 `rg`
/// 那条路**连白名单都没有**（搜所有文件），所以 `.env`、`credentials.json`、`id_rsa` 只要命中
/// 就会被读出来发走。原有的 `node_modules`/`target`/`dist` 排除是为了减噪，不是安全控制。
///
/// 刻意不做成开关：可关闭的安全控制等于没有。被排除的文件记进日志，保持可见而非静默。
///
/// 分两级是为了**不误伤源码**——`TokenService.java`、`secret.rs`、`credentials.md` 都得留着：
/// - Tier A：精确文件名 / 扩展名，任何位置命中即拒
/// - Tier B：仅当扩展名属于配置/数据类时，才按文件名子串拒
mod deny {
    /// Tier A-1：精确文件名（小写比较）。
    pub const EXACT_NAMES: &[&str] = &[
        ".env",
        ".npmrc",
        ".pypirc",
        ".netrc",
        "_netrc",
        ".htpasswd",
        ".git-credentials",
        ".dockercfg",
        "credentials",
        "credentials.json",
        "secrets.json",
        "id_rsa",
        "id_dsa",
        "id_ecdsa",
        "id_ed25519",
    ];

    /// Tier A-2：文件名前缀（覆盖 `.env.local` / `.env.production` / `id_rsa.pub` 等变体）。
    pub const NAME_PREFIXES: &[&str] = &[".env.", "id_rsa", "id_dsa", "id_ecdsa", "id_ed25519"];

    /// Tier A-3：扩展名（密钥/证书/密码库容器）。
    pub const EXTENSIONS: &[&str] = &[
        "pem", "key", "p12", "pfx", "jks", "keystore", "ppk", "kdbx", "asc", "gpg", "crt", "cer",
        "der",
    ];

    /// Tier B 适用的扩展名（配置/数据类；源码扩展名刻意不在此列）。
    pub const CONFIGISH_EXTS: &[&str] = &[
        "json", "yaml", "yml", "toml", "properties", "ini", "cfg", "conf", "env", "xml", "plist",
    ];

    /// Tier B：文件名子串（仅对 CONFIGISH_EXTS 生效）。
    pub const CONFIGISH_SUBSTRINGS: &[&str] = &[
        "secret",
        "credential",
        "password",
        "passwd",
        "apikey",
        "api_key",
        "api-key",
        "token",
        "private",
    ];
}

/// 该文件是否因可能含凭据而**禁止**被检索/外发。见 [`deny`] 的分级理由。
pub fn is_sensitive_path(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_lowercase();
    if name.is_empty() {
        return false;
    }
    if deny::EXACT_NAMES.contains(&name.as_str()) {
        return true;
    }
    if deny::NAME_PREFIXES.iter().any(|p| name.starts_with(p)) {
        return true;
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .unwrap_or_default();
    if !ext.is_empty() && deny::EXTENSIONS.contains(&ext.as_str()) {
        return true;
    }
    // Tier B：只有配置/数据类扩展名才按名字子串判定，避免误伤 TokenService.java / secret.rs
    if deny::CONFIGISH_EXTS.contains(&ext.as_str()) {
        // 去掉扩展名再比，避免 `.env` 这类扩展名本身触发子串
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_lowercase();
        if deny::CONFIGISH_SUBSTRINGS.iter().any(|s| stem.contains(s)) {
            return true;
        }
    }
    false
}

/// 从候选列表里剔除敏感文件，返回 (保留, 被拒的相对路径列表)。
fn filter_sensitive(
    items: Vec<(PathBuf, f32)>,
    work_dir: &Path,
) -> (Vec<(PathBuf, f32)>, Vec<String>) {
    let mut kept = Vec::with_capacity(items.len());
    let mut denied = Vec::new();
    for (p, s) in items {
        if is_sensitive_path(&p) {
            denied.push(
                p.strip_prefix(work_dir)
                    .unwrap_or(&p)
                    .to_string_lossy()
                    .to_string(),
            );
        } else {
            kept.push((p, s));
        }
    }
    (kept, denied)
}

/// 用 codegraph 做**符号级**检索：`query` 找种子符号 → `impact` 沿调用边扩链路。
///
/// 与 grep 路径的本质差别：拿到的是**符号 + 精确行区间**（`startLine`/`endLine`），据此只切出
/// 方法体，而不是把整份文件截 8000 字符发走。一条 5 节点的链约 4k 字符，对代码审查更有用
/// （无无关噪音），也不必把大项目"打包传输"。
///
/// 任何一步失败都记进 `diag` 供调用方落日志——**绝不静默返回空**。旧实现调了不存在的
/// `search` 子命令、又只看退出码（codegraph 恒为 0），导致集成从未生效却看起来像"没命中"。
async fn codegraph_symbols(
    program: &str,
    work_dir: &Path,
    keywords: &[String],
    depth: u32,
    diag: &mut Vec<String>,
) -> Vec<crate::codegraph::SymbolNode> {
    let mut seeds: Vec<crate::codegraph::SymbolNode> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    // 1) 种子：每个关键词查一次（只用前 5 个，控制调用次数）
    for kw in keywords.iter().take(5) {
        match crate::codegraph::query_symbols(program, work_dir, kw, 5).await {
            Ok(hits) => {
                for n in hits {
                    let key = format!("{}:{}", n.file_path, n.start_line);
                    if seen.insert(key) {
                        seeds.push(n);
                    }
                }
            }
            Err(e) => diag.push(format!("codegraph query \"{kw}\" 失败: {e}")),
        }
    }
    if seeds.is_empty() {
        return seeds;
    }

    // 2) 链路：对前 3 个种子取影响面（跨文件调用边），把邻接符号并入
    //    只用前 3 个是为了控 CLI 调用次数——impact 的 depth 已能覆盖多跳。
    let mut chain: Vec<crate::codegraph::SymbolNode> = Vec::new();
    for seed in seeds.iter().take(3) {
        match crate::codegraph::impact(program, work_dir, &seed.name, depth).await {
            Ok(rel) => {
                for r in rel {
                    let Some(line) = r.start_line else { continue };
                    let key = format!("{}:{}", r.file_path, line);
                    if !seen.insert(key) {
                        continue;
                    }
                    // impact 的节点没有 endLine，用 None 表示"边界未知"，
                    // 切片时退化为「从 startLine 起取有限行数」。
                    chain.push(crate::codegraph::SymbolNode {
                        kind: r.kind.unwrap_or_else(|| "symbol".into()),
                        name: r.name,
                        qualified_name: None,
                        file_path: r.file_path,
                        language: None,
                        start_line: line,
                        end_line: None,
                        signature: None,
                        return_type: None,
                        visibility: None,
                        docstring: None,
                    });
                }
            }
            Err(e) => diag.push(format!("codegraph impact \"{}\" 失败: {e}", seed.name)),
        }
    }
    seeds.extend(chain);
    seeds
}

/// 按行区间从文件里切出一个符号的源码。`end_line` 为 None 时取 `MAX_UNKNOWN_LINES` 行兜底。
///
/// 行号是 1-based 且**含端点**（codegraph 的约定，实机验证）。单个符号上限
/// `MAX_SYMBOL_LINES` 行——防 God method 一口吃掉整个预算。
fn slice_symbol(work_dir: &Path, node: &crate::codegraph::SymbolNode) -> Option<String> {
    const MAX_SYMBOL_LINES: usize = 150;
    const MAX_UNKNOWN_LINES: usize = 40;

    let abs = work_dir.join(&node.file_path);
    if is_sensitive_path(&abs) {
        return None;
    }
    let content = std::fs::read_to_string(&abs).ok()?;
    let lines: Vec<&str> = content.lines().collect();
    let start = (node.start_line as usize).saturating_sub(1);
    if start >= lines.len() {
        return None;
    }
    let end_exclusive = match node.end_line {
        Some(e) => (e as usize).min(lines.len()),
        None => (start + MAX_UNKNOWN_LINES).min(lines.len()),
    };
    if end_exclusive <= start {
        return None;
    }
    let span = end_exclusive - start;
    let take = span.min(MAX_SYMBOL_LINES);
    let mut body = lines[start..start + take].join("\n");
    if take < span {
        body.push_str(&format!("\n… [符号过长，已截断，共 {span} 行]"));
    }
    Some(body)
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

/// 主入口：从工作目录检索与 prompt 相关的代码上下文。
///
/// 两条路径，**符号级优先**：
/// 1. 装了 codegraph 且项目已索引 → `query` 找种子 + `impact` 扩调用链，按精确行区间切符号体。
///    这是"不把项目打包发走"的实现：只发链路上的方法体（一条 5 节点链约 4k 字符），
///    而非文件全文；对代码审查也更有用（无无关噪音）。
/// 2. 否则 → 原有 grep / 纯 Rust 遍历，按文件给内容（每文件截 8000 字符）。
///
/// 敏感文件排除（[`is_sensitive_path`]）在两条路径上都恒定生效。
/// 所有降级与失败原因都收进返回值的 `diagnostics`，由调用方落日志——不静默。
pub async fn retrieve(
    work_dir: &str,
    prompt: &str,
    max_tokens: u32,
) -> AppResult<Vec<RetrievedFile>> {
    Ok(retrieve_detailed(work_dir, prompt, max_tokens).await.files)
}

/// 检索结果 + 诊断信息（供日志展示走了哪条路、为什么降级、排除了哪些文件）。
pub struct RetrieveOutcome {
    pub files: Vec<RetrievedFile>,
    /// 人类可读的一行摘要，如 `codegraph 符号级 · 12 符号` / `grep 文件级 · codegraph 未索引`。
    pub summary: String,
    /// 逐条诊断（失败原因、被排除的敏感文件等）。
    pub diagnostics: Vec<String>,
}

pub async fn retrieve_detailed(work_dir: &str, prompt: &str, max_tokens: u32) -> RetrieveOutcome {
    let mut diag: Vec<String> = Vec::new();
    let work_path = Path::new(work_dir);
    if !work_path.exists() {
        return RetrieveOutcome {
            files: vec![],
            summary: format!("工作目录不存在: {work_dir}"),
            diagnostics: diag,
        };
    }

    let keywords = extract_keywords(prompt);
    if keywords.is_empty() {
        return RetrieveOutcome {
            files: vec![],
            summary: "prompt 未提取出关键词，跳过检索".into(),
            diagnostics: diag,
        };
    }

    // ---- 路径 1：codegraph 符号级 ----
    let indexed = work_path.join(".codegraph").is_dir();
    let resolved = crate::codegraph::resolve().await;
    match (&resolved, indexed) {
        (Some(r), true) => {
            if !r.on_path {
                diag.push(format!(
                    "codegraph 不在 PATH 中，改用绝对路径调用: {}（建议在当前 node 版本下重装）",
                    r.program
                ));
            }
            let nodes = codegraph_symbols(&r.program, work_path, &keywords, 2, &mut diag).await;
            if !nodes.is_empty() {
                let (files, used) = pack_symbols(work_path, &nodes, max_tokens, &mut diag);
                if !files.is_empty() {
                    return RetrieveOutcome {
                        summary: format!(
                            "codegraph 符号级 · v{} · 命中 {} 符号 · 采用 {}",
                            r.version,
                            nodes.len(),
                            used
                        ),
                        files,
                        diagnostics: diag,
                    };
                }
                diag.push("codegraph 命中符号但切片全为空，降级到文件级检索".into());
            } else {
                diag.push("codegraph 未命中符号，降级到文件级检索".into());
            }
        }
        (Some(_), false) => diag.push(
            "codegraph 已安装但该项目未建索引（缺 .codegraph/），降级到文件级检索；可在大脑聚合页建索引"
                .into(),
        ),
        (None, _) => diag.push("codegraph 未安装，使用文件级检索".into()),
    }

    // ---- 路径 2：文件级（grep / 遍历）----
    let (grep_opt, _) = tokio::join!(grep_search(work_path, &keywords, 15), async {});
    let grep_results = match grep_opt {
        Some(results) => results,
        None => {
            diag.push("ripgrep 不可用，改用纯 Rust 目录遍历".into());
            walk_search(work_path, &keywords, 15)
        }
    };

    // 敏感文件排除（恒定生效）
    let (kept, denied) = filter_sensitive(grep_results, work_path);
    if !denied.is_empty() {
        diag.push(format!("已排除可能含凭据的文件 {} 个: {}", denied.len(), denied.join(", ")));
    }

    let mut sorted = kept;
    sorted.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    // 按 token 限制读取文件内容
    let mut results = Vec::new();
    let mut total_tokens: u32 = 0;
    let per_file_cap = 8000; // 每个文件最多 8000 字符

    for (path, relevance) in sorted {
        let source = "grep";
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
    let n = results.len();
    RetrieveOutcome {
        files: results,
        summary: format!("文件级检索 · {n} 个文件"),
        diagnostics: diag,
    }
}

/// 把符号列表按 token 预算打包成 `RetrievedFile`（每个符号一条，path 带 `符号名 @ 文件:行`）。
/// 返回 (结果, 实际采用的符号数)。
fn pack_symbols(
    work_path: &Path,
    nodes: &[crate::codegraph::SymbolNode],
    max_tokens: u32,
    diag: &mut Vec<String>,
) -> (Vec<RetrievedFile>, usize) {
    let mut out = Vec::new();
    let mut total: u32 = 0;
    let mut denied = 0usize;
    for n in nodes {
        if total >= max_tokens {
            break;
        }
        let abs = work_path.join(&n.file_path);
        if is_sensitive_path(&abs) {
            denied += 1;
            continue;
        }
        let Some(body) = slice_symbol(work_path, n) else { continue };
        let tokens = estimate_tokens(&body);
        // 装不下时：若一个都还没装，把这个截到剩余预算再装（**不能直接 break**）。
        // 否则会返回空列表 → 上层判定「切片全为空」→ 降级到文件级检索 → 反而按 8000 字符/文件
        // 发送，比截断后的符号体大得多。预算越紧越不该退回更贵的路径。
        let (body, tokens) = if total + tokens > max_tokens {
            if !out.is_empty() {
                break;
            }
            let remaining = max_tokens.saturating_sub(total).max(1);
            let chars_budget = (remaining as usize) * 3; // 与文件级路径同一套粗略反推
            let trimmed = format!("{}\n… [预算不足，已截断]", take_chars(&body, chars_budget));
            let t = estimate_tokens(&trimmed);
            (trimmed, t)
        } else {
            (body, tokens)
        };
        total += tokens;
        // 头部带签名，让模型无需展开就知道契约
        let header = match (&n.signature, &n.return_type) {
            (Some(s), _) => format!("{} {}{}", n.kind, n.name, s),
            (None, Some(r)) => format!("{} {} -> {r}", n.kind, n.name),
            _ => format!("{} {}", n.kind, n.name),
        };
        let end = n.end_line.map(|e| e.to_string()).unwrap_or_else(|| "?".into());
        out.push(RetrievedFile {
            path: format!("{}:{}-{} · {}", n.file_path, n.start_line, end, header),
            content: body,
            relevance: 1.0,
            source: "codegraph".to_string(),
        });
    }
    if denied > 0 {
        diag.push(format!("已排除可能含凭据的文件 {denied} 个"));
    }
    let used = out.len();
    (out, used)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- 敏感文件排除（安全护栏，恒定生效）----

    #[test]
    fn denies_credential_files() {
        // Tier A：凭据类文件必须拒。这些一旦被检索到就会随 prompt 发给多个第三方上游。
        for p in [
            ".env",
            ".env.local",
            ".env.production",
            "config/.env.staging",
            "id_rsa",
            "id_rsa.pub",
            "id_ed25519",
            ".npmrc",
            ".netrc",
            ".git-credentials",
            "credentials",
            "credentials.json",
            "secrets.json",
            "server.pem",
            "client.key",
            "cert.p12",
            "store.jks",
            "vault.kdbx",
            "deploy.ppk",
        ] {
            assert!(
                is_sensitive_path(Path::new(p)),
                "{p} 含凭据风险，必须被排除"
            );
        }
    }

    #[test]
    fn denies_configish_by_name_substring() {
        // Tier B：仅配置/数据扩展名 + 名字含敏感子串才拒。
        for p in [
            "application-secret.yml",
            "db-password.properties",
            "apikey.json",
            "api_key.toml",
            "private.xml",
            "svc-token.ini",
        ] {
            assert!(is_sensitive_path(Path::new(p)), "{p} 应被 Tier B 排除");
        }
    }

    #[test]
    fn keeps_source_files_with_scary_names() {
        // 关键反例：分两级正是为了不误伤源码。若这些被排除，代码审查会漏掉核心实现。
        for p in [
            "TokenService.java",
            "src/secret.rs",
            "auth/credentials.go",
            "PasswordEncoder.kt",
            "apikey_manager.py",
            "src/private_helpers.ts",
            "docs/credentials.md",
            "README.md",
            "src-tauri/src/upstream.rs",
        ] {
            assert!(
                !is_sensitive_path(Path::new(p)),
                "{p} 是源码/文档，不应被排除"
            );
        }
    }

    #[test]
    fn filter_sensitive_partitions_and_reports() {
        let work = Path::new("/proj");
        let items = vec![
            (work.join("src/main.rs"), 1.0f32),
            (work.join(".env"), 0.9),
            (work.join("keys/server.pem"), 0.8),
            (work.join("src/token_service.rs"), 0.7),
        ];
        let (kept, denied) = filter_sensitive(items, work);
        let kept_names: Vec<String> = kept
            .iter()
            .map(|(p, _)| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(kept_names, vec!["main.rs", "token_service.rs"]);
        assert_eq!(denied.len(), 2, "被拒文件要能报出来供日志展示: {denied:?}");
    }

    // ---- 符号级切片（不发文件全文的核心）----

    fn write_temp(name: &str, content: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "synaroute_retr_test_{}_{}",
            std::process::id(),
            name
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, content).unwrap();
        p
    }

    fn node(file: &str, start: u32, end: Option<u32>) -> crate::codegraph::SymbolNode {
        crate::codegraph::SymbolNode {
            kind: "function".into(),
            name: "target".into(),
            qualified_name: None,
            file_path: file.into(),
            language: None,
            start_line: start,
            end_line: end,
            signature: Some("(x: u32) -> u32".into()),
            return_type: None,
            visibility: None,
            docstring: None,
        }
    }

    #[test]
    fn slices_exact_line_span_not_whole_file() {
        // 核心不变量：按 codegraph 的 1-based 含端点行区间只切符号体，
        // 不把整份文件塞进去——这正是「不打包传输」的实现点。
        let body: String = (1..=40).map(|i| format!("line{i}\n")).collect();
        let p = write_temp("span.rs", &body);
        let work = p.parent().unwrap();

        let out = slice_symbol(work, &node("span.rs", 10, Some(14))).expect("应切出内容");
        assert_eq!(
            out,
            "line10\nline11\nline12\nline13\nline14",
            "须含首尾两端、且不多不少"
        );
        assert!(!out.contains("line9") && !out.contains("line15"));

        std::fs::remove_dir_all(work).ok();
    }

    #[test]
    fn slice_caps_oversized_symbol() {
        // God method 保护：单符号超 150 行时截断并标注，避免一个符号吃光整个预算。
        let body: String = (1..=400).map(|i| format!("l{i}\n")).collect();
        let p = write_temp("big.rs", &body);
        let work = p.parent().unwrap();

        let out = slice_symbol(work, &node("big.rs", 1, Some(400))).unwrap();
        assert_eq!(out.lines().count(), 151, "150 行正文 + 1 行截断提示");
        assert!(out.contains("已截断"), "须标注被截断: {}", out.lines().last().unwrap());

        std::fs::remove_dir_all(work).ok();
    }

    #[test]
    fn slice_handles_unknown_end_line() {
        // impact 返回的邻接节点没有 endLine → 退化为从 startLine 起取有限行数，
        // 而不是读到文件末尾（否则大文件里一个节点就能撑爆预算）。
        let body: String = (1..=200).map(|i| format!("l{i}\n")).collect();
        let p = write_temp("noend.rs", &body);
        let work = p.parent().unwrap();

        let out = slice_symbol(work, &node("noend.rs", 5, None)).unwrap();
        assert_eq!(out.lines().count(), 40, "边界未知时取固定 40 行兜底");
        assert!(out.starts_with("l5"));

        std::fs::remove_dir_all(work).ok();
    }

    #[test]
    fn slice_refuses_sensitive_file() {
        // 切片层也要挡：codegraph 索引里可能有 .env 之类的节点。
        let p = write_temp(".env", "SECRET=abc\nTOKEN=def\n");
        let work = p.parent().unwrap();
        assert!(
            slice_symbol(work, &node(".env", 1, Some(2))).is_none(),
            "敏感文件即使被索引也不得切出内容"
        );
        std::fs::remove_dir_all(work).ok();
    }

    #[test]
    fn slice_out_of_range_start_is_none() {
        let p = write_temp("short.rs", "a\nb\n");
        let work = p.parent().unwrap();
        assert!(slice_symbol(work, &node("short.rs", 99, Some(120))).is_none());
        std::fs::remove_dir_all(work).ok();
    }

    #[test]
    fn pack_symbols_respects_budget_and_labels_path() {
        // 预算生效 + 路径标签带上「文件:起-止 · kind name 签名」，让模型不展开也知道契约。
        let body: String = (1..=100).map(|i| format!("x{i}\n")).collect();
        let p = write_temp("pack.rs", &body);
        let work = p.parent().unwrap();

        let nodes = vec![
            node("pack.rs", 1, Some(20)),
            node("pack.rs", 30, Some(50)),
            node("pack.rs", 60, Some(80)),
        ];
        let mut diag = Vec::new();
        // 给一个很小的预算：只应装下第一个符号（且它会被按剩余预算截断）
        let (files, used) = pack_symbols(work, &nodes, 8, &mut diag);
        assert_eq!(used, files.len());
        assert!(used >= 1 && used < nodes.len(), "预算应限制采用数量，实际 {used}");
        assert!(
            files[0].path.contains("pack.rs:1-20") && files[0].path.contains("(x: u32) -> u32"),
            "路径标签须含行区间与签名: {}",
            files[0].path
        );
        assert_eq!(files[0].source, "codegraph");
        assert!(
            files[0].content.contains("预算不足"),
            "预算紧张时首个符号应被截断而非整体放弃: {}",
            files[0].content
        );

        std::fs::remove_dir_all(work).ok();
    }

    #[test]
    fn pack_symbols_never_returns_empty_on_tiny_budget() {
        // 回归护栏：预算极小时也必须至少产出一条（截断版）。
        // 若返回空，上层会判定「切片全为空」而降级到文件级检索——按 8000 字符/文件发送，
        // 比截断后的符号体大一个量级，越省预算反而越费。
        let body: String = (1..=60).map(|i| format!("y{i}\n")).collect();
        let p = write_temp("tiny.rs", &body);
        let work = p.parent().unwrap();
        let mut diag = Vec::new();
        let (files, used) = pack_symbols(work, &[node("tiny.rs", 1, Some(60))], 1, &mut diag);
        assert_eq!(used, 1, "极小预算也要产出 1 条截断内容，不能返回空");
        assert!(files[0].content.contains("预算不足"));
        std::fs::remove_dir_all(work).ok();
    }

    #[test]
    fn pack_symbols_skips_sensitive_and_reports() {
        let body = "line1\nline2\nline3\n";
        let dir = std::env::temp_dir().join(format!("synaroute_pack_sens_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("ok.rs"), body).unwrap();
        std::fs::write(dir.join(".env"), body).unwrap();

        let nodes = vec![node(".env", 1, Some(3)), node("ok.rs", 1, Some(3))];
        let mut diag = Vec::new();
        let (files, _) = pack_symbols(&dir, &nodes, 10_000, &mut diag);
        assert_eq!(files.len(), 1, "敏感文件应被跳过");
        assert!(files[0].path.starts_with("ok.rs"));
        assert!(
            diag.iter().any(|d| d.contains("凭据")),
            "跳过要留诊断，不能静默: {diag:?}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
