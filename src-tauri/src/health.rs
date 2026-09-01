//! 健康检查与熔断（FR-011 / FR-012）。
//! 混合策略（arch-decisions §6）：主动探测 + 缓存 + 熔断态派生。

/// 弹性第三层：余额闸门（B4）。挂在这里的理由写在它的模块头 —— 一句话：本文件是它唯一的
/// 刷新发起方，而 `store.rs`/`lib.rs`/`proxy.rs` 三个"自然家"棘轮余量都是 0。
#[path = "balance_gate.rs"]
pub(crate) mod balance_gate;

use crate::model::{HealthState, HealthStatus};
use crate::store::Store;
use crate::upstream;
use chrono::Utc;
use std::sync::Arc;

/// 发系统通知（熔断/恢复告警）。非 test 走 `notification::notify`；test 下是 no-op
/// （测试进程不该弹系统框。注意插件 crate 在 test 下**照常链接**，见 notification.rs 模块注释）。
#[cfg(not(test))]
fn notify(title: &str, body: &str) {
    crate::notification::notify(title, body);
}
#[cfg(test)]
fn notify(_title: &str, _body: &str) {}

/// 熔断窗口此刻是否还活着（与 `is_candidate` 同口径：`until > now` 才算熔断中）。
///
/// 单独抽出来是因为**不能**用 `breaker_until.is_some()` 代替：`breaker_until` 只有在一次
/// 真实成功后才被清成 `None`，窗口自然到期时字段仍是 `Some(过去时刻)`，此时路由侧
/// （`is_candidate`）已判可用。用 `is_some()` 判「熔断中」会让重新武装时的
/// `now && !was` 恒为 false，第二次及以后的熔断永远不发通知。
///
/// 收成一个函数而不是在两处各写一遍同样的 `map(...).unwrap_or(false)`：这个判据刚因为
/// 两套口径分叉出过缺陷，再复制一份就是给下一次分叉留位置。
fn breaker_window_active(breaker_until: Option<i64>, now: i64) -> bool {
    breaker_until.map(|until| until > now).unwrap_or(false)
}

/// 熔断阈值：连续失败达到即熔断
const BREAKER_THRESHOLD: u32 = 3;
/// 熔断冷却时长（毫秒）
const BREAKER_COOLDOWN_MS: i64 = 60_000;
/// 「近期有真实转发成功」宽限窗口（毫秒）：窗口内后台探测失败不熔断，因真实流量证明该 Key 可用。
const LIVE_SUCCESS_GRACE_MS: i64 = 120_000;

// ===================== 弹性第二层：单模型锁定 =====================
//
// 借鉴 OmniRoute 的三层弹性模型（provider breaker / connection cooldown / model lockout），
// 见 `docs/architecture/RESILIENCE_GUIDE.md`。SynaRoute 此前只有一层（Key 级熔断），
// 于是「这条 Key 的某个模型没开通」这类失败会把整条 Key 打停 60 秒 —— 连它本来
// 能服务的模型一起误伤。这一层把作用域收窄到 (Key, 真实模型名) 这一对。
//
// 层间归属判据在 `proxy::failure_scope`（状态码 + 路径 → 罚哪一层）。**不要**在这里
// 复制一份判据：两处各写一份必然漂移，本仓已经因此出过一次缺陷
// （见 `TRANSIENT_4XX` 的「单一事实来源」注释）。

/// 单模型锁定的基础时长：第一次失败锁多久。
///
/// 取 120s 而不是 Key 级熔断那个 60s：模型级不可用（未开通/套餐不含）通常不是抖动，
/// 而是稳定状态；重试太勤只是白打上游。
const MODEL_LOCK_BASE_MS: i64 = 120_000;
/// 指数退避上限（30 分钟）。再长就有「上游已经开通了但我们半天不去试」的风险。
const MODEL_LOCK_MAX_MS: i64 = 1_800_000;
/// 一条 Key 上**同时锁着**这么多个不同模型 → 证据已指向「这条 Key 本身有问题」，
/// 升级到 Key 级熔断。
///
/// 为什么需要这个阀门：单模型锁定刻意**不罚** Key，那么一条对所有模型都回 404 的 Key
/// 会永远待在候选池首位、每个新模型都要白试一次。有了它，第 3 个模型被锁时整条 Key
/// 进入熔断窗口，行为退回到分层之前 —— 分层带来的是「误伤更少」，不该带来「坏 Key 赖着不走」。
const MODEL_LOCK_ESCALATE_AT: usize = 3;

/// 某条 Key 上的某个模型此刻是否被锁。
///
/// 与 [`breaker_window_active`] 同一口径（`until > now` 才算锁着），理由也相同：
/// 条目只在「该模型成功一次」时才被删，窗口自然到期时条目仍在，按「存在即锁着」判会永不放行。
fn model_lock_active(health: &HealthState, real_model: &str, now: i64) -> bool {
    health
        .model_locks
        .get(real_model)
        .map(|l| l.until > now)
        .unwrap_or(false)
}

/// 当前**仍然生效**的模型锁数量（到期的不算）。升级阀门的判据。
fn active_model_lock_count(health: &HealthState, now: i64) -> usize {
    health.model_locks.values().filter(|l| l.until > now).count()
}

/// 「退避档位的记忆」在锁到期后还值得保留多久。超过即视为陈旧，可以扫掉。
///
/// 取 2× [`MODEL_LOCK_MAX_MS`]（1 小时）：锁窗最长 30 分钟，若一个模型在锁到期之后
/// 又过了同样长的时间都没再失败过，那条 `fail_count` 表达的「它最近不太行」已经不成立。
/// 取值偏大是刻意的 —— 扫早了会把退避档位打回第一档，
/// 而那正是 [`decay_model_lock`] 注释里「成功即删」要避免的高频白打上游。
const MODEL_LOCK_STALE_AFTER_MS: i64 = MODEL_LOCK_MAX_MS * 2;

/// 扫掉**早已到期**的模型锁条目，返回扫掉几条。
///
/// 🔴 为什么需要它：条目只在「该模型成功一次」时才被 [`decay_model_lock`] 删除
/// （减半到 0）。而一条「什么模型都 404」的 Key 上，用户每换一个模型名就多一条记录，
/// **成功永远不会发生**，于是那些条目一条都不会被回收 —— 它们随
/// `HealthState` 一起进 `config.json`，而健康态每次落盘都要整份序列化。
/// 是单调增长的持久化状态，方向上永不自愈。
///
/// 判据刻意**不是**「到期就扫」：`fail_count` 是退避阶梯的记忆，
/// 到期的下一秒就扫掉会让「每隔几分钟失败一次」的模型永远停在第一档 120s。
/// 故只扫「到期且已过 [`MODEL_LOCK_STALE_AFTER_MS`]」的那些。
///
/// 对 [`model_lock_active`] 与 [`active_model_lock_count`] 都**无语义影响**：
/// 两者只看 `until > now`，而被扫掉的条目早已不满足。
fn sweep_stale_model_locks(h: &mut HealthState, now: i64) -> usize {
    let before = h.model_locks.len();
    h.model_locks
        .retain(|_, l| l.until > now - MODEL_LOCK_STALE_AFTER_MS);
    before - h.model_locks.len()
}

/// 把一次「上游明确表示这条 Key 不提供这个模型」计入**模型级**锁定。
///
/// 与 [`record_live_failure`] 的差别是作用域：这里**绝不动** `fail_count` / `breaker_until`
/// （除了触发升级阀门那一刻），因为这类失败不构成「该 Key 不可用」的证据。
///
/// 退避：`MODEL_LOCK_BASE_MS * 2^(fail_count-1)`，夹到 `MODEL_LOCK_MAX_MS`。
pub fn record_model_unavailable(store: &Arc<Store>, key_id: &str, real_model: &str) {
    if real_model.is_empty() {
        // 解析不出模型名时无处可锁。退回「不罚任何一层」而不是猜一个键 ——
        // 猜错会锁掉一个本来好的模型，而那种误伤是静默的。
        return;
    }
    let now = Utc::now().timestamp_millis();
    let mut escalate = false;
    let mut locked_secs = 0i64;
    let mut lock_count = 0u32;
    // 与 record_live_failure 同样走 mutate_health：read-modify-write 必须在同一个写锁内，
    // 否则并发下两次失败会读到同一份 fail_count 各自 +1 写回，退避档位失准。
    let _ = store.mutate_health(key_id, |h| {
        // 顺手扫掉早已陈旧的条目。挂在这里而不是另起定时器：这是**唯一**会让这张表
        // 变大的地方，判据天然对齐，且已经持有写锁（不额外加一次 read-modify-write）。
        //
        // 必须在 `entry()` 之前（之后扫会把刚插入的那条误伤 —— 它的 `until` 还是 0）。
        // 这一条**不需要测试守**：`entry()` 借着 `h`，在它之后调 `sweep(h, ..)`
        // 直接 E0499 编译不过。故顺序由借用检查器保证，写测试反而是把一条硬保证
        // 降级成软保证（实测确认：把它挪到 entry() 之后，编译即失败）。
        sweep_stale_model_locks(h, now);
        let entry = h
            .model_locks
            .entry(real_model.to_string())
            .or_insert(crate::model::ModelLock { until: 0, fail_count: 0 });
        entry.fail_count = entry.fail_count.saturating_add(1);
        // 2^(n-1) 用移位算，并把指数夹住防溢出。
        //
        // ⚠️ `.min(20)` 防的**不是**「窗口太长」（`saturating_mul` + 下面的 `.min(MAX_MS)`
        // 已经管住了），而是 **`1i64 << 64` 的移位 panic** —— debug 构建下直接崩，
        // 且崩在转发热路径上（一条模型被反复 404 的 Key 攒够 65 次就崩）。
        let steps = (entry.fail_count - 1).min(20);
        let backoff = MODEL_LOCK_BASE_MS.saturating_mul(1i64 << steps).min(MODEL_LOCK_MAX_MS);
        entry.until = now + backoff;
        locked_secs = backoff / 1000;
        lock_count = entry.fail_count;

        // 升级阀门：锁着的模型数达到阈值 → 同时武装 Key 级熔断。
        // 在**同一个临界区内**判定并写入，避免「判定时 2 个、写入前变成 4 个」这类窗口。
        if active_model_lock_count(h, now) >= MODEL_LOCK_ESCALATE_AT {
            h.breaker_until = Some(now + BREAKER_COOLDOWN_MS);
            escalate = true;
        }
        true
    });
    // 落一条事件。
    //
    // 不落的话这一层是**不可见**的：用户只会看到第一次请求的那条故障转移，
    // 之后这条 Key 被静默跳过 —— 界面上表现为「它明明启用着、健康着，却好像没在用」。
    // 那正是本项目反复吃过的「排障时看到的是假现场」。
    //
    // 折叠（`append_event_collapsible`）：同一 (Key, 模型) 连续锁定合成一条带 ×N，
    // 否则一条被反复 404 的模型会把有用事件挤出 MAX_EVENTS 环。
    if let Some(name) = store.key_name(key_id) {
        store.append_event_collapsible(
            store.get_key(key_id).map(|k| k.category_id).unwrap_or_default(),
            "failover",
            Some(key_id),
            &format!(
                "{name} · 模型 {real_model} 上游不提供，已锁定 {locked_secs}s（第 {lock_count} 次；\
                 该 Key 的其它模型不受影响）"
            ),
            None,
            Some(format!("mlock:{key_id}:{real_model}")),
        );
    }
    if escalate {
        let name = store.key_name(key_id).unwrap_or_else(|| key_id.to_string());
        notify(
            "Key 已熔断",
            &format!(
                "「{name}」已有 {MODEL_LOCK_ESCALATE_AT} 个模型不可用，已暂停整条 Key 60 秒。"
            ),
        );
    }
}

/// 一次成功之后让该模型的锁**衰减**：`fail_count` 减半，到 0 即删除整条。
///
/// 为什么不是「成功即删」：上游按分钟/按天配额放行时，一个模型会「偶尔成功」。
/// 成功即删会让退避档位永远停在第一档（120s），实际是在高频白打上游。
/// 减半既能让真恢复的模型很快解锁（3 → 1 → 0，两次成功），又保留了「它最近不太行」的记忆。
///
/// 返回是否改动过（供调用方决定要不要落盘）。
fn decay_model_lock(h: &mut HealthState, real_model: &str) -> bool {
    let Some(entry) = h.model_locks.get_mut(real_model) else {
        return false;
    };
    entry.fail_count /= 2;
    if entry.fail_count == 0 {
        h.model_locks.remove(real_model);
    } else {
        // 计数降了，锁窗也要相应缩短，否则「计数已经降到 1，却还按 30 分钟锁着」。
        let steps = (entry.fail_count - 1).min(20);
        let backoff = MODEL_LOCK_BASE_MS.saturating_mul(1i64 << steps).min(MODEL_LOCK_MAX_MS);
        let now = Utc::now().timestamp_millis();
        entry.until = entry.until.min(now + backoff);
    }
    true
}

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

    // 走 `mutate_health` 在**单个写锁临界区内**只改探测三字段（status/latency/last_checked），
    // 其余字段原地保留 —— 旧写法「get_key 读快照 → append_event（含日志 I/O）→
    // update_health 整份写回」跨两个临界区，窗口内 record_live_failure 刚累加的
    // fail_count / 刚武装的 breaker_until 会被 stale 快照覆盖回去：熔断计数被回退、
    // 甚至刚武装的熔断被静默解除，坏 Key 留在候选池首位。`mutate_health` 的文档
    // 与 concurrent_failures_never_lose_count 测试钉的正是这类跨临界区丢失更新。
    let _ = store.mutate_health(key_id, |h| {
        h.status = status;
        h.last_checked = Some(now);
        h.latency_ms = Some(latency);
        true
    });
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
    // 用窄读取器（只取 Option<i64>），不用 get_key 整份克隆——每失败都走这里，热路径。
    //
    // 口径是「窗口此刻是否还活着」，不是「字段有没有残留」：窗口自然到期后 breaker_until
    // 仍是 Some(过去时刻)（只有真实成功才清），若按 is_some() 算，重新武装时
    // was_tripped 恒为 true，第二次及以后的熔断就永远静默了。
    let was_tripped = breaker_window_active(store.key_breaker_until(key_id), now);
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
    // 同口径复算（mutate 里刚写的 breaker_until 一定在未来，故这里等价于「是否刚被武装」）。
    let now_tripped = breaker_window_active(store.key_breaker_until(key_id), now);
    if now_tripped && !was_tripped {
        let name = store.key_name(key_id).unwrap_or_else(|| key_id.to_string());
        // 🔴 **不许无条件承诺「其它 Key 自动接管」**（2026-09-01 用户日志实证）。
        //
        // 本函数只拿到 `key_id`，**不知道这次请求的是哪个模型** —— 而「能不能接管」恰恰
        // 取决于那个模型有没有别的 Key 能服务。用户那份日志里这句话被自己的系统当场
        // 否证了两次：`21:07:02.785` 说「其它 Key 自动接管」→ **791ms 后**
        // `model_pool` 的 503 闸门说「本池中能服务它的只有「luckyg」」→ 客户端拿到 503；
        // `21:10:54.513` 同一对，间隔 191ms。
        //
        // 代价不是「多一句废话」：用户读到「自动接管」会判定故障转移兜住了，
        // 于是排除掉「代理这边把请求挡了」这个方向 —— 而那正是真相。同
        // `model_pool` 里那句「文案要**指对方向**」，那道门已经执行了这个标准，这里没有。
        //
        // 改成**条件句**而不是去查一遍：查需要模型名（这里没有），而条件句在两种情形下
        // 都为真，且把「取决于什么」如实交给用户。
        let takeover = "若本池中还有能服务该模型的 Key，会自动接管";
        notify(
            "Key 已熔断",
            &format!("「{name}」连续失败，已暂停使用 60 秒。{takeover}。"),
        );
        // 🔴 事件是**必须**的，不是通知的附赠：系统通知可能被用户或系统静音（免打扰、
        // 焦点助手），而排障时唯一可回溯的地方是日志页。上面那句注释此前写着
        // 「发系统通知 + 记一条告警事件」而代码只做了前半 —— 于是按注释去日志页搜
        // 「熔断」什么都搜不到。模型级锁定那一层（本文件上方）一直有事件，Key 级反倒没有。
        // 折叠键按 Key：同一条 Key 反复进出熔断只占一行带 ×N，不挤 MAX_EVENTS 环。
        store.append_event_collapsible(
            store.get_key(key_id).map(|k| k.category_id).unwrap_or_default(),
            "warning",
            Some(key_id),
            &format!(
                "{name} 已熔断，暂停使用 {}s（连续失败达阈值；{takeover}）",
                BREAKER_COOLDOWN_MS / 1000
            ),
            None,
            Some(format!("breaker:{key_id}")),
        );
    }
}

/// 流末统一记账：健康态 **+ 一条用户看得见的失败事件**。
///
/// # 🔴 为什么必须有这个函数（而不是让调用方各写一遍二选一）
///
/// 流式转发的日志行是在**拿到 200 响应头那一刻**写下的（`proxy::log_success`，kind
/// `route` → 日志页「路由」绿组），延迟记的是到响应头的耗时。而流内失败（Anthropic 过载
/// 中途发 error 事件、或本仓的静默超时判定卡死）只纠正了**健康记账**，那一行日志
/// **没人回头改** —— `backfill_usage_for_collapsed_event` 只补 token 用量。
///
/// 于是用户在客户端看到报错、回到日志页看到这条请求是「成功 · 200 · 1.2s」，
/// 而它真实卡了 180 秒。排障者据此得出「代理这边没问题」，正是本仓最忌讳的
/// 「排障时看到的是假现场」。前两次失败连系统通知都没有（要攒到熔断阈值才弹）。
///
/// **刻意不去改那一行**：日志文件（`*.jsonl`）已经把「成功」那行写出去了，改内存里的副本
/// 会造出「界面说失败、文件说成功」两个平行事实 —— 本仓在 MSIX 那次惨案上吃够了平行宇宙。
/// 那一行本身**不是假话**（上游确实回了 200 并开了流），缺的是「后来怎么了」。
/// 故这里**追加**一条 `error` 级事件（→ 日志页红色「错误」组），两行合起来才是完整时间线。
///
/// 折叠键按 (Key, 对外模型名)：一条反复流内失败的 Key 只占一行带 ×N，不挤 `MAX_EVENTS` 环。
pub fn record_stream_end(
    store: &Arc<Store>,
    category: crate::model::CategoryType,
    key_id: &str,
    requested_model: &str,
    real_model: &str,
    errored: bool,
) {
    if !errored {
        record_live_success(store, key_id, Some(real_model));
        return;
    }
    record_live_failure(store, key_id);
    let name = store.key_name(key_id).unwrap_or_else(|| key_id.to_string());
    store.append_event_collapsible(
        category,
        "error",
        Some(key_id),
        &format!(
            "{name} · {requested_model} · 流内失败：上游先回 200 开了流，随后在流里发了 error \
             事件（或流被判定静默卡死）。上面那条「路由成功」记的是开流那一刻，不是最终结果。"
        ),
        None,
        Some(format!("streamfail:{key_id}:{requested_model}")),
    );
}

/// 把一次「实时转发成功」计入熔断器：解除熔断、标记 Up、让 `fail_count` **减半**，
/// 并让本次成功的那个模型的锁一起衰减。
///
/// `real_model`：本次成功打的**上游真实模型名**（拿不到时传 `None`）。用于衰减第二层的模型锁。
///
/// ## 为什么 `fail_count` 是减半而不是清零（2026-08-23 改）
///
/// 清零留了一个洞：一条「三次里坏两次」的 Key **永远不会熔断** —— 每次成功把计数抹平，
/// `BREAKER_THRESHOLD` 再也够不着。而那正是最该被切走的一类 Key（用户体验是「时好时坏」）。
/// 减半后同一条 Key 约 6 个请求内就会触发熔断。
///
/// 方向上这个改动只会让熔断**更容易**触发，故不可能复现历史上那个
/// 「流式路径提前记成功 → fail_count 恒为 1 → 永不熔断」的缺陷（见 proxy.rs 流式分支注释）。
///
/// 关键是：**`breaker_until` 仍然一次成功就清空**。所以「恢复的 Key 立刻回到候选池」
/// 这条性质完全不变（`is_candidate` 只看 `breaker_until`，不看 `fail_count`）；
/// 变的只是「这条 Key 最近不太行」这段记忆不再被一次成功抹得一干二净。
pub fn record_live_success(store: &Arc<Store>, key_id: &str, real_model: Option<&str>) {
    let now = Utc::now().timestamp_millis();
    // mutate 前读熔断残留：与 mutate 后对比，判断「本次是否真的清掉了熔断」。
    // 这里口径是 `is_some()`（有无残留）而非「窗口是否还活着」，且**刻意**与
    // `record_live_failure` 不同：残留只有真实成功才会被清成 None，所以
    // 「Some → None」精确对应「这个 Key 真的恢复了」。若这里也按窗口活着算，
    // 「熔断窗口自然到期后第一次成功」就发不出恢复通知——而用户上一条收到的正是熔断告警，
    // 不给恢复回执会让告警永远悬着。窄读取器只取 Option<i64>，热路径零整份克隆。
    let had_breaker_residue = store.key_breaker_until(key_id).is_some();
    let _ = store.mutate_health(key_id, |h| {
        // 已健康、无熔断残留、且 last_live_success 仍新鲜：内存与磁盘都无需动
        // （避免高频请求每次都写盘——这是热路径上最值钱的一处抑制，见 Store::update_health）。
        // 但若时间戳偏旧（超过宽限窗口一半），仍刷新一次，让「近期成功」宽限窗口
        // 在持续流量下不失效。
        let fresh = h
            .last_live_success
            .map(|t| now - t < LIVE_SUCCESS_GRACE_MS / 2)
            .unwrap_or(false);
        // 稳态成功路径：**完全不落盘**。
        //
        // 判据里必须带上 `model_locks` 为空这一条 —— 否则一条「Key 级健康、但还挂着模型锁」
        // 的 Key 会在这里提前 return，锁永远得不到衰减（一次成功也不解锁），
        // 那就是又造了一个「只能进不能出」的状态。
        if h.fail_count == 0
            && h.breaker_until.is_none()
            && h.status == HealthStatus::Up
            && fresh
            && h.model_locks.is_empty()
        {
            return false;
        }
        // 需要写：解除熔断、标 Up，并让失败计数减半（不是清零，理由见函数头注释）。
        h.status = HealthStatus::Up;
        h.fail_count /= 2;
        h.breaker_until = None;
        h.last_live_success = Some(now);
        // 第二层：让本次成功的那个模型的锁一起衰减（拿不到模型名时跳过，不猜）。
        if let Some(m) = real_model.filter(|m| !m.is_empty()) {
            decay_model_lock(h, m);
        }
        // last_checked / latency_ms 保持不动（那是探测的观测量，不是转发的）。
        true
    });
    // 熔断解除跃迁：mutate 后再读一次，对比确认「有残留 → 已清空」。
    // 并发窗口：另一线程可能在两次读之间解除熔断，导致本线程误发一条「已恢复」。
    // 恢复通知是低频事件，且重复一次「已恢复」无害（用户看到的是 Key 确实恢复了），
    // 不引入额外锁来消除这个窄窗口。
    let still_has_residue = store.key_breaker_until(key_id).is_some();
    if had_breaker_residue && !still_has_residue {
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
/// 判断某 Key 当前是否可作为路由候选（**只看第一层**，不看模型锁）。
///
/// 生产路径已改走 [`is_candidate_for_model`]（它多判一层单模型锁定）。本函数保留为
/// 「只验熔断语义」的入口，供既有的 4 条熔断语义测试与 `select_candidates` 参照实现使用。
///
/// 故标 `#[cfg(test)]`：既消掉生产构建的 dead_code 警告，也从编译期阻止有人误把生产
/// 调用点切回这条**少一层门槛**的路径 —— 那种回退是静默的（不报错，只是模型锁不再生效）。
#[cfg(test)]
pub fn is_candidate(health: &HealthState) -> bool {
    is_candidate_for_model(health, None)
}

/// 同 [`is_candidate`]，但再叠加**第二层**（单模型锁定）的门槛。
///
/// `real_model`：本次请求在这条 Key 上解析出的**上游真实模型名**。
/// 传 `None` 表示「不针对具体模型」（模型发现、UI 展示、只验熔断语义的测试），此时只看第一层。
///
/// 两层是 AND 关系，但语义不同、故**不能**并成一个计数：
/// - 第一层（`breaker_until`）说「这条 Key 现在别用」；
/// - 第二层（`model_locks`）说「这条 Key 别用来跑这个模型」。
///
/// 顺序上先判第一层：Key 级熔断成立时，模型锁的状态无关紧要。
pub fn is_candidate_for_model(health: &HealthState, real_model: Option<&str>) -> bool {
    let now = Utc::now().timestamp_millis();
    // 第一层：熔断中 → 不可用；其余（含探测判 Down 的）一律给机会——真实流量才是可用性裁判。
    let key_ok = match health.breaker_until {
        Some(until) => until <= now,
        None => true,
    };
    if !key_ok {
        return false;
    }
    // 第二层：这条 Key 上这个模型是否被锁。空模型名等同于 None（解析不出名字时不该凭空挡人）。
    match real_model {
        Some(m) if !m.is_empty() => !model_lock_active(health, m, now),
        _ => true,
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

    /// 熔断「跃迁」判据必须按**窗口是否还活着**算，不能按 `breaker_until.is_some()` 算。
    ///
    /// 背景：`breaker_until` 只有在一次真实成功后才会被清成 `None`。窗口自然到期
    /// （`until <= now`）时字段仍是 `Some(过去时刻)`，而 `is_candidate` 此时已判它可路由。
    /// 于是「熔断中」这个概念有两套口径：
    ///   - 路由用的（`is_candidate`）：`until > now` 才算熔断
    ///   - 通知曾用的（`is_some()`）：只要还有残留就算熔断
    ///
    /// 两套口径不一致会吃掉告警：Key 第一次熔断 → 窗口到期 → 再次连续失败被**重新武装**，
    /// 此时按 `is_some()` 算 `was_tripped` 恒为 true，`now && !was` 恒为 false，
    /// **第二次及以后的熔断永远不发通知**。用户只会在 Key 第一次出问题时收到一次告警，
    /// 之后无论故障多少轮都静默——而这恰恰是健康告警这个功能要解决的问题。
    #[test]
    fn breaker_arm_transition_uses_live_window_not_residual_field() {
        let now = Utc::now().timestamp_millis();
        // 窗口已自然到期，但字段仍有残留（真实成功前不会被清）。
        let expired = hs(HealthStatus::Up, 3, Some(now - 1_000));
        assert!(
            is_candidate(&expired),
            "前提：窗口到期后路由侧已认为可用（这正是两套口径分叉的地方）"
        );
        assert!(
            !breaker_window_active(expired.breaker_until, now),
            "通知侧必须与路由侧同口径：窗口到期即视为未熔断，否则重新武装时发不出告警"
        );

        // 窗口仍活着 → 两侧都应认为在熔断中。
        let active = hs(HealthStatus::Up, 3, Some(now + 30_000));
        assert!(!is_candidate(&active));
        assert!(breaker_window_active(active.breaker_until, now));

        // 从未熔断。
        let clean = hs(HealthStatus::Up, 0, None);
        assert!(is_candidate(&clean));
        assert!(!breaker_window_active(clean.breaker_until, now));
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
            allow_in_aggregate: false,
            priority: 0,
            headers_json: None,
            params: KeyParams::default(),
            models: vec![],
            mappings: vec![],
            default_model: None,
            tier_haiku: None,
            tier_sonnet: None,
            tier_opus: None,
            balance_query: None,
            cached_balance: None,
            cost_multiplier: None,
            icon: None,
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

        store.secrets.write().enable_master_password("TestPass123").unwrap();
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
                    allow_in_aggregate: false,
                    priority: i as i32,
                    headers_json: None,
                    params: crate::model::KeyParams::default(),
                    models: vec![],
                    mappings: vec![],
                    default_model: None,
                    tier_haiku: None,
                    tier_sonnet: None,
                    tier_opus: None,
                    balance_query: None,
                    cached_balance: None,
                    cost_multiplier: None,
                    icon: None,
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
                allow_in_aggregate: false,
                priority: prio,
                headers_json: None,
                params: KeyParams::default(),
                models: vec![],
                mappings: vec![],
                default_model: None,
                tier_haiku: None,
                tier_sonnet: None,
                tier_opus: None,
                balance_query: None,
                cached_balance: None,
                cost_multiplier: None,
                icon: None,
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
        let now = store.candidates_for(cat, "");
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
        let now = store.candidates_for(cat, "");
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
                allow_in_aggregate: false,
                priority: 0,
                headers_json: None,
                params: KeyParams::default(),
                models: vec![],
                mappings: vec![],
                default_model: None,
                tier_haiku: None,
                tier_sonnet: None,
                tier_opus: None,
                balance_query: None,
                cached_balance: None,
                cost_multiplier: None,
                icon: None,
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

        // 一次成功：解除熔断、立刻回到候选池，但 `fail_count` 是**减半**而非清零
        // （2026-08-23 改，理由见 `record_live_success` 文档：清零会让「三次里坏两次」
        // 的 Key 永远熔断不了）。
        record_live_success(&store, "k1", None);
        let h = store.get_key("k1").unwrap().health;
        assert_eq!(h.fail_count, N / 2, "成功应让计数减半（不是清零，也不是不变）");
        assert!(h.breaker_until.is_none(), "成功必须解除熔断");
        assert_eq!(h.status, HealthStatus::Up);
        // 减半不得动摇「恢复的 Key 立刻能用」这条性质 —— 候选资格只看熔断窗口。
        assert!(is_candidate(&h), "减半后仍必须立即可用（候选资格不看 fail_count）");

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
            allow_in_aggregate: false,
            priority: 0,
            headers_json: None,
            params: KeyParams::default(),
            models: vec![],
            mappings: vec![],
            default_model: None,
            tier_haiku: None,
            tier_sonnet: None,
            tier_opus: None,
            balance_query: None,
            cached_balance: None,
            cost_multiplier: None,
            icon: None,
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

        // 一次成功：解除熔断、标记 Up → 立即恢复候选。`fail_count` 减半（3 → 1）。
        record_live_success(&store, "k", None);
        let h = store.get_key("k").unwrap().health;
        assert_eq!(h.fail_count, 1, "3 次失败后一次成功 → 减半为 1");
        assert!(h.breaker_until.is_none());
        assert_eq!(h.status, HealthStatus::Up);
        assert!(is_candidate(&h), "成功后应立即恢复");
    }

    /// 「三次里坏两次」的 Key **必须**最终熔断。
    ///
    /// 这是把 `record_live_success` 从「清零」改成「减半」的**唯一理由**，
    /// 故单独一条测试盯住它：清零语义下这个循环永远跑不出熔断
    /// （每次成功把计数抹平，`BREAKER_THRESHOLD` 再也够不着），
    /// 用户体验就是一条「时好时坏」的 Key 永远赖在候选池首位。
    #[test]
    fn a_flapping_key_eventually_trips_the_breaker() {
        let store = temp_store();
        store.upsert_key(key("k")).unwrap();

        // 「坏坏好」循环。清零语义下 fail_count 恒在 {1,2,0} 之间跳，永不达 3。
        let mut tripped = false;
        for _ in 0..6 {
            record_live_failure(&store, "k");
            record_live_failure(&store, "k");
            if store.get_key("k").unwrap().health.breaker_until.is_some() {
                tripped = true;
                break;
            }
            record_live_success(&store, "k", None);
        }
        assert!(
            tripped,
            "「三次里坏两次」的 Key 必须最终熔断；若这里恒不熔断，说明 record_live_success \
             又回到了清零语义 —— 那条洞正是本测试存在的原因"
        );

        // 🔴 熔断必须在**日志页**留痕，不能只发系统通知。
        //
        // 那句注释此前写着「发系统通知 + 记一条告警事件」而代码只做了前半 ——
        // 于是排障者按注释去日志页搜「熔断」什么都搜不到；而系统通知可能被免打扰/
        // 焦点助手静音，此时这次熔断在应用里**完全不可见**（模型级锁定那一层一直有事件，
        // Key 级反倒没有，两层可见性不对称）。
        let hit = store
            .list_all_events()
            .into_iter()
            .find(|e| e.detail.contains("已熔断"))
            .expect("熔断跃迁必须落一条事件");
        assert_eq!(hit.kind, "warning", "熔断是告警级，不是普通 failover");
        assert_eq!(hit.key_id.as_deref(), Some("k"));
        // 🔴 不许无条件承诺「其它 Key 自动接管」—— 本函数不知道请求的是哪个模型。
        // 用户 2026-09-01 的日志里这句话被 503 闸门当场否证两次（间隔 791ms / 191ms）。
        assert!(
            !hit.detail.contains("其它 Key 自动接管")
                && !hit.detail.contains("其他 Key 将自动接管"),
            "无条件的接管承诺会让用户排除掉「代理挡了请求」这个方向，而那正是真相：{}",
            hit.detail
        );
        assert!(
            hit.detail.contains("若本池中还有能服务该模型的 Key"),
            "要如实说明接管取决于什么（条件句在两种情形下都为真）：{}",
            hit.detail
        );
    }

    /// 🔴 流内失败必须在日志页留下一条**红色**记录。
    ///
    /// 流式的日志行是在拿到 200 响应头那一刻写下的（kind `route` → 绿色「路由」组），
    /// 延迟记的是到响应头的耗时。此前流内失败只纠正健康记账，那一行**没人回头改** ——
    /// 于是用户在客户端看到报错、回到日志页看到「成功 · 200 · 1.2s」，
    /// 而它真实卡了 180 秒。排障者据此判定「代理这边没问题」。
    #[test]
    fn a_stream_that_fails_mid_flight_leaves_a_visible_error_row() {
        let store = temp_store();
        store.upsert_key(key("k")).unwrap();

        // 正常结束：一条 error 事件都不许落，否则每条正常流式请求都在「错误」组里冒一行。
        record_stream_end(&store, CategoryType::ClaudeCli, "k", "claude-opus-4-8", "glm-4.6", false);
        assert!(
            store.list_all_events().iter().all(|e| e.kind != "error"),
            "正常结束不许落错误事件"
        );

        record_stream_end(&store, CategoryType::ClaudeCli, "k", "claude-opus-4-8", "glm-4.6", true);
        let ev = store.list_all_events();
        let hit = ev.iter().find(|e| e.kind == "error").expect(
            "流内失败必须落一条 error 事件（→ 日志页红色「错误」组）—— 否则整条请求在界面上\
             只剩开流那一刻写下的「路由成功」",
        );
        assert_eq!(hit.key_id.as_deref(), Some("k"));
        assert!(hit.detail.contains("流内失败"), "{}", hit.detail);
        // 带**对外**模型名：那是用户在客户端看到的名字，上游真实名他不认识。
        assert!(hit.detail.contains("claude-opus-4-8"), "{}", hit.detail);
        assert_eq!(
            store.get_key("k").unwrap().health.fail_count,
            1,
            "健康记账仍要照记（这一半此前是对的，别改坏）"
        );

        // 折叠：反复流内失败只占一行带 ×N，不挤 MAX_EVENTS 环。
        record_stream_end(&store, CategoryType::ClaudeCli, "k", "claude-opus-4-8", "glm-4.6", true);
        let errs: Vec<_> = store
            .list_all_events()
            .into_iter()
            .filter(|e| e.kind == "error")
            .collect();
        assert_eq!(errs.len(), 1, "同 Key 同模型的连续流内失败应折叠成一条");
        assert_eq!(errs[0].repeat, 2, "折叠后要带次数，否则量级看不出来");
    }

    /// 🔴 接线判据：**两条**流式出口都必须走 `record_stream_end`。
    ///
    /// 上面那条只测函数本身 —— 把 proxy.rs 任一处改回「自己写二选一 `record_live_*`」
    /// 它照样全绿，而那正是「流内失败在日志页看不见」这个缺陷本身。
    /// 同协议直通与跨协议翻译是两条独立的流末路径，各写一遍必然漏掉一条
    /// （本仓已栽 10 次的接线盲区）。
    #[test]
    fn both_streaming_exits_must_record_through_record_stream_end() {
        let src = std::fs::read_to_string("src/proxy.rs").unwrap();
        let prod = crate::proxy::custom_headers::production_code_only(&src);
        assert_eq!(
            prod.matches("record_stream_end(").count(),
            2,
            "同协议直通与跨协议翻译两条流末路径都要走它"
        );
        // 流式路径不许自己记成功。历史缺陷（实测复现过）：拿到 2xx 响应头就同步
        // `record_live_success` 清零 fail_count，于是「流内报错补记失败」永远只能把它
        // 从 0 加到 1、够不到熔断阈值 → 该 Key 永不熔断、客户端无限重试同一条坏 Key。
        // 生产段里 `record_live_success` 只该剩**非流式**成功分支那一处。
        assert_eq!(
            prod.matches("record_live_success(").count(),
            1,
            "只有非流式成功分支能直接记成功；流式一律走 record_stream_end（流末才知道结果）"
        );
    }

    #[test]
    fn live_success_records_grace_timestamp() {
        let store = temp_store();
        store.upsert_key(key("k")).unwrap();
        let before = Utc::now().timestamp_millis();

        record_live_success(&store, "k", None);
        let h = store.get_key("k").unwrap().health;
        let ts = h.last_live_success.expect("应记录真实成功时间戳，供探测宽限窗口判定");
        assert!(ts >= before, "时间戳应为记录时刻");

        // 宽限窗口内：探测失败不应熔断该 Key（在 check_one 里用 last_live_success 判定）。
        let now = Utc::now().timestamp_millis();
        assert!(now - ts < LIVE_SUCCESS_GRACE_MS, "刚记录应在宽限窗口内");
    }

    // ==================== 弹性第二层：单模型锁定 ====================
    //
    // 这一层存在的唯一理由是「作用域」：`404` 说的是「这个模型没有」，不是「这条 Key 坏了」。
    // 下面每条测试都盯住这句话的一个侧面。

    /// 层与层的**基本分离**：404 只锁模型，绝不动 Key 级的 `fail_count` / `breaker_until`。
    #[test]
    fn model_lock_does_not_touch_the_key_level_breaker() {
        let store = temp_store();
        store.upsert_key(key("k")).unwrap();

        // 连锁 5 次同一个模型 —— 远超 BREAKER_THRESHOLD。
        for _ in 0..5 {
            record_model_unavailable(&store, "k", "gpt-9-turbo");
        }
        let h = store.get_key("k").unwrap().health;
        assert_eq!(
            h.fail_count, 0,
            "模型级失败绝不能累加 Key 级计数 —— 否则分层就白做了"
        );
        assert!(
            h.breaker_until.is_none(),
            "只有一个模型不可用时不该熔断整条 Key（升级阀门要 {MODEL_LOCK_ESCALATE_AT} 个不同模型）"
        );
        assert_eq!(h.model_locks.len(), 1);
        assert_eq!(h.model_locks["gpt-9-turbo"].fail_count, 5);
    }

    /// 这一层的**产品价值**：被锁的模型走不通，同一条 Key 的其它模型照常可用。
    ///
    /// 旧行为（只有 Key 级熔断）下第二个断言必然失败 —— 整条 Key 被打停，
    /// 它本来能服务的模型一起被挡住。
    #[test]
    fn only_the_locked_model_is_blocked_other_models_on_the_same_key_still_serve() {
        let store = temp_store();
        store.upsert_key(key("k")).unwrap();
        record_model_unavailable(&store, "k", "gpt-9-turbo");
        let h = store.get_key("k").unwrap().health;

        assert!(
            !is_candidate_for_model(&h, Some("gpt-9-turbo")),
            "被锁的模型必须被挡住"
        );
        assert!(
            is_candidate_for_model(&h, Some("claude-opus-5")),
            "同一条 Key 的其它模型必须照常可用 —— 这就是分层的全部意义"
        );
        // 不指定模型（模型发现、UI 展示）时只看第一层
        assert!(is_candidate_for_model(&h, None), "不针对模型时只看熔断层");
        assert!(is_candidate(&h), "Key 级依然健康");
    }

    /// 锁窗按指数退避递增，并被 `MODEL_LOCK_MAX_MS` 夹住。
    #[test]
    fn model_lock_backoff_grows_exponentially_and_is_capped() {
        let store = temp_store();
        store.upsert_key(key("k")).unwrap();

        let remaining = |store: &Arc<Store>| {
            let h = store.get_key("k").unwrap().health;
            h.model_locks["m"].until - Utc::now().timestamp_millis()
        };

        record_model_unavailable(&store, "k", "m");
        let first = remaining(&store);
        assert!(
            (first - MODEL_LOCK_BASE_MS).abs() < 2_000,
            "第一次应约等于基础时长，实际 {first}ms"
        );

        record_model_unavailable(&store, "k", "m");
        let second = remaining(&store);
        assert!(second > first * 3 / 2, "第二次应显著变长: {first} → {second}");

        // 打到远超上限的档位，必须被夹住。
        for _ in 0..30 {
            record_model_unavailable(&store, "k", "m");
        }
        let capped = remaining(&store);
        assert!(
            capped <= MODEL_LOCK_MAX_MS + 2_000,
            "锁窗必须被 {MODEL_LOCK_MAX_MS}ms 夹住，实际 {capped}ms —— 否则模型可能被锁到天荒地老"
        );
        // 并且没有因为 1<<n 溢出而 panic 或变成负数
        assert!(capped > 0, "锁窗算成了非正数：{capped}");

        // ⚠️ 必须把 `fail_count` 推过 **64**：`steps` 那个 `.min(20)` 夹子防的不是「窗口太长」
        // （`saturating_mul` + `.min(MAX_MS)` 已经管住了），而是 **`1i64 << 64` 的移位 panic**
        // ——debug 构建下那是 `attempt to shift left with overflow` 直接崩，
        // 而这条崩溃会发生在**转发热路径上**（一条模型被反复 404 的 Key 攒够次数就崩）。
        //
        // 写这条测试时先只跑到 33 次，去掉 `.min(20)` 依然全绿 —— 那是个假的安全感。
        // 教训同 CLAUDE.md 里 `db_copy_path` 那条：注入不变红时先怀疑用例没压到边界。
        for _ in 0..70 {
            record_model_unavailable(&store, "k", "m");
        }
        let h = store.get_key("k").unwrap().health;
        assert!(
            h.model_locks["m"].fail_count > 64,
            "前置条件：计数要真的推过 64（实际 {}）",
            h.model_locks["m"].fail_count
        );
        let still = remaining(&store);
        assert!(
            still > 0 && still <= MODEL_LOCK_MAX_MS + 2_000,
            "计数远超移位位宽后仍必须给出合法锁窗，实际 {still}ms"
        );
        // 衰减路径上有同一份指数计算，同样要过 64 这一关。
        record_live_success(&store, "k", Some("m"));
        let after = store.get_key("k").unwrap().health.model_locks["m"].until
            - Utc::now().timestamp_millis();
        assert!(after > 0, "衰减路径也不得算出非正锁窗：{after}ms");
    }

    /// 成功衰减：`fail_count` 减半，到 0 时整条删除（解锁）。
    #[test]
    fn success_decays_the_model_lock_and_eventually_clears_it() {
        let store = temp_store();
        store.upsert_key(key("k")).unwrap();
        for _ in 0..3 {
            record_model_unavailable(&store, "k", "m");
        }
        assert_eq!(store.get_key("k").unwrap().health.model_locks["m"].fail_count, 3);

        // 第一次成功：3 → 1，仍然锁着（但窗口应已缩短）
        record_live_success(&store, "k", Some("m"));
        let h = store.get_key("k").unwrap().health;
        assert_eq!(h.model_locks["m"].fail_count, 1, "应减半");

        // 第二次成功：1 → 0 → 整条删除，模型解锁
        record_live_success(&store, "k", Some("m"));
        let h = store.get_key("k").unwrap().health;
        assert!(
            h.model_locks.is_empty(),
            "计数归零应删除整条锁，实际仍有 {:?}",
            h.model_locks
        );
        assert!(is_candidate_for_model(&h, Some("m")), "解锁后应可用");
    }

    /// 成功只衰减**本次成功的那个**模型，不碰别的。
    #[test]
    fn success_decays_only_the_model_that_actually_succeeded() {
        let store = temp_store();
        store.upsert_key(key("k")).unwrap();
        record_model_unavailable(&store, "k", "a");
        record_model_unavailable(&store, "k", "b");

        record_live_success(&store, "k", Some("a"));
        let h = store.get_key("k").unwrap().health;
        assert!(!h.model_locks.contains_key("a"), "a 成功后应解锁");
        assert!(
            h.model_locks.contains_key("b"),
            "b 没成功过，不该被顺手解锁 —— 那等于用一个模型的成功给另一个背书"
        );
    }

    /// 「稳态成功不落盘」的短路判据里必须带上 `model_locks.is_empty()`。
    ///
    /// 少了这一条，一条 Key 级健康、但还挂着模型锁的 Key 会在短路处提前 return，
    /// 锁永远得不到衰减 —— 又造一个「只能进不能出」的状态。
    #[test]
    fn steady_state_shortcut_does_not_strand_a_model_lock() {
        let store = temp_store();
        store.upsert_key(key("k")).unwrap();

        // 先把 Key 级刷成「健康且新鲜」，让短路条件的前四项全部成立。
        record_live_success(&store, "k", None);
        let h = store.get_key("k").unwrap().health;
        assert_eq!(h.fail_count, 0);
        assert!(h.breaker_until.is_none());
        assert_eq!(h.status, HealthStatus::Up);

        // 此时给某个模型上锁，再来一次该模型的成功。
        record_model_unavailable(&store, "k", "m");
        record_live_success(&store, "k", Some("m"));
        assert!(
            store.get_key("k").unwrap().health.model_locks.is_empty(),
            "稳态短路不得把模型锁卡死 —— 若这里仍有锁，说明短路判据漏了 model_locks"
        );
    }

    /// 升级阀门：同一条 Key 上锁到第 N 个**不同**模型 → 熔断整条 Key。
    ///
    /// 这个阀门是分层的配套代价：模型级刻意不罚 Key，那么一条「对什么模型都回 404」的 Key
    /// 就会永远待在候选池首位、每来一个新模型都要白试一次。
    #[test]
    fn locking_enough_distinct_models_escalates_to_the_key_breaker() {
        let store = temp_store();
        store.upsert_key(key("k")).unwrap();

        for i in 0..MODEL_LOCK_ESCALATE_AT - 1 {
            record_model_unavailable(&store, "k", &format!("m{i}"));
            assert!(
                store.get_key("k").unwrap().health.breaker_until.is_none(),
                "第 {} 个模型被锁时还不该升级（阈值是 {MODEL_LOCK_ESCALATE_AT}）",
                i + 1
            );
        }
        record_model_unavailable(&store, "k", "last");
        let h = store.get_key("k").unwrap().health;
        assert!(
            h.breaker_until.is_some(),
            "锁满 {MODEL_LOCK_ESCALATE_AT} 个不同模型必须升级成 Key 级熔断，\
             否则一条「什么都 404」的 Key 会永远赖在候选池首位"
        );
        assert!(!is_candidate(&h));
    }

    /// 🔴 **早已陈旧的模型锁必须被回收**，否则是单调增长的持久化状态。
    ///
    /// 条目只在「该模型成功一次」时才被 `decay_model_lock` 删掉。而一条「什么模型都 404」
    /// 的 Key 上成功**永远不会发生** —— 用户每换一个模型名就多一条，全部随 `HealthState`
    /// 进 `config.json`，而健康态每次落盘都整份序列化。方向上永不自愈。
    ///
    /// 判据同时钉住两个方向，缺一个这条就没意义：
    /// - 早已陈旧的（超 `MODEL_LOCK_STALE_AFTER_MS`）必须被扫掉；
    /// - **刚到期不久的必须留着** —— `fail_count` 是退避阶梯的记忆，扫早了会让
    ///   「每隔几分钟失败一次」的模型永远停在第一档 120s，正是 `decay_model_lock`
    ///   注释里「成功即删」要避免的高频白打上游。
    #[test]
    fn stale_model_locks_are_swept_but_recently_expired_ones_are_kept() {
        let store = temp_store();
        store.upsert_key(key("k")).unwrap();
        let now = Utc::now().timestamp_millis();

        let _ = store.mutate_health("k", |h| {
            // 早已陈旧：到期时间在「now - 陈旧阈值」之前
            h.model_locks.insert(
                "ancient".into(),
                crate::model::ModelLock {
                    until: now - MODEL_LOCK_STALE_AFTER_MS - 60_000,
                    fail_count: 4,
                },
            );
            // 刚到期不久：退避记忆仍有价值
            h.model_locks.insert(
                "just-expired".into(),
                crate::model::ModelLock { until: now - 60_000, fail_count: 3 },
            );
            true
        });

        // 触发一次记账（扫描挂在这里 —— 这是唯一会让表变大的地方）
        record_model_unavailable(&store, "k", "fresh");

        let h = store.get_key("k").unwrap().health;
        assert!(
            !h.model_locks.contains_key("ancient"),
            "早已陈旧的条目必须被回收，否则 config.json 单调增长"
        );
        assert!(
            h.model_locks.contains_key("just-expired"),
            "刚到期的必须留着 —— 扫早了会把退避档位打回第一档"
        );
        assert_eq!(
            h.model_locks["just-expired"].fail_count, 3,
            "留下的条目不该被改动"
        );
        // 本次要锁的模型当然要在表里。这条是顺带确认，不是「扫描顺序」的判据 ——
        // 那个顺序由借用检查器保证（把 sweep 挪到 entry() 之后是 E0499，编译不过）。
        assert!(h.model_locks.contains_key("fresh"), "本次要锁的模型必须在表里");
        assert!(h.model_locks["fresh"].until > now, "新锁应当生效");
    }

    /// 扫描**不得**改变升级阀门的结论。
    ///
    /// 两者的口径必须保持独立：阀门只数 `until > now`，而扫描只删「到期且已很久」的。
    /// 若哪天把扫描判据放宽成「到期就删」，这条会连同上面那条一起变红。
    #[test]
    fn sweeping_does_not_change_the_escalation_verdict() {
        let now = Utc::now().timestamp_millis();
        let mut h = HealthState::default();
        for (name, until) in [
            ("live1", now + 60_000),
            ("live2", now + 60_000),
            // ⚠️ 这条**必须在**：没有它，把判据放宽成「到期就删」时本条测试照样绿
            // （活锁计数确实没变），而上面那条注释声称的「会一起变红」就是句假话。
            // 写这条测试时第一版正是这样，靠故障注入 B 才发现。
            ("just-expired", now - 60_000),
            ("ancient", now - MODEL_LOCK_STALE_AFTER_MS - 1),
        ] {
            h.model_locks
                .insert(name.into(), crate::model::ModelLock { until, fail_count: 1 });
        }
        let before = active_model_lock_count(&h, now);
        let swept = sweep_stale_model_locks(&mut h, now);
        assert_eq!(swept, 1, "只该扫掉那条陈旧的（刚到期的不算）");
        assert!(
            h.model_locks.contains_key("just-expired"),
            "刚到期的条目必须留着 —— 它的 fail_count 是退避阶梯的记忆"
        );
        assert_eq!(
            active_model_lock_count(&h, now),
            before,
            "活锁计数不得因扫描而变化 —— 变了说明扫到了仍生效的条目"
        );
    }

    /// 升级阀门只数**仍然生效**的锁，不数已过期的残留条目。
    ///
    /// 条目只在「该模型成功一次」时才被删（同 `breaker_until` 的口径），所以过期条目会长期
    /// 留在表里。若按 `len()` 数，一条 Key 上历史累计锁过 3 个模型就会被永久反复熔断。
    #[test]
    fn escalation_counts_live_locks_not_expired_leftovers() {
        let store = temp_store();
        store.upsert_key(key("k")).unwrap();
        let now = Utc::now().timestamp_millis();

        // 手工塞两条**已过期**的锁 + 一条新鲜的
        let _ = store.mutate_health("k", |h| {
            for m in ["old1", "old2"] {
                h.model_locks.insert(
                    m.to_string(),
                    crate::model::ModelLock { until: now - 60_000, fail_count: 1 },
                );
            }
            true
        });
        record_model_unavailable(&store, "k", "fresh");

        let h = store.get_key("k").unwrap().health;
        assert_eq!(h.model_locks.len(), 3, "表里确实有 3 条（含 2 条过期残留）");
        assert!(
            h.breaker_until.is_none(),
            "只有 1 条锁真的生效，不该升级 —— 若这里熔断了，说明阀门在数 len() 而不是数活锁"
        );
    }

    /// 到期即自动放行（惰性恢复，无后台定时器）。判据与 `breaker_until` 完全同源。
    #[test]
    fn an_expired_model_lock_stops_blocking_even_though_the_entry_remains() {
        let now = Utc::now().timestamp_millis();
        let mut h = HealthState::default();
        h.model_locks.insert(
            "m".into(),
            crate::model::ModelLock { until: now - 1, fail_count: 9 },
        );
        assert!(
            is_candidate_for_model(&h, Some("m")),
            "窗口已过期就该放行 —— 条目仍在不等于还锁着（同 breaker_until 的口径）"
        );

        h.model_locks.get_mut("m").unwrap().until = now + 60_000;
        assert!(!is_candidate_for_model(&h, Some("m")), "窗口内必须挡住");
    }

    /// 空模型名不得凭空锁住什么，也不得凭空挡住什么。
    ///
    /// 解析不出模型名时猜一个键会锁掉一个本来好的模型，而那种误伤是静默的。
    #[test]
    fn an_empty_model_name_locks_nothing_and_blocks_nothing() {
        let store = temp_store();
        store.upsert_key(key("k")).unwrap();
        record_model_unavailable(&store, "k", "");
        let h = store.get_key("k").unwrap().health;
        assert!(h.model_locks.is_empty(), "空模型名不该产生锁条目");
        assert!(is_candidate_for_model(&h, Some("")), "空模型名不该被挡");
    }

    /// 老 `config.json`（没有 `modelLocks` 字段）必须仍能读进来。
    ///
    /// 这条盯的是「加字段把老用户的配置读崩」——那是升级即数据丢失级的事故。
    /// `serde(default)` 靠注释提醒不住，得有机械判据。
    #[test]
    fn a_config_without_model_locks_still_deserializes() {
        // 刻意手写一份**缺** modelLocks 的 health（模拟 0.1.31 之前落盘的形态）
        let json = r#"{"status":"up","failCount":2,"breakerUntil":1234567890}"#;
        let h: HealthState = serde_json::from_str(json).expect("老配置必须能读");
        assert_eq!(h.fail_count, 2);
        assert_eq!(h.breaker_until, Some(1234567890));
        assert!(h.model_locks.is_empty(), "缺字段时应回退成空表");
    }

    /// 空锁表**不得**被序列化出去。
    ///
    /// 判据不是洁癖：`config.json` 里每条 Key 都带一个 `"modelLocks":{}` 是纯噪声，
    /// 而且这个结构会随 Key 数放大。`skip_serializing_if` 少写一次就静默退化。
    #[test]
    fn an_empty_model_lock_table_is_omitted_from_json() {
        let h = HealthState::default();
        let s = serde_json::to_string(&h).unwrap();
        assert!(!s.contains("modelLocks"), "空表不该落盘，实际: {s}");

        let mut h2 = HealthState::default();
        h2.model_locks
            .insert("m".into(), crate::model::ModelLock { until: 1, fail_count: 1 });
        let s2 = serde_json::to_string(&h2).unwrap();
        assert!(s2.contains("modelLocks"), "非空表必须落盘，实际: {s2}");
        // 前端 types.ts 用的是 camelCase 键名，两侧必须一致
        assert!(s2.contains("failCount"), "字段名应是 camelCase（前端 types.ts 依赖它）: {s2}");
    }

    /// 模型被锁时必须落一条日志。
    ///
    /// 不落的话这一层是**不可见**的：用户只看到第一次请求的故障转移，之后那条 Key 被静默
    /// 跳过 —— 界面上表现为「它明明启用着、健康着，却好像没在用」。
    #[test]
    fn locking_a_model_writes_a_visible_event() {
        let store = temp_store();
        store.upsert_key(key("k")).unwrap();
        record_model_unavailable(&store, "k", "gpt-9");

        let found = store
            .list_all_events()
            .into_iter()
            .any(|e| e.detail.contains("gpt-9") && e.detail.contains("锁定"));
        assert!(
            found,
            "模型锁定必须留下可见事件，否则这一层对用户完全不可见。实际事件: {:?}",
            store.list_all_events().iter().map(|e| e.detail.clone()).collect::<Vec<_>>()
        );
    }
}
