//! `BrainConfig` 落盘前的校验。
//!
//! # 为什么校验必须在保存时，而不是运行时
//!
//! 聚合的一轮要几十秒到几分钟，而且每个成员都是一次**付费**上游调用。配置里的错误
//! （引用写坏、成员配重了）此前一律等到运行时才暴露：用户点下去、等一分钟、拿到一句
//! 「无效引用: xxx」—— 而那句话在保存的那一刻就能说。
//!
//! # 校验哪些、不校验哪些
//!
//! 只挡「一定是错的」和「一定会造成损失的」。**不挡**取值偏好 —— 并发上限、
//! 工具轮数、结果字符上限这些在运行时都有 clamp，用户填个夸张的数字不会坏事，
//! 挡下来只是摩擦。
//!
//! 🔴 **只在保存路径校验，读取路径一个字都不检查。** 老用户配置里可能已经有本模块
//! 认为不合法的组合（比如两个一模一样的成员）；读取时也拒的话，升级一次就让他的
//! 大脑聚合彻底不可用，而他并没有做错任何事。保存时挡住 = 新的错配进不来，
//! 旧的照旧能跑（运行时行为与升级前完全一致）。

use crate::error::{AppError, AppResult};
use crate::model::BrainConfig;

/// 成员数上限。每个成员都是一次并发的付费上游调用，而 prompt 里嵌着全部检索文件 ——
/// 一轮的成本大致是「成员数 × 上下文长度」。16 已经远超「多专家会诊」的实际需要，
/// 到这个量级更可能是误操作（比如一键把某个 Key 的所有模型都加进来）。
const MAX_MEMBERS: usize = 16;

/// 整轮预算的下限：低于这个数连一次决策者调用都跑不完，而
/// `aggregate_phase::decider_floor_ms` 还要从里面切 35% 给决策者。
const MIN_TOTAL_TIMEOUT_MS: u64 = 10_000;

/// 整轮预算的上限（30 分钟）。**不是**为了对齐 MCP 客户端超时 —— 那个已经跟着这个值
/// 动态涨（见 `crate::service::mcp_client_timeout_ms`，取各分类最大值 + 30s 余量）。
/// 这条纯粹防误输入：把 60000 打成 6000000 的话，一次卡死的聚合会占着闸门 100 分钟，
/// 而用户只会以为「应用坏了」。
const MAX_TOTAL_TIMEOUT_MS: u64 = 30 * 60 * 1_000;

/// 注入文件内容的 token 上限。再大也没有意义 —— 上游窗口装不下，
/// 而 `output_budget` 会因为「输入占满窗口」直接让成员失败。
const MAX_CONTEXT_TOKENS: u32 = 1_000_000;

/// 校验一份即将落盘的 brain 配置。`Err` 的文案直接给用户看，必须说清**改哪里**。
pub(crate) fn validate(brain: &BrainConfig) -> AppResult<()> {
    let bad = |msg: String| AppError::Invalid(msg);

    // ① 引用形态。`call_ref` 在运行时按 `::` 切分，切不出来就整轮失败 ——
    //    那时用户已经等了几十秒，而这个错在保存的那一刻就看得见。
    for (what, r) in [
        ("最终决策者", brain.decider_ref.as_ref()),
        ("汇总模型", brain.summarizer_ref.as_ref()),
    ] {
        let Some(r) = r.filter(|s| !s.trim().is_empty()) else {
            continue; // 未配置是合法的（决策者在运行时另有必填校验，汇总者会回落决策者）
        };
        match r.split_once("::") {
            Some((key_id, model)) if !key_id.trim().is_empty() && !model.trim().is_empty() => {}
            _ => {
                return Err(bad(format!(
                    "「{what}」的引用格式不对（当前是 `{r}`），应为 `KeyId::模型名`。\
                     请在下拉里重新选一次。"
                )))
            }
        }
    }

    // ② 成员数上限。
    if brain.members.len() > MAX_MEMBERS {
        return Err(bad(format!(
            "参与者最多 {MAX_MEMBERS} 位，当前配了 {}。每位参与者都是一次并发的付费调用，\
             而每次请求都嵌着全部检索到的文件内容 —— 再多只是成倍烧额度。",
            brain.members.len()
        )));
    }

    // ③ 成员不许重复。
    //
    // 🔴 危害不只是「浪费一次调用」：成员的日志标签是 `Key名 / 模型名`，两个一模一样的
    // 成员会得到**完全相同的标签**，于是运行日志里分不清哪条属于谁，而工具调用事件的
    // 折叠键 `tool:{label}:{工具名}` 也会把两位成员的记录合并成一行。
    // 用户配重复通常是想要「两份独立意见」，但同 Key 同模型给出的是同一个模型的两次采样。
    let mut seen = std::collections::HashSet::new();
    for m in &brain.members {
        if !seen.insert((m.key_id.as_str(), m.model_name.as_str())) {
            return Err(bad(format!(
                "参与者里有重复项：同一条 Key 上的模型「{}」被添加了两次。\
                 同 Key 同模型只会得到同一个模型的两次采样，而两条记录在运行日志里\
                 标签完全相同、无法分辨。请删掉重复的那一位，或换成别的模型。",
                m.model_name
            )));
        }
    }

    // ④ 整轮预算范围。
    if brain.total_timeout_ms < MIN_TOTAL_TIMEOUT_MS {
        return Err(bad(format!(
            "总超时至少 {} 秒（当前 {} 毫秒）。再小连决策者一次调用都跑不完，\
             每一轮都会以超时收场。",
            MIN_TOTAL_TIMEOUT_MS / 1000,
            brain.total_timeout_ms
        )));
    }
    if brain.total_timeout_ms > MAX_TOTAL_TIMEOUT_MS {
        return Err(bad(format!(
            "总超时最多 {} 分钟（当前 {} 毫秒，约 {} 分钟）—— 这个数字通常是多打了一位 0。\
             MCP 客户端的超时会跟着它一起涨，填错会让一次卡住的聚合把客户端也拖住那么久。",
            MAX_TOTAL_TIMEOUT_MS / 60_000,
            brain.total_timeout_ms,
            brain.total_timeout_ms / 60_000
        )));
    }

    // ⑤ 上下文 token 上限。
    if brain.max_context_tokens > MAX_CONTEXT_TOKENS {
        return Err(bad(format!(
            "注入文件的 token 上限最多 {MAX_CONTEXT_TOKENS}（当前 {}）。\
             超过上游窗口的部分不会被接受，只会让每位参与者都因「输入占满上下文」而失败。",
            brain.max_context_tokens
        )));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AggregateMode, BrainMember, CategoryType};

    fn base() -> BrainConfig {
        BrainConfig {
            category_id: CategoryType::ClaudeCli,
            enabled: true,
            aggregate_mode: AggregateMode::Compressed,
            concurrency_limit: 3,
            total_timeout_ms: 60_000,
            summarizer_ref: None,
            decider_ref: Some("k1::claude-opus-5".into()),
            members: vec![],
            work_dir: None,
            max_context_tokens: 50_000,
            retrieval_enabled: false,
            auto_follow_active: false,
            tools_enabled: false,
            max_tool_rounds: 6,
            tool_ctx_budget_chars: 60_000,
            tool_result_cap_chars: 8_000,
        }
    }

    fn member(key: &str, model: &str) -> BrainMember {
        BrainMember {
            id: format!("{key}-{model}"),
            key_id: key.into(),
            model_name: model.into(),
        }
    }

    #[test]
    fn a_normal_config_passes() {
        let mut b = base();
        b.members = vec![member("k1", "claude-opus-5"), member("k2", "glm-5")];
        b.summarizer_ref = Some("k2::glm-5".into());
        validate(&b).expect("正常配置不该被拒");
    }

    /// 引用形态：`call_ref` 运行时按 `::` 切，切不出来整轮失败 ——
    /// 而那时用户已经等了几十秒。这个错在保存那一刻就看得见。
    #[test]
    fn a_malformed_reference_is_rejected_at_save_time() {
        for bad_ref in ["k1", "k1:claude", "::claude-opus-5", "k1::", "k1::   "] {
            let mut b = base();
            b.decider_ref = Some(bad_ref.into());
            let e = validate(&b).expect_err(&format!("{bad_ref:?} 应被拒"));
            assert!(
                format!("{e}").contains("KeyId::模型名"),
                "报错要给出正确格式：{e}"
            );
        }
        // 汇总模型走同一道（它是可选的，但填了就得对）。
        let mut b = base();
        b.summarizer_ref = Some("oops".into());
        assert!(validate(&b).is_err());
    }

    /// 未配置 / 空串是合法的：决策者在运行时另有必填校验，汇总者会回落到决策者。
    /// 保存路径不许替它们把「还没配完」判成错 —— 用户是**边配边存**的。
    #[test]
    fn an_absent_reference_is_not_an_error() {
        let mut b = base();
        b.decider_ref = None;
        b.summarizer_ref = None;
        validate(&b).expect("没配引用应能保存");
        b.decider_ref = Some(String::new());
        b.summarizer_ref = Some("   ".into());
        validate(&b).expect("空串按未配置处理");
    }

    /// 🔴 重复成员的危害不是「多花一次钱」，而是**日志分不清谁是谁**：
    /// 标签是 `Key名 / 模型名`，两个一样的成员标签逐字相同，工具事件的折叠键也会把
    /// 两位的记录合并成一行。
    #[test]
    fn duplicate_members_are_rejected_but_same_model_on_another_key_is_fine() {
        let mut b = base();
        b.members = vec![member("k1", "glm-5"), member("k1", "glm-5")];
        let e = validate(&b).expect_err("同 Key 同模型配两次应被拒");
        assert!(format!("{e}").contains("重复"), "{e}");

        // 反面：**不同 Key 上的同一个模型是合法的**（两家中转站的同名模型，
        // 权重与限流都不同，是「多专家」的正当用法）。判据必须按 (key, model) 而不是只看模型名。
        b.members = vec![member("k1", "glm-5"), member("k2", "glm-5")];
        validate(&b).expect("不同 Key 上的同名模型应放行");

        // 同一条 Key 上的不同模型同样合法。
        b.members = vec![member("k1", "glm-5"), member("k1", "glm-5-air")];
        validate(&b).expect("同 Key 不同模型应放行");
    }

    #[test]
    fn too_many_members_is_rejected() {
        let mut b = base();
        b.members = (0..=MAX_MEMBERS).map(|i| member("k1", &format!("m{i}"))).collect();
        let e = validate(&b).expect_err("超上限应被拒");
        assert!(format!("{e}").contains("最多"), "{e}");

        // 边界：正好 MAX_MEMBERS 位应通过（别把上限写成 `>=`）。
        let mut ok = base();
        ok.members = (0..MAX_MEMBERS).map(|i| member("k1", &format!("m{i}"))).collect();
        validate(&ok).expect("正好到上限应放行");
    }

    #[test]
    fn the_round_budget_must_stay_in_a_workable_range() {
        let mut b = base();
        b.total_timeout_ms = 5_000;
        assert!(validate(&b).is_err(), "低于下限应被拒");
        b.total_timeout_ms = MIN_TOTAL_TIMEOUT_MS;
        validate(&b).expect("正好到下限应放行");

        b.total_timeout_ms = 6_000_000; // 多打了一位 0
        let e = validate(&b).expect_err("超上限应被拒");
        assert!(format!("{e}").contains("多打了一位 0"), "报错要点出最可能的原因：{e}");
        b.total_timeout_ms = MAX_TOTAL_TIMEOUT_MS;
        validate(&b).expect("正好到上限应放行");
    }

    #[test]
    fn an_absurd_context_budget_is_rejected() {
        let mut b = base();
        b.max_context_tokens = MAX_CONTEXT_TOKENS + 1;
        assert!(validate(&b).is_err());
        b.max_context_tokens = MAX_CONTEXT_TOKENS;
        validate(&b).expect("正好到上限应放行");
    }

    /// 源码级接线判据：**校验必须挂在唯一落盘点上**。
    ///
    /// 上面那些用例全是直调 `validate`，把 `save_brain` 里那一行删掉它们照样全绿 ——
    /// 而那正是缺陷本体（配置照旧能存进去，错误照旧等到运行时才报）。
    /// 这是本仓第 16 次盯同一类接线盲区。
    #[test]
    fn save_brain_must_call_validate() {
        let store_src =
            crate::proxy::custom_headers::production_code_only(include_str!("store.rs"));
        assert!(
            store_src.contains("brain_config::validate(&brain)?"),
            "save_brain 必须先过校验 —— 它是 brain 配置唯一的落盘点"
        );
    }
}
