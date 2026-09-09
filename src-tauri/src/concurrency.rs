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
//! 满了就 `await`。理由与 `balance_gate` 的「降级不剔除」同源：这一层的判据是**我们自己
//! 猜的**（用户的上游到底允许多少并发我们不知道），猜错时排队只是慢一点，
//! 拒绝则是把一个本来会成功的请求变成失败。
//!
//! ⚠️ **排队本身受本次尝试的墙钟预算约束**（[`acquire_with_budget`]），到点返回
//! `Err(())`，调用方转成 [`crate::error::AppError::QueueTimeout`] 并让故障转移试下一个候选。
//! 本节原先写的是「不返回错误」「不设上限也不设超时」—— 那两句已不成立：
//! 裸 `await` 会让排队时间**加在**用户配的超时之外，并整段逃出故障转移 deadline
//! （`budget_left` 是排队之前算的），一条被打满的 Key 能把后续候选的机会一起拖没。
//!
//! 🔴 **排队超时绝不能计入熔断**：掐掉这次尝试的是**我们自己的信号量**，不是上游。
//! 那条误伤与 `budget_truncated_attempt` 要消除的完全同形 —— 一条完好、只是正忙的 Key
//! 连撞三次就被停 60s，而越好用（越忙）的 Key 越先被罚。判据在 `AppError::QueueTimeout`，
//! 两条转发路径各有一处 `!is_queue` 守着。

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// 每 Key 同时在途的上游请求上限。见模块头「阈值 8」。
pub(crate) const MAX_INFLIGHT_PER_KEY: usize = 8;

/// key_id → 该 Key 的信号量。
///
/// 进程级。条目数上界是用户配置的 Key 数（个位到几十），不像 `lan_guard` 的 `SEEN`
/// 那样能被外部无限撑大。空闲条目由 [`sweep`] 回收（删 Key / 导入替换会不断产生新 id）。
fn table() -> &'static parking_lot::Mutex<HashMap<String, Arc<Semaphore>>> {
    static T: std::sync::OnceLock<parking_lot::Mutex<HashMap<String, Arc<Semaphore>>>> =
        std::sync::OnceLock::new();
    T.get_or_init(|| parking_lot::Mutex::new(HashMap::new()))
}

/// 回收表里已经没人用的条目。删除 Key / 导入替换会不断产生新 id，只增不减会让托盘
/// 常驻的进程表长期增长。
///
/// 🔴 **起决定作用的是 `strong_count > 1`**，它防的是 [`acquire`] 自己那个窗口：
/// 它在锁内 clone 出 `sem`、**在锁外** await `acquire_owned()`。那一瞬间 permit 数还是满的，
/// 而 Arc 已经被克隆走了。此时若另一个线程的扫描只看 permit 数，就会把这一条从表里删掉
/// → 调用方拿着一个**孤立**的信号量继续跑 → 下一次同 key 的 acquire 新建一个 →
/// 同一条 Key 有了两个信号量，**上限从 8 静默变成 16**。
///
/// ⚠️ **`available_permits() < MAX` 这一半今天是被前一半蕴含的，不是「缺一不可」。**
/// 我们只用 `acquire_owned`，而 `OwnedSemaphorePermit` 自己持有一份 Arc ——
/// 于是「有 permit 在外面」必然意味着 `strong_count >= 2`。注入实测确认：把这一半删掉，
/// 本模块全部用例仍然绿（`strong_count` 那一半把它们都保住了）。
///
/// 留着它是**一道边界**：`OwnedSemaphorePermit::forget()` 与 `Semaphore::add_permits`
/// 能在不持有 Arc 的前提下让容量长期减少，那时只看 `strong_count` 就会把一个容量已被
/// 永久扣减的条目当成空闲收掉。生产段目前一处都没用它们，有源码级判据钉住这一点 ——
/// 哪天真用上了，这一半立刻从「冗余」变成「必需」。
///
/// 抽成独立函数是为了能被判据直接压到：`acquire` 锁外那个中间态在进程外**构造不出来**。
fn sweep(t: &mut HashMap<String, Arc<Semaphore>>) {
    t.retain(|_, s| Arc::strong_count(s) > 1 || s.available_permits() < MAX_INFLIGHT_PER_KEY);
}

/// 取一个槽位。满了就排队等（见模块头「排队而不是拒绝」）。
///
/// 返回的 permit **必须被持有到这次上游交互结束**。丢掉它（`let _ = acquire(..)`）
/// 会让整层静默失效 —— 那正是本模块最容易犯的错，有源码级判据钉着调用点。
pub(crate) async fn acquire(key_id: &str) -> OwnedSemaphorePermit {
    let sem = {
        let mut t = table().lock();
        sweep(&mut t);
        t.entry(key_id.to_string())
            .or_insert_with(|| Arc::new(Semaphore::new(MAX_INFLIGHT_PER_KEY)))
            .clone()
    };
    // `acquire_owned` 而不是 `acquire`：permit 要跨作用域搬进流的状态机，
    // 借用式的 permit 活不到那里（生命周期绑在 `sem` 这个局部变量上）。
    //
    // `expect`：只有 `close()` 之后才会 Err，而本模块从不调 `close()`。
    //
    // ⚠️ 上一版这里写的理由是「`table` 里的条目只增不减」—— 上面那行 `retain` 之后
    // 那句话已不成立。真正的保证是**从表里移除 ≠ close**：`sem` 是上面克隆出来的 Arc，
    // 被移出表之后它仍然活着（我们持有一份），而回收判据要求 `strong_count == 1`，
    // 所以正在被等待/持有的信号量根本不会被移除。
    sem.acquire_owned().await.expect("信号量从不被 close")
}

/// 在本次尝试的墙钟预算内等槽位。返回拿到槽位后还剩多少网络时间。
///
/// 这道 helper 的关键不是少写两行，而是**排队必须算进 Key/故障转移预算**：若先裸 await
/// 再给 `send()` 一份完整 timeout，用户配的 30s 会变成「排队 N 秒 + 网络 30s」。
pub(crate) async fn acquire_with_budget(
    key_id: &str,
    budget: std::time::Duration,
) -> Result<(OwnedSemaphorePermit, std::time::Duration), ()> {
    let started = std::time::Instant::now();
    let permit = tokio::time::timeout(budget, acquire(key_id)).await.map_err(|_| ())?;
    Ok((permit, budget.saturating_sub(started.elapsed())))
}

/// 这个 key 在表里还有没有条目（**只给判据用**）。
///
/// 回收是「表不许无界增长」与「活着的信号量不许被换掉」两条性质的交点，而从外面完全
/// 观察不到 —— 没有它，把 `retain` 整行删掉、或去掉 `strong_count` 那一半，
/// 一条用例都不会红（前者是功能消失，后者会让同一条 Key 出现两个信号量、上限翻倍）。
#[cfg(test)]
pub(crate) fn is_tracked(key_id: &str) -> bool {
    table().lock().contains_key(key_id)
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

    /// 🔴 **回收：空闲条目要收掉，而仍被持有的绝不许收掉。**
    ///
    /// 两个方向都必须钉，而且都只能从表里看 —— 从外面观察不到：
    /// - 收不掉 → 表随「删 Key / 导入替换」不断产生的新 id 无界增长（托盘常驻进程会一直涨）；
    /// - 收错了 → 同一条 Key 的信号量被换成一个新的，而旧 permit 还在别处持有着 →
    ///   **上限静默翻倍**（8 变 16）。后者是这一半 `strong_count` 判据存在的全部理由，
    ///   而它没有任何行为用例覆盖：把那一半删掉，其余 4 条照样全绿。
    ///
    /// 回收只发生在 `acquire` 里（顺带做，不额外起清理任务），所以要用**另一个 key** 的
    /// acquire 去触发一次扫描。
    #[tokio::test]
    async fn idle_entries_are_reclaimed_but_live_ones_are_never_swapped_out() {
        let live = "cc_gc_live";
        let idle = "cc_gc_idle";
        let trigger = "cc_gc_trigger";

        // ① 空闲条目：用完即还，随**下一次** acquire 的扫描被收掉。
        //
        // ⚠️ 扫描排在 `acquire` 的开头（在插入自己那一条之前），所以刚放掉的这一条
        // 此刻**还在表里** —— 要等别人来一次才收。写这条用例时先断言错了方向，
        // 那不是缺陷：任何一次 acquire 都会扫全表，故「无界增长」这个目标仍然成立，
        // 只是最后用过的那一条会留到下次。
        drop(acquire(idle).await);
        drop(acquire(trigger).await); // 触发扫描
        assert!(!is_tracked(idle), "全部槽位空闲且无人持有 → 必须被回收，否则表无界增长");

        // ② 仍被持有的条目：扫描不许碰它。
        let held = acquire(live).await;
        assert!(is_tracked(live));
        drop(acquire(trigger).await); // 触发一次扫描
        assert!(
            is_tracked(live),
            "🔴 仍有 permit 在外面时条目被收掉了 —— 下一次 acquire 会新建一个信号量，\
             同一条 Key 于是有两个，上限从 {MAX_INFLIGHT_PER_KEY} 静默翻倍"
        );
        assert_eq!(in_flight(live), 1, "在途计数必须仍看得到那一个");

        // ③ 上限没被换掉：拿满剩下的，再要一个必须等。
        let mut rest = Vec::new();
        for _ in 1..MAX_INFLIGHT_PER_KEY {
            rest.push(acquire(live).await);
        }
        assert_eq!(in_flight(live), MAX_INFLIGHT_PER_KEY);
        assert!(
            futures_util::future::FutureExt::now_or_never(acquire(live)).is_none(),
            "🔴 拿满之后还能立刻再拿到 = 信号量被换过，上限翻倍"
        );

        drop(held);
        drop(rest);
        // 全部还清之后它也该被收掉（同 ①，只是走的是「先满后空」那条路）。
        drop(acquire(trigger).await);
        assert!(!is_tracked(live), "还清之后应随下一次扫描回收");
    }

    /// 🔴 **`strong_count` 那一半必须在** —— 它是 `sweep` 里唯一起决定作用的判据。
    ///
    /// 上面那条行为用例抓不住「只删掉 `strong_count` 那一半」：只要 permit 在外面用着，
    /// `available` 那一半就先把条目保住了，于是它的贡献完全观察不到。
    /// 这里直接对 `sweep` 构造「Arc 被别处持有、但 permit 还是满的」那一格 ——
    /// 它就是 [`acquire`] 在锁外 await 时的真实中间态，从进程外**构造不出来**。
    ///
    /// ⚠️ **本条刻意不断言「`available` 那一半也必需」** —— 注入实测它是被蕴含的
    /// （见 [`sweep`] 的文档）。写一条「两半都必需」的断言会是假话，
    /// 而声称验过却没验是本仓最贵的那类过时注释。
    #[test]
    fn the_strong_count_half_of_the_sweep_predicate_is_load_bearing() {
        let mut t: HashMap<String, Arc<Semaphore>> = HashMap::new();

        // ① 只有表自己持有、permit 全空 → 该收。
        t.insert("gone".into(), Arc::new(Semaphore::new(MAX_INFLIGHT_PER_KEY)));

        // ② permit 在外面用着（Arc 也被 permit 持有着）→ 不许收。
        let busy = Arc::new(Semaphore::new(MAX_INFLIGHT_PER_KEY));
        let _permit = busy.clone().try_acquire_owned().expect("新信号量必然有槽");
        t.insert("busy".into(), busy);

        // ③ 🔴 Arc 被别处持有，但 permit **还是满的** —— `acquire` 在锁外 await 的那一瞬间。
        //    只看 permit 数的话这一条会被收掉，而调用方正拿着它 → 下一次 acquire 会新建
        //    第二个信号量 → 上限翻倍。这一格是 `strong_count` 那一半唯一的用处。
        let pending = Arc::new(Semaphore::new(MAX_INFLIGHT_PER_KEY));
        let _held_arc = pending.clone();
        t.insert("pending".into(), pending);

        sweep(&mut t);

        assert!(!t.contains_key("gone"), "没人用的条目必须收掉，否则表无界增长");
        assert!(t.contains_key("busy"), "还有 permit 在外面用着，收掉就是把上限废掉");
        assert!(
            t.contains_key("pending"),
            "🔴 Arc 已被 clone 走、permit 尚未取到（acquire 锁外 await 的中间态）—— \
             收掉它会让同一条 Key 出现两个信号量，上限从 {MAX_INFLIGHT_PER_KEY} 静默翻倍"
        );
    }

    /// 🔴 **`available_permits` 那一半「被蕴含」这个结论，只在没人用 `forget()` 时成立。**
    ///
    /// [`sweep`] 的文档里写着「有 permit 在外面 ⟹ `strong_count >= 2`」，依据是我们只用
    /// `acquire_owned`，而 `OwnedSemaphorePermit` 自己持有 Arc。
    /// `OwnedSemaphorePermit::forget()` 与 `Semaphore::add_permits` 会打破它：
    /// 它们能在**不持有 Arc** 的前提下让容量长期减少，那时只看 `strong_count` 就会把一个
    /// 容量已被永久扣减的条目当成空闲收掉。
    ///
    /// 判据钉住「生产段一处都没用」——哪天真用上了这里就变红，提醒去重读那段推理，
    /// 而不是让一句「已实测被蕴含」悄悄变成假话。
    #[test]
    fn the_subsumption_argument_depends_on_never_forgetting_a_permit() {
        let src = std::fs::read_to_string("src/concurrency.rs").unwrap();
        let prod = crate::proxy::custom_headers::production_code_only(&src);
        assert!(
            prod.contains("acquire_owned()"),
            "推理的前提是我们只用 acquire_owned（permit 自带 Arc）—— 换了取用方式先重读 sweep 的文档"
        );
        for bad in [".forget()", "add_permits"] {
            assert!(
                !prod.contains(bad),
                "发现 `{bad}`：它能在不持有 Arc 的前提下永久扣减容量，\
                 于是 `available_permits` 那一半从「冗余」变成「必需」——\
                 请回头修正 sweep 的文档，并给那一半补一条自己的判据"
            );
        }
    }

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
        //
        // 🔴 **钉性质不钉写法**：上一版写死 `concurrency::acquire(&key.id)` 恰好 2 次，
        // 于是把两条路径换成 `acquire_with_budget`（**更正确** —— 排队时间要算进 Key/
        // 故障转移预算，否则用户配的 30s 变成「排队 N 秒 + 网络 30s」）时它当场变红，
        // 而它想守的「两条路都按 key.id 取槽位」一条都没破。假红的代价是改进被撤回去。
        let keyed = prod.matches("acquire(&key.id").count()
            + prod.matches("acquire_with_budget(&key.id").count();
        assert_eq!(
            keyed, 2,
            "流式（try_stream_to_key）与非流式（forward_to_key）两条路都必须**按 key.id** 取槽位"
        );
        // 排队必须受预算约束：裸 `acquire` 在转发路径上会让排队逃出故障转移 deadline。
        assert_eq!(
            prod.matches("acquire_with_budget(&key.id").count(),
            2,
            "🔴 两条转发路径都必须走带预算的那个 —— 裸 acquire 的排队时间不计入任何超时"
        );
        // 🔴 **排队超时不许计入熔断，两条路径都要有那道门。**
        //
        // 掐掉这次尝试的是本模块自己的信号量，不是上游。漏掉它的表现与
        // `budget_truncated_attempt` 当初要消除的完全同形：一条完好、只是正忙的 Key 连撞
        // 三次就被停 60s，而**越好用（越忙）的 Key 越先被罚**。第五层刚上线时把它带回来过
        // 一次（三条独立审查线各自都报了它）。
        //
        // 钉「两处 `!is_queue`」而不是钉某一种写法：变体判定走
        // `AppError::is_queue_timeout()`，从消息文本反推那个数字在文案改动时会静默失效。
        assert_eq!(
            prod.matches("&& !is_queue").count(),
            2,
            "🔴 流式与非流式两条 Err 分支都必须把排队超时排除在熔断之外"
        );
        assert_eq!(
            prod.matches("e.is_queue_timeout()").count(),
            2,
            "判定必须走独立变体，不许从错误文本里反推（文案一改就静默失效）"
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
