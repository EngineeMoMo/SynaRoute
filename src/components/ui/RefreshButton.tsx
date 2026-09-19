import { useCallback, useRef, useState } from "react";
import { RefreshCw } from "lucide-react";
import { Button, type ButtonProps } from "@/components/ui/Button";
import { prefersReducedMotion } from "@/lib/motion";

/**
 * 统一的「刷新」按钮 —— 两处用户实报问题的收口（2026-09-18）。
 *
 * # 问题一：两个刷新按钮长得不一样
 *
 * 会话页那个有转圈图标 + 「刷新中…」+ 禁用锁，用量页只是一个纯文字按钮、点了毫无反馈。
 * 同一个动作在两页两种样子。收成一个组件，两页从此一致。
 *
 * # 问题二：转得太快，像有 bug
 *
 * 数据大多已在内存/本地（用量累计、会话列表都是读缓存），`onRefresh` 常常几十毫秒就 resolve，
 * 于是转圈图标闪一下就停 —— 用户反馈「刷新完成也跟有 bug 一样」。这里加一个**最短可见时长**
 * `MIN_SPIN_MS`：即便数据瞬间就绪，转圈也至少走完这段，让「我确实刷新了」这件事看得见。
 *
 * 🔴 **最短时长在减少动态效果时跳过**：那种偏好下图标压根不转（`animate-spin` 无动画），
 * 硬把按钮多锁 900ms 只是让它多禁用一段、毫无信息价值，反而拖慢操作。
 *
 * # 忙态自管，不依赖调用方
 *
 * `onRefresh` 只需返回它那趟异步（各页自己的 `load()`，已各自处理错误与代际防竞态）。
 * 忙态、最短时长、禁用与 `aria-busy` 全在这里，调用方一行接入。用 ref 记录「是否在忙」
 * 而非只靠 state：连点两下时第二下必须被挡住，否则两趟 `load` 交叠又回到竞态。
 */
const MIN_SPIN_MS = 900;

export function RefreshButton({
  onRefresh,
  idleLabel,
  busyLabel,
  variant = "outline",
  size = "sm",
  ...rest
}: {
  onRefresh: () => Promise<unknown> | void;
  idleLabel: string;
  busyLabel: string;
} & Omit<ButtonProps, "onClick" | "children">) {
  const [busy, setBusy] = useState(false);
  const busyRef = useRef(false);

  const run = useCallback(async () => {
    if (busyRef.current) return; // 连点第二下：上一趟还没完，直接忽略
    busyRef.current = true;
    setBusy(true);
    const started = performance.now();
    try {
      await onRefresh();
    } catch {
      // onRefresh 的实现（各页 load）自己吞错并写进各自的 error state；
      // 这里再兜一道，避免 `void run()` 把偶发 reject 变成 unhandled rejection。
    } finally {
      // 补足最短可见时长（减少动态效果时不补 —— 图标本就不转，多锁无意义）。
      const elapsed = performance.now() - started;
      const remaining = MIN_SPIN_MS - elapsed;
      if (remaining > 0 && !prefersReducedMotion()) {
        await new Promise((r) => setTimeout(r, remaining));
      }
      busyRef.current = false;
      setBusy(false);
    }
  }, [onRefresh]);

  return (
    <Button
      variant={variant}
      size={size}
      onClick={() => void run()}
      disabled={busy || rest.disabled}
      aria-busy={busy}
      {...rest}
    >
      <RefreshCw className={`h-4 w-4 ${busy ? "animate-spin" : ""}`} />
      {busy ? busyLabel : idleLabel}
    </Button>
  );
}
