//! 弹性第三层：**余额闸门**（docs/14 §21.1 B4）。
//!
//! 挂 `#[path]` 在 `health` 下（`crate::health::balance_gate`），理由同 `lan_guard` 挂
//! `proxy`：宿主是它唯一的刷新发起方（[`refresh_due`] 由 `check_all_categories` 调），
//! 而 `store.rs` / `lib.rs` / `proxy.rs` 三个本该是"自然家"的文件棘轮余量都是 **0**，
//! `health.rs` 有余量。它与另两层（Key 级熔断、单模型锁定）也确实同族。
//!
//! # 它治什么
//!
//! 代理此前对余额**完全不知情**：`Store::candidates_for` 只按 `priority` 排序，
//! `proxy.rs` 对余额零引用。于是一条**已经欠费**的 Key 只要排在前面，每个请求都先撞它：
//! - 上游回 **429**（中转站表达「额度用尽」最常见的形态）→ `TRANSIENT_4XX` 刻意**不计熔断**
//!   （不为上游抖动罚好 Key）→ 它永远留在候选池首位，**每个请求白耗一次往返**；
//! - 上游回 402/403 → 连撞三次熔断 60s，之后放回来、再撞三次、再熔断 ——
//!   **周期性反复白打**，而欠费不会自愈。用熔断表达一个永久故障是错的抽象
//!   （熔断的语义是「等一等可能就好」，见 `proxy::TRANSIENT_4XX` 那条注释）。
//!
//! 故本层的作用**不是防止失败**（失败早已被故障转移兜住），而是**别等失败才知道**。
//!
//! # 🔴 三条判据边界，每条对应一种误伤（各有测试 + 注入验证）
//!
//! 1. **只认「确定耗尽」，不设阈值。** 跨厂商比一个绝对数字不可靠 —— 余额字段的量纲各家
//!    不同（`balance.rs` 记着 Kimi 的 snake_case `available_balance` 与 Novita 那个 camelCase
//!    同名字段「量纲还不同，别合并」），而百分比要 `total`、上游给了才有。
//!    `remaining <= 0` 是唯一跨厂商都成立的判据：**零在任何货币里都是零**。
//! 2. **查不到 ≠ 为零。** 后端刻意让 `remaining` 可缺失（查不到时不给 0），`KeyCard` 那边
//!    写着「若写 `?? 0` 就会把『取不到』渲染成『余额 0』，后端为此付的代价不能在最后一步
//!    毁掉」。路由侧同一口径：`!ok` / `remaining == None` 一律 [`Verdict::Unknown`]，**不降级**。
//! 3. **降级，不剔除。** 耗尽的 Key 排到候选池末尾、仍在池子里。硬剔除会在「余额数据本身
//!    错了」时把一条好 Key 完全屏蔽，而那种误伤是静默的；更糟的是全部 Key 都被判耗尽时
//!    用户「一条都用不了」—— 比慢一点糟得多。这也与熔断的兜底语义一致
//!    （`candidates_for` 全被挡时忽略门槛全部纳入）。
//!
//! # 数据源：为什么必须先搬到后端
//!
//! 判据要在**请求到达那一刻**成立，而余额此前是**前端驱动**的：唯一入口是 IPC
//! `query_key_balance`，调用方只有 `KeyCard` 的 effect/轮询与编辑器的「测试查询」按钮，
//! 而 `usePolling` **窗口不可见就停表**。加上 `cached_balance` 带 `#[serde(skip)]`
//! （纯内存、重启即清空），托盘常驻用户在请求到达时的余额认知通常**是空的**。
//!
//! # ⚠️ 后台刷新刻意受三重约束（它反转了一个刻意决定，不该反转得更多）
//!
//! `KeyCard` 里写着「窗口不可见时停表：余额查询打的是真实上游、消耗额度，最小化到托盘后
//! 还在后台定时烧额度是用户不会预期的」。本模块**确实**要在后台查（路由需要新鲜值），
//! 但只在三条同时成立时（见 [`due_for_refresh`] 与 [`refresh_due`]）：
//! 1. 该 Key 的余额查询**已启用**；
//! 2. 用户**已明确开启自动查询**（`auto_interval_min > 0`）。`0` 的字段语义就是「不自动查，
//!    只在用户点刷新时查」，后台替他查 = 界面撒谎 + 悄悄烧额度。代价如实写在这里：
//!    **没开自动查询的用户，本闸门只在一次手动查询后的 TTL 内起作用**；
//! 3. 该分类的**代理正在跑**。不跑就不路由，不路由就不需要知道。
//!
//! 周期用**用户自己设的那个值**（同前端口径：1 分钟下限 + 90% 余量），不另立一套 ——
//! 第二套周期就是第二个事实来源。
//!
//! # ⚠️ 已知的不对称：只刷**启用**的 Key（审查时讨论过，刻意如此）
//!
//! [`refresh_due`] 扫的是 `enabled_key_ids`，故「已禁用 + 勾了允许大脑聚合」那种 Key
//! **不在后台刷新范围内**。它确实在被聚合真实调用、真实烧额度，而 `KeyCard` 的
//! `canAutoQuery` 也正是为此把它纳进了前台轮询。
//!
//! 后台仍不刷它的理由：本模块的职责是**给路由供数**，而聚合不走 `candidates_for`
//! （按 `keyId::model` 精确调用，见 `aggregate.rs`）—— 为一个不参与路由的 Key
//! 在后台定期打上游，属于「拿路由的名义去做别的事」。代价如实写在这里：
//! **托盘常驻用户看这类 Key 的余额会偏旧**（打开窗口即刷新）。
//! 同 `KeyCard` 那句「健康探测仍只对 `enabled` 发 → 这类 Key 健康状态停在未知」的不对称。
//! 要改的话，判据应当是「它是否被某个分类的聚合选为成员/决策者/汇总者」，
//! 而不是顺手把 `allow_in_aggregate` 加进这里 —— 后者会把没被任何聚合选中的 Key 也一起刷。

use crate::error::{AppError, AppResult};
use crate::model::{BalanceQuery, BalanceResult, ProviderKey};
use crate::store::Store;
use std::sync::Arc;

/// 余额对路由的结论。刻意只有三支 —— 中间没有「偏低」那一档，理由见模块头判据 1。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// 没配 / 没查过 / 查失败 / 上游没给数值。**不影响路由。**
    Unknown,
    /// 有数值且大于 0。
    Ok,
    /// 数值 ≤ 0，或上游明确说这个号不能用了（`is_valid == Some(false)`）。
    Exhausted,
}

/// 从「查询是否启用」+「一次查询结果」得出结论。
///
/// 拆成纯函数是为了能脱离 `Store` 与网络单测，也让 [`query_and_record`] 在**写缓存之前**
/// 就能算出新结论（判「是否刚变成耗尽」要拿旧结论和它比）。
fn verdict_from(query_enabled: bool, r: Option<&BalanceResult>) -> Verdict {
    if !query_enabled {
        return Verdict::Unknown;
    }
    let Some(r) = r else { return Verdict::Unknown };
    // `transient` 是并发去重哨兵，压根没打上游 —— 它长得像失败但不代表任何结论。
    if r.transient {
        return Verdict::Unknown;
    }
    // 🔴 **这一支必须排在 `!r.ok` 之前。** `balance.rs` 在「上游声明账号不可用**且**没给余额
    // 数字」时返回的是 `failed(why)` + `is_valid = Some(false)` —— 也就是 `ok == false`。
    // 顺序写反（我第一版就写反了）会让**最可靠的那个信号**被当成「我们这侧查询失败」丢掉：
    // `error` 是我们这侧的失败（超时/404/字段找不到），`is_valid` 是上游明确说
    // 「这个号不能用了」，`BalanceResult` 的字段文档专门把两者分开过，判据不能再把它们混起来。
    if r.is_valid == Some(false) {
        return Verdict::Exhausted;
    }
    // 我们这侧没拿到结论。⚠️ 今天这一支与下面的 `None` 分支**重叠**（`BalanceResult::failed`
    // 一律不带 `remaining`），故单独注入它不会让任何测试变红 —— 门仍然保留：
    // 「查询失败」与「上游没给数值」是两件独立的事，早退的语义不该依赖另一处的巧合成立。
    if !r.ok {
        return Verdict::Unknown;
    }
    match r.remaining {
        // 判据 2：取不到不等于零。**不要**在这里补 `unwrap_or(0.0)`。
        None => Verdict::Unknown,
        // 负数也算耗尽：部分站点透支后回负值。NaN 落不进这一支（比较为 false）→ Unknown，
        // 那正是想要的：一个解析出 NaN 的余额不该拿来做路由决定。
        Some(v) if v <= 0.0 => Verdict::Exhausted,
        Some(_) => Verdict::Ok,
    }
}

/// 这条 Key 此刻的余额结论。
pub(crate) fn verdict(key: &ProviderKey) -> Verdict {
    verdict_from(
        key.balance_query.as_ref().is_some_and(|b| b.enabled),
        key.cached_balance.as_ref(),
    )
}

/// 候选排序用的一位判据。`false` 排在 `true` 前（`bool: Ord`），故耗尽的整体后移。
///
/// 单独给一个 `bool` 函数而不是让 `store.rs` 自己 `matches!`：那边棘轮余量为 0，
/// 且判据只该有一处 —— 将来若要把 `Unknown` 也纳入降级，改这里一处就够。
pub(crate) fn is_exhausted(key: &ProviderKey) -> bool {
    verdict(key) == Verdict::Exhausted
}

/// 诊断报告 Key 摘要行里的那一位。
///
/// 为什么值得占一列：没有它，「一条余额耗尽的 Key 为什么排在最后」这个问题，
/// 报告给不出任何答案（同「允许聚合=」那一位加进摘要行的理由）。
pub(crate) fn verdict_label(key: &ProviderKey) -> &'static str {
    match verdict(key) {
        Verdict::Unknown => "未知",
        Verdict::Ok => "正常",
        Verdict::Exhausted => "已耗尽",
    }
}

// ===================== 后台刷新 =====================

/// 一轮后台刷新的并发上限。
///
/// 比健康探测的 4 更小：余额端点常与计费面板同源、限流更紧（`balance.rs` 的探测链本身
/// 就要按序试多个端点），而这条路径**不在任何人的等待路径上** —— 慢一点没有代价。
const REFRESH_CONCURRENCY: usize = 2;

/// 「到期了吗」的**检查**节奏。不是查询周期 —— 真正多久打一次上游由用户的
/// `auto_interval_min` 决定（[`due_for_refresh`]），这里只是多久醒来看一眼。
///
/// 取 60s：它决定了用户设的间隔最多被推迟多久（设 1 分钟最坏变成 2 分钟）。
const CHECK_INTERVAL_SECS: u64 = 60;

/// 查询某个 Key 的上游余额（IPC）。
///
/// 命令放在这里而不是 `lib.rs`：同 `key_flags` / `codex_catalog` 那两条 —— 命令该跟着它的
/// 实现走，而 `lib.rs` 棘轮余量本来就紧。`Trigger::User` 表示「用户正看着」，故每次都落事件
/// （后台那条只在结论变化时落，见 [`record_outcome`]）。
///
/// **不返回 `Err` 而是把失败装进 `BalanceResult.error`** —— 理由见 [`query_and_record`]。
#[tauri::command]
pub async fn query_key_balance(
    state: tauri::State<'_, crate::AppState>,
    key_id: String,
    force: Option<bool>,
) -> AppResult<BalanceResult> {
    query_and_record(&state.store, &key_id, force.unwrap_or(false), Trigger::User).await
}

/// 起那趟独立线程。`lib.rs` 只调这一行 —— 线程与运行时的搭建细节跟着功能走，
/// 而 `lib.rs` 棘轮余量本来就紧。
pub(crate) fn spawn_background(store: Arc<Store>) {
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().expect("tokio rt");
        rt.block_on(run_forever(store));
    });
}

/// 后台刷新的**独立**循环。由 [`spawn_background`] 起的线程跑它。
///
/// 🔴 **刻意不搭健康探测那趟车**（第一版就是那么写的，审查时按三条理由推翻）：
/// 1. 那趟车在 `health_check_interval_secs == 0`（用户关掉定时探测）时直接 `continue`
///    —— 余额刷新会跟着一起停，而它依赖的是**另一个**设置（`auto_interval_min`）。
///    两个无关设置的隐式耦合，且失效是静默的。
/// 2. 那趟车的周期跟着用户配的探测间隔走。用户把探测设成 3600s、余额设成 5 分钟，
///    实际就是一小时才刷一次 —— 用户设的数字静默失效。**`flush_usage_if_dirty` 当初
///    正是为这条理由才独立起了一趟线程**（`lib.rs` 那段注释写着「用量的丢失窗口不该被
///    探测设置牵着走」），同一个理由在这里同样成立。
/// 3. 那趟车整轮套着 `timeout(period)`，余额查询会吃掉探测的预算；一旦超时，
///    日志里打出来的是「**健康探测**一轮未在 X 内完成」—— 一条指错方向的告警。
///
/// 轮次不重叠：`sleep` 排在一轮之后（同健康探测那趟车的做法）。
pub(crate) async fn run_forever(store: Arc<Store>) {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(CHECK_INTERVAL_SECS)).await;
        refresh_due(&store).await;
    }
}

/// 这条 Key 现在该不该由**后台**去刷一次余额。见模块头「三重约束」的 1 与 2。
///
/// 90% 余量与前端 `KeyCard` 的 `freshFor` 及 `Store::get_balance_cache` 同一口径：
/// 挂载即查后缓存年龄恒略小于整周期，取整会把每隔一次的 tick 全部拦掉、
/// 实际周期变成配置值的 2 倍（用户设 30 分钟实为 1 小时的静默偏差）。
fn due_for_refresh(key: &ProviderKey, now_ms: i64) -> bool {
    let Some(bq) = key.balance_query.as_ref().filter(|b| b.enabled) else {
        return false;
    };
    if bq.auto_interval_min == 0 {
        // 🔴 用户明确说了「不自动查」。后台替他查 = 界面撒谎 + 悄悄烧额度。
        return false;
    }
    let period_ms = (bq.auto_interval_min.max(1) as i64) * 60_000;
    match key.cached_balance.as_ref() {
        None => true,
        Some(c) => now_ms - c.queried_at >= period_ms * 9 / 10,
    }
}

/// 后台刷新一轮到期的余额。挂在 `health::check_all_categories` 上。
///
/// **只扫代理正在跑的分类**（模块头约束 3）：不跑就不路由，不路由就不需要新鲜余额，
/// 而每次查询都是一次真实上游请求。
///
/// 锁定态整轮跳过，同 `check_all_categories`：取不到密钥时每条都会查成失败，
/// 把一堆「未配置密钥」写进事件流，而真实原因只是没解锁。
pub(crate) async fn refresh_due(store: &Arc<Store>) {
    if store.secrets.read().is_locked() {
        return;
    }
    let running = store.get_settings().proxy_running_categories;
    if running.is_empty() {
        return;
    }
    let now = chrono::Utc::now().timestamp_millis();
    let mut due: Vec<String> = Vec::new();
    for cat in running {
        for id in store.enabled_key_ids(cat) {
            if store.get_key(&id).is_some_and(|k| due_for_refresh(&k, now)) {
                due.push(id);
            }
        }
    }
    use futures_util::stream::StreamExt;
    futures_util::stream::iter(due)
        .for_each_concurrent(REFRESH_CONCURRENCY, |id| async move {
            // 失败已经在 `query_and_record` 内部落了事件，这里再报一遍只是噪音。
            let _ = query_and_record(store, &id, false, Trigger::Background).await;
        })
        .await;
}

// ===================== 单次查询（唯一实现） =====================

/// 正在查询余额的 Key 集合，防同一条 Key 被并发重复查询。
///
/// 🔴 **进程级 static，而不是 `AppState` 上的字段**：那样只有 IPC 命令能看见它，
/// 后台刷新（只拿得到 `&Arc<Store>`）就成了第二个不受去重约束的入口 ——
/// 用户点一下「刷新」正好撞上后台那一轮，两个请求同时打上游、两次都写缓存，
/// 后到的那个（可能是失败）盖掉先到的成功。去重集合必须与查询实现同一处。
fn in_flight() -> &'static parking_lot::Mutex<std::collections::HashSet<String>> {
    static S: std::sync::OnceLock<parking_lot::Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    S.get_or_init(Default::default)
}

/// 谁发起了这次查询。**决定要不要落那条例行事件**，见 [`record_outcome`]。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Trigger {
    /// 用户点了刷新 / 测试查询，或前端进入界面时拉取 —— 他正看着，结果必须留痕。
    User,
    /// 后台到期自动刷。**只在结论变化时留痕**，理由见 [`record_outcome`]。
    Background,
}

/// 查一条 Key 的余额，并把结果落进事件流 + 缓存（**唯一实现**）。
///
/// `query_key_balance` 那个 IPC 命令与 [`refresh_due`] 都走这里。两份实现必然漂移，
/// 而这条路上「事件 kind 分流」「探测命中地址回写」「缓存写入」三步各自都有踩坑记录，
/// 抄一份就是把那三条教训复制到一个没人会同步维护的副本里。
///
/// **不返回 `Err` 而是把失败装进 `BalanceResult.error`**：余额查不到是常态
/// （站点没这接口、路径填错、网络抖动），而 IPC 层的 `Err` 在前端会变成抛出的异常、
/// 需要 try/catch 才不炸掉整个面板。用值表达失败让前端能在卡片上就地显示原因。
/// 真正的 `Err` 只留给「Key 不存在」这种调用方用错了的情况。
///
/// `force = true` 跳过缓存检查（编辑器「测试查询」按钮要立刻看到新值）。
pub(crate) async fn query_and_record(
    store: &Arc<Store>,
    key_id: &str,
    force: bool,
    trigger: Trigger,
) -> AppResult<BalanceResult> {
    if !force {
        if let Some(cached) = store.get_balance_cache(key_id) {
            tracing::debug!(
                "余额缓存命中: key={} remaining={:?} age={}s",
                key_id,
                cached.remaining,
                (chrono::Utc::now().timestamp_millis() - cached.queried_at) / 1000
            );
            return Ok(cached);
        }
    }
    {
        let mut guard = in_flight().lock();
        if guard.contains(key_id) {
            tracing::debug!("余额查询已在进行中，拒绝重复请求: key={key_id}");
            // **标记为瞬时**：这次压根没打上游，不代表任何结论。不标的话前端会把它当真失败
            // 写进缓存，卡片被一条假错误钉住整个 TTL（最长 5 分钟），而真正在跑的那次查询
            // 结果反被这条后到的伪失败盖掉。`verdict_from` 也据此判 Unknown。
            return Ok(BalanceResult::transient("该 Key 的余额查询正在进行中，请稍候"));
        }
        guard.insert(key_id.to_string());
    }
    let owned = key_id.to_string();
    let _guard = scopeguard::guard((), move |_| {
        in_flight().lock().remove(&owned);
    });
    let Some(key) = store.get_key(key_id) else {
        return Err(AppError::NotFound(key_id.to_string()));
    };
    let Some(cfg) = key.balance_query.clone() else {
        return Ok(fail_with_event(store, &key, "该 Key 未配置余额查询"));
    };
    // 主口令锁定时取不到密钥：如实说明是「锁着」，不是「余额接口坏了」。
    if store.secrets.read().is_locked() {
        return Ok(fail_with_event(store, &key, "密钥库已锁定，请先用主口令解锁"));
    }
    // 允许用另一条密钥查余额（部分站点的计费面板与转发端点不同域、各用各的凭证）。
    let secret_ref = cfg.api_key_ref.as_deref().unwrap_or(key_id);
    let Some(secret) = store.secrets.read().get(secret_ref).ok().flatten() else {
        return Ok(fail_with_event(store, &key, "未配置密钥"));
    };
    let start = std::time::Instant::now();
    let result = crate::balance::query_balance(&key, &cfg, &secret).await;
    record_outcome(store, &key, &cfg, &result, start.elapsed().as_millis(), trigger);
    Ok(result)
}

/// 三条早退分支共用的「记一条 warning 事件并返回失败」。
///
/// kind 必须用**前端登记过的**英文标识：曾经这里写中文 "余额"，前端 `TYPE_META` 没有该键，
/// 于是走 `?? TYPE_META.route` 兜底 → 余额**查询失败**被渲染成绿色「路由成功」，
/// 且在「错误」筛选里查不到（用户完全看不见失败）。
fn fail_with_event(store: &Arc<Store>, key: &ProviderKey, reason: &str) -> BalanceResult {
    store.append_event(
        key.category_id,
        "warning",
        Some(&key.id),
        &format!("查询失败：{reason}"),
    );
    BalanceResult::failed(reason)
}

/// 一次真实查询之后的三件事：落事件、回写探测命中的地址、更新缓存。
///
/// `key` 是**查询之前**取的那一份，故 `key.cached_balance` 是旧值 —— 判「是否刚变成耗尽」
/// 正需要它。这个顺序不是巧合，改动时别把 `get_key` 挪到查询之后。
///
/// 🔴 **后台路径只在「结论变化」时落那条例行事件。** 事件环只有 `MAX_EVENTS = 500` 条、
/// 且与路由/故障转移共用；折叠也救不了这里 —— `append_event_collapsible` 只合并**紧邻**的
/// 上一条，而多条 Key 在同一轮里彼此穿插，跨轮压根挨不上。
/// 6 条 Key × 用户设的 1 分钟 = 360 条/小时，一个多小时就把整环冲干净，
/// 把排障真正需要的转发/故障转移事件挤出去（本仓缺陷分类法第 7 类「事件环被噪音挤满」）。
/// 这在改成后台刷新之后才成为可能 —— 此前它受「用户得开着那个分类页」天然限速。
/// 用户主动点刷新时（[`Trigger::User`]）照旧每次都落：他正看着，静默才是错的。
fn record_outcome(
    store: &Arc<Store>,
    key: &ProviderKey,
    cfg: &BalanceQuery,
    result: &BalanceResult,
    elapsed_ms: u128,
    trigger: Trigger,
) {
    // 脱敏：apiKey 与 accessToken 都换成 ***。`{{accessToken}}` 是占位符体系明文支持的 URL
    // 写法（部分面板把 token 放 query 参数），原样展开会把面板 token 明文写进运行日志
    // （可见、可导出、可截图）—— 此前只遮了 apiKey，与那行注释自己的声明矛盾。
    let base = cfg
        .base_url_override
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(&key.base_url);
    let token_masked = if cfg.access_token.as_deref().unwrap_or("").is_empty() {
        ""
    } else {
        "***"
    };
    let url = crate::balance::expand_placeholders(
        &cfg.url,
        base,
        "***",
        token_masked,
        cfg.user_id.as_deref().unwrap_or(""),
    );
    let detail = if let Some(err) = &result.error {
        format!("❌ 查询失败 | {elapsed_ms}ms | {url} | {err}")
    } else if let Some(remaining) = result.remaining {
        format!(
            "✅ 查询成功 | {}ms | {} | 余额 {} {}{}{}",
            elapsed_ms,
            url,
            remaining,
            result.unit.as_deref().unwrap_or("USD"),
            result.total.map(|t| format!(" / 总额 {t}")).unwrap_or_default(),
            if result.is_valid == Some(false) { " · 已失效" } else { "" }
        )
    } else {
        format!("⚠️ 查询成功但未取到余额值 | {elapsed_ms}ms | {url}")
    };
    // 按结果分流：失败 → warning（橙、归「错误」组）；成功 → balance（信息态、归「系统」组）。
    // 后台路径静默通过（见函数文档）；`announce_exhaustion` 与「地址已记住」不受此门限制，
    // 前者本身就只在变化时落，后者只在地址真的变了时落 —— 两者都不会积累。
    if trigger == Trigger::User || verdict(key) != verdict_from(true, Some(result)) {
        store.append_event(
            key.category_id,
            if result.error.is_some() { "warning" } else { "balance" },
            Some(&key.id),
            &detail,
        );
    }
    announce_exhaustion(store, key, result);
    // 探测命中的端点写回配置：此后每次只发 1 个请求，不再重跑整条探测链。
    // 走 `set_balance_query_url` 而非整份 upsert：那会把打开编辑器那一刻的旧快照写回去，
    // 顺带清掉运行中的熔断与余额缓存（`upsert_key` 的守卫只保这两项，其余字段照覆盖）。
    // 失败只记日志：地址没记住的代价是下次再探测一遍，而让一次成功的查询报错更糟。
    if let Some(tpl) = &result.resolved_url_template {
        match store.set_balance_query_url(&key.id, tpl) {
            Ok(true) => store.append_event(
                key.category_id,
                "config",
                Some(&key.id),
                &format!("余额查询地址已自动记住：{tpl}（此后不再逐个试探端点）"),
            ),
            Ok(false) => {}
            Err(e) => tracing::warn!("记住余额查询地址失败（key={}，下次会重新探测）: {e}", key.id),
        }
    }
    // 成功和失败都缓存，避免对失败端点短时间内重复轰炸。写入失败只记日志（缓存是优化手段）。
    if let Err(e) = store.update_balance_cache(&key.id, result.clone()) {
        tracing::warn!("余额缓存写入失败（key={}, 不影响本次查询）: {e}", key.id);
    }
}

/// 🔴 刚变成「已耗尽」时留一条**可折叠告警**。
///
/// 不落这条，这一层对用户**完全不可见** —— 他只会发现某条 Key 莫名排到了最后，
/// 而候选顺序在界面上压根没有呈现（主 Key 徽标看的是 `enabled_keys_sorted`，
/// 那条路刻意不受本闸门影响）。同「模型被锁必须落一条可折叠事件」那条：
/// 一层静默生效的路由干预，排障时无从解释。
///
/// **只在结论「刚变化」时落**，不是每轮都落：后台每 N 分钟刷一次，一条长期欠费的 Key
/// 会把 `MAX_EVENTS` 环刷满、把有用事件挤出去（同「短路窗口内每次重发都记一条」那个坑）。
/// 折叠键按 Key，故反复进出耗尽态只占一行带 ×N。
///
/// kind 用 `warning`（橙、归「错误」组）：它要求用户去充钱或换号，是要行动的。
fn announce_exhaustion(store: &Arc<Store>, key: &ProviderKey, result: &BalanceResult) {
    let before = verdict(key); // key 是查询前的快照，见 record_outcome 的文档
    let after = verdict_from(true, Some(result));
    if after != Verdict::Exhausted || before == Verdict::Exhausted {
        return;
    }
    let why = match (result.is_valid, result.remaining) {
        (Some(false), _) => result
            .invalid_message
            .clone()
            .unwrap_or_else(|| "上游声明该 Key 已失效".into()),
        (_, Some(v)) => format!("余额 {} {}", v, result.unit.as_deref().unwrap_or("USD")),
        _ => "余额已耗尽".into(),
    };
    store.append_event_collapsible(
        key.category_id,
        "warning",
        Some(&key.id),
        &format!(
            "余额已耗尽（{why}）—— 本 Key 已降到候选池末尾，仍会在其余 Key 都不可用时兜底使用。\
             请充值或改用其它 Key。"
        ),
        None,
        Some(format!("balance-exhausted:{}", key.id)),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CategoryType, Protocol};
    use std::path::PathBuf;

    /// 唯一临时目录（同 `store::tests::temp_dir`：pid + 进程内自增 —— 本机 `timestamp_nanos`
    /// 量化粒度只有 100ns，单靠时间戳并发下会撞名，那条坑记在 `ccswitch::db_copy_path`）。
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

    /// 一次「成功拿到数值」的查询结果。刻意从 `failed("")` 派生，免得日后
    /// `BalanceResult` 加字段时这里编译不过（那种维护成本没有价值）。
    fn queried(remaining: Option<f64>) -> BalanceResult {
        BalanceResult { ok: true, remaining, error: None, ..BalanceResult::failed("") }
    }

    fn key_with(id: &str, priority: i32, cached: Option<BalanceResult>) -> ProviderKey {
        ProviderKey {
            id: id.into(),
            category_id: CategoryType::ClaudeCli,
            name: id.into(),
            base_url: "https://api.example.com".into(),
            protocol: Protocol::Anthropic,
            enabled: true,
            priority,
            balance_query: Some(BalanceQuery { enabled: true, ..Default::default() }),
            cached_balance: cached,
            ..Default::default()
        }
    }

    /// 🔴 判据 2：**查不到 ≠ 为零。** 这是本模块最危险的误伤方向 ——
    /// 一次网络抖动/路径填错就把一条好 Key 降级，而降级是静默的。
    #[test]
    fn a_failed_query_is_never_treated_as_zero() {
        for r in [
            BalanceResult::failed("404 Not Found"),
            BalanceResult::failed("timeout"),
            // 并发去重哨兵：压根没打上游，不代表任何结论。
            BalanceResult::transient("正在进行中"),
            // 查询成功但上游没给数值（`remaining` 刻意可缺失）。
            queried(None),
        ] {
            assert_eq!(
                verdict_from(true, Some(&r)),
                Verdict::Unknown,
                "把查询失败当成余额 0 会静默降级一条好 Key：{r:?}"
            );
        }
        // 压根没查过也是 Unknown（重启后 cached_balance 为空，那是常态）。
        assert_eq!(verdict_from(true, None), Verdict::Unknown);
    }

    /// 判据 1：只有「真的 ≤ 0」或「上游明确说这个号不能用了」才算耗尽。
    #[test]
    fn only_a_real_zero_or_an_upstream_invalid_flag_counts_as_exhausted() {
        assert_eq!(verdict_from(true, Some(&queried(Some(0.0)))), Verdict::Exhausted);
        // 负数：部分站点透支后回负值。
        assert_eq!(verdict_from(true, Some(&queried(Some(-3.5)))), Verdict::Exhausted);
        assert_eq!(verdict_from(true, Some(&queried(Some(0.01)))), Verdict::Ok);
        assert_eq!(verdict_from(true, Some(&queried(Some(1234.0)))), Verdict::Ok);

        // 上游明确声明失效 —— 即使它同时回了一个正的余额数字（真实站点见过：
        // 号被封但额度还在），也按耗尽处理：那个额度已经花不出去了。
        let banned = BalanceResult { is_valid: Some(false), ..queried(Some(99.0)) };
        assert_eq!(verdict_from(true, Some(&banned)), Verdict::Exhausted);

        // 🔴 **判据顺序**：`balance.rs:525` 在「上游声明不可用**且**没给余额数字」时返回的是
        // `failed(why)` + `is_valid = Some(false)`，即 `ok == false`。若把 `!ok` 那道门排在
        // `is_valid` 之前（第一版就是），这个**最可靠的信号**会被当成「我们这侧查询失败」丢掉。
        let mut dead = BalanceResult::failed("上游报告该账号当前不可用");
        dead.is_valid = Some(false);
        assert!(!dead.ok && dead.remaining.is_none(), "夹具要复刻 balance.rs 的真实形态");
        assert_eq!(
            verdict_from(true, Some(&dead)),
            Verdict::Exhausted,
            "上游明说「这个号不能用了」不许因为它同时是一条 failed 就被忽略"
        );

        // 🔴 NaN 不许落进「耗尽」：一个解析成 NaN 的余额不该拿来做路由决定。
        // （`NaN <= 0.0` 为 false，故它自然落到 Ok —— 这条断言防的是有人日后
        //  把判据改成 `!(v > 0.0)`，那会把 NaN 一起吞进耗尽。）
        assert_eq!(verdict_from(true, Some(&queried(Some(f64::NAN)))), Verdict::Ok);
    }

    /// 余额查询没启用时，闸门必须完全不作用 —— 哪怕内存里还留着一份「余额 0」的旧缓存。
    ///
    /// 现实路径：用户发现余额查询配错了、把它关掉，而 `cached_balance` 是内存态、不会被清。
    /// 若这里仍判耗尽，那条 Key 会被一个用户已经放弃的配置永久降级。
    #[test]
    fn a_disabled_balance_query_never_affects_routing() {
        let zero = queried(Some(0.0));
        assert_eq!(verdict_from(false, Some(&zero)), Verdict::Unknown);

        let mut k = key_with("k1", 0, Some(zero));
        k.balance_query = Some(BalanceQuery { enabled: false, ..Default::default() });
        assert!(!is_exhausted(&k));
        // 压根没配过余额查询的 Key 同理。
        k.balance_query = None;
        assert!(!is_exhausted(&k));
        assert_eq!(verdict_label(&k), "未知");
    }

    /// 🔴 本模块的核心行为，且**同时钉住 `store.rs` 那行接线**：
    /// 耗尽的 Key **降到末尾**，但一条都不许消失。
    ///
    /// 「剔除」在两种真实情形下比「白打一次往返」糟得多：① 余额数据本身错了 →
    /// 一条好 Key 被完全屏蔽，而这种误伤是静默的；② 全部 Key 都被判耗尽 →
    /// 用户「一条都用不了」。故兜底那条路继承同一顺序，全耗尽时仍然有候选。
    #[test]
    fn an_exhausted_key_is_demoted_but_never_removed() {
        let s = store("gate_demote");
        // 优先级最高的那条已经欠费，第二条好着。
        s.upsert_key(key_with("broke", 0, Some(queried(Some(0.0))))).unwrap();
        s.upsert_key(key_with("rich", 1, Some(queried(Some(42.0))))).unwrap();

        let (cands, fallback) = s.candidates_for(CategoryType::ClaudeCli, "");
        let ids: Vec<&str> = cands.iter().map(|k| k.id.as_str()).collect();
        assert_eq!(ids, vec!["rich", "broke"], "余额耗尽的必须后移，但不能被剔除");
        assert!(!fallback, "这不是全熔断兜底路径");
    }

    /// 全部耗尽时仍然要有候选（顺序退化成纯优先级）。
    #[test]
    fn all_keys_exhausted_still_yields_candidates() {
        let s = store("gate_all_broke");
        s.upsert_key(key_with("a", 0, Some(queried(Some(0.0))))).unwrap();
        s.upsert_key(key_with("b", 1, Some(queried(Some(-1.0))))).unwrap();

        let (cands, _) = s.candidates_for(CategoryType::ClaudeCli, "");
        let ids: Vec<&str> = cands.iter().map(|k| k.id.as_str()).collect();
        assert_eq!(ids, vec!["a", "b"], "都耗尽时不许把池子清空，顺序退回优先级");
    }

    /// 🔴 后台刷新必须尊重「`auto_interval_min == 0` = 不自动查」。
    ///
    /// 那个字段的语义就是「只在用户点刷新时查」。后台替他查 = 界面撒谎 + 悄悄烧额度，
    /// 而余额查询打的是**真实上游**。代价（没开自动查询就享受不到闸门）写在模块头。
    #[test]
    fn the_background_refresh_respects_the_users_auto_query_setting() {
        let now = 1_000_000_000_000i64;
        let never_queried = |min: u32| {
            let mut k = key_with("k", 0, None);
            k.balance_query = Some(BalanceQuery { enabled: true, auto_interval_min: min, ..Default::default() });
            k
        };
        assert!(
            !due_for_refresh(&never_queried(0), now),
            "🔴 用户说了不自动查 —— 后台一次都不许打上游"
        );
        assert!(due_for_refresh(&never_queried(30), now), "开了自动查询、又从没查过 → 该查");

        // 关掉余额查询本身 → 不查（`query_balance` 也会拒，这里省掉那次无谓调用与事件）。
        let mut off = never_queried(30);
        off.balance_query = Some(BalanceQuery { enabled: false, auto_interval_min: 30, ..Default::default() });
        assert!(!due_for_refresh(&off, now));
        off.balance_query = None;
        assert!(!due_for_refresh(&off, now));

        // 新鲜缓存 → 不查；老于 90% 周期 → 该查。
        let with_age = |age_min: i64| {
            let mut k = never_queried(30);
            k.cached_balance = Some(BalanceResult {
                queried_at: now - age_min * 60_000,
                ..queried(Some(5.0))
            });
            k
        };
        assert!(!due_for_refresh(&with_age(10), now), "才 10 分钟，别打上游");
        // 90% 余量：同前端 `freshFor` 与 `Store::get_balance_cache` 的口径。取整周期会把
        // 每隔一次的 tick 全部拦掉、实际周期变成配置值的 2 倍（静默偏差）。
        assert!(!due_for_refresh(&with_age(26), now), "26 < 27（30 的 90%）");
        assert!(due_for_refresh(&with_age(27), now), "到了 90% 就该查，不能等满 30");
    }

    /// 代理没在跑的分类不刷 —— 不跑就不路由，不路由就不需要新鲜余额。
    ///
    /// 判据取「一条事件都没落」：`query_and_record` 的每条路径都会落事件
    /// （成功/失败/未配置密钥），所以「没有事件」等价于「压根没走进去」。
    #[tokio::test]
    async fn refresh_does_nothing_when_no_category_is_running() {
        let s = store("gate_not_running");
        // 这条 Key 各方面都「该刷」：启用、开了余额查询与自动查询、从没查过。
        let mut k = key_with("k1", 0, None);
        k.balance_query = Some(BalanceQuery { enabled: true, auto_interval_min: 1, ..Default::default() });
        s.upsert_key(k).unwrap();
        assert!(
            s.get_settings().proxy_running_categories.is_empty(),
            "夹具前提：默认没有分类在跑"
        );

        refresh_due(&s).await;
        assert!(
            s.list_all_events().is_empty(),
            "代理没跑就不该有任何余额查询动作：{:?}",
            s.list_all_events().iter().map(|e| e.detail.clone()).collect::<Vec<_>>()
        );
    }

    /// 🔴 耗尽必须**可见**，而且只在「刚变成耗尽」那一次落。
    ///
    /// 不落 → 这一层对用户完全不可见（候选顺序在界面上压根没有呈现，主 Key 徽标看的是
    /// `enabled_keys_sorted`，那条路刻意不受本闸门影响）。
    /// 每轮都落 → 后台每 N 分钟刷一次，一条长期欠费的 Key 会把 `MAX_EVENTS` 环刷满、
    /// 把有用事件挤出去（同「短路窗口内每次重发都记一条」那个坑）。
    #[test]
    fn exhaustion_is_announced_once_and_only_on_the_transition() {
        let s = store("gate_announce");
        let exhausted = queried(Some(0.0));

        // ① 旧值是「有钱」→ 新值耗尽：必须落。
        let rich = key_with("k1", 0, Some(queried(Some(10.0))));
        announce_exhaustion(&s, &rich, &exhausted);
        let ev = s.list_all_events();
        assert_eq!(ev.len(), 1, "刚耗尽必须留痕：{ev:?}");
        assert_eq!(ev[0].kind, "warning", "要行动的事（充钱/换号）必须归「错误」组");
        assert!(ev[0].detail.contains("余额已耗尽"), "{:?}", ev[0].detail);
        // 文案必须说清「降级不是剔除」，否则用户会以为这条 Key 已经废了。
        assert!(ev[0].detail.contains("兜底"), "{:?}", ev[0].detail);

        // ② 旧值已经是耗尽 → 不许再落（这就是「每轮都落」那个坑）。
        //
        // ⚠️ 判据必须看 `repeat`，**不能只数事件条数**：这条事件是可折叠的，同一个
        // collapse key 再 append 会被折叠进原来那条（`repeat` 变 2、`detail` 被覆盖），
        // 于是 `len()` 恒为 1 —— 第一版就是这么写的，注入「每轮都落」后**照样全绿**。
        // 同本仓那条教训：注入不变红时先怀疑判据压根没压到那个维度。
        let already = key_with("k1", 0, Some(queried(Some(0.0))));
        announce_exhaustion(&s, &already, &exhausted);
        let ev = s.list_all_events();
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].repeat, 1, "同一状态不许反复落事件（折叠会把它记成 ×N）");

        // ③ 新值不是耗尽 → 不落。
        announce_exhaustion(&s, &rich, &queried(Some(7.0)));
        assert_eq!(s.list_all_events()[0].repeat, 1);

        // ④ 查询失败**不许**报成耗尽（判据 2 在事件侧的另一半）。
        announce_exhaustion(&s, &rich, &BalanceResult::failed("timeout"));
        let ev = s.list_all_events();
        assert_eq!(ev.len(), 1, "查不到不是耗尽，不许惊动用户");
        assert_eq!(ev[0].repeat, 1, "查不到不是耗尽，不许惊动用户");
    }

    /// 🔴 接线判据一：候选排序必须过这道闸门。
    ///
    /// 上面那两条行为用例走的是真 `Store`，本来能抓住这条 —— 但它们只覆盖
    /// 「排序键里有它」这一个形态。这条源码级判据额外钉住「**只有一处**」，
    /// 防止日后有人在别处再补一份判据（两份必然漂移）。
    ///
    /// ⚠️ **2026-08-31 起看的是 `model_pool.rs`**：排序实现从 `store.rs` 搬进了
    /// `proxy::model_pool::rank_candidates`（模型感知路由与余额闸门是同一个排序键的两位）。
    /// `Store::candidates_for` 仍必须把决策交给它 —— 那一半由
    /// `model_pool::tests::candidates_for_must_delegate_to_this_module` 钉住。
    #[test]
    fn candidates_for_must_consult_the_balance_gate() {
        let src = std::fs::read_to_string("src/model_pool.rs").unwrap();
        let prod = crate::proxy::custom_headers::production_code_only(&src);
        assert_eq!(
            prod.matches("balance_gate::is_exhausted").count(),
            1,
            "候选排序必须过余额闸门，且判据只该有一处"
        );
        assert!(
            !prod.contains("enabled.sort_by_key(|k| k.priority)"),
            "排序键退回纯 priority 了 —— 余额闸门当场失效，而失效是静默的"
        );
    }

    /// 🔴 接线判据二：**主 Key 徽标的口径不许跟着余额动。**
    ///
    /// `enabled_keys_sorted` 是「用户配置的优先级顺序」这一事实的来源（主 Key 徽标、
    /// 托盘「主 Key」子菜单、状态条都用它）。余额耗尽是**运行态**，不该改写用户的配置视图 ——
    /// 这与熔断的处理完全一致（`candidates_for` 剔除熔断中的 Key，`enabled_keys_sorted` 不）。
    /// 让徽标跟着余额跳的表现是：用户什么都没改，主 Key 自己换了一条。
    #[test]
    fn the_primary_key_badge_must_not_follow_the_balance_gate() {
        let s = store("gate_badge");
        s.upsert_key(key_with("broke", 0, Some(queried(Some(0.0))))).unwrap();
        s.upsert_key(key_with("rich", 1, Some(queried(Some(42.0))))).unwrap();
        let ids: Vec<String> = s
            .enabled_keys_sorted(CategoryType::ClaudeCli)
            .into_iter()
            .map(|k| k.id)
            .collect();
        assert_eq!(ids, vec!["broke", "rich"], "配置视图必须保持纯优先级顺序");
    }

    /// 🔴 接线判据三：必须有**一趟独立的**后台循环在驱动刷新，且**不许**挂回健康探测那趟车。
    ///
    /// 上面那些用例全都直接调 `due_for_refresh` / `refresh_due`，把驱动那一行删掉它们
    /// **照样全绿** —— 而那正是缺陷本体：数据源退回前端驱动，托盘常驻用户的余额永远是
    /// 旧值或空值，闸门看着在工作、实际永不生效。这是本仓第 13 次盯同一类接线盲区。
    ///
    /// 反向那一半（`health.rs` 不许调）是**审查修出来的**：第一版就挂在
    /// `check_all_categories` 里，而那趟车在「用户关掉定时探测」时整轮 `continue`
    /// （余额跟着一起停）、周期被探测间隔牵着走（用户设的 5 分钟静默变成 1 小时）、
    /// 还会吃掉那一轮的 `timeout` 预算并让告警指错方向。理由全文见 `run_forever` 的文档。
    #[test]
    fn a_dedicated_background_loop_must_drive_the_refresh() {
        let lib = std::fs::read_to_string("src/lib.rs").unwrap();
        let lib_prod = crate::proxy::custom_headers::production_code_only(&lib);
        assert_eq!(
            lib_prod.matches("balance_gate::spawn_background(").count(),
            1,
            "lib.rs 必须起一趟独立线程跑后台刷新（且只该有一处）"
        );
        let health = std::fs::read_to_string("src/health.rs").unwrap();
        let health_prod = crate::proxy::custom_headers::production_code_only(&health);
        assert!(
            !health_prod.contains("refresh_due("),
            "🔴 又挂回健康探测那趟车了 —— 关掉定时探测会让余额刷新一起停，\
             且周期会被探测间隔牵着走。理由全文见 balance_gate::run_forever"
        );
    }

    /// 🔴 后台稳态**一条事件都不许落**；用户主动查时每次都要落。
    ///
    /// 事件环只有 500 条且与路由/故障转移共用，折叠救不了这里（只合并**紧邻**的上一条，
    /// 而多条 Key 在同一轮里彼此穿插）。6 条 Key × 用户设的 1 分钟 = 360 条/小时，
    /// 一个多小时就把整环冲干净。此前受「用户得开着那个分类页」天然限速，
    /// 改成后台刷新之后才成为可能。
    #[test]
    fn a_quiet_background_refresh_must_not_touch_the_event_ring() {
        let s = store("gate_quiet");
        let cfg = BalanceQuery { enabled: true, ..Default::default() };
        // 缓存里已经是「余额 10」，这一轮又查到「余额 9」—— 结论没变（都是 Ok）。
        let key = key_with("k1", 0, Some(queried(Some(10.0))));
        record_outcome(&s, &key, &cfg, &queried(Some(9.0)), 12, Trigger::Background);
        assert!(
            s.list_all_events().is_empty(),
            "后台稳态刷新不许占事件环：{:?}",
            s.list_all_events().iter().map(|e| e.detail.clone()).collect::<Vec<_>>()
        );

        // 结论变了（Ok → Exhausted）→ 必须落（例行那条 + 耗尽告警那条）。
        record_outcome(&s, &key, &cfg, &queried(Some(0.0)), 12, Trigger::Background);
        let ev = s.list_all_events();
        assert!(ev.len() >= 2, "结论变化必须留痕：{ev:?}");
        assert!(ev.iter().any(|e| e.detail.contains("余额已耗尽")), "{ev:?}");

        // 用户主动查 → 即使结论没变也要落。
        let s2 = store("gate_user");
        record_outcome(&s2, &key, &cfg, &queried(Some(9.0)), 12, Trigger::User);
        assert_eq!(s2.list_all_events().len(), 1, "用户正看着，静默才是错的");
    }

    /// 🔴 接线判据四：只能有**一份**余额查询实现，且那个 IPC 命令必须真的注册了。
    ///
    /// 命令函数就住在本模块（同 `key_flags` / `codex_catalog`）。策略门
    /// `invoke-command-must-exist` 只查**正向**（前端调的名字在 Rust 有 `#[tauri::command]`），
    /// 反向（写了命令却没进 `generate_handler!`）**只在用户点到那个按钮时炸**，故在这里钉住。
    #[test]
    fn the_single_implementation_must_be_the_one_that_is_registered() {
        let src = std::fs::read_to_string("src/lib.rs").unwrap();
        let prod = crate::proxy::custom_headers::production_code_only(&src);
        assert!(
            prod.contains("health::balance_gate::query_key_balance,"),
            "命令没进 generate_handler! —— 前端一点就 command not found"
        );
        assert!(
            !prod.contains("balance::query_balance("),
            "lib.rs 里又出现了第二份余额查询实现 —— 去重集合与三条踩坑步骤都会跟着裂开"
        );
    }
}









