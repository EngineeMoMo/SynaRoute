import { useEffect, useRef } from "react";

/**
 * 周期轮询，**窗口不可见时自动暂停**（UX#9）。
 *
 * 为什么需要：本应用的实时性全靠轮询撑（全仓零 Tauri 事件推送），共 6 处 `setInterval`，
 * 最密的日志页 2 秒一次。而这些定时器原先不看窗口状态——用户把主窗口最小化到托盘后
 * （这是本应用的常态使用姿势：起了代理就丢后台），轮询仍在持续跑，每次都要过一遍 IPC、
 * 让后端取锁读配置、序列化、前端反序列化再触发 React 更新。纯白烧 CPU 与电量。
 *
 * 语义：
 * - 挂载时若窗口可见 → 立即执行一次，然后按 `intervalMs` 周期执行；
 * - 窗口转为不可见 → 停表（**不是跳过一次**，而是彻底不再触发）；
 * - 窗口重新可见 → **立即补一次**再继续周期。这一点很重要：用户切回来最想看到的是当下状态，
 *   若只是恢复计时器，最坏要等一个完整周期才刷新，界面会显示成陈旧数据。
 *
 * @param fn 每次触发要做的事。用 ref 存住，故传入内联箭头函数不会导致重设定时器。
 * @param intervalMs 周期。传 0 或负数 = 停用（用于「用户把某项设成关」的场景）。
 * @param enabled 额外开关，false 时不轮询（如页面内某个折叠区未展开）。
 */
export function usePolling(fn: () => void, intervalMs: number, enabled = true) {
  // 用 ref 持有回调：调用方几乎都会传内联闭包，若直接进依赖数组会每次渲染都重建定时器。
  const fnRef = useRef(fn);
  fnRef.current = fn;

  useEffect(() => {
    if (!enabled || intervalMs <= 0) return;

    let timer: number | undefined;

    const stop = () => {
      if (timer !== undefined) {
        clearInterval(timer);
        timer = undefined;
      }
    };

    const start = () => {
      if (timer !== undefined) return; // 已在跑，避免重复计时器
      fnRef.current(); // 立即补一次：切回前台时不让用户对着陈旧数据等一个周期
      timer = window.setInterval(() => fnRef.current(), intervalMs);
    };

    const onVisibility = () => {
      if (document.visibilityState === "visible") start();
      else stop();
    };

    // 初始：可见才起表
    onVisibility();
    document.addEventListener("visibilitychange", onVisibility);
    return () => {
      document.removeEventListener("visibilitychange", onVisibility);
      stop();
    };
  }, [intervalMs, enabled]);
}
