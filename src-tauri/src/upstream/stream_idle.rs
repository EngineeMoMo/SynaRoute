//! 流式**静默超时**：两个数据块之间的最大间隔。
//!
//! # 为什么整体超时不够
//!
//! 我们此前只有 `client::key_timeout`（整个响应的总超时）。它管不住这一种形态：
//! **上游先回 200、SSE 流开起来，然后中途停住不再吐字节**。此时连接还活着、没有错误，
//! 于是我们干等到总超时或对端网关掐断为止。
//!
//! 真实案例（用户 2026-08-29 的日志）：`Anthropic HTTP 524`，耗时 **259 秒** ——
//! Cloudflare 网关等不到源站响应先掐断，我们才拿到一个 524。那 259 秒里我们既没有
//! 切换 Key、也没有告诉用户任何事，而**流早就不动了**。
//!
//! cc-switch 把流式超时拆成两个参数（`src/components/proxy/AutoFailoverConfigPanel.tsx`
//! 那张表：「流式首字节超时 90s」+「流式静默超时 180s，数据块之间的最大间隔，填 0 禁用
//! （防止中途卡住）」）。本模块抄的是第二个 —— 它才是治「中途卡死」的那一条。
//!
//! # 🔴 超时了为什么要**注入一个 SSE error 事件**，而不是直接结束流
//!
//! 直接 `return None` 结束流，下游看到的是「流正常结束」：
//! `log_success` 会坐实成功、清 `fail_count`、解除短路窗口 —— 那正是 `sse_stream_errored`
//! 那条注释里记的「200 后流内失败若不识别，会使熔断/短路在流式主路径上零保护」。
//! 静默把一次失败记成成功，是本仓最忌讳的方向。
//!
//! 注入 `event: error` 之后，**现成的两条路径同时接管**：
//! - [`super::sse_stream_errored`] 在流末窗口里认出它 → `record_live_failure`，熔断照常记账；
//! - 客户端（Claude Code / Codex）本就要处理 Anthropic 的 `error` 事件 → 用户看到的是
//!   一句明确的报错，而不是一个悄悄截断的回答。
//!
//! 也就是说这个模块零新增契约：它把「静默卡死」翻译成一种**系统里已经会正确处理**的失败。
//!
//! # 那句文案到得了用户眼前吗
//!
//! 1. **同协议 Anthropic 直通**：原样透传，客户端按 SSE 解析，能看到。
//! 2. **跨协议**（Codex→Anthropic Key、Claude Code→OpenAI Key）：走 `SseTranslator`，
//!    而它此前**认不出任何 error 事件**、会把这条整个丢掉 —— 那时注入只承担「让流末判成
//!    失败」这一半（熔断记账仍然正确），下游看到的是「流意外结束」。
//!    ✅ 这一半已由 [`super::sse_error`] 补上：本模块注入的就是一条 Anthropic 形态的
//!    `error` 事件，它走的正是那条新翻译路径，六个方向各自转成下游协议自己的错误形状。
//!    **改那个模块时别忘了这里是它的第二个上游**（另一个是上游自己在流内报的错）。
//!
//! ⚠️ 仍有一种形态到不了：上游返的**压根不是 SSE**（少数中转站对流式请求回 JSON，或带
//! gzip），这段明文不会被按 SSE 渲染。此时**响应体本来就已经截断**，故危害限于「提示失效」，
//! 不是新的损坏。
//!
//! # ⚠️ 记账口径：静默超时按 **Key 级**罚
//!
//! 走 `record_live_failure`（同 `failure_scope` 里 401 那类硬错误），而上游自己回 502/524
//! 是**不罚**的（不为上游抖动惩罚好 Key）。同一场网关故障，谁先到就决定了罚不罚 ——
//! 这是刻意取舍：200 之后已无故障转移余地（首字节已发出），熔断是此时唯一的保护手段。
//! 代价是链路慢的 Key 连撞三次（每次 180s）会被停用 60s，用户看到「好 Key 被停用」。
//! 要改成不罚，先想清楚「流卡死却什么都不记」会让熔断在流式主路径上再次归零。
//!
//! # 阈值
//!
//! 180 秒，与 cc-switch 的默认值一致。正常流不会撞上它：Anthropic 思考期间会持续吐
//! `thinking_delta`、OpenAI 会吐 keep-alive 注释行，真正 180 秒一个字节都不来的只有
//! 连接层已经坏掉的情形。**刻意不做成可配**，也**不给禁用开关** —— 这个数字要么远大于
//! 正常间隔（那就没人需要调），要么调小了会误杀正常的长思考。cc-switch 那张表把首字节
//! （90s）与静默（180s）分成两档并允许填 0 禁用；我们把两者**合并成这一个** 180s，
//! 即首字节也吃它。若真有用户报「长思考被中断」，判据是先看日志里有没有那条流内 error。

use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use std::time::Duration;

/// 两个数据块之间的最大间隔。见模块头「阈值」一节。
pub(crate) const IDLE_LIMIT: Duration = Duration::from_secs(180);

/// 超时时注入的 SSE 事件。形态与 Anthropic 的流内错误一致，故
/// [`super::sse_stream_errored`] 与客户端都不需要为它加任何特例。
fn idle_error_event(idle: Duration) -> Bytes {
    let secs = idle.as_secs();
    Bytes::from(format!(
        "event: error\ndata: {{\"type\":\"error\",\"error\":{{\"type\":\"timeout_error\",\
         \"message\":\"上游流已静默 {secs} 秒未发送任何数据，SynaRoute 判定连接卡死并中断本次转发。\
         常见成因：中转站网关或源站掉线（此时上游往往随后才回 502/524）。\"}}}}\n\n"
    ))
}

/// 给上游字节流套上静默超时。
///
/// 上游正常结束 → 原样结束；某一块等超过 [`IDLE_LIMIT`] → 产出一个 SSE `error` 事件后结束。
/// **不改 Item 类型**，故两个调用点（同协议直通、跨协议翻译）都是一行换一行。
/// 返回 `Pin<Box<dyn Stream>>` 而不是 `impl Stream`：`unfold` 产出的类型不是 `Unpin`，
/// 而 proxy.rs 的跨协议翻译流会对它直接 `.next().await`（要求 Unpin）。装箱是这里唯一
/// 一次堆分配，发生在每个流开始时、不在数据路径上。
pub(crate) fn guard<S>(
    stream: S,
) -> std::pin::Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send + Sync>>
where
    S: Stream<Item = Result<Bytes, reqwest::Error>> + Send + Sync + 'static,
{
    Box::pin(guard_with(stream, IDLE_LIMIT))
}

/// [`guard`] 的可注参数版本（测试用短超时，生产走常量）。
fn guard_with<S>(stream: S, idle: Duration) -> impl Stream<Item = Result<Bytes, reqwest::Error>>
where
    S: Stream<Item = Result<Bytes, reqwest::Error>> + Send + Sync + 'static,
{
    // 用 `stream::unfold` 承载状态机，与 proxy.rs 的翻译流同一手法（不引 async_stream 依赖）。
    enum St<S> {
        Live(std::pin::Pin<Box<S>>),
        /// 已注入超时事件：再被 poll 一次就结束，不会重复注入。
        Done,
    }
    futures_util::stream::unfold(St::Live(Box::pin(stream)), move |st| async move {
        match st {
            St::Live(mut s) => match tokio::time::timeout(idle, s.next()).await {
                Ok(Some(item)) => Some((item, St::Live(s))),
                Ok(None) => None,
                Err(_) => Some((Ok(idle_error_event(idle)), St::Done)),
            },
            St::Done => None,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 🔴 静默超时必须注入一个**现有失败识别路径认得出**的错误事件。
    ///
    /// 直接结束流的后果是 `log_success` 坐实成功、清 `fail_count`、解除短路窗口 ——
    /// 把一次失败静默记成成功，正是 `sse_stream_errored` 那条注释里记的
    /// 「200 后流内失败若不识别，熔断/短路在流式主路径上零保护」。
    #[tokio::test]
    async fn a_stalled_stream_gets_an_error_event_the_breaker_can_see() {
        let live: Vec<Result<Bytes, reqwest::Error>> = vec![Ok(Bytes::from("chunk1"))];
        let stalled = futures_util::stream::iter(live).chain(futures_util::stream::pending());
        let mut g = Box::pin(guard_with(stalled, Duration::from_millis(50)));

        // 正常块原样透传
        let first = g.next().await.expect("第一块该到").expect("不该是错误");
        assert_eq!(first, Bytes::from("chunk1"));

        // 之后上游静默 → 注入
        let injected = g.next().await.expect("静默后该注入一块").expect("注入的是 Ok");
        let text = String::from_utf8_lossy(&injected);
        assert!(text.starts_with("event: error"), "形态要与 Anthropic 流内错误一致：{text}");
        assert!(
            crate::upstream::sse_stream_errored(&text),
            "🔴 必须能被现有的「流内失败」识别路径认出，否则这次超时会被记成成功：{text}"
        );
        // 文案要带上静默秒数（用 50ms 超时 → as_secs() 为 0，故这里断言的是那个真实数字，
        // 不是「随便含个数字就算过」——第一版写成 `contains("50") || contains('0')`，
        // 那是个恒真的断言：任何秒数的十进制里都可能有 0，而 50ms 压根不会出现「50」。
        assert!(
            text.contains(&format!("静默 {} 秒", Duration::from_millis(50).as_secs())),
            "文案要如实带上静默秒数：{text}"
        );

        // 再 poll 一次就结束，不重复注入
        assert!(g.next().await.is_none(), "注入后应立即结束，不许重复注入");
    }

    /// 上游正常结束 → 一个字节都不加。
    #[tokio::test]
    async fn a_healthy_stream_passes_through_untouched() {
        let live: Vec<Result<Bytes, reqwest::Error>> =
            vec![Ok(Bytes::from("a")), Ok(Bytes::from("b"))];
        let mut g = Box::pin(guard_with(
            futures_util::stream::iter(live),
            Duration::from_secs(30),
        ));
        assert_eq!(g.next().await.unwrap().unwrap(), Bytes::from("a"));
        assert_eq!(g.next().await.unwrap().unwrap(), Bytes::from("b"));
        assert!(g.next().await.is_none(), "正常结束不该被加料");
    }

    /// 🔴 接线判据：`proxy.rs` 的**两个**流式出口都必须经过本模块。
    ///
    /// 上面两条只测本模块自己 —— 把 proxy.rs 那两行改回 `resp.bytes_stream()`
    /// 它们照样全绿，而那正是「流中途卡死无人管」这个缺陷本身。
    /// 这是本仓第 9 次盯同一类接线盲区。
    #[test]
    fn both_streaming_exits_must_go_through_the_idle_guard() {
        let src = std::fs::read_to_string("src/proxy.rs").unwrap();
        let prod = crate::proxy::custom_headers::production_code_only(&src);
        assert_eq!(
            prod.matches("guard_stream_idle(resp.bytes_stream())").count(),
            2,
            "同协议直通与跨协议翻译两条流式路径都要套静默超时"
        );
        assert!(
            !prod.contains("let upstream = resp.bytes_stream();"),
            "不许绕过静默超时直接用裸字节流"
        );
    }
}

