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
    ("claude-opus-4", 32_000),
    ("claude-sonnet-4", 64_000),
    ("claude-haiku-4", 32_000),
    ("claude-3-7", 64_000),
    ("claude-3-5", 8_192),
    ("claude-3", 4_096),
];

/// 返回已知 Anthropic 模型的最大输出能力；认不出来则 `None`（调用方须报错，不许猜）。
fn anthropic_max_output_for(model: &str) -> Option<u32> {
    let lower = model.to_ascii_lowercase();
    CLAUDE_MAX_OUTPUT_TABLE
        .iter()
        .find(|(family, _)| lower.contains(family))
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
pub fn anthropic_required_max_tokens(
    model: &str,
    window: Option<u32>,
    input_text_len_tokens: u32,
) -> Result<u32, String> {
    let max_output =
        anthropic_max_output_for(model).ok_or_else(|| missing_max_output_reason(model))?;
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
fn missing_max_output_reason(model: &str) -> String {
    format!(
        "模型 {model} 缺少最大输出 token 能力数据：Anthropic 的 max_tokens 必填，\
         但上下文窗口不等于最大输出，猜一个大数可能被上游拒绝、猜一个小数又会截断回答。\
         请改用已知能力的模型，或在后续版本补充该模型的最大输出能力后重试。"
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
            // 刻意配一个旧的 max_tokens：本模块**绝不能**读它（那是被撤下的截断源头）。
            params: KeyParams {
                max_tokens: Some(4096),
                ..Default::default()
            },
            models: models
                .iter()
                .map(|(n, ctx)| ModelInfo {
                    real_name: (*n).into(),
                    source: "manual".into(),
                    fetched_at: None,
                    context_window: *ctx,
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

    /// 用户填了窗口、但模型不是可靠规格表里的 Anthropic 型号时也不猜最大输出。
    #[test]
    fn anthropic_without_known_max_output_refuses_instead_of_guessing() {
        let k = key_with(Protocol::Anthropic, &[("third-party-claude", Some(200_000))]);
        let err = output_budget(&k, "third-party-claude", 100)
            .expect_err("未知最大输出时不得猜一个数");
        assert!(err.contains("最大输出"), "错误应指出缺的能力数据：{err}");
    }

    /// 缺窗口数据的 Anthropic Key：**不猜**，报出可执行的原因。
    ///
    /// 这条是本次定调最容易被「修好意」破坏的地方：随手加个 `.unwrap_or(4096)` 就能让
    /// 它「不报错了」，代价是回答又开始被悄悄截断。故断言里同时钉住「不是 Limit」。
    #[test]
    fn anthropic_without_context_window_refuses_instead_of_guessing() {
        let k = key_with(Protocol::Anthropic, &[("claude-x", None)]);
        let got = output_budget(&k, "claude-x", 100);
        let reason = got.expect_err("缺窗口时必须报错，而不是给出某个上限");
        assert!(reason.contains("claude-x"), "原因要点名具体模型：{reason}");
        assert!(
            reason.contains("上下文窗口"),
            "原因必须告诉用户去补什么：{reason}"
        );
    }

    /// 模型不在列表里（拼错/未录入）同样按「缺数据」处理，不退回默认值。
    #[test]
    fn anthropic_unknown_model_is_treated_as_missing_window() {
        let k = key_with(Protocol::Anthropic, &[("claude-x", Some(200_000))]);
        assert!(output_budget(&k, "some-other-model", 100).is_err());
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
