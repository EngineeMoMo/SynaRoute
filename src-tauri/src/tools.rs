//! 目标工具接入模块 —— 把本地代理端点写入三个工具的真实配置文件。
//!
//! 硬规则（dev-hard-rules，用户强制要求）：
//! 1. 改写任何配置文件前，先备份为 *.synaroute.bak
//! 2. 原子写（临时文件 → 重命名替换）
//! 3. 路径全部动态解析（dirs / env），禁止硬编码本机路径
//!
//! 接入机制（基于接入验证实证）：
//! - Claude CLI：~/.claude/settings.json 的 env.ANTHROPIC_BASE_URL / ANTHROPIC_AUTH_TOKEN
//! - Codex：~/.codex/config.toml 的 [shell_environment_policy.set].ANTHROPIC_BASE_URL（及 OpenAI base）
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
    let content = if path.exists() {
        std::fs::read_to_string(&path)?
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

    // 写入 [shell_environment_policy.set].ANTHROPIC_BASE_URL
    let table = doc
        .as_table_mut()
        .ok_or_else(|| AppError::ToolConfig("config.toml 顶层非表".into()))?;
    let sep = table
        .entry("shell_environment_policy".to_string())
        .or_insert_with(|| toml::Value::Table(Default::default()));
    let sep_table = sep
        .as_table_mut()
        .ok_or_else(|| AppError::ToolConfig("shell_environment_policy 非表".into()))?;
    let set = sep_table
        .entry("set".to_string())
        .or_insert_with(|| toml::Value::Table(Default::default()));
    let set_table = set
        .as_table_mut()
        .ok_or_else(|| AppError::ToolConfig("set 非表".into()))?;
    set_table.insert(
        "ANTHROPIC_BASE_URL".to_string(),
        toml::Value::String(endpoint.to_string()),
    );

    let serialized =
        toml::to_string_pretty(&doc).map_err(|e| AppError::ToolConfig(e.to_string()))?;
    backup_and_write_bytes(&path, serialized.as_bytes())?;
    Ok(format!(
        "已写入 Codex 配置：{}（ANTHROPIC_BASE_URL={endpoint}），原文件已备份",
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
