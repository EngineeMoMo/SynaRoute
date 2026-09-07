//! Anthropic 请求里的「推理强度」→ Chat 中枢档位。
//!
//! # 它补的洞
//!
//! `anthropic_to_openai` 原先**只读 `thinking.budget_tokens`**。而 Claude 的
//! Messages API 现在有两套形态：
//!
//! | 形态 | 长相 | 状态 |
//! |---|---|---|
//! | legacy | `thinking: {type:"enabled", budget_tokens:N}` | 仍受支持 |
//! | adaptive | `thinking: {type:"adaptive"}` + `output_config: {effort:"high"}` | 新形态 |
//!
//! 取证：CLIProxyAPI 的 `internal/thinking/apply.go` 里
//! `if thinkingType == "adaptive" || thinkingType == "auto"` 之后读的就是
//! `output_config.effort`（注释原文 "Claude adaptive thinking uses
//! output_config.effort (low/medium/high/max)"）；ccLoad 更进一步，把 `enabled`
//! 直接称作 **legacy** 并在入口处主动归一化它。
//!
//! 于是此前：下游 Claude Code 发 adaptive 形态、上游是 OpenAI/Chat 协议时，
//! **用户设的思考档位被整个丢掉**，请求照常成功、回答照常返回，只是不思考了 ——
//! 又一个静默失效。
//!
//! 顺带说明本仓为什么会漏掉它：`output_config` 这个信封我们**已经在用**了
//! （`structured_output.rs` 的 `output_config.format`，那里还专门钉过
//! 「字段名不是 `output_format`，后者是 beta 期旧名」），也就是同一个信封里
//! 一个字段跟上了、另一个没跟上。
//!
//! # 🔴 出方向刻意**不**改成 adaptive
//!
//! 我们发给 Anthropic 上游的仍然是 legacy `{type:"enabled", budget_tokens:N}`
//! （见 `convert.rs` 的 `effort_to_thinking_budget` / `apply_pending_thinking`）。
//! 理由是代价不对称：legacy 形态官方仍然支持、且**已经在所有用户的第三方中转站上
//! 跑通过**；换成 adaptive 是拿一个已验证的兼容面去赌一个未验证的 ——
//! 中转站不认新形态时上游直接 400，而这条路上没有任何回退。
//!
//! 真要改，判据是「拿到中转站对 adaptive 的实际响应」，不是读文档。
//! 同本仓「不拿真实上游试的组合就不发」那条。

use serde_json::Value;

/// 读出这个 Anthropic 请求想要的推理强度（中枢档位口径：
/// `minimal`/`low`/`medium`/`high`/`xhigh`）。两种形态都认 —— 只认一种就是
/// 静默丢掉另一种下游客户端设的档位。
///
/// legacy 优先：`budget_tokens` 是个**确切的数字**，比档位名携带的信息更多；
/// 而且两者同时出现时（客户端自己写了 budget 又写了 effort），
/// 数字才是 Anthropic 上游实际会执行的那个。
pub(super) fn request_effort(body: &Value) -> Option<&'static str> {
    let thinking = body.get("thinking");
    if let Some(budget) =
        thinking.and_then(|t| t.get("budget_tokens")).and_then(Value::as_u64)
    {
        return Some(budget_to_hub(budget));
    }
    // adaptive：预算交给模型自己定，档位落在 output_config.effort。
    // `auto` 与 adaptive 同义（CLIProxyAPI 两者同一分支）。
    let kind = thinking.and_then(|t| t.get("type")).and_then(Value::as_str)?;
    if kind != "adaptive" && kind != "auto" {
        return None;
    }
    let effort = body
        .get("output_config")
        .and_then(|c| c.get("effort"))
        .and_then(Value::as_str)?;
    claude_effort_to_hub(effort)
}

/// `thinking.budget_tokens` → 最接近的中枢档位（供下游 Chat/Responses 客户端
/// 连 Anthropic-thinking 上游时还原语义）。
fn budget_to_hub(budget: u64) -> &'static str {
    match budget {
        0..=3072 => "low",
        3073..=12288 => "medium",
        12289..=24576 => "high",
        _ => "xhigh",
    }
}

/// Claude 的 `output_config.effort` → 中枢档位。
///
/// Claude 的档位是 `low/medium/high/max`（4 档，见模块头 CLIProxyAPI 那条取证），
/// 中枢自 2026-09-07 起是 `minimal/low/medium/high/xhigh/max/ultra`（按官方
/// `codex debug models` 实测对齐）。**同名档直接对应**，Claude 的顶档 `max` 落中枢的 `max`。
///
/// ⚠️ **这里原先是 `"max" => Some("xhigh")`**，理由是「中枢不认 `max`，原样传过去会走
/// `_ => return None` = 用户选了最高档反而完全不思考」。那个理由**已经不成立** ——
/// `effort_to_thinking_budget` 现在给 `max` 65536 的预算。改成同名映射更忠实：
/// Claude 的 `max` 与中枢的 `max` 描述同为「最高推理深度」。
///
/// 🔴 **刻意不映到 `ultra`**：那一档官方描述是 "Maximum reasoning with automatic task
/// delegation" —— 多了「自动任务委派」这个 Claude 档位里压根没有的语义。把顶档对顶档
/// 硬凑会给用户一个他没要求的行为。
///
/// 未知值返回 `None`（不落字段），而不是猜一个档位：猜错的方向是「用户设了 A、
/// 实际按 B 执行」，比不生效更难查。
fn claude_effort_to_hub(effort: &str) -> Option<&'static str> {
    match effort.to_ascii_lowercase().as_str() {
        "none" | "minimal" => Some("minimal"),
        "low" => Some("low"),
        "medium" => Some("medium"),
        "high" => Some("high"),
        "max" => Some("max"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// legacy 形态的回归：这一半本来就在工作，别在补新形态时把它弄坏。
    #[test]
    fn the_legacy_budget_form_still_maps_to_a_tier() {
        for (budget, want) in [(1024, "low"), (8192, "medium"), (16384, "high"), (32768, "xhigh")] {
            let body = json!({ "thinking": { "type": "enabled", "budget_tokens": budget } });
            assert_eq!(request_effort(&body), Some(want), "budget={budget}");
        }
    }

    /// 🔴 本模块存在的理由：这个形态此前**整个被丢掉**，用户设的档位静默失效。
    #[test]
    fn the_adaptive_form_is_recognized() {
        for (effort, want) in [("low", "low"), ("medium", "medium"), ("high", "high")] {
            let body = json!({
                "thinking": { "type": "adaptive" },
                "output_config": { "effort": effort },
            });
            assert_eq!(request_effort(&body), Some(want), "effort={effort}");
        }
    }

    /// 🔴 **Claude 的顶档 `max` 必须真的到达中枢并开出思考预算。**
    ///
    /// 这条测试 2026-09-07 改过一次，改的是**断言的方向**、不是它守的东西：
    /// 原先断言 `max` 必须被翻成 `xhigh`，理由是「中枢不认 `max`，原样传 = 静默不思考」。
    /// 中枢现在认了（`effort_to_thinking_budget` 给 65536），故改成同名映射。
    ///
    /// 它守的不变量始终是同一个：**用户选了最高档，就必须真的开出最高的预算** ——
    /// 不管中间那一跳叫什么名字。所以判据落在「预算真的开出来了、且比 high 更高」，
    /// 而不是落在某一个档位名上（钉名字的判据会在两套档位表变动时制造假红）。
    #[test]
    fn claude_max_reaches_the_hub_and_opens_a_real_budget() {
        let body = json!({
            "thinking": { "type": "adaptive" },
            "output_config": { "effort": "max" },
        });
        // 🔴 **必须精确等于 `max`**，不是「某个够高的档」。
        //
        // 第一版写成「预算 > high 就算过」，而 `xhigh`(32768) 与 `ultra` 都满足 ——
        // 于是把映射改回 `xhigh`（静默降级）或改成 `ultra`（多给了 task delegation）
        // 两种注入**都不红**（实测）。**钉性质而不钉值，前提是那个性质真的排他。**
        assert_eq!(
            request_effort(&body),
            Some("max"),
            "Claude 顶档必须落中枢同名档：xhigh 是静默降级，ultra 多了它没要求的自动任务委派"
        );

        // 端到端：那个档位名送进中枢，必须真的开出思考预算（而不是被 `_ => None` 吞掉）。
        let mut payload = json!({ "_pending_effort": "max" });
        super::super::convert::apply_pending_thinking(&mut payload, 400_000);
        let budget = payload["thinking"]["budget_tokens"]
            .as_u64()
            .unwrap_or_else(|| panic!("Claude 顶档必须开出思考预算，实际 payload={payload}"));
        let mut high = json!({ "_pending_effort": "high" });
        super::super::convert::apply_pending_thinking(&mut high, 400_000);
        assert!(
            budget > high["thinking"]["budget_tokens"].as_u64().unwrap(),
            "顶档预算必须高于 high：{budget}"
        );
    }

    /// `auto` 与 `adaptive` 同义（CLIProxyAPI 里是同一分支）。
    #[test]
    fn auto_is_treated_like_adaptive() {
        let body = json!({ "thinking": { "type": "auto" }, "output_config": { "effort": "high" } });
        assert_eq!(request_effort(&body), Some("high"));
    }

    /// 两者同时出现时数字赢：那是 Anthropic 上游实际会执行的那个。
    #[test]
    fn an_explicit_budget_wins_over_a_tier_name() {
        let body = json!({
            "thinking": { "type": "enabled", "budget_tokens": 32768 },
            "output_config": { "effort": "low" },
        });
        assert_eq!(request_effort(&body), Some("xhigh"), "budget 是确切值，优先");
    }

    /// 不该出档位的几种形态。猜一个档位比不落字段更糟（「用户设了 A、实际按 B 跑」）。
    #[test]
    fn shapes_that_must_not_produce_a_tier() {
        for body in [
            json!({}),
            // 没开思考
            json!({ "model": "claude-opus-4-5" }),
            // adaptive 但没给档位 → 交给模型默认，我们不猜
            json!({ "thinking": { "type": "adaptive" } }),
            // 未知档位名
            json!({ "thinking": { "type": "adaptive" }, "output_config": { "effort": "ultra" } }),
            // enabled 但没 budget（不合法的请求，上游会自己 400，我们不代它猜）
            json!({ "thinking": { "type": "enabled" } }),
            // output_config 里只有结构化输出，没有 effort
            json!({ "thinking": { "type": "adaptive" }, "output_config": { "format": { "type": "json_schema" } } }),
        ] {
            assert_eq!(request_effort(&body), None, "不该出档位：{body}");
        }
    }

    /// 🔴 **接线判据**：上面全部用例都直接调 `request_effort`，
    /// 而「`anthropic_to_openai` 到底有没有调它」是另一回事 ——
    /// 把那个调用点删掉，上面 6 条照样全绿。本仓已在同一类盲区上栽过十余次。
    #[test]
    fn the_anthropic_to_chat_conversion_must_carry_the_tier_over() {
        let body = json!({
            "model": "claude-opus-4-5",
            "max_tokens": 4096,
            "messages": [{ "role": "user", "content": "hi" }],
            "thinking": { "type": "adaptive" },
            "output_config": { "effort": "high" },
        });
        let chat = super::super::convert::anthropic_to_openai(&body);
        assert_eq!(
            chat.get("reasoning").and_then(|r| r.get("effort")).and_then(Value::as_str),
            Some("high"),
            "adaptive 档位没被带进中枢 —— 转发给 OpenAI 协议上游时会静默丢掉：{chat}"
        );
        // legacy 那一半同样要走通（同一个调用点）。
        let legacy = json!({
            "model": "claude-opus-4-5",
            "max_tokens": 4096,
            "messages": [{ "role": "user", "content": "hi" }],
            "thinking": { "type": "enabled", "budget_tokens": 16384 },
        });
        let chat = super::super::convert::anthropic_to_openai(&legacy);
        assert_eq!(
            chat.get("reasoning").and_then(|r| r.get("effort")).and_then(Value::as_str),
            Some("high"),
            "legacy 形态的回归：{chat}"
        );
    }
}

