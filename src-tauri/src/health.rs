//! 健康检查与熔断（FR-011 / FR-012）。
//! 混合策略（arch-decisions §6）：主动探测 + 缓存 + 熔断态派生。

use crate::model::{HealthState, HealthStatus};
use crate::store::Store;
use crate::upstream;
use chrono::Utc;
use std::sync::Arc;

/// 熔断阈值：连续失败达到即熔断
const BREAKER_THRESHOLD: u32 = 3;
/// 熔断冷却时长（毫秒）
const BREAKER_COOLDOWN_MS: i64 = 60_000;
/// 「近期有真实转发成功」宽限窗口（毫秒）：窗口内后台探测失败不熔断，因真实流量证明该 Key 可用。
const LIVE_SUCCESS_GRACE_MS: i64 = 120_000;

/// 对单个 Key 执行一次健康检查并更新其状态。
pub async fn check_one(store: &Arc<Store>, key_id: &str) {
    let Some(key) = store.get_key(key_id) else { return };
    let secret = store.secrets.read().get(key_id).ok().flatten();
    let Some(secret) = secret else {
        // 无密钥无法探测，标记未知
        let _ = store.update_health(
            key_id,
            HealthState { status: HealthStatus::Unknown, ..Default::default() },
        );
        return;
    };

    // 探测方式按设置切换：默认轻量连通探测；开启后用真实补全探测（与业务一致，消耗少量额度）。
    let real_probe = store.get_settings().health_probe_real_completion;
    let (ok, latency, err) = if real_probe {
        upstream::health_probe_real(&key, &secret).await
    } else {
        upstream::health_probe(&key, &secret).await
    };
    let now = Utc::now().timestamp_millis();

    // 探测/熔断分离（借鉴 cc-switch）：探测**只观测可达性**，写 status + latency，
    // 绝不碰 breaker_until / fail_count。可达即 Up、连接层不可达即 Down；此 status 仅供
    // UI 展示，不参与路由门槛（is_candidate 只看熔断窗口）。故一次探测端点 401 / 探测模型名
    // 不匹配，再也不会把一个真实流量本可成功的 Key 踢出路由——熔断只由真实流量驱动。
    // 探测可能长达 30s，期间 health 可能已被真实流量更新 breaker/fail_count，故读最新快照，
    // 只覆盖 status/latency/last_checked 三个探测字段，其余原样保留。
    let prev = store.get_key(key_id).map(|k| k.health).unwrap_or_default();

    // 探测失败落日志（归「健康检查」分组）——供排查，但不触发任何熔断动作。
    if !ok {
        if let Some(reason) = &err {
            store.append_event(
                key.category_id,
                "health",
                Some(key_id),
                &format!("{} 健康探测失败（仅影响可达性展示，不熔断）：{reason}", key.name),
            );
        }
    }

    let status = if ok { HealthStatus::Up } else { HealthStatus::Down };

    let _ = store.update_health(
        key_id,
        HealthState {
            status,
            last_checked: Some(now),
            latency_ms: Some(latency),
            // 熔断相关字段原样保留——探测不参与熔断。
            fail_count: prev.fail_count,
            breaker_until: prev.breaker_until,
            last_live_success: prev.last_live_success,
        },
    );
}

/// 把一次「实时转发失败」计入熔断器（连接失败 / 上游非 2xx）。
///
/// 与后台探测共用 fail_count / breaker_until：连续失败累加，达到阈值则熔断
/// BREAKER_COOLDOWN_MS。这解决了「失败的 Key 每个请求都从头重试」——熔断窗口内
/// is_candidate 会跳过它，失败 Key 从「每次请求都试」降为「每 60s 才试一次」。
///
/// 注意：只动 fail_count / breaker_until，不改 status（status 交给后台探测维护，
/// 避免设 Down 后 is_candidate 永久拒绝、而实时流量又进不来无法恢复的死锁）。
pub fn record_live_failure(store: &Arc<Store>, key_id: &str) {
    let prev = store.get_key(key_id).map(|k| k.health).unwrap_or_default();
    let now = Utc::now().timestamp_millis();
    let fail_count = prev.fail_count.saturating_add(1);
    let breaker_until = if fail_count >= BREAKER_THRESHOLD {
        Some(now + BREAKER_COOLDOWN_MS)
    } else {
        prev.breaker_until
    };
    let _ = store.update_health(
        key_id,
        HealthState {
            status: prev.status,
            last_checked: prev.last_checked,
            latency_ms: prev.latency_ms,
            fail_count,
            breaker_until,
            last_live_success: prev.last_live_success,
        },
    );
}

/// 把一次「实时转发成功」计入熔断器：清零 fail_count、解除熔断，标记 Up。
/// 让恢复的 Key 立即回到候选池（无需等下一次后台探测）。
pub fn record_live_success(store: &Arc<Store>, key_id: &str) {
    let prev = store.get_key(key_id).map(|k| k.health).unwrap_or_default();
    let now = Utc::now().timestamp_millis();
    // 已健康、无熔断残留、且 last_live_success 仍新鲜：无需写盘（避免高频请求每次都写）。
    // 但若时间戳偏旧（超过宽限窗口一半），仍刷新一次，让「近期成功」宽限窗口在持续流量下不失效。
    let fresh = prev
        .last_live_success
        .map(|t| now - t < LIVE_SUCCESS_GRACE_MS / 2)
        .unwrap_or(false);
    if prev.fail_count == 0 && prev.breaker_until.is_none() && prev.status == HealthStatus::Up && fresh
    {
        return;
    }
    let _ = store.update_health(
        key_id,
        HealthState {
            status: HealthStatus::Up,
            last_checked: prev.last_checked,
            latency_ms: prev.latency_ms,
            fail_count: 0,
            breaker_until: None,
            last_live_success: Some(now),
        },
    );
}

/// 判断某 Key 当前是否可作为路由候选。
///
/// 探测/熔断分离（借鉴 cc-switch）：路由门槛**只看熔断窗口**，不看探测出的 `status`。
/// 探测（`check_one`）只观测「可达性」用于 UI 展示，绝不影响路由——因为探测端点
/// （/models 返 401、探测模型名与业务不符）常与真实业务不一致，让它决定路由会误杀
/// 一个真实流量本可成功的 Key。熔断只由真实转发流量的连续失败驱动（record_live_failure）。
pub fn is_candidate(health: &HealthState) -> bool {
    let now = Utc::now().timestamp_millis();
    // 熔断中 → 不可用；其余（含探测判 Down 的）一律给机会——真实流量才是可用性裁判。
    match health.breaker_until {
        Some(until) => until <= now,
        None => true,
    }
}

/// 从一组 Key 选路由候选：取「未熔断」的。
/// 熔断只由真实流量驱动，故全被熔断意味着这些 Key 真实转发都在连续失败——
/// 但仍返回它们（忽略熔断窗口兜底），避免单 Key / 全熔断时整服务直接 503：
/// 有一个能试的 Key 也比不可用强（熔断本为「多 Key 快速切换」设计，无处可切时不应自杀）。
/// 返回 (候选列表, 是否触发了兜底)。
pub fn select_candidates(keys: Vec<crate::model::ProviderKey>) -> (Vec<crate::model::ProviderKey>, bool) {
    let primary: Vec<_> = keys.iter().filter(|k| is_candidate(&k.health)).cloned().collect();
    if !primary.is_empty() {
        return (primary, false);
    }
    // 全熔断兜底：忽略熔断窗口，全部重新纳入（探测态不参与门槛，故不再排除 Down）。
    let used_fallback = !keys.is_empty();
    (keys, used_fallback)
}

/// 后台定时健康检查（对某分类**已启用**的 Key）。
/// 只探测启用的 Key：禁用的 Key 不参与路由，对它探测既白耗额度，又会因失败被判 Down/熔断，
/// 在界面上留下无意义的「熔断中/不可用」状态污染。
pub async fn check_category(store: &Arc<Store>, category: crate::model::CategoryType) {
    let keys = store.enabled_keys_sorted(category);
    for k in keys {
        check_one(store, &k.id).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hs(status: HealthStatus, fail_count: u32, breaker_until: Option<i64>) -> HealthState {
        HealthState { status, fail_count, breaker_until, ..Default::default() }
    }

    #[test]
    fn candidate_gated_only_by_breaker_not_probe_status() {
        // 探测/熔断分离后：路由门槛只看熔断窗口，探测出的 status 一律不影响候选资格。
        // 未熔断时，即便探测判 Down（探测端点 401 / 模型名不符）也应给机会——真实流量才是裁判。
        assert!(is_candidate(&hs(HealthStatus::Up, 0, None)));
        assert!(is_candidate(&hs(HealthStatus::Unknown, 0, None)), "未探测应给机会");
        assert!(is_candidate(&hs(HealthStatus::Checking, 0, None)));
        assert!(
            is_candidate(&hs(HealthStatus::Down, 3, None)),
            "分离后：探测判 Down 但未熔断 → 仍是候选（探测不再决定路由）"
        );
    }

    #[test]
    fn candidate_respects_breaker_window() {
        let now = Utc::now().timestamp_millis();
        // 熔断未到期 → 即便 status 是 Up 也不可用
        assert!(!is_candidate(&hs(HealthStatus::Up, 0, Some(now + 60_000))));
        // 熔断已过期 → 恢复可用
        assert!(is_candidate(&hs(HealthStatus::Up, 0, Some(now - 1))));
    }

    #[test]
    fn select_candidates_falls_back_when_all_tripped() {
        let now = Utc::now().timestamp_millis();
        let mut tripped = key("k");
        tripped.health = hs(HealthStatus::Up, 3, Some(now + 60_000)); // 熔断中但非 Down

        // 唯一 Key 被熔断：严格判定为空 → 触发兜底，忽略熔断仍返回它。
        let (cands, used_fallback) = select_candidates(vec![tripped]);
        assert_eq!(cands.len(), 1, "全熔断应兜底返回，避免单 Key 自杀");
        assert!(used_fallback, "应标记触发了熔断兜底");
    }

    #[test]
    fn select_candidates_prefers_healthy_no_fallback() {
        let now = Utc::now().timestamp_millis();
        let mut healthy = key("ok");
        healthy.health = hs(HealthStatus::Up, 0, None);
        let mut tripped = key("bad");
        tripped.health = hs(HealthStatus::Up, 3, Some(now + 60_000));

        // 有健康 Key 时只取健康的，不触发兜底、不把熔断中的混进来。
        let (cands, used_fallback) = select_candidates(vec![healthy, tripped]);
        assert_eq!(cands.len(), 1);
        assert_eq!(cands[0].id, "ok");
        assert!(!used_fallback);
    }

    #[test]
    fn probe_down_key_still_routable_when_not_tripped() {
        // 探测/熔断分离后：探测判 Down（仅可达性观测）但未被真实流量熔断的 Key，
        // 仍应是候选——探测端点与真实业务常不一致，不能凭它把 Key 踢出路由。
        let down = {
            let mut k = key("down");
            k.health = hs(HealthStatus::Down, 0, None);
            k
        };
        let (cands, used_fallback) = select_candidates(vec![down]);
        assert_eq!(cands.len(), 1, "探测 Down 但未熔断的 Key 仍应可路由");
        assert!(!used_fallback, "未熔断 → 走主路径，不是兜底");
    }

    // ---- 实时失败/成功喂熔断器 ----

    use crate::model::{CategoryType, KeyParams, Protocol, ProviderKey};
    use std::sync::atomic::{AtomicU64, Ordering};

    fn temp_store() -> Arc<Store> {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir()
            .join(format!("synaroute_health_test_{}_{}", std::process::id(), seq));
        std::fs::create_dir_all(&dir).unwrap();
        Arc::new(Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap())
    }

    fn key(id: &str) -> ProviderKey {
        ProviderKey {
            id: id.into(),
            category_id: CategoryType::ClaudeCli,
            name: id.into(),
            vendor: "test".into(),
            base_url: "https://x".into(),
            protocol: Protocol::Anthropic,
            has_secret: false,
            enabled: true,
            priority: 0,
            headers_json: None,
            params: KeyParams::default(),
            models: vec![],
            mappings: vec![],
            default_model: None,
            tier_haiku: None,
            tier_sonnet: None,
            tier_opus: None,
            health: HealthState::default(),
        }
    }

    #[test]
    fn live_failures_trip_breaker_at_threshold() {
        let store = temp_store();
        store.upsert_key(key("k")).unwrap();

        // 前两次失败：累加 fail_count，但未到阈值 → 仍是候选。
        record_live_failure(&store, "k");
        record_live_failure(&store, "k");
        let h = store.get_key("k").unwrap().health;
        assert_eq!(h.fail_count, 2);
        assert!(is_candidate(&h), "阈值前仍应可尝试");

        // 第三次失败：达到阈值 → 熔断，不再是候选。
        record_live_failure(&store, "k");
        let h = store.get_key("k").unwrap().health;
        assert_eq!(h.fail_count, 3);
        assert!(h.breaker_until.is_some(), "达阈值应熔断");
        assert!(!is_candidate(&h), "熔断窗口内应跳过");
    }

    #[test]
    fn live_success_clears_breaker() {
        let store = temp_store();
        store.upsert_key(key("k")).unwrap();

        // 先熔断。
        for _ in 0..3 {
            record_live_failure(&store, "k");
        }
        assert!(!is_candidate(&store.get_key("k").unwrap().health), "先确认已熔断");

        // 一次成功：清零 fail_count、解除熔断、标记 Up → 立即恢复候选。
        record_live_success(&store, "k");
        let h = store.get_key("k").unwrap().health;
        assert_eq!(h.fail_count, 0);
        assert!(h.breaker_until.is_none());
        assert_eq!(h.status, HealthStatus::Up);
        assert!(is_candidate(&h), "成功后应立即恢复");
    }

    #[test]
    fn live_success_records_grace_timestamp() {
        let store = temp_store();
        store.upsert_key(key("k")).unwrap();
        let before = Utc::now().timestamp_millis();

        record_live_success(&store, "k");
        let h = store.get_key("k").unwrap().health;
        let ts = h.last_live_success.expect("应记录真实成功时间戳，供探测宽限窗口判定");
        assert!(ts >= before, "时间戳应为记录时刻");

        // 宽限窗口内：探测失败不应熔断该 Key（在 check_one 里用 last_live_success 判定）。
        let now = Utc::now().timestamp_millis();
        assert!(now - ts < LIVE_SUCCESS_GRACE_MS, "刚记录应在宽限窗口内");
    }
}
