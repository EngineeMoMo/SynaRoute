//! Codex 模型目录（`model_catalog_json`）：让**非 GPT 模型**出现在 Codex 自己的模型选择器里，
//! 并让「思考强度」档位由我们声明、由 Codex UI 真实呈现。
//!
//! # 为什么走本地文件，而不是让 Codex 来 HTTP 拉
//!
//! Codex 有两条取模型清单的路，**互斥**（`models-manager/src/manager.rs`）：
//!
//! 1. `OpenAiModelsManager` —— 向 provider 的 `base_url` 打 `GET {base}/models?client_version=X`，
//!    响应体是 `{"models":[…]}`（`codex-api/src/endpoint/models.rs:70`），
//!    结果缓存进 `~/.codex/models_cache.json`，TTL 300s。
//! 2. `StaticModelsManager` —— 由 `model_catalog_json` 指定的本地文件喂满，
//!    它的 `raw_model_catalog` 把 `_refresh_strategy` 与 `_http_client_factory` 两个参数**整个丢弃**、
//!    `refresh_if_new_etag` 是空实现 → **配了它就永不联网、永不碰那个缓存文件**。
//!
//! 选 2 的三条理由，按重要性：
//!
//! - `models_cache.json` 是**全局单文件、无 provider 维度**（只有一份 `fetched_at`/`etag`/
//!   `client_version`）。走路 1 就等于把我们的清单写进一个官方登录态也会读的地方，
//!   而「切回官方后会不会读到我们的列表」没有任何字段能保证。路 2 完全不碰它。
//! - 路 1 要改 `proxy.rs` 的 `handle_list_models`，而那个文件棘轮余量为 0。
//! - 我们**已经在写** `~/.codex/config.toml`（`super::apply_at`），多写一行指针的边际成本近乎零。
//!
//! 代价是明确的：`model_catalog_json` 官方注明 **"applied on startup only"**
//! （`codex-rs/core/config.schema.json`），改了模型列表要重启一次 Codex。这与本仓已有的
//! 「`rewrite_registered_clients` 重写完客户端配置后要重启一次才读到」是同一个已知代价。
//!
//! # 三条实测判据（codex-cli 0.150.0-alpha.8，隔离 `CODEX_HOME` + `codex debug models`）
//!
//! | 判据 | 实测结果 | 失效方向 |
//! |---|---|---|
//! | `base_instructions` 或 `model_messages.instructions_template` **至少有一个** | 都省 → `Error: failed to parse model_catalog_json … is missing both …` | **Codex 启动即报错、完全起不来**（响亮，但比回落严重得多） |
//! | 目录是 **full replacement**，不是合并 | 只放我们 2 条 → `debug models` 输出正好 2 条，官方 10 条全消失 | 用户原有的 GPT 模型**从菜单里消失** |
//! | 非 GPT 名字**不被过滤** | `claude-opus-4-8` / `glm-4.6` 原样保留 | —— 与 Claude 桌面端那个 `sD()` 硬过滤完全不同，不需要 `claude-synaroute-` 前缀 |
//!
//! 另有两条从官方 `codex-rs/models-manager/models.json`（内置基底，10 条）抽出的数值判据：
//! `minimal_client_version` 是**字符串**（`"0.144.0"` / `"0.0.1"`，官方单元测试里那个数组
//! `[0,99,0]` 已过时）；官方 `priority` 占 **1~43**，故自定义条目必须从
//! [`PRIORITY_BASE`] 起，否则会抢掉用户的默认模型。
//!
//! # 工具能力为什么这样声明
//!
//! **因果证据（不是「观察到一致」）**：`core/src/tools/spec_plan.rs:366` 构建工具路由器时
//! `let tool_mode = effective_tool_mode(turn_context, model_info);` →
//! `let code_mode_enabled = matches!(tool_mode, ToolMode::CodeMode | ToolMode::CodeModeOnly);`
//! 而 `effective_tool_mode`（`core/src/tools/mod.rs`）直接从 `model_info` 取 —— 也就是
//! **我们在目录里写的这个字段就是那个输入**。
//!
//! 官方基底实测对照（`models-manager/models.json`，10 条）：
//!
//! - `gpt-5.6-sol`/`-terra`/`-luna` = `"code_mode_only"` → `additional_tools` + exec 沙箱（241 条）
//! - `gpt-5.5`/`5.4`/`5.2` = **`null`** → 顶层 `tools`（具名 function call，73 条）
//!
//! 与 docs/13 第十一节那 900 条按模型名的统计一一对应 —— 名字之所以看起来「决定」形态，
//! 是因为 Codex 按 slug 查元数据，查不到就落 `model_info_from_slug` 的 fallback
//! （`tool_mode: None`）。`claude-opus-4-7` 那 18 条走的正是 fallback，
//! 而 docs/13 实测它**工具与 MCP 全通**（含 `tool_search` 延迟检索链）。
//!
//! 故我们一律发 `null`（= `ToolMode::Direct`）：它同时是通用 OpenAI 语义、是取证覆盖过的
//! 那条、也是最保守的一档 —— `effective_tool_mode` 里 `CodeMode`（非 Only）在 code_mode
//! 不可用时**会回落 Direct**，而 `CodeModeOnly` 不回落。cc-switch 的 DeepSeek 模板同值。
//!
//! # 🔴 思考档位：判据是「我们这一跳怎么落地 effort」，不是「模型叫什么名字」
//!
//! 声明一个上游其实不认的档位，后果是**界面撒谎**：用户在 Codex 里切档位、请求正常返回、
//! 行为毫无变化，而日志里没有任何线索指向「那个档位被丢掉了」。cc-switch 用 22 家预设
//! 换来的结论原话是：只暴露「思考开/关」的供应商（Kimi、GLM、Qwen、MiniMax、MiMo、
//! SiliconFlow）**在 Codex 里调节思考等级不会有任何效果**。
//!
//! 所以本模块不去猜模型名，而是看 [`Protocol`] —— 那是我们自己这一跳的确定事实：
//!
//! | 上游 protocol | effort 在我们这里怎么落地 | 声明档位？ |
//! |---|---|---|
//! | [`Protocol::Anthropic`] | `convert.rs` 自己算成 `thinking.budget_tokens` | ✅ **生效由我们保证**，与上游认不认 effort 无关 |
//! | [`Protocol::OpenaiResponses`] | 原样透传 `reasoning.effort`（原生语义） | ✅ |
//! | [`Protocol::OpenaiChat`] | 落成顶层 `reasoning_effort`，认不认全看上游 | ❌ 不声明 |
//!
//! **口径必须是交集，不能只看主 Key** —— 同 `service::models_for_apply` 那条注释里记的
//! 「用超集口径导致桌面端列出备用 Key 服务不了的模型、故障转移后必然 404」。这里的对应
//! 失效是：主 Key 走 Anthropic 所以声明了四档，而故障转移落到一条 Chat 上游的备用 Key 上，
//! 档位当场静默失效。故 [`effort_levels_for`] 只在**所有**启用 Key 都能让 effort 生效时才声明。

use crate::error::{AppError, AppResult};
use crate::model::{Protocol, ProviderKey};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// 我们生成的目录文件名，落在 `~/.codex/` 下（与 `config.toml` 同目录）。
///
/// 名字里带 `synaroute` 是[`pointer_is_ours`]的唯一判据：认领指针时只认这个文件名，
/// 用户自己的目录路径**绝不覆盖**。cc-switch 为此修过一条（#6087）——它早期版本无条件
/// 把指针指向自己生成的文件、丢弃用户自定义路径，而且**改过的指针不会被还原**，
/// 只能发公告让用户手动指回去。
const CATALOG_FILE: &str = "synaroute-model-catalog.json";

/// 自定义条目的 `priority` 起点。
///
/// 官方内置基底（`models-manager/models.json`，10 条）占 **1~43**，实测值。数字越小越靠前，
/// 且 `build_available_models` 会把**排序后第一个 `visibility == list` 的**标成 `isDefault`。
/// 从 43 之后起，语义是「我们的模型可选，但不抢用户原有的默认模型」——
/// 这与 codexloom 把 DeepSeek 的 priority 从 1 改成 50 是同一个考量。
///
/// ⚠️ 只有在 catalog 里**同时**保留了官方条目时这个数字才有意义。当前实现是纯替换
/// （见 [`build_catalog`] 的文档），此时我们的第一条**就是**默认模型 —— 那也是对的，
/// 因为用户接入 SynaRoute 就是要走我们的路由。
const PRIORITY_BASE: i64 = 50;

/// `ModelInfo` 里**没有** `#[serde(default)]` 的字段，也就是「缺一个整份目录就解析失败」的那些。
///
/// 出处：`codex-rs/protocol/src/openai_models.rs` 的 `pub struct ModelInfo`（无容器级
/// `#[serde(default)]`，逐字段核对过）。`Option<T>` 但没有 `serde(default)` 的字段
/// **仍然必须出现**（值可以是 `null`）——`description` / `availability_nux` / `upgrade` /
/// `default_verbosity` / `apply_patch_tool_type` 四个都属于这一类，最容易漏。
///
/// 为什么要把它列成常量而不是「写代码时小心点」：漏一个键的表现是 **Codex 启动即报错、
/// 完全起不来**，而报错文本停在第一个出错的模型上、不会告诉你还差哪些。
/// 有 `every_entry_carries_all_required_keys` 一条测试遍历它。
const REQUIRED_KEYS: &[&str] = &[
    "slug",
    "display_name",
    "description",
    "supported_reasoning_levels",
    "shell_type",
    "visibility",
    "supported_in_api",
    "priority",
    "availability_nux",
    "upgrade",
    "support_verbosity",
    "default_verbosity",
    "apply_patch_tool_type",
    "truncation_policy",
    // 🔴 它在 `ModelInfo` 里是 `#[serde(default = "default_true")]` —— **省略即变成 true**，
    // 也就是悄悄打开一个 fallback 下关着的开关。列进来让 `validate` 也拦住「不小心删掉那一行」。
    "include_apps_usage_instructions",
    "experimental_supported_tools",
];

/// 顶层系统提示词（落在 `model_messages.instructions_template`）。
///
/// **就是 Codex 自己那份 `prompt.md`，逐字节副本。** 来源、许可（Apache-2.0）与代价记在
/// [`THIRD_PARTY_NOTICES.md`](../../../THIRD_PARTY_NOTICES.md)。
///
/// # 🔴 为什么必须逐字带，不能自己写一份短的
///
/// 我第一版写的是一段 ~1 KB 的自撰提示词，理由是「官方那份含 based on GPT-5 的错误陈述 +
/// 许可风险 + 版本漂移」。**那三条理由有两条是错的，第三条不足以抵掉代价**：
///
/// - 「含 GPT-5 字样」——错。那句在 `DEFAULT_PERSONALITY_HEADER` **常量**里，
///   `prompt.md` 全文零 `GPT` 字样，开头是 "You are a coding agent running in the Codex CLI"，
///   **模型中性**。
/// - 「许可风险」——错。openai/codex 是 **Apache-2.0**，逐字再分发合法，附声明即可。
/// - 「版本漂移」——真，但影响是行为准则渐进落后，而下面这条的代价大得多。
///
/// 真正推翻它的是 `models-manager/src/model_info.rs::model_info_from_slug`：**今天**没有目录时，
/// Codex 给未知模型（= 每一个走 SynaRoute 的非 GPT 模型）填的就是
/// `instructions_template: Some(BASE_INSTRUCTIONS)`，而 `BASE_INSTRUCTIONS =
/// include_str!("../prompt.md")`。它有 `# Tool Guidelines` / `## Shell commands` /
/// `## update_plan` 三节工具使用契约。换成短的 = **静默削弱现有工具调用能力**，
/// 正是本功能必须保住的东西。
///
/// # 我那条「短一点是安全的」推断为什么不成立
///
/// 当时的旁证是 `codex debug prompt-input`：`input[]` 里那条 9965 字符的 developer 消息
/// 由 Codex 自己注入（skills / escalation / sandbox），与本常量无关。那个观察是对的，
/// **但结论跳了一步** —— Responses API 的 `instructions` 是**顶层字段**，压根不在
/// `input[]` 里，所以那条探针从头到尾就没看见过本常量。用「看不到差异」当「没有差异」，
/// 是这个仓库记过多次的那类假绿。
const INSTRUCTIONS_TEMPLATE: &str = include_str!("codex_base_instructions.md");

/// 我们声明的思考档位，**刻意与官方 `gpt-5.5` 那条一字不差**（`low/medium/high/xhigh`）。
///
/// # 🔴 为什么不含 `max` / `ultra`
///
/// `ReasoningEffort` 枚举里有它们（官方 `gpt-5.6-*` 就声明了），但我们的
/// `convert.rs::effort_to_thinking_budget` 只 match `minimal/low/medium/high/xhigh`，
/// 其余走 `_ => return None` = **不开扩展思考**。也就是说声明 `max` 的后果是
/// 「用户在 Codex 里选了最高档 → 反而完全不思考」，方向最坏的一种界面撒谎。
/// 要加它们必须先在 `effort_to_thinking_budget` 里给出真实预算。
/// 有 `declared_levels_are_all_understood_by_the_converter` 一条测试钉住。
///
/// # 为什么也不含 `minimal`
///
/// 它在转换器里是「不开思考」，语义真实，但官方 10 条基底**全都是 `low` 起**
/// ——也就是 Codex UI 对 `minimal` 档的呈现没有被官方条目验证过。与官方对齐是这里
/// 唯一能拿到的保证，不为多一档去赌它长什么样。
const EFFORT_LEVELS: &[(&str, &str)] = &[
    ("low", "Fast responses with light reasoning"),
    ("medium", "Balanced reasoning depth for everyday tasks"),
    ("high", "Deeper reasoning for complex problems"),
    ("xhigh", "Maximum reasoning depth SynaRoute can map"),
];

/// 默认档位。与官方 `gpt-5.5` 的 `default_reasoning_level` 一致。
const DEFAULT_EFFORT: &str = "medium";

/// 这一组 Key 上，Codex 里选的思考档位能不能真的到达上游。
///
/// **交集口径**：只要有**一条**启用 Key 的上游是 Chat Completions，就返回 `false`。
/// 理由见模块头那张表——故障转移随时会落到那条 Key 上，而档位在那一跳是静默失效的。
/// 一个「大多数时候生效」的档位选择器比没有选择器更糟：用户无法判断某次没变化
/// 是模型的正常波动，还是档位压根没送出去。
///
/// 空列表返回 `false`（没有 Key 就没有依据，保守）。
fn effort_survives_all_keys(keys: &[ProviderKey]) -> bool {
    !keys.is_empty()
        && keys.iter().all(|k| match k.protocol {
            // 我们自己算成 thinking.budget_tokens，生效与上游认不认 effort 无关。
            Protocol::Anthropic => true,
            // 原生语义，原样透传。
            Protocol::OpenaiResponses => true,
            // 落成顶层 reasoning_effort，认不认全看上游；cc-switch 实测 Kimi/GLM/Qwen/
            // MiniMax/MiMo/SiliconFlow 一律忽略。不赌。
            Protocol::OpenaiChat => false,
        })
}

/// 该组 Key 应当声明的档位列表（`supported_reasoning_levels` 的值）。
/// 不该声明时返回**空数组** —— 实测空数组是合法的（`glm-4.6` 那条探针），Codex 不报错、
/// 只是那个模型在 UI 上没有档位可选。
fn effort_levels_for(keys: &[ProviderKey]) -> Vec<Value> {
    if !effort_survives_all_keys(keys) {
        return Vec::new();
    }
    EFFORT_LEVELS
        .iter()
        .map(|(effort, desc)| json!({ "effort": effort, "description": desc }))
        .collect()
}

/// 拿不到真实上下文窗口时填的值。**与 Codex 自己的 fallback 一字不差**
/// （`model_info_from_slug` 里 `context_window: Some(272_000)`）。
///
/// 不省略这个键的理由：`ModelInfo.context_window` 是 `Option` 且带 `serde(default)`，
/// 省了就是 `None`，而 Codex 用它推自动压缩阈值（`auto_compact_token_limit` 缺省时取 90%）。
/// 给 `None` 与今天的行为不同 —— 本模块的基线是**不改变现有行为**，那就照抄它的兜底值。
const FALLBACK_CONTEXT_WINDOW: i64 = 272_000;

/// 构造一个模型条目。
///
/// # 基线是「Codex 今天对未知模型用的那份元数据」，不是「官方 GPT 条目」
///
/// 逐字段对着 `models-manager/src/model_info.rs::model_info_from_slug` 抄，
/// **只改本功能必须改的四项**，其余一律保持：
///
/// | 改了 | 从 | 到 | 为什么 |
/// |---|---|---|---|
/// | `visibility` | `none` | `list` | 这就是「用户在 Codex 里看不到自己的模型」的根因 |
/// | `supported_reasoning_levels` | `[]` | 见 [`effort_levels_for`] | 这就是「Codex Desktop 不下发 effort」的根因 |
/// | `priority` | `99` | [`PRIORITY_BASE`]`+i` | 让顺序跟随我们的模型列表顺序 |
/// | `context_window` | 恒 `272000` | 有取证时用真实值 | 我们手上有 `ProviderKey.models[].context_window` |
///
/// **刻意没改的两项**，别当遗漏：
///
/// - `apply_patch_tool_type: null` —— fallback 就是 `None`，也就是说走 SynaRoute 的非 GPT
///   模型**今天没有 apply_patch 工具**，一直用 shell 改文件。给它 `"freeform"` 是**新增**
///   一个未验证过的工具（模型不会写 freeform patch 格式 → 产出坏补丁），而本轮的判据是
///   「不退化」，不是「顺手增强」。要加得单独一轮 + 真机验证。
/// - `include_skills_usage_instructions` / `_plugin_` / `_apps_` 全 `false` —— 同上。
///   ⚠️ `include_apps_usage_instructions` 在 `ModelInfo` 里是 `#[serde(default = "default_true")]`，
///   **省略它会变成 `true`**，那就悄悄开了一个 fallback 下关着的开关。必须显式写 `false`。
fn build_entry(slug: &str, index: usize, levels: Vec<Value>, context_window: Option<u32>) -> Value {
    let has_levels = !levels.is_empty();
    let ctx = context_window.map(i64::from).unwrap_or(FALLBACK_CONTEXT_WINDOW);
    json!({
        "slug": slug,
        // 与 slug 相同：用户在 Codex 菜单里看到的就是他在 SynaRoute 里配的对外名。
        // 加 "(SynaRoute)" 之类的后缀会造出第二个名字，而排障时「他说的那个模型」
        // 必须能一字不差地对上我们的配置。
        "display_name": slug,
        "description": "Routed by SynaRoute",
        "default_reasoning_level": if has_levels { json!(DEFAULT_EFFORT) } else { Value::Null },
        "supported_reasoning_levels": levels,
        "shell_type": "unified_exec",
        "visibility": "list",
        // 🔴 必须 true：`ModelPreset::filter_by_auth` 是 `chatgpt_mode || supported_in_api`，
        // 而我们走 experimental_bearer_token → chatgpt_mode = false。给 false 的表现是
        // 模型**静默从选择器里消失**（同 Claude 桌面端那个 sD() 硬过滤的失效方向）。
        "supported_in_api": true,
        "priority": PRIORITY_BASE + index as i64,
        "availability_nux": Value::Null,
        "upgrade": Value::Null,
        "model_messages": { "instructions_template": INSTRUCTIONS_TEMPLATE },
        "include_skills_usage_instructions": false,
        "include_plugin_usage_instructions": false,
        "include_apps_usage_instructions": false,
        "supports_reasoning_summary_parameter": true,
        "default_reasoning_summary": "auto",
        "support_verbosity": false,
        "default_verbosity": Value::Null,
        "apply_patch_tool_type": Value::Null,
        "web_search_tool_type": "text",
        // 注意是 bytes 不是 tokens —— fallback 用 `TruncationPolicyConfig::bytes(10_000)`，
        // 而官方 GPT 条目用 tokens。抄错这一个键会改变工具输出的截断行为。
        "truncation_policy": { "mode": "bytes", "limit": 10_000 },
        "supports_image_detail_original": false,
        "context_window": ctx,
        "max_context_window": ctx,
        "effective_context_window_percent": 95,
        "experimental_supported_tools": [],
        "supports_search_tool": false,
        "use_responses_lite": false,
        // null = ToolMode::Direct（顶层 tools 形态）。见模块头那张 tool_mode 对照表。
        "tool_mode": Value::Null,
    })
}

/// 该对外模型名在**已取证的**启用 Key 上都成立的上下文窗口。
///
/// 取已知值的**最小值**，而不是主 Key 的值：故障转移会落到任意一条 Key 上，
/// 报一个只有主 Key 撑得住的窗口，等于让 Codex 把超长上下文送去一条撑不住的上游（400）。
///
/// ⚠️ **边界（审查时收窄的说法）**：只有**填过** `context_window` 的 Key 参与取最小值。
/// 有 Key 没填时，我们报的窗口可能超过它的真实能力 —— 那是取证缺失的固有限制，
/// 这里无法改进（未知就是未知）。失效方向是上游 400，**响亮**，且 `budget.rs` 那套
/// 输出上限判据仍在管着 `max_tokens`。全部 Key 都没填 → `None` → 由 [`build_entry`]
/// 落到 [`FALLBACK_CONTEXT_WINDOW`]。
fn context_window_across_keys(name: &str, keys: &[ProviderKey]) -> Option<u32> {
    keys.iter()
        .filter_map(|k| k.context_window_for_outward(name))
        .min()
}

/// 构造整份目录。
///
/// # 🔴 这是 full replacement，不是合并（实测确认）
///
/// 实测：只放我们 2 条 → `codex debug models` 输出正好 2 条，官方内置 10 条**全部消失**。
/// 也就是说用户接入 Codex 分类之后，他在 Codex 菜单里**只能看到 SynaRoute 路由的模型**。
///
/// 这是刻意的，不是没想到：用户接入 SynaRoute 就是要让 Codex 走我们的代理，此时列一个
/// `gpt-5.6-terra` 在菜单里是**假选项** —— 选中它会被当成对外名发给我们，走
/// `resolve_model` 解析，落到一条根本不叫这个名字的上游模型上。菜单里出现选不动的东西，
/// 比菜单里少几个更难排查。还原会把 `config.toml` 整份从 `.bak` 恢复（指针那行随之消失）、
/// 并按 `.synaroute-created` 标记删掉目录文件本身，官方条目当场回来。
pub(super) fn build_catalog(models: &[String], keys: &[ProviderKey]) -> Value {
    let levels = effort_levels_for(keys);
    let entries: Vec<Value> = models
        .iter()
        .enumerate()
        .map(|(i, name)| {
            build_entry(
                name,
                i,
                levels.clone(),
                context_window_across_keys(name, keys),
            )
        })
        .collect();
    json!({ "models": entries })
}

/// 我们生成的目录文件路径：`~/.codex/synaroute-model-catalog.json`。
pub(super) fn catalog_path() -> AppResult<PathBuf> {
    let cfg = super::config_path()?;
    Ok(cfg
        .parent()
        .ok_or_else(|| AppError::ToolConfig("无法定位 .codex 目录".into()))?
        .join(CATALOG_FILE))
}

/// `config.toml` 里那个 `model_catalog_json` 指针**是不是我们的**。
///
/// 判据只有一条：文件名等于 [`CATALOG_FILE`]。目录部分刻意不比 —— 用户可能把整个
/// `CODEX_HOME` 搬了地方，那时路径不同但仍然是我们的文件。
///
/// # 为什么必须有这个判据
///
/// cc-switch 为此修过一条（#6087）：它早期版本切换供应商时**无条件**把指针指向自己生成的
/// 目录、丢弃用户自定义路径，而且被改过的指针**不会被还原** —— 只能发升级公告让用户
/// 手动指回去。指针是用户可能自己在用的东西（官方文档就教了 `model_catalog_json` 怎么配），
/// 抢掉它的后果是用户精心配的模型目录静默失效。
pub(super) fn pointer_is_ours(pointer: &str) -> bool {
    Path::new(pointer)
        .file_name()
        .is_some_and(|n| n == CATALOG_FILE)
}

/// 这批模型能不能撑起一份目录。
///
/// **空列表一律返回 `false`**，此时调用方必须既不写目录也不写指针（见 [`super::apply_at`]）。
/// 一份 `{"models":[]}` 会让 Codex 的 `default_model_from_available` 对着空列表挑默认模型，
/// 而它对空输入的行为**我们没有验证过** —— 而且这种状态下用户在菜单里一个模型都选不到，
/// 比「回落到内置的 GPT 条目」糟得多。宁可让本功能对这种配置不生效。
///
/// 什么时候会空：该分类**没有启用的 Key**，或主 Key 没有任何可服务模型。
/// ⚠️ **不包含**「多 Key 交集为空」——`discoverable_models`（`proxy.rs`）在交集为空时
/// **刻意回退主 Key 的超集**，故它永不返回空。原注释把这一支写成成因是错的。
/// 代价随之而来且是已知的：目录里可能有备用 Key 服务不了的条目，故障转移到那条时会 404
/// （effort 与 context_window 两维走的是真交集，模型列表这一维没有）。
pub(super) fn can_build(models: &[String]) -> bool {
    !models.is_empty()
}

/// 「工具配置预览」里代表这份目录文件的那一条。
///
/// 🔴 **刻意不给正文**：目录约 190 KB（9 条 × 内嵌的 20 KB 官方提示词），
/// 走 `read_preview_text` 会把 32 KB 提示词原文塞进每次预览的 IPC 载荷，而用户在这个面板里
/// 要核对的是「SynaRoute 动了我哪些文件」，不是读一遍提示词。给条目数与字节数就够。
///
/// 有它的理由：预览面板是用户核对改动范围的**唯一**界面。本轮新增了这个凭空创建的文件，
/// 而 summary 原文写着「只写 config.toml」、`files` 也只列了两条 —— 用户不会知道
/// 自己的 `.codex` 目录里多了一份 190 KB 的东西。
pub(super) fn file_preview() -> AppResult<crate::tools::ToolConfigFilePreview> {
    let path = catalog_path()?;
    let (exists, note) = match std::fs::metadata(&path) {
        Ok(m) => {
            let n = std::fs::read_to_string(&path)
                .ok()
                .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                .and_then(|v| v["models"].as_array().map(Vec::len))
                .unwrap_or(0);
            (
                true,
                Some(format!(
                    "/* 模型目录：{n} 个条目、{} 字节。每条内嵌官方提示词（Apache-2.0），正文略。\n\
                       还原时整份删除。 */",
                    m.len()
                )),
            )
        }
        Err(_) => (false, None),
    };
    Ok(crate::tools::ToolConfigFilePreview {
        path: path.display().to_string(),
        exists,
        format: "json".into(),
        content: note,
    })
}

/// 写盘前自检：每个条目都带齐 [`REQUIRED_KEYS`]。
///
/// # 为什么这道门必须在写盘**之前**
///
/// 目录解析失败的后果不是「回落到内置条目」，而是 **Codex 启动即报错、完全起不来**
/// （实测报文：`Error: failed to parse model_catalog_json … as JSON: model X is missing
/// both base_instructions and model_messages.instructions_template`）。也就是说一个漏掉的
/// 键会把用户的 Codex 整个弄挂，而他刚做的动作是「在 SynaRoute 里点了接入」——
/// 归因方向完全错。写之前挡住，报错里带上缺哪个键。
fn validate(catalog: &Value) -> AppResult<()> {
    let models = catalog
        .get("models")
        .and_then(Value::as_array)
        .ok_or_else(|| AppError::ToolConfig("模型目录缺少 models 数组".into()))?;
    for entry in models {
        let slug = entry.get("slug").and_then(Value::as_str).unwrap_or("<无 slug>");
        for key in REQUIRED_KEYS {
            if entry.get(*key).is_none() {
                return Err(AppError::ToolConfig(format!(
                    "模型目录条目 `{slug}` 缺少必填字段 `{key}`；写入会导致 Codex 启动即报错，已中止"
                )));
            }
        }
        // 上面那条报文里的字段名就是从这个失败模式抄来的，单独再判一次：
        // `model_messages` 存在但 `instructions_template` 为空，同样会被 Codex 拒绝。
        let has_instructions = entry
            .get("model_messages")
            .and_then(|m| m.get("instructions_template"))
            .and_then(Value::as_str)
            .is_some_and(|s| !s.trim().is_empty())
            || entry
                .get("base_instructions")
                .and_then(Value::as_str)
                .is_some_and(|s| !s.trim().is_empty());
        if !has_instructions {
            return Err(AppError::ToolConfig(format!(
                "模型目录条目 `{slug}` 既无 base_instructions 也无 \
                 model_messages.instructions_template；Codex 会拒绝整份目录并启动失败，已中止"
            )));
        }
    }
    Ok(())
}

/// 生成并写入目录文件。走 [`backup_and_write_bytes`]：文件本不存在时会落
/// `.synaroute-created` 标记，还原时按标记整份删除（语义正是「这份文件是我们凭空造的」）。
pub(super) fn write_catalog_at(
    path: &Path,
    models: &[String],
    keys: &[ProviderKey],
) -> AppResult<()> {
    let catalog = build_catalog(models, keys);
    validate(&catalog)?;
    let data = serde_json::to_vec_pretty(&catalog)?;
    super::backup_and_write_bytes(path, &data)
}

/// 把目录接进 `config.toml` 的顶层表：写目录文件、落 `model_catalog_json` 指针、
/// 并决定顶层 `model` 怎么处理。
///
/// 整段逻辑放在本模块而不是 `apply_at` 里，一半是因为 `codex.rs` 的棘轮余量只有 35 行，
/// 另一半是它们本来就是**一件事**：指针与目录文件必须同进同退 —— 写了指针而文件不在，
/// Codex 启动即报错。
///
/// # 🔴 顶层 `model` 的判据：已有值就不覆盖
///
/// 旧实现是「解析出模型名就无条件写顶层 `model`」。目录一上，那行就变成了 bug：
/// `models-manager` 的 `get_default_model` 开头是
/// `if let Some(model) = model { return model }` —— **`config.toml` 的 `model` 一旦有值就赢**，
/// 目录里的 `isDefault` 根本不参与。而 Codex 会把用户在 `/model` 里的选择写回这个键。
/// 于是每次接入（改端口、Key 变动都会触发重写）都把用户刚选的模型冲掉，
/// 表现是「在 Codex 里选了 A，下次打开又回到 B」。cc-switch 修过这条同样的 bug。
///
/// 三分支，缺一不可：
/// - 已有值**且仍在可服务列表里** → 一个字节都不动（那是用户的选择）；
/// - 已有值**但不在列表里**（用户改了 Key 池、旧模型没了）→ 改成列表首个。
///   不改的代价是 Codex 拿一个我们服务不了的名字去请求，被 `resolve_model` 兜到别处或 404；
/// - 没有值 → 写列表首个，让 Codex 启动即有模型可用（原有行为）。
pub(super) fn wire_into(
    table: &mut toml::value::Table,
    models: &[String],
    keys: &[ProviderKey],
    catalog_file: &Path,
) -> AppResult<()> {
    if can_build(models) {
        write_catalog_at(catalog_file, models, keys)?;
        table.insert(
            "model_catalog_json".to_string(),
            toml::Value::String(catalog_file.to_string_lossy().into_owned()),
        );
    } else {
        // 列表空了（无启用 Key，或多 Key 交集为空）。留着指针会让 Codex 读一份陈旧目录，
        // 而那份目录里的模型现在一个都服务不了。只摘我们自己的指针，见 `pointer_is_ours`。
        let ours = table
            .get("model_catalog_json")
            .and_then(|v| v.as_str())
            .is_some_and(pointer_is_ours);
        if ours {
            table.remove("model_catalog_json");
        }
    }

    let existing = table.get("model").and_then(|v| v.as_str()).map(str::trim);
    let keep_existing = existing.is_some_and(|m| !m.is_empty() && models.iter().any(|x| x == m));
    if !keep_existing {
        if let Some(first) = models.first().map(String::as_str).filter(|s| !s.is_empty()) {
            table.insert("model".to_string(), toml::Value::String(first.to_string()));
        }
    }
    Ok(())
}

/// Codex 的 `config.toml` 里当前那个顶层 `model`（**只读回显**，且只在我们接入的 config 上读）。
///
/// # 为什么需要它
///
/// 状态条那个「模型」下拉显示的是 `active_models`，而自 2026-08-30 起 Codex 会自己下发模型名、
/// [`crate::proxy`] 的 `model_choice::pick` 也**优先尊重它**；同时 Codex 会把用户在 `/model`
/// 里的选择写回这个键（见 [`wire_into`] 里「已有值且仍可服务就不覆盖」那一段）。
/// 于是「在 SynaRoute 里选了 b、在 Codex 里改成 a」这个完全正常的序列之后，状态条一直显示 b
/// 而每条请求走的是 a —— 界面用权威口吻说了一个不生效的值。
///
/// 把真实值读回来并排摆出去，用户一眼看出谁在生效。**只读**：这个函数一个字节都不写。
///
/// `None` 的四种情形都不该报错（「用户根本没接入 Codex」是最常见的一种）：
/// 取不到路径 / 文件不存在或读不出 / 不是我们接入的（`model_provider != synaroute`）/ 没有 `model` 键。
pub fn current_config_model() -> Option<String> {
    let text = std::fs::read_to_string(super::config_path().ok()?).ok()?;
    let doc = text.parse::<toml::Value>().ok()?;
    let table = doc.as_table()?;
    if table.get("model_provider").and_then(|v| v.as_str()) != Some(super::MCP_CLIENT_NAME) {
        return None;
    }
    let m = table.get("model").and_then(|v| v.as_str())?.trim();
    (!m.is_empty()).then(|| m.to_string())
}

/// [`current_config_model`] 的 IPC 包装。放在这里而不是 `lib.rs`：那个文件棘轮余量为 0，
/// 而命令函数本来就该跟着它的实现走（同 `usage_commands.rs` 的抽出理由）。
#[tauri::command]
pub fn get_codex_config_model() -> Option<String> {
    current_config_model()
}

/// 用户在应用内（主窗口下拉 / 托盘子菜单）选定 Codex 的对外模型。
///
/// # 为什么不能只写 `active_models`
///
/// 目录上线后 `proxy::model_choice::pick` 会**尊重 Codex 发来的可服务名字**，而接入之后
/// `config.toml` 的 `model` 恒是我们写的可服务名 —— 也就是说只改 `active_models` 的话，
/// 那个托盘子菜单**能点、永远没反应**。这是本仓不容忍的「界面撒谎」。
///
/// 故这里两处都写，且**语义一致**（同托盘代理启停那条「起=写、停=还原」的纪律）：
/// - `config.toml` 的顶层 `model` —— 真正让 Codex 切过去的那一处。它**不是**
///   `model_catalog_json` 那种 "applied on startup only"：`get_default_model` 在每次
///   session 初始化时读它，**新开一个会话就生效**，不必重启 Codex。
/// - `active_models` —— 兜底口径。Codex 还没读到新配置、仍发着旧名字时由它接手，
///   两处指向同一个模型，故不会出现「托盘说切到 A、实际走 B」。
///
/// # 三条刻意行为
///
/// - **没接入过就只写兜底**：`config.toml` 不存在时不凭空造一个（那会让「打开托盘菜单」
///   这个纯查看动作给没接入的用户生成一份 Codex 配置）。
/// - **不是我们接入的 config 一个字节都不动**：判据是 `model_provider == synaroute`。
///   用户可能正用 cc-switch 或官方登录，改他的 `model` 是越界。
/// - **空串（「跟随客户端透传」）不动 `config.toml`**：那个选项的语义就是「不干预」，
///   而删掉 `model` 键会连带清掉用户在 Codex 菜单里做过的选择。
pub(crate) fn select_model(
    store: &crate::store::Store,
    category: crate::model::CategoryType,
    model: &str,
) -> AppResult<()> {
    // 只有 Codex 需要动客户端配置：另两个分类的模型名由客户端按任务自己发，
    // `active_models` 在那边仍是「强制覆盖」语义（见 `proxy::model_choice::pick`）。
    // 这个分派刻意放在这里而不是 `lib.rs` —— 那个文件棘轮余量为 0，且两个入口
    //（下拉命令、托盘子菜单）都该走同一条判据，分两处写必然漂移。
    if category != crate::model::CategoryType::Codex {
        return store.set_active_model(category, model);
    }
    select_model_at(store, model, &super::config_path()?)
}

/// 可测入口：写入指定 `config.toml`（生产走 [`select_model`]，它取真实路径）。
///
/// # 🔴 两处顺序与写法都是被审查改过的，别改回去
///
/// **① 先写 `config.toml`、后写 `active_models`。** 反过来（第一版就是）的失效是：
/// 落盘成功但客户端配置写失败（Codex 正占着文件、权限不足）→ 托盘显示已切到 A、
/// 而 Codex 仍用 B，**正是这个函数存在的目的所要消除的那个现象**。现在的顺序下
/// 那种失败只会让兜底口径陈旧一轮，而兜底仅在「Codex 发来不可服务的名字」时才生效。
///
/// **② 走 [`write_without_locking_snapshot`] 而不是 `backup_and_write_bytes`。**
/// 后者是**首写即锁**的，会把「接入前快照」的时间点提前到「因为切一次模型而第一次
/// 碰这个文件之前」—— 那正是 `write_without_locking_snapshot` 的文档里记的那次真实事故
/// （MCP 注册抢走了 `.bak`，还原时把一份陈旧全量配置整份写回）。
/// 切模型**不是接入**，它不该决定快照时间点；而它也不需要快照：还原会把 `config.toml`
/// 整份从 `.bak` 恢复，`model` 键随之回到接入前的值。
fn select_model_at(
    store: &crate::store::Store,
    model: &str,
    path: &Path,
) -> AppResult<()> {
    let trimmed = model.trim();
    let write_config = |()| -> AppResult<()> {
        if trimmed.is_empty() || !path.is_file() {
            // 空串 =「跟随客户端透传」，语义是「不干预」——删掉 `model` 键会连带清掉用户在
            // Codex 菜单里做过的选择。文件不存在 = 没接入过，不凭空造一份。
            // ⚠️ 后一道与下面的 `ours` 判据**重叠**（空文件读出空表 → 取不到
            // `model_provider` → 同样早退），注入验证时确认过。仍然保留：早退的语义是
            // 「没接入过就不碰」，不该依赖另一道门的副作用来成立。
            return Ok(());
        }
        let text = std::fs::read_to_string(path)?;
        let mut doc = text
            .parse::<toml::Value>()
            .map_err(|e| AppError::ToolConfig(format!("解析 config.toml 失败: {e}")))?;
        let Some(table) = doc.as_table_mut() else {
            return Ok(());
        };
        let ours = table
            .get("model_provider")
            .and_then(|v| v.as_str())
            .is_some_and(|v| v == super::MCP_CLIENT_NAME);
        if !ours {
            return Ok(());
        }
        table.insert("model".to_string(), toml::Value::String(trimmed.to_string()));
        let serialized =
            toml::to_string_pretty(&doc).map_err(|e| AppError::ToolConfig(e.to_string()))?;
        crate::tools::write_without_locking_snapshot(path, serialized.as_bytes())
    };
    write_config(())?;
    store.set_active_model(crate::model::CategoryType::Codex, model)
}

/// 接入成功消息里关于模型目录的那一段。
///
/// 🔴 **「需重启 Codex」这句话必须在**：`model_catalog_json` 官方注明
/// "applied on startup only"，也就是说用户加一条 Key、改一次模型列表之后，Codex 菜单
/// **不会变**。没有这句话的话那个现象长得跟「功能没生效」一模一样,而用户能做的唯一正确
/// 动作(重启一次 Codex)压根没人告诉他。这条消息同时进接入结果提示与事件流(重写客户端
/// 配置那条链路也落它),所以它是这句提示唯一必须落地的地方 —— 不需要动任何前端文件。
pub(super) fn apply_note(models: &[String]) -> String {
    match models.first() {
        Some(m) => format!(
            "，模型目录 {} 条（首个 {m}）—— 目录只在 Codex 启动时加载，\
             之后改了模型列表要重启一次 Codex 才会看到",
            models.len()
        ),
        None => "，当前无可服务模型，故未写模型目录".to_string(),
    }
}

/// 目录文件缺失时的告警文案。
///
/// 文案放在本模块（而不是 `codex.rs` 的 `drift_warning`）是因为那个文件棘轮余量只剩个位数。
///
/// 🔴 **必须说清「Codex 现在完全起不来」**：这一支的实际表现不是「请求走错地方」，
/// 而是 `codex` 一启动就打印 `failed to parse model_catalog_json path … as JSON` 然后退出。
/// 用户此刻最可能的猜测是「SynaRoute 坏了」或「Codex 装坏了」，而真正的一句话解决办法
/// （重新点一次接入）必须出现在文案里 —— 本仓「指错方向的提示比没有提示更糟」那条。
pub(super) fn missing_catalog_warning(path: &str) -> String {
    format!(
        "Codex 的模型目录文件不见了：{path}\n\
         而 ~/.codex/config.toml 里的 model_catalog_json 仍指着它 —— 这种状态下 Codex \
         **一启动就会报 `failed to parse model_catalog_json` 并退出**，不是请求失败，是完全打不开。\n\
         处理：在 SynaRoute 里对 Codex 分类重新点一次「接入」，目录文件会被重新写出来。\n\
         （若你不想再用 SynaRoute 的模型目录，点「还原」即可 —— 指针会随 config.toml 一起交还。）"
    )
}

/// 还原时要处理的两个副文件：`auth.json` 里的占位凭据，以及我们生成的模型目录。
///
/// 合成一个函数是为了让 `tools::restore` 的 Codex 分支保持**一行**（那个文件棘轮余量为 0）。
/// 两件事的失败都不早退、都汇总上报——早退会让用户停在「假 key 摘不掉、目录也没删」
/// 这种两头皆输的状态，那正是原实现在 auth 这一支上已经定下的纪律。
///
/// # 指针为什么不在这里删
///
/// `config.toml` 的还原是**整份从 `.bak` 恢复**（`restore_one`），`model_catalog_json`
/// 那一行随之消失。在这里再去解析 toml 删一遍键，是第二个事实来源，且会与 `.bak`
/// 还原的结果打架（用户接入前本来就有自己的指针时，删掉它就是数据丢失）。
/// 还原的调用点在 `tools::restore`，故可见性放到 `crate::tools` 而不是 `super`。
pub(in crate::tools) fn restore_side_files() -> AppResult<Option<String>> {
    let mut notes: Vec<String> = Vec::new();
    let mut failure: Option<AppError> = None;

    match super::auth_path().and_then(|p| super::disarm_legacy_placeholder_auth(&p)) {
        Ok(Some(note)) => notes.push(note),
        Ok(None) => {}
        Err(e) => failure = Some(e),
    }

    // 目录文件走通用的 `restore_one`：它按 `.synaroute-created` 标记判定「凭空新建 → 整份删除」，
    // 按 `.bak` 判定「原本存在 → 还原原件」。我们的文件名带 synaroute，正常情况下走前者。
    match catalog_path().and_then(|p| crate::tools::restore_one(&p).map(|done| (p, done))) {
        Ok((p, true)) => notes.push(format!("已移除模型目录 {}", p.display())),
        Ok((_, false)) => {}
        Err(e) if failure.is_none() => failure = Some(e),
        // 两个都失败时只上报第一个（auth 那条）：它是更要紧的那一件（假凭据还武装着），
        // 而目录文件残留是无害的（指针已随 config.toml 的 .bak 一起消失）。
        // ⚠️ 这与上面注释里「都汇总上报」不是一回事，审查时特意分清：**不早退**是真的
        //（两件事都会尝试），**都上报**只做到「第一个错误 + 已完成项」。
        Err(_) => {}
    }

    match failure {
        Some(e) if notes.is_empty() => Err(e),
        Some(e) => Err(AppError::ToolConfig(format!(
            "已处理 {}，但另一项失败：{e}",
            notes.join("、")
        ))),
        None if notes.is_empty() => Ok(None),
        None => Ok(Some(notes.join("；"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CategoryType, ModelInfo};

    fn key(protocol: Protocol, models: &[(&str, Option<u32>)]) -> ProviderKey {
        ProviderKey {
            id: "k".into(),
            category_id: CategoryType::Codex,
            protocol,
            models: models
                .iter()
                .map(|(name, ctx)| ModelInfo {
                    real_name: (*name).to_string(),
                    source: "manual".into(),
                    fetched_at: None,
                    context_window: *ctx,
                    max_output_tokens: None,
                })
                .collect(),
            ..Default::default()
        }
    }

    fn entries(catalog: &Value) -> &Vec<Value> {
        catalog.get("models").unwrap().as_array().unwrap()
    }

    /// 每条用例一个独立目录：`select_model_at` 那两条会真起 `Store`（写 config.json）。
    ///
    /// ⚠️ **必须带进程内自增序号**，不能只靠 `纳秒 + pid`：本机实测
    /// `timestamp_nanos` 的量化粒度只有 **100ns**，并发跑的两条用例会撞到同一个目录、
    /// 互相删对方的文件。CLAUDE.md 里 `ccswitch::db_copy_path` 那条（8 线程 16 万采样
    /// 88% 撞车）就是这么红过的，本文件第一版也偶发红过一次。
    fn temp_store_dir(tag: &str) -> PathBuf {
        static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!(
            "sr_cat_{tag}_{}_{n}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// 取第一条条目的**拥有副本**：`build_catalog` 的返回值是临时量，直接借它的内部
    /// 过不了 borrow check。
    fn one(models: &[&str], keys: &[ProviderKey]) -> Value {
        let owned: Vec<String> = models.iter().map(|s| (*s).to_string()).collect();
        build_catalog(&owned, keys)["models"][0].clone()
    }

    /// 漏掉任一必填键的后果是 **Codex 启动即报错、完全起不来**，而报错文本只提第一个
    /// 出错的模型、不会告诉你还差哪些。故逐键遍历。
    #[test]
    fn every_entry_carries_all_required_keys() {
        let ks = [key(Protocol::Anthropic, &[("m", None)])];
        let cat = build_catalog(&["a".into(), "b".into()], &ks);
        for e in entries(&cat) {
            for k in REQUIRED_KEYS {
                assert!(
                    e.get(*k).is_some(),
                    "条目缺少必填键 `{k}`：{}",
                    serde_json::to_string(e).unwrap()
                );
            }
            let tpl = e
                .get("model_messages")
                .and_then(|m| m.get("instructions_template"))
                .and_then(Value::as_str)
                .unwrap_or("");
            assert_eq!(
                tpl.len(),
                20903,
                "🔴 内嵌的官方 prompt.md 必须是**逐字节副本** —— THIRD_PARTY_NOTICES.md 里\
                 写着这个可核对的字节数，许可声明靠它成立。\n\
                 最常见的走样不是有人编辑了它，而是**行尾**：`core.autocrlf=true` 下\
                 一次新克隆就会把它转成 CRLF（21178 字节），而 `include_str!` 嵌进二进制的\
                 也就不再是原文。`.gitattributes` 里那条 `-text` 是配套防线，别删。\n\
                 真要换一份新版官方提示词：同步改这个数与 THIRD_PARTY_NOTICES.md 第 9 行。\n\
                 当前 {} 字节",
                tpl.len()
            );
            // 🔴 存在性判据挡不住「值被改错」，而这两个键的注释里恰好写着改错的代价 ——
            // 实测注入（删掉 include_apps_usage_instructions 那行 + 把 mode 改成 tokens）
            // 全套 922 条**照样全绿**。判据存在 ≠ 判对了维度。
            assert_eq!(
                e["include_apps_usage_instructions"],
                json!(false),
                "必须显式 false：它在 ModelInfo 里默认 true，省略/写错就悄悄开了一个 fallback 下关着的开关"
            );
            assert_eq!(
                e["truncation_policy"],
                json!({ "mode": "bytes", "limit": 10_000 }),
                "fallback 用 bytes(10_000)，官方 GPT 条目用 tokens —— 抄错这一个键会改变工具输出的截断行为"
            );
        }
    }

    /// 🔴 `ModelPreset::filter_by_auth` 是 `chatgpt_mode || supported_in_api`，而我们走
    /// `experimental_bearer_token` → `chatgpt_mode = false`。给 false 的表现是模型
    /// **静默从选择器里消失**（同 Claude 桌面端那个 `sD()` 硬过滤的方向）。
    /// `visibility` 同理：`show_in_picker = visibility == List`。
    #[test]
    fn the_two_keys_that_decide_whether_the_picker_shows_it_at_all() {
        let ks = [key(Protocol::Anthropic, &[("m", None)])];
        {
            let e = one(&["claude-opus-4-8"], &ks);
            assert_eq!(e["supported_in_api"], json!(true), "false = 从菜单里消失");
            assert_eq!(e["visibility"], json!("list"), "非 list = show_in_picker 为假");
            // null = ToolMode::Direct（顶层 tools），docs/13 实测 opus 走通的那条形态。
            assert_eq!(e["tool_mode"], Value::Null);
        }
    }

    /// 🔴 声明一个 `convert.rs` 不认识的档位 = 界面撒谎，而且方向最坏：
    /// `effort_to_thinking_budget` 对未知档位走 `_ => return None` = **不开扩展思考**，
    /// 于是「用户选了最高档 → 反而完全不思考」。
    ///
    /// 判据落在 `convert.rs` 的**生产段且剥掉注释**（`production_code_only`）——
    /// 本仓已经三次栽在「注释里的字面量满足了断言」上（`data-dir-env-name-must-match`、
    /// `userPrefsParity`、`only_v6_must_be_set_explicitly`）。
    #[test]
    fn declared_levels_are_all_understood_by_the_converter() {
        let src = std::fs::read_to_string("src/upstream/convert.rs").unwrap();
        let prod = crate::proxy::custom_headers::production_code_only(&src);
        assert!(
            prod.contains("fn effort_to_thinking_budget"),
            "被扫的函数改名了，这条判据会变成空洞的绿"
        );
        for (level, _) in EFFORT_LEVELS {
            assert!(
                prod.contains(&format!("\"{level}\" =>")),
                "声明了档位 `{level}`，但 convert.rs 的生产代码里没有对应的 match 分支 —— \
                 它会走 `_ => return None`（不开思考），也就是用户切档位毫无效果"
            );
        }
        for banned in ["max", "ultra", "persistent"] {
            assert!(
                !EFFORT_LEVELS.iter().any(|(l, _)| *l == banned),
                "`{banned}` 在转换器里落到 `_ => None`（不开思考）。要声明它必须先在 \
                 effort_to_thinking_budget 里给出真实预算"
            );
        }
    }

    /// 🔴 口径必须是**交集**：一条 Chat 上游的备用 Key 就足以让整组不声明档位。
    ///
    /// 用超集（只看主 Key）的失效是：主 Key 走 Anthropic 所以声明了四档，
    /// 故障转移落到那条 Chat 上游的备用 Key 上，档位当场静默失效。同
    /// `service::models_for_apply` 注释里记的「超集口径 → 桌面端列出备用 Key 服务不了的模型」。
    #[test]
    fn one_chat_key_disables_effort_levels_for_the_whole_pool() {
        let anth = key(Protocol::Anthropic, &[("m", None)]);
        let chat = key(Protocol::OpenaiChat, &[("m", None)]);
        let resp = key(Protocol::OpenaiResponses, &[("m", None)]);

        assert_eq!(effort_levels_for(std::slice::from_ref(&anth)).len(), EFFORT_LEVELS.len());
        assert_eq!(effort_levels_for(std::slice::from_ref(&resp)).len(), EFFORT_LEVELS.len());
        assert_eq!(
            effort_levels_for(&[anth.clone(), resp.clone()]).len(),
            EFFORT_LEVELS.len(),
            "两种能生效的协议混合仍应声明"
        );
        assert!(
            effort_levels_for(&[anth, chat.clone()]).is_empty(),
            "主 Key 能生效但备用 Key 是 Chat → 必须不声明（这就是交集口径）"
        );
        assert!(effort_levels_for(std::slice::from_ref(&chat)).is_empty());
        assert!(effort_levels_for(&[]).is_empty(), "没有 Key 就没有依据");
    }

    /// 无档位时 `default_reasoning_level` 必须是 `null`，不能留一个指向空列表的默认值。
    #[test]
    fn no_levels_means_no_default_level() {
        let chat = [key(Protocol::OpenaiChat, &[("m", None)])];
        let e = one(&["glm-4.6"], &chat);
        assert_eq!(e["supported_reasoning_levels"], json!([]));
        assert_eq!(e["default_reasoning_level"], Value::Null);

        let anth = [key(Protocol::Anthropic, &[("m", None)])];
        let e = one(&["claude-opus-4-8"], &anth);
        assert_eq!(e["default_reasoning_level"], json!(DEFAULT_EFFORT));
    }

    /// 官方内置基底（`models-manager/models.json`，10 条）的 `priority` 占 1~43（实测）。
    /// 从 43 以内起会抢掉用户原有的默认模型。
    #[test]
    fn priority_starts_after_the_official_range() {
        // 官方内置基底占 1~43（实测）。这条是编译期常量断言：数值改小直接编译不过。
        const _: () = assert!(PRIORITY_BASE > 43);
        let ks = [key(Protocol::Anthropic, &[("m", None)])];
        let cat = build_catalog(&["a".into(), "b".into(), "c".into()], &ks);
        let got: Vec<i64> = entries(&cat)
            .iter()
            .map(|e| e["priority"].as_i64().unwrap())
            .collect();
        assert_eq!(got, vec![50, 51, 52], "顺序必须跟随我们的模型列表顺序");
    }

    /// 上下文窗口取**所有 Key 的最小值**：报一个只有主 Key 撑得住的窗口，等于让 Codex
    /// 把超长上下文送去一条撑不住的上游（400）。全未知时落到 Codex 自己的兜底值。
    #[test]
    fn context_window_takes_the_minimum_across_keys() {
        let big = key(Protocol::Anthropic, &[("m", Some(1_000_000))]);
        let small = key(Protocol::Anthropic, &[("m", Some(200_000))]);
        let unknown = key(Protocol::Anthropic, &[("m", None)]);

        let e = one(&["m"], &[big.clone(), small]);
        assert_eq!(e["context_window"], json!(200_000), "必须取小的那个");
        assert_eq!(e["max_context_window"], json!(200_000));

        let e = one(&["m"], &[unknown]);
        assert_eq!(
            e["context_window"],
            json!(FALLBACK_CONTEXT_WINDOW),
            "全未知 → 与 Codex 自己 fallback 的 272000 一致（不改变现有行为）"
        );
        // 已知值仍要生效，别被兜底值盖掉。
        let e = one(&["m"], &[big]);
        assert_eq!(e["context_window"], json!(1_000_000));
    }

    /// 🔴 空模型列表既不写目录也不写指针。
    ///
    /// `{"models":[]}` 会让 Codex 对着空列表挑默认模型（行为未验证），而用户在菜单里
    /// 一个都选不到 —— 比回落到官方内置条目糟得多。真实成因：该分类无启用 Key，
    /// 或多 Key 的可服务模型交集为空。
    #[test]
    fn empty_models_writes_neither_catalog_nor_pointer() {
        assert!(!can_build(&[]));
        assert!(can_build(&["a".to_string()]));

        let d = std::env::temp_dir().join(format!("sr_cat_empty_{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let file = d.join(CATALOG_FILE);
        let mut table = toml::value::Table::new();
        wire_into(&mut table, &[], &[], &file).unwrap();
        assert!(!file.exists(), "空列表不该写出目录文件");
        assert!(!table.contains_key("model_catalog_json"), "更不该写指针");
        assert!(!table.contains_key("model"), "没有模型可写时也不该造一个 model 键");

        // 已有的**我们的**指针要被摘掉（它会指向一份现在一个模型都服务不了的目录）；
        // 用户自己的指针一个字节都不许动。
        let mut mine = toml::value::Table::new();
        mine.insert(
            "model_catalog_json".into(),
            toml::Value::String(file.to_string_lossy().into_owned()),
        );
        wire_into(&mut mine, &[], &[], &file).unwrap();
        assert!(!mine.contains_key("model_catalog_json"));

        let mut theirs = toml::value::Table::new();
        theirs.insert(
            "model_catalog_json".into(),
            toml::Value::String("/home/u/.codex/my-catalog.json".into()),
        );
        wire_into(&mut theirs, &[], &[], &file).unwrap();
        assert_eq!(
            theirs["model_catalog_json"].as_str(),
            Some("/home/u/.codex/my-catalog.json"),
            "用户自己的目录路径绝不覆盖（cc-switch #6087 修的正是这条）"
        );

        std::fs::remove_dir_all(&d).ok();
    }

    /// 认领指针的唯一判据是文件名。目录部分刻意不比（用户可能搬过 `CODEX_HOME`）。
    ///
    /// 🔴 **反斜杠那一条必须 `#[cfg(windows)]`**：`Path::file_name()` 按**平台**语义切分
    /// —— Windows 认 `\` 也认 `/`，而 Unix **只认 `/`**。第一版把
    /// `D:\somewhere\else\<CATALOG_FILE>` 无条件断言成「是我们的」，于是它在 Windows 上绿、
    /// 在 macOS/Linux 上把整串当成一个文件名 → 判否 → 红。**这是 v0.1.43 那次
    /// `macOS check` 唯一的失败**（949 passed / 1 failed），而 Windows 侧的 Gates 与
    /// Release 构建都不跑非 Windows 的测试 —— `macos-check` 是唯一能抓到它的地方。
    ///
    /// **刻意不把生产代码改成「Unix 上也把 `\` 当分隔符」**：反斜杠在 Unix 上是**合法的
    /// 文件名字符**，那么一个真名叫 `weird\cc-switch-model-catalog.json` 的用户文件就会被
    /// 我们认领并覆盖 —— 方向恰好是这个判据要防的那种（cc-switch #6087 抢用户指针）。
    /// 平台原生语义是对的，错的是那条夹具。
    #[test]
    fn pointer_is_ours_only_matches_our_own_file_name() {
        assert!(pointer_is_ours(&format!("/home/u/.codex/{CATALOG_FILE}")));
        // 裸文件名：与平台无关，任何一方改坏切分逻辑都会红。
        assert!(pointer_is_ours(CATALOG_FILE));
        #[cfg(windows)]
        assert!(pointer_is_ours(&format!("D:\\somewhere\\else\\{CATALOG_FILE}")));
        #[cfg(windows)]
        assert!(pointer_is_ours(&format!("D:/forward/slashes/{CATALOG_FILE}")));
        assert!(!pointer_is_ours("/home/u/.codex/my-catalog.json"));
        assert!(!pointer_is_ours("/home/u/.codex/cc-switch-model-catalog.json"));
        assert!(!pointer_is_ours(""));
    }

    /// 🔴 顶层 `model` 已有值且仍可服务 → 一个字节都不动。
    ///
    /// `get_default_model` 开头就是 `if let Some(model) = model { return model }` ——
    /// `config.toml` 的 `model` 一旦有值就赢，目录里的 `isDefault` 根本不参与。而 Codex 会把
    /// 用户在 `/model` 里的选择写回这个键。无条件覆盖的表现是「在 Codex 里选了 A，
    /// 下次打开又回到 B」，且每次改端口 / Key 变动触发的重写都会再冲一次。
    #[test]
    fn wire_into_keeps_a_still_serviceable_model_choice() {
        let d = std::env::temp_dir().join(format!("sr_cat_model_{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let file = d.join(CATALOG_FILE);
        let ks = [key(Protocol::Anthropic, &[("m", None)])];
        let models = vec!["first".to_string(), "user-picked".to_string()];

        // ① 用户已选第二个 → 保留
        let mut t = toml::value::Table::new();
        t.insert("model".into(), toml::Value::String("user-picked".into()));
        wire_into(&mut t, &models, &ks, &file).unwrap();
        assert_eq!(t["model"].as_str(), Some("user-picked"), "用户的选择不许被冲掉");

        // ② 选的模型已不在可服务列表里（改了 Key 池）→ 换成首个，否则 Codex 会拿一个
        //    我们服务不了的名字去请求。
        let mut t = toml::value::Table::new();
        t.insert("model".into(), toml::Value::String("gone-away".into()));
        wire_into(&mut t, &models, &ks, &file).unwrap();
        assert_eq!(t["model"].as_str(), Some("first"));

        // ③ 没有 model 键 → 写首个（原有行为：启动即有模型可用）
        let mut t = toml::value::Table::new();
        wire_into(&mut t, &models, &ks, &file).unwrap();
        assert_eq!(t["model"].as_str(), Some("first"));

        // 指针与文件同时到位（写了指针而文件不在 = Codex 启动即报错）
        assert_eq!(
            t["model_catalog_json"].as_str(),
            Some(file.to_string_lossy().as_ref())
        );
        assert!(file.is_file(), "指针写了，文件必须同时在");

        std::fs::remove_dir_all(&d).ok();
    }

    /// 自检本身要能抓住缺键 —— 它是「Codex 启动即报错」这个后果的唯一拦截点。
    #[test]
    fn validate_rejects_a_missing_required_key_and_empty_instructions() {
        let ks = [key(Protocol::Anthropic, &[("m", None)])];
        let good = build_catalog(&["m".into()], &ks);
        assert!(validate(&good).is_ok());

        for k in REQUIRED_KEYS {
            let mut bad = good.clone();
            bad["models"][0].as_object_mut().unwrap().remove(*k);
            let err = validate(&bad).expect_err("删掉必填键必须被拦下");
            assert!(err.to_string().contains(k), "报错要点名缺哪个键：{err}");
        }

        let mut blank = good.clone();
        blank["models"][0]["model_messages"]["instructions_template"] = json!("   ");
        let err = validate(&blank).expect_err("空白提示词等于没有，Codex 会拒绝整份目录");
        assert!(err.to_string().contains("instructions_template"));

        // `base_instructions` 是另一条合法路径，给了它就不该被拦。
        let mut alt = good.clone();
        alt["models"][0]
            .as_object_mut()
            .unwrap()
            .remove("model_messages");
        alt["models"][0]["base_instructions"] = json!("non empty");
        assert!(validate(&alt).is_ok(), "两条路径任一满足即可");
    }

    /// 还原要把我们凭空造出来的目录文件删掉（`restore_one` 按 `.synaroute-created` 标记判定）。
    #[test]
    fn write_then_restore_leaves_no_catalog_behind() {
        let d = std::env::temp_dir().join(format!("sr_cat_restore_{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let file = d.join(CATALOG_FILE);
        let ks = [key(Protocol::Anthropic, &[("m", None)])];

        write_catalog_at(&file, &["m".into()], &ks).unwrap();
        assert!(file.is_file());
        assert!(
            crate::tools::restore_one(&file).unwrap(),
            "凭空新建的文件必须被 restore_one 认领"
        );
        assert!(!file.exists(), "还原后不该留下我们造的目录文件");

        std::fs::remove_dir_all(&d).ok();
    }

    /// 🔴 接线判据：上面那条只证明 `restore_one` 能删，**没有证明还原路径会去调它**。
    ///
    /// 实测注入过两次，两次都全绿（922 passed / clippy 干净）：
    /// ① 把 `restore_side_files` 里那句 `catalog_path().and_then(|p| restore_one(&p)…)`
    ///    换成 `catalog_path().map(|p| (p, false))`（= 还原时压根不删）；
    /// ② 把 `tools.rs` 那行整行退回只处理 auth 的旧写法 —— 这一支只被 clippy 的
    ///    `dead_code` 挡住，而那道门对「函数还在被调、里面那半被改坏」完全无效。
    ///
    /// 残留的表现是**惰性**的：指针随 config.toml 的 `.bak` 一起消失，所以文件不报错、
    /// 也永不自愈 —— 用户的 `.codex` 目录里从此多一份约 190 KB 的死文件，
    /// 而「凭空造的文件整份删除」正是本仓 restore 的契约。
    #[test]
    fn the_restore_path_must_actually_delete_the_catalog() {
        let tools = std::fs::read_to_string("src/tools.rs").unwrap();
        let tools_prod = crate::proxy::custom_headers::production_code_only(&tools);
        assert!(
            tools_prod.contains("codex::codex_catalog::restore_side_files()"),
            "Codex 的还原分支必须调 restore_side_files —— 否则目录文件永久残留"
        );
        let me = std::fs::read_to_string("src/tools/codex_catalog.rs").unwrap();
        let prod = crate::proxy::custom_headers::production_code_only(&me);
        let at = prod
            .find("pub(in crate::tools) fn restore_side_files")
            .expect("找不到 restore_side_files —— 判据失去目标，先修判据");
        let body = &prod[at..];
        let end = body.find("\n}").unwrap_or(body.len());
        assert!(
            body[..end].contains("catalog_path()") && body[..end].contains("restore_one("),
            "restore_side_files 里必须真的对 catalog_path() 调一次 restore_one —— \
             它是「凭空造的文件整份删除」这条契约在 Codex 侧的唯一落点"
        );
    }

    /// 🔴 应用内选模型必须**真的动 `config.toml` 的 `model`**，不能只写 `active_models`。
    ///
    /// 只写兜底的表现是那个托盘子菜单**能点、永远没反应** —— 因为 `pick` 会尊重 Codex
    /// 发来的可服务名字，而接入后 `config.toml` 的 `model` 恒是我们写的可服务名。
    #[test]
    fn selecting_a_model_writes_it_into_the_codex_config() {
        let d = temp_store_dir("sel");
        let store =
            crate::store::Store::new_at(d.join("config.json"), d.join("secrets.enc")).unwrap();
        let cfg = d.join("config.toml");
        let cat = d.join(CATALOG_FILE);
        super::super::apply_at(
            &cfg,
            "http://127.0.0.1:47101",
            &["a".to_string(), "b".to_string()],
            &[],
            &cat,
        )
        .unwrap();

        select_model_at(&store, "b", &cfg).unwrap();
        let doc = std::fs::read_to_string(&cfg)
            .unwrap()
            .parse::<toml::Value>()
            .unwrap();
        assert_eq!(doc["model"].as_str(), Some("b"), "必须写进 config.toml");
        assert_eq!(
            store
                .get_settings()
                .active_models
                .get(&crate::model::CategoryType::Codex)
                .map(String::as_str),
            Some("b"),
            "兜底口径要与它一致，否则会出现「托盘说切到 A、实际走 B」"
        );

        // 空串 = 「跟随客户端透传」：清兜底，但**不动** config.toml
        //（删掉 model 键会连带清掉用户在 Codex 菜单里做过的选择）。
        select_model_at(&store, "", &cfg).unwrap();
        let doc = std::fs::read_to_string(&cfg)
            .unwrap()
            .parse::<toml::Value>()
            .unwrap();
        assert_eq!(doc["model"].as_str(), Some("b"), "透传不该改 Codex 的配置");
        assert!(!store
            .get_settings()
            .active_models
            .contains_key(&crate::model::CategoryType::Codex));

        std::fs::remove_dir_all(&d).ok();
    }

    /// 🔴 切模型**不许抢走「接入前快照」**（`write_without_locking_snapshot` 的文档里
    /// 记着这条纪律换来的那次真实事故：MCP 注册用 `backup_and_write_bytes` 把 `.bak`
    /// 锁在了接入之前，还原时把一份陈旧全量配置整份写回）。
    ///
    /// 复现路径：接入过又还原过（`.bak` 已删）→ config.toml 里又出现了我们的 provider
    /// （cc-switch 存的档 / 用户手改）→ 切一次模型。若这一步抓了快照，下次真正接入时
    /// `.bak` 已存在、首写即锁不覆盖 → 还原会回到这个中间状态而不是接入前。
    #[test]
    fn selecting_a_model_never_steals_the_backup_snapshot() {
        let d = temp_store_dir("sel_bak");
        let store =
            crate::store::Store::new_at(d.join("config.json"), d.join("secrets.enc")).unwrap();
        let cfg = d.join("config.toml");
        std::fs::write(
            &cfg,
            "model_provider = \"synaroute\"\nmodel = \"old\"\n[model_providers.synaroute]\n",
        )
        .unwrap();

        select_model_at(&store, "new", &cfg).unwrap();
        let doc = std::fs::read_to_string(&cfg)
            .unwrap()
            .parse::<toml::Value>()
            .unwrap();
        assert_eq!(doc["model"].as_str(), Some("new"), "该写的还是要写");
        assert!(
            !crate::tools::backup_path_for(&cfg).exists(),
            "切模型不是接入，不许抓 .bak —— 否则会把「接入前快照」的时间点提前到这里"
        );
        assert!(
            !crate::tools::created_marker_path_for(&cfg).exists(),
            "也不该落「凭空新建」标记"
        );

        std::fs::remove_dir_all(&d).ok();
    }

    /// 🔴 写客户端配置失败时**不许**改兜底口径。
    ///
    /// 反了的话（第一版就是先写 `active_models`）失效是：托盘显示已切到 A、而 Codex 仍用 B
    /// —— 正是这个函数存在的目的所要消除的那个现象。而 Codex 正占着 config.toml 或权限不足
    /// 时写失败是真实会发生的。
    #[test]
    fn a_failed_config_write_leaves_the_fallback_untouched() {
        let d = temp_store_dir("sel_fail");
        let store =
            crate::store::Store::new_at(d.join("config.json"), d.join("secrets.enc")).unwrap();
        let cfg = d.join("config.toml");
        std::fs::write(&cfg, "this is not [[[ valid = = toml").unwrap();

        assert!(
            select_model_at(&store, "x", &cfg).is_err(),
            "坏 toml 必须报错，不能静默跳过"
        );
        assert!(
            !store
                .get_settings()
                .active_models
                .contains_key(&crate::model::CategoryType::Codex),
            "客户端配置没写成，兜底口径就不该动 —— 否则两处不一致且不会自愈"
        );

        std::fs::remove_dir_all(&d).ok();
    }

    /// 不是我们接入的 `config.toml` 一个字节都不动（用户可能正用 cc-switch 或官方登录），
    #[test]
    fn selecting_a_model_never_touches_someone_elses_config() {
        let d = temp_store_dir("sel_other");
        let store =
            crate::store::Store::new_at(d.join("config.json"), d.join("secrets.enc")).unwrap();

        // ① 别人的 config
        let cfg = d.join("config.toml");
        let theirs = "model_provider = \"custom\"\nmodel = \"his-model\"\n";
        std::fs::write(&cfg, theirs).unwrap();
        select_model_at(&store, "ours", &cfg).unwrap();
        assert_eq!(
            std::fs::read_to_string(&cfg).unwrap(),
            theirs,
            "别人的 config 必须逐字节不变"
        );

        // ② 压根不存在 → 不造
        let absent = d.join("nope").join("config.toml");
        select_model_at(&store, "ours", &absent).unwrap();
        assert!(!absent.exists(), "没接入过就不该凭空造一份 Codex 配置");

        // 两种情形下兜底口径仍然落了盘（那是应用内的选择，与客户端配置无关）。
        assert_eq!(
            store
                .get_settings()
                .active_models
                .get(&crate::model::CategoryType::Codex)
                .map(String::as_str),
            Some("ours")
        );

        std::fs::remove_dir_all(&d).ok();
    }

    /// 🔴 第 8 次盯同一类接线盲区：`lib.rs` 的两个「应用内选模型」入口
    /// （`set_active_model` 命令 + 托盘 `model::` 子菜单）必须走 [`select_model`]。
    ///
    /// 上面两条用例直接调 `select_model_at`，所以**把 lib.rs 改回
    /// `state.store.set_active_model(...)` 它们照样全绿** —— 而那就是「托盘能点、没反应」
    /// 这个缺陷本身。
    #[test]
    fn both_in_app_entry_points_must_go_through_select_model() {
        let src = std::fs::read_to_string("src/lib.rs").unwrap();
        let prod = crate::proxy::custom_headers::production_code_only(&src);
        let direct: Vec<&str> = prod
            .lines()
            // 带点的才是**调用**（`state.store.set_active_model(...)`）。第一版漏了这个点，
            // 命中的是 lib.rs 里那个同名 tauri 命令的定义行 `async fn set_active_model(`
            // —— 一个假阳性，同本仓「判据说别这么写就只能看代码」那一类。
            .filter(|l| l.contains(".set_active_model("))
            .map(str::trim)
            .collect();
        assert!(
            direct.is_empty(),
            "lib.rs 不许直接调 store.set_active_model —— 那会绕过写 config.toml 的那一半：{direct:?}"
        );
        assert_eq!(
            prod.matches("select_model(").count(),
            2,
            "两个入口各一处：set_active_model 命令与托盘 model:: 子菜单"
        );
    }

    /// 接入消息必须带「要重启 Codex」那句话。
    ///
    /// `model_catalog_json` 是 "applied on startup only"，用户加条 Key 之后 Codex 菜单不变，
    /// 而那个现象长得跟「功能没生效」一模一样。这句提示是唯一会到达用户眼前的文本。
    #[test]
    fn the_apply_note_tells_the_user_to_restart_codex() {
        let note = apply_note(&["a".to_string(), "b".to_string()]);
        assert!(note.contains('2'), "要报出条数：{note}");
        assert!(note.contains("重启"), "必须说清要重启 Codex：{note}");
        let empty = apply_note(&[]);
        assert!(
            !empty.contains("重启") && empty.contains("无可服务模型"),
            "没写目录时不该让人去重启：{empty}"
        );
    }

    /// 🔴 **对真实 `~/.codex/config.toml` 走一次完整接入**，用于 docs/14 的 C-16/17/18。
    ///
    /// 这是三条真机验证里我唯一能做完的那一半：把目录写到位。剩下的
    /// 「完全退出并重开 Codex → 看菜单」只能由使用者做 —— 目录是
    /// "applied on startup only"，而且我的进程活在 MSIX 包身份里。
    ///
    /// 跑法（`--ignored` 之外还要显式给两个环境变量，防误触）：
    /// ```text
    /// SYNAROUTE_APPLY_REAL=1 \
    /// SYNAROUTE_APPLY_MODELS=gpt-5.3-codex-spark,gpt-5.4-mini,… \
    /// SYNAROUTE_APPLY_ENDPOINT=http://127.0.0.1:47101 \
    ///   cargo test --lib apply_to_the_real_codex_home -- --ignored --nocapture
    /// ```
    ///
    /// ⚠️ **它会真的改你的 `~/.codex/config.toml`**（加 `model_catalog_json`、
    /// 可能改顶层 `model`）。跑之前自己备份 —— 本函数刻意不替你备份：
    /// `apply_at` 内部的 `.bak` 是「接入前快照」，语义是产品自己的，不该被探针借用。
    #[test]
    #[ignore = "会改写真实的 ~/.codex/config.toml，见函数文档"]
    fn apply_to_the_real_codex_home() {
        assert_eq!(
            std::env::var("SYNAROUTE_APPLY_REAL").as_deref(),
            Ok("1"),
            "需要 SYNAROUTE_APPLY_REAL=1 才会动真实配置"
        );
        let models: Vec<String> = std::env::var("SYNAROUTE_APPLY_MODELS")
            .expect("需要 SYNAROUTE_APPLY_MODELS（逗号分隔的可服务对外模型名）")
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        assert!(!models.is_empty(), "模型列表不能为空");
        let endpoint = std::env::var("SYNAROUTE_APPLY_ENDPOINT")
            .unwrap_or_else(|_| "http://127.0.0.1:47101".to_string());

        let cfg = super::super::config_path().unwrap();
        let cat = catalog_path().unwrap();
        // 档位由 Key 的 protocol 推导（见 `effort_levels_for`），所以必须造一条真实协议的 Key
        // —— 传空切片会让它保守地不声明任何档位，那样 C-17 就验不成了（第一次跑就踩了）。
        let proto = match std::env::var("SYNAROUTE_APPLY_PROTOCOL").as_deref() {
            Ok("anthropic") => Protocol::Anthropic,
            Ok("openai_chat") => Protocol::OpenaiChat,
            _ => Protocol::OpenaiResponses,
        };
        let pairs: Vec<(&str, Option<u32>)> = models.iter().map(|m| (m.as_str(), None)).collect();
        let keys = [key(proto, &pairs)];
        // 走完整生产路径（含 wire_into 对顶层 model 的三分支决策）。
        let msg = super::super::apply_at(&cfg, &endpoint, &models, &keys, &cat).unwrap();

        let doc = std::fs::read_to_string(&cfg)
            .unwrap()
            .parse::<toml::Value>()
            .unwrap();
        assert_eq!(
            doc["model_catalog_json"].as_str(),
            Some(cat.to_string_lossy().as_ref())
        );
        assert!(cat.is_file(), "目录文件必须真的写出来");
        let parsed: Value =
            serde_json::from_str(&std::fs::read_to_string(&cat).unwrap()).unwrap();
        let slugs: Vec<&str> = parsed["models"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|m| m["slug"].as_str())
            .collect();
        assert_eq!(slugs, models, "目录里的条目必须与传入的列表逐条一致");

        eprintln!("\n✅ 已写入 {}", cat.display());
        eprintln!("   {msg}");
        eprintln!("   config.toml 顶层 model = {:?}", doc["model"].as_str());
        eprintln!("   目录 {} 条：{slugs:?}", slugs.len());
        eprintln!("\n👉 现在请**完全退出并重开 Codex**，然后看模型菜单。");
    }

    /// 端到端：把**生产代码生成的**目录喂给真实的 codex 二进制，确认它真的被接受。 上面那些用例只证明「我们按自己理解的 schema 写了」。这一条证明「Codex 真的读得懂」——
    /// 而这条链路上最贵的失效（漏一个必填键 / 猜错一个字段形态）恰恰只在真二进制里才暴露。
    ///
    /// 手动跑：
    /// ```text
    /// SYNAROUTE_CODEX_PROBE=/path/to/codex.exe \
    ///   cargo test --lib catalog_is_accepted_by_the_real_codex -- --ignored --nocapture
    /// ```
    /// ⚠️ Codex Desktop 包内那份在 `WindowsApps` 下带 ACL、**不能直接执行**，要先复制出来。
    /// 找不到探针就 panic 而不是跳过：一个能静默跳过的端到端判据等于没有
    /// （本仓的 `bind_v6` 用例第一版就是这么假绿的）。
    #[test]
    #[ignore = "需要真实 codex 二进制，见函数文档"]
    fn catalog_is_accepted_by_the_real_codex_binary() {
        let probe = std::env::var("SYNAROUTE_CODEX_PROBE")
            .expect("请用 SYNAROUTE_CODEX_PROBE 指向可执行的 codex 二进制");
        let d = std::env::temp_dir().join(format!("sr_cat_e2e_{}", std::process::id()));
        let home = d.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let file = d.join(CATALOG_FILE);

        let ks = [key(Protocol::Anthropic, &[("real", Some(200_000))])];
        let models = vec!["claude-opus-4-8".to_string(), "glm-4.6".to_string()];
        write_catalog_at(&file, &models, &ks).unwrap();
        std::fs::write(
            home.join("config.toml"),
            format!(
                "model_catalog_json = \"{}\"\n",
                file.to_string_lossy().replace('\\', "/")
            ),
        )
        .unwrap();

        let out = std::process::Command::new(&probe)
            .args(["debug", "models"])
            .env("CODEX_HOME", &home)
            .output()
            .expect("跑 codex debug models 失败");
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            out.status.success(),
            "codex 拒绝了我们的目录：\nstderr: {stderr}\nstdout: {stdout}"
        );
        let parsed: Value = serde_json::from_str(stdout.trim())
            .unwrap_or_else(|e| panic!("debug models 的输出不是 JSON：{e}\n{stdout}"));
        let slugs: Vec<&str> = parsed["models"]
            .as_array()
            .expect("输出缺少 models 数组")
            .iter()
            .filter_map(|m| m["slug"].as_str())
            .collect();
        assert_eq!(
            slugs, models,
            "Codex 解析出的模型清单必须与我们写的逐条一致（顺序也是，priority 决定）"
        );
        eprintln!("✅ 真实 codex 接受了目录，解析出 {} 条：{slugs:?}", slugs.len());

        std::fs::remove_dir_all(&d).ok();
    }
}
