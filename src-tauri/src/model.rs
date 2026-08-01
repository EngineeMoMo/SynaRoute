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
    /// 全部分类（遍历用，如聚合 MCP 客户端超时联动需取各分类 total_timeout_ms 的最大值）。
    pub const ALL: [CategoryType; 3] = [
        CategoryType::ClaudeCli,
        CategoryType::ClaudeDesktop,
        CategoryType::Codex,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            CategoryType::ClaudeCli => "claude-cli",
            CategoryType::ClaudeDesktop => "claude-desktop",
            CategoryType::Codex => "codex",
        }
    }

    /// 从字符串解析分类（MCP 工具参数用）。未知值返回 None。
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "claude-cli" => Some(CategoryType::ClaudeCli),
            "claude-desktop" => Some(CategoryType::ClaudeDesktop),
            "codex" => Some(CategoryType::Codex),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Protocol {
    #[serde(rename = "anthropic")]
    Anthropic,
    /// OpenAI Chat Completions（messages[]/choices，端点 /chat/completions）。
    /// alias "openai" 无损迁移旧配置：此前所有 OpenAI 系（DeepSeek/GLM/Kimi）都是 Chat 语义。
    #[serde(rename = "openai_chat", alias = "openai")]
    OpenaiChat,
    /// OpenAI Responses API（input/output[]，端点 /responses）。Codex 客户端使用此协议。
    #[serde(rename = "openai_responses")]
    OpenaiResponses,
}

impl Protocol {
    /// 是否属于 OpenAI 家族（Chat 或 Responses）——鉴权头、模型发现形态等按家族区分。
    pub fn is_openai(self) -> bool {
        matches!(self, Protocol::OpenaiChat | Protocol::OpenaiResponses)
    }

    /// 本协议对应上游的主补全端点资源路径（跨协议转换时用；同协议保留原始下游路径）。
    pub fn completion_path(self) -> &'static str {
        match self {
            Protocol::Anthropic => "/v1/messages",
            Protocol::OpenaiChat => "/v1/chat/completions",
            Protocol::OpenaiResponses => "/v1/responses",
        }
    }
}

/// 厂商预设的一个模型条目（参考 cc-switch 的 modelCatalog，取长补短）。
/// 上游 `/v1/models` 不暴露或拉取失败时，用户可从此清单一键导入到 Key 的模型列表。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PresetModel {
    pub real_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u32>,
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
    /// 自定义图标（data-URL，如 `data:image/png;base64,...`）。
    /// 内置厂商为 None（走前端品牌图标启发式）；自定义厂商可选上传。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// 内置预设模型清单：上游不暴露模型接口时供一键导入。自定义厂商默认空。
    #[serde(default)]
    pub preset_models: Vec<PresetModel>,
}

impl Vendor {
    /// 内置厂商种子（首次运行注入）。
    pub fn builtin_seed() -> Vec<Vendor> {
        let mk = |id: &str, name: &str, url: &str, proto: Protocol, models: Vec<PresetModel>| Vendor {
            id: id.into(),
            name: name.into(),
            default_base_url: url.into(),
            default_protocol: proto,
            builtin: true,
            icon: None,
            preset_models: models,
        };
        // 预设模型的 context_window 取各家公开文档常见值；名称随厂商更新可能变，用户仍可手改/拉取。
        let pm = |real: &str, disp: &str, ctx: u32| PresetModel {
            real_name: real.into(),
            display_name: Some(disp.into()),
            context_window: Some(ctx),
        };
        vec![
            mk("anthropic", "Anthropic", "https://api.anthropic.com", Protocol::Anthropic, vec![
                pm("claude-opus-4-5", "Claude Opus 4.5", 200_000),
                pm("claude-sonnet-4-5", "Claude Sonnet 4.5", 200_000),
                pm("claude-haiku-4-5", "Claude Haiku 4.5", 200_000),
            ]),
            mk("openai", "OpenAI", "https://api.openai.com/v1", Protocol::OpenaiResponses, vec![
                pm("gpt-5.5", "GPT-5.5", 400_000),
                pm("gpt-5", "GPT-5", 400_000),
                pm("gpt-4o", "GPT-4o", 128_000),
            ]),
            mk("zhipu", "智谱 GLM", "https://open.bigmodel.cn/api/paas/v4", Protocol::OpenaiChat, vec![
                pm("glm-4.6", "GLM-4.6", 200_000),
                pm("glm-4.5", "GLM-4.5", 128_000),
                pm("glm-4.5-air", "GLM-4.5-Air", 128_000),
            ]),
            mk("deepseek", "DeepSeek", "https://api.deepseek.com", Protocol::OpenaiChat, vec![
                pm("deepseek-chat", "DeepSeek Chat", 128_000),
                pm("deepseek-reasoner", "DeepSeek Reasoner", 128_000),
            ]),
            mk("moonshot", "月之暗面 Kimi", "https://api.moonshot.cn/v1", Protocol::OpenaiChat, vec![
                pm("kimi-k2-0905-preview", "Kimi K2", 256_000),
                pm("moonshot-v1-128k", "Moonshot v1 128k", 128_000),
            ]),
            mk("custom", "自定义", "", Protocol::Anthropic, vec![]),
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub real_name: String,
    pub source: String, // "fetched" | "manual"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fetched_at: Option<i64>,
    /// 上下文窗口大小（token 数），如 200000、1000000
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u32>,
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
    /// 最近一次「真实转发成功」的时间戳（毫秒）。用于让后台探测失败不单方面熔断一个
    /// 正在成功服务的 Key——真实流量才是可用性的最终裁判。仅内存/持久化标记，不下发前端展示。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_live_success: Option<i64>,
}

impl Default for HealthState {
    fn default() -> Self {
        Self {
            status: HealthStatus::Unknown,
            last_checked: None,
            latency_ms: None,
            fail_count: 0,
            breaker_until: None,
            last_live_success: None,
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
    /// 默认兜底模型（可选）：故障转移到本 Key 时，若请求的模型名既非某条映射的期望名、
    /// 也不是本 Key 的真实模型名，则改用此模型转发。为空时退回「该 Key 第一个模型」。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    /// 三档快捷映射（取自 cc-switch 的 haiku/sonnet/opus 语义，落到我们的运行时代理）。
    /// Claude Code 按任务发不同家族模型名（含 haiku/sonnet/opus 子串），配了对应档位即改写为上游真实名。
    /// 与 `mappings` 并存：自由映射（精确名）优先级更高，三档作为家族级兜底。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier_haiku: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier_sonnet: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier_opus: Option<String>,
    #[serde(default)]
    pub health: HealthState,
}

/// Claude Code 网关模型发现：CLI 静默丢弃 id 不以 `claude`/`anthropic` 开头的条目
///（见官方 llm-gateway-protocol / CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY）。
/// 非合规名（如 grok-4.5、glm-4.6）在 `/v1/models` 暴露时包一层此外缀，客户端选中后
/// `resolve_model` 再剥掉还原。
pub const GATEWAY_ALIAS_PREFIX: &str = "claude-synaroute-";

/// 该模型 id 是否能直接出现在 Claude Code /model 的 From gateway 列表里。
pub fn is_cli_discoverable_model_id(name: &str) -> bool {
    let lower = name.trim().to_ascii_lowercase();
    lower.starts_with("claude") || lower.starts_with("anthropic")
}

/// 把可服务模型名变成 CLI 能展示的网关 id：已合规则原样，否则加 `claude-synaroute-` 前缀。
/// 仅用于 Claude CLI/桌面端的 `/v1/models` 响应；Codex 不包。
pub fn to_gateway_model_id(name: &str) -> String {
    let name = name.trim();
    if name.is_empty() || is_cli_discoverable_model_id(name) {
        name.to_string()
    } else {
        format!("{GATEWAY_ALIAS_PREFIX}{name}")
    }
}

/// 剥掉网关别名前缀；非别名则原样返回。
pub fn unwrap_gateway_model_id(name: &str) -> &str {
    name.strip_prefix(GATEWAY_ALIAS_PREFIX).unwrap_or(name)
}

// ---- Claude 桌面端（3p gateway）的模型名判据 ----
//
// **与 CLI 的规则完全不同，别混用**：
// - CLI（`is_cli_discoverable_model_id`）只看前缀 `claude`/`anthropic`，无厂商黑名单。
//   故非合规名可以靠 `to_gateway_model_id` 包 `claude-synaroute-` 前缀救回来。
// - 桌面端（本函数）要求「含关键词」**且**「不含厂商名」，且厂商名黑名单优先。
//   → `claude-synaroute-glm-4.6` 仍命中 `glm` 被拒，**前缀对桌面端毫无作用**。
//
// 判据来源：Claude 桌面端 `app.asar` v1.24012.9（包 `Claude_1.24012.9.0_x64__pzs8sxrjxfjjc`），
// offset ≈ 6842900 处的 `sD()`。反查方法见 docs/14 第七节（分块 grep 字节串 + dump 上下文）。
// 原文（minify 后）：
//   const Dc=["sonnet","opus","haiku","fable","mythos"];
//   const nhe=new RegExp(`^(${Dc.join("|")})(-[\d.]+)?$`);
//   const Snt=["claude",...Dc,"anthropic"];
//   const Ent=/ark-code|astron|…|dpsk/;
//   function sD(e){const t=e.toLowerCase();return Ent.test(t)?!1:nhe.test(t)||Snt.some(n=>t.includes(n))}
//
// **不合规的后果是硬过滤，不是提示**（此前误记为「仅面板黄字」）：
// `W0n(provider,{adminList,discovered})`（offset ≈ 7964450）末行 `return e?i.filter(o=>aD(e,o.id).ok):i`
// 把不合规条目**从模型列表里删掉**；随后 `resolveDefaultSessionModel()` 见空即返回
// `{ok:false, reason:"models_not_discovered"}`，打开会话直接抛 `ModelsNotDiscoveredError`。
// 即「`inferenceModels` 非空但全被过滤」与「`inferenceModels` 为空」后果等价。
// `validateSessionModel` 的 allowed 成员判断也是拿**过滤后**的列表比对，同样救不回来。

/// Claude 桌面端的三档 + 两个新家族名（官方 `Dc`）。
const DESKTOP_TIER_NAMES: [&str; 5] = ["sonnet", "opus", "haiku", "fable", "mythos"];

/// 「1M 上下文」的判定阈值（token）。`supports1m` 只对达到此窗口的模型置 true。
pub const ONE_MILLION_CONTEXT: u32 = 1_000_000;

/// 从**对外模型名**推断它代表哪个 Claude 档位（`anthropicFamilyTier` 的取值）。
///
/// 官方 `anthropicFamilyTier` 的作用：桌面端遇到裸别名（`opus` / `sonnet` / `haiku` / `fable`）
/// 时，会在 `inferenceModels` 里找 `anthropicFamilyTier` 等于该别名的条目并「钉」到它
/// （`ldn()`，offset ≈ 7400700）；opus/fable 还兼作 refusal fallback 的来源。不填则裸别名无处可落。
///
/// 判定方式与桌面端自身对模型名的家族识别一致——按名字含哪个档位子串。同时含多个时按
/// 官方 `Dc` 的顺序取第一个命中（`sonnet` → `opus` → `haiku` → `fable` → `mythos`），
/// 保证结果稳定、不依赖遍历顺序。
pub fn desktop_family_tier_of(outward_name: &str) -> Option<&'static str> {
    let lower = outward_name.trim().to_ascii_lowercase();
    DESKTOP_TIER_NAMES
        .iter()
        .find(|tier| lower.contains(*tier))
        .copied()
}

/// 必须命中其一的关键词（官方 `Snt` = `["claude", ...Dc, "anthropic"]`）。
const DESKTOP_ALLOW_KEYWORDS: [&str; 7] = [
    "claude", "sonnet", "opus", "haiku", "fable", "mythos", "anthropic",
];

/// 厂商名黑名单里的**普通子串**项（官方 `Ent` 中不带 `\b` / `\d` 的那些）。
/// `amazon.nova` 的点在原正则里是 `\.`（转义后为字面点），故此处也是字面量。
/// 带词边界的 `\bling\b` / `\bunic\b` / `\bds-` / `\bk2\.` / `\bm2\.` 与 `phi\d`
/// 不在此列，由 [`desktop_denied_vendor_name`] 单独处理。
const DESKTOP_DENY_SUBSTRINGS: [&str; 50] = [
    "ark-code", "astron", "command-r", "deepseek", "doubao", "gemini", "gemma", "glm", "gpt",
    "grok", "hermes", "hy3", "kimi", "lfm", "llama", "longcat", "mimo", "minimax", "mistral",
    "mixtral", "moonshot", "nemotron", "openai", "phi-", "qianfan", "qwen", "tc-code", "yi-",
    "stepfun", "step-3", "seed-", "bytedance", "hunyuan", "granite", "amazon.nova", "nova-",
    "devstral", "ministral", "ernie", "codex", "arcee", "trinity", "abab", "jamba", "arctic",
    "solar", "mercury", "zamba", "kat-coder", "dpsk",
];

/// JS 正则 `\w` 的字符集（`[A-Za-z0-9_]`），用于复刻 `\b` 词边界语义。
fn is_js_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// 复刻 JS 的 `\b<needle>` / `\b<needle>\b`：按需要求匹配位置左/右侧不是 `\w`（字符串边界算满足）。
///
/// 只处理 ASCII needle（黑名单全是 ASCII），故可安全按字节位置取相邻字符。
fn contains_with_boundaries(hay: &str, needle: &str, need_left: bool, need_right: bool) -> bool {
    let bytes = hay.as_bytes();
    let mut from = 0usize;
    while let Some(rel) = hay[from..].find(needle) {
        let start = from + rel;
        let end = start + needle.len();
        let left_ok = !need_left
            || start == 0
            || !is_js_word_char(bytes[start - 1] as char);
        let right_ok = !need_right
            || end == bytes.len()
            || !is_js_word_char(bytes[end] as char);
        if left_ok && right_ok {
            return true;
        }
        from = start + 1;
    }
    false
}

/// 命中官方厂商名黑名单（`Ent`）→ 桌面端一定拒绝，且**优先于**关键词允许判定。
fn desktop_denied_vendor_name(lower: &str) -> bool {
    if DESKTOP_DENY_SUBSTRINGS.iter().any(|s| lower.contains(s)) {
        return true;
    }
    // `\bling\b` / `\bunic\b`：两侧都要词边界。
    if contains_with_boundaries(lower, "ling", true, true)
        || contains_with_boundaries(lower, "unic", true, true)
    {
        return true;
    }
    // `\bk2\.` / `\bm2\.` / `\bds-`：只要求左侧词边界（右侧的 `.` / `-` 已在字面量里）。
    if contains_with_boundaries(lower, "k2.", true, false)
        || contains_with_boundaries(lower, "m2.", true, false)
        || contains_with_boundaries(lower, "ds-", true, false)
    {
        return true;
    }
    // `phi\d`：phi 紧跟一个数字。须遍历**全部**出现位置——只看第一处会漏
    // 「phi 后面不是数字、但后续还有一个 phi3」这类串。
    let mut from = 0usize;
    while let Some(rel) = lower[from..].find("phi") {
        let end = from + rel + "phi".len();
        if lower[end..].chars().next().is_some_and(|c| c.is_ascii_digit()) {
            return true;
        }
        from = from + rel + 1;
    }
    false
}

/// 整串恰为「档位名」或「档位名-数字/点」（官方 `nhe` = `^(sonnet|opus|…)(-[\d.]+)?$`）。
fn is_desktop_bare_tier_alias(lower: &str) -> bool {
    DESKTOP_TIER_NAMES.iter().any(|tier| {
        if lower == *tier {
            return true;
        }
        lower
            .strip_prefix(tier)
            .and_then(|rest| rest.strip_prefix('-'))
            .is_some_and(|digits| {
                !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit() || c == '.')
            })
    })
}

/// Claude 桌面端（3p gateway 供应商）是否接受该模型 id。
///
/// 复刻 `app.asar` 的 `sD()`（判据与后果见本节顶部注释）。不接受的名字会被桌面端从模型列表里
/// **过滤掉**——全部不接受时选择器为空、打开会话抛 `ModelsNotDiscoveredError`。
///
/// 注意：`claude-synaroute-` 前缀救不了桌面端（黑名单优先），所以桌面端分类的**对外名**必须
/// 本身合规；真实上游名不受此限（配一条映射 `claude-opus-4-8` → `glm-4.6` 即可）。
pub fn is_desktop_acceptable_model_id(name: &str) -> bool {
    let lower = name.trim().to_ascii_lowercase();
    if lower.is_empty() {
        return false;
    }
    if desktop_denied_vendor_name(&lower) {
        return false;
    }
    is_desktop_bare_tier_alias(&lower)
        || DESKTOP_ALLOW_KEYWORDS.iter().any(|k| lower.contains(k))
}

/// `resolve_model` 的命中路径，供日志展示「为什么变成这个模型」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelResolveKind {
    /// 自由映射 exact match
    Mapping,
    /// 三档家族（haiku/sonnet/opus）
    Tier,
    /// Key 的 models 列表里原生有这个名
    Native,
    /// 落到 default_model
    Default,
    /// 落到 models 列表首个
    First,
    /// 无任何配置，原样透传
    Passthrough,
}

impl ModelResolveKind {
    /// 中文标签，用于路由日志括号说明
    pub fn label_zh(self) -> &'static str {
        match self {
            Self::Mapping => "映射",
            Self::Tier => "三档",
            Self::Native => "原生",
            Self::Default => "默认兜底",
            Self::First => "列表首个",
            Self::Passthrough => "透传",
        }
    }
}

impl ProviderKey {
    /// 解析故障转移到本 Key 时应实际请求的上游模型名（参考 cc-switch，取长补短）。
    ///
    /// 优先级：
    /// 1. 映射命中：`requested` 命中某条映射的期望名 → 用其真实名（用户显式意图，最高）
    /// 2. 三档命中：`requested` 含 haiku/sonnet/opus 家族子串且配了对应档位 → 用该档真实名
    /// 3. 原生支持：`requested` 恰好是本 Key 的某个真实模型名 → 原样使用
    /// 4. 默认兜底：用户为本 Key 配置的 `default_model`（可选）
    /// 5. 第一个模型：本 Key 拉取/手填的模型列表非空则取首个
    /// 6. 透传：以上都不满足（Key 未配置任何模型）→ 原样发 `requested`（保持旧行为）
    ///
    /// 三档放在「原生支持」之前：Claude Code 发的是 `claude-sonnet-4-5-*` 等家族名，
    /// 上游多半不原生支持，应优先落三档；自由映射仍在最前——用户手配精确映射永远赢。
    pub fn resolve_model(&self, requested_model: &str) -> String {
        self.resolve_model_detail(requested_model).0
    }

    /// 同 [`Self::resolve_model`]，额外返回命中路径，供日志写清「请求/实际/原因」。
    pub fn resolve_model_detail(&self, requested_model: &str) -> (String, ModelResolveKind) {
        // 0. 网关别名反解：CLI 只能展示 claude/anthropic 前缀 id，/v1/models 对非合规名
        // 包了 `claude-synaroute-`；选中后客户端发回别名，先剥掉再走映射/三档/原生。
        let requested_model = unwrap_gateway_model_id(requested_model);

        // 1. 映射命中（精确名，最高优先级）
        if let Some(m) = self
            .mappings
            .iter()
            .find(|m| m.expected_name == requested_model)
        {
            return (m.real_name.clone(), ModelResolveKind::Mapping);
        }
        // 2. 三档命中（家族级：按模型名包含 haiku/sonnet/opus 子串匹配，CC 版本号常变故不精确匹配）
        if let Some(tier) = self.match_tier(requested_model) {
            return (tier, ModelResolveKind::Tier);
        }
        // 3. 本 Key 原生支持该模型名
        if self.models.iter().any(|m| m.real_name == requested_model) {
            return (requested_model.to_string(), ModelResolveKind::Native);
        }
        // 4. 默认兜底模型
        if let Some(dm) = self
            .default_model
            .as_ref()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
        {
            return (dm.to_string(), ModelResolveKind::Default);
        }
        // 5. 第一个模型
        if let Some(first) = self.models.first() {
            return (first.real_name.clone(), ModelResolveKind::First);
        }
        // 6. 透传（透传剥后的名字，避免把别名原样打给上游）
        (requested_model.to_string(), ModelResolveKind::Passthrough)
    }

    /// 三档家族匹配：`requested` 含 opus/sonnet/haiku 子串且配了对应档位 → 返回该档真实名。
    /// 先判 opus、再 sonnet、再 haiku（名称互不包含，顺序不影响结果，仅为可读）。
    /// 返回的档位真实名 trim 后为空视为未配。
    fn match_tier(&self, requested_model: &str) -> Option<String> {
        // 三档（快/中/强 = haiku/sonnet/opus）是 Claude Code 语义：它按任务发带
        // opus/sonnet/haiku 子串的模型名才触发档位改写。Codex 发 GPT 名或经应用内下拉
        // 覆盖的对外名（可能含 claude-*opus* 之类），一旦落到三档会被误改写到无关档位真实名
        // （如 claude-opus-4-8 → deepseek-reasoner）。故 Codex 分类一律不走三档，从后端根治
        // 误改写——不依赖前端保存守卫（旧数据可能仍带三档字段）。
        if matches!(self.category_id, CategoryType::Codex) {
            return None;
        }
        let lower = requested_model.to_ascii_lowercase();
        let pick = |v: &Option<String>| {
            v.as_ref()
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
        };
        if lower.contains("opus") {
            return pick(&self.tier_opus);
        }
        if lower.contains("sonnet") {
            return pick(&self.tier_sonnet);
        }
        if lower.contains("haiku") {
            return pick(&self.tier_haiku);
        }
        None
    }

    /// 本 Key 对客户端「可点选」的对外模型名集合，供 `/v1/models` 发现端点使用。
    ///
    /// 对齐 cc-switch：映射表是对外列表的主来源，避免「映射对外名 + 上游真实名」双暴露
    /// （用户配了 `opus-4-7→grok-4.5` 时，选择器只应看到对外名，不该再冒出 `grok-4.5`）。
    ///
    /// 规则（有序、去重）：
    /// 1. **有任意非空映射** → 只暴露映射的 `expected_name`（对外名）；
    ///    `models` 真实名仅作上游解析/探测素材，不进发现列表。
    /// 2. **无映射** → 暴露 `models` 的 `real_name`（直连/原生场景）。
    /// 3. 不论 1/2，已配的三档仍追加 Claude 家族代表名（`claude-*-4-5`），
    ///    因为 CLI 内置档会发这些名，需能被 `match_tier` 命中且在 From gateway 可见。
    pub fn serviceable_models(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut push = |name: &str| {
            let name = name.trim();
            if !name.is_empty() && !out.iter().any(|x| x == name) {
                out.push(name.to_string());
            }
        };
        let has_mapping = self
            .mappings
            .iter()
            .any(|mp| !mp.expected_name.trim().is_empty() && !mp.real_name.trim().is_empty());
        if has_mapping {
            for mp in &self.mappings {
                if mp.real_name.trim().is_empty() {
                    continue;
                }
                push(&mp.expected_name);
            }
        } else {
            for m in &self.models {
                push(&m.real_name);
            }
        }
        // 三档配了就把对应 Claude 家族代表名纳入——CC 会发这些名，能被 match_tier 命中。
        if self.tier_opus.as_ref().is_some_and(|s| !s.trim().is_empty()) {
            push("claude-opus-4-5");
        }
        if self.tier_sonnet.as_ref().is_some_and(|s| !s.trim().is_empty()) {
            push("claude-sonnet-4-5");
        }
        if self.tier_haiku.as_ref().is_some_and(|s| !s.trim().is_empty()) {
            push("claude-haiku-4-5");
        }
        out
    }

    /// 某**对外名**对应的上游模型上下文窗口（token 数）；查不到返回 None。
    ///
    /// 用于桌面端 gateway 档的 `supports1m` 断言：官方文档明确它是「**你对自己部署做的能力
    /// 断言**，只对确认支持 1M 窗口的模型设置」，故必须有依据、不能一律写 true——上游实际
    /// 不支持时，桌面端会给出一个必然失败的选项。
    ///
    /// 解析路径：对外名 → `resolve_model_detail` 得到真实名 → 在 `models` 里按真实名查
    /// `context_window`。之所以要先 resolve：对外名可能是映射的 `expected_name`
    /// （如 `claude-opus-4-8`），而窗口大小记在真实名（如 `glm-4.6`）那条 `ModelInfo` 上。
    ///
    /// **只认命中路径**（Mapping / Tier / Native）。兜底路径（Default / First / Passthrough）
    /// 一律返回 None：那时 `resolve_model` 返回的是「这个 Key 不认识该名字，随便给你一个」，
    /// 拿兜底模型的窗口去断言请求名的能力毫无依据 —— 会写出「对外名 X 支持 1M」而真实落点
    /// 只有 200k，桌面端于是给出一个必然被截断的选项，恰是本函数要避免的后果。
    pub fn context_window_for_outward(&self, outward_name: &str) -> Option<u32> {
        let (real, kind) = self.resolve_model_detail(outward_name);
        if !matches!(
            kind,
            ModelResolveKind::Mapping | ModelResolveKind::Tier | ModelResolveKind::Native
        ) {
            return None;
        }
        self.models
            .iter()
            .find(|m| m.real_name == real)
            .and_then(|m| m.context_window)
    }

    /// 选一个「保证能被上游接受」的真实模型名，用于真实补全健康探测。
    ///
    /// 关键：真实探测必须发上游认识的真实名，而非对外映射名。此前只看 default_model / models，
    /// 导致「只配了自由映射、没填模型列表」的 Key（探测模型选不出）退回轻量 /models 探测，
    /// 被上游 401/403 误判失败而反复熔断——即便真实业务（映射改写后）完全正常。
    ///
    /// 优先级：第一条映射的真实名 > 默认兜底 > 模型列表首个 > 三档真实名。都没有则 None
    /// （调用方据此退回轻量探测）。
    pub fn probe_model(&self) -> Option<String> {
        let clean = |s: &str| {
            let t = s.trim();
            (!t.is_empty()).then(|| t.to_string())
        };
        if let Some(m) = self.mappings.iter().find_map(|m| clean(&m.real_name)) {
            return Some(m);
        }
        if let Some(dm) = self.default_model.as_deref().and_then(clean) {
            return Some(dm);
        }
        if let Some(first) = self.models.iter().find_map(|m| clean(&m.real_name)) {
            return Some(first);
        }
        [&self.tier_opus, &self.tier_sonnet, &self.tier_haiku]
            .into_iter()
            .find_map(|t| t.as_deref().and_then(clean))
    }
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
    /// 用户配置的工作目录（文件检索的根路径）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub work_dir: Option<String>,
    /// 注入文件内容的 token 上限（默认 50000）
    #[serde(default = "default_max_context_tokens")]
    pub max_context_tokens: u32,
    /// 是否启用文件检索
    #[serde(default)]
    pub retrieval_enabled: bool,
    /// 是否自动跟随最近活动项目（从 Claude CLI/Codex 会话历史读取）。
    /// 开启后运行时忽略 `work_dir`，实时用检测到的最新活动项目路径。
    #[serde(default)]
    pub auto_follow_active: bool,
}

fn default_max_context_tokens() -> u32 {
    50_000
}

/// 单个文件的修改结果
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppliedChange {
    pub path: String,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// 聚合运行结果（内部标签枚举，序列化为 tagged JSON 给前端）。
///
/// 注意：serde 的内部标签（`#[serde(tag)]`）**不支持 newtype 变体包裹基本类型**
/// （如 `Plan(String)` 会在运行时报 "cannot serialize tagged newtype variant ...
/// containing a string"，导致 aggregate_plan 命令必然失败）。故所有变体必须是
/// struct 变体。字段名对齐前端契约 src/types.ts::AggregateResult（content / filesModified）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "resultType", rename_all = "camelCase")]
pub enum AggregateResult {
    /// 决策者输出的修改计划（Phase1）。work_dir 为本次解析定下的工作目录，
    /// 前端须在 Phase2 原样回传，避免 auto-follow 期间目录漂移把改动写进别的项目。
    #[serde(rename = "plan")]
    Plan {
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        work_dir: Option<String>,
    },
    /// 执行结果（Phase2）：content 为决策者原始输出，files_modified 为实际写入的文件路径。
    #[serde(rename = "applied")]
    Applied {
        content: String,
        files_modified: Vec<String>,
    },
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
    /// 该条是否带链路快照。**列表接口会把 `trace` 正文剥掉**（见 `Store::strip_trace`），
    /// 但前端仍需知道「这行能不能展开看详情」，故单独留一个布尔位——它只有 1 字节，
    /// 而正文最坏 40000 字符。展开时前端按事件 id 走 `get_event_trace` 单取一条。
    #[serde(default)]
    pub has_trace: bool,
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
    /// 诊断：在 request 日志里额外附「下游原始请求体」（转换前 Codex 发来的原样）。
    /// 默认关——下游原始 body 可达十几万字符（Codex 带大量 system/skills 清单），
    /// 常态记录既占日志又会把「转换后发往上游」段挤出截断窗口。仅排障（如核对推理强度
    /// 注入、reasoning 字段）时开启。关闭时 request 只记转换后 body（含 thinking 映射结果）。
    #[serde(default)]
    pub log_downstream_raw_enabled: bool,
    /// 后台定时健康检查间隔（秒）。用户可配置，默认 60s。
    #[serde(default = "default_health_interval")]
    pub health_check_interval_secs: u64,
    /// 日志文件目录（None 时使用默认 %APPDATA%\SynaRoute\logs）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log_dir: Option<String>,
    /// 是否启用内置 MCP 服务器（开启即随应用启动）
    #[serde(default)]
    pub mcp_enabled: bool,
    /// MCP 服务器监听端口（默认 9527）
    #[serde(default = "default_mcp_port")]
    pub mcp_port: u16,
    /// 已自动注册 MCP 的目标工具分类（如 ["claude-cli"]）。端口变化时据此重写这些客户端配置，
    /// 关闭 MCP 时据此移除。避免每次都盲扫三个工具。
    #[serde(default)]
    pub mcp_registered_categories: Vec<String>,
    /// 上游临时错误（502/503/504 及连接失败）自动重试。默认开启：中转商偶发网关故障时
    /// 自动重试 1-2 次，大幅减少「偶发 502 → 熔断 → 无响应」。
    #[serde(default = "default_true")]
    pub upstream_retry_enabled: bool,
    /// 健康探测用「真实补全请求」而非轻量连通探测。默认关（消耗少量额度）。
    /// 开启后健康「可用/熔断」与真实业务一致，不再出现「连通正常却熔断」的割裂。
    #[serde(default)]
    pub health_probe_real_completion: bool,
    /// 真实补全探测使用的「测试消息」候选列表（全局共享）。每次探测从中随机取一条作为 prompt，
    /// 避免上游对完全相同的极短请求做缓存/风控误判。空列表时回退到内置 "hi"。
    /// 仅 health_probe_real_completion 开启时生效。
    #[serde(default)]
    pub health_probe_test_messages: Vec<String>,
    /// 大脑聚合详细快照：开启后每次聚合把成员完整答案、汇总产物、决策者入参/回答
    /// 落进可展开 trace（体量可达数十万字符）。默认关，避免每轮聚合都写重型日志、
    /// 徒增磁盘 IO；状态行（参与者成功/失败、汇总/决策者返回）不受此开关影响，始终记录。
    #[serde(default)]
    pub aggregate_trace_enabled: bool,
    /// 各分类当前选定的「对外模型名」（key=分类字符串，value=对外模型名）。
    /// 借鉴 EchoBird：某些客户端（如 Codex）的模型菜单是内置固定清单、拉不到中转的真实模型，
    /// 故在本应用内选模型，代理转发时用此值覆盖客户端发来的模型名，再走 resolve_model 解析。
    /// 每请求实时读取（get_settings），改选即时生效、免重启客户端。
    /// 后端自管字段：由专用命令 set_active_model 更新，不随通用 save_settings 的陈旧快照覆盖
    /// （与 mcp_* 字段同一保全策略）。
    #[serde(default)]
    pub active_models: std::collections::HashMap<String, String>,
    /// 托盘「Codex 模型」快切子菜单开关。开启后右键托盘可直接切换 Codex 当前对外模型，
    /// 免打开主窗口（借鉴 cc-switch 托盘切换范式）。默认开。关闭则托盘只留显示/退出。
    #[serde(default = "default_true")]
    pub tray_model_switch_enabled: bool,
    /// 各分类的「默认推理强度」（key=分类字符串，value=effort 档位 low/medium/high/xhigh）。
    /// 缘由：Codex Desktop 对自定义 provider 不下发 reasoning.effort（只发 reasoning.summary），
    /// 客户端 UI 设的强度传不到上游。故在此配一个默认值，转发时若下游 body 无 effort 就注入，
    /// 让 Codex→Claude(thinking)/Chat(reasoning_effort) 的推理强度能真正生效。
    /// 仅在下游为 Responses(Codex) 且上游非原生 Responses 时注入（OpenAI 官方 Responses 直通不碰）。
    /// 后端自管字段：由专用命令 set_active_effort 更新，不随通用 save_settings 的陈旧快照覆盖
    /// （与 active_models / mcp_* 同一保全策略）。空/未配 = 不注入，保持现状。
    #[serde(default)]
    pub active_efforts: std::collections::HashMap<String, String>,
    /// 各分类代理的「首选监听端口」（key=分类字符串，value=端口）。
    /// 缘由：早期用 OS 随机端口（bind 0），SynaRoute 每次重启端口都变，而客户端
    /// （Codex/Claude）只在自身启动时读一次 config，不追踪端口变化 → 重启后客户端仍打旧端口、
    /// 连不上（error sending request）。改为「粘滞固定端口」：各分类有稳定默认端口，
    /// config.toml 写一次即长期有效；端口被占时在 [port, port+FALLBACK] 内向上兜底并写回此处
    /// 作为下次首选（与 mcp_port 同一粘滞策略）。缺省时用 default_proxy_port(category)。
    #[serde(default)]
    pub proxy_ports: std::collections::HashMap<String, u16>,
}

/// 各分类代理的默认首选端口。选用冷门段避开常见软件占用
/// （避开 8080/8888/3000/5173/7890/9527 等），且三分类连续好记。
pub fn default_proxy_port(category: &str) -> u16 {
    match category {
        "claude-cli" => 47100,
        "codex" => 47101,
        "claude-desktop" => 47102,
        _ => 47103,
    }
}

fn default_true() -> bool {
    true
}

fn default_mcp_port() -> u16 {
    9527
}

fn default_language() -> String {
    "zh".into()
}

fn default_health_interval() -> u64 {
    60
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
            log_downstream_raw_enabled: false,
            health_check_interval_secs: default_health_interval(),
            log_dir: None,
            mcp_enabled: false,
            mcp_port: default_mcp_port(),
            mcp_registered_categories: Vec::new(),
            upstream_retry_enabled: true,
            health_probe_real_completion: false,
            health_probe_test_messages: Vec::new(),
            aggregate_trace_enabled: false,
            active_models: std::collections::HashMap::new(),
            active_efforts: std::collections::HashMap::new(),
            tray_model_switch_enabled: true,
            proxy_ports: std::collections::HashMap::new(),
        }
    }
}

/// MCP 服务器运行状态（供设置页展示）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpStatus {
    pub running: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// 最近一次启动失败的原因（成功时为 None）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn model(name: &str) -> ModelInfo {
        ModelInfo {
            real_name: name.into(),
            source: "fetched".into(),
            fetched_at: None,
            context_window: None,
        }
    }

    fn key_with(models: Vec<ModelInfo>, mappings: Vec<ModelMapping>, default_model: Option<&str>) -> ProviderKey {
        ProviderKey {
            id: "k".into(),
            category_id: CategoryType::ClaudeCli,
            name: "k".into(),
            vendor: "custom".into(),
            base_url: "https://x".into(),
            protocol: Protocol::Anthropic,
            has_secret: true,
            enabled: true,
            priority: 0,
            headers_json: None,
            params: KeyParams::default(),
            models,
            mappings,
            default_model: default_model.map(|s| s.to_string()),
            tier_haiku: None,
            tier_sonnet: None,
            tier_opus: None,
            health: HealthState::default(),
        }
    }

    fn mapping(expected: &str, real: &str) -> ModelMapping {
        ModelMapping { id: "m".into(), expected_name: expected.into(), real_name: real.into() }
    }

    #[test]
    fn resolve_prefers_mapping() {
        // 映射优先级最高，即便该 Key 原生也有同名模型/默认兜底也走映射
        let k = key_with(vec![model("opus-4-8")], vec![mapping("opus-4-8", "glm-5")], Some("kimi-k2"));
        assert_eq!(k.resolve_model("opus-4-8"), "glm-5");
        assert_eq!(
            k.resolve_model_detail("opus-4-8"),
            ("glm-5".into(), ModelResolveKind::Mapping)
        );
    }

    #[test]
    fn resolve_uses_native_model_when_supported() {
        // 无映射，但该 Key 原生支持请求的模型名 → 原样使用（不误用兜底/首个）
        let k = key_with(vec![model("gpt-4o"), model("gpt-4o-mini")], vec![], Some("gpt-4o-mini"));
        assert_eq!(k.resolve_model("gpt-4o"), "gpt-4o");
        assert_eq!(
            k.resolve_model_detail("gpt-4o"),
            ("gpt-4o".into(), ModelResolveKind::Native)
        );
    }

    #[test]
    fn resolve_falls_back_to_default_model() {
        // 请求模型该 Key 没有、无映射 → 用用户配置的默认兜底模型（优先于「第一个模型」）
        let k = key_with(vec![model("glm-4.6"), model("glm-4.5")], vec![], Some("glm-4.6"));
        assert_eq!(k.resolve_model("opus-4-8"), "glm-4.6");
        assert_eq!(
            k.resolve_model_detail("opus-4-8"),
            ("glm-4.6".into(), ModelResolveKind::Default)
        );
    }

    #[test]
    fn resolve_falls_back_to_first_model() {
        // 没配默认兜底 → 取模型列表第一个
        let k = key_with(vec![model("deepseek-chat"), model("deepseek-reasoner")], vec![], None);
        assert_eq!(k.resolve_model("opus-4-8"), "deepseek-chat");
        assert_eq!(
            k.resolve_model_detail("opus-4-8"),
            ("deepseek-chat".into(), ModelResolveKind::First)
        );
    }

    #[test]
    fn resolve_passthrough_when_no_models() {
        // 什么都没配（未拉模型、无映射、无默认）→ 透传原请求名，保持旧行为
        let k = key_with(vec![], vec![], None);
        assert_eq!(k.resolve_model("opus-4-8"), "opus-4-8");
        assert_eq!(
            k.resolve_model_detail("opus-4-8"),
            ("opus-4-8".into(), ModelResolveKind::Passthrough)
        );
    }

    #[test]
    fn resolve_ignores_blank_default_model() {
        // 默认兜底为空白串视作未配置 → 退回第一个模型
        let k = key_with(vec![model("m-first")], vec![], Some("   "));
        assert_eq!(k.resolve_model("unknown"), "m-first");
    }

    /// 造一个配了三档的 Key（其余空）。
    fn key_with_tiers(haiku: Option<&str>, sonnet: Option<&str>, opus: Option<&str>) -> ProviderKey {
        let mut k = key_with(vec![], vec![], None);
        k.tier_haiku = haiku.map(|s| s.to_string());
        k.tier_sonnet = sonnet.map(|s| s.to_string());
        k.tier_opus = opus.map(|s| s.to_string());
        k
    }

    #[test]
    fn resolve_tier_matches_family_substring() {
        // CC 发家族名（含 sonnet/opus/haiku 子串、带版本号）→ 命中对应档位真实名
        let k = key_with_tiers(Some("glm-4.5-air"), Some("glm-4.6"), Some("deepseek-reasoner"));
        assert_eq!(k.resolve_model("claude-sonnet-4-5-20250929"), "glm-4.6");
        assert_eq!(k.resolve_model("claude-opus-4-5"), "deepseek-reasoner");
        assert_eq!(k.resolve_model("claude-3-5-haiku-20241022"), "glm-4.5-air");
    }

    #[test]
    fn resolve_mapping_beats_tier() {
        // 自由映射（精确名）优先于三档：用户手配的精确意图永远赢
        let mut k = key_with(vec![], vec![mapping("claude-sonnet-4-5", "kimi-k2")], None);
        k.tier_sonnet = Some("glm-4.6".into());
        assert_eq!(k.resolve_model("claude-sonnet-4-5"), "kimi-k2");
    }

    #[test]
    fn resolve_tier_beats_native_and_default() {
        // 三档优先于「原生支持 / 默认兜底 / 首个」：CC 家族名应落三档而非兜底
        let mut k = key_with(vec![model("claude-sonnet-4-5"), model("glm-4.6")], vec![], Some("glm-4.6"));
        k.tier_sonnet = Some("glm-4.5".into());
        assert_eq!(k.resolve_model("claude-sonnet-4-5"), "glm-4.5");
    }

    #[test]
    fn resolve_unconfigured_tier_falls_through() {
        // 只配了 sonnet；请求 opus 家族 → 三档未命中，退回旧兜底链（此处首个模型）
        let mut k = key_with(vec![model("glm-4.6")], vec![], None);
        k.tier_sonnet = Some("glm-4.5".into());
        assert_eq!(k.resolve_model("claude-opus-4-5"), "glm-4.6");
    }

    #[test]
    fn resolve_blank_tier_ignored() {
        // 档位为空白串视作未配 → 不命中，走兜底透传
        let k = key_with_tiers(None, Some("   "), None);
        assert_eq!(k.resolve_model("claude-sonnet-4-5"), "claude-sonnet-4-5");
    }

    /// 桌面端模型名判据必须与 `app.asar` 的 `sD()` 逐例一致。
    ///
    /// 用例是把官方 `sD()` 原样搬到 Node 里跑出来的期望值（判据来源与反查方法见
    /// `is_desktop_acceptable_model_id` 上方注释），不是凭规则推的。
    #[test]
    fn desktop_model_check_matches_official_asar_semantics() {
        use super::is_desktop_acceptable_model_id as ok;

        // ---- 接受 ----
        for name in [
            "claude-opus-4-8",
            "claude-opus-5",
            "claude-opus-4-8-thinking",
            "claude-3-5-haiku",
            "opus",              // nhe: 裸档位名
            "opus-4-8",          // nhe: 档位名 + 数字段（连字符 + 数字/点）
            "sonnet-4.5",
            "mythos",
            "fable-5",
            "anthropic/claude-opus-5",
            "my-opus-pro",       // Snt 只要求「包含」关键词，不要求前缀
            "x-claude-y",
            "claude",
            "OPUS",              // 大小写不敏感
        ] {
            assert!(ok(name), "桌面端应接受 {name:?}");
        }

        // ---- 拒绝：厂商名黑名单（Ent）----
        for name in [
            "glm-4.6",
            "grok-4.5",
            "gpt-5",
            "kimi-k2",
            "deepseek-v4-pro",
            "qwen3-max",
            "solar-pro",
            "kat-coder-1",
        ] {
            assert!(!ok(name), "桌面端应拒绝厂商名 {name:?}");
        }

        // ---- 拒绝：黑名单优先于关键词（本次最容易踩的一条）----
        // `claude-synaroute-` 前缀是给 Claude Code CLI 用的（它只判前缀、无黑名单）。
        // 桌面端这边 `glm`/`grok` 照样命中黑名单 → 前缀救不了，别指望它。
        assert!(
            !ok("claude-synaroute-glm-4.6"),
            "claude-synaroute- 前缀对桌面端无效：glm 仍命中黑名单"
        );
        assert!(!ok("claude-synaroute-grok-4.5"));
        assert!(!ok("claude-gpt-5"), "含 claude 也救不了 gpt");
        assert!(!ok("claude-opus-glm"), "含 claude+opus 也救不了 glm");

        // ---- 拒绝：黑名单里的词边界项（\bling\b / \bunic\b / \bds- / \bk2\. / \bm2\.）----
        assert!(!ok("ling-1.5"));
        assert!(!ok("unic-1"));
        assert!(!ok("ds-v3"));
        assert!(!ok("k2.5"));
        assert!(!ok("m2.1"));
        // 词边界的反面：这些名字里虽含 ling/unic 的字母序列，但左侧是 \w，官方 \b 不匹配，
        // 故仅因黑名单不成立；它们最终仍因「不含任何允许关键词」被拒。
        // 用带关键词的形式验证词边界确实生效（sibling 含 ling 但不构成 \bling\b）。
        assert!(
            ok("claude-sibling-opus"),
            "sibling 里的 ling 前面是 \\w，不构成 \\bling\\b，不该被误拒"
        );

        // ---- 拒绝：phi\d（phi- 已在普通子串项里）----
        assert!(!ok("phi4"));
        assert!(!ok("phi-3"));
        assert!(
            ok("claude-opus-phix"),
            "phi 后不是数字、也不是 phi-，不该命中 phi\\d"
        );

        // ---- 拒绝：不含任何允许关键词 ----
        assert!(!ok("sibling-model"), "既无关键词又非裸档位名");
        assert!(!ok(""), "空名不接受");
        assert!(!ok("   "), "空白名不接受");
    }

    /// CLI 与桌面端的判据是两套规则，不能互相顶替。
    #[test]
    fn cli_and_desktop_model_checks_are_different_rules() {
        // 非合规名：CLI 靠包前缀救回（客户端只看 claude/anthropic 前缀）；桌面端救不回。
        let wrapped = to_gateway_model_id("glm-4.6");
        assert_eq!(wrapped, "claude-synaroute-glm-4.6");
        assert!(
            is_cli_discoverable_model_id(&wrapped),
            "CLI 认前缀，包完就能在 /model 里展示"
        );
        assert!(
            !is_desktop_acceptable_model_id(&wrapped),
            "桌面端有厂商名黑名单且优先，包前缀无效——这正是桌面端必须用合规对外名的原因"
        );
        // 反向：裸档位名桌面端接受，但 CLI 的前缀判据不认（会被包上前缀）。
        assert!(is_desktop_acceptable_model_id("opus"));
        assert!(!is_cli_discoverable_model_id("opus"));
    }

    #[test]
    fn serviceable_models_includes_configured_tiers() {
        // 配了 sonnet/opus 档 → 家族代表名进入可服务集合，供 /v1/models 展示
        let k = key_with_tiers(None, Some("glm-4.6"), Some("glm-4.5"));
        let sm = k.serviceable_models();
        assert!(sm.contains(&"claude-sonnet-4-5".to_string()));
        assert!(sm.contains(&"claude-opus-4-5".to_string()));
        assert!(!sm.contains(&"claude-haiku-4-5".to_string()));
    }

    #[test]
    fn serviceable_models_mappings_only_when_present() {
        // 有映射：只暴露对外名，不并入 models 真实名（避免 /model 双轨）
        let k = key_with(
            vec![model("glm-5.1"), model("glm-5.2"), model("opus-4-7")],
            vec![mapping("opus-4-7", "glm-5.1"), mapping("opus-4-8", "glm-5.2")],
            None,
        );
        assert_eq!(k.serviceable_models(), vec!["opus-4-7", "opus-4-8"]);
    }

    #[test]
    fn serviceable_models_falls_back_to_model_list_without_mappings() {
        // 无映射：暴露原生模型列表（直连场景）
        let k = key_with(vec![model("grok-4.5"), model("glm-4.6")], vec![], None);
        assert_eq!(k.serviceable_models(), vec!["grok-4.5", "glm-4.6"]);
    }

    #[test]
    fn serviceable_models_skips_incomplete_mapping_rows() {
        // 缺 expected 或 real 的半成品映射不进发现列表
        let k = key_with(
            vec![model("grok-4.5")],
            vec![
                mapping("claude-opus-4-7", "grok-4.5"),
                mapping("", "grok-4.5"),
                mapping("orphan", ""),
            ],
            None,
        );
        assert_eq!(k.serviceable_models(), vec!["claude-opus-4-7"]);
    }

    #[test]
    fn serviceable_models_mappings_plus_tiers() {
        // 有映射 + 配了 sonnet 档 → 对外名 ∪ 家族代表名
        let mut k = key_with(
            vec![model("glm-4.6")],
            vec![mapping("claude-opus-4-7", "glm-4.6")],
            None,
        );
        k.tier_sonnet = Some("glm-4.6".into());
        assert_eq!(
            k.serviceable_models(),
            vec!["claude-opus-4-7", "claude-sonnet-4-5"]
        );
    }

    #[test]
    fn serviceable_models_empty_when_nothing_configured() {
        let k = key_with(vec![], vec![], None);
        assert!(k.serviceable_models().is_empty());
    }

    #[test]
    fn codex_category_never_matches_tier() {
        // Codex 分类禁用三档匹配：即使旧数据残留了三档配置，含 opus/sonnet/haiku 子串的
        // 模型名（如应用内选定的 claude-opus-4-8）也不得被误改写到档位真实名。
        // 三档是 Claude Code 的 opus/sonnet/haiku 语义，Codex 发不出触发它的意图。
        let mut k = key_with(vec![model("claude-opus-4-8")], vec![], None);
        k.category_id = CategoryType::Codex;
        k.tier_opus = Some("deepseek-reasoner".into());
        // 含 "opus" 但因分类是 Codex → 不走三档，落到原生同名（Native）。
        let (real, kind) = k.resolve_model_detail("claude-opus-4-8");
        assert_eq!(real, "claude-opus-4-8", "Codex 不应把 opus 名改写到 deepseek-reasoner");
        assert_eq!(kind, ModelResolveKind::Native);
    }

    #[test]
    fn non_codex_category_still_matches_tier() {
        // 对照：Claude CLI 分类三档正常生效（回归保护，勿因 Codex 守卫误伤主场景）。
        let mut k = key_with(vec![model("glm-4.6")], vec![], None);
        k.tier_opus = Some("deepseek-reasoner".into());
        let (real, kind) = k.resolve_model_detail("claude-opus-4-7");
        assert_eq!(real, "deepseek-reasoner");
        assert_eq!(kind, ModelResolveKind::Tier);
    }

    #[test]
    fn gateway_alias_wraps_non_claude_names_only() {
        assert_eq!(to_gateway_model_id("grok-4.5"), "claude-synaroute-grok-4.5");
        assert_eq!(to_gateway_model_id("glm-4.6"), "claude-synaroute-glm-4.6");
        // 已合规：原样
        assert_eq!(to_gateway_model_id("claude-opus-4-5"), "claude-opus-4-5");
        assert_eq!(to_gateway_model_id("anthropic.claude-v2"), "anthropic.claude-v2");
        // 空白
        assert_eq!(to_gateway_model_id("  "), "");
    }

    #[test]
    fn gateway_alias_unwrap_roundtrip() {
        assert_eq!(unwrap_gateway_model_id("claude-synaroute-grok-4.5"), "grok-4.5");
        assert_eq!(unwrap_gateway_model_id("claude-opus-4-5"), "claude-opus-4-5");
        assert_eq!(unwrap_gateway_model_id("grok-4.5"), "grok-4.5");
    }

    #[test]
    fn resolve_unwraps_gateway_alias_to_native() {
        // CLI 选中网关包装 id → 剥前缀 → 命中原生列表
        let k = key_with(vec![model("grok-4.5")], vec![], Some("grok-4.5"));
        assert_eq!(
            k.resolve_model_detail("claude-synaroute-grok-4.5"),
            ("grok-4.5".into(), ModelResolveKind::Native)
        );
    }

    #[test]
    fn resolve_unwraps_gateway_alias_then_mapping() {
        let k = key_with(
            vec![model("glm-5.2")],
            vec![mapping("opus-4-8", "glm-5.2")],
            None,
        );
        // 对外名本身不合规时也会被包成 claude-synaroute-opus-4-8；剥后走映射
        assert_eq!(
            k.resolve_model_detail("claude-synaroute-opus-4-8"),
            ("glm-5.2".into(), ModelResolveKind::Mapping)
        );
    }

    #[test]
    fn probe_model_uses_mapping_real_name_when_only_mappings() {
        // 核心回归：只配自由映射、无模型列表的 Key（如深度求索）应能选出真实名探测，
        // 而非退回 None（此前 None → 退回 /models 轻量探测 → 被 401/403 误杀熔断）。
        let k = key_with(
            vec![],
            vec![mapping("claude-opus-4-8", "deepseek-v4-pro")],
            None,
        );
        assert_eq!(k.probe_model().as_deref(), Some("deepseek-v4-pro"));
    }

    #[test]
    fn probe_model_prefers_mapping_over_default_and_models() {
        let k = key_with(
            vec![model("m-first")],
            vec![mapping("a", "map-real")],
            Some("default-m"),
        );
        assert_eq!(k.probe_model().as_deref(), Some("map-real"));
    }

    #[test]
    fn probe_model_falls_back_to_default_then_models_then_tier() {
        // 无映射：默认模型优先
        let k = key_with(vec![model("m-first")], vec![], Some("default-m"));
        assert_eq!(k.probe_model().as_deref(), Some("default-m"));
        // 无映射无默认：模型列表首个
        let k = key_with(vec![model("m-first")], vec![], None);
        assert_eq!(k.probe_model().as_deref(), Some("m-first"));
        // 全空但配了三档：取三档真实名（opus 优先）
        let mut k = key_with(vec![], vec![], None);
        k.tier_sonnet = Some("glm-4.6".into());
        assert_eq!(k.probe_model().as_deref(), Some("glm-4.6"));
    }

    #[test]
    fn probe_model_none_when_nothing_configured() {
        let k = key_with(vec![], vec![], None);
        assert!(k.probe_model().is_none());
    }

    // ---- Protocol 三态 + serde alias 迁移（最高危：旧配置无损升级）----

    #[test]
    fn protocol_legacy_openai_alias_migrates_to_chat() {
        // 旧配置里 protocol:"openai" 必须无损反序列化为 OpenaiChat，否则已存的
        // DeepSeek/GLM/Kimi 等 Key 全部失效。
        let p: Protocol = serde_json::from_str("\"openai\"").unwrap();
        assert_eq!(p, Protocol::OpenaiChat);
    }

    #[test]
    fn protocol_roundtrip_and_new_variants() {
        // 序列化用新名，且三态各自可往返。
        assert_eq!(serde_json::to_string(&Protocol::OpenaiChat).unwrap(), "\"openai_chat\"");
        assert_eq!(serde_json::to_string(&Protocol::OpenaiResponses).unwrap(), "\"openai_responses\"");
        assert_eq!(serde_json::to_string(&Protocol::Anthropic).unwrap(), "\"anthropic\"");
        for p in [Protocol::Anthropic, Protocol::OpenaiChat, Protocol::OpenaiResponses] {
            let s = serde_json::to_string(&p).unwrap();
            assert_eq!(serde_json::from_str::<Protocol>(&s).unwrap(), p);
        }
    }

    #[test]
    fn protocol_family_and_paths() {
        assert!(Protocol::OpenaiChat.is_openai());
        assert!(Protocol::OpenaiResponses.is_openai());
        assert!(!Protocol::Anthropic.is_openai());
        assert_eq!(Protocol::OpenaiResponses.completion_path(), "/v1/responses");
        assert_eq!(Protocol::OpenaiChat.completion_path(), "/v1/chat/completions");
        assert_eq!(Protocol::Anthropic.completion_path(), "/v1/messages");
    }

    #[test]
    fn provider_key_with_legacy_openai_deserializes() {
        // 整个 ProviderKey 带旧 protocol:"openai" 的 JSON 应能反序列化（迁移端到端）。
        let json = r#"{
            "id":"k","categoryId":"codex","name":"n","vendor":"deepseek",
            "baseUrl":"https://api.deepseek.com","protocol":"openai",
            "hasSecret":true,"enabled":true,"priority":0
        }"#;
        let k: ProviderKey = serde_json::from_str(json).unwrap();
        assert_eq!(k.protocol, Protocol::OpenaiChat);
    }

    #[test]
    fn app_settings_tray_toggle_defaults_true_for_legacy_config() {
        // 旧 config.json 无 trayModelSwitchEnabled 字段：反序列化应默认为 true，
        // 避免升级用户的托盘快切被意外关掉（default_true）。
        let json = r#"{"theme":"system","language":"zh"}"#;
        let s: AppSettings = serde_json::from_str(json).unwrap();
        assert!(s.tray_model_switch_enabled, "旧配置缺该字段应默认开启");
        // active_models 缺失应为空 map（不 panic）。
        assert!(s.active_models.is_empty());
    }
}
