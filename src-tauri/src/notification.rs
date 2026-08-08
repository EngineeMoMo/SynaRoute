//! 系统通知（代理健康告警：Key 熔断 / 恢复）。
//!
//! 刻意独立成模块并整体 `#[cfg(not(test))]`：`tauri-plugin-notification` 在 Windows 上
//! 通过 `notify-rust` → `winrt-notification` 链接 WinRT API set（delay-load）。把它链接进
//! `cargo test --lib` 的测试二进制会导致 `STATUS_ENTRYPOINT_NOT_FOUND`（0xc0000139）——
//! 进程启动即崩、一个测试都跑不了。测试进程不初始化插件、也永远不该发系统通知，
//! 故测试路径完全不编译本模块。
//!
//! 代价：本模块的单元测试只能靠「不崩」间接覆盖；熔断跃迁的逻辑判据（只在未熔断→刚熔断、
//! 熔断→恢复时发）在 `health.rs` 侧，那里不依赖本模块、仍可测。

#![cfg(not(test))]

/// 发一条**系统级**通知（Key 熔断 / 恢复时由 `health.rs` 调用）。
///
/// 与 `events::emit` 的区别：emit 是前端「敲门」信号，这条是真的要弹系统框。
///
/// **节流纪律**：调用方必须在**状态跃迁**时才调（如熔断武装 / 解除），不是每次失败都调，
/// 否则会刷屏。权限读取与 `show` 失败都不致命（记日志即可），通知是锦上添花，
/// 不该因此打断转发热路径。
pub fn notify(title: &str, body: &str) {
    use tauri_plugin_notification::NotificationExt;
    // AppHandle 在 setup 里装（events::init 已持有）。未初始化（`--mcp-stdio`）时无通知。
    let Some(app) = crate::events::app_handle() else { return };
    // Windows 上首次调用会触发系统通知权限请求（若未授权）。失败不致命。
    let Ok(permission) = app.notification().permission_state() else {
        tracing::debug!("读取通知权限状态失败，跳过通知");
        return;
    };
    if !matches!(permission, tauri::plugin::PermissionState::Granted) {
        tracing::debug!("通知权限未授权（{permission:?}），跳过系统通知");
        return;
    }
    let _ = app
        .notification()
        .builder()
        .title(title)
        .body(body)
        .show();
}
