//! 后端状态变更推送（UX#5）。
//!
//! ## 为什么要有这个模块
//!
//! 本应用此前的实时性 **100% 靠前端 7 处 `setInterval` 撑**，状态最长滞后 5s，
//! 且应用开着就恒定烧 IPC（空闲时也一样）。改为「后端在状态真的变了的时候推一下、
//! 前端收到即刷」，轮询退成 30s 兜底。收益是双向的：交互变即时，空闲 IPC 降一个数量级。
//!
//! ## 为什么用 OnceLock 而不是把 AppHandle 传下去
//!
//! `Store` / `ProxyManager` / 后台健康探测线程**全都在 `tauri::Builder` 之前就构造好了**
//! （见 `lib.rs` 的 `run()`），那时进程里还不存在 AppHandle；而需要 emit 的位置
//! （每请求收尾的 `record_live_*`、代理的 tokio 任务、探测循环）没有一个在命令上下文里。
//!
//! 两条被否决的路：
//! - 给 Store/ProxyManager 加 `AppHandle` 字段 —— 会把 `tauri` 类型污染进 `store.rs`，
//!   而那里有数百条直接 `Store::new_at(tmpdir)` 的单测，全部要跟着改。
//! - 一路当参数传 —— `record_live_failure` 在转发最内层，要穿透 proxy→health→store
//!   三层十几个签名。
//!
//! 故用进程级 `OnceLock`。未初始化时 `emit` 是**空操作** —— `--mcp-stdio` 转发模式
//! 与 `cargo test --lib` 正好走这条路，不需要任何特判。
//!
//! ## 为什么中间要垫一个泵线程，而不是直接 app.emit
//!
//! 1. **结构性防死锁**。调用点遍布持锁代码附近（store 的 config 写锁、proxy 的 running
//!    互斥锁）。parking_lot 非重入且写者优先，一旦有人在持写锁时 emit，而主线程此刻正在
//!    某个 IPC 命令里等同一把锁，就是经典的锁序反转。垫一层「try_send 到有界队列」后，
//!    emit 在调用方**恒定非阻塞、且完全不进 Tauri**，该风险从「靠纪律维持」变成
//!    「结构上不可能」。
//! 2. **去抖**。一轮健康探测可能翻转多条 Key，一次导入可能连写十几次配置。
//!    泵按 topic 合并，一个窗口内的同类信号只发一次。
//! 3. **不拖热路径**。转发收尾的 emit 只是一次 channel push。
//!
//! ## 载荷里为什么不带业务数据
//!
//! 事件只当「敲门」，数据一律走已有的类型化命令。带数据等于给同一份数据开第二条序列化
//! 路径，`health` 事件里的 HealthState 与 `list_keys` 返回的 `ProviderKey.health` 迟早分叉
//! ——本仓已被这类「两处各写一份」坑过多次（见 `set_primary_key` 收归后端的注释）。
//! 而且带数据就要在 emit 点从锁里把数据捞出来克隆，正是这里要避免的事。

use crate::model::CategoryType;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::OnceLock;

/// 前端 `listen` 的事件名。
///
/// Tauri 2 的事件名合法字符集是 字母数字 / `-` / `/` / `:` / `_`，这个名字合规
/// （有测试锁住 —— 名字非法时 Tauri 会在运行时静默拒发，不会编译报错）。
pub const EVENT_NAME: &str = "synaroute://state-changed";

/// 队列容量。满了就丢，**丢弃是安全的**：前端还有 30s 兜底轮询兜底。
/// 这一点必须写清楚，否则后人不敢丢、会把 try_send 改回 send 而把热路径挂住。
const QUEUE_CAP: usize = 256;

/// 批量收集窗口：第一条信号到达后，再等这么久把同一批合并掉。
const COALESCE_MS: u64 = 250;

/// 状态变更的主题。
///
/// `as_str` 与 `min_interval_ms` 都用**穷举 match**：加第 8 个 topic 时编译器会同时点出
/// 这两处要改（与本仓 `Protocol::auth_scheme` 同一手法），而不是漏一处然后静默发错。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Topic {
    /// Key 增删改、优先级重排、导入 —— 凡走 `mutate_and_persist` 的配置落盘
    Config,
    /// 后端自己改了 settings（托盘快切模型/强度等）。**与 Config 分开**，理由见 hub 注释
    Settings,
    /// 健康**可见态**翻转（up/down、熔断武装/解除），不含 fail_count/latency 这类不上屏的
    Health,
    /// 代理启停
    Proxy,
    /// 密钥库锁定/解锁
    Vault,
    /// 有新事件写入运行日志
    Logs,
}

// 刻意**没有** Takeover 主题：桌面端「接入被 cc-switch 接管」的判据是外部文件
// （`_meta.json`）被别人改写，后端没有、也不该为此加一套文件监视 —— 那是另一套机制，
// 而接管是低频事件，30s 兜底轮询完全够用。留一个永不构造的枚举变体反而会让后人
// 以为「推送已覆盖接管」，那正是本仓最防的「看起来做了、实际没有」。

impl Topic {
    pub fn as_str(&self) -> &'static str {
        match self {
            Topic::Config => "config",
            Topic::Settings => "settings",
            Topic::Health => "health",
            Topic::Proxy => "proxy",
            Topic::Vault => "vault",
            Topic::Logs => "logs",
        }
    }

    /// 同一主题两次推送之间的最小间隔（毫秒）。0 = 不限。
    ///
    /// 只有 `Logs` 需要压制：`append_event_full` 每次转发要走好几次，250ms 合并后最坏仍是
    /// 4 次/秒，而前端对 logs 的反应是**重拉 500 条事件** —— 那比现在 2s 一次还糟。
    /// 1500ms 让最坏情况退化到「比现状略好」，而空闲时是 0（立刻推）。
    fn min_interval_ms(&self) -> i64 {
        match self {
            Topic::Logs => 1500,
            Topic::Config | Topic::Settings | Topic::Health | Topic::Proxy | Topic::Vault => 0,
        }
    }
}

/// 一条待发信号：主题 + 可选分类。
pub type Sig = (Topic, Option<CategoryType>);

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct StateChanged {
    topic: &'static str,
    /// 哪个分类变了。None = 与分类无关（如密钥库锁态），前端一律刷新
    category_id: Option<CategoryType>,
}

static APP: OnceLock<tauri::AppHandle> = OnceLock::new();
static TX: OnceLock<SyncSender<Sig>> = OnceLock::new();
static DROPPED: AtomicU64 = AtomicU64::new(0);

/// 安装 AppHandle 并启动泵线程。在 `setup` 里调一次。
pub fn init(app: &tauri::AppHandle) {
    if APP.set(app.clone()).is_err() {
        return; // 已初始化（重复调用无害）
    }
    let (tx, rx) = std::sync::mpsc::sync_channel::<Sig>(QUEUE_CAP);
    if TX.set(tx).is_err() {
        return;
    }
    std::thread::Builder::new()
        .name("synaroute-event-pump".into())
        .spawn(move || pump(rx))
        .ok();
}

/// 推一条状态变更。
///
/// **调用纪律：emit 之前，本函数持有的所有 parking_lot guard 必须已经释放。**
/// 泵的设计让持锁 emit 也不至于死锁，但这条纪律要留住 —— 它防的是将来某人「简化」掉泵、
/// 把 emit 换成直接 `app.emit` 时悄悄埋雷。
///
/// 未初始化（测试、`--mcp-stdio`）时是空操作。队列满时丢弃并计数，前端 30s 兜底轮询兜住。
pub fn emit(topic: Topic, category: Option<CategoryType>) {
    let Some(tx) = TX.get() else { return };
    if tx.try_send((topic, category)).is_err() {
        let n = DROPPED.fetch_add(1, Ordering::Relaxed) + 1;
        // 只在量级点告警，避免高频丢弃时告警本身刷爆日志（照抄日志队列的节流写法）。
        if n == 1 || n % 100 == 0 {
            tracing::warn!("状态推送队列已满，累计丢弃 {n} 条（前端 30s 兜底轮询仍会追上）");
        }
    }
}

/// 已丢弃的推送数（可观测性）。
pub fn dropped_count() -> u64 {
    DROPPED.load(Ordering::Relaxed)
}

/// 决定本轮发哪些、哪些要压制到以后。**纯函数**，本模块唯一有分支逻辑、也是唯一可脱离
/// Tauri 单测的地方。
///
/// 返回 `(该发的, 若有被压制的则最早可再发的时刻)`。
///
/// 被压制的信号**绝不丢弃**，而是让泵下一轮用 `recv_timeout(那个时刻 - now)` 等着补发。
/// 否则一阵密集日志之后的最后一条会被吞掉，用户要等 30s 兜底才看得到 —— 那正是
/// 「看起来是实时的，偶尔却莫名卡住」这类最难查的体验问题。
fn plan_emission(
    now_ms: i64,
    last_sent: &HashMap<Sig, i64>,
    pending: &HashSet<Sig>,
) -> (Vec<Sig>, Option<i64>) {
    let mut send = Vec::new();
    let mut next_wake: Option<i64> = None;
    for sig in pending {
        let min = sig.0.min_interval_ms();
        let ready_at = last_sent.get(sig).map_or(0, |t| t + min);
        if min == 0 || now_ms >= ready_at {
            send.push(*sig);
        } else {
            next_wake = Some(next_wake.map_or(ready_at, |w: i64| w.min(ready_at)));
        }
    }
    (send, next_wake)
}

fn pump(rx: std::sync::mpsc::Receiver<Sig>) {
    use std::sync::mpsc::RecvTimeoutError;
    let mut last_sent: HashMap<Sig, i64> = HashMap::new();
    let mut pending: HashSet<Sig> = HashSet::new();

    loop {
        // 空闲时线程完全休眠在这里，不做定时唤醒。
        match rx.recv() {
            Ok(sig) => {
                pending.insert(sig);
            }
            Err(_) => return, // 发送端全部析构 = 进程在收尾
        }

        // 收集窗口：把同一阵变更合并掉（一轮探测翻转多条 Key、一次导入连写多次配置）。
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(COALESCE_MS);
        while let Some(left) = deadline.checked_duration_since(std::time::Instant::now()) {
            match rx.recv_timeout(left) {
                Ok(sig) => {
                    pending.insert(sig);
                }
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }

        // 发送。此时**不持有任何 Store 锁**，载荷里也没有任何 Store 数据。
        loop {
            let now = chrono::Utc::now().timestamp_millis();
            let (send, next_wake) = plan_emission(now, &last_sent, &pending);
            for sig in &send {
                pending.remove(sig);
                last_sent.insert(*sig, now);
                if let Some(app) = APP.get() {
                    use tauri::Emitter;
                    let _ = app.emit(
                        EVENT_NAME,
                        StateChanged { topic: sig.0.as_str(), category_id: sig.1 },
                    );
                }
            }
            let Some(wake) = next_wake else { break };
            // 还有被压制的：睡到它可以发的时刻再补发（期间新到的信号会在下一轮 recv 里合并）。
            let sleep_ms = (wake - chrono::Utc::now().timestamp_millis()).max(0) as u64;
            std::thread::sleep(std::time::Duration::from_millis(sleep_ms.min(5_000)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 事件名必须落在 Tauri 2 允许的字符集内（字母数字 / `-` / `/` / `:` / `_`）。
    ///
    /// 为什么要测：名字非法时 Tauri 是**运行时静默拒发**，不会编译报错 ——
    /// 表现为「后端明明 emit 了、前端一条都收不到」，而且没有任何报错可查。
    #[test]
    fn event_name_uses_only_tauri_allowed_chars() {
        assert!(
            EVENT_NAME
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '/' | ':' | '_')),
            "事件名 {EVENT_NAME} 含 Tauri 不接受的字符，会被静默拒发"
        );
    }

    /// 不限速的主题应当立即放行，不受上次发送时间影响。
    #[test]
    fn unthrottled_topics_send_immediately() {
        let mut last = HashMap::new();
        last.insert((Topic::Health, None), 1_000_i64);
        let pending: HashSet<Sig> = [(Topic::Health, None)].into_iter().collect();
        let (send, wake) = plan_emission(1_001, &last, &pending);
        assert_eq!(send, vec![(Topic::Health, None)], "健康态翻转必须立刻到，不允许延迟");
        assert!(wake.is_none());
    }

    /// 限速主题在窗口内被压制，但**必须安排补发**而不是丢弃。
    ///
    /// 判据：丢弃会让一阵密集日志之后的最后一条被吞掉，用户要等 30s 兜底才看到 ——
    /// 「看起来实时、偶尔莫名卡住」是最难查的一类体验问题。
    #[test]
    fn throttled_topic_is_deferred_not_dropped() {
        let mut last = HashMap::new();
        last.insert((Topic::Logs, None), 1_000_i64);
        let pending: HashSet<Sig> = [(Topic::Logs, None)].into_iter().collect();

        let (send, wake) = plan_emission(1_100, &last, &pending);
        assert!(send.is_empty(), "1500ms 窗口内不该发");
        assert_eq!(wake, Some(2_500), "必须安排在窗口结束时补发，而不是丢掉");

        // 到点后应放行
        let (send, wake) = plan_emission(2_500, &last, &pending);
        assert_eq!(send, vec![(Topic::Logs, None)]);
        assert!(wake.is_none());
    }

    /// 不同分类的同一主题互不干扰（否则 A 分类的日志会压住 B 分类的）。
    #[test]
    fn throttling_is_per_category() {
        let mut last = HashMap::new();
        last.insert((Topic::Logs, Some(CategoryType::ClaudeCli)), 1_000_i64);
        let pending: HashSet<Sig> = [(Topic::Logs, Some(CategoryType::Codex))].into_iter().collect();
        let (send, _) = plan_emission(1_100, &last, &pending);
        assert_eq!(
            send,
            vec![(Topic::Logs, Some(CategoryType::Codex))],
            "另一个分类不该被压制"
        );
    }

    /// 未初始化时 emit 必须是空操作（测试进程与 --mcp-stdio 转发模式走这条路）。
    #[test]
    fn emit_without_init_is_noop() {
        let before = dropped_count();
        emit(Topic::Config, None);
        emit(Topic::Health, Some(CategoryType::ClaudeCli));
        assert_eq!(dropped_count(), before, "未初始化不该计入丢弃，它根本没入队");
    }
}
