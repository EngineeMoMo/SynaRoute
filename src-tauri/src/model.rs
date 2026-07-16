//! 数据模型 —— 严格对齐前端 src/types.ts 的 IPC 契约。
//! serde 统一 camelCase，使 Rust snake_case 字段与前端 camelCase 自动映射。

use serde::{Deserialize, Serialize};

/// 三个目标工具分类
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CategoryType {
    #[serde(rename = "claude-cli")]
    ClaudeCli,
    #[serde(rename = "claude-desktop")]
    ClaudeDesktop,
    #[serde(rename = "codex")]
    Codex,
}

impl CategoryType {
    pub fn as_str(&self) -> &'static str {
        match self {
            CategoryType::ClaudeCli => "claude-cli",
            CategoryType::ClaudeDesktop => "claude-desktop",
            CategoryType::Codex => "codex",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Anthropic,
    Openai,
}

/// 厂商预设：选中后自动填充 Key 的 baseUrl / protocol。
/// 内置项 builtin=true，只读；用户可增删改自定义项。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Vendor {
    pub id: String,
    pub name: String,
    pub default_base_url: String,
    pub default_protocol: Protocol,
    #[serde(default)]
    pub builtin: bool,
}

impl Vendor {
    /// 内置厂商种子（首次运行注入）。
    pub fn builtin_seed() -> Vec<Vendor> {
        let mk = |id: &str, name: &str, url: &str, proto: Protocol| Vendor {
            id: id.into(),
            name: name.into(),
            default_base_url: url.into(),
            default_protocol: proto,
            builtin: true,
        };
        vec![
            mk("anthropic", "Anthropic", "https://api.anthropic.com", Protocol::Anthropic),
            mk("openai", "OpenAI", "https://api.openai.com/v1", Protocol::Openai),
            mk("zhipu", "智谱 GLM", "https://open.bigmodel.cn/api/paas/v4", Protocol::Openai),
            mk("deepseek", "DeepSeek", "https://api.deepseek.com", Protocol::Openai),
            mk("moonshot", "月之暗面 Kimi", "https://api.moonshot.cn/v1", Protocol::Openai),
            mk("custom", "自定义", "", Protocol::Anthropic),
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HealthStatus {
    Up,
    Down,
    Unknown,
    Checking,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AggregateMode {
    Compressed,
    Full,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelMapping {
    pub id: String,
    pub expected_name: String,
    pub real_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

impl Default for KeyParams {
    fn default() -> Self {
        Self { temperature: None, max_tokens: None, top_p: None, timeout_ms: None }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub real_name: String,
    pub source: String, // "fetched" | "manual"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fetched_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthState {
    pub status: HealthStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_checked: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    #[serde(default)]
    pub fail_count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub breaker_until: Option<i64>,
}

impl Default for HealthState {
    fn default() -> Self {
        Self {
            status: HealthStatus::Unknown,
            last_checked: None,
            latency_ms: None,
            fail_count: 0,
            breaker_until: None,
        }
    }
}

/// 单条厂商 Key。注意：密钥本身不在此结构里（存于加密库，见 secret 模块），
/// 仅用 has_secret 标记是否已配置，避免密钥经 IPC 下发到前端（NFR-006）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderKey {
    pub id: String,
    pub category_id: CategoryType,
    pub name: String,
    pub vendor: String,
    pub base_url: String,
    pub protocol: Protocol,
    #[serde(default)]
    pub has_secret: bool,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub priority: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers_json: Option<String>,
    #[serde(default)]
    pub params: KeyParams,
    #[serde(default)]
    pub models: Vec<ModelInfo>,
    #[serde(default)]
    pub mappings: Vec<ModelMapping>,
    #[serde(default)]
    pub health: HealthState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrainMember {
    pub id: String,
    pub key_id: String,
    pub model_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrainConfig {
    pub category_id: CategoryType,
    #[serde(default)]
    pub enabled: bool,
    pub aggregate_mode: AggregateMode,
    #[serde(default = "default_concurrency")]
    pub concurrency_limit: u32,
    #[serde(default = "default_timeout")]
    pub total_timeout_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summarizer_ref: Option<String>, // keyId::modelName
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decider_ref: Option<String>,
    #[serde(default)]
    pub members: Vec<BrainMember>,
}

fn default_concurrency() -> u32 {
    3
}
fn default_timeout() -> u64 {
    60_000
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyState {
    pub category_id: CategoryType,
    pub port: Option<u16>,
    pub status: String, // "running" | "stopped" | "error"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventLogEntry {
    pub id: String,
    pub ts: i64,
    pub category_id: CategoryType,
    #[serde(rename = "type")]
    pub kind: String, // route | failover | health | aggregate | error | request
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_id: Option<String>,
    pub detail: String,
    /// 调用模型日志的完整链路快照（仅 request 类型有；开关开启时才产生）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace: Option<RequestTrace>,
}

/// 一次代理转发的完整快照，供「调用模型日志」在页面上展开调试。
/// 记录：下游发来的原始请求 → 经模型映射/协议转换后实际发往上游的请求 → 上游返回。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestTrace {
    /// 命中的 Key 名称
    pub key_name: String,
    /// 命中的厂商标识（ProviderKey.vendor）
    pub vendor: String,
    /// 目标上游协议：anthropic | openai
    pub protocol: Protocol,
    /// 实际请求的上游完整 URL
    pub url: String,
    /// 下游请求的模型名（映射前）
    pub requested_model: String,
    /// 映射后实际发往上游的模型名
    pub real_model: String,
    /// 发往上游的请求体（已做模型映射+协议转换；不含鉴权头，密钥不落日志）
    pub request_body: String,
    /// 上游返回体（成功为响应内容，失败为错误体片段）
    pub response_body: String,
    /// 上游 HTTP 状态码（连接失败等无响应时为 None）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    /// 本次耗时（毫秒）
    pub latency_ms: u64,
    /// 是否成功（2xx）
    pub ok: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSettings {
    pub theme: String, // light | dark | system
    #[serde(default = "default_language")]
    pub language: String, // zh | en
    #[serde(default)]
    pub auto_start: bool,
    #[serde(default)]
    pub master_password_enabled: bool,
    #[serde(default)]
    pub lan_exposure: bool,
    /// 调用模型日志：开启后每次代理转发记一条 request 事件（默认关，避免噪音）
    #[serde(default)]
    pub request_log_enabled: bool,
}

fn default_language() -> String {
    "zh".into()
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            theme: "system".into(),
            language: default_language(),
            auto_start: false,
            master_password_enabled: false,
            lan_exposure: false,
            request_log_enabled: false,
        }
    }
}

/// 落盘的整体配置（不含密钥）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppConfig {
    #[serde(default)]
    pub keys: Vec<ProviderKey>,
    #[serde(default)]
    pub brain: Vec<BrainConfig>,
    #[serde(default)]
    pub vendors: Vec<Vendor>,
    #[serde(default)]
    pub settings: AppSettings,
}
