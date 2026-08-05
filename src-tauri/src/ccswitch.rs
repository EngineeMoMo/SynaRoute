//! 从 cc-switch 导入历史 Key。
//!
//! cc-switch 把各端的供应商档存在 `~/.cc-switch/cc-switch.db`（SQLite）的 `providers` 表：
//! `id / app_type / name / settings_config(JSON) / sort_index / is_current / …`。
//! `settings_config` 有两种形态（2026-07-31 实测本机库）：
//! - `claude` / `claude-desktop`：`{"env":{"ANTHROPIC_BASE_URL":…,"ANTHROPIC_AUTH_TOKEN":…}}`
//! - `codex`：`{"auth":{"OPENAI_API_KEY":…,"auth_mode":…},"config":"<整段 config.toml 文本>"}`
//!   其中 base_url / wire_api 要从 TOML 的 `[model_providers.<model_provider>]` 里取。
//!
//! **只读铁律**：本模块只 `SELECT`，且先把 db 复制到临时文件再打开，
//! 绝不在 cc-switch 的库上加写锁、绝不改动/删除它的任何数据（用户可能还要继续用它）。
//!
//! **不自动接入**：导入只把 Key 存进 SynaRoute（含 DPAPI 加密密钥），
//! 不写任何客户端配置、不改 appliedId、不启代理——接入与否由用户显式操作。

use crate::error::{AppError, AppResult};
use crate::model::{CategoryType, KeyParams, Protocol, ProviderKey};
use crate::store::Store;
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;

/// cc-switch 的数据目录（固定在用户主目录下的 `.cc-switch`）。
fn ccswitch_db_path() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    let p = home.join(".cc-switch").join("cc-switch.db");
    p.exists().then_some(p)
}

/// 这台机器上有没有 cc-switch 的库（首启向导第②步用）。
///
/// 刻意不复用 `scan()`：那个会把整个 db 复制到临时目录再开 sqlite 连接、逐条解析候选。
/// 向导只需要「有没有」这一个 bit 来决定默认高亮哪个主选项 —— 对已经在用 cc-switch 的用户，
/// 「从这里导入」比手工填 13 个字段快得多，值得默认选中；没装的人则不该看到一个无用选项。
/// 真正的候选列表仍由用户点开 `CcSwitchImportDialog` 时的 `scan()` 提供。
pub fn db_available() -> bool {
    ccswitch_db_path().is_some()
}

/// 一条可导入（或明确不可导入）的候选。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportCandidate {
    /// cc-switch 侧的 providers.id，导入时按它回查
    pub source_id: String,
    /// cc-switch 的 app_type 原值（claude / claude-desktop / codex / gemini / …）
    pub app_type: String,
    /// 映射到的 SynaRoute 分类；不可映射时为 None
    pub category_id: Option<CategoryType>,
    pub name: String,
    pub base_url: String,
    pub protocol: Option<Protocol>,
    /// 从 codex 的 config.toml 顶层 `model` 取到的默认模型（其余端为 None）
    pub default_model: Option<String>,
    /// 该档在 cc-switch 里是否为当前生效项
    pub is_current: bool,
    /// 密钥掩码（前 6 + 后 4 + 长度），明文绝不出前端
    pub secret_masked: String,
    /// 与 SynaRoute 已有 Key 重复时给出对方名称（同分类 + 同 base_url + 同密钥）
    pub duplicate_of: Option<String>,
    /// 不可导入的原因（官方登录档 / 无密钥 / 分类不支持 等）。为 None 才可导入。
    pub skip_reason: Option<String>,
}

/// 扫描结果。`db_path` 供 UI 展示数据来源，`total` 是库里的 providers 总数。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanResult {
    pub db_path: String,
    pub total: usize,
    pub candidates: Vec<ImportCandidate>,
}

/// 导入结果：逐条给出结局，便于 UI 精确回显（而非只报个数）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportOutcome {
    pub source_id: String,
    pub name: String,
    /// imported | skipped | failed
    pub status: String,
    pub detail: String,
    /// 新建的 SynaRoute keyId（仅 imported 时有）
    pub key_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    pub imported: usize,
    pub skipped: usize,
    pub failed: usize,
    pub outcomes: Vec<ImportOutcome>,
}

/// 掩码密钥：保留前 6、后 4 与长度，够用户辨认是哪一条，又不泄露。
fn mask_secret(s: &str) -> String {
    let n = s.chars().count();
    if n == 0 {
        return String::new();
    }
    if n <= 12 {
        return format!("{}… ({n})", s.chars().take(2).collect::<String>());
    }
    let head: String = s.chars().take(6).collect();
    let tail: String = s.chars().skip(n - 4).collect();
    format!("{head}…{tail} ({n})")
}

/// base_url 归一化（仅用于去重比较）：去尾斜杠 + 小写。
fn norm_url(s: &str) -> String {
    s.trim().trim_end_matches('/').to_ascii_lowercase()
}

/// cc-switch 的 app_type → SynaRoute 分类。gemini 等未支持端返回 None。
fn map_category(app_type: &str) -> Option<CategoryType> {
    CategoryType::ALL.into_iter().find(|c| c.meta().ccswitch_app_type == app_type)
}

/// providers 表里的一行原始数据。
#[derive(Debug)]
struct RawProvider {
    id: String,
    app_type: String,
    name: String,
    settings_config: String,
    sort_index: i64,
    is_current: bool,
}

/// 从 cc-switch 库读出全部 providers。
///
/// 先把 db 文件复制到临时目录再打开：cc-switch 可能正在运行并持有连接，
/// 直接以 SQLite 打开原文件即便只读也可能因 WAL / 锁而失败或触发副作用。
/// 复制读还顺带保证「绝不可能写到用户的库」。
fn read_providers() -> AppResult<(PathBuf, Vec<RawProvider>)> {
    let db = ccswitch_db_path().ok_or_else(|| {
        AppError::NotFound("未找到 cc-switch 数据库（~/.cc-switch/cc-switch.db）".into())
    })?;
    let rows = read_providers_at(&db)?;
    Ok((db, rows))
}

/// 可测入口：从指定 db 文件读 providers（生产走 [`read_providers`] 定位真实路径）。
fn read_providers_at(db: &std::path::Path) -> AppResult<Vec<RawProvider>> {
    let tmp = std::env::temp_dir().join(format!(
        "synaroute-ccswitch-{}-{}.db",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    std::fs::copy(db, &tmp)
        .map_err(|e| AppError::Other(format!("复制 cc-switch 库失败（{}）: {e}", db.display())))?;

    let result = (|| -> AppResult<Vec<RawProvider>> {
        let conn = rusqlite::Connection::open_with_flags(
            &tmp,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|e| AppError::Other(format!("打开 cc-switch 库副本失败: {e}")))?;
        let mut stmt = conn
            .prepare(
                // COALESCE 不是保险起见：本机实测 cc-switch 对用户自建档的 `sort_index` 写的是
                // NULL（只有内置官方模板写 0）。不兜底会让 rusqlite 取列时报 InvalidColumnType，
                // 整次扫描直接失败。name / app_type 同理兜底，杜绝单条脏数据毁掉全部导入。
                "SELECT COALESCE(id, ''), COALESCE(app_type, ''), COALESCE(name, ''), \
                 settings_config, COALESCE(sort_index, 0), COALESCE(is_current, 0) \
                 FROM providers ORDER BY app_type, sort_index, name",
            )
            .map_err(|e| AppError::Other(format!("查询 providers 失败（表结构可能已变）: {e}")))?;
        let rows = stmt
            .query_map([], |r| {
                Ok(RawProvider {
                    id: r.get(0)?,
                    app_type: r.get(1)?,
                    name: r.get(2)?,
                    settings_config: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                    sort_index: r.get(4)?,
                    is_current: r.get::<_, i64>(5)? != 0,
                })
            })
            .map_err(|e| AppError::Other(format!("读取 providers 行失败: {e}")))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| AppError::Other(format!("解析 providers 行失败: {e}")))?);
        }
        Ok(out)
    })();

    // 无论成败都清掉副本，不在临时目录留用户密钥。
    let _ = std::fs::remove_file(&tmp);
    result
}

/// 从一行 provider 解出接入四要素。`Err(String)` 是「明确不可导入」的原因（人类可读）。
struct Parsed {
    base_url: String,
    secret: String,
    protocol: Protocol,
    default_model: Option<String>,
}

fn parse_provider(app_type: &str, settings_config: &str) -> Result<Parsed, String> {
    let cfg: serde_json::Value = serde_json::from_str(settings_config)
        .map_err(|e| format!("settings_config 不是合法 JSON: {e}"))?;

    match app_type {
        // Claude CLI / 桌面端：env 形态，协议恒为 Anthropic。
        "claude" | "claude-desktop" => {
            let env = cfg.get("env").and_then(|v| v.as_object());
            let get = |k: &str| -> String {
                env.and_then(|e| e.get(k))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_string()
            };
            let base_url = get("ANTHROPIC_BASE_URL");
            // cc-switch 也可能用 ANTHROPIC_API_KEY 承载密钥，两者都收。
            let secret = {
                let t = get("ANTHROPIC_AUTH_TOKEN");
                if t.is_empty() { get("ANTHROPIC_API_KEY") } else { t }
            };
            if base_url.is_empty() && secret.is_empty() {
                return Err("官方登录档（env 为空），无需导入".into());
            }
            if base_url.is_empty() {
                return Err("缺少 ANTHROPIC_BASE_URL".into());
            }
            if secret.is_empty() {
                return Err("缺少 ANTHROPIC_AUTH_TOKEN".into());
            }
            Ok(Parsed { base_url, secret, protocol: Protocol::Anthropic, default_model: None })
        }
        // Codex：auth 段给密钥，config 段是一整份 config.toml 文本。
        "codex" => {
            let auth = cfg.get("auth").and_then(|v| v.as_object());
            let auth_mode = auth
                .and_then(|a| a.get("auth_mode"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let secret = auth
                .and_then(|a| a.get("OPENAI_API_KEY"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            if secret.is_empty() {
                return Err(if auth_mode == "chatgpt" {
                    "ChatGPT 官方登录档（只有 OAuth token，无 API Key）".into()
                } else {
                    "缺少 OPENAI_API_KEY".into()
                });
            }
            let toml_text = cfg.get("config").and_then(|v| v.as_str()).unwrap_or("");
            let doc: toml::Value = toml_text
                .parse()
                .map_err(|e| format!("config 段不是合法 TOML: {e}"))?;
            // 顶层 model_provider 指向 [model_providers.<id>]；缺省时取表里唯一一项。
            let providers = doc.get("model_providers").and_then(|v| v.as_table());
            let selected = doc.get("model_provider").and_then(|v| v.as_str());
            let entry = match (providers, selected) {
                (Some(t), Some(id)) => t.get(id).or_else(|| t.values().next()),
                (Some(t), None) => t.values().next(),
                _ => None,
            }
            .and_then(|v| v.as_table())
            .ok_or_else(|| "config 里没有 [model_providers.*] 表".to_string())?;
            let base_url = entry
                .get("base_url")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            if base_url.is_empty() {
                return Err("[model_providers.*] 缺少 base_url".into());
            }
            // wire_api 决定协议；缺省按 Codex 惯例视为 responses。
            let protocol = match entry.get("wire_api").and_then(|v| v.as_str()).unwrap_or("responses") {
                "chat" => Protocol::OpenaiChat,
                _ => Protocol::OpenaiResponses,
            };
            let default_model = doc
                .get("model")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            Ok(Parsed { base_url, secret, protocol, default_model })
        }
        other => Err(format!("SynaRoute 暂不支持 cc-switch 的 `{other}` 端")),
    }
}

/// 在 SynaRoute 已有 Key 里找重复（同分类 + 同 base_url + 同密钥）。返回对方名称。
///
/// 必须比到密钥：同一站点多账号（同 base_url 不同 key）是常见用法，
/// 只按 base_url 判重会把第二个账号误判成重复、导不进来。
fn find_duplicate(
    store: &Arc<Store>,
    category: CategoryType,
    base_url: &str,
    secret: &str,
) -> Option<String> {
    let want = norm_url(base_url);
    for k in store.list_keys(category) {
        if norm_url(&k.base_url) != want {
            continue;
        }
        let existing = store.secrets.read().get(&k.id).ok().flatten();
        if existing.as_deref().map(String::as_str) == Some(secret) {
            return Some(k.name);
        }
    }
    None
}

/// 扫描 cc-switch 库，产出候选列表（不写任何东西）。
pub fn scan(store: &Arc<Store>) -> AppResult<ScanResult> {
    let (db, raws) = read_providers()?;
    let candidates = build_candidates(store, &raws);
    Ok(ScanResult { db_path: db.display().to_string(), total: raws.len(), candidates })
}

/// 可测入口：对指定 db 做扫描。
#[cfg(test)]
fn scan_at(store: &Arc<Store>, db: &std::path::Path) -> AppResult<ScanResult> {
    let raws = read_providers_at(db)?;
    let candidates = build_candidates(store, &raws);
    Ok(ScanResult { db_path: db.display().to_string(), total: raws.len(), candidates })
}

fn build_candidates(store: &Arc<Store>, raws: &[RawProvider]) -> Vec<ImportCandidate> {
    let mut candidates = Vec::with_capacity(raws.len());
    for r in raws {
        let category_id = map_category(&r.app_type);
        let mut cand = ImportCandidate {
            source_id: r.id.clone(),
            app_type: r.app_type.clone(),
            category_id,
            name: r.name.clone(),
            base_url: String::new(),
            protocol: None,
            default_model: None,
            is_current: r.is_current,
            secret_masked: String::new(),
            duplicate_of: None,
            skip_reason: None,
        };
        match parse_provider(&r.app_type, &r.settings_config) {
            Ok(p) => {
                cand.base_url = p.base_url.clone();
                cand.protocol = Some(p.protocol);
                cand.default_model = p.default_model.clone();
                cand.secret_masked = mask_secret(&p.secret);
                match category_id {
                    Some(cat) => {
                        cand.duplicate_of = find_duplicate(store, cat, &p.base_url, &p.secret);
                        if cand.duplicate_of.is_some() {
                            cand.skip_reason = Some("SynaRoute 里已有同站点同密钥的 Key".into());
                        }
                    }
                    None => {
                        cand.skip_reason =
                            Some(format!("SynaRoute 暂不支持 cc-switch 的 `{}` 端", r.app_type));
                    }
                }
            }
            Err(reason) => cand.skip_reason = Some(reason),
        }
        candidates.push(cand);
    }
    candidates
}

/// 生成新 Key 的 id。
///
/// 用 uuid v4 而非时间戳（P3-5）：id 被 `portable.rs` 与 `Store::apply_imported_config`
/// 当作**全局唯一标识**做「同 id 即同一条 Key」的覆盖判据。时间戳在跨机场景会碰撞——
/// 「两台机器照同一份教程配置」是真实场景，落在同一毫秒即撞号，跨机导入会把一条**完全
/// 无关**的本机 Key 静默覆盖成对方的 base_url/协议/映射。配 P2-3 的孤儿密钥问题还会得到
/// 「新配置 + 旧密钥」的嵌合体，转发 401 而界面一切正常。
///
/// `seq` 参数保留但不再参与 id 生成（uuid 自身已无碰撞之虞），仅为调用点签名稳定。
fn new_key_id(_seq: usize) -> String {
    crate::store::new_key_id()
}

/// 按 source_id 导入选中的候选。**只写 SynaRoute 自己的 Key 与密钥库**，
/// 不写任何客户端配置、不改接入状态、不启代理（用户约定：导入后不接入）。
///
/// 逐条独立处理：某条失败不影响其余条，最后在 report 里精确回显每条结局。
pub fn import(store: &Arc<Store>, source_ids: &[String]) -> AppResult<ImportReport> {
    let (_, raws) = read_providers()?;
    import_rows(store, &raws, source_ids)
}

/// 可测入口：对指定 db 做导入。
#[cfg(test)]
fn import_at(
    store: &Arc<Store>,
    db: &std::path::Path,
    source_ids: &[String],
) -> AppResult<ImportReport> {
    let raws = read_providers_at(db)?;
    import_rows(store, &raws, source_ids)
}

fn import_rows(
    store: &Arc<Store>,
    raws: &[RawProvider],
    source_ids: &[String],
) -> AppResult<ImportReport> {
    let mut report = ImportReport { imported: 0, skipped: 0, failed: 0, outcomes: Vec::new() };

    for (seq, want) in source_ids.iter().enumerate() {
        let Some(raw) = raws.iter().find(|r| &r.id == want) else {
            report.failed += 1;
            report.outcomes.push(ImportOutcome {
                source_id: want.clone(),
                name: String::new(),
                status: "failed".into(),
                detail: "cc-switch 库里已找不到该档（可能刚被删除）".into(),
                key_id: None,
            });
            continue;
        };
        let push_skip = |report: &mut ImportReport, detail: String| {
            report.skipped += 1;
            report.outcomes.push(ImportOutcome {
                source_id: raw.id.clone(),
                name: raw.name.clone(),
                status: "skipped".into(),
                detail,
                key_id: None,
            });
        };

        let Some(category) = map_category(&raw.app_type) else {
            push_skip(&mut report, format!("不支持的端 `{}`", raw.app_type));
            continue;
        };
        let parsed = match parse_provider(&raw.app_type, &raw.settings_config) {
            Ok(p) => p,
            Err(reason) => {
                push_skip(&mut report, reason);
                continue;
            }
        };
        if let Some(dup) = find_duplicate(store, category, &parsed.base_url, &parsed.secret) {
            push_skip(&mut report, format!("已存在同站点同密钥的 Key「{dup}」"));
            continue;
        }

        let key_id = new_key_id(seq);
        let key = ProviderKey {
            id: key_id.clone(),
            category_id: category,
            // 名称冲突不阻断：SynaRoute 用 id 唯一标识，名字允许重名。
            name: raw.name.clone(),
            // vendor 供 UI 分组/图标用，导入来源统一标记，便于事后辨认与回溯。
            vendor: "cc-switch".into(),
            base_url: parsed.base_url.clone(),
            protocol: parsed.protocol,
            // 先建 Key（has_secret=false），密钥写成功后再置 true 落盘——
            // 与 save_secret 同一顺序，避免「标记有密钥但库里没有」的不一致。
            has_secret: false,
            // 导入即启用：用户是主动勾选的，默认可用符合预期；不想用可在列表里关。
            enabled: true,
            priority: raw.sort_index as i32,
            headers_json: None,
            params: KeyParams::default(),
            // 模型列表留空：导入不联网。用户点「拉取模型」再填，避免导入阶段卡在网络上。
            models: vec![],
            mappings: vec![],
            default_model: parsed.default_model.clone(),
            tier_haiku: None,
            tier_sonnet: None,
            tier_opus: None,
            health: Default::default(),
        };

        if let Err(e) = store.upsert_key(key) {
            report.failed += 1;
            report.outcomes.push(ImportOutcome {
                source_id: raw.id.clone(),
                name: raw.name.clone(),
                status: "failed".into(),
                detail: format!("写入 Key 失败: {e}"),
                key_id: None,
            });
            continue;
        }
        if let Err(e) = store.secrets.write().set(&key_id, &parsed.secret) {
            // 密钥没存进去，这条 Key 是残废的，回滚掉，别在列表里留个不能用的条目。
            let _ = store.delete_key(&key_id);
            report.failed += 1;
            report.outcomes.push(ImportOutcome {
                source_id: raw.id.clone(),
                name: raw.name.clone(),
                status: "failed".into(),
                detail: format!("保存密钥失败（已回滚该 Key）: {e}"),
                key_id: None,
            });
            continue;
        }
        if let Some(mut k) = store.get_key(&key_id) {
            k.has_secret = true;
            let _ = store.upsert_key(k);
        }
        store.append_event(
            category,
            "config",
            Some(&key_id),
            &format!(
                "从 cc-switch 导入 Key「{}」（{}），未接入客户端配置",
                raw.name, parsed.base_url
            ),
        );
        report.imported += 1;
        report.outcomes.push(ImportOutcome {
            source_id: raw.id.clone(),
            name: raw.name.clone(),
            status: "imported".into(),
            detail: format!("已导入到分类 {}", category.as_str()),
            key_id: Some(key_id),
        });
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn temp_dir(tag: &str) -> PathBuf {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "synaroute_ccswitch_test_{tag}_{}_{seq}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn temp_store(dir: &std::path::Path) -> Arc<Store> {
        Arc::new(Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap())
    }

    /// 按 cc-switch 真实表结构造一个库（列名/类型取自本机 2026-07-31 实测的 `providers` 表）。
    fn make_db(dir: &std::path::Path, rows: &[(&str, &str, &str, &str, i64, i64)]) -> PathBuf {
        let path = dir.join("cc-switch.db");
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE providers (
                id TEXT PRIMARY KEY, app_type TEXT, name TEXT, settings_config TEXT,
                website_url TEXT, category TEXT, created_at INTEGER, sort_index INTEGER,
                notes TEXT, icon TEXT, icon_color TEXT, meta TEXT,
                is_current BOOLEAN, in_failover_queue BOOLEAN, cost_multiplier TEXT,
                limit_daily_usd TEXT, limit_monthly_usd TEXT, provider_type TEXT);",
        )
        .unwrap();
        for (id, app_type, name, cfg, sort_index, is_current) in rows {
            conn.execute(
                "INSERT INTO providers (id, app_type, name, settings_config, sort_index, is_current)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![id, app_type, name, cfg, sort_index, is_current],
            )
            .unwrap();
        }
        drop(conn);
        path
    }

    /// 本机实测的两种 settings_config 原样（脱敏后的等价结构）。
    const CLAUDE_CFG: &str = r#"{"env":{"ANTHROPIC_BASE_URL":"https://sub.example.com","ANTHROPIC_AUTH_TOKEN":"sk-aaaaaaaaaaaaaaaaaaaaaaaaaaaa1111"}}"#;
    const DESKTOP_CFG: &str = r#"{"env":{"ANTHROPIC_BASE_URL":"https://k40.example.cn","ANTHROPIC_AUTH_TOKEN":"sk-bbbbbbbbbbbbbbbbbbbbbbbbbbbb2222"}}"#;
    const OFFICIAL_CFG: &str = r#"{"env":{}}"#;
    const CODEX_RESP_CFG: &str = r#"{"auth":{"OPENAI_API_KEY":"sk-cccccccccccccccccccccccccccc3333"},"config":"model_provider = \"custom\"\nmodel = \"gpt-5.6-sol\"\nmodel_reasoning_effort = \"high\"\ndisable_response_storage = true\n\n[model_providers.custom]\nname = \"公益\"\nbase_url = \"https://muyuan.example/v1\"\nwire_api = \"responses\"\nrequires_openai_auth = true\n"}"#;
    const CODEX_CHAT_CFG: &str = r#"{"auth":{"OPENAI_API_KEY":"sk-dddddddddddddddddddddddddddd4444","auth_mode":"apikey"},"config":"model_provider = \"cheap\"\nmodel = \"glm-4.6\"\n\n[model_providers.cheap]\nbase_url = \"https://chat.example/v1\"\nwire_api = \"chat\"\n"}"#;
    const CODEX_OAUTH_CFG: &str = r#"{"auth":{"auth_mode":"chatgpt","OPENAI_API_KEY":null,"tokens":{"id_token":"eyJhbGc","refresh_token":"rt"},"last_refresh":"2026-07-01T00:00:00Z"}}"#;

    fn full_db(dir: &std::path::Path) -> PathBuf {
        make_db(
            dir,
            &[
                ("p-claude", "claude", "Sub2API", CLAUDE_CFG, 0, 1),
                ("p-desktop", "claude-desktop", "林夕", DESKTOP_CFG, 1, 0),
                ("p-official", "claude", "Claude Official", OFFICIAL_CFG, 2, 0),
                ("p-codex-r", "codex", "公益", CODEX_RESP_CFG, 3, 1),
                ("p-codex-c", "codex", "便宜站", CODEX_CHAT_CFG, 4, 0),
                ("p-codex-oauth", "codex", "OpenAI Official", CODEX_OAUTH_CFG, 5, 0),
                ("p-gemini", "gemini", "Google Official", OFFICIAL_CFG, 6, 0),
            ],
        )
    }

    fn find<'a>(r: &'a ScanResult, id: &str) -> &'a ImportCandidate {
        r.candidates.iter().find(|c| c.source_id == id).expect("候选缺失")
    }

    #[test]
    fn scan_maps_all_three_ends_with_real_shapes() {
        let dir = temp_dir("scan_map");
        let store = temp_store(&dir);
        let db = full_db(&dir);
        let r = scan_at(&store, &db).unwrap();
        assert_eq!(r.total, 7);

        let c = find(&r, "p-claude");
        assert_eq!(c.category_id, Some(CategoryType::ClaudeCli), "claude → CLI 分类");
        assert_eq!(c.protocol, Some(Protocol::Anthropic));
        assert_eq!(c.base_url, "https://sub.example.com");
        assert!(c.is_current, "cc-switch 当前项应标出来");
        assert_eq!(c.skip_reason, None, "可导入项不该有 skip 原因");

        let d = find(&r, "p-desktop");
        assert_eq!(d.category_id, Some(CategoryType::ClaudeDesktop));
        assert_eq!(d.protocol, Some(Protocol::Anthropic));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn scan_reads_codex_protocol_and_model_from_embedded_toml() {
        // Codex 的 base_url / wire_api 藏在 settings_config.config 那段 TOML 里，
        // 且要按顶层 model_provider 选中对应的 [model_providers.<id>] 表。
        let dir = temp_dir("scan_codex");
        let store = temp_store(&dir);
        let db = full_db(&dir);
        let r = scan_at(&store, &db).unwrap();

        let resp = find(&r, "p-codex-r");
        assert_eq!(resp.category_id, Some(CategoryType::Codex));
        assert_eq!(resp.protocol, Some(Protocol::OpenaiResponses), "wire_api=responses");
        assert_eq!(resp.base_url, "https://muyuan.example/v1", "base_url 取自选中的 provider 表");
        assert_eq!(resp.default_model.as_deref(), Some("gpt-5.6-sol"), "顶层 model → 默认模型");
        assert_eq!(resp.skip_reason, None);

        let chat = find(&r, "p-codex-c");
        assert_eq!(chat.protocol, Some(Protocol::OpenaiChat), "wire_api=chat 要映射成 Chat 协议");
        assert_eq!(chat.base_url, "https://chat.example/v1", "非 custom 的表名也要能选中");
        assert_eq!(chat.default_model.as_deref(), Some("glm-4.6"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn scan_marks_unimportable_with_reasons_and_never_leaks_plaintext() {
        let dir = temp_dir("scan_skip");
        let store = temp_store(&dir);
        let db = full_db(&dir);
        let r = scan_at(&store, &db).unwrap();

        // 官方档：env 为空 → 明确原因，而不是静默消失（用户要能看懂为什么导不了）。
        let off = find(&r, "p-official");
        assert!(off.skip_reason.is_some(), "官方档应给出不可导入原因");
        assert!(off.secret_masked.is_empty(), "无密钥不应产出掩码");

        // ChatGPT OAuth 档：OPENAI_API_KEY 是 JSON null（不是空串），必须识别为无 API Key。
        let oauth = find(&r, "p-codex-oauth");
        let reason = oauth.skip_reason.clone().expect("OAuth 档应不可导入");
        assert!(reason.contains("ChatGPT"), "原因要点明是 ChatGPT 登录态，实际: {reason}");

        let gem = find(&r, "p-gemini");
        assert_eq!(gem.category_id, None, "gemini 无对应分类");
        assert!(gem.skip_reason.is_some());

        // 全量断言：任何候选都不得携带明文密钥（掩码里不能出现完整 token）。
        for c in &r.candidates {
            assert!(
                !c.secret_masked.contains("aaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
                "掩码泄露了密钥主体: {}",
                c.secret_masked
            );
            let json = serde_json::to_string(c).unwrap();
            assert!(!json.contains("3333\""), "序列化后不得含完整密钥: {json}");
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn import_creates_key_with_secret_and_leaves_ccswitch_db_untouched() {
        let dir = temp_dir("import_ok");
        let store = temp_store(&dir);
        let db = full_db(&dir);
        let before = std::fs::read(&db).unwrap();

        let rep =
            import_at(&store, &db, &["p-claude".to_string(), "p-codex-r".to_string()]).unwrap();
        assert_eq!(rep.imported, 2, "两条都应导入: {:?}", rep.outcomes);
        assert_eq!(rep.failed, 0);

        let cli = store.list_keys(CategoryType::ClaudeCli);
        assert_eq!(cli.len(), 1);
        assert_eq!(cli[0].name, "Sub2API");
        assert_eq!(cli[0].base_url, "https://sub.example.com");
        assert_eq!(cli[0].protocol, Protocol::Anthropic);
        assert!(cli[0].has_secret, "密钥写成功后 has_secret 必须为 true（否则 UI 显示未配置）");
        assert!(cli[0].enabled, "导入即启用");
        assert_eq!(cli[0].vendor, "cc-switch", "vendor 标来源，便于事后辨认");
        assert_eq!(
            store.secrets.read().get(&cli[0].id).unwrap().as_deref().map(String::as_str),
            Some("sk-aaaaaaaaaaaaaaaaaaaaaaaaaaaa1111"),
            "密钥必须能原样解出来（DPAPI 往返）"
        );

        let codex = store.list_keys(CategoryType::Codex);
        assert_eq!(codex.len(), 1);
        assert_eq!(codex[0].protocol, Protocol::OpenaiResponses);
        assert_eq!(codex[0].default_model.as_deref(), Some("gpt-5.6-sol"));
        assert!(codex[0].models.is_empty(), "导入不联网，模型列表应留空");

        // 只读铁律：cc-switch 的库字节级不变。
        assert_eq!(std::fs::read(&db).unwrap(), before, "绝不允许改动 cc-switch 的库");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn import_skips_duplicate_but_allows_same_site_different_key() {
        // 同站点多账号是常见用法：只按 base_url 判重会把第二个账号误杀。
        let dir = temp_dir("import_dup");
        let store = temp_store(&dir);
        let db = full_db(&dir);

        let first = import_at(&store, &db, &["p-claude".to_string()]).unwrap();
        assert_eq!(first.imported, 1);

        // 同一条再导 → 判重跳过，不产生第二个 Key。
        let again = import_at(&store, &db, &["p-claude".to_string()]).unwrap();
        assert_eq!(again.imported, 0);
        assert_eq!(again.skipped, 1);
        assert!(
            again.outcomes[0].detail.contains("已存在"),
            "跳过原因要说明是重复: {}",
            again.outcomes[0].detail
        );
        assert_eq!(store.list_keys(CategoryType::ClaudeCli).len(), 1, "不得重复建 Key");

        // 同 base_url、不同密钥 → 另一个账号，必须能导入。
        let sub = dir.join("other");
        std::fs::create_dir_all(&sub).unwrap();
        let other = make_db(
            &sub,
            &[(
                "p-same-site",
                "claude",
                "同站另一账号",
                r#"{"env":{"ANTHROPIC_BASE_URL":"https://sub.example.com","ANTHROPIC_AUTH_TOKEN":"sk-zzzzzzzzzzzzzzzzzzzzzzzzzzzz9999"}}"#,
                0,
                0,
            )],
        );
        let third = import_at(&store, &other, &["p-same-site".to_string()]).unwrap();
        assert_eq!(third.imported, 1, "同站不同密钥应视为新账号: {:?}", third.outcomes);
        assert_eq!(store.list_keys(CategoryType::ClaudeCli).len(), 2);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn import_reports_unknown_source_id_as_failed_not_silent() {
        let dir = temp_dir("import_missing");
        let store = temp_store(&dir);
        let db = full_db(&dir);
        let rep = import_at(&store, &db, &["p-does-not-exist".to_string()]).unwrap();
        assert_eq!(rep.failed, 1, "找不到的 id 必须报失败，不能静默忽略");
        assert_eq!(rep.imported, 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn import_skips_oauth_and_official_even_if_explicitly_selected() {
        // 用户在 UI 里可能连不可导入项一起提交，后端必须自己再判一次、给出原因。
        let dir = temp_dir("import_skip");
        let store = temp_store(&dir);
        let db = full_db(&dir);
        let rep = import_at(
            &store,
            &db,
            &["p-official".to_string(), "p-codex-oauth".to_string(), "p-gemini".to_string()],
        )
        .unwrap();
        assert_eq!(rep.imported, 0, "三条都不可导入");
        assert_eq!(rep.skipped, 3, "{:?}", rep.outcomes);
        assert!(store.list_keys(CategoryType::ClaudeCli).is_empty());
        assert!(store.list_keys(CategoryType::Codex).is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn mask_secret_keeps_head_tail_and_length() {
        assert_eq!(mask_secret(""), "");
        assert_eq!(mask_secret("sk-abcdef123456789"), "sk-abc…6789 (18)");
        let short = mask_secret("sk-1234");
        assert!(short.starts_with("sk") && short.contains("(7)"), "短串掩码: {short}");
    }

    #[test]
    fn missing_db_reports_readable_error_rather_than_panicking() {
        let dir = temp_dir("import_nodb");
        let err = read_providers_at(&dir.join("nope.db")).unwrap_err();
        assert!(format!("{err}").contains("复制"), "缺库应给可读错误: {err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 真机实证：cc-switch 对**用户自建档**的 `sort_index` 写的是 NULL（只有内置官方模板是 0）。
    /// 这条锁住 NULL 兜底——不兜底则 rusqlite 取列报 InvalidColumnType，整次扫描全灭。
    #[test]
    fn scan_survives_null_sort_index_and_null_name() {
        let dir = temp_dir("scan_nulls");
        let store = temp_store(&dir);
        let path = dir.join("cc-switch.db");
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE providers (
                id TEXT PRIMARY KEY, app_type TEXT, name TEXT, settings_config TEXT,
                sort_index INTEGER, is_current BOOLEAN);",
        )
        .unwrap();
        // sort_index / name / settings_config 全为 NULL 的极端行。
        conn.execute(
            "INSERT INTO providers (id, app_type, name, settings_config, sort_index, is_current)
             VALUES ('p-null', 'claude', NULL, NULL, NULL, NULL)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO providers (id, app_type, name, settings_config, sort_index, is_current)
             VALUES ('p-ok', 'claude', '正常档', ?1, NULL, 1)",
            rusqlite::params![CLAUDE_CFG],
        )
        .unwrap();
        drop(conn);

        let r = scan_at(&store, &path).expect("NULL 列不得让整次扫描失败");
        assert_eq!(r.total, 2);
        let bad = find(&r, "p-null");
        assert!(bad.skip_reason.is_some(), "settings_config 为 NULL 应标不可导入而非崩");
        let ok = find(&r, "p-ok");
        assert_eq!(ok.skip_reason, None, "同库里的正常档必须照常可导入");
        assert!(ok.is_current);

        std::fs::remove_dir_all(&dir).ok();
    }
}
