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
        let (ok, latency) = upstream::health_probe(&key, &secret).await;
        (ok, latency, (!ok).then(|| "连通探测失败（连接层错误或 401/403）".to_string()))
    };
    let now = Utc::now().timestamp_millis();

    // 探测可能长达 30s，期间 health 可能已被另一次检查（后台定时 / 前端手动）更新。
    // 用探测「开始前」的旧快照会覆盖更新的结果（例如把刚探测成功的 Key 又基于旧 fail_count
    // 熔断掉）。故此处重新读取最新 health 作为基线，把 TOCTOU 窗口从 30s 缩到微秒级。
    let prev = store.get_key(key_id).map(|k| k.health).unwrap_or_default();
    let fail_count = if ok { 0 } else { prev.fail_count + 1 };

    // 探测失败落日志（归「健康检查」分组）——旧实现丢弃了失败原因，导致探测失败静默、无从排查。
    if !ok {
        if let Some(reason) = &err {
            store.append_event(
                key.category_id,
                "health",
                Some(key_id),
                &format!("{} 健康探测失败：{reason}", key.name),
            );
        }
    }

    // 「近期有真实转发成功」的宽限窗口：真实流量才是可用性的最终裁判。若该 Key 在最近
    // GRACE 内成功服务过真实请求，则后台探测失败**不熔断**——避免探测端点（如 /models
    // 返 401、或探测模型名与业务不一致）单方面把一个正在成功服务的 Key 熔断掉。
    let recently_served = prev
        .last_live_success
        .map(|t| now - t < LIVE_SUCCESS_GRACE_MS)
        .unwrap_or(false);

    // 熔断态派生
    let breaker_until = if ok {
        None
    } else if fail_count >= BREAKER_THRESHOLD && !recently_served {
        Some(now + BREAKER_COOLDOWN_MS)
    } else {
        prev.breaker_until
    };

    // 探测失败但近期有真实成功 → 不判 Down（否则 is_candidate 会拒），保持原状态。
    let status = if ok {
        HealthStatus::Up
    } else if recently_served {
        prev.status
    } else {
        HealthStatus::Down
    };

    if !ok && recently_served {
        store.append_event(
            key.category_id,
            "health",
            Some(key_id),
            &format!("{} 探测失败，但近期有真实请求成功，暂不熔断", key.name),
        );
    }

    let _ = store.update_health(
        key_id,
        HealthState {
            status,
            last_checked: Some(now),
            latency_ms: Some(latency),
            fail_count,
            breaker_until,
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

/// 判断某 Key 当前是否可作为路由候选（未熔断且非明确不可用）。
pub fn is_candidate(health: &HealthState) -> bool {
    let now = Utc::now().timestamp_millis();
    // 熔断中 → 不可用
    if let Some(until) = health.breaker_until {
        if until > now {
            return false;
        }
    }
    // down 明确不可用；up/unknown/checking 都允许尝试（unknown 表示尚未探测，给机会）
    !matches!(health.status, HealthStatus::Down)
}

/// 忽略熔断窗口的候选判定（仅排除 Down）：用于「全熔断兜底」。
/// 当所有候选都在熔断窗口内（如单 Key 场景下该 Key 刚被熔断），严格 is_candidate 会返回空、
/// 使整个服务不可用。此时退而求其次：无视熔断，只要不是明确 Down 就仍给机会尝试——
/// 有一个能用的 Key 也比直接 503 强（熔断本是为「多 Key 快速切换」设计，无处可切时不应自杀）。
pub fn is_candidate_ignoring_breaker(health: &HealthState) -> bool {
    !matches!(health.status, HealthStatus::Down)
}

/// 从一组 Key 选路由候选：优先取「未熔断」的；若全被熔断（候选为空），
/// 则忽略熔断窗口兜底（只排除 Down），避免单 Key / 全熔断时整服务不可用。
/// 返回 (候选列表, 是否触发了兜底)。
pub fn select_candidates(keys: Vec<crate::model::ProviderKey>) -> (Vec<crate::model::ProviderKey>, bool) {
    let primary: Vec<_> = keys.iter().filter(|k| is_candidate(&k.health)).cloned().collect();
    if !primary.is_empty() {
        return (primary, false);
    }
    // 全熔断兜底：忽略熔断窗口，只要不是 Down 就重新纳入。
    let fallback: Vec<_> = keys
        .into_iter()
        .filter(|k| is_candidate_ignoring_breaker(&k.health))
        .collect();
    let used_fallback = !fallback.is_empty();
    (fallback, used_fallback)
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
    fn candidate_allows_up_unknown_checking_rejects_down() {
        assert!(is_candidate(&hs(HealthStatus::Up, 0, None)));
        assert!(is_candidate(&hs(HealthStatus::Unknown, 0, None)), "未探测应给机会");
        assert!(is_candidate(&hs(HealthStatus::Checking, 0, None)));
        assert!(!is_candidate(&hs(HealthStatus::Down, 3, None)), "Down 明确不可用");
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
    fn select_candidates_excludes_down_even_in_fallback() {
        // 明确 Down 的 Key 即使在兜底阶段也不纳入（探测确认过不可用）。
        let down = {
            let mut k = key("down");
            k.health = hs(HealthStatus::Down, 5, None);
            k
        };
        let (cands, used_fallback) = select_candidates(vec![down]);
        assert!(cands.is_empty(), "Down 不参与兜底");
        assert!(!used_fallback);
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
