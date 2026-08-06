//! 子进程启动的统一入口 —— **Windows 上必须不弹控制台窗口**。
//!
//! 为什么单独成一个模块：`Command::new` 在 Windows 上启动**控制台子系统**的程序时，
//! 系统会为它分配一个新控制台并**弹出一个黑窗口**。我们自己的 exe 是 GUI 子系统
//! （`main.rs` 的 `windows_subsystem = "windows"`，已验证 PE Subsystem = 2），
//! 所以主窗口不带控制台 —— 但每次起 `rg` / `codegraph.cmd` / `npm.cmd` 这类控制台程序，
//! 都会闪一个黑窗口出来。`.cmd` 更明显：它得先起 `cmd.exe` 再执行脚本。
//!
//! 用户看到的现象就是「点某个页面时黑窗口一闪」，而且因为是异步起的、时机不定，
//! 很难和具体操作对上号。
//!
//! 修法是给 `CreateProcess` 传 `CREATE_NO_WINDOW`：子进程照常跑、stdout/stderr 照常被我们
//! 捕获（我们本来就用 `.output()` 收管道，从不依赖那个控制台），只是不再分配可见控制台。
//!
//! **新增任何子进程调用都必须走这里**，否则黑窗口会重新出现。

use tokio::process::Command;

/// `CREATE_NO_WINDOW`（winbase.h）：不为子进程分配控制台窗口。
///
/// 刻意用字面量而不引 `windows` crate 的常量：这一个数值三十年没变过，
/// 而为一个常量把 `windows-sys` 拉进依赖树不值得。
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 建一个「不弹控制台窗口」的 `Command`（非 Windows 平台等价于 `Command::new`）。
///
/// 用法与 `Command::new` 完全一致：
/// ```ignore
/// let out = hidden("rg").args(["--files-with-matches", kw]).output().await;
/// ```
///
/// 注：`creation_flags` 是 `tokio::process::Command` 在 Windows 上的**固有方法**，
/// 不需要 `use std::os::windows::process::CommandExt`（那是给 `std` 的 Command 用的）。
pub fn hidden(program: impl AsRef<std::ffi::OsStr>) -> Command {
    // `mut` 只有 Windows 分支用得上（`creation_flags` 取 `&mut self`）。非 Windows 上
    // 它是死的，clippy/rustc 会报 unused_mut —— 在 macOS runner 上首次编译才看得见。
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 冒烟：`hidden()` 造出的 Command 仍能正常执行并捕获 stdout。
    ///
    /// 这条护栏的意义在于「加了 CREATE_NO_WINDOW 之后管道还通」—— 若某天误把
    /// `CREATE_NEW_CONSOLE` 之类的标志写进去，子进程的 stdout 会跑到那个新控制台里，
    /// 我们收到空输出，而所有依赖 stdout 判据的逻辑（codegraph 三态检测、rg 命中列表）
    /// 会**静默退化**成「工具不可用」。
    #[tokio::test]
    async fn hidden_command_still_captures_stdout() {
        // 用各平台都有的命令：Windows 上 cmd 是内置壳（.cmd 走 cmd.exe），其余用 sh。
        #[cfg(windows)]
        let out = hidden("cmd").args(["/C", "echo", "synaroute-probe"]).output().await;
        #[cfg(not(windows))]
        let out = hidden("sh").args(["-c", "echo synaroute-probe"]).output().await;

        let out = out.expect("子进程应能启动");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("synaroute-probe"),
            "加了 CREATE_NO_WINDOW 后 stdout 仍必须被捕获到，实际: {stdout:?}"
        );
    }
}
