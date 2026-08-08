//! 系统通知（代理健康告警：Key 熔断 / 恢复）。
//!
//! 本模块整体 `#![cfg(not(test))]`：测试进程不初始化插件、也永远不该弹系统框，
//! 故测试路径干脆不编译发送逻辑。
//!
//! ⚠️ **不要把这个 cfg 当成「阻止 WinRT 进测试二进制」的机关**。历史上 `cargo test --lib`
//! 出过 `STATUS_ENTRYPOINT_NOT_FOUND`（0xc0000139，进程启动即崩），当时把原因记为
//! 「`notify-rust` → `winrt-notification` 的 delay-load 被链进测试二进制」，并把本 cfg
//! 记为修复手段。2026-08-08 实测两条都不成立：
//!
//!   1. `tauri_plugin_notification::init()` 在 `lib.rs` 的 `run()` 里**没有 cfg 门**，
//!      插件 crate 在 `cargo test` 下同样被链接 —— 本 cfg 拦不住链接，只拦住调用点。
//!   2. 临时去掉本 cfg 后 `cargo test --lib` 仍是 580 passed / 0 failed，不复现崩溃。
//!
//! 结论：本 cfg 是「测试不该发通知」的语义隔离，**不是**那次崩溃的解药。若将来又撞上
//! 同样的启动崩溃，别在这一行上找答案，去查依赖版本与实际链接的 API set。
//!
//! 代价：本模块无法被单元测试覆盖；熔断跃迁的逻辑判据（只在未熔断→刚熔断、
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
    // 权限门：桌面端（Windows/macOS）的 `permission_state()` 是**无条件返回 Granted** 的
    // 存根实现（tauri-plugin-notification 2.3.3 `desktop.rs:65`），既不查系统设置也不弹请求框。
    // 这里保留它只为移动端语义与将来插件行为变化；**不要**据此以为桌面端已确认拿到权限
    // ——桌面端真正会不会弹，取决于系统通知设置，失败会体现在下面 `show()` 的返回值上。
    let Ok(permission) = app.notification().permission_state() else {
        tracing::debug!("读取通知权限状态失败，跳过通知");
        return;
    };
    if !matches!(permission, tauri::plugin::PermissionState::Granted) {
        tracing::debug!("通知权限未授权（{permission:?}），跳过系统通知");
        return;
    }
    // `show()` 失败**必须留痕**：健康告警是「出问题时才响」的功能，平时不响是正常的，
    // 所以一旦发送链路整体坏掉（系统关闭了通知、专注模式、WinRT 调用失败），
    // 用户与排障者都无法从「没收到通知」区分出「没出故障」和「告警已死」。
    // 静默 `let _ =` 正是本项目反复踩过的失效形态，故这里降级为日志而不是丢弃。
    if let Err(e) = app.notification().builder().title(title).body(body).show() {
        tracing::warn!("系统通知发送失败（健康告警未能弹出）：{e}");
    }
}
