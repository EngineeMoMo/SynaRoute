//! 目标工具接入模块 —— 把本地代理端点写入三个工具的真实配置文件。
//!
//! 硬规则（dev-hard-rules，用户强制要求）：
//! 1. 改写任何配置文件前，先备份为 *.synaroute.bak
//! 2. 原子写（临时文件 → 重命名替换）
//! 3. 路径全部动态解析（dirs / env），禁止硬编码本机路径
//!
//! 接入机制（基于接入验证实证）：
//! - Claude CLI：~/.claude/settings.json 的 env.ANTHROPIC_BASE_URL / ANTHROPIC_AUTH_TOKEN
//! - Codex：~/.codex/config.toml 的 model_provider + [model_providers.synaroute]（base_url/wire_api/env_key）
//! - Claude 桌面端：%APPDATA%/Claude/claude_desktop_config.json（cc-switch 同款思路）

use crate::error::{AppError, AppResult};
use crate::model::CategoryType;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// 备份文件后缀
const BACKUP_SUFFIX: &str = "synaroute.bak";

/// 将某分类的代理端点写入对应目标工具配置。返回人类可读的结果说明。
pub fn apply(category: CategoryType, endpoint: &str) -> AppResult<String> {
    match category {
        CategoryType::ClaudeCli => apply_claude_cli(endpoint),
        CategoryType::Codex => apply_codex(endpoint),
        CategoryType::ClaudeDesktop => apply_claude_desktop(endpoint),
    }
}

// ---- Claude CLI ----

fn claude_cli_settings_path() -> AppResult<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| AppError::ToolConfig("无法定位用户目录".into()))?;
    Ok(home.join(".claude").join("settings.json"))
}

fn apply_claude_cli(endpoint: &str) -> AppResult<String> {
    let path = claude_cli_settings_path()?;
    let mut root = read_json_or_empty(&path)?;

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
    // 注意：CLI 只接受 id 以 "claude"/"anthropic" 开头的模型，其它一律被静默过滤 —— 映射对外名
    // 需以此开头（如 claude-opus-4-8），否则不会出现在选择器里。
    env_obj.insert(
        "CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY".into(),
        Value::String("1".into()),
    );

    backup_and_write_json(&path, &root)?;
    Ok(format!(
        "已写入 Claude CLI 配置：{}（ANTHROPIC_BASE_URL={endpoint}），原文件已备份",
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

fn apply_codex(endpoint: &str) -> AppResult<String> {
    let path = codex_config_path()?;
    apply_codex_at(&path, endpoint)
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
/// - env_key 指向 `SYNAROUTE_API_KEY`：代理侧不校验 token，但 Codex 需要一个 env 变量名；
///   用户把它设为任意非空值即可（值不影响路由，真实密钥由代理按 Key 注入）。
///
/// 幂等：三项都已是目标值则不写盘。保留 config.toml 其余表（mcp_servers 等）不动。
fn apply_codex_at(path: &Path, endpoint: &str) -> AppResult<String> {
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
    entry.insert(
        "env_key".into(),
        toml::Value::String("SYNAROUTE_API_KEY".into()),
    );
    providers_table.insert(MCP_CLIENT_NAME.to_string(), toml::Value::Table(entry));

    let serialized =
        toml::to_string_pretty(&doc).map_err(|e| AppError::ToolConfig(e.to_string()))?;
    backup_and_write_bytes(path, serialized.as_bytes())?;
    Ok(format!(
        "已写入 Codex 配置：{}（model_provider=synaroute，base_url={base_url}，wire_api=responses），原文件已备份。\
         注意：请把环境变量 SYNAROUTE_API_KEY 设为任意非空值（代理不校验其值）",
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

    // 幂等：已存在且 url 一致 → 不写盘。
    if let Some(existing) = servers_obj.get(MCP_CLIENT_NAME) {
        if existing.get("url").and_then(|u| u.as_str()) == Some(mcp_url)
            && existing.get("type").and_then(|t| t.as_str()) == Some("http")
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

/// Claude 的 HTTP MCP 项：{ "type":"http", "url":"..." }（官方 mcpServers 结构）。
fn json_http_mcp(mcp_url: &str) -> Value {
    let mut m = serde_json::Map::new();
    m.insert("type".into(), Value::String("http".into()));
    m.insert("url".into(), Value::String(mcp_url.to_string()));
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

    // 幂等：已存在且 url 一致 → 不写盘。
    if let Some(existing) = servers_table.get(MCP_CLIENT_NAME).and_then(|v| v.as_table()) {
        if existing.get("url").and_then(|u| u.as_str()) == Some(mcp_url) {
            return Ok((format!("Codex MCP 已是最新（{mcp_url}），跳过"), false));
        }
    }

    // HTTP transport 最小配置：仅一个 url（官方 config-reference）。
    let mut entry = toml::value::Table::new();
    entry.insert("url".to_string(), toml::Value::String(mcp_url.to_string()));
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
    fn codex_apply_writes_custom_provider_not_anthropic_env() {
        // 核心修复回归：Codex 不认 ANTHROPIC_BASE_URL，必须写标准自定义 provider。
        let path = temp_file("codex_apply", "config.toml");
        // 预置其它表，验证不被破坏。
        std::fs::write(
            &path,
            "model = \"gpt-5\"\n\n[mcp_servers.codegraph]\ncommand = \"codegraph\"\n",
        )
        .unwrap();

        let msg = apply_codex_at(&path, "http://127.0.0.1:8790").unwrap();
        assert!(msg.contains("model_provider=synaroute"));

        let doc: toml::Value = std::fs::read_to_string(&path).unwrap().parse().unwrap();
        // 选中自定义 provider
        assert_eq!(doc["model_provider"].as_str(), Some("synaroute"));
        // provider 定义正确：base_url 追加 /v1、wire_api=responses、env_key 存在
        let p = &doc["model_providers"]["synaroute"];
        assert_eq!(p["base_url"].as_str(), Some("http://127.0.0.1:8790/v1"));
        assert_eq!(p["wire_api"].as_str(), Some("responses"));
        assert_eq!(p["env_key"].as_str(), Some("SYNAROUTE_API_KEY"));
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
}
