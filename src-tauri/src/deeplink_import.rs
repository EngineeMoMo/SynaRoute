//! `synaroute://` 深链接一键导入 Key。
//!
//! 网页（如自建 sub2api）点「导入到 SynaRoute」→ 打开
//! `synaroute://v1/import?resource=provider&category=…&name=…&protocol=…&endpoint=…&apiKey=…`
//! → 系统唤起本应用 → **弹确认框**（绝不静默导入）→ 用户确认后落盘为一条 ProviderKey（含密钥）。
//!
//! # 🔴 为什么密钥不进事件、不往返前端
//!
//! `events.rs` 全模块规约是「载荷只带 topic + category，业务数据前端自己 fetch」。而这里的载荷
//! 恰恰含**明文密钥**。故设计成：后端解析后把结果（含密钥）存进**进程内 pending slot**，只把
//! **不含密钥的预览**（`ImportPreview`，密钥仅以 `has_secret` 体现）交给前端确认框；用户点确认后
//! 后端从 slot 取出原始密钥落盘。密钥全程只在后端内存里待过，不经 IPC 往返、不进任何日志。
//!
//! # 🔴 安全：任何网页/任何人都能构造 `synaroute://` URL 打给本机
//!
//! 因此**必用确认框**（对外动作先确认，同本仓 lan_guard 纪律）；确认框显示**端点域名**让用户看清
//! 密钥会被发去哪（防钓鱼站诱导导入一个把密钥转走的端点）；解析失败/字段缺失一律整条不落。

use crate::error::{AppError, AppResult};
use crate::model::{CategoryType, ProviderKey, Protocol};
use crate::store::Store;
use serde::Serialize;
use std::sync::Mutex;

/// 解析后的一次导入请求（含明文密钥，**只在后端内存**）。
#[derive(Debug, Clone)]
pub(crate) struct ParsedImport {
    pub category: Option<CategoryType>,
    pub name: String,
    pub protocol: Option<Protocol>,
    /// 主端点（多端点时取首个）。
    pub base_url: String,
    /// 被忽略的额外端点（多端点时确认框如实注明）。
    pub extra_endpoints: Vec<String>,
    /// 明文密钥（可能为空 —— 那时确认框提示「未带密钥，导入后需手填」）。
    pub api_key: String,
    pub tier_haiku: Option<String>,
    pub tier_sonnet: Option<String>,
    pub tier_opus: Option<String>,
    pub tier_fable: Option<String>,
    /// 兜底默认模型（`default_model`）。中转站每条 Key 常只服务一个模型，带上它导入后即可直接用，
    /// 不必再手填。为空 → 走「三档 → 首个模型 → 透传」的既有兜底链。
    pub default_model: Option<String>,
    /// 请求超时（毫秒，落 `KeyParams.timeout_ms`）。为空 → 用全局默认 30000ms。
    /// 推理类中转常需更长（如 120000），带上它免得导入后被 30s 截断。
    pub timeout_ms: Option<u64>,
}

/// 交给前端确认框的预览：**不含密钥**，密钥只以 `has_secret` 体现。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ImportPreview {
    /// 已解析出的分类（serde 值，如 `claude-cli`）；`None` = URL 没带，确认框让用户选。
    pub category: Option<CategoryType>,
    pub name: String,
    /// 协议（serde 值，如 `openai_responses`）；`None` = URL 没带，确认框让用户选。
    pub protocol: Option<Protocol>,
    pub base_url: String,
    pub extra_endpoints: Vec<String>,
    pub has_secret: bool,
    pub tier_haiku: Option<String>,
    pub tier_sonnet: Option<String>,
    pub tier_opus: Option<String>,
    pub tier_fable: Option<String>,
}

impl ParsedImport {
    fn to_preview(&self) -> ImportPreview {
        ImportPreview {
            category: self.category,
            name: self.name.clone(),
            protocol: self.protocol,
            base_url: self.base_url.clone(),
            extra_endpoints: self.extra_endpoints.clone(),
            has_secret: !self.api_key.is_empty(),
            tier_haiku: self.tier_haiku.clone(),
            tier_sonnet: self.tier_sonnet.clone(),
            tier_opus: self.tier_opus.clone(),
            tier_fable: self.tier_fable.clone(),
        }
    }
}

/// 进程内待确认导入槽。深链接到达 → 存这里 → 前端确认 → 取出落盘。
/// 只保留**最近一次**：用户连点两个导入链接，以最后一个为准（旧的那次还没确认就作废，
/// 符合直觉 —— 屏幕上的确认框内容就是最后点的那个）。
static PENDING: Mutex<Option<ParsedImport>> = Mutex::new(None);

/// 解析 `synaroute://` URL 并存入待确认槽。返回**不含密钥**的预览供前端弹框。
///
/// 解析失败返回 `Err`（整条不落，不存半个）。这是深链接到达时的唯一入口
/// （冷启动的 `on_open_url` 与已运行的 single-instance 回调都走它）。
pub(crate) fn stage_import(url: &str) -> AppResult<ImportPreview> {
    let parsed = parse_import_url(url)?;
    let preview = parsed.to_preview();
    // 锁只护一个 Option 的替换，绝不跨 .await / 不做 IO，不会长持。
    *PENDING.lock().unwrap() = Some(parsed);
    Ok(preview)
}

/// 读回当前待确认导入的预览（前端确认框挂载时拉一次；无则 None）。
pub(crate) fn peek_pending() -> Option<ImportPreview> {
    PENDING.lock().unwrap().as_ref().map(ParsedImport::to_preview)
}

/// 用户点「取消」：丢弃待确认槽。
pub(crate) fn discard_pending() {
    *PENDING.lock().unwrap() = None;
}

/// 用户点「导入」：取出待确认项落盘。`category`/`protocol` 由前端传回
/// （URL 没带时用户在确认框选的；URL 带了则前端回传同一个值）。
///
/// 成功返回新建 Key 的 id。落盘走既有写路径（`save_key` + `save_secret`），
/// 主口令锁定时 `save_secret` 返 Err → 整条视为失败（fail closed）。
pub(crate) fn apply_pending(
    store: &Store,
    category: CategoryType,
    protocol: Protocol,
) -> AppResult<String> {
    // take：无论成功失败都消费掉这一次（失败时用户需重新点链接，不留脏槽）。
    let parsed = PENDING
        .lock()
        .unwrap()
        .take()
        .ok_or_else(|| AppError::Invalid("没有待确认的导入请求（可能已被取消或超时覆盖）".into()))?;

    let key = ProviderKey {
        id: String::new(), // 让 upsert_key 自生成
        category_id: category,
        name: if parsed.name.trim().is_empty() { "导入的 Key".into() } else { parsed.name.clone() },
        vendor: String::new(),
        base_url: parsed.base_url.clone(),
        protocol,
        has_secret: false, // 先建条，密钥由下面的 save_secret 落库后置 true
        enabled: true,     // 导入即启用（确认框已注明）
        allow_in_aggregate: false,
        priority: 999, // upsert_key 的碰撞规则会顶到 max+1（见 store.rs upsert_key 注释）
        headers_json: None,
        // 只带 timeout_ms（temperature/top_p 刻意不设 —— Responses 上游不收采样参数，见本仓
        // 「Codex 走 Responses 一律 400」那条：默认预填 1.0 就是那次的成因）。
        params: crate::model::KeyParams { timeout_ms: parsed.timeout_ms, ..Default::default() },
        models: Vec::new(),
        mappings: Vec::new(),
        default_model: parsed.default_model.clone(),
        allow_named_model_fallback: false,
        tier_haiku: parsed.tier_haiku.clone(),
        tier_sonnet: parsed.tier_sonnet.clone(),
        tier_opus: parsed.tier_opus.clone(),
        tier_fable: parsed.tier_fable.clone(),
        health: Default::default(),
        balance_query: None,
        cached_balance: None,
        cost_multiplier: None,
        budget_usd: None, // 深链接不带预算（URL 格式里没有这一项）；用户之后在 Key 编辑器里设
        icon: None,
    };

    // 先建条拿到 id。
    let saved = crate::service::save_key(store, key)?;
    let id = saved.id.clone();

    // 带密钥时落库（`save_secret` 内部：先写库成功再置 has_secret=true，顺序不可交换）。
    // 🔴 锁定态 `set` 返 Err → 这里 `?` 直接失败，Key 已建但 has_secret=false —— 与「用户
    // 新建 Key 后还没填密钥」是同一种既有中间态，用户在卡片上会看到「未配置密钥」，可自行补。
    // 不为「密钥没落成」去回滚删 Key：删条是更重的动作，而半条的表现是既有的、可自愈的。
    if !parsed.api_key.is_empty() {
        crate::service::save_secret(store, &id, &parsed.api_key)?;
    }

    // 落盘后刷新前端 Key 列表（同所有走 mutate_and_persist 的配置变更）。
    crate::events::emit(crate::events::Topic::Config, Some(category));
    Ok(id)
}

/// 处理一条到达的 `synaroute://` 深链接：解析并存进待确认槽 → 置前窗口 → 发事件让前端弹确认框。
///
/// 冷启动、macOS 已运行（`on_open_url`）、Windows 已运行（single-instance argv）三条路径都调它，
/// 保证「谁来的都走同一处理」（避免本仓反复踩的双路径分叉）。解析失败只记日志、不打扰用户
/// （深链接可能被人乱构造，弹一堆报错框反而是骚扰面）——真正的错误在确认框/落盘阶段如实报。
pub(crate) fn handle_deeplink(app: &tauri::AppHandle, url: &str) {
    match stage_import(url) {
        Ok(_) => {
            crate::show_main_window(app);
            // 只发信号，预览由前端 `deeplink_import_peek` 拉取（密钥不进事件，见本模块头）。
            crate::events::emit(crate::events::Topic::ImportRequest, None);
        }
        Err(e) => tracing::warn!("忽略一条无法解析的 synaroute:// 深链接：{e}"),
    }
}

// ============ IPC 命令（前端 bridge.ts 对齐） ============

/// 确认框挂载时拉取当前待确认的导入预览（**不含密钥**）；无则 None。
#[tauri::command]
pub fn deeplink_import_peek() -> Option<ImportPreview> {
    peek_pending()
}

/// 用户点「导入」：把待确认项落盘为一条 Key（含密钥）。category/protocol 由确认框传回。
#[tauri::command]
pub fn deeplink_import_apply(
    app: tauri::AppHandle,
    state: tauri::State<crate::AppState>,
    category: CategoryType,
    protocol: Protocol,
) -> AppResult<String> {
    let id = apply_pending(&state.store, category, protocol)?;
    // Key 变动 → 同步客户端模型清单（同 upsert_key 命令：走 resync 包一层）。
    crate::resync(&app, &state, Ok(id))
}

/// 用户点「取消」：丢弃待确认槽。
#[tauri::command]
pub fn deeplink_import_discard() {
    discard_pending();
}

/// 解析 `synaroute://v1/import?resource=provider&…`。
///
/// 形态对齐 cc-switch 的 `ccswitch://`（已取证其 deeplink/parser.rs），减少网页侧改造成本：
/// host = 版本（`v1`）、path = `/import`、query 带各字段。
fn parse_import_url(url_str: &str) -> AppResult<ParsedImport> {
    let url = url::Url::parse(url_str)
        .map_err(|e| AppError::Invalid(format!("深链接 URL 无法解析：{e}")))?;

    if url.scheme() != "synaroute" {
        return Err(AppError::Invalid(format!(
            "深链接协议应为 synaroute://，实际是 {}://",
            url.scheme()
        )));
    }
    // host = 版本。只认 v1（加新版本时在此显式放行，避免默默接受未知形态）。
    let version = url.host_str().unwrap_or_default();
    if version != "v1" {
        return Err(AppError::Invalid(format!("不支持的深链接版本：{version}（应为 v1）")));
    }
    if url.path() != "/import" {
        return Err(AppError::Invalid(format!("深链接路径应为 /import，实际是 {}", url.path())));
    }

    let params: std::collections::HashMap<String, String> =
        url.query_pairs().into_owned().collect();

    match params.get("resource").map(String::as_str) {
        Some("provider") => {}
        Some(other) => {
            return Err(AppError::Invalid(format!("暂不支持导入 {other}，本版本只支持 provider")))
        }
        None => return Err(AppError::Invalid("深链接缺少 resource 参数".into())),
    }

    // category / protocol 都是可选：URL 没带时留 None，确认框让用户选（不猜）。
    let category = match params.get("category") {
        Some(s) => Some(parse_category(s)?),
        None => None,
    };
    let protocol = match params.get("protocol") {
        Some(s) => Some(parse_protocol(s)?),
        None => None,
    };

    let name = params.get("name").cloned().unwrap_or_default();

    // endpoint 支持逗号分隔多个（cc-switch 语义：首个为主）。SynaRoute 单 base_url，
    // 只取首个、其余记入 extra_endpoints 供确认框注明。每个都必须是合法 http(s)。
    let endpoints: Vec<String> = params
        .get("endpoint")
        .map(|e| e.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect())
        .unwrap_or_default();
    let base_url = endpoints
        .first()
        .cloned()
        .ok_or_else(|| AppError::Invalid("深链接缺少 endpoint 参数".into()))?;
    for ep in &endpoints {
        validate_http_url(ep)?;
    }
    let extra_endpoints = endpoints.into_iter().skip(1).collect();

    // apiKey：base64 编码（容错 + / URL-safe / 缺 padding，照抄 cc-switch 思路）。
    // 允许缺省（未带密钥 → 导入后手填）。
    let api_key = match params.get("apiKey") {
        Some(raw) if !raw.is_empty() => decode_base64_key(raw)?,
        _ => String::new(),
    };

    // timeoutMs：可选毫秒。非法（非数字 / 0）一律整条拒绝，同其它字段的 fail-hard 口径——
    // 静默忽略会让用户以为设进去了，然后去查中转站/网络。
    let timeout_ms = match params.get("timeoutMs").map(String::as_str) {
        None | Some("") => None,
        Some(s) => Some(
            s.parse::<u64>()
                .ok()
                .filter(|&n| n > 0)
                .ok_or_else(|| AppError::Invalid(format!("timeoutMs 非法：{s}（应为正整数毫秒）")))?,
        ),
    };

    Ok(ParsedImport {
        category,
        name,
        protocol,
        base_url,
        extra_endpoints,
        api_key,
        tier_haiku: params.get("haikuModel").cloned().filter(|s| !s.is_empty()),
        tier_sonnet: params.get("sonnetModel").cloned().filter(|s| !s.is_empty()),
        tier_opus: params.get("opusModel").cloned().filter(|s| !s.is_empty()),
        tier_fable: params.get("fableModel").cloned().filter(|s| !s.is_empty()),
        default_model: params.get("defaultModel").cloned().filter(|s| !s.is_empty()),
        timeout_ms,
    })
}

fn parse_category(s: &str) -> AppResult<CategoryType> {
    // 借 serde 的 rename 表（claude-cli/claude-desktop/codex），保持单一事实来源。
    serde_json::from_value::<CategoryType>(serde_json::Value::String(s.to_string()))
        .map_err(|_| AppError::Invalid(format!("未知分类：{s}（应为 claude-cli / claude-desktop / codex）")))
}

fn parse_protocol(s: &str) -> AppResult<Protocol> {
    // 借 serde（含 `openai` → OpenaiChat 的 alias），与 model.rs 单一事实来源。
    serde_json::from_value::<Protocol>(serde_json::Value::String(s.to_string()))
        .map_err(|_| AppError::Invalid(format!("未知协议：{s}（应为 anthropic / openai_chat / openai_responses）")))
}

fn validate_http_url(s: &str) -> AppResult<()> {
    let u = url::Url::parse(s).map_err(|e| AppError::Invalid(format!("端点 URL 非法：{s}（{e}）")))?;
    if u.scheme() != "http" && u.scheme() != "https" {
        return Err(AppError::Invalid(format!("端点 URL 必须是 http(s)：{s}")));
    }
    Ok(())
}

/// base64 解码密钥，容错常见的 URL 形态问题（照抄 cc-switch decode_base64_param 思路）：
/// URL 里 `+` 常被解成空格 → 先把空格还原成 `+`；缺 padding → 补 `=`；标准与 URL-safe 都试。
fn decode_base64_key(raw: &str) -> AppResult<String> {
    use base64::Engine;
    let trimmed = raw.trim_matches(|c| c == '\r' || c == '\n');
    let mut candidates: Vec<String> = Vec::new();
    if trimmed.contains(' ') {
        candidates.push(trimmed.replace(' ', "+"));
    }
    candidates.push(trimmed.to_string());
    // 补 padding 变体。
    for c in candidates.clone() {
        let rem = c.len() % 4;
        if rem != 0 {
            let mut p = c.clone();
            // 不用 `repeat_n`（Rust 1.82，本仓 MSRV 1.77，clippy incompatible_msrv 拦）。
            p.push_str(&"=".repeat(4 - rem));
            candidates.push(p);
        }
    }
    for c in &candidates {
        for engine in [
            &base64::engine::general_purpose::STANDARD,
            &base64::engine::general_purpose::STANDARD_NO_PAD,
        ] {
            if let Ok(bytes) = engine.decode(c) {
                if let Ok(s) = String::from_utf8(bytes) {
                    return Ok(s);
                }
            }
        }
        for engine in [
            &base64::engine::general_purpose::URL_SAFE,
            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        ] {
            if let Ok(bytes) = engine.decode(c) {
                if let Ok(s) = String::from_utf8(bytes) {
                    return Ok(s);
                }
            }
        }
    }
    Err(AppError::Invalid(
        "apiKey 参数 base64 解码失败：请确认已 base64 编码并做 URL 转义（尤其 `+` 编码为 %2B，或用 URL-safe base64）".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    fn b64(s: &str) -> String {
        base64::engine::general_purpose::STANDARD.encode(s)
    }

    /// 完整合法链接：各字段都解析出来、密钥 base64 解回明文、tier 映射到位。
    #[test]
    fn a_full_valid_url_parses_every_field() {
        let key_b64 = b64("sk-real-secret-123");
        let url = format!(
            "synaroute://v1/import?resource=provider&category=claude-cli&name=Sub2API\
             &protocol=openai_responses&endpoint=https://sub.100xlabs.space/v1&apiKey={key_b64}\
             &opusModel=glm-4.6&haikuModel=glm-4.5-air&defaultModel=glm-4.6&timeoutMs=120000"
        );
        let p = parse_import_url(&url).expect("合法链接必须解析成功");
        assert_eq!(p.category, Some(CategoryType::ClaudeCli));
        assert_eq!(p.protocol, Some(Protocol::OpenaiResponses));
        assert_eq!(p.name, "Sub2API");
        assert_eq!(p.base_url, "https://sub.100xlabs.space/v1");
        assert_eq!(p.api_key, "sk-real-secret-123", "密钥必须 base64 解回明文");
        assert_eq!(p.tier_opus.as_deref(), Some("glm-4.6"));
        assert_eq!(p.tier_haiku.as_deref(), Some("glm-4.5-air"));
        assert!(p.tier_sonnet.is_none(), "没带的 tier 必须是 None，不是空串");
        assert_eq!(p.default_model.as_deref(), Some("glm-4.6"), "defaultModel 必须解析");
        assert_eq!(p.timeout_ms, Some(120000), "timeoutMs 必须解析成 u64");
        // 预览绝不含密钥，只以 has_secret 体现。
        let pv = p.to_preview();
        assert!(pv.has_secret);
    }

    /// category/protocol 可选：URL 没带时留 None（确认框让用户选），不猜。
    #[test]
    fn missing_category_and_protocol_stay_none_not_guessed() {
        let url = "synaroute://v1/import?resource=provider&name=X&endpoint=https://a.test/v1";
        let p = parse_import_url(url).expect("缺 category/protocol 仍应解析成功");
        assert!(p.category.is_none(), "没带 category → None，交给确认框");
        assert!(p.protocol.is_none(), "没带 protocol → None，交给确认框");
    }

    /// 多端点：只取首个作 base_url，其余记进 extra_endpoints 供确认框注明。
    #[test]
    fn comma_separated_endpoints_keep_only_the_first() {
        let url = "synaroute://v1/import?resource=provider&name=X&protocol=anthropic\
                   &endpoint=https://a.test/v1,https://b.test/v1,https://c.test/v1";
        let p = parse_import_url(url).unwrap();
        assert_eq!(p.base_url, "https://a.test/v1");
        assert_eq!(p.extra_endpoints, vec!["https://b.test/v1", "https://c.test/v1"]);
    }

    /// base64 的 URL 变体都要认：`+`→空格、缺 padding、URL-safe。
    #[test]
    fn base64_url_variants_all_decode() {
        // 含会产生 `+` 与 `/` 的字节，制造需要容错的形态。
        let raw = "sk-aa+bb/cc==dd";
        let std = base64::engine::general_purpose::STANDARD.encode(raw);
        // 模拟 URL 把 `+` 变成空格的形态。
        let spaced = std.replace('+', " ");
        assert_eq!(decode_base64_key(&spaced).unwrap(), raw, "`+`→空格 必须还原");
        // URL-safe 无 padding。
        let urlsafe = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw);
        assert_eq!(decode_base64_key(&urlsafe).unwrap(), raw, "URL-safe 无 padding 必须认");
    }

    /// 非法输入一律 Err、不落半条。
    #[test]
    fn malformed_urls_are_rejected() {
        // 错协议
        assert!(parse_import_url("ccswitch://v1/import?resource=provider&name=X&endpoint=https://a.test").is_err());
        // 错版本
        assert!(parse_import_url("synaroute://v2/import?resource=provider&name=X&endpoint=https://a.test").is_err());
        // 错路径
        assert!(parse_import_url("synaroute://v1/other?resource=provider&name=X&endpoint=https://a.test").is_err());
        // 缺 resource
        assert!(parse_import_url("synaroute://v1/import?name=X&endpoint=https://a.test").is_err());
        // 非 provider 资源
        assert!(parse_import_url("synaroute://v1/import?resource=prompt&name=X&endpoint=https://a.test").is_err());
        // 缺 endpoint
        assert!(parse_import_url("synaroute://v1/import?resource=provider&name=X").is_err());
        // endpoint 非 http(s)
        assert!(parse_import_url("synaroute://v1/import?resource=provider&name=X&endpoint=ftp://a.test").is_err());
        // 未知分类
        assert!(parse_import_url("synaroute://v1/import?resource=provider&category=weird&name=X&endpoint=https://a.test/v1").is_err());
        // timeoutMs 非数字
        assert!(parse_import_url("synaroute://v1/import?resource=provider&name=X&endpoint=https://a.test/v1&timeoutMs=soon").is_err());
        // timeoutMs 为 0（无意义，拒绝而非静默忽略）
        assert!(parse_import_url("synaroute://v1/import?resource=provider&name=X&endpoint=https://a.test/v1&timeoutMs=0").is_err());
    }

    /// 落盘：确认后建出 Key、密钥入库、可读回。
    #[test]
    fn apply_creates_a_key_with_secret() {
        let (store, dir) = crate::service::tests::temp_store("dl_apply");
        let key_b64 = b64("sk-landed-999");
        let url = format!(
            "synaroute://v1/import?resource=provider&name=W&endpoint=https://w.test/v1&apiKey={key_b64}\
             &defaultModel=glm-4.6&timeoutMs=120000"
        );
        stage_import(&url).unwrap();
        let id = apply_pending(&store, CategoryType::Codex, Protocol::OpenaiResponses)
            .expect("确认后必须落盘成功");

        let keys = store.list_keys(CategoryType::Codex);
        let k = keys.iter().find(|k| k.id == id).expect("新 Key 必须出现在该分类");
        assert_eq!(k.base_url, "https://w.test/v1");
        assert!(k.enabled, "导入即启用");
        assert!(k.has_secret, "带了密钥 → has_secret 必须为真");
        assert_eq!(k.protocol, Protocol::OpenaiResponses);
        assert_eq!(k.default_model.as_deref(), Some("glm-4.6"), "defaultModel 必须落到 Key");
        assert_eq!(k.params.timeout_ms, Some(120000), "timeoutMs 必须落到 KeyParams");
        let secret = store.secrets.read().get(&id).unwrap();
        assert_eq!(secret.as_deref().map(|s| s.to_string()), Some("sk-landed-999".to_string()));
        // 用完即清：pending 槽应已空。
        assert!(peek_pending().is_none(), "apply 后待确认槽必须清空");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 🔴 fail closed：主口令锁定时密钥落库返 Err → 整个 apply 返 Err，不静默丢密钥。
    ///
    /// Key 本体可能已建（has_secret=false，与「新建后没填密钥」同一种可自愈中间态），
    /// 但**绝不能**返回 Ok 让用户以为密钥导进去了。
    #[test]
    fn a_locked_vault_fails_the_import_instead_of_dropping_the_secret() {
        let (store, dir) = crate::service::tests::temp_store("dl_locked");
        store.secrets.write().enable_master_password("TestPass123").unwrap();
        store.secrets.write().lock();
        assert!(store.secrets.read().is_locked(), "前置：锁定态");

        let key_b64 = b64("sk-should-not-land");
        let url = format!(
            "synaroute://v1/import?resource=provider&name=L&endpoint=https://l.test/v1&apiKey={key_b64}"
        );
        stage_import(&url).unwrap();
        let r = apply_pending(&store, CategoryType::ClaudeCli, Protocol::Anthropic);
        assert!(r.is_err(), "🔴 锁定态必须整条失败，不能静默把密钥丢掉后返回 Ok");
        let _ = std::fs::remove_dir_all(dir);
    }
}
