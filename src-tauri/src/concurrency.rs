//! 每 Key 并发上限：弹性的**第五层**。
//!
//! # 它治什么
//!
//! 前四层都是「出了事之后」：熔断（Key 级失败计数）、模型锁（某模型 404）、
//! 配额窗口（上游给了 `Retry-After`）、余额闸门（确定耗尽）。它们的共同前提是
//! **先失败一次**。而这一层管的是「别把上游打到失败」——
//!
//! Claude Code 一次会话会同时发起多路请求（主对话 + 杂活档的 haiku 调用 + 标题生成），
//! Codex 的工具循环也会并发。中转站按并发计费/限流的相当多，撞上去的表现是 429 或
//! `bad_response_status_code`，而 **429 刻意不计熔断**（那条规则是对的）→ 我们只会一次次
//! 白撞。配额窗口能兜住「上游明说了多久之后再来」那一种，兜不住「它只是拒绝，不给头」。
//!
//! # 🔴 槽位必须活到**响应体 drop**，不是拿到响应头就放
//!
//! 这是本模块唯一容易做错的地方，也是它当初被判成「做不到」的原因。
//! 流式请求在**拿到 2xx 之后**才是真正占用上游那一段（SSE 可以持续几分钟）。
//! 把 permit 在 `send()` 返回时就释放，等于只限制了「同时握手的数量」——
//! 而真实的并发压力全在握手之后，也就是**这个上限会静默地什么都不限**。
//!
//! 挂法：permit 交给 [`super::upstream::guard_stream_idle`] 一起搬进流的状态机，
//! 流被 drop（正常结束 / 客户端断开 / 静默超时注入后终止）时它跟着 drop。
//! 非流式那条路不用特别处理 —— `resp.text()` 在同一个作用域里 await 完，permit 随之落地。
//!
//! # 🔴 为什么是进程级常量，不是 `KeyParams` 字段
//!
//! 加字段会连带 KeyEditor 的 UI、`providerKeyDraftParity`、`upsert_key` 的运行态清单 ——
//! 而那条链上本仓刚栽过一次（`allow_in_aggregate` 保存一次 Key 就被清成 false）。
//! 本层的收益不依赖用户填对一个数字：**默认值就已经拿到了主要那部分**。
//! 真要做成可配是独立一轮，届时这里改成「读 Key 上的值、缺省回落本常量」即可，
//! 调用点一行都不用动。
//!
//! # 阈值 8：为什么不是更小
//!
//! 判据是「不改变今天正常用户的体验，只削掉尖峰」。实测形态里 Claude Code 单会话的并发
//! 在 2~4，Codex 工具循环受 `MAX_CONCURRENT_TOOLS` 约束也在这个量级。取 8 留了一倍余量：
//! **宁可这一层偶尔不起作用，也不要它成为正常使用下的排队点** —— 后者的表现是「代理变慢了」
//! 而用户无从归因，比「偶尔撞一次上游限流」糟得多（那一次至少有日志与故障转移兜着）。
//!
//! # 排队而不是拒绝
//!
//! 满了就 `await`，不返回错误。理由与 `balance_gate` 的「降级不剔除」同源：
//! 这一层的判据是**我们自己猜的**（用户的上游到底允许多少并发我们不知道），
//! 猜错时排队只是慢一点，拒绝则是把一个本来会成功的请求变成失败。
//!
//! 排队**不设上限也不设超时**：上游侧的超时（`key_timeout` / 故障转移预算）已经罩着整条
//! 链路，在这里再加一层会制造两个数字打架。

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// 每 Key 同时在途的上游请求上限。见模块头「阈值 8」。
pub(crate) const MAX_INFLIGHT_PER_KEY: usize = 8;

/// key_id → 该 Key 的信号量。
///
/// 进程级、只增不减 —— 条目数上界是用户配置的 Key 数（个位到几十），
/// 不像 `lan_guard` 的 `SEEN` 那样能被外部无限撑大，故不需要上限。
fn table() -> &'static parking_lot::Mutex<HashMap<String, Arc<Semaphore>>> {
    static T: std::sync::OnceLock<parking_lot::Mutex<HashMap<String, Arc<Semaphore>>>> =
        std::sync::OnceLock::new();
    T.get_or_init(|| parking_lot::Mutex::new(HashMap::new()))
}

/// 取一个槽位。满了就排队等（见模块头「排队而不是拒绝」）。
///
/// 返回的 permit **必须被持有到这次上游交互结束**。丢掉它（`let _ = acquire(..)`）
/// 会让整层静默失效 —— 那正是本模块最容易犯的错，有源码级判据钉着调用点。
pub(crate) async fn acquire(key_id: &str) -> OwnedSemaphorePermit {
    let sem = {
        let mut t = table().lock();
        t.entry(key_id.to_string())
            .or_insert_with(|| Arc::new(Semaphore::new(MAX_INFLIGHT_PER_KEY)))
            .clone()
    };
    // `acquire_owned` 而不是 `acquire`：permit 要跨作用域搬进流的状态机，
    // 借用式的 permit 活不到那里（生命周期绑在 `sem` 这个局部变量上）。
    //
    // `expect`：只有 `close()` 之后才会 Err，而本模块从不关闭任何信号量
    // （`table` 里的条目只增不减）。这是不变量断言，不是可能失败的 IO。
    sem.acquire_owned().await.expect("信号量从不被 close")
}

/// 当前有多少个槽位被占用（诊断用）。
pub(crate) fn in_flight(key_id: &str) -> usize {
    let t = table().lock();
    match t.get(key_id) {
        Some(s) => MAX_INFLIGHT_PER_KEY.saturating_sub(s.available_permits()),
        None => 0,
    }
}

// 🔴 **刻意没有 `reset_for_tests`**（`quota_window` 有一个）。两者的表结构不同：
// 那个模块的用例要断言「全表里有没有窗口」，而本模块的表**按 key id 分桶**，用例只关心自己
// 那一桶 —— 清整张表反而会踩到并行跑的别的用例。隔离靠独有 key id，见测试段开头那段注释。

#[cfg(test)]
mod tests {
    use super::*;

    // 🔴 **隔离靠「每条用例独有的 key id」，不靠串行化。**
    //
    // 进程级表确实会跨用例串台（`quota_window` / `lan_guard` 都实测过），但本模块的表是
    // **按 key id 分桶**的 —— 用不同的 id 就天然互不干扰。第一版加了一把 std Mutex 串行化，
    // 被 clippy 的 `await_holding_lock` 拦下：那个 lint 是对的，`MutexGuard` 跨 await 是真隐患。
    // 而 `reset_for_tests()` 会清**整张**表（会踩别的用例），所以也一并不用它。

    /// 上限真的生效：拿满之后第 N+1 个必须等，而放掉一个之后它立刻能进。
    #[tokio::test]
    async fn the_limit_actually_blocks_and_releases() {
        let id = "cc_limit";
        let mut held = Vec::new();
        for i in 0..MAX_INFLIGHT_PER_KEY {
            // 🔴 **连这个「本该立刻成功」的循环也不许裸 `.await`。**
            //
            // 注入实测（`t.entry(key_id)` → 固定串 = 退化成全局上限）：另一条用例先占满了那个
            // 全局桶，于是这里第一次 acquire 就永远等下去，`cargo test` 被外层超时杀掉。
            // 挂死比变红糟得多 —— 它在 CI 里烧满整个 job 的时间预算，且长得像基建故障。
            held.push(
                futures_util::future::FutureExt::now_or_never(acquire(id))
                    .unwrap_or_else(|| panic!("第 {i} 个槽位本该立刻拿到（上限是 {MAX_INFLIGHT_PER_KEY}）")),
            );
        }
        assert_eq!(in_flight(id), MAX_INFLIGHT_PER_KEY, "满载时在途数应等于上限");

        // 第 N+1 个必须拿不到。用 `now_or_never` 而不是 sleep：后者会让这条用例
        // 在慢机器上偶发假绿（等得够久说不定真拿到了）。
        let blocked = futures_util::future::FutureExt::now_or_never(acquire(id));
        assert!(blocked.is_none(), "🔴 满载时不该还能拿到槽位 —— 那说明上限压根没生效");

        // 放掉一个 → 立刻能进。
        drop(held.pop());
        let got = futures_util::future::FutureExt::now_or_never(acquire(id));
        assert!(got.is_some(), "放掉一个之后应当立刻有槽位");
        assert_eq!(in_flight(id), MAX_INFLIGHT_PER_KEY, "又满了");
    }

    /// 上限是**每 Key** 的，不是全局的。
    ///
    /// 判据存在的理由：把 `table()` 退化成一个全局 `Semaphore`（少几行代码、看着更简单）
    /// 会让一条 Key 的流量把**别的 Key** 一起挡住 —— 而那与故障转移的目的直接冲突
    /// （切到备用 Key 正是为了绕开有问题的那条）。
    #[tokio::test]
    async fn the_limit_is_per_key_not_global() {
        let mut held = Vec::new();
        for i in 0..MAX_INFLIGHT_PER_KEY {
            // 同上一条用例：不许裸 `.await`（注入下会挂死而不是变红）。
            held.push(
                futures_util::future::FutureExt::now_or_never(acquire("cc_a"))
                    .unwrap_or_else(|| panic!("A 的第 {i} 个槽位本该立刻拿到")),
            );
        }
        // ⚠️ **必须用 `now_or_never` 而不是裸 `.await`。**
        //
        // 注入实测（把 `t.entry(key_id)` 换成固定串 = 退化成全局上限）：裸 await 会让这条用例
        // **永远挂着**，`cargo test` 被外层超时杀掉 —— 而挂死比变红糟得多，它在 CI 里烧满整个
        // job 的时间预算，且长得像基建故障而不是判据失败。`now_or_never` 把「拿不到」变成一个
        // 立刻可断言的 `None`。
        let other = futures_util::future::FutureExt::now_or_never(acquire("cc_b"));
        assert!(
            other.is_some(),
            "🔴 A 满载不该挡住 B —— 那会让故障转移切到备用 Key 也一样被堵住"
        );
        assert_eq!(in_flight("cc_a"), MAX_INFLIGHT_PER_KEY);
        assert_eq!(in_flight("cc_b"), 1);
    }

    /// 没见过的 key 报 0，不 panic（诊断报告会对每条 Key 调它，包括从没转发过的）。
    #[test]
    fn an_unseen_key_reports_zero() {
        assert_eq!(in_flight("cc_never_seen"), 0);
    }

    /// 🔴 **源码级接线判据：两条转发路径都要取槽位，且流式那条不许提前放。**
    ///
    /// 上面三条只测本模块自己 —— 把 `proxy.rs` 里那两行 `acquire` 整个删掉，
    /// **它们照样全绿**，而那就是「上限压根没接上」这个缺陷本身。这是本仓第 19 次
    /// 盯同一类盲区（前几次：`route_meta` / `lan_guard` 的 peer / `log_rotate` 的写线程…）。
    ///
    /// 第二条断言守的是本模块最容易做错的那一点：**permit 必须一路交给
    /// `guard_stream_idle`**。就地 `drop(permit)` 或写成 `let _ = permit;` 会让整层退化成
    /// 「只限同时握手的数量」—— 而真实并发压力全在握手之后，也就是这个上限会**静默地
    /// 什么都不限**。那种失效不会让任何行为用例变红。
    #[test]
    fn both_forwarding_paths_must_take_a_slot_and_the_streaming_one_must_not_drop_it_early() {
        let src = std::fs::read_to_string("src/proxy.rs").unwrap();
        let prod = crate::proxy::custom_headers::production_code_only(&src);
        // 🔴 **必须钉「按这条 Key 的 id 取」，不能只钉「调了 acquire」。**
        // 只钉调用存在时，把实参换成一个固定字符串（= 退化成全局上限，一条 Key 的流量挡住
        // 所有 Key）判据照样绿 —— 写第一版时的注入实测就是这个结论。
        assert_eq!(
            prod.matches("concurrency::acquire(&key.id)").count(),
            2,
            "流式（try_stream_to_key）与非流式（forward_to_key）两条路都必须**按 key.id** 取槽位"
        );
        // 流式那条：permit 必须作为实参交给 guard，而不是在本地被丢掉。
        assert_eq!(
            prod.matches("guard_stream_idle(resp.bytes_stream(), permit)").count(),
            2,
            "🔴 两个流式出口都必须把 permit 交给 guard —— 就地 drop 会让上限静默失效"
        );
        for bad in ["drop(permit)", "let _ = permit;"] {
            assert!(
                !prod.contains(bad),
                "不许提前释放槽位（发现 `{bad}`）—— 那会让并发上限只管住握手阶段"
            );
        }
        // guard 那一侧：permit 必须被搬进 unfold 的状态，不能在函数体里被丢掉。
        //
        // 🔴 **正向断言（「它必须交给 unfold」）比一串反向黑名单可靠。** 第一版只列了
        // `let _ = permit;`，而注入用 `drop(permit); … guard_with(.., None)` 就绕过去了 ——
        // 判据仍绿，而那正是「上限退化成只限握手」这个缺陷本身。列黑名单永远漏一种写法。
        let guard_src = std::fs::read_to_string("src/upstream/stream_idle.rs").unwrap();
        let guard_prod = crate::proxy::custom_headers::production_code_only(&guard_src);
        assert!(
            guard_prod.contains("guard_with(stream, IDLE_LIMIT, Some(permit))"),
            "🔴 guard 必须把 permit 原样交给 guard_with —— 那是让它活到响应体 drop 的唯一去处"
        );
        assert!(
            guard_prod.contains("(St::Live(Box::pin(stream)), permit)"),
            "🔴 permit 必须被搬进 unfold 的状态里；留在函数体内它会在流开始前就被 drop"
        );
        for bad in ["drop(permit)", "let _ = permit;"] {
            assert!(
                !guard_prod.contains(bad),
                "guard 里不许提前释放槽位（发现 `{bad}`）"
            );
        }
    }
}
