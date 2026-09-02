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
/// 🔴 **两套档位的顶端名字不同**：Claude 的最高档叫 `max`，中枢（沿用 OpenAI 口径）
/// 叫 `xhigh`。原样传 `max` 过去的后果是静默的 —— `effort_to_thinking_budget`
/// 对未知档位走 `_ => return None`（「不擅自开思考」），也就是**用户选了最高档
/// 反而完全不思考**，正是本仓 `codex_catalog` 里刻意不声明 `max` 档的同一个坑。
///
/// 未知值返回 `None`（不落字段），而不是猜一个档位：猜错的方向是「用户设了 A、
/// 实际按 B 执行」，比不生效更难查。
fn claude_effort_to_hub(effort: &str) -> Option<&'static str> {
    match effort.to_ascii_lowercase().as_str() {
        "none" | "minimal" => Some("minimal"),
        "low" => Some("low"),
        "medium" => Some("medium"),
        "high" => Some("high"),
        "max" => Some("xhigh"),
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

    /// 🔴 两套档位的顶端名字不同：Claude 叫 `max`，中枢叫 `xhigh`。
    /// 原样传 `max` 的后果是 `effort_to_thinking_budget` 走 `_ => None`，
    /// 也就是**用户选了最高档反而完全不思考** —— 静默、且方向最坏。
    #[test]
    fn claude_max_becomes_xhigh_not_max() {
        let body = json!({
            "thinking": { "type": "adaptive" },
            "output_config": { "effort": "max" },
        });
        assert_eq!(request_effort(&body), Some("xhigh"), "max 必须落到中枢的最高档名");
        assert_ne!(request_effort(&body), Some("max"), "原样传 max = 静默不思考");
        // 反证这个坑真的存在：把 "max" 原样送进中枢，下游那一步压根不会开思考。
        let mut payload = json!({ "_pending_effort": "max" });
        super::super::convert::apply_pending_thinking(&mut payload, 64_000);
        assert!(
            payload.get("thinking").is_none(),
            "中枢不认 max —— 这就是为什么必须在这里翻成 xhigh：{payload}"
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

