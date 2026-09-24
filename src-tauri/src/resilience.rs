//! 可靠性总览（把五层弹性做成**可见**的一块面板）。
//!
//! 挂 `#[path]` 在 `diagnostics` 下（`crate::diagnostics::resilience`），理由同 `balance_gate`
//! 挂 `health`：纯为棘轮 —— `lib.rs` 余量为 0，而 `diagnostics.rs` 有余量，且它本就已经把
//! 这里要读的每一位状态在文本报告里汇过一遍（见 `build_diagnostics_report` 的「Key 健康状态」段）。
//!
//! # 它治什么
//!
//! 代理的五层弹性（Key 级熔断 / 单模型锁定 / 配额窗口 / 余额闸门 / 每 Key 并发上限）此前对用户
//! **完全不可见** —— 它们默默把请求从坏 Key 移开、用户只看到「一切正常」。好处是无感，代价是
//! 排障时看不出「这条 Key 现在到底怎么了、为什么没在用它」。本面板把这些运行态一次性摊开：
//! 每分类一张「正常 / 熔断 / 限流 / 余额耗尽 / 超预算」的汇总，加逐 Key 的状态明细。
//!
//! # 🔴 只读快照，绝不改状态
//!
//! 全部数据来自**当前**内存态（`HealthState` + 三张进程级表 + 配置里的预算），现算不落盘、
//! 不碰热路径。故它与 `candidates_for` 的判据天然一致（读的是同几个函数），不会出现
//! 「面板显示正常、路由却在跳过它」这种两份口径的漂移。
//!
//! # ⚠️ 边界：这是「此刻的状态」，不是「历史累计」
//!
//! 「近期活动」那几个数字来自 500 条的事件环（`MAX_EVENTS`），会滚动 —— 故如实标成
//! **近期**（近 N 条事件里的构成），不冒充「总共为你挡下了多少次」。做后者要独立持久化计数器
//! （同 `usage_totals` 的理由：不能从会滚动的环里现算累计），那是另一轮的事。

use crate::model::{CategoryType, HealthStatus};
use crate::store::Store;
use serde::Serialize;

/// 逐 Key 的运行态明细。字段都是「此刻」的读数。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct KeyResilience {
    pub(crate) key_id: String,
    pub(crate) key_name: String,
    pub(crate) enabled: bool,
    pub(crate) status: HealthStatus,
    pub(crate) fail_count: u32,
    /// Key 级熔断窗口此刻仍生效（`breaker_until > now`）。**不能用 `breaker_until.is_some()`**：
    /// 那个字段到期后仍有残留，只有一次成功才清（同 `health::breaker_window_active` 的口径）。
    pub(crate) breaker_active: bool,
    /// 上游用 `Retry-After` 开的配额窗口此刻生效（进程级，不进 `HealthState`）。
    pub(crate) rate_limited: bool,
    /// 余额闸门判「确定耗尽」（查不到 ≠ 耗尽，见 `balance_gate`）。
    pub(crate) balance_exhausted: bool,
    /// 累计估算花费已达用户设的预算（软上限，仍兜底，见 `usage_cost::over_budget_ids`）。
    pub(crate) over_budget: bool,
    pub(crate) in_flight: usize,
    pub(crate) max_in_flight: usize,
}

/// 一个分类的汇总 + 逐 Key 明细。计数只统计**已启用**的 Key（停用的不参与路由，
/// 把它们算进「正常/异常」会误导 —— 它压根不在候选池里）。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CategoryResilience {
    pub(crate) category_id: CategoryType,
    pub(crate) proxy_running: bool,
    pub(crate) enabled_count: usize,
    /// 既没熔断、没被限流、余额没耗尽、状态也不是 Down 的启用 Key 数。
    pub(crate) healthy_count: usize,
    pub(crate) breaker_count: usize,
    pub(crate) rate_limited_count: usize,
    pub(crate) exhausted_count: usize,
    pub(crate) over_budget_count: usize,
    pub(crate) keys: Vec<KeyResilience>,
}

/// 近期事件构成（来自会滚动的 500 条环）。**是「近期」不是「累计」**，见模块头边界。
#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RecentActivity {
    pub(crate) total_events: usize,
    pub(crate) routes: usize,
    pub(crate) failovers: usize,
    pub(crate) errors: usize,
    pub(crate) warnings: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ResilienceOverview {
    pub(crate) categories: Vec<CategoryResilience>,
    pub(crate) recent: RecentActivity,
}

/// 组装当前快照（纯只读，可脱离 tauri 单测）。
pub(crate) fn build(store: &Store) -> ResilienceOverview {
    let now = chrono::Utc::now().timestamp_millis();
    let running: std::collections::HashSet<CategoryType> =
        store.get_settings().proxy_running_categories.into_iter().collect();
    let mut categories = Vec::new();
    for cat in CategoryType::ALL {
        let cfg_keys = store.list_keys(cat);
        if cfg_keys.is_empty() {
            continue;
        }
        // 与 `candidates_for` 同一处判据：现算超预算集合，不另立口径。
        let over = crate::usage_cost::over_budget_ids(store, cat);
        let keys: Vec<KeyResilience> = cfg_keys
            .iter()
            .map(|k| KeyResilience {
                key_id: k.id.clone(),
                key_name: k.name.clone(),
                enabled: k.enabled,
                status: k.health.status,
                fail_count: k.health.fail_count,
                breaker_active: k.health.breaker_until.is_some_and(|t| t > now),
                rate_limited: crate::health::quota_window::active(&k.id),
                balance_exhausted: crate::health::balance_gate::verdict(k)
                    == crate::health::balance_gate::Verdict::Exhausted,
                over_budget: over.contains(&k.id),
                in_flight: crate::health::concurrency::in_flight(&k.id),
                max_in_flight: crate::health::concurrency::MAX_INFLIGHT_PER_KEY,
            })
            .collect();
        let en: Vec<&KeyResilience> = keys.iter().filter(|r| r.enabled).collect();
        categories.push(CategoryResilience {
            category_id: cat,
            proxy_running: running.contains(&cat),
            enabled_count: en.len(),
            healthy_count: en
                .iter()
                .filter(|r| {
                    !r.breaker_active
                        && !r.rate_limited
                        && !r.balance_exhausted
                        && r.status != HealthStatus::Down
                })
                .count(),
            breaker_count: en.iter().filter(|r| r.breaker_active).count(),
            rate_limited_count: en.iter().filter(|r| r.rate_limited).count(),
            exhausted_count: en.iter().filter(|r| r.balance_exhausted).count(),
            over_budget_count: en.iter().filter(|r| r.over_budget).count(),
            keys,
        });
    }
    let events = store.list_all_events();
    let mut recent = RecentActivity { total_events: events.len(), ..Default::default() };
    for e in &events {
        match e.kind.as_str() {
            "route" => recent.routes += 1,
            "failover" => recent.failovers += 1,
            "error" => recent.errors += 1,
            "warning" => recent.warnings += 1,
            _ => {}
        }
    }
    ResilienceOverview { categories, recent }
}

/// 可靠性总览（只读快照）。前端「可靠性」页轮询它。
#[tauri::command]
pub async fn resilience_overview(
    state: tauri::State<'_, crate::AppState>,
) -> crate::error::AppResult<ResilienceOverview> {
    Ok(build(&state.store))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::tests::{key, temp_store};

    /// 每一层弹性都要如实汇总。用**独有 id**（`res_*`）避免与别处（如 balance_gate 的 "broke"）
    /// 撞进程级表 —— 那是本仓记过的跨用例串台类。
    #[test]
    fn overview_rolls_up_each_resilience_layer() {
        let (store, dir) = temp_store("resilience");
        for id in ["res_ok", "res_broke", "res_capped"] {
            let mut k = key(CategoryType::ClaudeCli);
            k.id = id.into();
            if id == "res_capped" {
                k.default_model = Some("claude-sonnet-4-5".into());
                k.budget_usd = Some(0.0001); // 极低，一笔用量就超
            }
            store.upsert_key(k).unwrap();
        }
        // res_broke 熔断中。
        let future = chrono::Utc::now().timestamp_millis() + 60_000;
        store.mutate_health("res_broke", |h| {
            h.breaker_until = Some(future);
            true
        }).unwrap();
        // res_capped 跑一笔用量 → 累计花费超过它的 $0.0001 预算。
        store.append_event_full(
            CategoryType::ClaudeCli,
            "route",
            Some("res_capped"),
            "转发",
            None,
            None,
            Some(crate::upstream::TokenUsage { input: 100_000, output: 10_000, ..Default::default() }),
        );

        let ov = build(&store);
        let cli = ov
            .categories
            .iter()
            .find(|c| c.category_id == CategoryType::ClaudeCli)
            .expect("有 claude-cli 分类");
        let find = |id: &str| cli.keys.iter().find(|k| k.key_id == id).unwrap();

        assert_eq!(cli.enabled_count, 3);
        assert_eq!(cli.breaker_count, 1, "熔断中的那条要汇总进去");
        assert_eq!(cli.over_budget_count, 1, "超预算的那条要汇总进去");
        assert!(find("res_broke").breaker_active);
        assert!(!find("res_ok").breaker_active);
        assert!(find("res_capped").over_budget);
        // 🔴 超预算 ≠ 不健康：healthy 只排除熔断/限流/余额耗尽/Down。res_capped 只是软上限超了、
        // 健康态没问题，仍算健康；只有 res_broke（熔断）不健康 → healthy = ok + capped = 2。
        assert_eq!(cli.healthy_count, 2, "只有熔断的那条不健康，超预算的仍健康");

        std::fs::remove_dir_all(&dir).ok();
    }
}
