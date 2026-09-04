//! 聚合的**闸门**：进程级并发上限 + 打上游之前的前置检查。
//!
//! # 为什么并发闸门必须是进程级的
//!
//! `gather_members` 的信号量原先**每轮新建**，于是 `concurrency_limit` 实际是「每轮上限」
//! 而不是「同时上限」。而聚合有四个入口能同时开轮：Claude CLI 走 HTTP `/mcp/claude-cli`、
//! Codex 与桌面端各走一个 stdio 子进程、桌面端 BrainPage 自己还能点。四路各开一轮 →
//! 真实并发 = 4 × limit，也就是**用户设的那个数字在最需要它的场合恰好不生效**。
//!
//! 代价是真金白银：每个成员都是一次付费上游调用，而且必然撞上游的并发/速率限制 ——
//! 撞出来的 429 又刻意不计熔断，于是下一轮照样撞。
//!
//! # 为什么按「分类 + 上限」缓存
//!
//! 上限是用户可改的，而 `Semaphore` 的容量创建后不可变。故缓存 key 带上容量：用户改了
//! 上限，下一轮自然拿到一把新的，旧的那把随最后一个持有者释放而消失。按分类分表是因为
//! 三个分类的 Key 池与配置各自独立 —— 一个分类的聚合排队不该让另一个分类干等。
//!
//! # 前置检查：聚合此前只看了三层弹性里的一层
//!
//! 熔断那层聚合早就看了。另两层没看，而它们对聚合**同样适用**：
//!
//! - **配额窗口**（[`crate::health::quota_window`]）：上游刚用 `Retry-After` 明说「N 秒后再来」。
//!   代理路径会跳过这条 Key，聚合照打 —— 每个成员白耗一次往返；而 429 刻意不计熔断，
//!   所以这个白打**每轮重复**。同一条 Key 的配额是共享的，与谁在用它无关。
//! - **余额耗尽**（[`crate::health::balance_gate`]）：已确定为 0 的 Key，打过去必然失败。
//!   判据用 `is_exhausted` 而不是自己比数字 —— 那边是三态（`Unknown` 不算耗尽），
//!   「查不到 ≠ 为零」这条边界只该有一处实现。
//!
//! 🔴 **成员「跳过」、决策者只「警告」。** 两者代价不对称：跳过一个成员只是少一份意见，
//! 而决策者被跳过整轮就没有答案了。故 [`precheck_decider`] 只落一条事件、不拦 ——
//! 宁可白打一次，也不要让一次会诊因为一个可能过时的余额数字而整轮失败。

use crate::model::{CategoryType, ProviderKey};
use crate::store::Store;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Semaphore;

/// 单个分类的成员并发上限的硬顶。用户填 999 时不至于真开 999 条并发上游连接。
const MAX_CONCURRENCY: u32 = 32;

/// `(分类, 容量)` → 信号量。进程级：四个入口共用同一把闸。
type Table = HashMap<(CategoryType, u32), Arc<Semaphore>>;

fn table() -> &'static parking_lot::Mutex<Table> {
    static T: std::sync::OnceLock<parking_lot::Mutex<Table>> = std::sync::OnceLock::new();
    T.get_or_init(|| parking_lot::Mutex::new(HashMap::new()))
}

/// 用户配的上限**实际生效**的那个值。
///
/// 🔴 **给用户看数字的地方必须用这个，不能用 `brain.concurrency_limit`。**
/// 排队耗尽那条错误原来直接回显用户填的原值并让他「提高并发上限」——
/// 而填了 999 时真实上限是 [`MAX_CONCURRENCY`]，那句话指向一个不会有任何效果的操作
/// （同本仓「指错方向的提示比没有提示更糟」那条）。
pub(crate) fn effective_limit(limit: u32) -> u32 {
    limit.clamp(1, MAX_CONCURRENCY)
}

/// 取这个分类当前该用的成员并发闸。同一分类 + 同一上限恒返回**同一把**。
pub(crate) fn member_permits(category: CategoryType, limit: u32) -> Arc<Semaphore> {
    let cap = effective_limit(limit);
    let mut t = table().lock();
    // 上限变了就换一把：旧的仍被在途的那一轮持有，等它跑完自然回收。
    // 顺手清掉没人再用的旧容量条目，免得用户来回调上限时这张表只增不减。
    t.retain(|(c, k), sem| (*c, *k) == (category, cap) || Arc::strong_count(sem) > 1);
    t.entry((category, cap))
        .or_insert_with(|| Arc::new(Semaphore::new(cap as usize)))
        .clone()
}

/// 成员打上游之前的前置检查。`Some(原因)` = 本轮跳过它，别白打。
///
/// 三层弹性合在这一处，让「聚合看哪几层」只有一个答案。文案都写明「本轮跳过」而不是
/// 「不可用」—— 这三种都是**暂时**的，用户下一轮就可能拿到它。
pub(crate) fn precheck_member(key: &ProviderKey) -> Option<String> {
    // ① 熔断窗口（真实流量连续失败触发）。聚合不做故障转移，跳过只为避免白等超时。
    if let Some(until) = key.health.breaker_until {
        if until > chrono::Utc::now().timestamp_millis() {
            return Some("熔断窗口内，本轮跳过（聚合不做故障转移换 Key）".into());
        }
    }
    // ② 上游自己给的 Retry-After 窗口。
    if crate::health::quota_window::active(&key.id) {
        return Some(
            "上游刚返回 Retry-After（配额窗口内），本轮跳过 —— 现在打过去必然还是 429".into(),
        );
    }
    // ③ 余额已确定耗尽（`Unknown` 不算，见 balance_gate 的三态）。
    if crate::health::balance_gate::is_exhausted(key) {
        return Some("余额已耗尽，本轮跳过（充值或改用别的 Key 后自动恢复）".into());
    }
    None
}

/// 决策者 / 汇总者打上游之前的**软**检查：只落事件，不拦。
///
/// 为什么不拦：决策者是整轮的出口，拦掉等于整轮失败；而这三个信号都可能过时
/// （余额缓存有 TTL、配额窗口是上游的一面之词）。落事件的价值在于**事后归因** ——
/// 「决策者为什么 429」这个问题，没有这条日志就只能猜。
pub(crate) fn precheck_decider(
    store: &Arc<Store>,
    category: CategoryType,
    key: &ProviderKey,
    label: &str,
) {
    let Some(why) = precheck_member(key) else {
        return;
    };
    store.append_event_collapsible(
        category,
        "aggregate",
        Some(&key.id),
        &format!(
            "决策者/汇总者带风险调用 · {label} · {why}（不拦，整轮出口不能因此失败）"
        ),
        None,
        // 折叠键按 Key：同一条 Key 反复命中只占一行带 ×N，不冲掉 MAX_EVENTS 环。
        Some(format!("agg-precheck:{}", key.id)),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{BalanceQuery, BalanceResult};

    fn key_with(id: &str) -> ProviderKey {
        ProviderKey {
            id: id.into(),
            ..Default::default()
        }
    }

    /// 闸门的**语义**：同一分类 + 同一上限恒是同一把；改了上限换一把；分类之间互不影响。
    ///
    /// `Arc::ptr_eq` 是这条判据的关键 —— 「每轮新建」在别的断言下全都看不出区别
    /// （容量一样、行为一样），只有「是不是同一个对象」能区分。
    #[test]
    fn the_member_gate_is_shared_across_rounds() {
        let a = member_permits(CategoryType::ClaudeCli, 3);
        let b = member_permits(CategoryType::ClaudeCli, 3);
        assert!(
            Arc::ptr_eq(&a, &b),
            "同分类同上限必须拿到同一把闸 —— 每轮新建等于上限只对单轮生效"
        );
        assert_eq!(a.available_permits(), 3);

        // 上限变了：换一把（Semaphore 容量创建后不可变）。
        let c = member_permits(CategoryType::ClaudeCli, 5);
        assert!(!Arc::ptr_eq(&a, &c), "改了上限应拿到新的一把");
        assert_eq!(c.available_permits(), 5);

        // 分类之间独立：一个分类排队不该让另一个干等。
        let d = member_permits(CategoryType::Codex, 5);
        assert!(!Arc::ptr_eq(&c, &d), "不同分类必须各有一把");
    }

    #[test]
    fn concurrency_limit_is_clamped_to_a_sane_range() {
        // 0（用户清空输入框）不能变成「永远拿不到 permit」的死锁。
        assert_eq!(member_permits(CategoryType::ClaudeDesktop, 0).available_permits(), 1);
        // 999 不能真开 999 条并发上游连接。
        assert_eq!(
            member_permits(CategoryType::ClaudeDesktop, 999).available_permits(),
            MAX_CONCURRENCY as usize
        );
    }

    /// 三层弹性都要被看到。**此前只看了熔断**，另两层对聚合同样适用却漏了。
    #[test]
    fn all_three_resilience_layers_skip_the_member() {
        // 干净的 Key：不跳过。
        assert!(precheck_member(&key_with("clean")).is_none());

        // ① 熔断窗口内
        let mut breaking = key_with("breaking");
        breaking.health.breaker_until = Some(chrono::Utc::now().timestamp_millis() + 60_000);
        assert!(
            precheck_member(&breaking)
                .unwrap_or_default()
                .contains("熔断")
        );
        // 已过期的熔断窗口不该继续挡（否则一条恢复了的 Key 永远回不来）。
        breaking.health.breaker_until = Some(chrono::Utc::now().timestamp_millis() - 1);
        assert!(precheck_member(&breaking).is_none());

        // ③ 余额已确定耗尽
        let mut broke = key_with("broke");
        broke.balance_query = Some(BalanceQuery {
            enabled: true,
            ..Default::default()
        });
        broke.cached_balance = Some(BalanceResult {
            ok: true,
            remaining: Some(0.0),
            error: None,
            ..BalanceResult::failed("")
        });
        assert!(
            precheck_member(&broke).unwrap_or_default().contains("余额"),
            "余额确定为 0 的 Key 打过去必然失败"
        );

        // 🔴 反面：**查不到 ≠ 为零**。同 balance_gate 的三态 —— 一次网络抖动不该
        // 让一条好 Key 在聚合里被静默跳过（那比路由降级更难发现，用户只看到「少一位专家」）。
        let mut unknown = key_with("unknown");
        unknown.balance_query = Some(BalanceQuery {
            enabled: true,
            ..Default::default()
        });
        unknown.cached_balance = Some(BalanceResult::failed("timeout"));
        assert!(
            precheck_member(&unknown).is_none(),
            "查询失败必须按 Unknown 处理、照常参与"
        );
    }

    /// ② 配额窗口那一层。**必须用独有的 key id**：窗口表是进程级的，
    /// 短 id（`k1`）会与别的模块的测试串台（本仓为此红过 8 条）。
    #[test]
    fn an_upstream_retry_after_window_skips_the_member() {
        let key = key_with("agg_gate_qw_1");
        assert!(precheck_member(&key).is_none(), "开局不该被挡");

        let dir = std::env::temp_dir().join(format!("synaroute_gate_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Arc::new(
            Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap(),
        );
        crate::health::quota_window::arm(&store, CategoryType::ClaudeCli, &key.id, 60);
        assert!(
            precheck_member(&key)
                .unwrap_or_default()
                .contains("Retry-After"),
            "上游刚说「N 秒后再来」，这一轮就别打它 —— 429 刻意不计熔断，白打会每轮重复"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 🔴 **给用户看的数字必须是实际生效的那个。**
    ///
    /// 排队耗尽那条错误原来回显 `brain.concurrency_limit` 的原值并让用户「提高并发上限」——
    /// 而填了 999 时真实上限是 [`MAX_CONCURRENCY`]，那句话指向一个**不会有任何效果**的操作。
    /// 同本仓「指错方向的提示比没有提示更糟」那条。
    #[test]
    fn user_facing_numbers_use_the_effective_limit() {
        assert_eq!(effective_limit(0), 1, "0 要被抬到 1，不是「永远拿不到 permit」");
        assert_eq!(effective_limit(3), 3, "区间内原样");
        assert_eq!(effective_limit(999), MAX_CONCURRENCY, "超顶要报硬顶，不是原值");
        // 与闸门的容量必须是同一个数 —— 两处各算一份就会出现「报的和实际不一样」。
        for limit in [0u32, 1, 5, 999] {
            assert_eq!(
                member_permits(CategoryType::Codex, limit).available_permits(),
                effective_limit(limit) as usize,
                "limit={limit} 时报的数字与闸门容量必须一致"
            );
        }
    }

    /// 源码级接线判据：**成员并发闸只能来自 [`member_permits`]**。
    ///
    /// 上面那条 `Arc::ptr_eq` 用例只证明「这个函数返回同一把」，它**证明不了**
    /// `gather_members` 用的是这个函数 —— 把那一行改回 `Semaphore::new(...)`，
    /// 它照样全绿，而那正是缺陷本体。这是本仓第 15 次盯同一类接线盲区。
    #[test]
    fn gather_members_must_take_its_gate_from_this_module() {
        let agg = crate::proxy::custom_headers::production_code_only(include_str!(
            "../aggregate.rs"
        ));
        assert!(
            agg.contains("gate::member_permits(category, brain.concurrency_limit)"),
            "gather_members 必须从进程级闸门取 permit"
        );
        assert!(
            !agg.contains("Semaphore::new"),
            "aggregate.rs 生产段不许再自己建信号量 —— 每轮一把等于上限只对单轮生效"
        );
        assert!(
            agg.contains("gate::precheck_member(&key)"),
            "成员前置检查必须走 gate（此前那三层里只判了熔断）"
        );
        assert!(
            !agg.contains("key.health.breaker_until"),
            "熔断判定已收进 gate::precheck_member，这里不该再有第二份"
        );
    }

    /// 🔴 **给用户看的那个数字必须是实际生效的上限。**
    ///
    /// 「成员阶段预算已被排队耗尽（并发上限 N 低于成员数）…可提高『并发上限』」这条错误
    /// 原来直接回显用户填的原值。用户填 999 时真实上限是 [`MAX_CONCURRENCY`] ——
    /// 于是那句话告诉他「并发上限 999 太低，去提高它」，而提高到任何值都不会有效果。
    /// 同本仓「指错方向的提示比没有提示更糟」那条。
    #[test]
    fn the_user_facing_limit_is_the_effective_one() {
        // 与 member_permits 用的是同一份 clamp —— 两处各写一份的话，
        // 文案说的数字与真实闸门容量会漂开。
        for limit in [0, 1, 3, 32, 33, 999] {
            assert_eq!(
                effective_limit(limit) as usize,
                member_permits(CategoryType::Codex, limit).available_permits(),
                "limit={limit} 时文案里的数字与闸门实际容量必须一致"
            );
        }
        assert_eq!(effective_limit(999), MAX_CONCURRENCY, "填 999 时生效的是硬顶");
        assert_eq!(effective_limit(0), 1, "填 0 不能变成 0 并发（死锁）");

        // 源码级接线：排队耗尽那条文案必须取 effective_limit，且生产段不许再有第二份 clamp。
        let agg = crate::proxy::custom_headers::production_code_only(include_str!(
            "../aggregate.rs"
        ));
        assert!(
            agg.contains("gate::effective_limit(brain.concurrency_limit)"),
            "用户可见的并发上限数字必须过 effective_limit"
        );
        assert!(
            !agg.contains("brain.concurrency_limit.max(1)"),
            "不许在 aggregate.rs 里另写一份 clamp —— 它会与 gate 的硬顶漂开"
        );
    }
}
