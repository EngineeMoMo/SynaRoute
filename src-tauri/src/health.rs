//! 健康检查与熔断（FR-011 / FR-012）。
//! 混合策略（arch-decisions §6）：主动探测 + 缓存 + 熔断态派生。

use crate::model::{HealthState, HealthStatus};
use crate::store::Store;
use crate::upstream;
use chrono::Utc;
use std::sync::Arc;

/// 发系统通知（熔断/恢复告警）。非 test 走 `notification::notify`；test 下是 no-op
/// （notification 模块整体 `#[cfg(not(test))]`，测试进程不链接通知插件的 WinRT 依赖）。
#[cfg(not(test))]
fn notify(title: &str, body: &str) {
    crate::notification::notify(title, body);
}
#[cfg(test)]
fn notify(_title: &str, _body: &str) {}

/// 熔断阈值：连续失败达到即熔断
const BREAKER_THRESHOLD: u32 = 3;
/// 熔断冷却时长（毫秒）
const BREAKER_COOLDOWN_MS: i64 = 60_000;
/// 「近期有真实转发成功」宽限窗口（毫秒）：窗口内后台探测失败不熔断，因真实流量证明该 Key 可用。
const LIVE_SUCCESS_GRACE_MS: i64 = 120_000;

// 「从测试消息列表随机取一条作为真实补全探测 prompt」的逻辑已移入 `Store::probe_message_if_real`：
// 那里能在**一次读锁内**完成「读开关 + 随机选取」，避免把整个消息列表克隆出来
// （探测是每轮 × 每 Key 调用）。此处不再保留重复实现。
// 列表为空（用户未配置）时回退内置 "hi"，保持旧行为；空白项过滤掉后仍为空也回退。

/// 对单个 Key 执行一次健康检查并更新其状态。
pub async fn check_one(store: &Arc<Store>, key_id: &str) {
    let Some(key) = store.get_key(key_id) else { return };
    // 锁定态（主口令未解锁）不改任何健康状态：取不到密钥不代表 Key 坏了。
    // 前端「手动检测」也走这里，故必须在这一层挡住，而不只是在 check_category。
    if store.secrets.read().is_locked() {
        return;
    }
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
    //
    // 用窄读取器而非 `get_settings()`：后者克隆整份 `AppSettings`（3 个 HashMap + 2 个 Vec），
    // 而这里只要一个 bool 加（可能的）一条测试消息。探测是「每轮 × 每 Key」调用，
    // 与 `request_log_enabled` 等热路径读取同一处理原则。
    let (ok, latency, err) = match store.probe_message_if_real() {
        // Some(msg) = 开启了真实补全探测，msg 已从列表里随机取好（空列表回退内置 "hi"）
        Some(msg) => upstream::health_probe_real(&key, &secret, &msg).await,
        None => upstream::health_probe(&key, &secret).await,
    };
    let now = Utc::now().timestamp_millis();

    // 探测/熔断分离（借鉴 cc-switch）：探测**只观测可达性**，写 status + latency，
    // 绝不碰 breaker_until / fail_count。可达即 Up、连接层不可达即 Down；此 status 仅供
    // UI 展示，不参与路由门槛（is_candidate 只看熔断窗口）。故一次探测端点 401 / 探测模型名
    // 不匹配，再也不会把一个真实流量本可成功的 Key 踢出路由——熔断只由真实流量驱动。
    // 探测可能长达 8s（`fast_timeout` 封顶），期间 health 可能已被真实流量更新 breaker/fail_count，
    // 故读最新快照，只覆盖 status/latency/last_checked 三个探测字段，其余原样保留。
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
    let now = Utc::now().timestamp_millis();
    // 先读「是否已熔断」：熔断是状态跃迁，只在「未熔断 → 刚熔断」时发一次系统通知
    // （不是每次失败都发，否则刷屏）。这条读与下面的 mutate_health 之间有个极窄窗口，
    // 并发下可能漏报一次 —— 通知本就是尽力而为（events::notify 失败不致命），可接受。
    // 用窄读取器（只取 bool），不用 get_key 整份克隆——每失败都走这里，热路径。
    let was_tripped = store.key_breaker_tripped(key_id);
    // 走 `mutate_health` 而非「get_key → 算 → update_health」：后者跨两个临界区，
    // 两个并发失败会都读到同一个 prev.fail_count 各自 +1 写回，导致熔断迟钝
    // （详见 Store::mutate_health 的文档）。这里 read-modify-write 在同一个写锁内完成。
    let _ = store.mutate_health(key_id, |h| {
        h.fail_count = h.fail_count.saturating_add(1);
        if h.fail_count >= BREAKER_THRESHOLD {
            h.breaker_until = Some(now + BREAKER_COOLDOWN_MS);
        }
        // status / last_checked / latency_ms / last_live_success 一概不动：
        // status 交给后台探测维护（改这里会造成「设 Down 后 is_candidate 永久拒绝、
        // 实时流量又进不来无法恢复」的死锁，见函数头注释）。
        //
        // 落盘判据与 update_health 保持一致：只有熔断相关字段变了才写。fail_count 每次
        // 必变，故这里恒为 true —— 与旧行为等价（旧代码 fail_count 变化即触发 persist）。
        true
    });
    // 熔断武装跃迁：发系统通知 + 记一条告警事件。
    let now_tripped = store.key_breaker_tripped(key_id);
    if now_tripped && !was_tripped {
        let name = store.key_name(key_id).unwrap_or_else(|| key_id.to_string());
        notify(
            "Key 已熔断",
            &format!("「{name}」连续失败，已暂停使用 60 秒。其他 Key 将自动接管。"),
        );
    }
}

/// 把一次「实时转发成功」计入熔断器：清零 fail_count、解除熔断，标记 Up。
/// 让恢复的 Key 立即回到候选池（无需等下一次后台探测）。
pub fn record_live_success(store: &Arc<Store>, key_id: &str) {
    let now = Utc::now().timestamp_millis();
    // mutate 前读「是否已熔断」：与 mutate 后的状态对比，判断「本次是否真的发生了
    // 熔断解除」。若在 mutate 后才读，无法区分「本来就没熔断」与「刚解除」——会误发
    // 「已恢复」通知给一个一直健康的 Key。窄读取器只取 bool，热路径零整份克隆。
    let was_tripped = store.key_breaker_tripped(key_id);
    let _ = store.mutate_health(key_id, |h| {
        // 已健康、无熔断残留、且 last_live_success 仍新鲜：内存与磁盘都无需动
        // （避免高频请求每次都写盘——这是热路径上最值钱的一处抑制，见 Store::update_health）。
        // 但若时间戳偏旧（超过宽限窗口一半），仍刷新一次，让「近期成功」宽限窗口
        // 在持续流量下不失效。
        let fresh = h
            .last_live_success
            .map(|t| now - t < LIVE_SUCCESS_GRACE_MS / 2)
            .unwrap_or(false);
        if h.fail_count == 0 && h.breaker_until.is_none() && h.status == HealthStatus::Up && fresh {
            return false; // 稳态成功路径：**完全不落盘**
        }
        // 需要写：清零熔断计数、解除熔断、标 Up，让恢复的 Key 立即回到候选池。
        h.status = HealthStatus::Up;
        h.fail_count = 0;
        h.breaker_until = None;
        h.last_live_success = Some(now);
        // last_checked / latency_ms 保持不动（那是探测的观测量，不是转发的）。
        true
    });
    // 熔断解除跃迁：mutate 后再读一次，对比确认「已熔断 → 已解除」。
    // 并发窗口：另一线程可能在两次读之间解除熔断，导致本线程误发一条「已恢复」。
    // 恢复通知是低频事件，且重复一次「已恢复」无害（用户看到的是 Key 确实恢复了），
    // 不引入额外锁来消除这个窄窗口。
    let now_tripped = store.key_breaker_tripped(key_id);
    if was_tripped && !now_tripped {
        let name = store.key_name(key_id).unwrap_or_else(|| key_id.to_string());
        notify(
            "Key 已恢复",
            &format!("「{name}」恢复可用，已回到候选池。"),
        );
    }
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
///
/// **生产路径已改走 `Store::candidates_for`**（它在一把读锁内筛选排序、只克隆入选者，
/// 省掉这条路径的「两轮全量克隆」）。本函数保留为**纯函数参照实现**：
/// - 原有 4 条熔断语义测试直接打它（不需要构造 Store）；
/// - `candidates_for_matches_legacy_path` 用它做等价性基准——两者一旦漂移即测试失败。
///
/// 故标 `#[cfg(test)]`：既消掉生产构建的 dead_code 警告，也从编译期防止有人误把生产
/// 调用点切回这条更慢的路径。
#[cfg(test)]
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
/// 生产路径已改走 [`check_all_categories`]（三分类拉平后有界并发，见 P2-4）。
/// 本函数保留为**按分类探测**的入口，供测试与将来可能的「只重测某分类」功能使用。
#[cfg_attr(not(test), allow(dead_code))]
pub async fn check_category(store: &Arc<Store>, category: crate::model::CategoryType) {
    // 密钥库锁着（主口令模式未解锁）时取不到任何密钥：整轮探测会把每个 Key 都探成
    // Unknown/Down，UI 一片红，而真实原因只是没解锁。直接跳过，保留各 Key 现有健康态
    // ——与「间隔设为 0 关闭探测」同一处理原则。
    if store.secrets.read().is_locked() {
        return;
    }
    let ids: Vec<String> = store.enabled_key_ids(category);
    probe_ids_concurrently(store, ids).await;
}

/// 一轮探测的并发上限（P2-4）。
///
/// 为什么必须**有界**而不是 `join_all` 无限并发：那会把「启动瞬间打爆上游」的风险从无变有
/// ——多分类多 Key 时会同时发出几十个请求，中转商侧极可能触发限流，反而把好 Key 探成 Down。
/// 4 是「显著缩短一轮墙钟」与「不给上游造成突发压力」之间的折中。
const PROBE_CONCURRENCY: usize = 4;

/// 并发（有界）探测一批 Key。
///
/// 为什么不再串行（旧实现 `for k in keys { check_one().await }`）：单次探测超时上限是
/// `fast_timeout`（原 30s），串行下 6 条不可达的 Key 一轮就是 180 秒，**长于默认 60s 的探测
/// 间隔**——轮次首尾相接、永不空闲，常驻一个后台任务持续打上游。更实际的后果是
/// **健康状态严重滞后**：排在最后的 Key 要等前面全部超时完才被探到，UI 徽标与真实状态
/// 可能差几分钟，而这正是用户判断「该换哪条 Key」的依据。
async fn probe_ids_concurrently(store: &Arc<Store>, ids: Vec<String>) {
    use futures_util::stream::StreamExt;
    futures_util::stream::iter(ids)
        .for_each_concurrent(PROBE_CONCURRENCY, |id| async move {
            check_one(store, &id).await;
        })
        .await;
}

/// 一轮扫描全部分类（P2-4）：把三个分类的 Key **拉平成一个任务流**再有界并发。
///
/// 旧实现是「三分类串行 await，每分类内再串行逐 Key」（`lib.rs` 的后台循环里），
/// 两层串行叠加使最坏一轮 = 全部 Key 数 × 单次超时。拉平后最坏一轮 ≈
/// ⌈总 Key 数 / PROBE_CONCURRENCY⌉ × 单次超时。
pub async fn check_all_categories(store: &Arc<Store>) {
    if store.secrets.read().is_locked() {
        return;
    }
    let mut ids: Vec<String> = Vec::new();
    for cat in crate::model::CategoryType::ALL {
        ids.extend(store.enabled_key_ids(cat));
    }
    probe_ids_concurrently(store, ids).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hs(status: HealthStatus, fail_count: u32, breaker_until: Option<i64>) -> HealthState {
        HealthState { status, fail_count, breaker_until, ..Default::default() }
    }

    /// 密钥库锁着时健康探测必须**一个字节都不改**。
    ///
    /// 若不挡住：锁定态取不到密钥 → 每个 Key 都被写成 Unknown（甚至 Down），UI 一片红，
    /// 而真实原因只是没解锁。用户会去逐个检查 Key 配置，方向完全错。
    #[tokio::test]
    async fn locked_vault_skips_probe_without_touching_health() {
        use crate::model::{CategoryType, HealthStatus, KeyParams, Protocol, ProviderKey};
        let dir = std::env::temp_dir().join(format!(
            "synaroute_health_locked_{}_{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Arc::new(
            Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap(),
        );
        let key = ProviderKey {
            id: "k1".into(),
            category_id: CategoryType::ClaudeCli,
            name: "k".into(),
            vendor: "test".into(),
            // 不可路由地址：万一没被挡住而真去探测，会判 Down —— 让断言能区分两种结果。
            base_url: "http://127.0.0.1:1".into(),
            protocol: Protocol::Anthropic,
            has_secret: true,
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
        };
        store.upsert_key(key).unwrap();
        store.secrets.write().set("k1", "sk-x").unwrap();
        // 先摆一个「已探测为 Up」的既有状态，用来检验它不被覆盖。
        store
            .update_health(
                "k1",
                HealthState { status: HealthStatus::Up, latency_ms: Some(42), ..Default::default() },
            )
            .unwrap();

        store.secrets.write().enable_master_password("pw").unwrap();
        store.secrets.write().lock();

        check_one(&store, "k1").await;
        let h = store.get_key("k1").unwrap().health;
        assert_eq!(h.status, HealthStatus::Up, "锁定态不得把既有健康态改掉");
        assert_eq!(h.latency_ms, Some(42), "延迟等展示字段也应原样保留");

        check_category(&store, CategoryType::ClaudeCli).await;
        assert_eq!(
            store.get_key("k1").unwrap().health.status,
            HealthStatus::Up,
            "整轮探测同样必须跳过"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// P2-4：一轮探测的并发度必须**有上界**，且确实是并发（不是串行）。
    ///
    /// 上界是硬要求：用 `join_all` 无限并发会把「启动瞬间打爆上游」的风险从无变有——
    /// 多分类多 Key 时同时发几十个请求，中转商极可能触发限流，反而把好 Key 探成 Down。
    ///
    /// 用一个慢 mock 上游 + 计数器测峰值：每个请求进入时 +1、离开时 -1，记录峰值。
    /// 12 个 Key 全指向它，峰值必须恰好等于 PROBE_CONCURRENCY，且总耗时明显短于串行。
    #[tokio::test]
    async fn probe_concurrency_is_bounded_and_actually_concurrent() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc as StdArc;

        let inflight = StdArc::new(AtomicUsize::new(0));
        let peak = StdArc::new(AtomicUsize::new(0));

        // 慢 mock：每个请求睡 300ms，期间统计并发数
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        {
            let inflight = inflight.clone();
            let peak = peak.clone();
            tokio::spawn(async move {
                loop {
                    let Ok((stream, _)) = listener.accept().await else { break };
                    let inflight = inflight.clone();
                    let peak = peak.clone();
                    tokio::spawn(async move {
                        let io = hyper_util::rt::TokioIo::new(stream);
                        let svc = hyper::service::service_fn(move |_req| {
                            let inflight = inflight.clone();
                            let peak = peak.clone();
                            async move {
                                let now = inflight.fetch_add(1, Ordering::SeqCst) + 1;
                                peak.fetch_max(now, Ordering::SeqCst);
                                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                                inflight.fetch_sub(1, Ordering::SeqCst);
                                Ok::<_, std::convert::Infallible>(
                                    hyper::Response::builder()
                                        .status(200)
                                        .body(http_body_util::Full::new(bytes::Bytes::from(
                                            r#"{"content":[{"type":"text","text":"ok"}]}"#,
                                        )))
                                        .unwrap(),
                                )
                            }
                        });
                        let _ = hyper::server::conn::http1::Builder::new()
                            .serve_connection(io, svc)
                            .await;
                    });
                }
            });
        }

        let dir = std::env::temp_dir().join(format!("synaroute_probe_conc_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = StdArc::new(
            Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap(),
        );
        const N: usize = 12;
        for i in 0..N {
            let id = format!("k{i}");
            store
                .upsert_key(crate::model::ProviderKey {
                    id: id.clone(),
                    category_id: CategoryType::ClaudeCli,
                    name: id.clone(),
                    vendor: "t".into(),
                    base_url: format!("http://{addr}"),
                    protocol: crate::model::Protocol::Anthropic,
                    has_secret: true,
                    enabled: true,
                    priority: i as i32,
                    headers_json: None,
                    params: crate::model::KeyParams::default(),
                    models: vec![],
                    mappings: vec![],
                    default_model: None,
                    tier_haiku: None,
                    tier_sonnet: None,
                    tier_opus: None,
                    health: HealthState::default(),
                })
                .unwrap();
            store.secrets.write().set(&id, "sk").unwrap();
        }

        let t0 = std::time::Instant::now();
        check_all_categories(&store).await;
        let elapsed = t0.elapsed();

        let observed_peak = peak.load(Ordering::SeqCst);
        assert!(
            observed_peak <= PROBE_CONCURRENCY,
            "并发峰值 {observed_peak} 超过上限 {PROBE_CONCURRENCY}——无界并发会打爆上游"
        );
        assert!(
            observed_peak > 1,
            "峰值只有 {observed_peak}，说明退化成串行了（本条优化的目的就是不再串行）"
        );
        // 串行 12×300ms = 3.6s；并发 4 时约 ⌈12/4⌉×300ms = 900ms。取 2.5s 上限留足抖动余量。
        assert!(
            elapsed < std::time::Duration::from_millis(2500),
            "一轮耗时 {elapsed:?}，看起来仍是串行（串行约 3.6s）"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// `Store::candidates_for` 必须与旧路径
    /// （`enabled_keys_sorted` + `select_candidates`）**逐项等价**。
    ///
    /// 补这条的理由：`candidates_for` 是为省掉「两轮全量克隆」而新开的生产路径，而原有的
    /// 4 条熔断测试打的是 `select_candidates` **纯函数**——新路径没有任何覆盖。两者一旦
    /// 语义漂移，表现是路由候选集悄悄变了（少一个候选＝少一次故障转移机会，多一个＝把
    /// 熔断中的 Key 又拉回来），不报错、不 panic。故这里对同一份 store 状态跑两条路径逐项比对。
    #[test]
    fn candidates_for_matches_legacy_path() {
        use crate::model::{CategoryType, KeyParams, Protocol, ProviderKey};
        let dir = std::env::temp_dir().join(format!(
            "synaroute_cand_eq_{}_{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Arc::new(
            Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap(),
        );

        let mk = |id: &str, cat: CategoryType, prio: i32, enabled: bool, health: HealthState| {
            ProviderKey {
                id: id.into(),
                category_id: cat,
                name: id.into(),
                vendor: "test".into(),
                base_url: "http://127.0.0.1:1".into(),
                protocol: Protocol::Anthropic,
                has_secret: false,
                enabled,
                priority: prio,
                headers_json: None,
                params: KeyParams::default(),
                models: vec![],
                mappings: vec![],
                default_model: None,
                tier_haiku: None,
                tier_sonnet: None,
                tier_opus: None,
                health,
            }
        };
        let future = chrono::Utc::now().timestamp_millis() + 60_000;
        let cat = CategoryType::ClaudeCli;

        // 故意乱序插入 + 混入他分类与禁用项，同时放一个熔断中的。
        store.upsert_key(mk("c", cat, 2, true, HealthState::default())).unwrap();
        store.upsert_key(mk("a", cat, 0, true, hs(HealthStatus::Up, 0, None))).unwrap();
        store.upsert_key(mk("burnt", cat, 1, true, hs(HealthStatus::Up, 3, Some(future)))).unwrap();
        store.upsert_key(mk("off", cat, 0, false, HealthState::default())).unwrap();
        store.upsert_key(mk("other", CategoryType::Codex, 0, true, HealthState::default())).unwrap();

        let legacy = select_candidates(store.enabled_keys_sorted(cat));
        let now = store.candidates_for(cat);
        assert_eq!(
            now.0.iter().map(|k| k.id.as_str()).collect::<Vec<_>>(),
            legacy.0.iter().map(|k| k.id.as_str()).collect::<Vec<_>>(),
            "候选集与顺序必须与旧路径一致"
        );
        assert_eq!(now.1, legacy.1, "兜底标记必须一致");
        // 具体断言，防止两条路径「一起错」也判等价。
        assert_eq!(
            now.0.iter().map(|k| k.id.as_str()).collect::<Vec<_>>(),
            vec!["a", "c"],
            "应按 priority 升序、剔除熔断中的 burnt、排除禁用与他分类"
        );
        assert!(!now.1);

        // 全部熔断 → 兜底返回全部启用 Key（含熔断中的），两条路径同样必须一致。
        store.mutate_health("a", |h| { h.breaker_until = Some(future); true }).unwrap();
        store.mutate_health("c", |h| { h.breaker_until = Some(future); true }).unwrap();
        let legacy = select_candidates(store.enabled_keys_sorted(cat));
        let now = store.candidates_for(cat);
        assert_eq!(
            now.0.iter().map(|k| k.id.as_str()).collect::<Vec<_>>(),
            legacy.0.iter().map(|k| k.id.as_str()).collect::<Vec<_>>(),
        );
        assert_eq!(now.1, legacy.1);
        assert!(now.1, "全熔断必须标记为已触发兜底");
        assert_eq!(now.0.len(), 3, "兜底应纳入全部 3 个启用 Key（不含禁用/他分类）");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 并发失败**不得丢计数**：N 个并发 `record_live_failure` 必须精确累加成 N。
    ///
    /// 补这条的理由：旧实现是「`get_key` 读锁取 prev → 算 → `update_health` 写锁」，
    /// 跨两个独立临界区。两个并发失败会都读到 `fail_count == 1`、各自算出 2 写回，
    /// 真实值应为 3 —— 表现为**熔断比预期迟钝**（要多失败几次才触发），并发越高越迟钝。
    /// 这类 bug 不报错、不 panic，只让阈值悄悄失准，靠读代码很难发现。
    /// 改走 `Store::mutate_health` 后 read-modify-write 在同一写锁内完成。
    #[test]
    fn concurrent_failures_never_lose_count() {
        use crate::model::{CategoryType, KeyParams, Protocol, ProviderKey};
        let dir = std::env::temp_dir().join(format!(
            "synaroute_health_race_{}_{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Arc::new(
            Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap(),
        );
        store
            .upsert_key(ProviderKey {
                id: "k1".into(),
                category_id: CategoryType::ClaudeCli,
                name: "k".into(),
                vendor: "test".into(),
                base_url: "http://127.0.0.1:1".into(),
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
            })
            .unwrap();

        // 8 线程 × 各记 1 次失败。旧实现下这里会稳定小于 8。
        const N: u32 = 8;
        std::thread::scope(|s| {
            for _ in 0..N {
                let st = Arc::clone(&store);
                s.spawn(move || record_live_failure(&st, "k1"));
            }
        });

        let h = store.get_key("k1").unwrap().health;
        assert_eq!(h.fail_count, N, "并发失败必须精确累加，丢一次都会让熔断阈值失准");
        assert!(
            h.breaker_until.is_some(),
            "累计已远超阈值 {BREAKER_THRESHOLD}，必须已武装熔断"
        );

        // 一次成功即清零并解除熔断（让恢复的 Key 立刻回到候选池）。
        record_live_success(&store, "k1");
        let h = store.get_key("k1").unwrap().health;
        assert_eq!(h.fail_count, 0, "成功必须清零计数");
        assert!(h.breaker_until.is_none(), "成功必须解除熔断");
        assert_eq!(h.status, HealthStatus::Up);

        std::fs::remove_dir_all(&dir).ok();
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
