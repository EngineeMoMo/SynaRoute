//! 大脑聚合的**输出 token 预算**：按协议决定「发不发上限」，Anthropic 再按上下文窗口算。
//!
//! ## 为什么需要这个模块（产品定调 2026-08-15）
//!
//! 大脑聚合此前对所有协议一律用 `key.params.max_tokens.unwrap_or(4096)`。那个 4096 是
//! **SynaRoute 自己加的截断点**：参与者/汇总者/决策者的长回答会在 4096 token 处被切掉，
//! 用户看到的是「模型答一半」，而没有任何地方告诉他这是本地配置造成的。用户定的边界是
//! 「大脑聚合也不该设上限，否则拿不到完整回答」。
//!
//! ## 两家协议的能力不同，不能一刀切
//!
//! - **OpenAI Chat / Responses**：`max_tokens` / `max_completion_tokens` / `max_output_tokens`
//!   都是**可选**请求控制项。省略即「不由请求方限制」，由服务端/模型默认值决定。
//!   → 聚合直接**不发**这个字段（[`OutputBudget::Unbounded`]）。
//! - **Anthropic Messages**：`max_tokens` 是**必填**字段，省略直接 HTTP 400。
//!   → 不可能既「请求成功」又「请求里没有上限」。只能填一个**尽可能大**的值。
//!
//! ⚠️ 「省略」不等于数学意义上的无限：仍受模型自身上下文窗口、服务商默认值与服务端策略约束。
//! 这一点必须在文档里说清，否则用户会以为回答长度从此无约束。
//!
//! ## Anthropic 侧怎么取「尽可能大」
//!
//! `max_tokens` 与输入共享上下文窗口，故上限 = `context_window − 本轮输入`。窗口取
//! **本次实际要打的真实模型**那条 `ModelInfo.context_window`（[`ProviderKey::context_window_of_real`]），
//! 而不是常量 —— 硬编码一个大数（如 64000）在窗口更小的模型/中转上会直接 400，
//! 硬编码一个小数（如 4096）就是把刚拆掉的截断点又装回去。
//!
//! **没有可信窗口数据时不猜**：`fetch_models` 拉来的模型一律 `context_window: None`（很常见），
//! 此时既不能回退 4096（会静默截断，正是本次要消除的），也不能瞎填一个大数（会 400 且归因困难）。
//! 故返回 `Err(可行动原因)`，由调用方把这条 Key 报成「不可用 + 明确原因」——
//! 让用户去补上下文窗口，而不是拿到一个被悄悄截断的答案。这是**刻意牺牲兼容性换取诚实**。

use crate::model::{Protocol, ProviderKey};
use serde_json::Value;

/// 留给协议开销的安全余量（token）。
///
/// 估算永远不可能与厂商 tokenizer 逐 token 一致，而**高估输出预算的后果是上游 400**
/// （`max_tokens` 加输入超过窗口），比低估严重得多。故整体向「略微保守」偏。
const SAFETY_MARGIN_TOKENS: u32 = 1_024;

/// 文本 token 的保守估计。
///
/// ASCII（英文、绝大多数代码）约 4 字符/token；CJK、emoji 和其它非 ASCII 字符不能按 2 字符/token
/// 算——中文常接近 1 字符/token，除以 2 会**低估**输入，进而允许一个超出上下文窗口的输出预算。
/// 因此采用混合上界：ASCII 每 4 字符算 1 token，任何非 ASCII 字符逐个算 1 token。偏保守
/// 的代价只是给输出少留一点空间；低估的代价是 Anthropic 400，后者不可接受。
pub fn estimate_tokens(text: &str) -> u32 {
    let mut ascii = 0usize;
    let mut non_ascii = 0usize;
    for ch in text.chars() {
        if ch.is_ascii() {
            ascii += 1;
        } else {
            non_ascii += 1;
        }
    }
    ascii
        .div_ceil(4)
        .saturating_add(non_ascii)
        .min(u32::MAX as usize) as u32
}

/// 估算文本与 JSON 结构的 token 数（**不含图片 transport base64**）。
///
/// 图像的 base64 是 HTTP/JSON 传输编码，不是模型 tokenizer 实际接收到的文字；直接把它
/// 按字符计数，一张允许的 5MB 图片会被估成 300 多万 token，明明有效的视觉请求却被本地
/// 错判「塞满 200k 窗口」。图像 token 由服务端按尺寸/视觉规则计算，客户端没有尺寸时
/// 无法可靠估；故只对**非图片** JSON 结构做文本估算，图像统一走保守固定占位。
const IMAGE_INPUT_TOKEN_ESTIMATE: u32 = 8_000;

/// 将 JSON 值估算为 token，跳过名为 `data`（Anthropic source.base64）与 `url`
/// （OpenAI data:image/...;base64,...）的图片传输正文，替为每张一份视觉输入占位。
///
/// 这个函数只服务 Anthropic 的「窗口 − 输入」预算，不参与模型实际计费。宁可把图像占位
/// 估得偏大一点，也不能把 base64 字节数当文本 token 把输出预算压成 0。
pub fn estimate_json_tokens_without_image_transport(v: &Value) -> u32 {
    fn walk(v: &Value, image_slot: bool) -> u32 {
        match v {
            Value::Object(obj) => obj
                .iter()
                .fold(0u32, |total, (k, value)| {
                    let is_data_url = k == "url"
                        && value
                            .as_str()
                            .is_some_and(|s| s.starts_with("data:image/"));
                    let is_image_base64 = image_slot && k == "data";
                    if is_data_url || is_image_base64 {
                        total.saturating_add(IMAGE_INPUT_TOKEN_ESTIMATE)
                    } else {
                        let key_tokens = estimate_tokens(k);
                        // Anthropic image block: {type:"image", source:{type:"base64", data:"..."}}
                        let child_image_slot = k == "source"
                            && value
                                .get("type")
                                .and_then(|t| t.as_str())
                                == Some("base64");
                        total
                            .saturating_add(key_tokens)
                            .saturating_add(walk(value, child_image_slot))
                    }
                }),
            Value::Array(arr) => arr
                .iter()
                .fold(0u32, |total, item| total.saturating_add(walk(item, false))),
            Value::String(s) => estimate_tokens(s),
            // JSON 标点/数字/boolean 仍有一点 token 成本；不必精算但也别当 0。
            Value::Number(n) => estimate_tokens(&n.to_string()),
            Value::Bool(b) => estimate_tokens(&b.to_string()),
            Value::Null => 1,
        }
    }
    walk(v, false)
}

/// 各 Claude 家族的**最大单次输出** token（不是上下文窗口）。
///
/// **上下文窗口 ≠ 最大输出**：4.5 系都是 200k context 但最大输出 64k。只取
/// `window − input` 时短 prompt 会发近 200k 的 `max_tokens`，官方与严格中转直接 400
/// —— 这是审计实测出的 provider-break，不能只靠 `contextWindow` 推断。
///
/// 表按「家族片段 → 最大输出」组织，用 `contains` 而非全等：第三方中转普遍给模型名加
/// 前后缀（`anthropic/claude-sonnet-4-5`、`claude-3-7-sonnet-20250219` 等），
/// 全等匹配会把它们全判成未知、拒掉本项目的主场景（Codex 接 Claude 中转）。
/// 顺序从**新到旧**：`claude-3-7` 必须排在 `claude-3` 之前，否则前者会先命中后者的 8192。
const CLAUDE_MAX_OUTPUT_TABLE: &[(&str, u32)] = &[
    ("claude-opus-4-5", 64_000),
    ("claude-sonnet-4-5", 64_000),
    ("claude-haiku-4-5", 64_000),
    ("claude-opus-4-1", 32_000),
    ("claude-opus-4", 32_000),
    ("claude-sonnet-4", 64_000),
    ("claude-haiku-4", 32_000),
    ("claude-3-7", 64_000),
    ("claude-3-5", 8_192),
    ("claude-3", 4_096),
];

/// **非 Claude 家族**的最大输出（按厂商/家族片段匹配）。
///
/// 为什么需要它：Anthropic 协议的 `max_tokens` 必填，而本项目的主场景是**中转站**——
/// 用户拿一个 Anthropic 协议的 Key 指向 GLM / DeepSeek / Kimi / Qwen 的中转。这些模型名
/// 完全不含 `claude`，上面那张表一个都认不出，于是全部落到「请手填最大单次输出」。
/// 用户视角就是「加个 Key 还要我去查文档填数字」——而这些值本就是公开且稳定的。
///
/// 数值来源：各家官方文档的 max output tokens（2026-08 核对）。取值刻意**偏保守**：
/// 填小了只是长回答被截断（且下面的 `window − input` 钳制通常先生效），填大了会被上游 400，
/// 后者是硬失败。故拿不准时取该家族的下限档。
const OTHER_MAX_OUTPUT_TABLE: &[(&str, u32)] = &[
    // 智谱 GLM：4.6/4.5 系 96k 输出；老 4 系 8k
    ("glm-5", 96_000),
    ("glm-4-6", 96_000),
    ("glm-4-5", 96_000),
    ("glm-4", 8_192),
    ("glm", 8_192),
    // DeepSeek：v3.2/reasoner 64k，chat 8k
    ("deepseek-v4", 64_000),
    ("deepseek-v3", 64_000),
    ("deepseek-reasoner", 64_000),
    ("deepseek-chat", 8_192),
    ("deepseek", 8_192),
    // 月之暗面 Kimi：k2 系 32k
    ("kimi-k2", 32_000),
    ("kimi", 32_000),
    ("moonshot", 32_000),
    // 通义千问：coder/plus 系 64k，其余 8k
    ("qwen3-coder", 64_000),
    ("qwen3", 32_000),
    ("qwen", 8_192),
    // OpenAI（经 Anthropic 协议中转的情形）：gpt-5 系 128k，4o 系 16k
    ("gpt-5", 128_000),
    ("gpt-4o", 16_384),
    ("gpt-4", 8_192),
    ("o3", 100_000),
    ("o1", 100_000),
    // xAI Grok：4 系 32k
    ("grok-4", 32_000),
    ("grok", 32_000),
    // MiniMax / StepFun / Mistral
    ("minimax", 32_000),
    ("step-3", 32_000),
    ("mistral", 32_000),
];

/// 认不出模型名时的**兜底最大输出**。
///
/// ## 为什么改成兜底、不再报错
///
/// 旧行为是「认不出就报错、让用户去 Key 编辑器手填」。设计初衷是「不许猜」，但实测下来
/// 代价过高：中转站的私有模型名（`gpt-5.6-sol`、`claude-opus-5-thinking` 之类）千变万化，
/// 内置表**永远追不上**，于是用户每加一个 Key 都撞一次「全部 Key 失败: 缺少最大输出能力数据」
/// —— 一个本该开箱可用的软件，变成必须先查文档填数字。用户明确要求去掉这道人工步骤。
///
/// ## 为什么 8192 是安全的兜底值
///
/// 关键在于**填小了与填大了的后果不对称**：
/// - 填大了（如按窗口取 200k）→ 官方与严格中转直接 **400**，请求根本发不出去（硬失败）；
/// - 填小了 → 只在「回答确实超过这个长度」时被截断，而截断现在**有可见性**
///   （`was_truncated` 会写进日志与 stop_reason，见 sse.rs 的截断信号透传）。
///
/// 8192 是所有主流模型都支持的下限（连 claude-3-5 与 gpt-4 都是这个值），故绝不会因为
/// 「上游不支持这么大」而 400。且 `anthropic_required_max_tokens` 还会再取
/// `min(window − input, 这个值)`，所以短窗口场景下它自然更小。
///
/// 代价是「未知的大模型只能一次输出 8k」。这是**刻意的取舍**：宁可让长回答分几次拿到，
/// 也不要让用户开箱就撞 400 或被迫查文档。用户仍可在 Key 编辑器里手填覆盖它（那条路没删）。
const FALLBACK_MAX_OUTPUT: u32 = 8_192;

/// 按模型名推断最大输出：先查 Claude 表，再查其它厂商表，都认不出返回 `None`。
///
/// 两张表分开而不合并成一张：Claude 那张有**子版本边界检查**（未列出的新子版本一律不匹配，
/// 见 [`anthropic_max_output_for`] 的文档），而其它厂商的版本号规律各不相同、没有那套判据，
/// 混在一起会让边界检查对它们产生误判。
fn known_max_output_for(model: &str) -> Option<u32> {
    if let Some(v) = anthropic_max_output_for(model) {
        return Some(v);
    }
    let lower = model.to_ascii_lowercase().replace('.', "-");
    OTHER_MAX_OUTPUT_TABLE
        .iter()
        .find(|(family, _)| lower.contains(family))
        .map(|(_, max)| *max)
}

/// Claude 家族的**下限参考值**：不做子版本边界检查的裸 `contains` 匹配。
///
/// 只用于一个地方：给「未列出的新 Claude 子版本」（如 `claude-opus-4-6`）挑兜底值。
///
/// ## 为什么不能直接用全局兜底 8192
///
/// `claude-opus-4-6` 的同族 `claude-opus-4` 是 32k。用 8192 兜底等于把一个已知支持 32k 的
/// 模型限制到 8k —— 比「按同族取值」明显更差。而**新子版本几乎不会降低输出上限**
/// （厂商迭代方向一直是往上），故同族的值是个安全下界。
///
/// ## 为什么仍要与 8192 取 max
///
/// 反向情形同样存在：`claude-3-8` 的同族 `claude-3` 只有 4096，比全局兜底还低。
/// 取 `max(同族, 8192)` 在两种方向上都不差于任一单独取值：
/// - `claude-opus-4-6` → max(32k, 8192) = 32k（拿到同族的真实能力）
/// - `claude-3-8`      → max(4096, 8192) = 8192（不被老族的低上限拖累）
///
/// 子版本边界检查本身**保留**（[`anthropic_max_output_for`] 仍对未列出子版本返回 `None`）：
/// 它区分「日期后缀」与「版本后缀」的能力仍然需要，只是「认不出之后做什么」从
/// 「报错让用户填」改成了「按同族兜底」——因为截断现在是**可见的**
/// （`was_truncated` 进日志、`stop_reason: max_tokens` 透传给下游，见 sse.rs），
/// 而报错是把用户挡在门外。
fn claude_family_floor(model: &str) -> Option<u32> {
    let lower = model.to_ascii_lowercase().replace('.', "-");
    CLAUDE_MAX_OUTPUT_TABLE
        .iter()
        .find(|(family, _)| lower.contains(family))
        .map(|(_, max)| *max)
}

/// 返回已知 Anthropic 模型的最大输出能力；认不出来则 `None`（调用方须报错，不许猜）。
///
/// **子版本边界检查**：家族片段后紧跟 `-<1~2 位数字>` 说明是表里没列的**更新子版本**
/// （如 `claude-opus-4-6` 命中片段 `claude-opus-4`）——其能力未知，不得静默继承旧同族的
/// 上限（旧族 32k、新族实际 64k 时，长回答会在 32k 处被截断且无任何报错，正是本模块
/// 声称要消除的本地静默截断点，只是从 4096 变成了 32000）。这类返回 `None`，
/// 落到 `missing_max_output_reason` 引导用户手填「最大单次输出」。
///
/// `-<4 位以上数字>` 是**日期后缀**（`claude-sonnet-4-5-20250929`），属同一型号的快照，
/// 照常匹配 —— 用位数区分这两种形态（版本号 1~2 位、日期 8 位，中间没有现实用例）。
///
/// **点号分隔符归一**：第三方中转与用户手写常用点号写版本（`claude-3.7-sonnet`、
/// `claude-opus-4.5`）。不归一的话 `claude-3.7-sonnet` 会命中片段 `claude-3` 且其后是 `.`
/// 而非 `-` → 落到 `None => true` 无条件匹配 → 静默返回 `claude-3` 的 4096（实际应 64000），
/// 长回答被截断且无报错 —— 正是边界检查要消除的静默截断点被点号绕开。故先把 `.` 归一成 `-`
/// 再匹配，让点号版本走与短横线版本**同一条**边界判定（含子版本拒绝）。
fn anthropic_max_output_for(model: &str) -> Option<u32> {
    // 先小写、再把点号版本分隔符归一成短横线（见函数文档「点号分隔符归一」）。
    let lower = model.to_ascii_lowercase().replace('.', "-");
    CLAUDE_MAX_OUTPUT_TABLE
        .iter()
        .find(|(family, _)| {
            let Some(pos) = lower.find(family) else {
                return false;
            };
            let rest = &lower[pos + family.len()..];
            match rest.strip_prefix('-') {
                Some(after_dash) => {
                    let digits = after_dash.chars().take_while(|c| c.is_ascii_digit()).count();
                    // 1~2 位数字 = 未列出的新子版本 → 不匹配（宁可报错让用户手填）
                    !(1..=2).contains(&digits)
                }
                // 片段后不是 `-`（如 `claude-3-5-sonnet` 命中 `claude-3-5` 后是 `-s`，
                // 或整串结束）→ 正常匹配
                None => true,
            }
        })
        .map(|(_, max)| *max)
}

/// Anthropic 必填 `max_tokens` 的取值：同时受「窗口剩余」与「模型最大输出」约束。
///
/// 两条路径共用（大脑聚合、代理跨协议转换后补必填字段），**不能各写一份** ——
/// 其中一份漏掉某个钳制就会变成另一条链路上的 400 或静默截断。
///
/// `window` 传 `None` 表示没有窗口数据：此时仍可只按模型最大输出取值
/// （真实 Claude 窗口都 ≥ 200k，输入通常远小于它，故这个值实际安全），
/// 但**模型最大输出未知时必须报错**，因为那时无论填什么都是猜。
///
/// `user_max_output`：用户在该模型上手填的最大输出（`ModelInfo.max_output_tokens`）。
/// **它优先于内置表** —— 用户填了就该信他的。没填时按
/// 「Claude 表 → 其它厂商表 → [`FALLBACK_MAX_OUTPUT`] 兜底」三级推断，**永不报错**。
///
/// 改成兜底而非报错的完整理由见 [`FALLBACK_MAX_OUTPUT`]：中转站的私有模型名内置表永远
/// 追不上，报错会让用户每加一个 Key 都被迫先去查文档填数字。
pub fn anthropic_required_max_tokens(
    model: &str,
    window: Option<u32>,
    input_text_len_tokens: u32,
    user_max_output: Option<u32>,
) -> Result<u32, String> {
    // 用户手填优先；否则查两张内置表；都认不出用保守兜底值（8192，所有主流模型都支持）。
    // `filter(|v| *v > 0)` 挡掉手填 0 —— 那会让 max_tokens=0，上游必然 400，
    // 而用户以为「填 0 = 不限制」。前端也校验，这里是纵深防御。
    let max_output = user_max_output
        .filter(|v| *v > 0)
        .or_else(|| known_max_output_for(model))
        // 未列出的新 Claude 子版本：按同族下限兜底（与 8192 取 max），见 claude_family_floor。
        .or_else(|| claude_family_floor(model).map(|f| f.max(FALLBACK_MAX_OUTPUT)))
        .unwrap_or(FALLBACK_MAX_OUTPUT);
    let Some(window) = window else {
        // 无窗口数据：只受模型能力约束。不报错是刻意的 —— 报错会拒掉「用户没填窗口
        // 但模型名可辨识」这类完全可用的配置（本项目主场景之一）。
        return Ok(max_output);
    };
    let reserved = input_text_len_tokens.saturating_add(SAFETY_MARGIN_TOKENS);
    if reserved >= window {
        return Err(input_exhausts_context_reason(
            model,
            window,
            input_text_len_tokens,
        ));
    }
    Ok((window - reserved).min(max_output))
}

/// 输入已没有为输出留下空间时的可行动错误。
fn input_exhausts_context_reason(model: &str, window: u32, input: u32) -> String {
    format!(
        "模型 {model} 的上下文窗口为 {window} token，而本轮输入估算已占 {input} token（另需保留 {SAFETY_MARGIN_TOKENS} token 协议余量），\
         没有空间生成回答。请减少文件检索内容、缩短提示词或降低工具历史预算后重试。"
    )
}

/// 缺模型最大输出数据时的可行动错误。
///
/// **已不再用于阻断请求**（2026-08-21）：`anthropic_required_max_tokens` 现在按
/// 「用户手填 → Claude 表 → 其它厂商表 → [`FALLBACK_MAX_OUTPUT`] 兜底」推断，永不报错。
/// 理由见 `FALLBACK_MAX_OUTPUT` 的文档：中转站的私有模型名内置表永远追不上，
/// 报错等于让用户每加一个 Key 都被迫先查文档填数字。
///
/// 保留这个函数**只为一件事**：万一将来把兜底改回报错（比如某天发现兜底值确实会造成
/// 数据错误），文案与它指向的自助入口（Key 编辑器 → 模型列表 → 最大单次输出）还在，
/// 不必重新想一遍。删掉它省下 6 行，代价是下次要重建这套判据。
#[allow(dead_code)]
fn missing_max_output_reason(model: &str) -> String {
    format!(
        "模型 {model} 缺少最大输出 token 能力数据：Anthropic 的 max_tokens 必填，\
         但上下文窗口不等于最大输出，猜一个大数可能被上游拒绝、猜一个小数又会截断回答。\
         请到「Key 编辑器 → 模型列表」为该模型填写「最大单次输出」（如 64000，\
         可查上游服务商的模型文档），保存后重试即可。"
    )
}

/// 计算本次上游调用的输出预算。
///
/// 返回值的三种情形**刻意用 `Result<Option<u32>, _>` 表达**，因为它正好是「要往请求体里写
/// 什么」的三种答案，调用方 `?` 一下就对了：
/// - `Ok(None)` —— **不写**输出上限字段（OpenAI Chat / Responses）；
/// - `Ok(Some(n))` —— 写 `max_tokens: n`（Anthropic，n 已按窗口与本轮输入算过）；
/// - `Err(reason)` —— 无法安全取值（Anthropic 缺窗口数据），`reason` 是可直接给用户看的
///   行动指引。调用方**必须**据此中止本次调用，**不得**自行回退到某个默认值 ——
///   那就是把刚撤掉的静默截断装回去。
///
/// `model` 必须是**本次实际要打的真实模型名**（映射解析之后），否则查不到窗口数据。
/// `input_text_len_tokens` 是本轮请求体里全部输入内容的 token 估计
/// （prompt / 完整消息历史 / 工具声明都要算进去 —— 工具循环每轮重发整份历史，
/// 只算首轮 prompt 会在第 N 轮把预算算得过大而 400）。
pub fn output_budget(
    key: &ProviderKey,
    model: &str,
    input_text_len_tokens: u32,
) -> Result<Option<u32>, String> {
    match key.protocol {
        // 可选字段 → 不发，由上游自己决定自然长度。
        Protocol::OpenaiChat | Protocol::OpenaiResponses => Ok(None),
        Protocol::Anthropic => anthropic_required_max_tokens(
            model,
            key.context_window_of_real(model),
            input_text_len_tokens,
            key.max_output_of_real(model),
        )
        .map(Some),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CategoryType, KeyParams, ModelInfo};

    fn key_with(protocol: Protocol, models: &[(&str, Option<u32>)]) -> ProviderKey {
        ProviderKey {
            id: "k".into(),
            category_id: CategoryType::ClaudeCli,
            name: "K".into(),
            vendor: "v".into(),
            base_url: "https://example.com".into(),
            protocol,
            has_secret: true,
            enabled: true,
            priority: 0,
            headers_json: None,
            params: KeyParams::default(),
            models: models
                .iter()
                .map(|(n, ctx)| ModelInfo {
                    real_name: (*n).into(),
                    source: "manual".into(),
                    fetched_at: None,
                    context_window: *ctx,
                    max_output_tokens: None,
                })
                .collect(),
            mappings: vec![],
            default_model: None,
            tier_haiku: None,
            tier_sonnet: None,
            tier_opus: None,
            balance_query: None,
            cached_balance: None,
            cost_multiplier: None,
            health: Default::default(),
        }
    }

    /// OpenAI 两种协议一律「不发上限」——这是本次定调在 OpenAI 侧的全部内容。
    /// 即便该 Key 上配了 max_tokens、即便模型没有窗口数据，都不影响。
    #[test]
    fn openai_protocols_never_carry_a_limit() {
        for protocol in [Protocol::OpenaiChat, Protocol::OpenaiResponses] {
            // 有窗口数据
            let k = key_with(protocol, &[("gpt-x", Some(128_000))]);
            assert_eq!(output_budget(&k, "gpt-x", 100), Ok(None));
            // 无窗口数据也不该报错（缺窗口只是 Anthropic 才有的约束）
            let k = key_with(protocol, &[("gpt-x", None)]);
            assert_eq!(output_budget(&k, "gpt-x", 100), Ok(None));
        }
    }

    /// Anthropic 必填 max_tokens，故按「窗口 − 输入 − 余量」给一个尽可能大的值。
    #[test]
    fn anthropic_uses_remaining_context_window() {
        let k = key_with(
            Protocol::Anthropic,
            &[("claude-sonnet-4-5", Some(200_000))],
        );
        // 窗口剩余本可达 188_976，但 Claude 4.5 的官方最大输出是 64k，必须再钳一次。
        assert_eq!(
            output_budget(&k, "claude-sonnet-4-5", 10_000),
            Ok(Some(64_000))
        );
        // 关键回归点：绝不能等于 Key 上配的 4096（那是被撤下的旧默认值）。
        assert_ne!(
            output_budget(&k, "claude-sonnet-4-5", 10_000),
            Ok(Some(4096))
        );
    }

    /// 输入越大，留给输出的越小 —— 工具循环每轮重算依赖的正是这个单调性。
    #[test]
    fn anthropic_budget_shrinks_as_input_grows() {
        let k = key_with(
            Protocol::Anthropic,
            &[("claude-sonnet-4-5", Some(200_000))],
        );
        let small = output_budget(&k, "claude-sonnet-4-5", 1_000)
            .unwrap()
            .unwrap();
        // 输入足够大时，窗口余量低于 64k，此时预算应继续随着输入变大而缩小。
        let large = output_budget(&k, "claude-sonnet-4-5", 150_000)
            .unwrap()
            .unwrap();
        assert!(
            large < small,
            "输入变大时输出预算必须变小：small={small} large={large}"
        );
    }

    /// 输入逼近/超过窗口时必须**拒绝**，不能硬塞一个最小 max_tokens 再让上游报 context overflow。
    #[test]
    fn anthropic_refuses_when_input_exhausts_context() {
        let k = key_with(
            Protocol::Anthropic,
            &[("claude-sonnet-4-5", Some(200_000))],
        );
        for input in [199_000, 200_000, 500_000, u32::MAX] {
            let err = output_budget(&k, "claude-sonnet-4-5", input)
                .expect_err("输入占满窗口时不得再发请求");
            assert!(err.contains("没有空间生成回答"), "错误应可行动：{err}");
            assert!(err.contains("减少文件检索"), "错误应告诉用户怎么缩输入：{err}");
        }
    }

    /// 完全认不出的模型名（中转站私有别名）**不再报错**，而是取安全兜底值。
    ///
    /// ## 契约在 2026-08-21 反过来了，这里记下为什么
    ///
    /// 旧行为：认不出就报错、引导用户去「Key 编辑器 → 模型列表」手填最大输出。
    /// 初衷是「不许猜」，但实测代价过高 —— 中转站的私有模型名（`third-party-claude`、
    /// `gpt-5.6-sol`、站点自定义别名）千变万化，内置表**永远追不上**，于是用户每加一个 Key
    /// 都撞一次「全部 Key 失败：缺少最大输出能力数据」。一个本该开箱可用的软件变成
    /// 「先查文档填数字才能用」。
    ///
    /// 新行为的安全性依据是**两种错法的后果不对称**：
    /// - 填大了（如按窗口取 200k）→ 官方与严格中转直接 **400**，请求根本发不出去（硬失败）；
    /// - 填小了 → 只在回答确实超长时截断，而截断现在**可见**（`was_truncated` 进日志、
    ///   stop_reason 跨协议透传，见 sse.rs 的截断信号那批修复）。
    ///
    /// 8192 是所有主流模型都支持的下限（claude-3-5 与 gpt-4 都是这个值），故绝不会因
    /// 「上游不支持这么大」而 400。用户手填仍然优先（见下一条测试），那条路没删。
    #[test]
    fn anthropic_unknown_model_falls_back_instead_of_failing() {
        let k = key_with(Protocol::Anthropic, &[("third-party-claude", Some(200_000))]);
        assert_eq!(
            output_budget(&k, "third-party-claude", 100),
            Ok(Some(FALLBACK_MAX_OUTPUT)),
            "认不出的模型名必须取兜底值放行，而不是把用户挡在门外"
        );
        // 兜底值必须是「所有主流模型都支持」的下限 —— 它的意义全在于绝不触发 400。
        assert_eq!(FALLBACK_MAX_OUTPUT, 8_192, "兜底值改大会重新引入 400 风险");

        // 连 `claude` 字样都不含的名字（用户拼错、或中转站的完全私有别名）同样走兜底。
        // 与上面那条分开断言：上面走的是「含 claude 但认不出型号」，这条连 `claude_family_floor`
        // 都命中不了，是最彻底的未知形态 —— 它才是「开箱可用」的底线。
        assert_eq!(
            output_budget(&k, "some-other-model", 100),
            Ok(Some(FALLBACK_MAX_OUTPUT)),
            "完全认不出的模型名必须走兜底，而不是让整轮请求失败"
        );
    }

    /// 用户手填的 `max_output_tokens` **优先于内置表**，且能救回内置表认不出的模型。
    ///
    /// 为什么需要这条能力：`CLAUDE_MAX_OUTPUT_TABLE` 只认 Claude 家族片段，而第三方中转
    /// 普遍用私有模型名（`gpt-5.6-sol`、站点自定义别名）。此前这类模型会被
    /// `missing_max_output_reason` 整个拒掉 —— 用户明知该模型能输出多少，却没有地方告诉程序。
    ///
    /// 三条断言各覆盖一种情形，去掉 `user_max_output` 那一支后三条都会红。
    #[test]
    fn user_supplied_max_output_overrides_builtin_table() {
        let mut k = key_with(Protocol::Anthropic, &[("third-party-claude", Some(200_000))]);

        // ① 内置表认不出的模型：填了就能用（此前必被拒）
        k.models[0].max_output_tokens = Some(16_000);
        assert_eq!(
            output_budget(&k, "third-party-claude", 100),
            Ok(Some(16_000)),
            "内置表认不出的模型，用户填了最大输出就该照用"
        );

        // ② 手填值优先于内置表：模型名能被内置表认出（claude-sonnet-4-5 = 64k），
        //    但用户填了 8k（如中转商实际限制更严），必须用 8k
        let mut k2 = key_with(Protocol::Anthropic, &[("claude-sonnet-4-5", Some(200_000))]);
        k2.models[0].max_output_tokens = Some(8_000);
        assert_eq!(
            output_budget(&k2, "claude-sonnet-4-5", 100),
            Ok(Some(8_000)),
            "用户手填必须覆盖内置表的 64k"
        );

        // ③ 窗口钳制仍然生效：手填 100k 但窗口只剩 ~50k 时取窗口余量
        let mut k3 = key_with(Protocol::Anthropic, &[("custom-model", Some(60_000))]);
        k3.models[0].max_output_tokens = Some(100_000);
        let got = output_budget(&k3, "custom-model", 8_000).unwrap().unwrap();
        assert!(
            got < 60_000 && got > 40_000,
            "手填值不得绕过窗口钳制（窗口 60k − 输入 8k − 余量 1k ≈ 51k），实际 {got}"
        );

        // ④ 手填 0 视为未填（回退内置表）：0 会让上游 400，而用户可能以为「0 = 不限制」
        let mut k4 = key_with(Protocol::Anthropic, &[("claude-sonnet-4-5", Some(200_000))]);
        k4.models[0].max_output_tokens = Some(0);
        assert_eq!(
            output_budget(&k4, "claude-sonnet-4-5", 100),
            Ok(Some(64_000)),
            "手填 0 必须回退内置表，不能真的发 max_tokens=0"
        );
    }

    /// 缺窗口数据的 Anthropic Key：按模型能力取值，**不报错**。
    ///
    /// 契约在 2026-08-21 改过（用户要求「不用用户设置，自己按厂商给默认」）：此前缺窗口就报错
    /// 引导用户去填 contextWindow。现在只要能推断出最大输出就照用 —— 真实 Claude 窗口都
    /// ≥200k，输入通常远小于它，故不带窗口钳制也安全（见 `anthropic_required_max_tokens`
    /// 里 `window == None` 那条分支的说明）。
    ///
    /// **仍必须钉住的**：不能因为「不报错了」就退回某个硬编码小值。`claude-x` 含 `claude`
    /// 但不含任何已列家族片段 → 走全局兜底 8192；若哪天有人给它加个 `.unwrap_or(4096)`
    /// 之类的旁路，这条会红。
    #[test]
    fn anthropic_without_context_window_uses_capability_not_error() {
        let k = key_with(Protocol::Anthropic, &[("claude-x", None)]);
        assert_eq!(
            output_budget(&k, "claude-x", 100),
            Ok(Some(FALLBACK_MAX_OUTPUT)),
            "缺窗口不该报错，应按兜底能力取值（8192，所有主流模型都支持）"
        );
    }

    /// 表里未列出的**新子版本号**：按同族下限兜底（不是全局 8192，也不是报错）。
    ///
    /// ## 契约演进（2026-08-21）
    ///
    /// 旧行为：`claude-opus-4-6` 不含 `claude-opus-4-5` 片段、但含 `claude-opus-4` →
    /// 边界检查判它是「未列出的新子版本」→ **报错**，引导用户手填。
    /// 那时的理由是「静默取旧族 32k，若新族实际 64k 则长回答被悄悄截断」。
    ///
    /// 现在改为按同族兜底，因为两个前提都变了：
    /// 1. **截断已可见** —— `was_truncated` 进日志、`stop_reason: max_tokens` 透传给下游
    ///    （见 sse.rs 的截断信号透传），不再是「悄悄」；
    /// 2. 报错的代价被证实过高 —— 用户每遇到一个新模型名就被挡在门外。
    ///
    /// 取 `max(同族, 8192)`：新族几乎不会降低上限，故同族值是安全下界；而对
    /// `claude-3-8` 这种同族只有 4096 的，8192 兜底更好。两个方向都不差于单独取值。
    ///
    /// **边界检查本身保留**：它区分「日期后缀 vs 版本后缀」的能力仍然需要（下面两条断言），
    /// 变的只是「认不出之后做什么」。
    ///
    /// 故障注入判据：删掉 `claude_family_floor` 那一支 → 第一条 `claude-opus-4-6`
    /// 期望的 32000 会变成 8192。
    #[test]
    fn newer_subversion_falls_back_to_family_floor() {
        // 未列出的新子版本 → 取同族下限（opus-4 = 32k），而非全局兜底 8192
        for m in ["claude-opus-4-6", "claude-opus-4-7-20260301"] {
            let k = key_with(Protocol::Anthropic, &[(m, Some(200_000))]);
            assert_eq!(
                output_budget(&k, m, 100),
                Ok(Some(32_000)),
                "{m} 应按同族 claude-opus-4 的 32k 兜底（比全局 8192 更贴近真实能力）"
            );
        }
        // haiku-4-6：同族 haiku-4 = 32k
        let k = key_with(Protocol::Anthropic, &[("claude-haiku-4-6", Some(200_000))]);
        assert_eq!(output_budget(&k, "claude-haiku-4-6", 100), Ok(Some(32_000)));

        // 同族低于全局兜底时取兜底：claude-3-8 的同族 claude-3 只有 4096
        let k = key_with(Protocol::Anthropic, &[("claude-3-8", Some(200_000))]);
        assert_eq!(
            output_budget(&k, "claude-3-8", 100),
            Ok(Some(FALLBACK_MAX_OUTPUT)),
            "同族上限低于 8192 时不该被老族拖累"
        );

        // 日期后缀（≥4 位数字）是同一型号的快照，照常精确匹配 —— 边界检查的核心能力
        let k = key_with(Protocol::Anthropic, &[("claude-sonnet-4-5-20250929", Some(200_000))]);
        assert_eq!(
            output_budget(&k, "claude-sonnet-4-5-20250929", 100),
            Ok(Some(64_000)),
            "日期后缀不该被当成新子版本"
        );
        // 家族片段后接文字（claude-3-5-sonnet-20241022）也照常匹配
        let k = key_with(Protocol::Anthropic, &[("claude-3-5-sonnet-20241022", Some(200_000))]);
        assert_eq!(
            output_budget(&k, "claude-3-5-sonnet-20241022", 100),
            Ok(Some(8_192))
        );
        // 中转前缀（anthropic/claude-sonnet-4-5）仍要能匹配 —— contains 的初衷不能丢
        let k = key_with(Protocol::Anthropic, &[("anthropic/claude-sonnet-4-5", Some(200_000))]);
        assert_eq!(
            output_budget(&k, "anthropic/claude-sonnet-4-5", 100),
            Ok(Some(64_000))
        );
    }

    /// **非 Claude 厂商的模型也要开箱可用**（用户要求：不用手填，按厂商给默认）。
    ///
    /// 本项目主场景是中转站：用户拿一个 Anthropic 协议的 Key 指向 GLM / DeepSeek / Kimi /
    /// Qwen。这些模型名完全不含 `claude`，旧实现一个都认不出、全部要求手填。
    ///
    /// 故障注入判据：删掉 `OTHER_MAX_OUTPUT_TABLE` 的查表那一支 → 全部变成 8192，本条红。
    #[test]
    fn non_claude_vendors_get_builtin_defaults() {
        for (model, want) in [
            ("glm-4.6", 96_000u32),
            ("glm-4-plus", 8_192),
            ("deepseek-reasoner", 64_000),
            ("deepseek-chat", 8_192),
            ("kimi-k2-0905-preview", 32_000),
            ("qwen3-coder-plus", 64_000),
            ("gpt-5.6-sol", 128_000), // 中转站私有别名，含 gpt-5 片段
            ("grok-4-fast", 32_000),
            ("minimax-m2.7", 32_000),
        ] {
            let k = key_with(Protocol::Anthropic, &[(model, Some(200_000))]);
            assert_eq!(
                output_budget(&k, model, 100),
                Ok(Some(want)),
                "{model} 应按内置厂商表取 {want}，而不是要求用户手填"
            );
        }
    }


    #[test]
    fn dot_notation_versions_match_same_as_dash_and_opus_4_1_recognized() {
        // 点号版本名（第三方中转/用户手写都常见）必须与短横线版本走同一判定，不能因分隔符
        // 不同而静默继承旧同族的错误上限。回归：`claude-3.7-sonnet` 曾命中片段 `claude-3`、
        // 其后是 `.` 落到 `None => true` → 静默返回 4096（应 64000），长回答被截断且无报错。
        for (m, want) in [
            ("claude-3.7-sonnet", 64_000u32), // 应同 claude-3-7，而非 claude-3 的 4096
            ("claude-3.5-sonnet", 8_192),     // 应同 claude-3-5
            ("claude-haiku-4.5", 64_000),     // 应同 claude-haiku-4-5
            ("claude-opus-4.5", 64_000),      // 应同 claude-opus-4-5
        ] {
            let k = key_with(Protocol::Anthropic, &[(m, Some(200_000))]);
            assert_eq!(
                output_budget(&k, m, 100),
                Ok(Some(want)),
                "点号版本 {m} 应归一后匹配到 {want}，而非静默继承更旧同族的上限"
            );
        }
        // 点号写的**未列出新子版本**：不再报错，改为按同族下限兜底（契约已反转，
        // 见 anthropic_unknown_model_falls_back_instead_of_failing 的完整理由）。
        //
        // 仍然**必须钉住的性质**：不得静默继承同族那个可能过大的上限。这里 opus-4 族下限
        // 是 32000，兜底取 `max(族下限, 8192)`；关键是它**不会**因为 4.8 未知就去发 200k
        // 而撞 400，也不会把用户挡在门外。
        let k = key_with(Protocol::Anthropic, &[("claude-opus-4.8", Some(200_000))]);
        let got = output_budget(&k, "claude-opus-4.8", 100).expect("未知子版本应兜底放行，不再报错");
        assert_eq!(
            got,
            Some(32_000),
            "未列出的新子版本应按同族下限兜底（opus-4 族 = 32000），既不报错也不发 200k 撞 400"
        );
        // Opus 4.1 是现役型号，规范 ID 与点号写法都应认得（真实上限 32000）。
        for m in ["claude-opus-4-1-20250805", "claude-opus-4.1"] {
            let k = key_with(Protocol::Anthropic, &[(m, Some(200_000))]);
            assert_eq!(
                output_budget(&k, m, 100),
                Ok(Some(32_000)),
                "Opus 4.1 应被内置表认出为 32000，而非开箱拒绝：{m}"
            );
        }
    }

    /// 中文按字符而非 UTF-8 字节估算，并刻意按 1 字符/token 取**上界**。
    /// 用 len() 会把中文输入估成三倍；除以 2 则会低估到实际窗口外，二者都不行。
    #[test]
    fn token_estimate_handles_cjk_without_underestimating() {
        let cn = "中文字符串";
        assert_eq!(cn.len(), 15, "前提：5 个中文字 = 15 字节");
        assert_eq!(estimate_tokens(cn), 5, "CJK 每字符至少算 1 token");
        // ASCII 仍按 4 字符/token 的常用近似。
        assert_eq!(estimate_tokens("abcdefgh"), 2);
        // 空串不该被算成 1。
        assert_eq!(estimate_tokens(""), 0);
    }

    /// 图片 transport base64 不是模型看到的文字，不能把编码长度按 token 计。
    #[test]
    fn image_base64_is_replaced_by_bounded_vision_estimate() {
        let huge_base64 = "A".repeat(6_700_000);
        let image = serde_json::json!({
            "role": "user",
            "content": [{
                "type": "image",
                "source": { "type": "base64", "media_type": "image/png", "data": huge_base64 }
            }]
        });
        let got = estimate_json_tokens_without_image_transport(&image);
        assert!(got >= IMAGE_INPUT_TOKEN_ESTIMATE);
        assert!(
            got < 20_000,
            "图片 base64 不应膨胀成数百万 token，实际 {got}"
        );
    }
}
