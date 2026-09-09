//! 定时检查更新：每 30 分钟一轮，**只在结论变化时**敲一下前端。
//!
//! # 它补的缺口
//!
//! 在此之前更新检查只有两个触发点：`App.tsx` 挂载时静默查一次、设置页一个手动按钮。
//! 也就是说**进程活多久，都只查过启动那一次** —— 而本应用的常态姿势恰恰是「起了代理就丢
//! 后台」（关窗=隐藏到托盘，见 `lib.rs` 的 `CloseRequested`）。托盘常驻好几天的用户，
//! 中间发了新版也永远看不到横幅，除非他自己想起来去设置页点一下。
//!
//! # 🔴 为什么在后端，不是前端一个 `setInterval`
//!
//! 前端那条路上有一个**已知会咬人的**性质：[`usePolling`](../../src/lib/usePolling.ts)
//! 刻意在窗口不可见时**停表**（不是跳过一次，是彻底不再触发），理由写在它的文档里 ——
//! 省掉托盘常驻时的白烧。而这里要的恰好是「窗口不可见时也继续查」，两者直接冲突：
//! 用它 = 功能在唯一需要它的场景下不工作；不用它而裸写 `setInterval` = 绕开一条全仓
//! 统一的纪律，且要赌 WebView2 在隐藏/被遮挡时的后台计时器行为（本机没有取证）。
//!
//! 后端没有这个问题，而且这是本仓**既有范式**：健康探测、用量落盘、`balance_gate`、
//! `codex_watch` 四趟定时线程全在后端，前端一条都没有。`balance_gate` 更是从前端驱动
//! **搬回**后端的，搬的理由与这里一字不差（详见其模块文档：托盘常驻用户在需要那份数据时
//! 认知通常是空的）。
//!
//! # 🔴 必须独立一趟，不许搭健康探测的车
//!
//! `balance_gate` 上踩过完整一遍：那趟车在 `health_check_interval_secs == 0`（用户关掉
//! 定时探测）时整轮 `continue`，于是一个**无关设置**能让本功能静默停摆；而且它的周期被
//! 探测间隔牵着走（用户设 3600s 就一小时才查一次），整轮还套 `timeout(period)`，
//! 吃掉预算后打出的是「**健康探测**一轮未完成」——一条指错方向的告警。
//!
//! # 载荷里不带版本号，只敲门
//!
//! 与 `events.rs` 全模块同一条纪律：事件只当「敲门」，数据一律走已有的类型化命令。
//! 前端收到敲门后自己调 `check_for_updates` 拿版本号与发布说明，于是「版本号、发布说明、
//! 友好化错误」只有一条序列化路径。代价是那一次**多打一趟 GitHub** ——
//! 而它只在真发现新版本时才发生（见下面「只在结论变化时敲」），不是每 30 分钟一次。
//!
//! # 只在结论变化时敲
//!
//! 无条件敲的两个代价都真实：① 前端每收到一次就重打一趟 GitHub，等于把 30 分钟一次变成
//! 两次；② `MAX_EVENTS` 与推送队列都与路由/故障转移共用，而本模块是**周期性**的 ——
//! 同 B4 那次「事件环洪水」的教训（那次也是把一个前台动作搬到后台才让它成为可能）。
//!
//! # 失败一律静默，**而且是真的一个字都不写**
//!
//! GitHub 在国内常被墙，`updater.check()` 失败是**常态**而不是异常。落事件/发通知会让
//! 这些用户每 30 分钟收到一条无行动价值的噪音，而他们该做的事（配代理、或去官网手动下）
//! 与这条消息无关。手动按钮那条路仍会把友好化后的错误显示给用户 —— 那是他主动问的。
//!
//! ⚠️ 失败分支里那句 `tracing::debug!` **在生产里到不了任何地方**，别把它当成一条记录：
//! 默认过滤级是 `info`（`lib.rs` 的 `EnvFilter` 兜底值），而即便打开 `RUST_LOG=debug`，
//! `tracing_subscriber::fmt()` 也只写 stdout —— 双击启动的 GUI 进程没有控制台
//! （同 `log_rotate.rs` 里记的那条）。它只在 `cargo run` 调试时有用。
//!
//! **成功那一侧刻意相反**：走 `append_event`（见 [`announce`]），因为「定时检查是否真的在跑」
//! 必须有可查的痕迹 —— 横幅是转瞬即逝的，窗口没开就压根没出现过。

use std::time::Duration;

/// 轮询周期。**固定值、不做成设置项**，判据同 `codex_watch::POLL` 与 `stream_idle` 那 180s：
/// 这个数字要么远小于「用户会注意到」的尺度（没人需要调快），要么调慢了就失去意义。
///
/// 做成设置项的代价不只是多一个字段：`AppSettings` 会被前端整份 `saveSettings` 覆盖
/// （`userPrefsParity` 那条判据就是为此建的），而这里失效方向是「定时检查静默停掉」。
const POLL: Duration = Duration::from_secs(30 * 60);

/// 一轮检查的结论。
///
/// 🔴 **`Unknown`（我们没查到）与 `UpToDate`（上游明确说已最新）必须分开。**
/// 合并成两态之后唯一会坏的地方是 [`Announced::on`]：那里只有 `UpToDate` 才清记忆，
/// 而把网络失败也当成「已最新」会让下一轮把**同一个版本重新敲一遍**（详见该函数的文档）。
/// 同 `balance_gate` 的三态（「查不到 ≠ 为零」）与 `model_pool::Confidence`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// 上游有更新，附版本号
    Available(String),
    /// 上游明确说已是最新
    UpToDate,
    /// 我们没查到（网络/端点/签名等）。**不是**「已最新」
    Unknown,
}

/// 「已经敲过哪个版本」的记忆。**进程级、不落盘**。
///
/// 不落盘是刻意的：横幅的「稍后」本来就只作用于本次启动（`UpdateBanner` 刻意不做持久化
/// 「永久忽略」，理由写在它的注释里 —— 避免用户一次误点后再也不知道有新版本）。
/// 记忆跟着进程走，与那个语义正好对齐。
#[derive(Debug, Default)]
pub(crate) struct Announced(Option<String>);

impl Announced {
    /// 吃进一轮结论，返回**是否该敲门**。
    ///
    /// 三条判据，每条对应一种噪音或一种漏报：
    /// - `Available(v)` 且 v 与上次敲的**不同** → 敲。同一个版本反复敲只会让前端反复重打
    ///   GitHub，而横幅那侧按版本号去重、压根不会因此再出现一次。
    /// - `UpToDate` → **清掉记忆**、不敲。清是为了「发布被撤回又重发同一个号」这种情形下
    ///   还能再敲一次；不敲是因为「没有新版本」这件事前端不需要知道（它的默认状态就是这个）。
    /// - `Unknown` → **既不敲也不清**。清了的话，一次网络抖动就会让下一轮把**同一个**版本
    ///   重新敲一遍 —— 于是在墙内用户那里（`check()` 时成时不成）退化成「每两轮白打
    ///   一趟 GitHub」，正是本模块的去重想消除的东西。
    pub(crate) fn on(&mut self, outcome: &Outcome) -> bool {
        match outcome {
            Outcome::Available(v) => {
                if self.0.as_deref() == Some(v.as_str()) {
                    return false;
                }
                self.0 = Some(v.clone());
                true
            }
            Outcome::UpToDate => {
                self.0 = None;
                false
            }
            Outcome::Unknown => false,
        }
    }
}

/// 起后台轮询任务。在 `setup` 里调一次。
///
/// 用 `tauri::async_runtime::spawn` 而不是 `std::thread` + 临时 Runtime：`updater.check()`
/// 是 async 的，而 `lib.rs` 顶部那几趟 `std::thread` 各自 `Runtime::new()` 是因为它们在
/// `tauri::Builder` **之前**就要起来。本模块要 `AppHandle`（`app.updater()` 从它来），
/// 只能在 setup 里起，那时 Tauri 托管运行时已经在了 —— 生命周期与应用一致。
///
/// ⚠️ 同 `lib.rs` 里 MCP 自启动那条记着的坑：**别**用 `std::thread` + `block_on`
/// 包一个只 spawn 就返回的调用，临时 Runtime 一 drop 就把任务连根带走。本函数里的
/// `async move` 自己就是那个长活的任务，不存在那个形状。
pub(crate) fn spawn_background(app: tauri::AppHandle, store: std::sync::Arc<crate::store::Store>) {
    tauri::async_runtime::spawn(async move {
        let mut announced = Announced::default();
        loop {
            // 🔴 **先睡后查**：`App.tsx` 挂载时已经静默查过一次，开局立刻再查一次就是
            // 同一件事做两遍（同 `codex_watch` 第一轮只记基线的取舍）。
            tokio::time::sleep(POLL).await;
            let outcome = check_once(&app).await;
            if announced.on(&outcome) {
                if let Outcome::Available(v) = &outcome {
                    announce(&store, v);
                }
                crate::events::emit(crate::events::Topic::Update, None);
            }
        }
    });
}

/// 落一条「发现新版本」事件。
///
/// 🔴 **必须是 `append_event` 而不是 `tracing::info!`。** 第一版写的是后者，而 tracing 在本仓
/// **到不了任何用户看得到的地方**：`fmt()` 默认写 stdout，而双击启动的 GUI 进程没有控制台
/// （`log_rotate.rs` 里记着同一件事），且它**完全不进** `logs/*.jsonl` —— 那些文件是
/// `append_event_full` 写的，两条路径互不相通。
///
/// 于是「定时检查是否真的在跑」这个问题当时没有任何可查的痕迹，而横幅是转瞬即逝的
/// （关掉就没了、窗口没开就压根没出现过）。**一个只在界面上闪一下的功能等于不可验证。**
///
/// 落 `system` 组（同启动自检）而不是 `warning`：这不是要用户处理的异常。
/// 分类用 `ClaudeCli` 是因为**事件必须带一个分类**而本事件与分类无关 —— 同启动自检的取舍。
///
/// **折叠键带版本号**：同一版本反复出现只占一行带 ×N（`Announced` 已经去重了，
/// 但重启进程会让记忆归零、于是同一版本可能再落一次）；换了版本号则是新的一行。
fn announce(store: &crate::store::Store, version: &str) {
    store.append_event_collapsible(
        crate::model::CategoryType::ClaudeCli,
        "system",
        None,
        &format!("定时检查发现新版本 v{version}（已通知界面显示更新横幅）"),
        None,
        Some(format!("update-available:{version}")),
    );
}

/// 查一轮。**只返回结论，不做任何副作用** —— 副作用（敲门/日志）全在调用点，
/// 这样「查」与「要不要说」各自可读。
async fn check_once(app: &tauri::AppHandle) -> Outcome {
    use tauri_plugin_updater::UpdaterExt;
    let Ok(updater) = app.updater() else {
        // 拿不到 updater = 配置/公钥层面的问题，重试也不会变好，但也不该 return
        // 让整趟循环退出：用户可能在设置页看到友好化错误后去修，而修完不必重启应用。
        return Outcome::Unknown;
    };
    match updater.check().await {
        Ok(Some(u)) => Outcome::Available(u.version.clone()),
        Ok(None) => Outcome::UpToDate,
        Err(e) => {
            // ⚠️ 这句在生产里到不了任何地方（默认过滤级 info + fmt 只写 stdout + GUI 无控制台），
            // 只在 `cargo run` 调试时有用。**这是刻意的**，理由见模块头「失败一律静默」。
            tracing::debug!("定时检查更新失败（不影响手动检查）: {e}");
            Outcome::Unknown
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 周期必须是 30 分钟 —— 用户明确定的数字（2026-09-09 从 10 分钟改过来），
    /// 不许被顺手改掉。
    ///
    /// 这条判据的价值不在「30 比 29 好」，而在于它**逼改动者过一遍**：这个数字乘以用户数
    /// 就是我们给 GitHub 的请求量（每用户每天 48 次；10 分钟那版是 144 次），
    /// 调小要想清楚这件事。
    #[test]
    fn the_poll_period_is_thirty_minutes() {
        assert_eq!(POLL, Duration::from_secs(1800), "定时检查更新的周期必须是 30 分钟");
    }

    /// 同一个版本只敲一次；换了版本号要再敲。
    #[test]
    fn the_same_version_is_announced_only_once() {
        let mut a = Announced::default();
        assert!(a.on(&Outcome::Available("0.1.61".into())), "第一次发现必须敲");
        assert!(!a.on(&Outcome::Available("0.1.61".into())), "同一个版本不许反复敲");
        assert!(!a.on(&Outcome::Available("0.1.61".into())));
        assert!(a.on(&Outcome::Available("0.1.62".into())), "换了版本号必须再敲");
    }

    /// 「已是最新」不敲门，但要**清掉记忆**。
    ///
    /// 清记忆是为了「发布被撤回、随后又用同一个号重发」这种情形 —— 不清的话那一版
    /// 永远敲不出来。而「没有新版本」本身不需要通知前端：那正是它的默认状态。
    #[test]
    fn up_to_date_is_silent_but_clears_the_memory() {
        let mut a = Announced::default();
        assert!(a.on(&Outcome::Available("0.1.61".into())));
        assert!(!a.on(&Outcome::UpToDate), "「已最新」不该敲门");
        assert!(
            a.on(&Outcome::Available("0.1.61".into())),
            "记忆没清 —— 撤回后重发同一个版本号就再也敲不出来了"
        );
    }

    /// 🔴 **`Unknown` 既不敲也不清记忆。**
    ///
    /// 这是 `Unknown` 与 `UpToDate` 必须分成两个变体的**唯一**理由，也是本模块最容易被
    /// 「简化」掉的一处：把 `Unknown` 也当成清记忆（或干脆合并成 `Option`），那么在
    /// `check()` 时成时不成的网络下（墙内用户的常态），序列
    /// `Available(v) → Unknown → Available(v)` 会敲**两次**同一个版本，
    /// 于是前端白打一趟 GitHub。轮次一多就退化成「每两轮白打一趟」。
    #[test]
    fn a_failed_check_neither_announces_nor_forgets() {
        let mut a = Announced::default();
        assert!(a.on(&Outcome::Available("0.1.61".into())));
        assert!(!a.on(&Outcome::Unknown), "查不到不是「有新版本」，不许敲");
        assert!(
            !a.on(&Outcome::Available("0.1.61".into())),
            "网络抖动不该让同一个版本被重新敲一遍（这就是 Unknown 不能并进 UpToDate 的理由）"
        );

        // 反向：从未敲过任何版本时，Unknown 同样不许敲（否则启动后第一轮网络失败
        // 就会让前端白打一趟）。
        let mut b = Announced::default();
        assert!(!b.on(&Outcome::Unknown));
    }

    /// 🔴 **接线判据：必须独立一趟，且真的在 `setup` 里起来。**
    ///
    /// 上面那 4 条全是直调纯函数 —— 把 `lib.rs` 那行 `spawn_background` 删掉，
    /// 它们照样全绿，而那正是「定时检查整个不工作」这个缺陷本身。本仓已为同一类接线盲区
    /// 付过 20+ 次代价（`route_meta` 的出口、`lan_guard` 的 peer、`log_rotate` 的写线程……）。
    ///
    /// 三个方向都钉：起了、没搭健康探测的车、本模块不读那个无关设置。
    #[test]
    fn the_watcher_must_run_on_its_own_task() {
        let prod = |s: &str| crate::proxy::custom_headers::production_code_only(s);
        let lib = prod(include_str!("lib.rs"));
        assert!(
            lib.contains("update_watch::spawn_background("),
            "lib.rs 里没起这趟任务 —— 定时检查整个不工作，且完全静默"
        );
        let health = prod(include_str!("health.rs"));
        assert!(
            !health.contains("update_watch::"),
            "不许搭健康探测那趟车（理由见本模块文档与 balance_gate::run_forever）"
        );
        let me = prod(include_str!("update_watch.rs"));
        assert!(
            !me.contains("health_check_interval_secs"),
            "本模块的周期必须固定，不许被探测设置牵着走"
        );
    }

    /// 🔴 **发现新版本必须留下可查的痕迹，不能只有一个转瞬即逝的横幅。**
    ///
    /// ⚠️ 这条是**代码审查补上来的**：第一版用 `tracing::info!`，而 tracing 在本仓到不了任何
    /// 用户看得到的地方（默认过滤级 `info` 之下 debug 全丢；`fmt()` 只写 stdout 而双击启动的
    /// GUI 没有控制台；且它**完全不进** `logs/*.jsonl` —— 那些是 `append_event_full` 写的）。
    /// 于是「定时检查到底在跑吗」当时没有任何可查的答案，而我给 C-37 写的验证判据
    /// （「日志里应有那一行」）**是假话** —— 照它去 tail 永远找不到，找不到的人会判定功能坏了。
    /// 那正是本仓「指错方向的提示比没有提示更糟」。
    ///
    /// 三个维度都钉：走 `append_event`、落 `system` 组（不是 `warning` —— 这不是要用户处理的
    /// 异常）、折叠键带版本号（重启后记忆归零，同一版本可能再落一次，不带版本号会让
    /// 两个不同版本折进同一行）。
    #[test]
    fn finding_a_new_version_leaves_a_trace_in_the_event_log() {
        let me = crate::proxy::custom_headers::production_code_only(include_str!("update_watch.rs"));
        assert!(
            me.contains("store.append_event_collapsible("),
            "必须落事件 —— tracing 在本仓到不了用户眼前（理由见本测试文档）"
        );
        assert!(
            me.contains("\"system\""),
            "落 system 组（同启动自检），不是 warning —— 这不是要用户处理的异常"
        );
        assert!(
            me.contains("format!(\"update-available:{version}\")"),
            "折叠键必须带版本号，否则两个不同版本会折进同一行"
        );
        // 反向：失败那侧**不许**落事件（墙内用户每 30 分钟一条噪音）。
        //
        // ⚠️ 数的是**调用形态** `append_event…(` 而不是裸字面量 `append_event`：后者会被
        // 一个恰好提到它的**字符串**满足（注释由 `production_code_only` 剥掉了，字符串不会）。
        // 注入实测撞到过 —— 那是本仓「判据被自己扫的文本满足」那一族的字符串版。
        assert_eq!(
            me.matches("append_event_collapsible(").count(),
            1,
            "只许有一处落事件 —— 失败路径落事件会让墙内用户每轮收到一条无行动价值的噪音"
        );
    }

    /// 🔴 **跨语言接线：前端必须订阅 `update` 主题。**
    ///
    /// 后端敲了门而前端没人听，表现与「压根没做」完全一样 —— 而且是静默的：
    /// 后端日志里有「已通知界面」，界面上什么都不会发生。编译器管不到这条缝。
    ///
    /// 判据落在 hub（`useBackendEvents`）而不是某个页面：横幅在**所有页面**都要显示，
    /// 挂在某一页上就变成「只有开着那一页才会更新」。
    /// ⚠️ **本判据第一版是假的，注入实测才发现** —— 它写的是裸 `hub.contains("update")`，
    /// 而那个字面量在 `BackendTopic` 的**类型联合**里、以及注释里同样出现。于是把订阅整个
    /// 改成 `useBackendEvent(["logs"], …)`（= 缺陷本体）**判据照样绿**。
    ///
    /// 这是本仓第 6 次栽在同一处（前五次记在 `production_code_only` 的文档里）。故：
    /// ① 先剥注释；② 钉**调用形态**而不是裸字面量 —— 类型声明是多行的，匹配不到它。
    #[test]
    fn the_frontend_must_subscribe_to_the_update_topic() {
        let hub = crate::proxy::custom_headers::production_code_only(include_str!(
            "../../src/lib/useBackendEvents.ts"
        ));
        assert!(
            hub.contains(r#"useBackendEvent(["update"]"#),
            "前端 hub 没订阅 update 主题 —— 后端敲了门没人听，与没做这个功能等价"
        );
        assert!(
            hub.contains("checkForUpdates"),
            "订阅了却不去取数据 —— 事件只是敲门，版本号要走类型化命令拿（见 events.rs）"
        );
        // Topic 的字符串必须与前端那个字面量一致（两边各写一份的漂移是静默的）。
        assert_eq!(crate::events::Topic::Update.as_str(), "update");
    }
}
