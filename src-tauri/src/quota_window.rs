//! 上游明确给出的「配额恢复时刻」窗口 —— 弹性的第三层。
//!
//! 挂在 [`crate::health`] 下（`#[path]`），同 `balance_gate.rs`：它与另两层
//! （Key 级熔断、单模型锁定）确实同族，而 `store.rs`/`proxy.rs` 余量都是 0。
//!
//! # 它补的洞
//!
//! 上游回 429/503 并带 `Retry-After: N` 时，`TRANSIENT_4XX` 刻意**不计熔断**
//! （「不罚好 Key」，那条规则本身是对的）。代价是这条 Key **立刻回到候选池首位**，
//! 下一个请求照样先打它、照样 429 —— 每个请求白耗一次往返，直到额度自己恢复。
//!
//! 也就是说：上游明明告诉了我们「N 秒后再来」，我们却只把这个数字**转给了下游**
//! （`retry_after_hint`），自己一个字都没记住。
//!
//! 这与 `balance_gate` 是同一层、同一个道理（「别等失败才知道」），区别在数据来源：
//! 余额要我们主动去查，配额恢复时刻是**上游自己送上来的** —— 可信度更高、
//! 零额度成本、且不需要用户配置任何东西。
//!
//! 参照 ccLoad 的 `internal/cooldown`：它把「显式上游重置截止时间」排在指数退避
//! **之前**（README 原文 "explicit upstream reset deadlines take priority"）。
//!
//! # 🔴 刻意**不是**熔断，三处语义都不同
//!
//! 1. **不由 `record_live_success` 清除。** 熔断是「这条 Key 好像坏了」的猜测，
//!    一次成功就该推翻它；配额窗口是上游给的**事实**，另一个模型/另一次请求成功
//!    不代表配额恢复（多数中转站按模型或按端点分别限流）。窗口只由时间解除。
//! 2. **不进 `HealthState`、不落盘。** Retry-After 是秒到分钟级的量，
//!    持久化的唯一效果是「重启后仍被一个早已过期的窗口挡住」。同短路窗口的做法。
//! 3. **不参与「主 Key 是哪条」。** 那是配置态（`enabled_keys_sorted`），
//!    判据与 `balance_gate` 一字不差：让徽标跟着每次 429 跳的表现是
//!    「用户什么都没改，主 Key 自己换了一条」。
//!
//! # 🔴 时长必须夹上限
//!
//! `Retry-After` 由上游填，见过的值从几秒到 86400（一天）都有。把一条 Key 挡一天
//! 是不可接受的 —— 用户可能刚充了钱、刚换了套餐，而我们要到明天才肯再试一次。
//! 夹到 [`MAX_WINDOW_SECS`] 之后最坏是「过期后白撞一次 429」，一次往返的代价，
//! 换来「配额恢复后立刻能用」。同本仓「静默错比响亮错更糟」那条取舍。
//!
//! # 全池都在窗口内时会怎样
//!
//! `model_pool::rank_candidates` 的兜底分支（primary 为空 → 忽略全部运行态门槛）
//! 自动接管：仍然把全部启用 Key 按原顺序返回，让它们各撞一次。
//! 这是对的 —— 无处可切时不该自杀成 503，而下游拿到的 429 会带上
//! `retry_after_hint`（那条链路不受本模块影响）。
//!
//! # ⚠️ 进程级表 + 共用短 id 的测试夹具 = 跨模块串台（真踩过）
//!
//! 本模块一上线，全量 `cargo test --lib` 当场红 **8 条**（单独跑本模块永远绿）。
//! 成因不是功能：`proxy.rs` 的测试里有 **37 处**都用 `key("k1"/"k2", …)`，
//! 而其中三条会让上游 mock 返回 `Retry-After` → 它们武装的窗口把**别的用例**的
//! 候选挡掉了。本模块内部的串行锁只保护自己，管不住同进程其它模块。
//!
//! 修的是**夹具**不是生产代码（生产 id 是 UUID，不可能撞）：那三条用例改用
//! `ra*`/`mp*`/`mf*` 独有前缀，本模块的用例一律用 `qw_*`。
//! 加新的「上游返回 Retry-After」用例时，**必须给它独有的 key id**。

use crate::model::CategoryType;
use crate::store::Store;
use chrono::Utc;
use std::collections::HashMap;
use std::sync::Arc;

/// 单次窗口的上限（秒）。见模块头「时长必须夹上限」。
const MAX_WINDOW_SECS: i64 = 300;

/// 表内条目上限。key_id 是有限集（用户配的 Key 数），正常远小于它；
/// 设上限只为堵住「配置被反复重建导致 id 无限增长」这类进程级只增不减的泄漏
/// （同 `lan_guard` 的 `SEEN` 那 1024 上限）。
const MAX_ENTRIES: usize = 256;

/// key_id → 窗口结束时刻（epoch ms）。进程级，见模块头第 2 条。
fn table() -> &'static parking_lot::Mutex<HashMap<String, i64>> {
    static T: std::sync::OnceLock<parking_lot::Mutex<HashMap<String, i64>>> =
        std::sync::OnceLock::new();
    T.get_or_init(|| parking_lot::Mutex::new(HashMap::new()))
}

/// 这条 Key 现在是否在配额窗口内（= 本次不该选它）。
///
/// 顺带清掉已过期的条目：读路径上做清理，免得为它单开一趟后台线程。
pub fn active(key_id: &str) -> bool {
    let now = Utc::now().timestamp_millis();
    let mut t = table().lock();
    t.retain(|_, until| *until > now);
    t.contains_key(key_id)
}

/// 上游给了 `Retry-After` → 记住它，并在**结论刚变化时**落一条可折叠事件。
///
/// `secs` 直接来自上游响应头（已由调用方解析）。非正值忽略：那既可能是上游填了 0，
/// 也可能是解析出的负数，两种都不构成「N 秒后再来」这个信息。
///
/// 🔴 **只在窗口从「无」变「有」时落事件**。同 `balance_gate` 那条教训：
/// 后台/热路径上无条件落事件会把有用事件挤出 `MAX_EVENTS` 环，而
/// `append_event_collapsible` 只合并**紧邻**的上一条 —— 多条 Key 交替时压根挨不上。
pub fn arm(store: &Arc<Store>, category: CategoryType, key_id: &str, secs: i64) {
    if secs <= 0 {
        return;
    }
    let capped = secs.min(MAX_WINDOW_SECS);
    let now = Utc::now().timestamp_millis();
    let until = now + capped * 1000;
    let fresh = {
        let mut t = table().lock();
        t.retain(|_, u| *u > now);
        if t.len() >= MAX_ENTRIES && !t.contains_key(key_id) {
            return;
        }
        // 已在窗口内 → 取更晚的那个（上游可能在窗口内再报一次更长的等待），
        // 但不重复落事件。
        match t.get(key_id).copied() {
            Some(prev) => {
                t.insert(key_id.into(), prev.max(until));
                false
            }
            None => {
                t.insert(key_id.into(), until);
                true
            }
        }
    };
    if !fresh {
        return;
    }
    let name = store.key_name(key_id).unwrap_or_else(|| key_id.to_string());
    let note = if capped < secs {
        format!("（上游要求 {secs}s，已夹到 {capped}s —— 到期后会再试一次）")
    } else {
        String::new()
    };
    store.append_event_collapsible(
        category,
        "failover",
        Some(key_id),
        &format!(
            "{name} · 上游给了 Retry-After {secs}s，接下来 {capped}s 内不再选这条 Key{note}。\
             这**不是**熔断：一次成功不会解除它，只由时间解除。全部 Key 都在窗口内时仍会各试一次。"
        ),
        None,
        Some(format!("quota:{key_id}")),
    );
}

/// 测试专用：清空全表。进程级 static 会跨用例串台（同 `lan_guard` 的
/// `DENY_COUNTER_LOCK` 那条教训），故每条用例先自己清干净。
#[cfg(test)]
pub(crate) fn reset_for_test() {
    table().lock().clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ProviderKey, Protocol};
    use std::path::PathBuf;

    /// 🔴 进程级 static 会跨用例串台（同 `lan_guard` 那条：`DENIED_TOTAL` 让
    /// 「打 3 次、断言差值恰好 3」在同进程另一条用例插进来时红，连跑 3 次红 2 次）。
    /// 本模块的表也是进程级，故全部用例串行 + 各自先清表。
    ///
    /// ⚠️ **光串行化不够，key id 还必须是本模块独有的**（这里用 `qw_` 前缀）。
    /// 第一版用了 `"a"`/`"b"`，全量跑当场红 **8 条** —— `model_pool` / `health` /
    /// `proxy` 的用例也用这种短 id 调 `rank_candidates`，于是**我武装的窗口把别人的
    /// 候选挡掉了**。串行锁只保护本模块内部，管不住同进程别的模块。
    /// 单独跑 `cargo test --lib quota_window` 永远绿，这是那类只在全量下现形的假绿。
    fn lock() -> std::sync::MutexGuard<'static, ()> {
        static L: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let g = L.lock().unwrap_or_else(|e| e.into_inner());
        reset_for_test();
        g
    }

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

    fn key(id: &str, priority: i32) -> ProviderKey {
        ProviderKey {
            id: id.into(),
            category_id: CategoryType::ClaudeCli,
            name: id.into(),
            base_url: "https://api.example.com".into(),
            protocol: Protocol::Anthropic,
            enabled: true,
            priority,
            ..Default::default()
        }
    }

    /// 🔴 本模块存在的理由：429 之后那条 Key 此前**立刻回到候选池首位**。
    #[test]
    fn an_armed_key_drops_out_of_the_candidate_pool() {
        let _g = lock();
        let s = store("quota_pool");
        let keys = vec![key("qw_a", 0), key("qw_b", 1)];
        let (before, _) = crate::proxy::model_pool::rank_candidates(
            &keys,
            CategoryType::ClaudeCli,
            "claude-opus-4-5",
        );
        assert_eq!(before[0].id, "qw_a", "武装前主 Key 排第一");

        arm(&s, CategoryType::ClaudeCli, "qw_a", 30);
        let (after, fallback) = crate::proxy::model_pool::rank_candidates(
            &keys,
            CategoryType::ClaudeCli,
            "claude-opus-4-5",
        );
        assert!(!fallback, "还有 b 可用，不该走兜底");
        assert_eq!(after.len(), 1, "窗口内的 a 必须被剔除：{after:?}");
        assert_eq!(after[0].id, "qw_b");
    }

    /// 全池都在窗口内时**不许**自杀成 503：兜底分支忽略全部运行态门槛。
    #[test]
    fn a_fully_armed_pool_still_falls_back_to_everyone() {
        let _g = lock();
        let s = store("quota_all");
        let keys = vec![key("qw_a", 0), key("qw_b", 1)];
        arm(&s, CategoryType::ClaudeCli, "qw_a", 30);
        arm(&s, CategoryType::ClaudeCli, "qw_b", 30);
        let (c, fallback) = crate::proxy::model_pool::rank_candidates(
            &keys,
            CategoryType::ClaudeCli,
            "claude-opus-4-5",
        );
        assert!(fallback, "全部被挡 → 必须是兜底路径");
        assert_eq!(c.len(), 2, "兜底要把两条都放回来，各撞一次总比 503 好");
        assert_eq!(c[0].id, "qw_a", "兜底继承同一排序");
    }

    /// 🔴 时长必须夹上限：上游填 86400 时把 Key 挡一天是不可接受的。
    #[test]
    fn an_absurd_retry_after_is_capped() {
        let _g = lock();
        let s = store("quota_cap");
        arm(&s, CategoryType::ClaudeCli, "qw_a", 86_400);
        let until = *table().lock().get("qw_a").expect("窗口已武装");
        let secs = (until - Utc::now().timestamp_millis()) / 1000;
        assert!(
            secs <= MAX_WINDOW_SECS && secs > MAX_WINDOW_SECS - 5,
            "必须夹到 {MAX_WINDOW_SECS}s 附近，实际 {secs}s"
        );
        // 事件里要如实说明「上游要的更长、我们夹短了」——否则用户按上游那个数字等，
        // 而我们其实早就再试过了。
        let ev = s.list_all_events();
        assert_eq!(ev.len(), 1);
        assert!(ev[0].detail.contains("86400"), "要写明上游原始值：{}", ev[0].detail);
        assert!(ev[0].detail.contains("夹到"), "要写明我们夹短了：{}", ev[0].detail);
    }

    /// 非正的 Retry-After 不构成「N 秒后再来」这个信息，一律忽略。
    #[test]
    fn a_non_positive_retry_after_arms_nothing() {
        let _g = lock();
        let s = store("quota_zero");
        for secs in [0, -1, -300] {
            arm(&s, CategoryType::ClaudeCli, "qw_a", secs);
            assert!(!active("qw_a"), "secs={secs} 不该武装窗口");
        }
        assert!(s.list_all_events().is_empty(), "没武装就不该落事件");
    }

    /// 窗口内再次收到 Retry-After：取更晚的那个，但**不重复落事件**
    /// （事件环只有 500 条，与路由/故障转移共用 —— 同 balance_gate 那条洪水教训）。
    ///
    /// ⚠️ **必须断言 `repeat` 而不是 `len()`**：这条事件带 collapse key，同 key 再
    /// append 会**折进原来那条**、条数恒为 1。第一版按 `len()` 写，注入「每次都落事件」
    /// 后照样绿 —— 与 B4 那条注入⑧栽的是同一个坑，本仓第二次。
    #[test]
    fn re_arming_extends_the_window_without_a_second_event() {
        let _g = lock();
        let s = store("quota_re");
        arm(&s, CategoryType::ClaudeCli, "qw_a", 10);
        let first = *table().lock().get("qw_a").unwrap();
        arm(&s, CategoryType::ClaudeCli, "qw_a", 60);
        let second = *table().lock().get("qw_a").unwrap();
        assert!(second > first, "更长的等待要覆盖更短的");
        arm(&s, CategoryType::ClaudeCli, "qw_a", 5);
        assert_eq!(*table().lock().get("qw_a").unwrap(), second, "更短的不该缩短窗口");
        let ev = s.list_all_events();
        assert_eq!(ev.len(), 1, "只该有一条事件");
        assert_eq!(ev[0].repeat, 1, "窗口内重复武装不该再落事件（折叠计数必须仍是 1）");
    }

    /// 🔴 **不是熔断**：一次成功不解除它。另一个模型成功不代表配额恢复
    /// （多数中转站按模型或按端点分别限流）。
    #[test]
    fn a_success_does_not_clear_the_window() {
        let _g = lock();
        let s = store("quota_succ");
        arm(&s, CategoryType::ClaudeCli, "qw_a", 60);
        crate::health::record_live_success(&s, "qw_a", Some("claude-opus-4-5"));
        assert!(active("qw_a"), "record_live_success 清的是熔断，不该动配额窗口");
    }

    /// 🔴 **接线判据**：上面全部用例都直接调 `arm`，而「转发路径到底有没有调它」
    /// 是另一回事。而且失败分支有**两条**（流式非 2xx / 非流式非 2xx），
    /// 只挂一条的表现是静默的 —— 那种客户端（stream:true 或 false）永远学不到配额。
    /// 同 `rectify_thinking_signature` 那条注释记的教训：挂一条必然漏掉另一条。
    #[test]
    fn both_retry_after_paths_must_arm_the_window() {
        let src = include_str!("proxy.rs");
        let prod = crate::proxy::custom_headers::production_code_only(src);
        let n = prod.matches("quota_window::arm(").count();
        assert_eq!(
            n, 2,
            "流式与非流式两条「上游给了 Retry-After」分支都必须武装窗口，实际 {n} 处"
        );
        // 两处都必须紧跟在 retry_after_hint 那一行之后（同一个 if 里）——
        // 挪到别处就可能落在「只有部分状态码走到」的分支里。
        assert_eq!(
            prod.matches("retry_after_hint.map_or(s, |cur: i64| cur.min(s)));\r\n")
                .count()
                .max(prod.matches("retry_after_hint.map_or(s, |cur: i64| cur.min(s)));\n").count()),
            2,
            "retry_after_hint 的两处更新形态变了，本判据要跟着改"
        );
    }
}


