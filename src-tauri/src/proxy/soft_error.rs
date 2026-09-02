//! **非流式**「HTTP 200 但响应体是一个错误」的降级。
//!
//! 挂在 [`crate::proxy`] 下（`#[path]`）—— `proxy.rs` 棘轮余量为 0。
//! 同 `lan_guard.rs` / `custom_headers.rs` / `model_choice.rs` 的挂法。
//!
//! # 它补的洞
//!
//! 流式那半早就修过了（`sse_error.rs` + `proxy.rs` 流末那道 `saw_upstream_error`
//! 更宽的门 + `health::record_stream_end`）。**非流式这半一直开着**：
//! `proxy.rs` 的 `match result` 只看 `outcome.ok`（即「是不是 2xx」），
//! 于是上游回 `200 {"error":{...}}` 时会走成功分支 ——
//! `record_live_success` 坐实成功、`fail_count` 减半、`clear_all_failed_gate`
//! 解除短路窗口、日志页记一条绿色「路由」，**而客户端拿到的是一个错误**。
//!
//! 失效方向与 `stream_idle` 那条「超时了不能直接 return None」一字不差：
//! 把一次失败静默记成成功。而这一半更糟一点 —— 非流式我们**还有故障转移余地**
//! （响应体已完整拿到、还没发给下游），本可以切下一个候选却没切。
//!
//! 形态取证来自 ccLoad（它把「HTTP 200 with error content」列为五大痛点之一）：
//! `{"error":{...}}`、`{"type":"error",...}`、SSE error 里的限流码。
//! 前两种是本模块的范围，第三种早已由 `sse_error.rs` 覆盖。
//!
//! # 🔴 判据不许在这里重写一份
//!
//! 「这个非流式响应体是不是上游报错」的唯一实现是
//! [`crate::upstream::body_is_upstream_error`]（它自己又建立在
//! `upstream_error_message` 这个三形态判据之上）。**大脑聚合那条链路
//! （`session.rs`）复用同一个函数** —— 只堵转发路径就是同一个洞只堵一半，
//! 而本仓对这种形态记过：「修了 A→B，同一个坑几乎必然也在 B→A」。
//!
//! # 🔴 为什么降级成 502 而不是留着 200
//!
//! 只把 `ok` 改成 false 而留着 `status = 200`，整条既有失败链路会拿 200 去问
//! `status_counts_against_breaker` / `all_failed_is_hard_error` —— 200 既不是 4xx
//! 也不是 5xx，落在那两个判据的**定义域之外**，行为是「碰巧的」而不是设计过的。
//!
//! 降成 502 让它精确落进既有的「5xx：临时性、**不罚 Key**、带 Retry-After 语义、
//! 可重试」那一支，而 502 Bad Gateway 的语义本身就是准确的：
//! 上游给了一个我们无法当作成功的响应。
//!
//! # 🔴 为什么刻意**不**计熔断
//!
//! `status_counts_against_breaker(502)` 为假，这是**选定**的结果不是副作用。
//! 本模块的判据读的是上游自己拼的自由文本 JSON，启发式的东西必然有误判；
//! 而两种错法的代价不对称 —— 不罚的代价是「这条 Key 排在前面、每次白耗一次往返」
//! （而故障转移已经让用户拿到了正确回答），罚错的代价是屏蔽一条**好** Key 60 秒。
//! 同 `thinking_rectify` 那条「判据脆但失效方向是退回现状」。
//!
//! # 🔴 为什么必须落一条事件
//!
//! 降级之后日志页看到的是 502，而**上游明明回的是 200** —— 不落事件的话，
//! 排障的人看到的是假现场（本仓最在意的那一类）。同 `record_stream_end` 的处置：
//! 那条也是「不去改已经写出去的那一行，而是**追加**一条 error 级可折叠事件」。
//!
//! # 已知边界（写明免得被当 bug 重查）
//!
//! **「上游返的压根不是 JSON」这一形态刻意不做。** ccLoad 还认纯文本的
//! 「当前模型负载过高」，而按内容匹配在这里不安全：用户完全可能在问
//! 「我的服务器负载过高怎么办」，模型的正常回答里就含那几个字。
//! 按「解析不出 JSON 就算错误」判则要先确认压缩路径（`reqwest` 的
//! gzip/brotli 自动解码是否在所有中转站形态下都生效），本轮没有那份取证。

use super::ForwardOutcome;
use crate::model::{CategoryType, ProviderKey};
use crate::store::Store;
use std::sync::Arc;

/// 软错误降级后对外呈现的状态码。见模块头「为什么降级成 502」。
const DEMOTED_STATUS: u16 = 502;

/// 命中时做两件事，缺一不可：落一条 error 级可折叠事件（否则日志里那个 502
/// 是假现场），以及把 `ok`/`status` 改成失败口径（否则调用方会记成功）。
///
/// 非软错误（含所有已经是非 2xx 的 outcome）**原样返回**，零行为变化。
pub(super) fn demote(
    store: &Arc<Store>,
    category: CategoryType,
    key: &ProviderKey,
    outcome: ForwardOutcome,
) -> ForwardOutcome {
    if !outcome.ok {
        return outcome;
    }
    let Some(msg) = crate::upstream::body_is_upstream_error(&outcome.bytes) else {
        return outcome;
    };
    let name = store.key_name(&key.id).unwrap_or_else(|| key.id.clone());
    store.append_event_collapsible(
        category,
        "error",
        Some(&key.id),
        &format!(
            "{name} · {} · 上游回了 HTTP {}，但响应体是一个错误：{msg}\n\
             已按失败处置并切换下一个候选。日志里那条 HTTP {DEMOTED_STATUS} 是我们降级后的\
             口径，**不是**上游给的状态码。本次不计入熔断（判据读的是上游自由文本，\
             宁可少罚也不误伤好 Key）。",
            outcome.real_model, outcome.status,
        ),
        None,
        Some(format!("softerr:{}:{}", key.id, outcome.real_model)),
    );
    ForwardOutcome { ok: false, status: DEMOTED_STATUS, ..outcome }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Protocol;
    use bytes::Bytes;
    use std::path::PathBuf;

    /// 唯一临时目录（同 `balance_gate::tests::temp_dir`：pid + 进程内自增 —— 本机
    /// `timestamp_nanos` 量化粒度只有 100ns，单靠时间戳并发下会撞名）。
    fn temp_dir(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir()
            .join(format!("synaroute_test_{}_{}_{}", tag, std::process::id(), seq));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn store(tag: &str) -> Arc<Store> {
        let dir = temp_dir(tag);
        Arc::new(Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap())
    }

    fn key() -> ProviderKey {
        ProviderKey {
            id: "k1".into(),
            category_id: CategoryType::ClaudeCli,
            name: "测试站".into(),
            base_url: "https://api.example.com".into(),
            protocol: Protocol::Anthropic,
            enabled: true,
            ..Default::default()
        }
    }

    /// 一个 2xx 的 outcome。刻意从字面构造全部字段：`ForwardOutcome` 没有 `Default`，
    /// 日后加字段这里编译不过 —— 那正是我们想要的（新字段要过一遍降级语义）。
    fn ok_outcome(body: &str) -> ForwardOutcome {
        ForwardOutcome {
            bytes: Bytes::from(body.to_string()),
            url: "https://api.example.com/v1/messages".into(),
            real_model: "claude-opus-4-5".into(),
            request_body: String::new(),
            status: 200,
            ok: true,
            retry_after: None,
        }
    }

    fn demoted(body: &str) -> (Arc<Store>, ForwardOutcome) {
        let s = store("softerr");
        let out = demote(&s, CategoryType::ClaudeCli, &key(), ok_outcome(body));
        (s, out)
    }

    /// 🔴 本模块存在的理由：这些形态此前全部被记成**成功**。
    #[test]
    fn the_error_shapes_hiding_behind_a_200_are_demoted() {
        for body in [
            // Chat / 通用：顶层 error 对象
            r#"{"error":{"message":"当前分组上游负载已饱和","type":"server_error"}}"#,
            // Anthropic 明确错误形态
            r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
            // Responses 的失败形态
            r#"{"type":"response.failed","response":{"error":{"message":"upstream refused"}}}"#,
            // 有些中转站把 error 直接写成字符串
            r#"{"error":"insufficient quota"}"#,
        ] {
            let (_s, out) = demoted(body);
            assert!(!out.ok, "这个响应体必须被判成失败：{body}");
            assert_eq!(out.status, DEMOTED_STATUS, "降级口径必须是 502：{body}");
        }
    }

    /// 假阳性防线。误判的代价是把一次**完整的成功回答**扔掉并白切一次 Key，
    /// 所以正常响应一个都不许命中。
    #[test]
    fn normal_responses_are_never_demoted() {
        for body in [
            // Anthropic Messages 正常回答
            r#"{"id":"msg_1","type":"message","role":"assistant","content":[{"type":"text","text":"好"}]}"#,
            // OpenAI Chat 正常回答
            r#"{"id":"c1","object":"chat.completion","choices":[{"message":{"content":"hi"}}]}"#,
            // 🔴 Responses 的响应结构里**本来就有** error 字段，正常时是 null
            r#"{"id":"r1","object":"response","status":"completed","error":null,"output":[]}"#,
            // 🔴 空字符串占位：`is_string()` 对它为真，故 SSE 那侧的 null 防线挡不住
            r#"{"id":"r2","object":"response","error":"","output":[]}"#,
            // 模型正常回答里**讨论**错误，不是上游报错（顶层没有 error）
            r#"{"type":"message","content":[{"type":"text","text":"你的 error 字段应该这样写"}]}"#,
            // 顶层不是对象 → 压根不是补全响应，不在这里下判断
            r#"[{"error":{"message":"x"}}]"#,
            // 解析不出 JSON（纯文本形态刻意不做，见模块头「已知边界」）
            "当前模型负载过高，请稍后重试",
        ] {
            let (s, out) = demoted(body);
            assert!(out.ok, "正常响应被误判成软错误了：{body}");
            assert_eq!(out.status, 200, "正常响应的状态码不许被改：{body}");
            assert!(
                s.list_all_events().is_empty(),
                "没命中就不该落事件（会把有用事件挤出 MAX_EVENTS 环）：{body}"
            );
        }
    }

    /// `error: ""` 与 `type:"error"` 同时出现时仍然是错误 —— 那是 Anthropic 的
    /// 明确错误声明，空 message 不改变它是个错误这件事。
    #[test]
    fn an_empty_error_string_still_counts_when_the_type_says_error() {
        let (_s, out) = demoted(r#"{"type":"error","error":""}"#);
        assert!(!out.ok, "type==error 是明确声明，不该被空串防线放过");
    }

    /// 🔴 降级会让日志页显示 502，而上游明明回的是 200。不落这条事件，排障的人
    /// 看到的就是**假现场**（本仓最在意的那一类）。
    #[test]
    fn the_demotion_leaves_a_trace_that_names_the_real_status() {
        let (s, _out) = demoted(r#"{"error":{"message":"负载已饱和"}}"#);
        let ev = s.list_all_events();
        assert_eq!(ev.len(), 1, "必须留痕，否则 502 是个无从解释的假现场");
        assert_eq!(ev[0].kind, "error", "落在错误分组，用户才会去看它");
        assert!(ev[0].detail.contains("HTTP 200"), "必须写明上游给的是 200：{}", ev[0].detail);
        assert!(ev[0].detail.contains("负载已饱和"), "必须带上游原话：{}", ev[0].detail);
        assert!(
            ev[0].detail.contains("不计入熔断"),
            "必须说明记账口径，否则「为什么这条 Key 没被熔断」查不出来：{}",
            ev[0].detail
        );
    }

    /// 非 2xx 原样返回：那条路已经在走失败链路，再落一条事件是重复留痕。
    #[test]
    fn a_non_2xx_outcome_is_left_untouched() {
        let s = store("softerr_pass");
        let raw = ForwardOutcome { status: 429, ok: false, ..ok_outcome(r#"{"error":{"message":"rate limited"}}"#) };
        let out = demote(&s, CategoryType::ClaudeCli, &key(), raw);
        assert_eq!(out.status, 429, "已经是失败的 outcome 不许被改成 502");
        assert!(s.list_all_events().is_empty(), "非 2xx 不该由本模块再落一条");
    }

    /// 🔴 **接线判据**：上面那些用例全都直接调 `demote`，把 `proxy.rs` 那行改回
    /// 裸 `match result` 它们照样全绿 —— 而那正是缺陷本体。本仓已在同一类盲区上
    /// 栽过十余次（`route_meta` 的出口、`lan_guard` 的 peer、`log_rotate` 的写线程…）。
    ///
    /// 同时钉住**顺序**：降级必须排在 `Ok(outcome) if outcome.ok` 那道守卫之前。
    /// 排在后面等于没排 —— 那时 `record_live_success` 已经执行过了。
    #[test]
    fn the_forwarding_path_must_demote_before_the_success_guard() {
        let src = include_str!("../proxy.rs");
        let prod = crate::proxy::custom_headers::production_code_only(src);
        let calls = prod.matches("soft_error::demote(").count();
        assert_eq!(calls, 1, "非流式转发路径上必须恰好有一处降级调用，实际 {calls} 处");
        let at_demote = prod.find("soft_error::demote(").expect("上面刚断言过存在");
        let at_guard = prod
            .find("Ok(outcome) if outcome.ok")
            .expect("非流式成功分支的守卫形态变了，本判据要跟着改");
        assert!(
            at_demote < at_guard,
            "降级必须排在成功守卫之前（demote@{at_demote} vs guard@{at_guard}）——\
             排在后面时 record_live_success 已经把这次失败记成成功了"
        );
    }
}



