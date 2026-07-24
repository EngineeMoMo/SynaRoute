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
//! - **Claude 桌面端**：`claude_desktop_config.json`（MCP/本地入口，不写 CLI settings、不写 DEFAULT_*）

use crate::error::{AppError, AppResult};
use crate::model::CategoryType;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// 备份文件后缀
const BACKUP_SUFFIX: &str = "synaroute.bak";

/// 将某分类的代理端点写入对应目标工具配置。返回人类可读的结果说明。
///
/// `default_model`：当前分类「主 Key」首个可服务对外名（与 `/v1/models` 口径一致）。
/// - **Claude CLI only**：env.ANTHROPIC_MODEL + 顶层 `model`；并清除三档 DEFAULT_* 残留。
/// - **Codex only**：config.toml 顶层 `model`（Responses 形态，与 Claude 字段无关）。
/// - **桌面端**：忽略（不写 settings.json，不写 ANTHROPIC_*）。
pub fn apply(category: CategoryType, endpoint: &str, default_model: Option<&str>) -> AppResult<String> {
    match category {
        CategoryType::ClaudeCli => apply_claude_cli(endpoint, default_model),
        CategoryType::Codex => apply_codex(endpoint, default_model),
        CategoryType::ClaudeDesktop => apply_claude_desktop(endpoint),
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

/// 幂等合并写入 Codex auth.json：仅设置/补齐 `OPENAI_API_KEY` 占位，保留其余字段。
/// 已是目标占位值则不写盘。
fn write_codex_auth(path: &Path) -> AppResult<()> {
    const PLACEHOLDER: &str = "synaroute-proxy";
    let mut root = read_json_or_empty(path)?;
    let obj = root
        .as_object_mut()
        .ok_or_else(|| AppError::ToolConfig("auth.json 顶层非对象".into()))?;
    // 幂等：已有任意非空 OPENAI_API_KEY 则保留不动（避免覆盖用户真实 key / 反复写盘）。
    if let Some(Value::String(existing)) = obj.get("OPENAI_API_KEY") {
        if !existing.trim().is_empty() {
            return Ok(());
        }
    }
    obj.insert("OPENAI_API_KEY".into(), Value::String(PLACEHOLDER.into()));
    backup_and_write_json(path, &root)
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
/// 幂等：目标值已写则不重复。保留 config.toml 其余表（mcp_servers 等）不动。
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

fn claude_desktop_config_path() -> AppResult<PathBuf> {
    // %APPDATA%/Claude/claude_desktop_config.json
    let base = dirs::config_dir()
        .or_else(dirs::data_dir)
        .ok_or_else(|| AppError::ToolConfig("无法定位 APPDATA".into()))?;
    Ok(base.join("Claude").join("claude_desktop_config.json"))
}

fn apply_claude_desktop(endpoint: &str) -> AppResult<String> {
    let path = claude_desktop_config_path()?;
    // 用 with_rollback 包裹：读-改-写若中途失败即恢复原文件，与 Codex 接入保持同一套原子保证
    // （桌面端配置文件是用户 Claude 桌面端的关键设置，宁可整体回滚也不留半配置）。
    with_rollback(&[path.clone()], || {
        let mut root = read_json_or_empty(&path)?;
        let obj = root
            .as_object_mut()
            .ok_or_else(|| AppError::ToolConfig("claude_desktop_config.json 顶层非对象".into()))?;

        // 桌面端以自定义端点字段接入（cc-switch 同款思路）；键名随桌面端版本，写入常见字段
        obj.insert("baseUrl".into(), Value::String(endpoint.to_string()));

        backup_and_write_json(&path, &root)?;
        Ok(format!(
            "已写入 Claude 桌面端配置：{}（baseUrl={endpoint}），原文件已备份。注意：桌面端可能需重启生效",
            path.display()
        ))
    })
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

/// 单次 MCP 工具调用超时（毫秒），写进客户端配置的 `timeout` 字段。
///
/// SynaRoute 的多模型聚合天然慢（多模型并行 + 决策者二次调用，实测 40~60s）。
/// Claude Code 对 HTTP MCP 有两层超时：单次工具调用总时长、以及「首字节」per-request
/// 计时器（默认 60s，请求超时不重试）。聚合逼近甚至超过 60s 就会被客户端判超时、重试。
/// 官方文档：把 server 的 `timeout` 设为 ≥60s 会**同时**抬高首字节计时器到该值。
/// 故这里统一写 600000（10 分钟），彻底覆盖聚合耗时，无需用户手动配。
const MCP_TOOL_TIMEOUT_MS: u64 = 600_000;

/// Claude 全局配置 ~/.claude.json（Claude CLI 的 mcpServers 存放处）。
fn claude_json_path() -> AppResult<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| AppError::ToolConfig("无法定位用户目录".into()))?;
    Ok(home.join(".claude.json"))
}

/// 把 SynaRoute MCP 服务器注册进某分类对应工具的客户端配置。
/// 返回 (人类可读结果, 是否实际写盘)。已是同 url 时不写盘、返回 false。
pub fn register_mcp_client(category: CategoryType, mcp_url: &str) -> AppResult<(String, bool)> {
    match category {
        // 桌面端的 MCP 也读 ~/.claude.json（与 CLI 同源 mcpServers）。
        CategoryType::ClaudeCli | CategoryType::ClaudeDesktop => register_mcp_claude(mcp_url),
        CategoryType::Codex => register_mcp_codex(mcp_url),
    }
}

/// 从某分类对应工具的客户端配置移除 synaroute MCP 项（关闭开关时）。
pub fn unregister_mcp_client(category: CategoryType) -> AppResult<(String, bool)> {
    match category {
        CategoryType::ClaudeCli | CategoryType::ClaudeDesktop => unregister_mcp_claude(),
        CategoryType::Codex => unregister_mcp_codex(),
    }
}

fn register_mcp_claude(mcp_url: &str) -> AppResult<(String, bool)> {
    register_mcp_claude_at(&claude_json_path()?, mcp_url)
}

fn register_mcp_claude_at(path: &Path, mcp_url: &str) -> AppResult<(String, bool)> {
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
    // 而永远升不上 600000，超时修复形同虚设。
    if let Some(existing) = servers_obj.get(MCP_CLIENT_NAME) {
        if existing.get("url").and_then(|u| u.as_str()) == Some(mcp_url)
            && existing.get("type").and_then(|t| t.as_str()) == Some("http")
            && existing.get("timeout").and_then(|t| t.as_u64()) == Some(MCP_TOOL_TIMEOUT_MS)
        {
            return Ok((format!("Claude MCP 已是最新（{mcp_url}），跳过"), false));
        }
    }

    servers_obj.insert(
        MCP_CLIENT_NAME.to_string(),
        json_http_mcp(mcp_url),
    );
    backup_and_write_json(path, &root)?;
    Ok((
        format!("已注册 MCP 到 Claude：{}（{mcp_url}），重启客户端生效", path.display()),
        true,
    ))
}

/// Claude 的 HTTP MCP 项：{ "type":"http", "url":"...", "timeout":600000 }。
/// timeout（毫秒）：Claude Code 对 HTTP MCP 有两层超时——单次工具调用总时长、以及
/// 「首字节」per-request timer（默认 60s）。多模型聚合常需 40~60s 才吐首字节，逼近 60s 线
/// 会被判超时并重试。官方文档：把 timeout 设为 ≥60s 会同时抬高首字节 timer 到该值，
/// 故写 600000（10 分钟）彻底规避（见 code.claude.com/docs/en/mcp）。
fn json_http_mcp(mcp_url: &str) -> Value {
    let mut m = serde_json::Map::new();
    m.insert("type".into(), Value::String("http".into()));
    m.insert("url".into(), Value::String(mcp_url.to_string()));
    m.insert("timeout".into(), Value::Number(MCP_TOOL_TIMEOUT_MS.into()));
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

fn register_mcp_codex(mcp_url: &str) -> AppResult<(String, bool)> {
    register_mcp_codex_at(&codex_config_path()?, mcp_url)
}

fn register_mcp_codex_at(path: &Path, mcp_url: &str) -> AppResult<(String, bool)> {
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
    let servers = table
        .entry("mcp_servers".to_string())
        .or_insert_with(|| toml::Value::Table(Default::default()));
    let servers_table = servers
        .as_table_mut()
        .ok_or_else(|| AppError::ToolConfig("mcp_servers 非表".into()))?;

    // 幂等：已存在且 url / timeout 全一致 → 不写盘（timeout 必须比对，理由同 Claude 端）。
    if let Some(existing) = servers_table.get(MCP_CLIENT_NAME).and_then(|v| v.as_table()) {
        if existing.get("url").and_then(|u| u.as_str()) == Some(mcp_url)
            && existing.get("timeout").and_then(|t| t.as_integer()) == Some(MCP_TOOL_TIMEOUT_MS as i64)
        {
            return Ok((format!("Codex MCP 已是最新（{mcp_url}），跳过"), false));
        }
    }

    // HTTP transport：url + timeout（毫秒，理由见 json_http_mcp 注释）。
    let mut entry = toml::value::Table::new();
    entry.insert("url".to_string(), toml::Value::String(mcp_url.to_string()));
    entry.insert("timeout".to_string(), toml::Value::Integer(MCP_TOOL_TIMEOUT_MS as i64));
    servers_table.insert(MCP_CLIENT_NAME.to_string(), toml::Value::Table(entry));

    let serialized =
        toml::to_string_pretty(&doc).map_err(|e| AppError::ToolConfig(e.to_string()))?;
    backup_and_write_bytes(path, serialized.as_bytes())?;
    Ok((
        format!("已注册 MCP 到 Codex：{}（{mcp_url}），重启客户端生效", path.display()),
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
    // 规则1：改写前备份原文件
    if path.exists() {
        let backup = path.with_extension(match path.extension().and_then(|e| e.to_str()) {
            Some(ext) => format!("{ext}.{BACKUP_SUFFIX}"),
            None => BACKUP_SUFFIX.to_string(),
        });
        std::fs::copy(path, &backup)?;
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

/// 还原某工具配置（从 .synaroute.bak 恢复）
pub fn restore(category: CategoryType) -> AppResult<String> {
    let path = match category {
        CategoryType::ClaudeCli => claude_cli_settings_path()?,
        CategoryType::Codex => codex_config_path()?,
        CategoryType::ClaudeDesktop => claude_desktop_config_path()?,
    };
    let backup = path.with_extension(match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => format!("{ext}.{BACKUP_SUFFIX}"),
        None => BACKUP_SUFFIX.to_string(),
    });
    if !backup.exists() {
        return Err(AppError::ToolConfig("未找到备份文件".into()));
    }
    let data = std::fs::read(&backup)?;
    crate::secret::atomic_write(&path, &data)?;
    Ok(format!("已从备份还原：{}", path.display()))
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
    let path = claude_desktop_config_path()?;
    let (exists, content) = read_preview_text(&path, true)?;
    Ok(ToolConfigPreview {
        category_id: CategoryType::ClaudeDesktop,
        summary: "Claude 桌面端：claude_desktop_config.json。写入 baseUrl 指向本机代理。不写 Claude CLI 的 settings.json，不写 ANTHROPIC_DEFAULT_*。".into(),
        files: vec![ToolConfigFilePreview {
            path: path.display().to_string(),
            exists,
            format: "json".into(),
            content,
        }],
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
        let (_, wrote) = register_mcp_claude_at(&path, url).unwrap();
        assert!(wrote, "首次注册应写盘");

        let v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let entry = &v["mcpServers"]["synaroute"];
        assert_eq!(entry["type"], "http", "必须是官方 http transport");
        assert_eq!(entry["url"], url, "url 应写入");
        assert_eq!(entry["timeout"], 600000, "必须写入 timeout 抬高客户端首字节超时，避免聚合被判超时");

        // 再次相同 url：幂等，不写盘
        let (_, wrote2) = register_mcp_claude_at(&path, url).unwrap();
        assert!(!wrote2, "相同 url 应跳过写盘");

        // 换端口：应重写
        let url2 = "http://127.0.0.1:9600/mcp";
        let (_, wrote3) = register_mcp_claude_at(&path, url2).unwrap();
        assert!(wrote3, "url 变化应写盘");
        let v2: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(v2["mcpServers"]["synaroute"]["url"], url2);

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

        register_mcp_claude_at(&path, "http://127.0.0.1:9527/mcp").unwrap();
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
    fn codex_register_writes_url_and_preserves_other_tables() {
        let path = temp_file("codex_reg", "config.toml");
        std::fs::write(
            &path,
            "model = \"gpt-5\"\n\n[mcp_servers.codegraph]\ncommand = \"codegraph\"\n",
        )
        .unwrap();
        let url = "http://127.0.0.1:9527/mcp";

        let (_, wrote) = register_mcp_codex_at(&path, url).unwrap();
        assert!(wrote);

        let doc: toml::Value = std::fs::read_to_string(&path).unwrap().parse().unwrap();
        assert_eq!(
            doc["mcp_servers"]["synaroute"]["url"].as_str(),
            Some(url),
            "应写入 http url"
        );
        assert_eq!(
            doc["mcp_servers"]["synaroute"]["timeout"].as_integer(),
            Some(MCP_TOOL_TIMEOUT_MS as i64),
            "应写入 timeout（规避 Claude/Codex 60s 首字节超时）"
        );
        assert_eq!(
            doc["mcp_servers"]["codegraph"]["command"].as_str(),
            Some("codegraph"),
            "已有 MCP 应保留"
        );
        assert_eq!(doc["model"].as_str(), Some("gpt-5"), "顶层键应保留");

        // 幂等
        let (_, wrote2) = register_mcp_codex_at(&path, url).unwrap();
        assert!(!wrote2, "相同 url 应跳过");

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
}
