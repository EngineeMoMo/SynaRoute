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

    let (ok, latency) = upstream::health_probe(&key, &secret).await;
    let now = Utc::now().timestamp_millis();

    let prev = key.health.clone();
    let fail_count = if ok { 0 } else { prev.fail_count + 1 };

    // 熔断态派生
    let breaker_until = if !ok && fail_count >= BREAKER_THRESHOLD {
        Some(now + BREAKER_COOLDOWN_MS)
    } else if ok {
        None
    } else {
        prev.breaker_until
    };

    let status = if ok { HealthStatus::Up } else { HealthStatus::Down };

    let _ = store.update_health(
        key_id,
        HealthState {
            status,
            last_checked: Some(now),
            latency_ms: Some(latency),
            fail_count,
            breaker_until,
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

/// 后台定时全量健康检查（对某分类所有 Key）。
pub async fn check_category(store: &Arc<Store>, category: crate::model::CategoryType) {
    let keys = store.list_keys(category);
    for k in keys {
        check_one(store, &k.id).await;
    }
}
