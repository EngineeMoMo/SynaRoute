import { isTauri } from "@/lib/bridge";

/**
 * 系统通知权限请求（FR-028 代理健康告警）。
 *
 * 系统级弹窗（Key 熔断/恢复通知）需要权限；应用内警告条不需要。启动时调一次：
 * - 已授权：静默通过
 * - 未决定：弹系统授权框（macOS 首次、Windows 首次都会弹）
 * - 拒绝：应用内警告条仍工作，仅系统弹窗不弹（后端 `events::notify` 会跳过未授权）
 *
 * 用动态 import：浏览器 mock（无 Tauri 通知插件）下 import 不会崩，只是不执行。
 */
export async function requestNotificationPermission(): Promise<void> {
  if (!isTauri()) return;
  try {
    const { isPermissionGranted, requestPermission } = await import(
      "@tauri-apps/plugin-notification"
    );
    if (await isPermissionGranted()) return;
    await requestPermission();
  } catch (e) {
    // 通知是锦上添花：权限请求失败不该影响启动。记日志即可。
    console.error("requestNotificationPermission failed", e);
  }
}
