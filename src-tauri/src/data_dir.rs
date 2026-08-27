//! 应用数据目录的解析，含 `SYNAROUTE_DATA_DIR` 覆盖。
//!
//! # 为什么需要一个覆盖开关
//!
//! `smoke:installer` 那道门要**真装真启动**产物。没有覆盖手段时，冒烟实例读的是用户
//! **真实**的 `%APPDATA%\SynaRoute\config.json` —— 2026-08-23 第一次真跑时它报
//! `配置=…\SynaRoute\config.json · keys=13`。两个后果：
//!
//! 1. 真实配置里 `proxyRunningCategories` 非空 → 冒烟实例开机自动启动代理，
//!    而「起=顺带写工具配置」，于是它会把 `~/.claude/settings.json` 改写成指向一个
//!    随后被 kill 的临时端口。**「起了没还原」这种状态不自愈。** 那次没造成损害纯属运气：
//!    脚本一看到自检行就收尾，早于代理自启动写完。
//! 2. 与用户自己那份实例抢同一个代理端口。
//!
//! 所以那道门有一条硬判据：自检行的 `keys=` 必须为 0，否则整个门红
//! （一个静默失去隔离的门比一个红的门糟得多）。在本模块之前，本机必然红。
//!
//! # 🔴 已证伪的做法，别再试
//!
//! 给子进程传 `env: { APPDATA: <临时目录> }` **无效**。`dirs::data_dir()` 在 Windows 上走
//! `SHGetKnownFolderPath(FOLDERID_RoamingAppData)` 这个已知文件夹 API，**不读 `APPDATA`
//! 环境变量**，改了照样 `keys=13`。覆盖只能由本进程自己认一个专用变量来实现 ——
//! 这也是 OmniRoute 的 `check-pack-boot.mjs` 用 `DATA_DIR` 做隔离的同一手法。
//!
//! 本变量**不解决** MSIX 虚拟化（那层重定向发生在文件系统调用上，比这里更底层，
//! 见 docs/11）。它只负责「让我们能指定一个别的目录」。
//!
//! # 目前只接了 `Store::init` 这一处
//!
//! 另有三处 `dirs::data_dir()` 未走本模块：`mcp.rs`（端口文件）、`tools.rs`
//! （接入态 `desktop_prev_applied.json`）。冒烟场景下它们**不会被写** ——
//! 隔离后 `keys=0` 且 `proxyRunningCategories` 为空，实例不会自启动代理、也就不接入工具。
//! 要把它们一并统一时，判据是：设了本变量后启动实例、跑完一次接入与还原，
//! 真实 `%APPDATA%\SynaRoute\` 下不新增任何文件。

use std::path::PathBuf;

/// 覆盖数据目录的环境变量名。
///
/// 有一条源码级判据 `env_var_name_is_frozen` 钉住这个字面量：
/// 脚本侧（`scripts/smoke-installer.mjs`）按同名变量传值，改名字而不同步改脚本
/// 会让隔离**静默失效** —— 而失效的表现恰恰是「门变绿了」，因为它读回用户真实配置的
/// 那条路径本来就是通的。编译器管不到这条缝。
pub(crate) const ENV_OVERRIDE: &str = "SYNAROUTE_DATA_DIR";

/// 应用数据目录（`config.json` / `secrets.enc` 所在）。
///
/// 返回 `AppResult` 而不是 `Option`：定不出目录时的那句错误消息属于本模块的职责，
/// 让每个调用方各写一遍 `ok_or_else` 只会让文案漂移。
pub(crate) fn app_data_dir() -> crate::error::AppResult<PathBuf> {
    app_data_dir_from(std::env::var(ENV_OVERRIDE).ok().as_deref())
        .ok_or_else(|| crate::error::AppError::Other("无法定位数据目录".into()))
}

/// [`app_data_dir`] 的纯函数形态，覆盖值由入参给出。
///
/// 🔴 **为什么要有这个形态**：Rust 测试默认并行，而 `std::env::set_var` 改的是**整个进程**
/// 的全局状态 —— 一条用例设了变量，同刻运行的另一条就会读到它。本仓有 800+ 条用例、
/// 其中不少会构造 `Store`，靠 `set_var` 测这段等于给整个套件埋一颗偶发红的种子
/// （同 CLAUDE.md 里那条「测并发缺陷的用例必须并发地写」的反面：**不该并发的状态别放进并发用例**）。
/// 故逻辑全部落在这里测，真正读环境变量的只有上面那个薄壳。
pub(crate) fn app_data_dir_from(dir_override: Option<&str>) -> Option<PathBuf> {
    // 空串视为未设置：CI 里 `env: { SYNAROUTE_DATA_DIR: "" }` 是很常见的写法，
    // 若当成有效值就会把配置写到**进程当前工作目录**下的相对路径去 ——
    // 那比读到真实配置更糟（产物目录里凭空多出一份密钥库，且没人会想到去那里找）。
    if let Some(dir) = dir_override.filter(|s| !s.trim().is_empty()) {
        return Some(PathBuf::from(dir));
    }
    dirs::data_dir().map(|d| d.join("SynaRoute"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 变量名是脚本侧与 Rust 侧的**唯一**契约，改名必须同步改 smoke-installer.mjs。
    /// 这条判据本身不能证明脚本跟着改了（那是 check-forbidden 那侧的事），
    /// 但它至少让「随手重命名」这个动作变红一次、迫使改的人去看另一侧。
    #[test]
    fn env_var_name_is_frozen() {
        assert_eq!(ENV_OVERRIDE, "SYNAROUTE_DATA_DIR");
    }

    #[test]
    fn explicit_override_wins_and_is_used_verbatim() {
        let got = app_data_dir_from(Some("D:/tmp/synaroute-smoke")).unwrap();
        assert_eq!(got, PathBuf::from("D:/tmp/synaroute-smoke"));
        // 刻意**不**在覆盖值后面再拼 "SynaRoute"：调用方给的就是最终目录。
        // 若这里自作主张拼一层，脚本里 mkdir 出来的那个目录就不是实例真正用的那个,
        // 而冒烟脚本要去那里读自检行 —— 会变成「门绿了但读的是空目录」。
        assert!(!got.ends_with("SynaRoute"), "覆盖值不得被追加子目录: {got:?}");
    }

    /// 空串 / 纯空白必须当成未设置，否则配置会落到相对路径。
    #[test]
    fn blank_override_falls_back_to_the_system_location() {
        let sys = app_data_dir_from(None);
        for blank in ["", "   ", "\t"] {
            assert_eq!(
                app_data_dir_from(Some(blank)),
                sys,
                "空白覆盖值 {blank:?} 应回落系统目录"
            );
        }
    }

    /// 未设置时行为与改动前**逐字节一致**：`<data_dir>/SynaRoute`。
    /// 这条是防回归的主判据 —— 绝大多数用户永远不会设这个变量，
    /// 他们走的就是这条路径，一旦它变了就是「升级后配置全空」级别的事故。
    #[test]
    fn unset_override_matches_the_previous_behaviour() {
        let expected = dirs::data_dir().map(|d| d.join("SynaRoute"));
        assert_eq!(app_data_dir_from(None), expected);
    }
}
