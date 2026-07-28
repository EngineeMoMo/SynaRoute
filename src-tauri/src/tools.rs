//! 目标工具接入模块 —— 把本地代理端点写入三个工具的真实配置文件。
//!
//! 硬规则（dev-hard-rules，用户强制要求）：
//! 1. 改写任何配置文件前，先备份为 *.synaroute.bak
//! 2. 原子写（临时文件 → 重命名替换）
//! 3. 路径全部动态解析（dirs / env），禁止硬编码本机路径
//!
//! 接入机制（三端严格分离，禁止混写）：
//! - **Claude CLI**：`~/.claude/settings.json`
//!   - 写：env.ANTHROPIC_BASE_URL / ANTHROPIC_AUTH_TOKEN(占位) / GATEWAY_MODEL_DISCOVERY
//!   - 写：env.ANTHROPIC_MODEL + 顶层 `model`（主 Key 首个可服务**对外名**；策略 A）
//!   - **不写** ANTHROPIC_DEFAULT_{HAIKU,SONNET,OPUS}_MODEL（避免 /model 三个 Custom 同名）
//!   - 应用时**删除** env 里残留的三档 DEFAULT_*（清 cc-switch/旧版写入）
//! - **Codex**：`~/.codex/config.toml` + `auth.json`（OpenAI 形态，无 ANTHROPIC_*）
//!   - 写：model_provider=synaroute、[model_providers.synaroute]、可选顶层 model、OPENAI_API_KEY 占位
//! - **Claude 桌面端**：切「第三方部署模式（deploymentMode=3p）」+ 预置 gateway 配置档
//!   （对齐 cc-switch）：写 `<Claude|Claude-3p>/claude_desktop_config.json` 的 deploymentMode、
//!   `<Claude-3p>/configLibrary/{ID}.json`（gateway 端点/占位 key/模型）与 `_meta.json`。
//!   凭据预填齐 → 桌面端跳过 get-started 登录。不写 CLI settings、不写 ANTHROPIC_*。

use crate::error::{AppError, AppResult};
use crate::model::CategoryType;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// 备份文件后缀
const BACKUP_SUFFIX: &str = "synaroute.bak";

/// Codex auth.json 的 `OPENAI_API_KEY` 占位值。接入时写入（Codex 需非空 key 才走鉴权流程），
/// 真实密钥由代理按路由 Key 注入、代理侧不校验此值。还原时用它识别「接入凭空新建的 auth.json」。
const CODEX_AUTH_PLACEHOLDER: &str = "synaroute-proxy";

/// SynaRoute 在 Claude 桌面端 `configLibrary` 里的专属配置档 ID。
/// 刻意区别于 cc-switch 的 `00000000-0000-4000-8000-000000157210`：两者可**共存**于
/// `_meta.entries`，`appliedId` 指向当前接入者。还原时只删本档、绝不动 cc-switch 的档。
/// 末段 `000053796e61` 是 "Syna"（S=0x53 y=0x79 n=0x6e a=0x61）的 hex，便于辨识。
const DESKTOP_PROFILE_ID: &str = "00000000-0000-4000-8000-000053796e61";
/// 本档在 `_meta.entries` 里显示的名称。
const DESKTOP_PROFILE_NAME: &str = "SynaRoute";
/// 桌面端 gateway 档的 `inferenceGatewayApiKey` 占位值（代理剥掉入站鉴权头、按路由 Key 注入
/// 真实密钥，故此值仅需非空以让桌面端走 bearer 鉴权流程，代理侧不校验）。
const DESKTOP_GATEWAY_PLACEHOLDER: &str = "synaroute-proxy";
/// 两个部署目录里的部署配置文件名。
const DESKTOP_CONFIG_FILE: &str = "claude_desktop_config.json";
/// 3p 目录下存放配置档的子目录名。
const DESKTOP_CONFIG_LIBRARY: &str = "configLibrary";
/// configLibrary 里的元数据文件名（登记各档 id/name 与当前 appliedId）。
const DESKTOP_META_FILE: &str = "_meta.json";

/// 将某分类的代理端点写入对应目标工具配置。返回人类可读的结果说明。
///
/// `models`：当前分类「主 Key」的可服务对外名列表（与 `/v1/models` 口径一致，有序）。
/// - **Claude CLI only**：取首个写 env.ANTHROPIC_MODEL + 顶层 `model`；并清除三档 DEFAULT_* 残留。
/// - **Codex only**：取首个写 config.toml 顶层 `model`（Responses 形态，与 Claude 字段无关）。
/// - **桌面端**：整份列表写进 gateway 档的 `inferenceModels`（切 3p 部署模式，见 apply_claude_desktop）。
pub fn apply(category: CategoryType, endpoint: &str, models: &[String]) -> AppResult<String> {
    let first = models.first().map(String::as_str);
    match category {
        CategoryType::ClaudeCli => apply_claude_cli(endpoint, first),
        CategoryType::Codex => apply_codex(endpoint, first),
        CategoryType::ClaudeDesktop => apply_claude_desktop(endpoint, models),
    }
}

// ---- Claude CLI ----

fn claude_cli_settings_path() -> AppResult<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| AppError::ToolConfig("无法定位用户目录".into()))?;
    Ok(home.join(".claude").join("settings.json"))
}

fn apply_claude_cli(endpoint: &str, default_model: Option<&str>) -> AppResult<String> {
    let path = claude_cli_settings_path()?;
    apply_claude_cli_at(&path, endpoint, default_model)
}

/// 可测入口：写入指定 settings.json（生产走 `claude_cli_settings_path`）。
fn apply_claude_cli_at(
    path: &Path,
    endpoint: &str,
    default_model: Option<&str>,
) -> AppResult<String> {
    let mut root = read_json_or_empty(path)?;

    // 确保 env 对象存在
    let env = root
        .as_object_mut()
        .ok_or_else(|| AppError::ToolConfig("settings.json 顶层非对象".into()))?
        .entry("env")
        .or_insert_with(|| Value::Object(Default::default()));
    let env_obj = env
        .as_object_mut()
        .ok_or_else(|| AppError::ToolConfig("env 非对象".into()))?;

    env_obj.insert("ANTHROPIC_BASE_URL".into(), Value::String(endpoint.to_string()));
    // 代理侧不校验 token，但工具要求存在，写占位
    env_obj
        .entry("ANTHROPIC_AUTH_TOKEN".to_string())
        .or_insert_with(|| Value::String("synaroute-proxy".into()));
    // 开启网关模型发现：Claude Code 默认不调用 <base>/v1/models，必须显式置 1 才会拉取代理
    // 暴露的可选模型填充 /model 选择器（需 CLI ≥ v2.1.129）。强制写 1，这正是 SynaRoute
    // 多 Key 路由要生效的前提。
    // CLI 只接受 id 以 "claude"/"anthropic" 开头的模型，其它静默过滤。代理侧对非合规名
    // （如 grok-4.5）自动包成 `claude-synaroute-<name>` 暴露，resolve 时再剥前缀；映射
    // 对外名若想出现在选择器里仍可直接写成 claude-*（无需包装）。
    env_obj.insert(
        "CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY".into(),
        Value::String("1".into()),
    );

    // 策略 A（用户拍板）：只写 ANTHROPIC_MODEL + 顶层 model（对外名），
    // 不写 ANTHROPIC_DEFAULT_{HAIKU,SONNET,OPUS}_MODEL —— 内置三档靠代理 resolve_model，
    // 避免 /model 出现三个「Custom * 都是同一个 id」。
    // 同时删除 env 里残留的三档 DEFAULT_*（旧版/cc-switch 可能写入）。
    for k in [
        "ANTHROPIC_DEFAULT_HAIKU_MODEL",
        "ANTHROPIC_DEFAULT_SONNET_MODEL",
        "ANTHROPIC_DEFAULT_OPUS_MODEL",
    ] {
        env_obj.remove(k);
    }

    let model_note = if let Some(m) = default_model.map(str::trim).filter(|s| !s.is_empty()) {
        env_obj.insert("ANTHROPIC_MODEL".into(), Value::String(m.to_string()));
        // 顶层 model：/model 当前默认；覆盖 claude-synaroute-* 等 Custom 残留
        if let Some(obj) = root.as_object_mut() {
            obj.insert("model".into(), Value::String(m.to_string()));
        }
        format!("，默认模型={m}（未写三档 DEFAULT_*）")
    } else {
        String::new()
    };

    backup_and_write_json(path, &root)?;
    Ok(format!(
        "已写入 Claude CLI 配置：{}（ANTHROPIC_BASE_URL={endpoint}{model_note}），原文件已备份",
        path.display()
    ))
}

// ---- Codex ----

fn codex_config_path() -> AppResult<PathBuf> {
    // 优先 CODEX_HOME 环境变量，回退 ~/.codex
    if let Ok(h) = std::env::var("CODEX_HOME") {
        return Ok(PathBuf::from(h).join("config.toml"));
    }
    let home = dirs::home_dir().ok_or_else(|| AppError::ToolConfig("无法定位用户目录".into()))?;
    Ok(home.join(".codex").join("config.toml"))
}

/// Codex 密钥文件 ~/.codex/auth.json（与 config.toml 同目录）。
/// Codex 从这里读 `OPENAI_API_KEY`（provider 的 env_key 未在真实环境变量中设置时）。
fn codex_auth_path() -> AppResult<PathBuf> {
    // 与 config.toml 同目录，保证 CODEX_HOME 覆盖时一起走。
    let cfg = codex_config_path()?;
    Ok(cfg.with_file_name("auth.json"))
}

fn apply_codex(endpoint: &str, default_model: Option<&str>) -> AppResult<String> {
    let path = codex_config_path()?;
    let auth_path = codex_auth_path()?;
    // Codex 接入要同时写两个文件（config.toml + auth.json）。用 with_rollback 快照两者，
    // 任一步失败就整体回滚，杜绝「config 已改但 auth 没写」的半配置状态（借鉴 cc-switch 的原子切换）。
    with_rollback(&[path.clone(), auth_path.clone()], || {
        let msg = apply_codex_at(&path, endpoint, default_model)?;
        // 借鉴 cc-switch：把占位密钥写进 auth.json 的 OPENAI_API_KEY，免去用户手设环境变量。
        // 代理侧不校验该值（真实密钥由代理按路由 Key 注入），但 Codex 需要一个非空 key 才走鉴权流程。
        write_codex_auth(&auth_path)?;
        Ok(format!(
            "{msg}\n已写入 {} 的 OPENAI_API_KEY 占位（无需手设环境变量）",
            auth_path.display()
        ))
    })
}

/// 写入 Codex auth.json 的 `OPENAI_API_KEY` 占位（接入需非空 key 才走鉴权流程）。
///
/// **整份替换为纯占位，不 merge 进原有字段**——关键修复：
/// ChatGPT OAuth 用户的 auth.json 含 `tokens.*`（access/refresh/id_token）且 `OPENAI_API_KEY` 为空。
/// 若把占位符 merge 进去，会造出「OAuth tokens + 占位 key」混合态：Codex 桌面端据此判定为 api-key 模式、
/// 不再认 ChatGPT 账号的 Codex 权限 → 账号门「You don't have access to Codex」。
/// 故这里整份替换为 `{OPENAI_API_KEY: 占位}`，让 Codex 干净地走 api-key 模式（经 synaroute provider 到本地代理）。
/// 原 OAuth 字段由 backup_and_write_bytes 完整存入 `.synaroute.bak`，停止代理还原时凭它恢复官方登录。
fn write_codex_auth(path: &Path) -> AppResult<()> {
    let root = read_json_or_empty(path)?;
    // 幂等/保护：已有任意非空 OPENAI_API_KEY（用户真实 api-key，或上次写的占位）则不动，
    // 避免覆盖用户真实 key、反复写盘、以及把已接入内容再拷进 .bak。
    if let Some(Value::String(existing)) = root.as_object().and_then(|o| o.get("OPENAI_API_KEY")) {
        if !existing.trim().is_empty() {
            return Ok(());
        }
    }
    let pure = serde_json::json!({ "OPENAI_API_KEY": CODEX_AUTH_PLACEHOLDER });
    backup_and_write_json(path, &pure)
}

/// 写入 Codex 的自定义 provider 指向本地代理。
///
/// 关键修复（此前 bug）：旧实现写 `shell_environment_policy.set.ANTHROPIC_BASE_URL`，
/// 但 Codex **不认** Anthropic 环境变量——它通过 `model_provider` + `[model_providers.<id>]`
/// 表配置上游（字段 base_url / wire_api / env_key，见 Codex 配置参考）。故旧写法对 Codex 完全无效。
///
/// 现改为写标准的自定义 provider：
/// - `[model_providers.synaroute]`：base_url = `{endpoint}/v1`（Codex 按 wire_api=responses
///   调 `{base_url}/responses`，本地代理已识别 `/v1/responses`）；wire_api="responses"（Codex 默认且唯一支持）。
/// - `model_provider = "synaroute"` 选中它。
/// - requires_openai_auth=true（借鉴 cc-switch）：走 OpenAI 风格鉴权、读同目录 auth.json 的
///   OPENAI_API_KEY（由 write_codex_auth 写占位），故**不写 env_key**——避免让 Codex 改从
///   环境变量读 key、重新引入「用户手设环境变量」负担。真实密钥仍由代理按路由 Key 注入。
///
/// 幂等：序列化结果与磁盘现有内容一致时，backup_and_write_bytes 会短路（不备份不写），
/// 故重复接入不会把已接入的 config 覆盖进 .synaroute.bak。保留 config.toml 其余表（mcp_servers 等）不动。
fn apply_codex_at(path: &Path, endpoint: &str, default_model: Option<&str>) -> AppResult<String> {
    let content = if path.exists() {
        std::fs::read_to_string(path)?
    } else {
        String::new()
    };

    let mut doc: toml::Value = if content.trim().is_empty() {
        toml::Value::Table(Default::default())
    } else {
        content
            .parse::<toml::Value>()
            .map_err(|e| AppError::ToolConfig(format!("解析 config.toml 失败: {e}")))?
    };

    let base_url = format!("{}/v1", endpoint.trim_end_matches('/'));
    let table = doc
        .as_table_mut()
        .ok_or_else(|| AppError::ToolConfig("config.toml 顶层非表".into()))?;

    // model_provider = "synaroute"
    table.insert(
        "model_provider".to_string(),
        toml::Value::String(MCP_CLIENT_NAME.to_string()),
    );

    // 默认模型（借鉴 cc-switch：写顶层 model 字段，让 Codex 启动即有模型，无需 /model 手选）。
    // 取该分类可服务模型集的首个（对外名，代理侧 resolve_model 会改写为上游真实名）。
    // 仅在能解析出模型时写入；解析不出则不动用户已有的 model 字段（避免清空）。
    if let Some(m) = default_model.map(|s| s.trim()).filter(|s| !s.is_empty()) {
        table.insert("model".to_string(), toml::Value::String(m.to_string()));
    }

    // [model_providers.synaroute]
    let providers = table
        .entry("model_providers".to_string())
        .or_insert_with(|| toml::Value::Table(Default::default()));
    let providers_table = providers
        .as_table_mut()
        .ok_or_else(|| AppError::ToolConfig("model_providers 非表".into()))?;
    let mut entry = toml::value::Table::new();
    entry.insert("name".into(), toml::Value::String("SynaRoute".into()));
    entry.insert("base_url".into(), toml::Value::String(base_url.clone()));
    entry.insert("wire_api".into(), toml::Value::String("responses".into()));
    // 鉴权走 auth.json 的 OPENAI_API_KEY（见 write_codex_auth），故**不写 env_key**：
    // env_key 会让 Codex 改从该名环境变量读 key，与 auth.json 冲突、且重新引入「用户手设环境变量」负担。
    // requires_openai_auth=true（借鉴 cc-switch 验证过的写法）让 Codex 走 OpenAI 风格鉴权、读 auth.json。
    entry.insert("requires_openai_auth".into(), toml::Value::Boolean(true));
    providers_table.insert(MCP_CLIENT_NAME.to_string(), toml::Value::Table(entry));

    let serialized =
        toml::to_string_pretty(&doc).map_err(|e| AppError::ToolConfig(e.to_string()))?;
    backup_and_write_bytes(path, serialized.as_bytes())?;
    let model_note = default_model
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|m| format!("，默认模型={m}"))
        .unwrap_or_default();
    Ok(format!(
        "已写入 Codex 配置：{}（model_provider=synaroute，base_url={base_url}，wire_api=responses{model_note}），原文件已备份",
        path.display()
    ))
}

// ---- Claude 桌面端 ----
//
// 桌面端不像 CLI 那样读 ANTHROPIC_BASE_URL —— 它有自己的「部署模式（deploymentMode）」概念：
// `1p`=官方后端（走 get-started 登录），`3p`=第三方 inference gateway（预置好凭据即跳过登录）。
// 早期实现往 `%APPDATA%\Roaming\Claude\claude_desktop_config.json` 写 `{"baseUrl":...}`——位置
// 与字段皆错（桌面端在 Windows 读 %LOCALAPPDATA%，且无 baseUrl 概念、启动时会用自己的
// preferences 覆盖该文件），故从未生效、桌面端始终停在 get-started。
//
// 现对齐 cc-switch 的真实机制（本机已有其生效样本作为字段/布局权威依据）：
// - `<Claude>/claude_desktop_config.json`      → 合并写 deploymentMode="3p"
// - `<Claude-3p>/claude_desktop_config.json`   → 合并写 deploymentMode="3p"（保留 preferences 等）
// - `<Claude-3p>/configLibrary/{ID}.json`      → gateway 档（inferenceProvider/BaseUrl/ApiKey/…）
// - `<Claude-3p>/configLibrary/_meta.json`     → entries[] 登记本档 + appliedId 指向本档
// 凭据（BaseUrl+占位 ApiKey+bearer）预填齐 → 桌面端认为环境已配好 → 跳过 get-started。
// 与 cc-switch 用独立 DESKTOP_PROFILE_ID **共存**：还原只动本档，绝不误删 cc-switch 的档。

/// 定位 Claude 桌面端的两个部署基目录：`normal`（官方，如 `Claude`）与 `threep`（第三方，如
/// `Claude-3p`）。Windows 用 `%LOCALAPPDATA%`，macOS/Linux 用 `~/Library/Application Support`
/// 等 `data_dir`。找不到精确名时扫描以 `Claude` 开头的目录兜底（区分是否带 `-3p` 后缀）。
fn claude_desktop_dirs() -> AppResult<(PathBuf, PathBuf)> {
    // 桌面端数据在 Windows 落 %LOCALAPPDATA%（与 CLI 的 %APPDATA% 不同！早期 bug 正源于此）。
    // 非 Windows 走 data_dir（macOS = ~/Library/Application Support）。
    #[cfg(windows)]
    let base = dirs::data_local_dir();
    #[cfg(not(windows))]
    let base = dirs::data_dir();
    let base = base.ok_or_else(|| AppError::ToolConfig("无法定位桌面端数据目录".into()))?;

    let normal = pick_desktop_dir(&base, false).unwrap_or_else(|| base.join("Claude"));
    let threep = pick_desktop_dir(&base, true).unwrap_or_else(|| base.join("Claude-3p"));
    Ok((normal, threep))
}

/// 在 `base` 下挑选桌面端目录：`want_3p=true` 找第三方目录（名以 `Claude` 开头且含 `-3p`），
/// `false` 找官方目录（以 `Claude` 开头且不含 `-3p`）。精确名（`Claude`/`Claude-3p`）优先；
/// 否则扫描现有目录取排序首个。都没有则返回 None（调用方回退到精确名）。
fn pick_desktop_dir(base: &Path, want_3p: bool) -> Option<PathBuf> {
    let exact = base.join(if want_3p { "Claude-3p" } else { "Claude" });
    if exact.is_dir() {
        return Some(exact);
    }
    let mut matches: Vec<PathBuf> = std::fs::read_dir(base)
        .ok()?
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| e.path())
        .filter(|p| {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            name.starts_with("Claude") && (name.contains("-3p") == want_3p)
        })
        .collect();
    matches.sort();
    matches.into_iter().next()
}

/// 3p 目录下的 gateway 配置档路径 `configLibrary/{DESKTOP_PROFILE_ID}.json`。
fn desktop_profile_path(threep: &Path) -> PathBuf {
    threep
        .join(DESKTOP_CONFIG_LIBRARY)
        .join(format!("{DESKTOP_PROFILE_ID}.json"))
}

/// 3p 目录下的元数据文件路径 `configLibrary/_meta.json`。
fn desktop_meta_path(threep: &Path) -> PathBuf {
    threep.join(DESKTOP_CONFIG_LIBRARY).join(DESKTOP_META_FILE)
}

fn apply_claude_desktop(endpoint: &str, models: &[String]) -> AppResult<String> {
    let (normal, threep) = claude_desktop_dirs()?;
    let normal_config = normal.join(DESKTOP_CONFIG_FILE);
    let threep_config = threep.join(DESKTOP_CONFIG_FILE);
    let profile = desktop_profile_path(&threep);
    let meta = desktop_meta_path(&threep);

    // 四个文件一次写完，任一步失败整体回滚（避免半配置：如 deploymentMode 已切 3p 但 gateway
    // 档没写 → 桌面端进 3p 却无凭据、反而卡死）。与 Codex 双文件接入同一套原子保证。
    with_rollback(
        &[
            normal_config.clone(),
            threep_config.clone(),
            profile.clone(),
            meta.clone(),
        ],
        || {
            apply_desktop_at(&normal_config, &threep_config, &profile, &meta, endpoint, models)
        },
    )
}

/// 可测入口：把 3p 部署模式与 gateway 档写入指定的四个文件。
fn apply_desktop_at(
    normal_config: &Path,
    threep_config: &Path,
    profile: &Path,
    meta: &Path,
    endpoint: &str,
    models: &[String],
) -> AppResult<String> {
    // 1) 两个部署配置：合并写 deploymentMode="3p"，保留 preferences 等既有键。
    write_deployment_mode(normal_config, "3p")?;
    write_deployment_mode(threep_config, "3p")?;

    // 2) gateway 档：预填端点 + 占位 key + bearer + 可选模型清单。
    let profile_json = build_gateway_profile(endpoint, models);
    backup_and_write_json(profile, &profile_json)?;

    // 3) _meta.json：登记本档（与 cc-switch 档共存）并把 appliedId 指向本档。
    write_desktop_meta_apply(meta)?;

    // 4) 清理早期失效实现的残留（写在 %APPDATA%\Roaming\Claude 的 baseUrl 及其 .bak）。
    // 尽力而为、不影响接入结果（在回滚集合外，失败仅告警）。
    cleanup_legacy_desktop_residue();

    Ok(format!(
        "已接入 Claude 桌面端（3p 部署模式）：{}，gateway 端点={endpoint}{}，原文件已备份。请重启桌面端生效。",
        threep_config.display(),
        if models.is_empty() {
            String::new()
        } else {
            format!("，模型={}", models.join("/"))
        }
    ))
}

/// 清理早期失效实现的残留：旧代码往 `%APPDATA%\Roaming\Claude\claude_desktop_config.json`
/// 写 `{"baseUrl":...}`（位置/字段皆错、从未生效），并留下同名 `.synaroute.bak`。此处尽力删除
/// 这两个文件——**仅当** config 内容确为旧实现的产物（顶层含 `baseUrl` 键）时才删，避免误伤
/// 桌面端在该路径可能另建的合法文件。`.bak` 只要存在即删（它是旧实现凭空造的备份）。
///
/// 全程 best-effort：任何一步失败只告警、不影响接入（本函数在 with_rollback 集合之外调用）。
fn cleanup_legacy_desktop_residue() {
    // 旧实现用 config_dir()（Windows=%APPDATA%\Roaming）→ Claude\claude_desktop_config.json。
    let Some(base) = dirs::config_dir() else { return };
    let legacy = base.join("Claude").join(DESKTOP_CONFIG_FILE);
    let legacy_bak = backup_path_for(&legacy);

    // config 仅在「确是旧 baseUrl 产物」时删：读出顶层含 baseUrl 键才动手。
    if legacy.exists() {
        let is_legacy = std::fs::read_to_string(&legacy)
            .ok()
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
            .and_then(|v| v.as_object().map(|o| o.contains_key("baseUrl")))
            .unwrap_or(false);
        if is_legacy {
            if let Err(e) = std::fs::remove_file(&legacy) {
                tracing::warn!("清理旧桌面端残留 {} 失败: {e}", legacy.display());
            }
        }
    }
    // .bak 是旧实现凭空造的（真桌面端不会建 .synaroute.bak），存在即删。
    if legacy_bak.exists() {
        if let Err(e) = std::fs::remove_file(&legacy_bak) {
            tracing::warn!("清理旧桌面端残留 {} 失败: {e}", legacy_bak.display());
        }
    }
}

/// 读-改-写某 config 的 `deploymentMode`，保留其它键（preferences 等）。文件/目录不存在时创建。
fn write_deployment_mode(path: &Path, mode: &str) -> AppResult<()> {
    let mut root = read_json_or_empty(path)?;
    let obj = root
        .as_object_mut()
        .ok_or_else(|| AppError::ToolConfig(format!("{} 顶层非对象", path.display())))?;
    obj.insert("deploymentMode".into(), Value::String(mode.to_string()));
    backup_and_write_json(path, &root)
}

/// 构造 gateway 配置档 JSON（对齐 cc-switch `build_gateway_profile`）。
/// `inferenceGatewayApiKey` 用占位（代理剥入站鉴权头、按路由 Key 注入真实密钥）；
/// `inferenceGatewayBaseUrl` 指向本地代理源（桌面端按 Anthropic 风格发 /v1/messages，代理已识别）。
/// `models` 非空时填 `inferenceModels`；含 `1m`/`-1m` 或超长上下文名不特判，统一按 supports1m=true
/// 暴露（对齐本机 cc-switch 样本：opus 系均标 supports1m）。
fn build_gateway_profile(endpoint: &str, models: &[String]) -> Value {
    let mut obj = serde_json::Map::new();
    obj.insert(
        "coworkEgressAllowedHosts".into(),
        Value::Array(vec![Value::String("*".into())]),
    );
    obj.insert("disableDeploymentModeChooser".into(), Value::Bool(true));
    obj.insert(
        "inferenceGatewayApiKey".into(),
        Value::String(DESKTOP_GATEWAY_PLACEHOLDER.into()),
    );
    obj.insert(
        "inferenceGatewayAuthScheme".into(),
        Value::String("bearer".into()),
    );
    obj.insert(
        "inferenceGatewayBaseUrl".into(),
        Value::String(endpoint.to_string()),
    );
    obj.insert("inferenceProvider".into(), Value::String("gateway".into()));
    if !models.is_empty() {
        let arr: Vec<Value> = models
            .iter()
            .map(|m| {
                let mut e = serde_json::Map::new();
                e.insert("name".into(), Value::String(m.clone()));
                e.insert("supports1m".into(), Value::Bool(true));
                Value::Object(e)
            })
            .collect();
        obj.insert("inferenceModels".into(), Value::Array(arr));
    }
    Value::Object(obj)
}

/// 接入时更新 `_meta.json`：确保 entries 里有本档（去重，与 cc-switch 档共存），appliedId 指向本档。
fn write_desktop_meta_apply(path: &Path) -> AppResult<()> {
    let mut root = read_json_or_empty(path)?;
    let obj = root
        .as_object_mut()
        .ok_or_else(|| AppError::ToolConfig("_meta.json 顶层非对象".into()))?;

    let entries = obj
        .entry("entries")
        .or_insert_with(|| Value::Array(Vec::new()));
    let arr = entries
        .as_array_mut()
        .ok_or_else(|| AppError::ToolConfig("_meta.entries 非数组".into()))?;
    // 去重：移除已存在的本档 entry，再重新追加（幂等）。不动其它档（cc-switch 的等）。
    arr.retain(|e| e.get("id").and_then(|v| v.as_str()) != Some(DESKTOP_PROFILE_ID));
    arr.push(serde_json::json!({
        "id": DESKTOP_PROFILE_ID,
        "name": DESKTOP_PROFILE_NAME,
    }));

    obj.insert(
        "appliedId".into(),
        Value::String(DESKTOP_PROFILE_ID.into()),
    );
    backup_and_write_json(path, &root)
}

// ---- MCP 客户端自动注册 ----
//
// 让「启用 MCP 开关」后无需用户手动 `claude mcp add`：后端直接把 synaroute 这台 HTTP MCP
// 服务器写进目标工具的客户端配置，用户重启客户端即可用。格式严格按官方：
// - Claude CLI：~/.claude.json 的 mcpServers.synaroute = { type:"http", url }
// - Codex：~/.codex/config.toml 的 [mcp_servers.synaroute] url = "..."
// 幂等：已存在同 url 则跳过写盘（不产生无谓备份 / 事件噪音）。

/// MCP 服务器在客户端配置里的固定名称（不随端口变化，故端口变了只需改 url）。
const MCP_CLIENT_NAME: &str = "synaroute";

/// Codex 顶层实验开关：启用支持 HTTP/streamable 传输的 rmcp MCP 客户端。
/// Codex 默认 MCP 客户端仅支持 stdio；不开此开关时 HTTP 型（url）MCP server 连不上
/// （表现为「命名空间空壳、tools/list 拿不到工具」）。接入 Codex 大脑聚合时自动写入。
/// Codex stdio MCP 的 args：`synaroute.exe --mcp-stdio`，进入无 UI 的 stdio JSON-RPC 模式。
const MCP_STDIO_FLAG: &str = "--mcp-stdio";

/// Codex stdio MCP 的每工具调用超时（秒），写进 `tool_timeout_sec`。默认 60s 不够大脑聚合
/// 跑完（多模型并行 + 决策者综合常 30s+，偶尔超 60s → `user cancelled MCP tool call`），放大到 600s。
const MCP_TOOL_TIMEOUT_SEC: i64 = 600;

/// MCP 单次工具调用超时（毫秒）的**兜底下限**，写进客户端配置的 `timeout` 字段。
///
/// SynaRoute 的多模型聚合天然慢（多模型并行 + 决策者二次调用）。Claude Code 对 HTTP MCP
/// 有两层超时：单次工具调用总时长、以及「首字节」per-request 计时器（默认 60s，请求超时
/// 不重试）。官方文档：把 server 的 `timeout` 设为 ≥60s 会**同时**抬高首字节计时器到该值。
///
/// 注意：此值仅作**下限兜底**。实际写入的客户端超时由 lib.rs 的 `mcp_client_timeout_ms`
/// 按「各分类整轮预算 total_timeout_ms 的最大值 + 余量」动态算出，并对本常量取 max——
/// 保证客户端超时始终 ≥ 服务端整轮预算 + 余量（服务端总在客户端杀连接前优雅降级返回），
/// 且不会比历史值（10 分钟）更短。
pub(crate) const MCP_TOOL_TIMEOUT_MS: u64 = 600_000;

/// Claude 全局配置 ~/.claude.json（Claude CLI 的 mcpServers 存放处）。
fn claude_json_path() -> AppResult<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| AppError::ToolConfig("无法定位用户目录".into()))?;
    Ok(home.join(".claude.json"))
}

/// 把 SynaRoute MCP 服务器注册进某分类对应工具的客户端配置。
/// `timeout_ms`：写入客户端的单次工具调用超时（由调用方按整轮预算联动算出，见
/// lib.rs `mcp_client_timeout_ms`）。返回 (人类可读结果, 是否实际写盘)。已是同 url+timeout
/// 时不写盘、返回 false。
pub fn register_mcp_client(category: CategoryType, mcp_url: &str, timeout_ms: u64) -> AppResult<(String, bool)> {
    match category {
        // 桌面端的 MCP 也读 ~/.claude.json（与 CLI 同源 mcpServers）。
        CategoryType::ClaudeCli | CategoryType::ClaudeDesktop => register_mcp_claude(mcp_url, timeout_ms),
        CategoryType::Codex => register_mcp_codex(mcp_url, timeout_ms),
    }
}

/// 从某分类对应工具的客户端配置移除 synaroute MCP 项（关闭开关时）。
pub fn unregister_mcp_client(category: CategoryType) -> AppResult<(String, bool)> {
    match category {
        CategoryType::ClaudeCli | CategoryType::ClaudeDesktop => unregister_mcp_claude(),
        CategoryType::Codex => unregister_mcp_codex(),
    }
}

/// 检测某分类对应工具的客户端配置里是否已注册 synaroute MCP（供配置预览显示接入状态）。
/// 读各端真实 MCP 客户端文件（Claude=~/.claude.json 的 mcpServers，Codex=config.toml 的
/// mcp_servers），只判存在性、不改盘。文件不存在或解析失败均视为未注册。
pub fn is_mcp_registered(category: CategoryType) -> bool {
    match category {
        CategoryType::ClaudeCli | CategoryType::ClaudeDesktop => {
            let Ok(path) = claude_json_path() else { return false };
            if !path.exists() {
                return false;
            }
            let Ok(raw) = std::fs::read_to_string(&path) else { return false };
            let Ok(v) = serde_json::from_str::<Value>(&raw) else { return false };
            v.get("mcpServers")
                .and_then(|s| s.get(MCP_CLIENT_NAME))
                .is_some()
        }
        CategoryType::Codex => {
            let Ok(path) = codex_config_path() else { return false };
            if !path.exists() {
                return false;
            }
            let Ok(raw) = std::fs::read_to_string(&path) else { return false };
            let Ok(v) = raw.parse::<toml::Value>() else { return false };
            v.get("mcp_servers")
                .and_then(|s| s.get(MCP_CLIENT_NAME))
                .is_some()
        }
    }
}

fn register_mcp_claude(mcp_url: &str, timeout_ms: u64) -> AppResult<(String, bool)> {
    register_mcp_claude_at(&claude_json_path()?, mcp_url, timeout_ms)
}

fn register_mcp_claude_at(path: &Path, mcp_url: &str, timeout_ms: u64) -> AppResult<(String, bool)> {
    let mut root = read_json_or_empty(path)?;
    let obj = root
        .as_object_mut()
        .ok_or_else(|| AppError::ToolConfig("~/.claude.json 顶层非对象".into()))?;

    let servers = obj
        .entry("mcpServers")
        .or_insert_with(|| Value::Object(Default::default()));
    let servers_obj = servers
        .as_object_mut()
        .ok_or_else(|| AppError::ToolConfig("mcpServers 非对象".into()))?;

    // 幂等：已存在且 url / type / timeout 全一致 → 不写盘。
    // 必须比对 timeout：否则老配置（无 timeout 或旧值）会因 url/type 匹配被判「已是最新」
    // 而永远升不上目标值，超时修复形同虚设。
    if let Some(existing) = servers_obj.get(MCP_CLIENT_NAME) {
        if existing.get("url").and_then(|u| u.as_str()) == Some(mcp_url)
            && existing.get("type").and_then(|t| t.as_str()) == Some("http")
            && existing.get("timeout").and_then(|t| t.as_u64()) == Some(timeout_ms)
        {
            return Ok((format!("Claude MCP 已是最新（{mcp_url}），跳过"), false));
        }
    }

    servers_obj.insert(
        MCP_CLIENT_NAME.to_string(),
        json_http_mcp(mcp_url, timeout_ms),
    );
    backup_and_write_json(path, &root)?;
    Ok((
        format!("已注册 MCP 到 Claude：{}（{mcp_url}），重启客户端生效", path.display()),
        true,
    ))
}

/// Claude 的 HTTP MCP 项：{ "type":"http", "url":"...", "timeout":<ms> }。
/// timeout（毫秒）：Claude Code 对 HTTP MCP 有两层超时——单次工具调用总时长、以及
/// 「首字节」per-request timer（默认 60s）。把 timeout 设为 ≥60s 会同时抬高首字节 timer 到该值
/// （见 code.claude.com/docs/en/mcp）。此值由调用方按整轮预算联动算出（见
/// lib.rs `mcp_client_timeout_ms`），保证 ≥ 服务端整轮预算 + 余量。
fn json_http_mcp(mcp_url: &str, timeout_ms: u64) -> Value {
    let mut m = serde_json::Map::new();
    m.insert("type".into(), Value::String("http".into()));
    m.insert("url".into(), Value::String(mcp_url.to_string()));
    m.insert("timeout".into(), Value::Number(timeout_ms.into()));
    Value::Object(m)
}

fn unregister_mcp_claude() -> AppResult<(String, bool)> {
    unregister_mcp_claude_at(&claude_json_path()?)
}

fn unregister_mcp_claude_at(path: &Path) -> AppResult<(String, bool)> {
    if !path.exists() {
        return Ok(("Claude 配置不存在，无需移除".into(), false));
    }
    let mut root = read_json_or_empty(path)?;
    let Some(obj) = root.as_object_mut() else {
        return Ok(("~/.claude.json 顶层非对象，跳过".into(), false));
    };
    let removed = obj
        .get_mut("mcpServers")
        .and_then(|s| s.as_object_mut())
        .map(|servers| servers.remove(MCP_CLIENT_NAME).is_some())
        .unwrap_or(false);
    if !removed {
        return Ok(("Claude 未注册 synaroute，无需移除".into(), false));
    }
    backup_and_write_json(path, &root)?;
    Ok((format!("已从 Claude 移除 MCP：{}", path.display()), true))
}

/// Codex 走 **stdio** MCP（而非 HTTP）：Codex 对 HTTP/streamable MCP 仅实验性支持
/// （需 experimental_use_rmcp_client、握手挑剔、易「空壳」），而 stdio 是其一等公民
/// （codegraph/sqlcl 等均为 stdio），稳定、无端口漂移、无首字节超时。故写成:
///   [mcp_servers.synaroute]
///   command = "<synaroute.exe 绝对路径>"
///   args = ["--mcp-stdio"]
/// 由 Codex 以子进程拉起 synaroute.exe --mcp-stdio，用 stdin/stdout 传 JSON-RPC。
/// `_timeout_ms` 于 stdio 不需要（无 HTTP 首字节超时），保留形参与 Claude 端签名一致。
fn register_mcp_codex(_mcp_url: &str, _timeout_ms: u64) -> AppResult<(String, bool)> {
    let exe = std::env::current_exe()
        .map_err(|e| AppError::ToolConfig(format!("无法定位 synaroute 可执行文件: {e}")))?;
    register_mcp_codex_at(&codex_config_path()?, &exe.display().to_string())
}

fn register_mcp_codex_at(path: &Path, exe_path: &str) -> AppResult<(String, bool)> {
    let content = if path.exists() {
        std::fs::read_to_string(path)?
    } else {
        String::new()
    };
    let mut doc: toml::Value = if content.trim().is_empty() {
        toml::Value::Table(Default::default())
    } else {
        content
            .parse::<toml::Value>()
            .map_err(|e| AppError::ToolConfig(format!("解析 config.toml 失败: {e}")))?
    };
    let table = doc
        .as_table_mut()
        .ok_or_else(|| AppError::ToolConfig("config.toml 顶层非表".into()))?;

    // 幂等：mcp_servers.synaroute 已是 stdio 形态（command 指向当前 exe、args=["--mcp-stdio"]）
    // → 跳过写盘。exe 路径变化（升级换目录）时会重写，保证 command 始终指向现役 exe。
    let already = table
        .get("mcp_servers")
        .and_then(|v| v.as_table())
        .and_then(|s| s.get(MCP_CLIENT_NAME))
        .and_then(|v| v.as_table())
        .map(|e| {
            e.get("command").and_then(|c| c.as_str()) == Some(exe_path)
                && e.get("args")
                    .and_then(|a| a.as_array())
                    .map(|a| a.len() == 1 && a[0].as_str() == Some(MCP_STDIO_FLAG))
                    .unwrap_or(false)
                // type="stdio" 必须纳入幂等：Codex 桌面端靠此字段识别 stdio MCP，缺了就不加载
                // 该 MCP（CLI 宽松、不需要也能识别，但桌面端严格）。老配置缺 type 时须重写补上，
                // 不能因 command/args 一致就判「已最新」跳过。
                && e.get("type").and_then(|t| t.as_str()) == Some("stdio")
                // tool_timeout_sec 同样纳入幂等：默认 60s 不够聚合跑（多模型并行+决策者常需 30s+，
                // 偶尔超 60s → `user cancelled MCP tool call`）。老配置缺此字段时须重写补上。
                && e.get("tool_timeout_sec").and_then(|t| t.as_integer()) == Some(MCP_TOOL_TIMEOUT_SEC)
        })
        .unwrap_or(false);
    if already {
        return Ok(("Codex MCP（stdio）已是最新，跳过".into(), false));
    }

    let servers = table
        .entry("mcp_servers".to_string())
        .or_insert_with(|| toml::Value::Table(Default::default()));
    let servers_table = servers
        .as_table_mut()
        .ok_or_else(|| AppError::ToolConfig("mcp_servers 非表".into()))?;

    // stdio transport：command + args + type。无 url、无 timeout、无 experimental 开关。
    // type="stdio" 不可省：Codex 桌面端靠它识别 stdio MCP（CLI 宽松、缺了也识别，但桌面端
    // 缺了就不加载该 MCP，表现为「对话里根本没有该工具」——与能用的 codegraph/sqlcl 的唯一差异）。
    let mut entry = toml::value::Table::new();
    entry.insert("command".to_string(), toml::Value::String(exe_path.to_string()));
    entry.insert(
        "args".to_string(),
        toml::Value::Array(vec![toml::Value::String(MCP_STDIO_FLAG.to_string())]),
    );
    entry.insert("type".to_string(), toml::Value::String("stdio".to_string()));
    // tool_timeout_sec：大脑聚合要跑多模型并行 + 决策者综合，常 30s+，偶尔更久。Codex 默认
    // 每工具调用超时仅 60s，不够 → 表现为「synaroute_ai started 后 user cancelled」。放大到 600s。
    // startup_timeout_sec：子进程启动握手超时，默认 10s，给足 30s 稳妥。
    entry.insert("tool_timeout_sec".to_string(), toml::Value::Integer(MCP_TOOL_TIMEOUT_SEC));
    entry.insert("startup_timeout_sec".to_string(), toml::Value::Integer(30));
    servers_table.insert(MCP_CLIENT_NAME.to_string(), toml::Value::Table(entry));

    let serialized =
        toml::to_string_pretty(&doc).map_err(|e| AppError::ToolConfig(e.to_string()))?;
    backup_and_write_bytes(path, serialized.as_bytes())?;
    Ok((
        format!("已接入大脑聚合到 Codex（stdio）：{}，重启 Codex 生效", path.display()),
        true,
    ))
}

fn unregister_mcp_codex() -> AppResult<(String, bool)> {
    unregister_mcp_codex_at(&codex_config_path()?)
}

fn unregister_mcp_codex_at(path: &Path) -> AppResult<(String, bool)> {
    if !path.exists() {
        return Ok(("Codex 配置不存在，无需移除".into(), false));
    }
    let content = std::fs::read_to_string(path)?;
    if content.trim().is_empty() {
        return Ok(("Codex 配置为空，无需移除".into(), false));
    }
    let mut doc: toml::Value = content
        .parse::<toml::Value>()
        .map_err(|e| AppError::ToolConfig(format!("解析 config.toml 失败: {e}")))?;
    let removed = doc
        .as_table_mut()
        .and_then(|t| t.get_mut("mcp_servers"))
        .and_then(|s| s.as_table_mut())
        .map(|servers| servers.remove(MCP_CLIENT_NAME).is_some())
        .unwrap_or(false);
    if !removed {
        return Ok(("Codex 未注册 synaroute，无需移除".into(), false));
    }
    let serialized =
        toml::to_string_pretty(&doc).map_err(|e| AppError::ToolConfig(e.to_string()))?;
    backup_and_write_bytes(path, serialized.as_bytes())?;
    Ok((format!("已从 Codex 移除 MCP：{}", path.display()), true))
}

// ---- 通用：备份 + 原子写 ----

fn read_json_or_empty(path: &Path) -> AppResult<Value> {
    if path.exists() {
        let raw = std::fs::read_to_string(path)?;
        if raw.trim().is_empty() {
            Ok(Value::Object(Default::default()))
        } else {
            serde_json::from_str(&raw)
                .map_err(|e| AppError::ToolConfig(format!("解析 {} 失败: {e}", path.display())))
        }
    } else {
        Ok(Value::Object(Default::default()))
    }
}

/// 备份原文件（若存在），然后原子写入新 JSON
fn backup_and_write_json(path: &Path, value: &Value) -> AppResult<()> {
    let data = serde_json::to_vec_pretty(value)?;
    backup_and_write_bytes(path, &data)
}

fn backup_and_write_bytes(path: &Path, data: &[u8]) -> AppResult<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // 幂等 + 保护备份：新内容与磁盘现有内容完全一致时，不备份也不写盘。
    // 关键：杜绝「重复接入」把已接入的内容再拷进 .synaroute.bak，冲掉用户接入前的原始备份
    // （否则 restore 只能还原出「已接入」态，官方 config / OAuth 登录再也回不来）。
    // 首次接入时新旧内容不同 → 正常备份原文件；此后再接入内容相同 → 直接跳过，.bak 保持原始态。
    if path.exists() {
        if let Ok(current) = std::fs::read(path) {
            if current == data {
                return Ok(());
            }
        }
    }
    // 规则1：改写前备份原文件
    if path.exists() {
        std::fs::copy(path, backup_path_for(path))?;
    }
    // 规则2：原子写
    crate::secret::atomic_write(path, data)
}

/// 多文件写入的整体回滚（借鉴 cc-switch 的 with_rollback）。
///
/// 场景：一次接入要动多个文件（如 Codex 的 config.toml + auth.json）。若中途某步失败，
/// 已写的文件会留下「半配置」状态。此辅助先对每个目标文件拍快照（内容 or「原本不存在」），
/// 执行闭包；闭包返回 Err 时把所有文件恢复到执行前的状态，避免部分写入。
///
/// 快照失败（无法读原文件）直接返回错误、不执行闭包——宁可不写，也不在无法回滚时冒险。
fn with_rollback<T>(
    paths: &[PathBuf],
    op: impl FnOnce() -> AppResult<T>,
) -> AppResult<T> {
    // 拍快照：Some(bytes)=原内容；None=原本不存在（回滚时应删除）。
    let mut snapshots: Vec<(PathBuf, Option<Vec<u8>>)> = Vec::with_capacity(paths.len());
    for p in paths {
        let snap = if p.exists() {
            Some(std::fs::read(p).map_err(|e| {
                AppError::ToolConfig(format!("回滚快照失败(读 {}): {e}", p.display()))
            })?)
        } else {
            None
        };
        snapshots.push((p.clone(), snap));
    }

    match op() {
        Ok(v) => Ok(v),
        Err(e) => {
            // 尽力回滚：逐个恢复原状。回滚本身的错误不覆盖原始错误，仅告警。
            for (p, snap) in &snapshots {
                let restored = match snap {
                    Some(bytes) => crate::secret::atomic_write(p, bytes),
                    None => std::fs::remove_file(p).map_err(AppError::from),
                };
                if let Err(re) = restored {
                    tracing::warn!("接入写入失败后回滚 {} 出错: {re}", p.display());
                }
            }
            Err(e)
        }
    }
}

/// 某文件对应的 `.synaroute.bak` 备份路径。
fn backup_path_for(path: &Path) -> PathBuf {
    path.with_extension(match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => format!("{ext}.{BACKUP_SUFFIX}"),
        None => BACKUP_SUFFIX.to_string(),
    })
}

/// 从 `.synaroute.bak` 还原单个文件；备份不存在则返回 false（跳过，不报错）。
fn restore_one(path: &Path) -> AppResult<bool> {
    let backup = backup_path_for(path);
    if !backup.exists() {
        return Ok(false);
    }
    let data = std::fs::read(&backup)?;
    crate::secret::atomic_write(path, &data)?;
    Ok(true)
}

/// auth.json 是否为「接入凭空新建的纯占位符文件」：顶层对象恰好只有 `OPENAI_API_KEY` 且值为占位符。
/// 用于还原时安全清理——严格要求无其它字段（尤其不能含 OAuth 的 `tokens` 段），杜绝误删真实凭证。
fn is_placeholder_only_auth(path: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(&text) else {
        return false;
    };
    obj.len() == 1
        && matches!(obj.get("OPENAI_API_KEY"), Some(Value::String(v)) if v == CODEX_AUTH_PLACEHOLDER)
}

/// 还原某工具配置（从 .synaroute.bak 恢复）。
///
/// 与接入对称：接入时 Codex 双文件写（config.toml + auth.json，见 apply_codex），
/// 故还原也必须双文件——否则断开后 config.toml 复原、auth.json 仍卡在占位符，
/// 用户官方登录（ChatGPT OAuth 令牌）不会自动回来。
///
/// 「无备份」不视为错误：还原由「停止代理」自动触发，从未接入过的分类本就没有 .bak，
/// 此时已处于接入前状态，返回成功（无需还原），避免每次停止都弹误报错。
/// Codex 的 auth.json 备份仅在存在时还原（用户接入前无 auth.json 则本就无此备份）。
pub fn restore(category: CategoryType) -> AppResult<String> {
    // 桌面端不是「从 .bak 还原单文件」那套：接入切了 deploymentMode=3p 并写了 gateway 档，
    // 还原须把两个 config 复位 1p、删本档 profile、从 _meta 摘掉本档并改 appliedId（镜像
    // cc-switch 的 restore）。故单独分派。
    if category == CategoryType::ClaudeDesktop {
        return restore_claude_desktop();
    }
    let path = match category {
        CategoryType::ClaudeCli => claude_cli_settings_path()?,
        CategoryType::Codex => codex_config_path()?,
        CategoryType::ClaudeDesktop => unreachable!("桌面端已在上方分派"),
    };
    let mut restored = Vec::new();
    if restore_one(&path)? {
        restored.push(path.display().to_string());
    }
    // Codex 接入会额外覆盖 auth.json（写占位 OPENAI_API_KEY，原 OAuth 令牌被拷入 .bak）。
    // 还原须一并把 auth.json 复原，用户官方登录才会立即恢复、无需重新登录。
    if category == CategoryType::Codex {
        let auth_path = codex_auth_path()?;
        if restore_one(&auth_path)? {
            restored.push(auth_path.display().to_string());
        } else if is_placeholder_only_auth(&auth_path) {
            // 无 .bak 说明接入前本无 auth.json（接入凭空建了纯占位符文件）。删除它，
            // 让用户回到接入前的「无 auth.json」态，避免残留占位符 key 干扰官方鉴权。
            // 严格守卫已确保只删纯占位符文件、不碰含 OAuth tokens 的真实凭证。
            std::fs::remove_file(&auth_path)?;
            restored.push(format!("{}（清除占位）", auth_path.display()));
        }
    }
    if restored.is_empty() {
        Ok("无备份，无需还原（未接入或已还原）".into())
    } else {
        Ok(format!("已从备份还原：{}", restored.join("、")))
    }
}

/// 断开 Claude 桌面端接入：把两个 config 的 deploymentMode 复位 `1p`、删除本档 gateway 文件、
/// 从 `_meta.json` 摘掉本档并把 appliedId 指向剩余首个（或删除）。镜像 cc-switch 的 restore。
///
/// 只动 SynaRoute 自己的档（DESKTOP_PROFILE_ID）——cc-switch 的档（若共存）原样保留，
/// 避免误删用户另一套接入。deploymentMode 复位为 1p 让桌面端回到官方后端（重新走登录）。
///
/// 「未接入」（无本档 profile、appliedId 也不是本档）不视为错误：还原由「停止代理」自动触发，
/// 返回成功、不弹误报。deploymentMode 仅在当前确由本档驱动（appliedId==本档）时才复位 1p——
/// 否则用户可能正用 cc-switch 的档，不能把人家踢回官方。
fn restore_claude_desktop() -> AppResult<String> {
    let (normal, threep) = claude_desktop_dirs()?;
    let normal_config = normal.join(DESKTOP_CONFIG_FILE);
    let threep_config = threep.join(DESKTOP_CONFIG_FILE);
    let profile = desktop_profile_path(&threep);
    let meta = desktop_meta_path(&threep);

    let mut done = Vec::new();

    // 仅当当前 appliedId 是本档时，才把部署模式复位 1p（让桌面端回官方登录）。
    // 若 appliedId 是别的档（如 cc-switch 的），说明用户当前在用那套，不能动其部署模式。
    let applied_is_ours = read_desktop_applied_id(&meta).as_deref() == Some(DESKTOP_PROFILE_ID);
    if applied_is_ours {
        write_deployment_mode(&normal_config, "1p")?;
        write_deployment_mode(&threep_config, "1p")?;
        done.push("deploymentMode→1p".to_string());
    }

    // 删本档 gateway 文件（存在才删）。
    if profile.exists() {
        std::fs::remove_file(&profile)?;
        done.push(format!("删除 {}", profile.display()));
    }

    // 从 _meta 摘掉本档，appliedId 若指向本档则改指剩余首个或移除。
    if write_desktop_meta_clear(&meta)? {
        done.push("_meta 清除本档".to_string());
    }

    if done.is_empty() {
        Ok("桌面端未接入 SynaRoute，无需还原".into())
    } else {
        Ok(format!("已断开 Claude 桌面端接入：{}。请重启桌面端生效。", done.join("、")))
    }
}

/// 读 `_meta.json` 的 `appliedId`（不存在/解析失败均返回 None）。
fn read_desktop_applied_id(meta: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(meta).ok()?;
    let v: Value = serde_json::from_str(&raw).ok()?;
    v.get("appliedId")
        .and_then(|a| a.as_str())
        .map(|s| s.to_string())
}

/// 还原时更新 `_meta.json`：移除本档 entry；若 appliedId 指向本档，则改指剩余 entries 首个
/// （无剩余则删除 appliedId 键）。返回是否实际改动。文件不存在视为无需改动（返回 false）。
fn write_desktop_meta_clear(meta: &Path) -> AppResult<bool> {
    if !meta.exists() {
        return Ok(false);
    }
    let mut root = read_json_or_empty(meta)?;
    let Some(obj) = root.as_object_mut() else {
        return Ok(false);
    };

    // 移除本档 entry。
    let mut changed = false;
    if let Some(arr) = obj.get_mut("entries").and_then(|e| e.as_array_mut()) {
        let before = arr.len();
        arr.retain(|e| e.get("id").and_then(|v| v.as_str()) != Some(DESKTOP_PROFILE_ID));
        changed |= arr.len() != before;
    }

    // appliedId 若指向本档：改指剩余首个 entry，或删除该键。
    if obj.get("appliedId").and_then(|a| a.as_str()) == Some(DESKTOP_PROFILE_ID) {
        let next = obj
            .get("entries")
            .and_then(|e| e.as_array())
            .and_then(|a| a.first())
            .and_then(|e| e.get("id"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        match next {
            Some(id) => {
                obj.insert("appliedId".into(), Value::String(id));
            }
            None => {
                obj.remove("appliedId");
            }
        }
        changed = true;
    }

    if changed {
        backup_and_write_json(meta, &root)?;
    }
    Ok(changed)
}

// ---- 只读预览（阶段 2：不编辑，只展示路径与脱敏正文）----

/// 某分类对应「目标工具」配置文件的只读快照。
/// 三端路径/格式不同：Claude CLI=settings.json；Codex=config.toml+auth.json；桌面=claude_desktop_config.json。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolConfigPreview {
    pub category_id: CategoryType,
    /// 人类可读说明：本分类写哪些文件、不写哪些
    pub summary: String,
    pub files: Vec<ToolConfigFilePreview>,
    /// 本分类是否已接入 MCP 大脑聚合（目标配置文件里已含 synaroute MCP 段）。
    /// 供前端「接入/移除大脑聚合」按钮判定当前态，与全局 mcp_enabled 开关无关。
    pub mcp_registered: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolConfigFilePreview {
    pub path: String,
    pub exists: bool,
    /// json | toml | text
    pub format: String,
    /// 脱敏后的文件正文；不存在时为 None
    pub content: Option<String>,
}

/// 读取当前分类工具配置的只读预览（token 脱敏，不修改磁盘）。
pub fn preview(category: CategoryType) -> AppResult<ToolConfigPreview> {
    match category {
        CategoryType::ClaudeCli => preview_claude_cli(),
        CategoryType::Codex => preview_codex(),
        CategoryType::ClaudeDesktop => preview_claude_desktop(),
    }
}

fn preview_claude_cli() -> AppResult<ToolConfigPreview> {
    let path = claude_cli_settings_path()?;
    let (exists, content) = read_preview_text(&path, true)?;
    Ok(ToolConfigPreview {
        category_id: CategoryType::ClaudeCli,
        summary: "Claude CLI：~/.claude/settings.json。写入 BASE_URL / AUTH_TOKEN(占位) / 发现开关 / ANTHROPIC_MODEL / 顶层 model；不写三档 DEFAULT_*，不写 Codex/桌面端文件。".into(),
        mcp_registered: is_mcp_registered(CategoryType::ClaudeCli),
        files: vec![ToolConfigFilePreview {
            path: path.display().to_string(),
            exists,
            format: "json".into(),
            content,
        }],
    })
}

fn preview_codex() -> AppResult<ToolConfigPreview> {
    let cfg = codex_config_path()?;
    let auth = codex_auth_path()?;
    let (cfg_exists, cfg_content) = read_preview_text(&cfg, false)?;
    let (auth_exists, auth_content) = read_preview_text(&auth, true)?;
    Ok(ToolConfigPreview {
        category_id: CategoryType::Codex,
        summary: "Codex：~/.codex/config.toml + auth.json。写入 model_provider=synaroute、[model_providers.synaroute]、可选 model、OPENAI_API_KEY 占位。不写任何 ANTHROPIC_* / settings.json。".into(),
        mcp_registered: is_mcp_registered(CategoryType::Codex),
        files: vec![
            ToolConfigFilePreview {
                path: cfg.display().to_string(),
                exists: cfg_exists,
                format: "toml".into(),
                content: cfg_content,
            },
            ToolConfigFilePreview {
                path: auth.display().to_string(),
                exists: auth_exists,
                format: "json".into(),
                content: auth_content,
            },
        ],
    })
}

fn preview_claude_desktop() -> AppResult<ToolConfigPreview> {
    // 桌面端接入落在 %LOCALAPPDATA% 的两个部署目录（非 CLI 的 %APPDATA%）：Claude 与 Claude-3p。
    // 预览展示真正生效的四个文件：两个 config（deploymentMode）+ 3p 目录的 gateway 档 + _meta。
    let (normal, threep) = claude_desktop_dirs()?;
    let normal_config = normal.join(DESKTOP_CONFIG_FILE);
    let threep_config = threep.join(DESKTOP_CONFIG_FILE);
    let profile = desktop_profile_path(&threep);
    let meta = desktop_meta_path(&threep);

    let mut files = Vec::new();
    for p in [&normal_config, &threep_config, &profile, &meta] {
        let (exists, content) = read_preview_text(p, true)?;
        files.push(ToolConfigFilePreview {
            path: p.display().to_string(),
            exists,
            format: "json".into(),
            content,
        });
    }
    Ok(ToolConfigPreview {
        category_id: CategoryType::ClaudeDesktop,
        summary: "Claude 桌面端（3p 部署模式）：两个 claude_desktop_config.json 写 deploymentMode=3p，Claude-3p/configLibrary 里写 gateway 档（inferenceGatewayBaseUrl 指向本机代理 + 占位 key + bearer + 可选模型）并登记进 _meta。凭据预填齐即跳过 get-started。与 cc-switch 用独立档共存。不写 CLI 的 settings.json。".into(),
        mcp_registered: is_mcp_registered(CategoryType::ClaudeDesktop),
        files,
    })
}

fn read_preview_text(path: &Path, redact_secrets: bool) -> AppResult<(bool, Option<String>)> {
    if !path.exists() {
        return Ok((false, None));
    }
    let raw = std::fs::read_to_string(path)
        .map_err(|e| AppError::ToolConfig(format!("读取 {} 失败: {e}", path.display())))?;
    let text = if redact_secrets {
        redact_config_secrets(&raw)
    } else {
        raw
    };
    // 预览截断：按 char 边界截，避免切在 UTF-8 多字节中间 panic
    const CAP: usize = 32_000;
    let text = if text.len() > CAP {
        let mut end = CAP;
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        format!(
            "{}…\n/* truncated {} bytes */",
            &text[..end],
            text.len() - end
        )
    } else {
        text
    };
    Ok((true, Some(text)))
}

/// 脱敏：避免预览面板泄露 token（不用 regex 依赖，按键名扫描 JSON/简单文本）。
fn redact_config_secrets(s: &str) -> String {
    let mut out = s.to_string();
    for key in [
        "ANTHROPIC_AUTH_TOKEN",
        "ANTHROPIC_API_KEY",
        "OPENAI_API_KEY",
        "api_key",
        "apiKey",
        // Codex 官方 ChatGPT OAuth 登录的令牌（auth.json 的 tokens.* 段）：均为 JWT，
        // 不带 sk- 前缀，故按键名单独脱敏，避免只读预览面板把访问/刷新令牌明文回传前端。
        "access_token",
        "refresh_token",
        "id_token",
        // Claude 桌面端 3p gateway 档的 API key（预览会读 configLibrary/{ID}.json）。
        "inferenceGatewayApiKey",
    ] {
        out = redact_json_string_field(&out, key);
    }
    // bare sk- tokens（含 "sk-" 在内至少 12 字符）。按字符边界扫描：不能用字节索引切片，
    // 否则配置里出现多字节 UTF-8（如中文路径）时 &s[i..i+3] 会切在字符中间 panic，
    // 且 `byte as char` 会把多字节序列拆成乱码。
    let mut result = String::with_capacity(out.len());
    let mut rest = out.as_str();
    while let Some(pos) = rest.find("sk-") {
        result.push_str(&rest[..pos]);
        let after = &rest[pos + 3..];
        // 连续的 token 字符（字母数字 / _ / -）长度，按字符边界累加。
        let tok_len: usize = after
            .char_indices()
            .take_while(|(_, c)| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
            .map(|(idx, c)| idx + c.len_utf8())
            .last()
            .unwrap_or(0);
        let total = 3 + tok_len; // 含 "sk-" 的完整 token 长度
        if total >= 12 {
            result.push_str("sk-***");
        } else {
            result.push_str(&rest[pos..pos + total]);
        }
        rest = &rest[pos + total..];
    }
    result.push_str(rest);
    result
}

/// 把 `"key": "...."` 的值换成 `***`（仅处理双引号 JSON 字段）。
fn redact_json_string_field(s: &str, key: &str) -> String {
    let needle = format!("\"{key}\"");
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(idx) = rest.find(&needle) {
        out.push_str(&rest[..idx]);
        out.push_str(&needle);
        let after_key = &rest[idx + needle.len()..];
        // skip whitespace + colon + whitespace + opening quote
        let mut chars = after_key.char_indices().peekable();
        let mut pos = 0;
        // copy whitespace
        while let Some(&(i, c)) = chars.peek() {
            if c.is_whitespace() {
                pos = i + c.len_utf8();
                chars.next();
            } else {
                break;
            }
        }
        out.push_str(&after_key[..pos]);
        let after_ws = &after_key[pos..];
        if !after_ws.starts_with(':') {
            rest = after_key;
            continue;
        }
        out.push(':');
        let after_colon = &after_ws[1..];
        let mut p2 = 0;
        let mut c2 = after_colon.char_indices().peekable();
        while let Some(&(i, c)) = c2.peek() {
            if c.is_whitespace() {
                p2 = i + c.len_utf8();
                c2.next();
            } else {
                break;
            }
        }
        out.push_str(&after_colon[..p2]);
        let after_ws2 = &after_colon[p2..];
        if !after_ws2.starts_with('"') {
            rest = after_colon;
            continue;
        }
        // find closing quote (no escape handling for simplicity — secrets rarely have \")
        if let Some(end) = after_ws2[1..].find('"') {
            out.push_str("\"***\"");
            rest = &after_ws2[1 + end + 1..];
        } else {
            out.push_str(after_ws2);
            rest = "";
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn temp_file(tag: &str, name: &str) -> PathBuf {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir()
            .join(format!("synaroute_tools_test_{}_{}_{}", tag, std::process::id(), seq));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[test]
    fn claude_register_writes_http_entry_and_is_idempotent() {
        let path = temp_file("claude_reg", ".claude.json");
        let url = "http://127.0.0.1:9527/mcp";

        // 首次注册：写盘
        let (_, wrote) = register_mcp_claude_at(&path, url, MCP_TOOL_TIMEOUT_MS).unwrap();
        assert!(wrote, "首次注册应写盘");

        let v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let entry = &v["mcpServers"]["synaroute"];
        assert_eq!(entry["type"], "http", "必须是官方 http transport");
        assert_eq!(entry["url"], url, "url 应写入");
        assert_eq!(entry["timeout"], 600000, "必须写入 timeout 抬高客户端首字节超时，避免聚合被判超时");

        // 再次相同 url：幂等，不写盘
        let (_, wrote2) = register_mcp_claude_at(&path, url, MCP_TOOL_TIMEOUT_MS).unwrap();
        assert!(!wrote2, "相同 url 应跳过写盘");

        // 换端口：应重写
        let url2 = "http://127.0.0.1:9600/mcp";
        let (_, wrote3) = register_mcp_claude_at(&path, url2, MCP_TOOL_TIMEOUT_MS).unwrap();
        assert!(wrote3, "url 变化应写盘");
        let v2: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(v2["mcpServers"]["synaroute"]["url"], url2);

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn claude_register_writes_coupled_timeout_and_rewrites_on_change() {
        // 客户端超时联动：写入调用方算出的 timeout（= 整轮预算 + 余量），且 timeout 变化必须重写，
        // 否则用户调大整轮预算后客户端超时不跟随，聚合仍被客户端提前杀死。
        let path = temp_file("claude_timeout", ".claude.json");
        let url = "http://127.0.0.1:9527/mcp";

        // 用一个非默认的联动值（630000 = 600000 整轮 + 30000 余量）。
        let (_, wrote) = register_mcp_claude_at(&path, url, 630_000).unwrap();
        assert!(wrote);
        let v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(v["mcpServers"]["synaroute"]["timeout"], 630000, "应写入联动算出的 timeout");

        // 同 url 同 timeout：幂等。
        let (_, wrote2) = register_mcp_claude_at(&path, url, 630_000).unwrap();
        assert!(!wrote2, "url+timeout 都没变应跳过");

        // url 不变但 timeout 变大（用户调大整轮预算）：必须重写。
        let (_, wrote3) = register_mcp_claude_at(&path, url, 1_830_000).unwrap();
        assert!(wrote3, "timeout 变化应重写");
        let v2: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(v2["mcpServers"]["synaroute"]["timeout"], 1830000);

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn claude_register_preserves_existing_servers_and_keys() {
        let path = temp_file("claude_preserve", ".claude.json");
        // 预置已有 mcpServers 与其它顶层键，注册不能破坏它们。
        std::fs::write(
            &path,
            r#"{"numStartups":5,"mcpServers":{"other":{"type":"stdio","command":"x"}}}"#,
        )
        .unwrap();

        register_mcp_claude_at(&path, "http://127.0.0.1:9527/mcp", MCP_TOOL_TIMEOUT_MS).unwrap();
        let v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(v["numStartups"], 5, "其它顶层键应保留");
        assert_eq!(v["mcpServers"]["other"]["command"], "x", "已有 MCP 应保留");
        assert_eq!(v["mcpServers"]["synaroute"]["type"], "http");

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn claude_unregister_removes_only_synaroute() {
        let path = temp_file("claude_unreg", ".claude.json");
        std::fs::write(
            &path,
            r#"{"mcpServers":{"synaroute":{"type":"http","url":"u"},"other":{"type":"stdio","command":"x"}}}"#,
        )
        .unwrap();

        let (_, wrote) = unregister_mcp_claude_at(&path).unwrap();
        assert!(wrote);
        let v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(v["mcpServers"].get("synaroute").is_none(), "synaroute 应被移除");
        assert_eq!(v["mcpServers"]["other"]["command"], "x", "其它 MCP 应保留");

        // 再次移除：无操作
        let (_, wrote2) = unregister_mcp_claude_at(&path).unwrap();
        assert!(!wrote2, "已无 synaroute，应不写盘");

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn codex_register_writes_stdio_command_and_preserves_other_tables() {
        let path = temp_file("codex_reg", "config.toml");
        std::fs::write(
            &path,
            "model = \"gpt-5\"\n\n[mcp_servers.codegraph]\ncommand = \"codegraph\"\n",
        )
        .unwrap();
        let exe = "C:\\Program Files\\SynaRoute\\synaroute.exe";

        let (_, wrote) = register_mcp_codex_at(&path, exe).unwrap();
        assert!(wrote);

        let doc: toml::Value = std::fs::read_to_string(&path).unwrap().parse().unwrap();
        // stdio 形态：command 指向 exe、args=["--mcp-stdio"]，无 url/timeout/实验开关。
        assert_eq!(
            doc["mcp_servers"]["synaroute"]["command"].as_str(),
            Some(exe),
            "应写入 stdio command 指向 synaroute.exe"
        );
        assert_eq!(
            doc["mcp_servers"]["synaroute"]["args"][0].as_str(),
            Some(MCP_STDIO_FLAG),
            "args 应为 [--mcp-stdio]"
        );
        assert!(
            doc["mcp_servers"]["synaroute"].get("url").is_none(),
            "stdio 形态不应有 url"
        );
        assert_eq!(
            doc["mcp_servers"]["synaroute"]["type"].as_str(),
            Some("stdio"),
            "必须写 type=stdio（Codex 桌面端靠它识别 stdio MCP，缺了就不加载）"
        );
        assert_eq!(
            doc["mcp_servers"]["synaroute"]["tool_timeout_sec"].as_integer(),
            Some(MCP_TOOL_TIMEOUT_SEC),
            "必须写 tool_timeout_sec（默认 60s 不够聚合跑，会 user cancelled）"
        );
        assert_eq!(
            doc["mcp_servers"]["codegraph"]["command"].as_str(),
            Some("codegraph"),
            "已有 MCP 应保留"
        );
        assert_eq!(doc["model"].as_str(), Some("gpt-5"), "顶层键应保留");

        // 幂等：command/args 一致 → 跳过
        let (_, wrote2) = register_mcp_codex_at(&path, exe).unwrap();
        assert!(!wrote2, "相同 stdio 配置应跳过");

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    /// 回归：老配置是旧的 HTTP 形态（url+timeout+experimental 开关）。再次接入必须迁移成
    /// stdio 形态（command+args），不能因残留 url 而误判已最新——否则 Codex 仍连不上。
    #[test]
    fn codex_register_migrates_http_to_stdio() {
        let path = temp_file("codex_migrate", "config.toml");
        let exe = "C:\\Program Files\\SynaRoute\\synaroute.exe";
        // 预置：旧 HTTP 形态。
        std::fs::write(
            &path,
            "model = \"gpt-5\"\nexperimental_use_rmcp_client = true\n\n[mcp_servers.synaroute]\nurl = \"http://127.0.0.1:9530/mcp\"\ntimeout = 600000\n",
        )
        .unwrap();

        let (_, wrote) = register_mcp_codex_at(&path, exe).unwrap();
        assert!(wrote, "旧 HTTP 形态必须被重写为 stdio");

        let doc: toml::Value = std::fs::read_to_string(&path).unwrap().parse().unwrap();
        assert_eq!(
            doc["mcp_servers"]["synaroute"]["command"].as_str(),
            Some(exe),
            "应迁移为 stdio command"
        );
        assert!(
            doc["mcp_servers"]["synaroute"].get("url").is_none(),
            "旧 url 应被替换掉（stdio 条目整体覆盖）"
        );

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    /// 回归：老配置已是 stdio（command/args 都对）但**缺 type="stdio"**（首版接入按钮漏写）。
    /// 再次接入必须补上 type，不能因 command/args 一致就判「已最新」跳过——否则 Codex 桌面端
    /// 靠 type 识别 stdio MCP，缺了就不加载该工具（对话里根本没有 synaroute_ai）。
    #[test]
    fn codex_register_backfills_missing_type_stdio() {
        let path = temp_file("codex_type_backfill", "config.toml");
        let exe = "C:\\Program Files\\SynaRoute\\synaroute.exe";
        // 预置：stdio 形态但缺 type 字段。
        std::fs::write(
            &path,
            format!(
                "model = \"gpt-5\"\n\n[mcp_servers.synaroute]\ncommand = '{}'\nargs = [\"--mcp-stdio\"]\n",
                exe
            ),
        )
        .unwrap();

        let (_, wrote) = register_mcp_codex_at(&path, exe).unwrap();
        assert!(wrote, "缺 type 时即便 command/args 一致也必须重写补上");

        let doc: toml::Value = std::fs::read_to_string(&path).unwrap().parse().unwrap();
        assert_eq!(
            doc["mcp_servers"]["synaroute"]["type"].as_str(),
            Some("stdio"),
            "type=stdio 应被补上（Codex 桌面端识别 stdio MCP 的关键字段）"
        );

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn redact_json_string_field_masks_known_secrets() {
        let raw = r#"{"ANTHROPIC_AUTH_TOKEN":"sk-abc1234567890","OPENAI_API_KEY":"secret","model":"x"}"#;
        let out = redact_config_secrets(raw);
        assert!(out.contains(r#""ANTHROPIC_AUTH_TOKEN": "***""#) || out.contains(r#""ANTHROPIC_AUTH_TOKEN":"***""#));
        assert!(out.contains("***"));
        assert!(!out.contains("sk-abc1234567890"));
        assert!(!out.contains("secret"));
        assert!(out.contains(r#""model":"x""#) || out.contains(r#""model": "x""#));
    }

    #[test]
    fn redact_handles_non_ascii_without_panic() {
        // 配置常含中文路径/工作目录（本机 Windows 用户名即中文）。脱敏必须按字符边界扫描，
        // 不得因 sk- 扫描的字节切片切在多字节字符中间而 panic，且非 ASCII 不能被拆成乱码。
        let raw = r#"{"cwd":"C:\\Users\\莫海明\\项目","OPENAI_API_KEY":"sk-abcdefghijklmnop","note":"路径含中文🚀"}"#;
        let out = redact_config_secrets(raw);
        // 已知字段脱敏
        assert!(!out.contains("sk-abcdefghijklmnop"));
        assert!(out.contains("***"));
        // 非 ASCII 原样保留（未乱码）
        assert!(out.contains("莫海明"));
        assert!(out.contains("项目"));
        assert!(out.contains("路径含中文🚀"));
    }

    #[test]
    fn redact_bare_sk_token_only_when_long_enough() {
        // 短 sk- 串（不足 12 字符）不脱敏；达到阈值才脱敏。
        let short = redact_config_secrets("prefix sk-abc done");
        assert!(short.contains("sk-abc"), "短 token 应原样保留: {short}");
        let long = redact_config_secrets("prefix sk-abcdefghij done");
        assert!(long.contains("sk-***"));
        assert!(!long.contains("sk-abcdefghij"));
        assert!(long.contains("prefix ") && long.contains(" done"), "周边文本应保留");
    }

    #[test]
    fn preview_truncate_respects_utf8_boundary() {
        // 构造刚好跨 CAP 的多字节字符，截断不得 panic
        let mut s = "a".repeat(31_998);
        s.push('中'); // 3 bytes UTF-8
        s.push_str("tail");
        let path = temp_file("preview_utf8", "settings.json");
        std::fs::write(&path, &s).unwrap();
        let (exists, content) = read_preview_text(&path, false).unwrap();
        assert!(exists);
        let c = content.unwrap();
        assert!(c.contains("truncated"));
        // 必须是合法 UTF-8（unwrap 已保证）且不含 panic
        assert!(c.is_char_boundary(c.len()));
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn claude_cli_apply_overwrites_model_defaults_like_cc_switch() {
        // 策略 A：只写 ANTHROPIC_MODEL + 顶层 model；并清除三档 DEFAULT_* 残留。
        // 不写 DEFAULT_*，避免 /model 出现三个 Custom 同名。仅 Claude CLI 路径。
        let path = temp_file("claude_cli_model", "settings.json");
        std::fs::write(
            &path,
            r#"{
  "env": {
    "ANTHROPIC_BASE_URL": "http://old:1",
    "ANTHROPIC_MODEL": "claude-synaroute-grok-4.5",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL": "grok-4.5",
    "ANTHROPIC_DEFAULT_SONNET_MODEL": "grok-4.5",
    "ANTHROPIC_DEFAULT_OPUS_MODEL": "grok-4.5"
  },
  "model": "claude-synaroute-grok-4.5"
}"#,
        )
        .unwrap();

        let msg = apply_claude_cli_at(
            &path,
            "http://127.0.0.1:8788",
            Some("claude-opus-4-7"),
        )
        .unwrap();
        assert!(msg.contains("默认模型=claude-opus-4-7"));
        assert!(msg.contains("未写三档 DEFAULT_*"));

        let v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(v["model"], "claude-opus-4-7");
        assert_eq!(v["env"]["ANTHROPIC_BASE_URL"], "http://127.0.0.1:8788");
        assert_eq!(v["env"]["ANTHROPIC_MODEL"], "claude-opus-4-7");
        // 策略 A：必须清除三档 DEFAULT_*
        assert!(v["env"].get("ANTHROPIC_DEFAULT_HAIKU_MODEL").is_none());
        assert!(v["env"].get("ANTHROPIC_DEFAULT_SONNET_MODEL").is_none());
        assert!(v["env"].get("ANTHROPIC_DEFAULT_OPUS_MODEL").is_none());
        assert_eq!(v["env"]["CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY"], "1");
        assert_eq!(v["env"]["ANTHROPIC_AUTH_TOKEN"], "synaroute-proxy");

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn claude_cli_apply_skips_model_fields_when_no_default() {
        // 取不到可服务模型时，不碰用户已有 model / ANTHROPIC_MODEL；但仍清 DEFAULT_* 残留
        let path = temp_file("claude_cli_skip", "settings.json");
        std::fs::write(
            &path,
            r#"{"env":{"ANTHROPIC_MODEL":"keep-me","ANTHROPIC_DEFAULT_OPUS_MODEL":"stale"},"model":"keep-me"}"#,
        )
        .unwrap();

        apply_claude_cli_at(&path, "http://127.0.0.1:8788", None).unwrap();
        let v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(v["model"], "keep-me");
        assert_eq!(v["env"]["ANTHROPIC_MODEL"], "keep-me");
        assert_eq!(v["env"]["ANTHROPIC_BASE_URL"], "http://127.0.0.1:8788");
        assert!(
            v["env"].get("ANTHROPIC_DEFAULT_OPUS_MODEL").is_none(),
            "即无 default_model 也应清掉 DEFAULT_* 残留"
        );

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn codex_apply_writes_custom_provider_not_anthropic_env() {
        // 核心修复回归：Codex 不认 ANTHROPIC_BASE_URL，必须写标准自定义 provider。
        // Codex 路径不得写入任何 ANTHROPIC_*（与 Claude CLI 完全分离）。
        let path = temp_file("codex_apply", "config.toml");
        // 预置其它表，验证不被破坏。
        std::fs::write(
            &path,
            "model = \"gpt-5\"\n\n[mcp_servers.codegraph]\ncommand = \"codegraph\"\n",
        )
        .unwrap();

        let msg = apply_codex_at(&path, "http://127.0.0.1:8790", Some("claude-opus-4-8")).unwrap();
        assert!(msg.contains("model_provider=synaroute"));

        let doc: toml::Value = std::fs::read_to_string(&path).unwrap().parse().unwrap();
        // 选中自定义 provider
        assert_eq!(doc["model_provider"].as_str(), Some("synaroute"));
        // 默认模型写入顶层 model（借鉴 cc-switch，让 Codex 启动即有模型）
        assert_eq!(doc["model"].as_str(), Some("claude-opus-4-8"));
        // provider 定义正确：base_url 追加 /v1、wire_api=responses、requires_openai_auth=true
        let p = &doc["model_providers"]["synaroute"];
        assert_eq!(p["base_url"].as_str(), Some("http://127.0.0.1:8790/v1"));
        assert_eq!(p["wire_api"].as_str(), Some("responses"));
        assert_eq!(p["requires_openai_auth"].as_bool(), Some(true));
        // 鉴权走 auth.json，故不写 env_key（避免让 Codex 改从环境变量读、重新引入手设负担）
        assert!(p.get("env_key").is_none(), "不应写 env_key，鉴权走 auth.json");
        // 绝不能再写 ANTHROPIC_BASE_URL
        assert!(
            doc.get("shell_environment_policy").is_none(),
            "不应再写 shell_environment_policy.set.ANTHROPIC_BASE_URL"
        );
        // 其它表保留
        assert_eq!(doc["model_providers"].as_table().unwrap().len(), 1);
        assert_eq!(doc["mcp_servers"]["codegraph"]["command"].as_str(), Some("codegraph"));

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn codex_unregister_removes_only_synaroute() {
        let path = temp_file("codex_unreg", "config.toml");
        std::fs::write(
            &path,
            "[mcp_servers.synaroute]\nurl = \"u\"\n\n[mcp_servers.codegraph]\ncommand = \"codegraph\"\n",
        )
        .unwrap();

        let (_, wrote) = unregister_mcp_codex_at(&path).unwrap();
        assert!(wrote);
        let doc: toml::Value = std::fs::read_to_string(&path).unwrap().parse().unwrap();
        assert!(
            doc["mcp_servers"].get("synaroute").is_none(),
            "synaroute 应被移除"
        );
        assert!(
            doc["mcp_servers"].get("codegraph").is_some(),
            "其它 MCP 应保留"
        );

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn restore_one_recovers_from_backup() {
        // backup_and_write_bytes 先备份原文件、再写新内容；restore_one 应把原内容还原回来。
        let path = temp_file("restore_one", "auth.json");
        std::fs::write(&path, b"ORIGINAL_OAUTH").unwrap();

        // 模拟接入：备份原文件并写入占位内容。
        backup_and_write_bytes(&path, b"PLACEHOLDER").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"PLACEHOLDER");

        // 断开还原：应从 .synaroute.bak 拿回原内容。
        let ok = restore_one(&path).unwrap();
        assert!(ok, "备份存在应还原并返回 true");
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"ORIGINAL_OAUTH",
            "restore_one 应还原到接入前的原始内容"
        );

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn restore_one_skips_when_no_backup() {
        // 备份不存在（如用户接入前无 auth.json）：应返回 false 且不报错、不动目标文件。
        let path = temp_file("restore_none", "auth.json");
        std::fs::write(&path, b"UNTOUCHED").unwrap();

        let ok = restore_one(&path).unwrap();
        assert!(!ok, "无备份应返回 false");
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"UNTOUCHED",
            "无备份时不应改动目标文件"
        );

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn repeated_apply_does_not_clobber_backup() {
        // Q4 回归：重复接入（内容相同）不得把已接入内容再拷进 .bak，否则冲掉接入前的原始备份。
        let path = temp_file("no_clobber", "config.toml");
        std::fs::write(&path, b"OFFICIAL_CONFIG").unwrap();

        // 首次接入：原始官方内容进 .bak，写入「已接入」内容。
        backup_and_write_bytes(&path, b"SYNAROUTE_CONFIG").unwrap();
        let backup = backup_path_for(&path);
        assert_eq!(std::fs::read(&backup).unwrap(), b"OFFICIAL_CONFIG");

        // 二次接入（内容相同）：内容相等守卫应短路，.bak 必须仍是官方内容、不被覆盖。
        backup_and_write_bytes(&path, b"SYNAROUTE_CONFIG").unwrap();
        assert_eq!(
            std::fs::read(&backup).unwrap(),
            b"OFFICIAL_CONFIG",
            "重复接入不得把已接入内容覆盖进 .bak（否则官方配置备份永久丢失）"
        );

        // 还原应拿回官方内容。
        assert!(restore_one(&path).unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), b"OFFICIAL_CONFIG");

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn restore_removes_placeholder_only_auth_without_backup() {
        // Q3 回归：接入前无 auth.json → 接入凭空建纯占位符文件 → 无 .bak。
        // 还原应删除该占位符文件，让用户回到接入前「无 auth.json」态。
        let path = temp_file("ph_auth", "auth.json");
        std::fs::write(&path, br#"{"OPENAI_API_KEY":"synaroute-proxy"}"#).unwrap();

        assert!(
            is_placeholder_only_auth(&path),
            "纯占位符文件应被识别"
        );

        // 含 OAuth tokens 的真实凭证绝不能被识别为占位符（防误删）。
        let real = temp_file("real_auth", "auth.json");
        std::fs::write(
            &real,
            br#"{"OPENAI_API_KEY":"synaroute-proxy","tokens":{"access_token":"x"}}"#,
        )
        .unwrap();
        assert!(
            !is_placeholder_only_auth(&real),
            "含 tokens 的文件不得被当作纯占位符（否则会误删 OAuth 凭证）"
        );

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
        std::fs::remove_dir_all(real.parent().unwrap()).ok();
    }

    #[test]
    fn redact_masks_oauth_tokens() {
        // Q5 回归：ChatGPT OAuth 令牌（JWT，非 sk- 前缀）必须按键名脱敏，不得明文出现在预览里。
        let raw = r#"{"tokens":{"access_token":"eyJhbGciOiJ.SECRET.SIG","refresh_token":"rt-abc123","id_token":"eyJ.id.tok"}}"#;
        let out = redact_config_secrets(raw);
        assert!(!out.contains("eyJhbGciOiJ.SECRET.SIG"), "access_token 明文不得残留");
        assert!(!out.contains("rt-abc123"), "refresh_token 明文不得残留");
        assert!(!out.contains("eyJ.id.tok"), "id_token 明文不得残留");
        assert!(out.contains("***"), "应替换为脱敏占位");
    }

    #[test]
    fn write_codex_auth_replaces_oauth_with_pure_placeholder() {
        // ChatGPT OAuth 用户：auth.json 含 tokens.* 且 OPENAI_API_KEY 为空。
        // 接入应写「纯占位」（不 merge 进 tokens，避免 Codex 账号门混合态），OAuth 原文完整进 .bak。
        let path = temp_file("codex_auth_pure", "auth.json");
        std::fs::write(
            &path,
            br#"{"auth_mode":"chatgpt","tokens":{"access_token":"eyJ.a.b","refresh_token":"rt-1","id_token":"eyJ.c.d","account_id":"acc-1"}}"#,
        )
        .unwrap();

        write_codex_auth(&path).unwrap();

        // 活文件：纯占位，无任何 OAuth 残留。
        let live: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let obj = live.as_object().unwrap();
        assert_eq!(obj.len(), 1, "活 auth.json 应只剩纯占位");
        assert_eq!(obj["OPENAI_API_KEY"], Value::String(CODEX_AUTH_PLACEHOLDER.into()));
        assert!(live.get("tokens").is_none(), "活文件不得残留 OAuth tokens");

        // 备份：完整 OAuth，供还原恢复官方登录。
        let bak: Value =
            serde_json::from_slice(&std::fs::read(backup_path_for(&path)).unwrap()).unwrap();
        assert_eq!(bak["auth_mode"], Value::String("chatgpt".into()));
        assert!(bak["tokens"]["access_token"].is_string(), "备份应含完整 OAuth tokens");

        // 幂等：再次接入不动（占位已非空）。
        write_codex_auth(&path).unwrap();
        let live2: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(live2, live, "重复接入应幂等、不改动活文件");
        // .bak 仍是 OAuth（未被占位覆盖）。
        let bak2: Value =
            serde_json::from_slice(&std::fs::read(backup_path_for(&path)).unwrap()).unwrap();
        assert!(bak2["tokens"]["access_token"].is_string(), "幂等重入后 .bak 仍须是 OAuth");

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn write_codex_auth_preserves_real_api_key() {
        // api-key 用户：已有真实非空 OPENAI_API_KEY → 幂等不动，不覆盖用户真实 key。
        let path = temp_file("codex_auth_real", "auth.json");
        std::fs::write(&path, br#"{"OPENAI_API_KEY":"sk-real-user-key-value"}"#).unwrap();
        write_codex_auth(&path).unwrap();
        let live: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            live["OPENAI_API_KEY"],
            Value::String("sk-real-user-key-value".into()),
            "真实 api-key 不得被占位覆盖"
        );
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn with_rollback_restores_all_files_on_failure() {
        // 多文件写入中途失败：已改的文件应回滚到原内容，原本不存在的应被删除。
        let dir = std::env::temp_dir().join(format!(
            "synaroute_rollback_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let existing = dir.join("existing.json");
        let fresh = dir.join("fresh.json");
        std::fs::write(&existing, b"ORIGINAL").unwrap();
        // fresh 原本不存在

        let paths = vec![existing.clone(), fresh.clone()];
        let result: AppResult<()> = with_rollback(&paths, || {
            // 先改两个文件，再返回 Err，模拟「第二步失败」。
            crate::secret::atomic_write(&existing, b"MODIFIED")?;
            crate::secret::atomic_write(&fresh, b"NEW")?;
            Err(AppError::ToolConfig("模拟失败".into()))
        });

        assert!(result.is_err(), "闭包返回 Err 应上抛");
        // existing 回滚到原内容
        assert_eq!(
            std::fs::read(&existing).unwrap(),
            b"ORIGINAL",
            "已存在文件应回滚到原内容"
        );
        // fresh 原本不存在 → 应被删除
        assert!(!fresh.exists(), "原本不存在的文件应在回滚时被删除");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn with_rollback_keeps_writes_on_success() {
        // 闭包成功：所有写入应保留，不触发回滚。
        let dir = std::env::temp_dir().join(format!(
            "synaroute_rollback_ok_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("f.json");

        let paths = vec![f.clone()];
        let result: AppResult<()> = with_rollback(&paths, || {
            crate::secret::atomic_write(&f, b"WRITTEN")?;
            Ok(())
        });

        assert!(result.is_ok());
        assert_eq!(std::fs::read(&f).unwrap(), b"WRITTEN", "成功时写入应保留");

        std::fs::remove_dir_all(&dir).ok();
    }

    // ---- Claude 桌面端 3p 部署模式 ----

    /// 在临时目录里搭出 normal / 3p 两个部署目录，返回 (dir, normal_config, threep_config, profile, meta)。
    fn desktop_layout(tag: &str) -> (PathBuf, PathBuf, PathBuf, PathBuf, PathBuf) {
        let normal = temp_file(tag, DESKTOP_CONFIG_FILE);
        let dir = normal.parent().unwrap().to_path_buf();
        let threep_dir = dir.join("threep");
        std::fs::create_dir_all(&threep_dir).unwrap();
        let threep_config = threep_dir.join(DESKTOP_CONFIG_FILE);
        let profile = desktop_profile_path(&threep_dir);
        let meta = desktop_meta_path(&threep_dir);
        (dir, normal, threep_config, profile, meta)
    }

    #[test]
    fn desktop_apply_writes_3p_mode_and_gateway_profile() {
        let (dir, normal, threep, profile, meta) = desktop_layout("desktop_apply");
        // 预置 3p config 已有 preferences，验证合并写不丢它。
        std::fs::write(
            &threep,
            r#"{"preferences":{"remoteToolsDeviceName":"win-x"}}"#,
        )
        .unwrap();

        let models = vec!["claude-opus-4-8".to_string(), "claude-opus-5".to_string()];
        let msg = apply_desktop_at(
            &normal,
            &threep,
            &profile,
            &meta,
            "http://127.0.0.1:47102",
            &models,
        )
        .unwrap();
        assert!(msg.contains("3p 部署模式"));

        // 两个 config 都切 3p，preferences 保留。
        let n: Value = serde_json::from_slice(&std::fs::read(&normal).unwrap()).unwrap();
        assert_eq!(n["deploymentMode"], "3p");
        let t: Value = serde_json::from_slice(&std::fs::read(&threep).unwrap()).unwrap();
        assert_eq!(t["deploymentMode"], "3p");
        assert_eq!(
            t["preferences"]["remoteToolsDeviceName"], "win-x",
            "既有 preferences 必须保留"
        );

        // gateway 档字段齐全。
        let p: Value = serde_json::from_slice(&std::fs::read(&profile).unwrap()).unwrap();
        assert_eq!(p["inferenceProvider"], "gateway");
        assert_eq!(p["inferenceGatewayBaseUrl"], "http://127.0.0.1:47102");
        assert_eq!(p["inferenceGatewayAuthScheme"], "bearer");
        assert_eq!(p["inferenceGatewayApiKey"], DESKTOP_GATEWAY_PLACEHOLDER);
        assert_eq!(p["disableDeploymentModeChooser"], true);
        assert_eq!(p["coworkEgressAllowedHosts"][0], "*");
        // inferenceModels：数组、名字对、supports1m=true。
        assert_eq!(p["inferenceModels"][0]["name"], "claude-opus-4-8");
        assert_eq!(p["inferenceModels"][0]["supports1m"], true);
        assert_eq!(p["inferenceModels"][1]["name"], "claude-opus-5");

        // _meta：本档登记 + appliedId 指向本档。
        let m: Value = serde_json::from_slice(&std::fs::read(&meta).unwrap()).unwrap();
        assert_eq!(m["appliedId"], DESKTOP_PROFILE_ID);
        assert_eq!(m["entries"][0]["id"], DESKTOP_PROFILE_ID);
        assert_eq!(m["entries"][0]["name"], DESKTOP_PROFILE_NAME);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn desktop_apply_no_models_omits_inference_models() {
        let (dir, normal, threep, profile, meta) = desktop_layout("desktop_nomodels");
        apply_desktop_at(&normal, &threep, &profile, &meta, "http://127.0.0.1:1", &[]).unwrap();
        let p: Value = serde_json::from_slice(&std::fs::read(&profile).unwrap()).unwrap();
        assert!(
            p.get("inferenceModels").is_none(),
            "无模型时不应写 inferenceModels 键"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn desktop_apply_coexists_with_ccswitch_profile() {
        // _meta 已有 cc-switch 档 + appliedId 指向它：接入后两档共存，appliedId 改指本档，
        // cc-switch 的 entry 原样保留（不误删用户另一套接入）。
        let (dir, normal, threep, profile, meta) = desktop_layout("desktop_coexist");
        let ccswitch_id = "00000000-0000-4000-8000-000000157210";
        std::fs::write(
            &meta,
            format!(
                r#"{{"appliedId":"{ccswitch_id}","entries":[{{"id":"{ccswitch_id}","name":"CC Switch"}}]}}"#
            ),
        )
        .unwrap();

        apply_desktop_at(&normal, &threep, &profile, &meta, "http://127.0.0.1:1", &[]).unwrap();

        let m: Value = serde_json::from_slice(&std::fs::read(&meta).unwrap()).unwrap();
        assert_eq!(m["appliedId"], DESKTOP_PROFILE_ID, "appliedId 应改指本档");
        let entries = m["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 2, "两档共存");
        assert!(
            entries.iter().any(|e| e["id"] == ccswitch_id),
            "cc-switch 档必须保留（不误删）"
        );
        assert!(entries.iter().any(|e| e["id"] == DESKTOP_PROFILE_ID));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn desktop_apply_idempotent_no_duplicate_entry() {
        // 重复接入：entries 里本档不重复出现（去重）。
        let (dir, normal, threep, profile, meta) = desktop_layout("desktop_idem");
        for _ in 0..3 {
            apply_desktop_at(&normal, &threep, &profile, &meta, "http://127.0.0.1:1", &[]).unwrap();
        }
        let m: Value = serde_json::from_slice(&std::fs::read(&meta).unwrap()).unwrap();
        let ours: Vec<_> = m["entries"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["id"] == DESKTOP_PROFILE_ID)
            .collect();
        assert_eq!(ours.len(), 1, "重复接入本档 entry 不得重复");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn desktop_restore_resets_1p_and_removes_our_profile() {
        // 接入后还原：deploymentMode 复位 1p、本档 profile 删除、_meta 清本档且清 appliedId。
        let (dir, normal, threep, profile, meta) = desktop_layout("desktop_restore");
        apply_desktop_at(&normal, &threep, &profile, &meta, "http://127.0.0.1:1", &[]).unwrap();
        assert!(profile.exists());

        // 复用 restore_claude_desktop 的内部动作：手动镜像其步骤（restore_claude_desktop 走真实
        // 目录，故这里直接验证 write_desktop_meta_clear + deploymentMode 复位这两个可测单元）。
        assert_eq!(
            read_desktop_applied_id(&meta).as_deref(),
            Some(DESKTOP_PROFILE_ID)
        );
        write_deployment_mode(&normal, "1p").unwrap();
        write_deployment_mode(&threep, "1p").unwrap();
        std::fs::remove_file(&profile).unwrap();
        let changed = write_desktop_meta_clear(&meta).unwrap();
        assert!(changed);

        let n: Value = serde_json::from_slice(&std::fs::read(&normal).unwrap()).unwrap();
        assert_eq!(n["deploymentMode"], "1p");
        let m: Value = serde_json::from_slice(&std::fs::read(&meta).unwrap()).unwrap();
        assert!(
            m.get("appliedId").is_none(),
            "唯一档被清后 appliedId 应删除"
        );
        assert!(
            m["entries"].as_array().unwrap().is_empty(),
            "本档 entry 应被摘掉"
        );
        assert!(!profile.exists(), "本档 profile 应删除");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn desktop_meta_clear_repoints_applied_to_remaining() {
        // 两档共存、appliedId 指向本档：清本档后 appliedId 应改指剩余的 cc-switch 档，不删键。
        let meta = temp_file("desktop_meta_repoint", "_meta.json");
        let ccswitch_id = "00000000-0000-4000-8000-000000157210";
        std::fs::write(
            &meta,
            format!(
                r#"{{"appliedId":"{DESKTOP_PROFILE_ID}","entries":[{{"id":"{ccswitch_id}","name":"CC Switch"}},{{"id":"{DESKTOP_PROFILE_ID}","name":"SynaRoute"}}]}}"#
            ),
        )
        .unwrap();

        assert!(write_desktop_meta_clear(&meta).unwrap());
        let m: Value = serde_json::from_slice(&std::fs::read(&meta).unwrap()).unwrap();
        assert_eq!(
            m["appliedId"], ccswitch_id,
            "appliedId 应改指剩余的 cc-switch 档"
        );
        let entries = m["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["id"], ccswitch_id, "cc-switch 档保留");

        std::fs::remove_dir_all(meta.parent().unwrap()).ok();
    }

    #[test]
    fn desktop_meta_clear_noop_when_not_ours() {
        // appliedId 指向别的档、entries 无本档：清理应为无操作（不动用户的 cc-switch 接入）。
        let meta = temp_file("desktop_meta_noop", "_meta.json");
        let ccswitch_id = "00000000-0000-4000-8000-000000157210";
        let original = format!(
            r#"{{"appliedId":"{ccswitch_id}","entries":[{{"id":"{ccswitch_id}","name":"CC Switch"}}]}}"#
        );
        std::fs::write(&meta, &original).unwrap();

        let changed = write_desktop_meta_clear(&meta).unwrap();
        assert!(!changed, "无本档时应无操作");
        let m: Value = serde_json::from_slice(&std::fs::read(&meta).unwrap()).unwrap();
        assert_eq!(m["appliedId"], ccswitch_id, "别人的 appliedId 不得改动");

        std::fs::remove_dir_all(meta.parent().unwrap()).ok();
    }

    #[test]
    fn desktop_gateway_api_key_is_redacted_in_preview() {
        // 预览脱敏必须覆盖 inferenceGatewayApiKey（即便占位，也不应把该字段值明文回传）。
        let raw = r#"{"inferenceGatewayApiKey":"synaroute-proxy","inferenceGatewayBaseUrl":"http://127.0.0.1:1"}"#;
        let out = redact_config_secrets(raw);
        assert!(
            out.contains(r#""inferenceGatewayApiKey":"***""#)
                || out.contains(r#""inferenceGatewayApiKey": "***""#),
            "inferenceGatewayApiKey 应脱敏: {out}"
        );
        assert!(
            out.contains("http://127.0.0.1:1"),
            "非密钥字段应保留"
        );
    }

    #[test]
    fn cleanup_legacy_only_removes_baseurl_config() {
        // 清理只删「确是旧 baseUrl 产物」的 config；非 baseUrl 的合法文件不动。
        // 直接测判定逻辑：构造两种 config，验证 baseUrl 键存在性判定。
        let legacy = temp_file("legacy_baseurl", DESKTOP_CONFIG_FILE);
        std::fs::write(&legacy, r#"{"baseUrl":"http://x"}"#).unwrap();
        let is_legacy = std::fs::read_to_string(&legacy)
            .ok()
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
            .and_then(|v| v.as_object().map(|o| o.contains_key("baseUrl")))
            .unwrap_or(false);
        assert!(is_legacy, "含 baseUrl 应判为旧产物");

        let legit = temp_file("legit_prefs", DESKTOP_CONFIG_FILE);
        std::fs::write(&legit, r#"{"preferences":{}}"#).unwrap();
        let is_legit_legacy = std::fs::read_to_string(&legit)
            .ok()
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
            .and_then(|v| v.as_object().map(|o| o.contains_key("baseUrl")))
            .unwrap_or(false);
        assert!(!is_legit_legacy, "不含 baseUrl 的合法文件不应被判旧产物");

        std::fs::remove_dir_all(legacy.parent().unwrap()).ok();
        std::fs::remove_dir_all(legit.parent().unwrap()).ok();
    }
}
