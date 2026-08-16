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
///
/// **子版本边界检查**：家族片段后紧跟 `-<1~2 位数字>` 说明是表里没列的**更新子版本**
/// （如 `claude-opus-4-6` 命中片段 `claude-opus-4`）——其能力未知，不得静默继承旧同族的
/// 上限（旧族 32k、新族实际 64k 时，长回答会在 32k 处被截断且无任何报错，正是本模块
/// 声称要消除的本地静默截断点，只是从 4096 变成了 32000）。这类返回 `None`，
/// 落到 `missing_max_output_reason` 引导用户手填「最大单次输出」。
///
/// `-<4 位以上数字>` 是**日期后缀**（`claude-sonnet-4-5-20250929`），属同一型号的快照，
/// 照常匹配 —— 用位数区分这两种形态（版本号 1~2 位、日期 8 位，中间没有现实用例）。
fn anthropic_max_output_for(model: &str) -> Option<u32> {
    let lower = model.to_ascii_lowercase();
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
/// **它优先于内置表** —— 内置表只认 Claude 家族片段，第三方中转的私有模型名
/// （`gpt-5.6-sol` 之类）认不出来就会被整个拒掉；用户填了就该信他的。
/// 传 `None` 时回退内置表，内置表也认不出才报错。
pub fn anthropic_required_max_tokens(
    model: &str,
    window: Option<u32>,
    input_text_len_tokens: u32,
    user_max_output: Option<u32>,
) -> Result<u32, String> {
    // 用户手填优先于内置表：内置表按 Claude 家族片段匹配，认不出第三方私有模型名。
    // `filter(|v| *v > 0)` 挡掉手填 0 —— 那会让 max_tokens=0，上游必然 400，
    // 而用户以为「填 0 = 不限制」。前端也校验，这里是纵深防御。
    let max_output = user_max_output
        .filter(|v| *v > 0)
        .or_else(|| anthropic_max_output_for(model))
        .ok_or_else(|| missing_max_output_reason(model))?;
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
/// 必须指向**用户 30 秒能自助完成的修复入口**（Key 编辑器模型列表的「最大单次输出」），
/// 而不是「等后续版本」—— 那个字段正是为救回内置表认不出的第三方模型名而加的，
/// 文案不提它等于把用户引向死胡同。
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

    /// 用户填了窗口、但模型不是可靠规格表里的 Anthropic 型号时也不猜最大输出。
    #[test]
    fn anthropic_without_known_max_output_refuses_instead_of_guessing() {
        let k = key_with(Protocol::Anthropic, &[("third-party-claude", Some(200_000))]);
        let err = output_budget(&k, "third-party-claude", 100)
            .expect_err("未知最大输出时不得猜一个数");
        assert!(err.contains("最大输出"), "错误应指出缺的能力数据：{err}");
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

    /// 表里未列出的**新子版本号**不得静默继承旧同族的上限；日期后缀照常匹配。
    ///
    /// 反例（本条要防的）：`claude-opus-4-6` 不含 `claude-opus-4-5` 片段、但含
    /// `claude-opus-4` → 旧逻辑静默取 32k。若新族实际支持 64k+，长回答在 32k 处被截断
    /// 且无任何报错 —— 本模块声称消除的「本地静默截断点」换个数字回来了。
    /// 现在这类必须报「缺能力数据」，引导用户手填最大单次输出。
    ///
    /// 故障注入判据：把 `anthropic_max_output_for` 的边界检查删掉（退回裸 `contains`）
    /// → 前两条断言变红。
    #[test]
    fn newer_subversion_does_not_inherit_older_family_cap() {
        // 未列出的新子版本 → 必须报错（而不是拿 claude-opus-4 的 32k）
        for m in ["claude-opus-4-6", "claude-opus-4-7-20260301", "claude-haiku-4-6"] {
            let k = key_with(Protocol::Anthropic, &[(m, Some(200_000))]);
            let err = output_budget(&k, m, 100)
                .expect_err("新子版本不得继承旧同族上限，必须报缺能力数据");
            assert!(err.contains("最大单次输出"), "错误应指向手填入口：{err}");
        }
        // 日期后缀（≥4 位数字）是同一型号的快照，照常匹配
        let k = key_with(Protocol::Anthropic, &[("claude-sonnet-4-5-20250929", Some(200_000))]);
        assert_eq!(
            output_budget(&k, "claude-sonnet-4-5-20250929", 100),
            Ok(Some(64_000)),
            "日期后缀不该被当成新子版本拒掉"
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
