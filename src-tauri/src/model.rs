//! 数据模型 —— 严格对齐前端 src/types.ts 的 IPC 契约。
//! serde 统一 camelCase，使 Rust snake_case 字段与前端 camelCase 自动映射。

use serde::{Deserialize, Serialize};

/// 三个目标工具分类
///
/// `Default` = `ClaudeCli`，**仅为 `ProviderKey::default()` 的测试便利**而存在
/// （见该结构的文档）。业务逻辑里绝不要依赖这个默认值 —— 分类必须来自用户选择，
/// 猜错会把 Key 写进错误的客户端配置。
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
pub enum CategoryType {
    #[default]
    #[serde(rename = "claude-cli")]
    ClaudeCli,
    #[serde(rename = "claude-desktop")]
    ClaudeDesktop,
    #[serde(rename = "codex")]
    Codex,
}

/// 一个分类的**全部固有属性**（P2-8：查表化）。
///
/// 建这张表是为了消灭「同一个属性散落在 8 处 match 里」的局面。散落的代价不是难看，
/// 而是**加第 4 个分类时漏改一处只会静默走错**：漏了端口就撞端口、漏了协议就发错格式、
/// 漏了模型名过滤就让桌面端模型选择器变空 —— 每一种都不报错，只在真机上表现为
/// 「某个分类莫名其妙不好使」。
///
/// 表建好之后，`meta()` 是**全仓唯一**的分类属性 match，禁止 `_ =>` 兜底：
/// 新增分类时编译器就是清单，逼你逐字段回答。这与 `Protocol` 的能力方法是同一条纪律。
#[derive(Debug, Clone, Copy)]
pub struct CategoryMeta {
    pub id: CategoryType,
    /// 落盘与 IPC 上的字符串形态。**改它等于改磁盘格式**，老配置会读不出来。
    pub wire_id: &'static str,
    /// 界面与托盘上的显示名
    pub display_name: &'static str,
    /// 代理的默认首选端口。选用冷门段避开常见软件占用（8080/8888/3000/5173/7890/9527…），
    /// 且三分类连续好记
    pub default_port: u16,
    /// 是否参与「三档（快/中/强 = haiku/sonnet/opus）」改写。
    /// Codex 为 false：它发 GPT 名或应用内下拉覆盖的对外名，落到三档会被误改写到无关档位。
    pub tier_rewrite: bool,
    /// 对外模型名是否受**硬过滤**约束。仅 Claude 桌面端为 true ——
    /// 不合规的名字会被它从模型列表里静默过滤掉，全被滤掉时选择器为空、
    /// 打开会话报 ModelsNotDiscoveredError（判据取自 app.asar）
    pub strict_model_id: bool,
    /// cc-switch 库里对应的 appType
    pub ccswitch_app_type: &'static str,
}

const ROW_CLAUDE_CLI: CategoryMeta = CategoryMeta {
    id: CategoryType::ClaudeCli,
    wire_id: "claude-cli",
    display_name: "Claude CLI",
    default_port: 47100,
    tier_rewrite: true,
    strict_model_id: false,
    ccswitch_app_type: "claude",
};

const ROW_CLAUDE_DESKTOP: CategoryMeta = CategoryMeta {
    id: CategoryType::ClaudeDesktop,
    wire_id: "claude-desktop",
    display_name: "Claude 桌面端",
    default_port: 47102,
    tier_rewrite: true,
    strict_model_id: true,
    ccswitch_app_type: "claude-desktop",
};

const ROW_CODEX: CategoryMeta = CategoryMeta {
    id: CategoryType::Codex,
    wire_id: "codex",
    display_name: "Codex",
    default_port: 47101,
    tier_rewrite: false,
    strict_model_id: false,
    ccswitch_app_type: "codex",
};

impl CategoryType {
    /// 全部分类（遍历用，如聚合 MCP 客户端超时联动需取各分类 total_timeout_ms 的最大值）。
    pub const ALL: [CategoryType; 3] = [
        CategoryType::ClaudeCli,
        CategoryType::ClaudeDesktop,
        CategoryType::Codex,
    ];

    /// 本分类的属性行。**全仓唯一的分类属性 match**，禁止 `_ =>` 兜底。
    ///
    /// 返回 `&'static` 走的是 rvalue static promotion：`CategoryMeta` 无 Drop、
    /// 无内部可变性，可安全提升。刻意不写成 `&TABLE[i]` —— 那种形式的常量提升
    /// 没有语言保证。
    pub fn meta(self) -> &'static CategoryMeta {
        match self {
            CategoryType::ClaudeCli => &ROW_CLAUDE_CLI,
            CategoryType::ClaudeDesktop => &ROW_CLAUDE_DESKTOP,
            CategoryType::Codex => &ROW_CODEX,
        }
    }

    pub fn as_str(&self) -> &'static str {
        self.meta().wire_id
    }

    /// 界面与托盘显示名
    pub fn display_name(&self) -> &'static str {
        self.meta().display_name
    }

    /// 从字符串解析分类（MCP 工具参数用）。未知值返回 None。
    pub fn from_str(s: &str) -> Option<Self> {
        Self::ALL.iter().map(|c| c.meta()).find(|m| m.wire_id == s).map(|m| m.id)
    }
}

/// `Default` = `Anthropic`，理由同 [`CategoryType`]：只为测试构造便利，
/// 业务侧的协议必须来自用户配置或厂商预设（猜错会用错端点与鉴权头）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Protocol {
    #[default]
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

    // ---- 能力方法（P2-2）：一律用**穷举 match**，不用 `is_openai()` 之类的二分 ----
    //
    // 为什么必须穷举：这些是「新增协议时逐项必须重新决定」的东西。原先散落 11 处
    // `matches!(p, Anthropic)` / `is_openai()` 形式的二分判断，加第 4 种协议（如 Gemini）时
    // **不会编译失败**，而是静默按「非 Anthropic 即 OpenAI」处理 —— 于是鉴权头被套上
    // `Bearer`、客户端身份头被套 Codex UA，不 panic、不报错，直接向上游发错误的头，
    // 表现为 401 或 `client_restricted` 403，排查方向极易被误导到「Key 配错了」。
    //
    // 穷举 match 让编译器成为清单：加变体即报错，逐个方法回答「这个新协议该怎么办」。
    // 故这些方法里**不许出现 `_ =>` 兜底臂**。

    /// 鉴权头名与取值形态。
    pub fn auth_scheme(self) -> AuthScheme {
        match self {
            Protocol::Anthropic => AuthScheme::XApiKey,
            Protocol::OpenaiChat => AuthScheme::Bearer,
            Protocol::OpenaiResponses => AuthScheme::Bearer,
        }
    }

    /// 该协议要求的 API 版本头（名, 值）；`None` = 不需要。
    ///
    /// 收敛这一处的动机：`anthropic-version` 曾经在 proxy 的两条路径里各写一遍、
    /// 而 `upstream::apply_auth` 里没有、改由三个调用点各自补 —— 三份实现已经分叉。
    pub fn version_header(self) -> Option<(&'static str, &'static str)> {
        match self {
            Protocol::Anthropic => Some(("anthropic-version", "2023-06-01")),
            Protocol::OpenaiChat => None,
            Protocol::OpenaiResponses => None,
        }
    }

    /// 是否支持 Anthropic 的 1M 上下文 beta 特性（`anthropic-beta: context-1m-*`）。
    pub fn supports_1m_beta(self) -> bool {
        match self {
            Protocol::Anthropic => true,
            Protocol::OpenaiChat => false,
            Protocol::OpenaiResponses => false,
        }
    }
}

/// 鉴权头的形态。用枚举而非直接返回 (名, 值) 是因为取值需要拼 secret，
/// 而 secret 不该进 `model` 这一层（它只描述协议能力，不接触密钥）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthScheme {
    /// `x-api-key: <secret>`（Anthropic）
    XApiKey,
    /// `authorization: Bearer <secret>`（OpenAI 家族）
    Bearer,
}

impl AuthScheme {
    /// 头名。
    pub fn header_name(self) -> &'static str {
        match self {
            AuthScheme::XApiKey => "x-api-key",
            AuthScheme::Bearer => "authorization",
        }
    }
    /// 按形态拼出头值。
    pub fn header_value(self, secret: &str) -> String {
        match self {
            AuthScheme::XApiKey => secret.to_string(),
            AuthScheme::Bearer => format!("Bearer {secret}"),
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
///
/// `Default` 是为测试便利加的（`ProviderKey { id: ..., ..Default::default() }`）：
/// 这个结构已有 20 个字段，每加一个就要改动散落各处的测试构造点。
/// **生产代码不要用 `Default`** —— `category_id` / `protocol` 的默认值没有业务含义，
/// 真实 Key 一律经 `KeyEditor` 或导入路径构造。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
    /// 余额查询配置（可选）。`None` = 该 Key 未配置余额查询。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub balance_query: Option<BalanceQuery>,
    /// 上次余额查询的缓存结果（成功或失败都缓存，避免短时间内重复查询）。
    ///
    /// 只在内存中维护、不落盘（重启后重新查一次是合理的）。
    /// 前端轮询时先看缓存：未过期直接返回、过期才真查上游。
    #[serde(skip)]
    pub cached_balance: Option<BalanceResult>,
    /// 计费倍率（如 `"0.3"` = 官方价三折）。中转站普遍按官方价打折计费。
    ///
    /// 存字符串而非 f64：它会参与金额计算，而 JSON 里的 `0.3` 反序列化成 f64 后
    /// 再序列化可能变 `0.30000000000000004`，用户在界面上看到这种数字会以为程序坏了。
    /// 真正参与运算时才 parse 一次（见 `pricing::calculate_cost_nano`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_multiplier: Option<String>,
}

/// 余额查询配置（对齐 cc-switch 的 `usage_script`，但不执行用户代码）。
///
/// cc-switch 让用户写 JavaScript 并内置引擎执行；SynaRoute 改为**声明式**：
/// 由 Rust 发请求 + 按候选字段名递归提取。理由见
/// `docs/17-用量查询与悬浮窗方案.md` §2.1 —— 内置 JS 引擎等于开一个任意代码
/// 执行入口，而实测 cc-switch 自己的「通用模板」提取逻辑就是几个 `??` 回退，
/// 声明式完全覆盖。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BalanceQuery {
    /// 总开关。关闭时不发任何请求（用户可保留配置但暂停查询）。
    #[serde(default)]
    pub enabled: bool,
    /// 预设模板 id：`generic` / `newapi` / `deepseek` / `official` / `custom`。
    /// 仅用于界面回显「用户当初选了哪个模板」，实际请求只看下面的字段。
    #[serde(default)]
    pub template: String,
    /// 请求路径，支持 `{{baseUrl}}` / `{{apiKey}}` 占位符。
    pub url: String,
    /// HTTP 方法（默认 GET）。
    #[serde(default)]
    pub method: String,
    /// 认证头形态：`bearer` / `x-api-key` / `none`。
    #[serde(default)]
    pub auth: String,
    /// 覆盖 baseUrl（留空 = 用本 Key 的 `base_url`）。
    ///
    /// 存在的必要性：部分中转站的计费面板与转发端点**不同域**
    /// （转发在 `api.foo.com`，余额在 `panel.foo.com`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url_override: Option<String>,
    /// 覆盖密钥的**密钥库键名**（留空 = 用本 Key 自己的密钥）。
    ///
    /// 只存键名不存明文：明文一律进 `secrets.enc`，配置文件里不落密钥
    /// （cc-switch 的 `usage_script.apiKey` 是明文落库的，这里刻意不照搬）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_ref: Option<String>,
    /// 超时（秒）。0 或缺省 → `DEFAULT_BALANCE_TIMEOUT_SECS`。
    #[serde(default)]
    pub timeout_secs: u32,
    /// 自动查询间隔（分钟）。`0` = 不自动查，只在用户点刷新时查。
    #[serde(default)]
    pub auto_interval_min: u32,
    /// 自定义取值路径（点分，支持数组下标如 `"balance_infos.0.total_balance"`）。
    /// 留空 = 走内置候选链自动探测。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remaining_path: Option<String>,
    /// NewAPI 类面板的 access token（对应 `{{accessToken}}` 占位符）。
    ///
    /// 与 API Key 分开：NewAPI 的用量接口认的是**面板登录态**，不是转发用的 API Key。
    /// 明文存在配置里 —— 它不是转发凭据、权限仅限查自己的用量面板，
    /// 与 API Key 不同级；真要藏可留空并改用自定义模板。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_token: Option<String>,
    /// NewAPI 类面板的用户 id（对应 `{{userId}}` 占位符，也用于 `New-Api-User` 头）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
}

impl Default for BalanceQuery {
    fn default() -> Self {
        Self {
            enabled: false,
            template: "generic".into(),
            // 与 cc-switch 通用模板**逐字一致**（其文档 §2.5：`url: "{{baseUrl}}/user/balance"`）。
            //
            // 此前这里写的是 `/v1/usage` —— 那是我从某个站的**自定义脚本**里读到的路径，
            // 误当成了通用默认值，导致新用户一开开关就 404。对齐官方模板才是正确起点。
            url: "{{baseUrl}}/user/balance".into(),
            method: "GET".into(),
            auth: "bearer".into(),
            base_url_override: None,
            api_key_ref: None,
            timeout_secs: DEFAULT_BALANCE_TIMEOUT_SECS,
            auto_interval_min: 0,
            remaining_path: None,
            access_token: None,
            user_id: None,
        }
    }
}

/// 余额查询默认超时（秒）。与 cc-switch 界面上的默认值一致。
pub const DEFAULT_BALANCE_TIMEOUT_SECS: u32 = 10;

/// 一次余额查询的结果。
///
/// 字段对齐 cc-switch 的 extractor 返回契约（其用户手册 §2.5 列了 8 个可选字段：
/// `isValid` / `invalidMessage` / `remaining` / `unit` / `planName` / `total` /
/// `used` / `extra`），这样同一套站点配置在两边的表达力一致。
///
/// **失败必须可见**：查不到就带上 `error` 如实呈现，绝不返回
/// `remaining: 0` —— 显示 0 会让用户以为余额真的用光了，
/// 这类误导比不显示更糟。故 `remaining` 是 `Option`。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BalanceResult {
    /// 查询是否成功拿到了数值。
    pub ok: bool,
    /// 剩余额度。`None` = 没取到（此时 `error` 必有内容）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remaining: Option<f64>,
    /// 货币单位（`USD` / `CNY` / 上游自报的其它）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// 上游是否声明该 Key 仍有效（部分站点会给 `is_active` / `is_available`）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_valid: Option<bool>,
    /// 账号无效时上游给的原因（cc-switch 的 `invalidMessage`）。
    ///
    /// 与 `error` 分开：`error` 是**我们这侧**的失败（超时、404、字段找不到），
    /// 这个是**上游明确说**「这个号不能用了」。两者混在一起，用户就分不清
    /// 「查询坏了」和「账号欠费了」——而这两件事的处理方式完全不同。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invalid_message: Option<String>,
    /// 套餐名（多套餐站点会给，cc-switch 的 `planName`）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_name: Option<String>,
    /// 总额度。有它才能算出「已用百分比」。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<f64>,
    /// 已消耗额度。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub used: Option<f64>,
    /// 查询时刻（epoch ms）。面板显示「刷新于 X 分钟前」。
    pub queried_at: i64,
    /// 失败原因（超时 / HTTP 状态 / 字段找不到）。成功时为 `None`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl BalanceResult {
    /// 构造一个失败结果（带原因）。
    pub fn failed(reason: impl Into<String>) -> Self {
        Self {
            ok: false,
            remaining: None,
            unit: None,
            is_valid: None,
            invalid_message: None,
            plan_name: None,
            total: None,
            used: None,
            queried_at: chrono::Utc::now().timestamp_millis(),
            error: Some(reason.into()),
        }
    }
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

/// 一个不被桌面端接受的对外模型名，以及给它的合规替代建议。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopModelNameIssue {
    /// 用户当前填的对外名（不合规的那个）
    pub name: String,
    /// 建议改成的合规名
    pub suggestion: String,
}

/// 某个 Key 的桌面端对外模型名体检结果。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopModelNameReport {
    /// 是否适用（只有 Claude 桌面端分类才适用；其余分类恒为 false，前端据此完全不显示提示）
    pub applicable: bool,
    /// 体检了多少个对外名
    pub total: usize,
    /// 不合规的那些（空 = 全部合规）
    pub issues: Vec<DesktopModelNameIssue>,
}

/// 为一个不合规的对外名生成合规替代。
///
/// **档位靠猜，但猜错不影响路由**：自由映射的 exact match 优先于三档家族匹配
/// （见 `resolve_model`），档位只决定桌面端把它归到哪个 `anthropicFamilyTier` 家族桶里显示。
/// 所以这里宁可给一个「一定能用」的名字，也不追求档位判断精准。
///
/// `taken` 是已被占用的名字，用于同批去重：`glm-4.6` 与 `gpt-4.6` 若都推 `claude-sonnet-4-6`，
/// 第二条映射的对外名会与第一条撞车而永远匹配不到。
///
/// **返回前必须再过一次 `is_desktop_acceptable_model_id`**：这个函数是拼字符串拼出来的，
/// 万一源串里带了黑名单词而被原样带进结果（例如版本段解析出意外内容），
/// 就会给用户一个「点了采纳、保存仍被拒」的死循环。过不了就退回最朴素的 `claude-<档位>`。
pub fn suggest_desktop_model_name(source: &str, taken: &[String]) -> String {
    let lower = source.trim().to_ascii_lowercase();

    // 档位：按规格词猜。小规格词 → haiku，重推理词 → opus，其余 sonnet。
    let tier = if ["air", "lite", "mini", "flash", "small", "nano", "turbo"]
        .iter()
        .any(|k| lower.contains(k))
    {
        "haiku"
    } else if ["reasoner", "thinking", "max", "ultra", "pro", "plus"]
        .iter()
        .any(|k| lower.contains(k))
    {
        "opus"
    } else {
        "sonnet"
    };

    // 版本：取末尾的数字/点片段（glm-4.6 → 4.6 → 4-6）。取不到就用 4-5。
    let ver = {
        let tail: String = lower
            .chars()
            .rev()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let t = tail.trim_matches('.');
        if t.is_empty() { "4-5".to_string() } else { t.replace('.', "-") }
    };

    let base = format!("claude-{tier}-{ver}");
    // 同批去重：撞了就追加 -2 / -3 …
    let mut candidate = base.clone();
    let mut n = 2;
    while taken.iter().any(|t| t.eq_ignore_ascii_case(&candidate)) {
        candidate = format!("{base}-{n}");
        n += 1;
    }
    if is_desktop_acceptable_model_id(&candidate) {
        candidate
    } else {
        format!("claude-{tier}")
    }
}

/// 对一组对外名逐个体检，返回不合规的那些及其建议。
///
/// `taken` 先装入**已经合规**的那些名字：建议出来的新名字不能和用户已有的合规名撞车。
/// 每生成一个建议就压回 `taken`，保证同一次调用内的多个建议互不相同。
pub fn desktop_model_name_issues(outward: &[String]) -> Vec<DesktopModelNameIssue> {
    let mut taken: Vec<String> = outward
        .iter()
        .filter(|m| is_desktop_acceptable_model_id(m))
        .cloned()
        .collect();
    let mut issues = Vec::new();
    for name in outward {
        if is_desktop_acceptable_model_id(name) {
            continue;
        }
        let suggestion = suggest_desktop_model_name(name, &taken);
        taken.push(suggestion.clone());
        issues.push(DesktopModelNameIssue { name: name.clone(), suggestion });
    }
    issues
}

/// 一个 Key 的桌面端对外模型名体检。
///
/// **输入源必须是 `serviceable_models()`**，与保存拦截
/// （`lib.rs` 的 `reject_desktop_key_with_unusable_model_names`）看的是同一个集合。
/// 两边若各自 filter，迟早会出现「界面说没问题、保存却被拒」这种自相矛盾。
pub fn desktop_model_name_report(key: &ProviderKey) -> DesktopModelNameReport {
    if !key.category_id.meta().strict_model_id {
        return DesktopModelNameReport { applicable: false, total: 0, issues: Vec::new() };
    }
    let outward = key.serviceable_models();
    DesktopModelNameReport {
        applicable: true,
        total: outward.len(),
        issues: desktop_model_name_issues(&outward),
    }
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

    /// 下游**完全没给模型名**时，为本 Key 选一个它一定能认的模型。
    ///
    /// 只在 `requested_model` 为空时用（见 `resolve_model_detail` 第 6 步的注释）。
    /// 顺序：默认兜底 → 首个已知模型 → 任一映射的真实名 → 任一三档。
    /// 全都没有则返回 `None`（该 Key 确实没有任何模型信息，只能让上游报错，
    /// 此时硬编造一个名字反而会把「配置不全」伪装成「上游拒绝」）。
    ///
    /// **刻意不硬编码任何厂商模型名**：各 Key 指向不同中转商，写死
    /// `claude-3-5-sonnet` 之类对只支持 GPT 的 Key 必然 404 —— 那是把一种错换成另一种错。
    fn fallback_model_for_empty_request(&self) -> Option<String> {
        if let Some(dm) = self
            .default_model
            .as_ref()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
        {
            return Some(dm.to_string());
        }
        if let Some(first) = self.models.first() {
            return Some(first.real_name.clone());
        }
        // 映射的真实名同样是「本 Key 认得的模型」，可用
        if let Some(m) = self
            .mappings
            .iter()
            .find(|m| !m.real_name.trim().is_empty())
        {
            return Some(m.real_name.trim().to_string());
        }
        // 三档任一（顺序取 opus → sonnet → haiku，与家族默认口径一致）
        [&self.tier_opus, &self.tier_sonnet, &self.tier_haiku]
            .into_iter()
            .flatten()
            .map(|s| s.trim())
            .find(|s| !s.is_empty())
            .map(|s| s.to_string())
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
        //
        // **空请求名的兜底**（2026-08-02 真机实证，勿删）：下游可能压根不传 `model`
        // （Claude 桌面端的部分请求、部分客户端的非补全调用），此时 requested 是空串，
        // 前 5 级全不命中而落到这里。若原样透传空串，上游一律回
        // `400 model is required` / `Model name not specified` —— 实测一次会话里 16 次 400
        // 全是这个形状，且**每个候选 Key 都会重打一遍**，最后报「全部 Key 失败」，
        // 用户看到的是「所有中转商都挂了」，真实原因只是我们没填模型名。
        //
        // 更隐蔽的是日志：`log_request` 展示的是 `resolve_model` ���结果，
        // 空串时写成「客户端要 ? → 实际用 <兜底名>」，看起来我们已经兜底了 —— 但那只是
        // 展示层各算各的，请求体里塞的仍是空串。这正是本项目反复防的「看起来做了、实际没做」。
        //
        // 兜底顺序与上面一致（default_model → 首个模型），只是这里要在**空请求名**时也生效；
        // 两者都没有则交给 `first_serviceable_or_family` 给一个该 Key 一定能认的名字。
        if requested_model.is_empty() {
            if let Some(fallback) = self.fallback_model_for_empty_request() {
                return (fallback, ModelResolveKind::Default);
            }
        }
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
        if !self.category_id.meta().tier_rewrite {
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

    /// 本 Key 里某个**真实模型名**的上下文窗口（token）。
    ///
    /// 与 [`Self::context_window_for_outward`] 的分工：那个用于桌面端 `supports1m`
    /// **能力断言**（故要求命中 Mapping/Tier/Native 才敢断言）；这个用于转发时判断
    /// 「本次实际要打的模型是否 1M」，输入已是解析后的真实名，直接查表即可。
    pub fn context_window_of_real(&self, real_name: &str) -> Option<u32> {
        self.models
            .iter()
            .find(|m| m.real_name == real_name)
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
    /// 是否给参与者一组**只读**检索工具（read_file / grep / list_dir / codegraph_query），
    /// 让它按需一步步挖，而不是靠 `retrieval` 一次性猜哪些文件相关。
    ///
    /// **默认关**：每轮工具调用都要重发完整消息历史，额度消耗显著高于单轮，用户该明确知道
    /// 自己在开什么（UI 上附了「会增加额度消耗」说明）。
    #[serde(default)]
    pub tools_enabled: bool,
    /// 工具调用的轮数上限。模型可能陷入「读文件 → 再读 → 再读」的循环烧掉整轮预算，
    /// 到顶后强制它基于已有材料出结论。运行时会 clamp 到 2~12。
    #[serde(default = "default_max_tool_rounds")]
    pub max_tool_rounds: u32,
    /// 工具结果历史的**字符预算**：累计超过就把较早轮次的工具结果压成占位说明
    /// （消息条数与配对不动），控制「每轮重发完整历史」导致的额度膨胀。
    ///
    /// 真机实测：不加限制时一次成员调用请求体峰值达 20 万字符（约 10 万 token）。
    /// 默认 60000 字符（约 3 万 token），运行时 clamp 到 8000~400000；**0 = 关闭裁剪**。
    #[serde(default = "default_tool_ctx_budget")]
    pub tool_ctx_budget_chars: u32,
    /// **单次**工具结果的字符上限（原先硬编码 8000）。调小能直接压低每轮的增量，
    /// 代价是模型每次看到的文件片段更短、可能要多调几次。
    /// 默认 8000，运行时 clamp 到 1000~40000。
    #[serde(default = "default_tool_result_cap")]
    pub tool_result_cap_chars: u32,
}

fn default_tool_result_cap() -> u32 {
    8_000
}

fn default_tool_ctx_budget() -> u32 {
    60_000
}

fn default_max_tool_rounds() -> u32 {
    6
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

/// 一次上游调用的 token 用量。两协议字段名不同，这里归一。
///
/// 为什么要有：用户在真机上遇到「一次聚合 2 分 38 秒、请求体 20 万字符」，
/// 但**日志里看不到任何 token 数字**，无从判断是哪个环节吃掉了额度、也无法验证
/// 「工具开关关掉后是否真的省了」。可观测性是控成本的前提。
///
/// 为什么定义在 `model` 而非 `upstream`：它是**领域观测量**，被 [`EventLogEntry`] 直接持有。
/// 原先定义在 `upstream.rs` 会让本模块反向依赖那个 7000+ 行的协议模块（且 `model` 对
/// upstream 的依赖仅此一处）。协议侧的解析采集逻辑（`extract_usage` 等）仍留在 `upstream`。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsage {
    /// 输入（prompt）token
    pub input: u64,
    /// 输出（completion）token
    pub output: u64,
    /// 命中缓存的输入 token（Anthropic 有，OpenAI 部分中转商也给）
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub cache_read: u64,
    /// 写入缓存的 token（首次发送缓存前缀时产生）。
    /// 成本通常是输入价的 1.25 倍，漏它会让计费偏低。
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub cache_creation: u64,
}

fn is_zero_u64(v: &u64) -> bool {
    *v == 0
}

/// 用量统计：按「分类 × Key」聚合的一行。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsageByKey {
    pub category_id: CategoryType,
    /// 空串 = 该分类的系统级事件（无具体 Key）。
    pub key_id: String,
    pub usage: TokenUsage,
}

/// 用量累计的**落盘快照**（`usage.json`）。
///
/// 刻意与 `config.json` 分开成独立文件，不塞进 `AppConfig`：
/// - 用量是**运行期遥测**，不是用户配置。混进去会让「每分钟落一次用量」变成
///   「每分钟重写一次用户的全部 Key 与设置」——把一份只该增长计数器的写入，
///   放大成对用户最宝贵数据的反复覆写，任何一次坏写都可能带走 Key 配置。
/// - 体积与节奏都不同：config 是事件驱动、~20KB；usage 是定时、通常几百字节。
///
/// **v2 改动（2026-08-11）**：按日分桶 + 90 天滚动。v1 只有单个全局 `entries`，
/// 运行几个月后会攒到几千行；v2 把它们按 UTC 日期分桶，flush 时自动删 91 天前的桶。
/// v1→v2 迁移：首次 flush 时把 v1 的 `entries` 全搬进一个桶（日期按 `since_ms` 推算），
/// 之后 `entries` 字段留空（保留它是为了让 v2 能读 v1 文件）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageSnapshot {
    /// 格式版本。v1 = 全局 `entries`；v2 = 按日分桶 `daily_buckets`。
    #[serde(default = "usage_snapshot_version")]
    pub version: u32,
    /// 统计起始时刻（毫秒时间戳）。首次创建时写入，之后原样保留 ——
    /// 面板据此显示「统计自 X 起」，否则一个只增不减的数字用户无从判断它覆盖多长时间。
    #[serde(default)]
    pub since_ms: i64,
    /// 最后一次落盘时刻，仅供人工排查「这份文件是不是卡住不更新了」。
    #[serde(default)]
    pub updated_ms: i64,
    /// **v2 数据**：按 UTC 日期分桶的用量，最多保留 90 天。
    /// 每个桶 = 一天内所有分类 × Key 的累计。降序排列（最新的在前）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub daily_buckets: Vec<DailyUsageBucket>,
    /// **v1 兼容字段**：v2 写出时留空，只在读 v1 文件时有值。
    /// v2 程序首次 flush 会把这里的内容迁移进 `daily_buckets` 的一个桶。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entries: Vec<TokenUsageByKey>,
}

/// 一天的用量分桶（v2 格式）。`entries` 含义与 v1 相同：该天内所有 Key 的累计。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DailyUsageBucket {
    /// UTC 日期字符串，格式 `"YYYY-MM-DD"`（如 `"2026-08-11"`）。
    pub date: String,
    /// 该天内按 `(分类, key_id)` 聚合的用量。Vec 而非 map 的理由同 v1。
    pub entries: Vec<TokenUsageByKey>,
}

/// `usage.json` 的当前格式版本。**读写共用这一个常量**，别两边各写字面量 ——
/// 那样改了写侧忘了改读侧，版本门就会把自己刚写出去的文件判为"来自未来"。
///
/// 改动规则：只有当**旧程序按旧结构解析新文件会得到错误结果**时才递增。
/// v1→v2（按日分桶）正是这种情形：v1 程序解析 v2 文件时 `daily_buckets` 会被
/// 忽略（未知字段），读出的 `entries` 是空的，紧接着 flush 就会用空数据覆盖
/// 用户攒了几个月的累计 —— **版本门必须拦住**。
pub const USAGE_SNAPSHOT_VERSION: u32 = 2;

fn usage_snapshot_version() -> u32 {
    USAGE_SNAPSHOT_VERSION
}

impl TokenUsage {
    pub fn total(&self) -> u64 {
        self.input + self.output
    }
    pub fn is_empty(&self) -> bool {
        self.input == 0 && self.output == 0 && self.cache_read == 0 && self.cache_creation == 0
    }
    /// 累加（聚合各环节汇总用）。
    pub fn add(&mut self, o: &TokenUsage) {
        self.input += o.input;
        self.output += o.output;
        self.cache_read += o.cache_read;
        self.cache_creation += o.cache_creation;
    }
    /// 紧凑展示：`↑1.2k ↓340`（缓存命中不为 0 时附 `缓存 900`）。
    pub fn fmt_compact(&self) -> String {
        fn k(n: u64) -> String {
            if n >= 10_000 {
                format!("{:.1}k", n as f64 / 1000.0)
            } else {
                n.to_string()
            }
        }
        let mut s = format!("↑{} ↓{}", k(self.input), k(self.output));
        if self.cache_read > 0 {
            s.push_str(&format!(" 缓存{}", k(self.cache_read)));
        }
        if self.cache_creation > 0 {
            s.push_str(&format!(" 写缓存{}", k(self.cache_creation)));
        }
        s
    }
}

/// 首启向导第④步的探针结果：自某个时刻起，某分类有没有真的收到过转发请求。
///
/// 这是向导里**唯一的正反馈**——用户接入完客户端后，此前没有任何东西告诉他「成功了」。
/// 同时带上失败信息：接入配错时用户同样卡在这一步，只说「还没收到请求」帮不了他。
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FirstRequestProbe {
    /// 是否已收到至少一次成功路由的请求
    pub routed: bool,
    /// 那一次的时间戳
    pub ts: Option<i64>,
    /// 那一次的摘要（哪个模型、走了哪条 Key）
    pub detail: Option<String>,
    /// 是否出现过失败（错误或故障转移）
    pub failed: bool,
    /// 失败摘要
    pub failure_detail: Option<String>,
}

/// 首启向导该不该显示，以及显示时需要的上下文。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OnboardingState {
    pub should_show: bool,
    pub done: bool,
    pub total_keys: usize,
    /// 这台机器上有没有 cc-switch 的库。有则把「从 cc-switch 导入」作为默认高亮的主选项 ——
    /// 对已经在用 cc-switch 的用户，那是比手工填 13 个字段快得多的路。
    pub ccswitch_available: bool,
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
    /// Key 的**可读名**（key_id 是 uuid，用户认不出；列表接口按 id 回填）。
    ///
    /// 为什么要在列表带上而让前端从 detail 字符串里挖：detail 是拼好的展示文本
    /// （「Key名 · 模型段 · 动词」），折叠态被 truncate 截断时 Key 名可能看不见；
    /// 而请求路径可视化（FR-028 之三）要「不展开就知道走了哪个 Key」，靠稳定的
    /// 结构化字段而不是解析字符串。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_name: Option<String>,
    pub detail: String,
    /// 连续同类事件被折叠的条数（1 = 未折叠）。
    ///
    /// 高频转发下「同一个 Key、同一个模型、都成功」的记录只是延迟不同，逐条列出会瞬间刷屏
    /// （实测 14 秒 12 条）。故内存态把**连续**的同类记录合成一条并在这里计数，UI 显示
    /// 「×N」。判据是 [`Self::collapse_key`]，只折叠紧邻的一条 —— 中间插进任何别的事件
    /// （失败、故障转移、配置变更）就重新起一条，时间线不会被压扁到看不出穿插关系。
    ///
    /// **日志文件仍逐条完整写**（`write_log_to_file` 在折叠前调用），排障取证不受影响。
    #[serde(default = "default_repeat")]
    pub repeat: u32,
    /// 折叠判据：同 key 的连续事件才合并。仅后端内存态使用，不下发前端。
    #[serde(skip)]
    pub collapse_key: Option<String>,
    /// 本次调用的 token 用量（上游给了才有）。
    ///
    /// 没有它就无法回答「这次聚合花了多少额度、哪个环节花得多」——真机上用户遇到
    /// 一次 2 分 38 秒、请求体 20 万字符的聚合，日志里却找不到任何 token 数字。
    /// `None` = 该中转商未返回用量（如实陈述，不写 0 冒充）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<TokenUsage>,
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
    /// **一次请求内**故障转移的总时间预算（毫秒）。默认 90s。
    ///
    /// 为什么需要：候选遍历原先只有 per-Key 超时，没有整体上限。用户配 6 条 Key、上游整体
    /// 抖动（网关挂 30s 再回 502）时，单请求最坏 6×30s = **180 秒**才拿到 529。而客户端
    /// （Claude Code / Codex）自身请求超时远短于此——它早已超时报错并重发，代理侧那条
    /// 「僵尸链」还在继续逐个打上游、**继续烧额度**、继续写日志。
    ///
    /// 语义是「不再**开始**新的候选尝试」，不是硬掐正在进行的请求：
    /// - 非流式：把剩余预算与 Key 自身超时取小值传给本次请求；
    /// - 流式：只约束 `send()` 探头阶段，**绝不掐已建立的 SSE 流**（否则长回答会被截断，
    ///   那是刻意设计的行为，见 `try_stream_to_key` 的超时注释）。
    ///
    /// 设 0 = 关闭整体预算（回到旧行为，仅受 per-Key 超时约束）。
    #[serde(default = "default_failover_budget")]
    pub failover_total_budget_ms: u64,
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
    #[serde(default, deserialize_with = "de_category_vec")]
    pub mcp_registered_categories: Vec<CategoryType>,
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
    #[serde(default, deserialize_with = "de_category_map")]
    pub active_models: std::collections::BTreeMap<CategoryType, String>,
    /// 托盘「Codex 模型」快切子菜单开关。开启后右键托盘可直接切换 Codex 当前对外模型，
    /// 免打开主窗口（借鉴 cc-switch 托盘切换范式）。默认开。关闭则托盘只留显示/退出。
    #[serde(default = "default_true")]
    pub tray_model_switch_enabled: bool,
    /// 桌面悬浮窗开关（第⑥批）。**默认关**。
    ///
    /// 开启后并不立即显示 —— 只在主窗口**最小化到托盘**时才出现（见 `lib.rs` 的
    /// `CloseRequested` 处理）。这是用户明确要求的语义：主窗口在前台时悬浮窗只会挡事。
    #[serde(default)]
    pub floating_widget_enabled: bool,
    /// 悬浮球是否**置顶**（始终在其它窗口之上）。**默认关**。
    ///
    /// 为什么单开一个开关、且默认关：悬浮球原先是无条件 `always_on_top(true)` 的，
    /// 于是用别的软件时它一直盖在最上面挡着 —— 这是用户实测反馈的问题。
    /// 置顶对「瞥一眼状态」确实有用（否则会被别的窗口埋掉），但那该由用户自己选，
    /// 不该是写死的行为。
    #[serde(default)]
    pub floating_widget_always_on_top: bool,
    /// 各分类的「默认推理强度」（key=分类字符串，value=effort 档位 low/medium/high/xhigh）。
    /// 缘由：Codex Desktop 对自定义 provider 不下发 reasoning.effort（只发 reasoning.summary），
    /// 客户端 UI 设的强度传不到上游。故在此配一个默认值，转发时若下游 body 无 effort 就注入，
    /// 让 Codex→Claude(thinking)/Chat(reasoning_effort) 的推理强度能真正生效。
    /// 仅在下游为 Responses(Codex) 且上游非原生 Responses 时注入（OpenAI 官方 Responses 直通不碰）。
    /// 后端自管字段：由专用命令 set_active_effort 更新，不随通用 save_settings 的陈旧快照覆盖
    /// （与 active_models / mcp_* 同一保全策略）。空/未配 = 不注入，保持现状。
    #[serde(default, deserialize_with = "de_category_map")]
    pub active_efforts: std::collections::BTreeMap<CategoryType, String>,
    /// 各分类代理的「首选监听端口」（key=分类字符串，value=端口）。
    /// 缘由：早期用 OS 随机端口（bind 0），SynaRoute 每次重启端口都变，而客户端
    /// （Codex/Claude）只在自身启动时读一次 config，不追踪端口变化 → 重启后客户端仍打旧端口、
    /// 连不上（error sending request）。改为「粘滞固定端口」：各分类有稳定默认端口，
    /// config.toml 写一次即长期有效；端口被占时在 [port, port+FALLBACK] 内向上兜底并写回此处
    /// 作为下次首选（与 mcp_port 同一粘滞策略）。缺省时用 CategoryType::meta().default_port。
    #[serde(default, deserialize_with = "de_category_map")]
    pub proxy_ports: std::collections::BTreeMap<CategoryType, u16>,
    /// 上次运行结束时**处于启用状态**的代理分类。启动时据此自动恢复（FR-029）。
    ///
    /// 缘由：代理的运行态原先只活在 `ProxyManager.running`（内存 HashMap），进程一退就没了，
    /// 用户每次开应用都得手点一次「启动」——而绝大多数人的用法是「一直开着」。
    ///
    /// **记的是快照而非用户意图**：启停的意图点有 4 处以上（`start_proxy` 命令、
    /// `apply_tool_config`、托盘 handler、首启向导、`set_proxy_port`），在每处写标记
    /// 漏一处就是静默失效。改为周期性 + 退出时采样 `ProxyManager` 的活真相，
    /// 任何路径都无法漏记。附带好处：`set_proxy_port` 内部 `stop()→start()` 的瞬态
    /// 天然不影响结果，因为只在稳定态采样。
    ///
    /// 恢复时必须走 `service::apply_tool_config`（= `proxy.start()` + 写客户端配置），
    /// 不能裸 `proxy.start()` —— 后者会重演「界面/托盘说已启动、客户端根本没走代理」
    /// 那个已修过的无头案（见 `service::apply_tool_config` 的文档）。
    ///
    /// 后端自管字段：由 `set_proxy_running_categories` 更新。它不在 `UserPrefs` 白名单里，
    /// 故通用 `save_settings` 的陈旧前端快照天然覆盖不到（与 `mcp_*` / `active_models` 同策略）。
    #[serde(default, deserialize_with = "de_category_vec")]
    pub proxy_running_categories: Vec<CategoryType>,
    /// 首启向导是否已完成（UX#1）。**三态**：
    /// - `None` = 从未判定。旧版本升级上来的配置里没这个字段，全新安装第一次启动也是这个值。
    /// - `Some(true)` = 已完成或用户主动跳过 → 不再显示。
    /// - `Some(false)` = 判定过、该显示向导。
    ///
    /// 为什么要三态而不是 `bool`：`bool` 的默认值 `false` 会让**所有老用户**升级后
    /// 突然被首启向导拦住 —— 他们早就配好了。`None` 让启动时的对账
    /// （`Store::reconcile_onboarding_flag`）能区分「没判定过」与「判定过、结论是要显示」，
    /// 据当前 Key 数一次性定下来。
    ///
    /// 后端自管字段：由专用命令 `set_onboarding_done` 更新，不随通用 `save_settings`
    /// 的陈旧快照覆盖（与 `mcp_*` / `active_models` 同一保全策略）。
    #[serde(default)]
    pub onboarding_done: Option<bool>,
}


/// 容错反序列化：把磁盘上的 `{"claude-cli": V}` 读成 `BTreeMap<CategoryType, V>`（P2-8）。
///
/// **必须容错**：配置文件是用户可编辑的、也可能来自别的机器或更新的版本。
/// 若用 serde 默认行为，一个不认识的分类键（比如未来版本加的 `gemini`，或用户手滑打错的键）
/// 会让**整份 config.json 解析失败** —— 那会走 .corrupt 备份路径，用户的全部配置一次性消失。
/// 这里只丢弃认不出的那一项并记一行日志，其余照常读出。
///
/// 用 BTreeMap 而不是 HashMap：序列化顺序稳定。HashMap 的迭代顺序每次进程都不同，
/// 会让导出文件的 sha256 自校验随机失败 —— 那种失败没有规律、极难归因。
fn de_category_map<'de, D, V>(d: D) -> Result<std::collections::BTreeMap<CategoryType, V>, D::Error>
where
    D: serde::Deserializer<'de>,
    V: Deserialize<'de>,
{
    let raw = Option::<std::collections::BTreeMap<String, V>>::deserialize(d)?.unwrap_or_default();
    let mut out = std::collections::BTreeMap::new();
    for (k, v) in raw {
        match CategoryType::from_str(&k) {
            Some(c) => {
                out.insert(c, v);
            }
            None => tracing::warn!("配置里有未知分类键 {k}，已忽略该项（其余配置照常读出）"),
        }
    }
    Ok(out)
}

/// 同 []，用于分类**列表**（已注册 MCP 的分类）。
fn de_category_vec<'de, D>(d: D) -> Result<Vec<CategoryType>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Option::<Vec<String>>::deserialize(d)?.unwrap_or_default();
    let mut out = Vec::new();
    for k in raw {
        match CategoryType::from_str(&k) {
            Some(c) => out.push(c),
            None => tracing::warn!("配置里有未知分类 {k}，已忽略（其余配置照常读出）"),
        }
    }
    Ok(out)
}

fn default_true() -> bool {
    true
}

/// 事件折叠计数的默认值。旧日志文件里没有 `repeat` 字段，反序列化时按「未折叠」算。
fn default_repeat() -> u32 {
    1
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

/// 故障转移总预算默认 90 秒。
///
/// 取值理由：要明显大于「单次正常生成耗时」以免误杀慢但正常的请求（非流式长回答几十秒
/// 常见），又要明显小于「候选数 × per-Key 超时」的最坏值（6×30s = 180s）。90s 让
/// 6 Key 场景最坏减半，同时给 2~3 个候选留出各自跑满 30s 的空间。
fn default_failover_budget() -> u64 {
    90_000
}

/// **前端可写的那部分设置**（`save_settings` 的入参类型）。
///
/// ## 它解决的问题
///
/// `AppSettings` 里混着两类字段：用户偏好（前端是唯一写者）与后端自管的运行态
/// （粘滞端口、MCP 注册记录、已选模型、密钥库模式镜像、开机自启动…）。
/// 而前端保存设置时提交的是它**挂载那一刻的整份快照** —— 用户切个主题，
/// 那份旧快照就会把后端刚写的运行态一起顶回去。
///
/// 此前的防线是**黑名单**：`save_settings` 里逐个字段 `mem::take` 保留后端值。
/// 它出过 P0 —— `auto_start` 不在名单里，于是切主题/切语言会把用户刚关掉的开机自启动
/// 重新装回系统。黑名单的问题是「默认不安全」：日后给后端加一个自管字段，
/// 只要忘了往名单里补一行，就又是一次同形态的事故，而且没有任何东西会提醒你。
///
/// 换成白名单后，前端**在类型上就无法表达**「我要改 mcpPort」这件事：多余的键在
/// 反序列化时被 serde 静默丢弃。日后给 `AppSettings` 加后端自管字段，默认就是安全的。
///
/// ## 为什么不把 AppSettings 物理拆成两个结构
///
/// 那样要改 60 多处 `settings.X` 的字段路径，而收益与本方案**完全相同** ——
/// 安全性来自「入参类型里没有那些字段」，不来自「它们住在不同结构里」。
/// 磁盘格式也因此完全不受影响（`AppSettings` 的序列化一个字节都没动）。
///
/// ## 加字段时怎么办
///
/// 新增的字段若是用户偏好，同时加进这里与 `AppSettings`，并在 `apply_to` 里赋值；
/// 若是后端自管的，**只**加进 `AppSettings` 并配一个专用写入方法。
/// `apply_to` 是逐字段显式赋值（不是 `..Default::default()` 展开）——
/// 漏了新字段会表现为「用户改了但没保存」，所以那里配了一条守卫测试。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserPrefs {
    pub theme: String,
    #[serde(default = "default_language")]
    pub language: String,
    #[serde(default)]
    pub lan_exposure: bool,
    #[serde(default)]
    pub request_log_enabled: bool,
    #[serde(default)]
    pub log_downstream_raw_enabled: bool,
    #[serde(default = "default_health_interval")]
    pub health_check_interval_secs: u64,
    #[serde(default = "default_failover_budget")]
    pub failover_total_budget_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_dir: Option<String>,
    #[serde(default = "default_true")]
    pub upstream_retry_enabled: bool,
    #[serde(default)]
    pub health_probe_real_completion: bool,
    #[serde(default)]
    pub health_probe_test_messages: Vec<String>,
    #[serde(default)]
    pub aggregate_trace_enabled: bool,
    #[serde(default = "default_true")]
    pub tray_model_switch_enabled: bool,
    // ⚠️ **悬浮球那两个字段刻意不在这里**（`floating_widget_enabled` /
    // `floating_widget_always_on_top`）。它们带窗口副作用，由专用命令
    // `set_floating_widget` / `set_floating_pinned` 管。
    //
    // 曾经在这里，是个**静默关掉用户开关**的缺陷：前端 `prefs.ts` 的白名单里
    // 从来没有这两个键（那侧的注释也写着「不在白名单里」），于是每次 `saveSettings`
    // 提交的 JSON 都缺键 → `#[serde(default)]` 补成 false → `apply_to` 把它们
    // 写成 false。表现为「切一下主题/语言，开着的悬浮球就没了」，而用户完全不知道
    // 是那个动作干的。与自启动那条 P0 同类，方向相反。
    //
    // 判据：本结构体的字段集必须与 `src/lib/prefs.ts` 的 `pickPrefs` 逐字段对齐。
    // 加字段时先问「前端白名单里有吗」——没有就不该加到这里。
}

impl UserPrefs {
    /// 把这些偏好写进一份完整设置，**不碰任何后端自管字段**。
    ///
    /// 逐字段显式赋值是刻意的：用 `..` 展开或整份替换都会把 runtime 字段一起带走，
    /// 那正是本类型要防的事。
    pub fn apply_to(self, s: &mut AppSettings) {
        s.theme = self.theme;
        s.language = self.language;
        s.lan_exposure = self.lan_exposure;
        s.request_log_enabled = self.request_log_enabled;
        s.log_downstream_raw_enabled = self.log_downstream_raw_enabled;
        s.health_check_interval_secs = self.health_check_interval_secs;
        s.failover_total_budget_ms = self.failover_total_budget_ms;
        s.log_dir = self.log_dir;
        s.upstream_retry_enabled = self.upstream_retry_enabled;
        s.health_probe_real_completion = self.health_probe_real_completion;
        s.health_probe_test_messages = self.health_probe_test_messages;
        s.aggregate_trace_enabled = self.aggregate_trace_enabled;
        s.tray_model_switch_enabled = self.tray_model_switch_enabled;
        // 悬浮球两个开关**刻意不赋值**：它们不在 UserPrefs 里（理由见该结构体的注释）。
        // 别"顺手补上"——补上就等于把那个静默关开关的缺陷再装回来。
    }
}

impl From<&AppSettings> for UserPrefs {
    fn from(s: &AppSettings) -> Self {
        Self {
            theme: s.theme.clone(),
            language: s.language.clone(),
            lan_exposure: s.lan_exposure,
            request_log_enabled: s.request_log_enabled,
            log_downstream_raw_enabled: s.log_downstream_raw_enabled,
            health_check_interval_secs: s.health_check_interval_secs,
            failover_total_budget_ms: s.failover_total_budget_ms,
            log_dir: s.log_dir.clone(),
            upstream_retry_enabled: s.upstream_retry_enabled,
            health_probe_real_completion: s.health_probe_real_completion,
            health_probe_test_messages: s.health_probe_test_messages.clone(),
            aggregate_trace_enabled: s.aggregate_trace_enabled,
            tray_model_switch_enabled: s.tray_model_switch_enabled,
        }
    }
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
            failover_total_budget_ms: default_failover_budget(),
            log_dir: None,
            mcp_enabled: false,
            mcp_port: default_mcp_port(),
            mcp_registered_categories: Vec::new(),
            upstream_retry_enabled: true,
            health_probe_real_completion: false,
            health_probe_test_messages: Vec::new(),
            aggregate_trace_enabled: false,
            active_models: std::collections::BTreeMap::new(),
            active_efforts: std::collections::BTreeMap::new(),
            tray_model_switch_enabled: true,
            proxy_ports: std::collections::BTreeMap::new(),
            proxy_running_categories: Vec::new(),
            // None = 从未判定。启动时 reconcile_onboarding_flag 会据当前 Key 数定下来，
            // 老用户因此不会突然被首启向导拦住。
            onboarding_done: None,
            // 悬浮窗默认**关闭**（用户明确要求）：它是个额外的常驻窗口，
            // 不该在用户没开口的情况下自己冒出来。
            floating_widget_enabled: false,
            floating_widget_always_on_top: false,
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
    /// 配置文件版本号，用于跟踪迁移状态。
    /// 每次需要迁移已存配置时递增此版本号。
    #[serde(default)]
    pub config_version: u32,
    #[serde(default)]
    pub keys: Vec<ProviderKey>,
    #[serde(default)]
    pub brain: Vec<BrainConfig>,
    #[serde(default)]
    pub vendors: Vec<Vendor>,
    #[serde(default)]
    pub settings: AppSettings,
}

/// 当前配置文件的最新版本号。
///
/// 版本历史：
/// - v1: 初始版本（隐式，未存储版本号）
/// - v2: 余额查询 URL 从 `/v1/usage` 迁移到 `/user/balance`
pub const CURRENT_CONFIG_VERSION: u32 = 2;

#[cfg(test)]
mod tests {
    use super::*;

    /// 带窗口/系统副作用的开关**不能**被批量 `save_settings` 改动。
    ///
    /// 钉的是一个真实发生过的缺陷：`floating_widget_enabled` 与
    /// `floating_widget_always_on_top` 曾在 `UserPrefs` 里，而前端 `prefs.ts` 的白名单
    /// 从来没有这两个键 —— 于是每次 saveSettings 提交的 JSON 都缺键，serde 补成 false，
    /// `apply_to` 把用户开着的悬浮球静默关掉。表现为「切一下主题，悬浮球就没了」。
    ///
    /// 判据刻意用**前端会真实发出的那种 JSON**（缺这些键）走一遍完整往返，
    /// 而不是直接断言结构体没有该字段 —— 后者在字段被加回来时才失败，
    /// 而这个在「加回来 且 前端没同步补键」时就失败，正是缺陷的真实形状。
    #[test]
    fn batch_save_settings_never_touches_side_effect_toggles() {
        // 用户已通过专用命令开启的状态。用结构体更新语法而非「先 default 再逐个赋值」：
        // 后者会触发 clippy 的 field_reassign_with_default，而本仓基线是零警告。
        let mut s = AppSettings {
            floating_widget_enabled: true,
            floating_widget_always_on_top: true,
            auto_start: true,
            ..Default::default()
        };

        // 前端真实提交的形态：只有白名单里的键。这里刻意只给一个键，
        // 其余全靠 serde 默认 —— 这正是「缺键」的最坏情况。
        let wire = serde_json::json!({ "theme": "dark" });
        let prefs: UserPrefs = serde_json::from_value(wire).expect("UserPrefs 应能容忍缺键");
        prefs.apply_to(&mut s);

        assert!(
            s.floating_widget_enabled,
            "批量保存把悬浮球开关关掉了 —— 它必须只由 set_floating_widget 改"
        );
        assert!(
            s.floating_widget_always_on_top,
            "批量保存把悬浮球置顶关掉了 —— 它必须只由 set_floating_pinned 改"
        );
        assert!(
            s.auto_start,
            "批量保存把开机自启动关掉了 —— 那是已修过的 P0，不能回归"
        );
    }

    /// 表里的 wire_id 必须与 serde 的 rename 完全一致（P2-8）。
    ///
    /// 这两处是**两套独立的字符串**：serde 的 rename 决定磁盘与 IPC 上的形态，
    /// 表里的 wire_id 决定 as_str()/from_str() 的行为。它们分叉不会编译失败，
    /// 只会让「配置文件里写 claude-cli，代码里却认 claude_cli」这类问题在运行时才炸。
    #[test]
    fn category_wire_id_matches_serde_representation() {
        for c in CategoryType::ALL {
            let json = serde_json::to_string(&c).unwrap();
            let expect = format!("\"{}\"", c.meta().wire_id);
            assert_eq!(json, expect, "{c:?} 的 serde 形态与表里的 wire_id 不一致");
            assert_eq!(CategoryType::from_str(c.meta().wire_id), Some(c), "from_str 回不去");
        }
    }

    /// 三个分类的默认端口必须互不相同，否则两个代理会撞端口、后起的那个起不来。
    #[test]
    fn category_default_ports_are_distinct() {
        let mut seen = std::collections::HashSet::new();
        for c in CategoryType::ALL {
            assert!(seen.insert(c.meta().default_port), "{c:?} 的默认端口与别的分类撞了");
        }
    }

    /// ALL 必须覆盖全部变体：漏一个会让「遍历所有分类」的逻辑静默漏掉它
    /// （表现为某个分类的代理不会随应用启动、托盘菜单里少一项）。
    #[test]
    fn category_all_covers_every_variant() {
        assert_eq!(CategoryType::ALL.len(), 3);
        for c in CategoryType::ALL {
            assert_eq!(c.meta().id, c, "表行的 id 字段与它所属的分类不一致");
        }
    }


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
            balance_query: None,
            cached_balance: None,
            cost_multiplier: None,
            health: HealthState::default(),
        }
    }

    fn mapping(expected: &str, real: &str) -> ModelMapping {
        ModelMapping { id: "m".into(), expected_name: expected.into(), real_name: real.into() }
    }

    #[test]
    fn empty_request_model_never_passes_through_as_empty() {
        // 真机回归（2026-08-02 日志实证）：下游未传 model 时，前 5 级解析全不命中，
        // 原样透传空串 → 上游一律 400「model is required / Model name not specified」，
        // 且**每个候选 Key 都重打一遍**，最终报「全部 Key 失败」，用户以为所有中转商都挂了。
        // 一次会话里 16 次 400 全是这个形状。
        //
        // 更隐蔽的是日志会写成「客户端要 ? → 实际用 gpt-5.5」——展示层自己算了兜底，
        // 而请求体里塞的仍是空串，属「看起来做了、实际没做」。

        // ① 有 default_model → 用它
        let k = key_with(vec![model("gpt-5.5")], vec![], Some("gpt-5.5"));
        let (m, kind) = k.resolve_model_detail("");
        assert_eq!(m, "gpt-5.5", "空请求名必须落到默认兜底，不能是空串");
        assert_eq!(kind, ModelResolveKind::Default);

        // ② 无 default_model 但有模型列表 → 用首个
        let k = key_with(vec![model("claude-opus-4-8"), model("x")], vec![], None);
        assert_eq!(k.resolve_model(""), "claude-opus-4-8");

        // ③ 只配了映射（没填模型列表）→ 用映射的真实名
        let k = key_with(vec![], vec![mapping("claude-opus-4-8", "glm-4.6")], None);
        assert_eq!(k.resolve_model(""), "glm-4.6");

        // ④ 什么都没配 → 只能返回空串（此时硬编造模型名会把「配置不全」
        //    伪装成「上游拒绝」，反而更难排查）
        let k = key_with(vec![], vec![], None);
        assert_eq!(k.resolve_model(""), "");

        // ⑤ 关键反例：非空请求名的行为**不得**被这条兜底改变
        let k = key_with(vec![model("gpt-5.5")], vec![], Some("gpt-5.5"));
        assert_eq!(
            k.resolve_model("claude-opus-4-7"),
            "gpt-5.5",
            "非空请求名仍走原有的默认兜底路径"
        );
        let k = key_with(vec![model("kimi-k2")], vec![], None);
        assert_eq!(
            k.resolve_model("kimi-k2"),
            "kimi-k2",
            "原生支持的模型名必须原样保留"
        );
    }

    #[test]
    fn empty_request_model_prefers_tier_when_only_tiers_configured() {
        // 只配了三档（无模型列表、无映射、无默认）的 Key 也要能兜底 ——
        // 否则这类 Key 遇到「下游不传 model」时仍会 400。
        let mut k = key_with(vec![], vec![], None);
        k.tier_sonnet = Some("glm-4.6".into());
        k.tier_opus = Some("deepseek-v3".into());
        // 顺序 opus → sonnet → haiku，与家族默认口径一致
        assert_eq!(k.resolve_model(""), "deepseek-v3");

        let mut k2 = key_with(vec![], vec![], None);
        k2.tier_haiku = Some("qwen-turbo".into());
        assert_eq!(k2.resolve_model(""), "qwen-turbo");
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

    /// **P2-8 最关键的一条**：settings 的分类键从字符串换成枚举后，磁盘 JSON 形状必须**零变化**。
    ///
    /// 这条守的是数据丢失级事故：`config.json` 是**已经落在用户机器上的**文件。
    /// 若序列化出来的键名变了（比如 `claude-cli` 变成 `ClaudeCli`），老用户升级后
    /// 那几项会落进「未知字段」被静默丢弃 —— 已选模型、粘滞端口、MCP 注册记录全部归零，
    /// 而且**没有任何报错**，只是「设置好像被重置了」。
    ///
    /// 用**手写的字符串字面量**而不是 `to_string(&Default::default())`：后者会随代码一起变，
    /// 测不出格式回归，那才是这条测试的全部价值。
    #[test]
    fn legacy_settings_json_round_trips_with_identical_shape() {
        // 一份「旧格式」settings：分类键都是字符串。这正是用户磁盘上的形态。
        const LEGACY: &str = r#"{
            "theme": "dark",
            "language": "en",
            "autoStart": true,
            "mcpPort": 9531,
            "mcpRegisteredCategories": ["claude-cli", "codex"],
            "activeModels": { "codex": "gpt-5", "claude-cli": "claude-opus-4-8" },
            "activeEfforts": { "codex": "xhigh" },
            "proxyPorts": { "claude-cli": 47100, "codex": 47101 }
        }"#;

        let s: AppSettings = serde_json::from_str(LEGACY).expect("旧格式必须能读出来");

        // 值都落到了正确的枚举键上
        assert_eq!(s.active_models.get(&CategoryType::Codex).map(String::as_str), Some("gpt-5"));
        assert_eq!(
            s.active_models.get(&CategoryType::ClaudeCli).map(String::as_str),
            Some("claude-opus-4-8")
        );
        assert_eq!(s.active_efforts.get(&CategoryType::Codex).map(String::as_str), Some("xhigh"));
        assert_eq!(s.proxy_ports.get(&CategoryType::ClaudeCli).copied(), Some(47100));
        assert_eq!(s.proxy_ports.get(&CategoryType::Codex).copied(), Some(47101));
        assert_eq!(
            s.mcp_registered_categories,
            vec![CategoryType::ClaudeCli, CategoryType::Codex]
        );
        assert!(s.auto_start);
        assert_eq!(s.mcp_port, 9531);

        // 写回后，那几段的 JSON 形状必须与读进来时**完全一致**。
        // 比较 serde_json::Value 而非字节：键顺序允许变（BTreeMap 会排序），键集与值不许变。
        let back = serde_json::to_value(&s).unwrap();
        let orig: serde_json::Value = serde_json::from_str(LEGACY).unwrap();
        for field in ["mcpRegisteredCategories", "activeModels", "activeEfforts", "proxyPorts"] {
            assert_eq!(
                back.get(field),
                orig.get(field),
                "{field} 写回后与原始磁盘格式不一致 —— 老用户的这一项会丢"
            );
        }
    }

    /// 未知分类键必须**只丢那一项**，不能连累整份配置。
    ///
    /// 场景：用户装过某个未来版本（多了第 4 个分类）后又降级回来，或手改过 config.json。
    /// 若整份解析失败，会走 `.corrupt` 备份路径 —— 用户看到的是「配置全没了」，
    /// 而实际只是多了一个不认识的键。
    #[test]
    fn unknown_category_key_is_dropped_without_losing_the_rest() {
        const JSON: &str = r#"{
            "theme": "system",
            "activeModels": { "codex": "gpt-5", "gemini-cli": "gemini-3" },
            "proxyPorts": { "claude-cli": 47100, "some-future-tool": 47999 },
            "mcpRegisteredCategories": ["codex", "not-a-real-category"]
        }"#;
        let s: AppSettings = serde_json::from_str(JSON).expect("有未知键也必须能读出来");
        assert_eq!(s.active_models.len(), 1, "只保留认识的那一项");
        assert_eq!(s.active_models.get(&CategoryType::Codex).map(String::as_str), Some("gpt-5"));
        assert_eq!(s.proxy_ports.get(&CategoryType::ClaudeCli).copied(), Some(47100));
        assert_eq!(s.mcp_registered_categories, vec![CategoryType::Codex]);
        assert_eq!(s.theme, "system", "其余字段不受影响");
    }

    // ===== 桌面端对外模型名「建议」（UX#4 即时校验的可点修法） =====
    /// 给出的建议**自己必须合规**，否则用户点了「采纳」保存仍被拒 —— 那比不给建议更糟
    /// （他会以为按钮坏了，或以为自己哪里还没改对）。
    ///
    /// 遍历全部 50 条厂商名黑名单 + 词边界项各造一个 `<厂商名>-4.6` 送进去，
    /// 只要有一条建议不合规就红。
    #[test]
    fn suggest_desktop_name_is_always_accepted_by_desktop() {
        let mut sources: Vec<String> =
            DESKTOP_DENY_SUBSTRINGS.iter().map(|k| format!("{k}-4.6")).collect();
        // 词边界项（不在子串黑名单里，靠 \b 匹配拒掉）也要覆盖
        for k in ["ling", "unic", "ds-v3", "k2.5", "m2.1", "phi4"] {
            sources.push(format!("{k}-4.6"));
        }
        for src in &sources {
            let s = suggest_desktop_model_name(src, &[]);
            assert!(
                is_desktop_acceptable_model_id(&s),
                "对 {src} 给出的建议 {s} 本身不被桌面端接受"
            );
        }
    }

    /// 同一份报告里的多个建议必须互不相同。
    ///
    /// 判据：两条映射若用同一个对外名，`resolve_model` 取首个命中，第二条**永远匹配不到** ——
    /// 表现为「配了映射但那个模型一直路由到别处」，极难联想到是重名。
    #[test]
    fn suggest_desktop_name_dedups_within_one_report() {
        let issues = desktop_model_name_issues(&["glm-4.6".into(), "gpt-4.6".into()]);
        assert_eq!(issues.len(), 2);
        assert_ne!(
            issues[0].suggestion, issues[1].suggestion,
            "同一批建议撞名会让后一条映射永远匹配不到"
        );
    }

    /// 建议要保留版本数字，用户才能一眼对上是哪个上游模型（不然不敢点）。
    #[test]
    fn suggest_desktop_name_keeps_version_digits() {
        let s = suggest_desktop_model_name("glm-4.6", &[]);
        assert!(s.contains("4-6"), "建议 {s} 丢了版本号，用户对不上是哪个模型");
    }

    /// 不能撞上三档追加的家族代表名。
    ///
    /// 配了三档时 `serviceable_models` 会追加 `claude-*-4-5`（见本文件 serviceable_models 规则 3），
    /// 建议若正好等于它，就会把档位代表名顶掉。
    #[test]
    fn suggest_desktop_name_avoids_taken_tier_family_names() {
        let taken = vec!["claude-opus-4-5".to_string()];
        let s = suggest_desktop_model_name("glm-4.5-max", &taken);
        assert_ne!(s, "claude-opus-4-5", "不得与已占用的档位代表名相同");
        assert!(is_desktop_acceptable_model_id(&s), "去重后仍须合规");
    }

    /// 非桌面端分类一律「不适用」，一个提示都不该出。
    ///
    /// 判据：CLI 有 `to_gateway_model_id` 包 `claude-synaroute-` 前缀救回、Codex 走 OpenAI 形态，
    /// 在那两个分类报警会逼用户去改**本来就正常**的配置。
    #[test]
    fn desktop_report_not_applicable_for_cli_and_codex() {
        for cat in [CategoryType::ClaudeCli, CategoryType::Codex] {
            let mut k = key_with(vec![model("glm-4.6"), model("gpt-5")], vec![], None);
            k.category_id = cat;
            let r = desktop_model_name_report(&k);
            assert!(!r.applicable, "{cat:?} 不该适用桌面端模型名判据");
            assert!(r.issues.is_empty());
        }
    }

    /// **一键修法必须给每个模型都建映射**，只给不合规的建会静默吞掉合规模型。
    ///
    /// 判据来自 `serviceable_models` 的语义：只要存在任意一条有效映射，`models` 列表就被
    /// **整份忽略**、对外集合只由映射决定。所以 models=[glm-4.6, claude-opus-4-8] 时
    /// 若只给 glm-4.6 加映射，`claude-opus-4-8` 会从桌面端选择器里**消失**
    /// —— 用户「修完一个问题、丢了一个模型」，且毫无提示。
    #[test]
    fn applying_report_suggestions_makes_key_saveable() {
        let mut k = key_with(
            vec![model("glm-4.6"), model("grok-4.5"), model("claude-opus-4-8")],
            vec![],
            None,
        );
        k.category_id = CategoryType::ClaudeDesktop;

        let before = desktop_model_name_report(&k);
        assert_eq!(before.issues.len(), 2, "glm 与 grok 两条不合规");

        // 照抄前端「一键加映射」的规则：models 里**每一个**模型都建一条，
        // 不合规的用 suggestion 作对外名，合规的建 realName→realName 的恒等映射。
        k.mappings = k
            .models
            .iter()
            .enumerate()
            .map(|(i, m)| {
                let outward = before
                    .issues
                    .iter()
                    .find(|x| x.name == m.real_name)
                    .map(|x| x.suggestion.clone())
                    .unwrap_or_else(|| m.real_name.clone());
                ModelMapping {
                    id: format!("m_{i}"),
                    expected_name: outward,
                    real_name: m.real_name.clone(),
                }
            })
            .collect();

        let after = desktop_model_name_report(&k);
        assert!(after.issues.is_empty(), "修法之后不应再有不合规名：{:?}", after.issues);
        assert_eq!(
            k.serviceable_models().len(),
            3,
            "三个模型都要仍然对外可见（漏建恒等映射会吞掉合规的那个）"
        );
    }
}
