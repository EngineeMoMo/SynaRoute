//! Codex `config.toml` 的**外部改动监听**：轮询 mtime，变了就重跑一次漂移检测。
//!
//! # 为什么需要它
//!
//! [`super::drift_state`] 是**被动**的 —— 只在启动、接入、以及用户打开「配置预览」时查。
//! 而改写 `config.toml` 的另外三方都不通知我们：Codex 桌面端自己会重写它（把用户在
//! `/model` 里的选择写回去，也可能连带覆盖我们的 provider 块）、cc-switch 切档、
//! 用户手改。于是「接入已失效」这件事要等到下次打开应用/切到那一页才被发现，
//! 而这段窗口里每一个请求都是 401 —— 那正是用户 2026-09-03 报的形态。
//!
//! # 🔴 不引入 `notify` crate
//!
//! 判据同 `socket2` 那条的取舍标准：`Cargo.lock` 里当前**零** `notify`，而它会带进一串
//! 平台后端（inotify / FSEvents / ReadDirectoryChangesW）。这里要的信息只有「那个文件变了吗」，
//! 一次 `metadata()` 就够，60s 一次的成本在噪音级别以下。
//!
//! 顺带一条实质理由：文件系统事件在这个场景里**反而更差** —— 编辑器与 Codex 自己写盘常是
//! 「写临时文件 + rename」，那会产生一串事件（有时还带一个「文件短暂不存在」的中间态），
//! 要自己做去抖。轮询 mtime 天然只看终态。
//!
//! # 🔴 必须独立起线程，不许搭 `check_all_categories` 的车
//!
//! `balance_gate` 上刚踩过完整的一遍（见该模块 `run_forever` 的三条理由）：那趟车在
//! `health_check_interval_secs == 0` 时整轮 `continue`，于是一个**无关设置**能让本功能
//! 静默停摆；而且它的周期被探测间隔牵着走。`lib.rs` 里 `flush_usage_if_dirty` 与
//! `balance_gate::spawn_background` 都是独立线程，照它们。
//!
//! # 只在「变了 **且** 有告警」时落事件
//!
//! mtime 变化本身不是问题（Codex 每次会话都可能回写 `model`）。落事件的条件是
//! **漂移检测这一轮真的有告警** —— 否则托盘常驻用户一天能攒出几十条无信息量的行，
//! 而 `MAX_EVENTS` 只有 500 且与路由/故障转移共用（同 B4 那次「事件环洪水」的教训）。
//!
//! 事件**可折叠**且折叠键固定：反复被同一个程序改写只占一行带 ×N。

use crate::store::Store;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

/// 轮询周期。**固定值、不做成设置项**：这个数字要么远小于「用户会注意到」的尺度
/// （没人需要调快），要么调慢了就失去意义。同 `stream_idle` 那 180s 的取舍。
const POLL: Duration = Duration::from_secs(60);

/// 事件折叠键。固定字符串 —— 反复漂移只占一行带 ×N。
const COLLAPSE_KEY: &str = "codex-config-drift";

/// 起后台监听线程。
pub(crate) fn spawn_background(store: Arc<Store>) {
    std::thread::spawn(move || run_forever(&store));
}

/// 上一轮看到的 (mtime, 大小)。`None` = 还没看过，或文件不存在。
type Stamp = Option<(SystemTime, u64)>;

fn stamp_of(path: &PathBuf) -> Stamp {
    let m = std::fs::metadata(path).ok()?;
    Some((m.modified().ok()?, m.len()))
}

fn run_forever(store: &Arc<Store>) {
    // 路径解析一次就够：`CODEX_HOME` 在进程生命周期内不变（它变了要重启我们，
    // 而那种不一致由 `env_conflicts` 的目录判据单独报）。
    let Ok(cfg) = super::config_path() else { return };
    let Ok(auth) = super::auth_path() else { return };
    // 🔴 **第一轮只记基线、不报告**：启动时的漂移已经由启动自检那条路查过了，
    // 在这里再报一次就是同一件事说两遍。本模块只管「**从现在起**又被改了」。
    let mut last = stamp_of(&cfg);
    loop {
        std::thread::sleep(POLL);
        let now = stamp_of(&cfg);
        if now == last {
            continue;
        }
        last = now;
        // 端点取「设置里记的那个」而不是运行中的端口：代理没在跑时后者拿不到，
        // 而那时漂移照样值得报（用户下次点启动才会发现）。
        let endpoint = expected_endpoint_from_settings(store);
        let state = super::drift_state(&cfg, &auth, &endpoint, super::believed_applied(&cfg));
        if let Some(w) = super::drift_warning(&state) {
            store.append_event_collapsible(
                crate::model::CategoryType::Codex,
                "warning",
                None,
                // ⚠️ **措辞是条件句，因为我们分辨不出是谁改的。** 我们手上只有「mtime 变了 +
                // 这一轮漂移检测有告警」两个事实，没有任何信息能指认改写者。
                // 绝大多数情况确实是外部程序（Codex 自己 / cc-switch / 手改），但也有一种是
                // **我们自己**：还原中途失败（config.toml 已交还、auth.json 的占位符没解除）
                // 会走到这里。断言「被其它程序改动」在那一次是假话，而它会让用户去查一个
                // 不存在的第三方 —— 同本仓「拿不到判断依据时把话写成条件句」那条。
                &format!("检测到 ~/.codex/config.toml 在本次运行期间被改动过（可能是 Codex 自己、cc-switch、或手动编辑）。{w}"),
                None,
                Some(COLLAPSE_KEY.to_string()),
            );
        }
    }
}

/// Codex 分类期望的入口。**刻意不取运行中的端口**，理由见调用点。
fn expected_endpoint_from_settings(store: &Arc<Store>) -> String {
    let cat = crate::model::CategoryType::Codex;
    let port = store
        .get_settings()
        .proxy_ports
        .get(&cat)
        .copied()
        .unwrap_or_else(|| cat.meta().default_port);
    format!("http://127.0.0.1:{port}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// mtime 或大小任一变化都要认出来。
    ///
    /// 🔴 **只比 mtime 不够**：同一秒内的两次写盘在某些文件系统上 mtime 完全相同
    /// （NTFS 的时间戳粒度是 100ns，但 Codex 与编辑器都可能在同一个 tick 内完成
    /// 「写临时文件 + rename」）。加上大小是零成本的第二个维度。
    ///
    /// 反过来「内容变了但 mtime 与大小都没变」这一漏检是接受的：那需要等长替换 + 刻意
    /// 回拨时间戳，不是真实场景（而 `codex_session_view` 的标题缓存刻意利用了同一个性质）。
    #[test]
    fn a_rewrite_is_noticed_by_either_mtime_or_size() {
        let dir = std::env::temp_dir().join(format!("sr-cxwatch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("config.toml");

        assert_eq!(stamp_of(&f), None, "文件不存在时必须是 None，不能 panic");

        std::fs::write(&f, b"model = \"a\"").unwrap();
        let first = stamp_of(&f);
        assert!(first.is_some());
        assert_eq!(stamp_of(&f), first, "没动过就必须完全相同（否则每轮都误报）");

        // 等长改写 + 把 mtime 拨回去 → 两个维度都不变，这是已知的漏检（写在文档里）。
        let t = std::fs::metadata(&f).unwrap().modified().unwrap();
        std::fs::write(&f, b"model = \"b\"").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&f)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(t))
            .unwrap();
        assert_eq!(stamp_of(&f), first, "这一漏检是刻意接受的，见本测试文档");

        // 长度变了 → 必须认出来（哪怕 mtime 被回拨）。
        std::fs::write(&f, b"model = \"much longer value\"").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&f)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(t))
            .unwrap();
        assert_ne!(stamp_of(&f), first, "大小变了必须认出来 —— 这是第二个维度存在的理由");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 🔴 **必须独立起线程，不许搭健康探测那趟车。**
    ///
    /// `balance_gate` 上踩过完整的一遍：那趟车在 `health_check_interval_secs == 0` 时整轮
    /// `continue`，于是一个**无关设置**能让本功能静默停摆，且周期被探测间隔牵着走。
    /// 判据两个方向都钉：`lib.rs` 必须有那一行，`health.rs` **不许**出现本模块的调用。
    #[test]
    fn the_watcher_must_run_on_its_own_thread() {
        let prod = |s: &str| crate::proxy::custom_headers::production_code_only(s);
        let lib = prod(include_str!("../lib.rs"));
        assert!(
            lib.contains("codex_watch::spawn_background("),
            "lib.rs 里没起这趟线程 —— 监听整个不工作，且完全静默"
        );
        let health = prod(include_str!("../health.rs"));
        assert!(
            !health.contains("codex_watch::"),
            "不许搭健康探测那趟车（理由见本测试文档与 balance_gate::run_forever）"
        );
        // 本模块自己也不许去读那个设置 —— 周期是固定的。
        let me = prod(include_str!("codex_watch.rs"));
        assert!(
            !me.contains("health_check_interval_secs"),
            "本模块的周期必须固定，不许被探测设置牵着走"
        );
    }

    /// 事件只在**真有告警**时落，且必须可折叠。
    ///
    /// mtime 变化本身不是问题（Codex 每次会话都可能回写 `model`）。无条件落事件的后果是
    /// 托盘常驻用户一天攒几十条无信息量的行，而 `MAX_EVENTS` 只有 500 且与路由共用 ——
    /// 同 B4 那次「事件环洪水」。折叠键必须是固定串，否则折叠不生效。
    #[test]
    fn the_event_is_collapsible_and_only_fires_when_there_is_a_warning() {
        let me = crate::proxy::custom_headers::production_code_only(include_str!("codex_watch.rs"));
        assert!(
            me.contains("if let Some(w) = super::drift_warning(&state)"),
            "落事件必须以「这一轮真的有告警」为条件"
        );
        assert!(
            me.contains("append_event_collapsible") && me.contains("Some(COLLAPSE_KEY.to_string())"),
            "必须是可折叠事件且带固定折叠键"
        );
        assert_eq!(
            me.matches("append_event").count(),
            1,
            "只许有一处落事件 —— 多一处就会绕过上面那个条件"
        );
    }
}
