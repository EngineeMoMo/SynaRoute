import { useEffect, useRef, useState } from "react";
import { prefersReducedMotion } from "@/lib/motion";

/**
 * 数字变化时做补间（用量页的 token 数与金额）。
 *
 * # 为什么是 rAF 补间，而不是纯 CSS
 *
 * CSS 侧有个时髦写法：`@property` 注册一个数值自定义属性 + `counter()` + `content`
 * 把它渲染出来。**这里不能用** —— `content` 生成的文本**选不中、复制不了**，
 * 读屏器也可能整段跳过。而用量页的金额恰恰是用户要复制出去对账的东西。
 * 少一点「前沿」，换回可选中的真实文本节点，这笔交换是划算的。
 *
 * # 为什么首次渲染不补间
 *
 * 进页面时从 0 数上来是演示感，不是信息 —— 用户第一眼要的是「多少」，
 * 不是看它爬。只有**已经在看着这一格、而它变了**时，补间才回答了
 * 「刚才那个数动了吗、往哪动」这个真实问题（用量页有 30s 轮询）。
 *
 * # 为什么按相对幅度判断值不值得补间
 *
 * 金额从 `$0.0042` 变成 `$0.0043` 时补间 520ms，中间每一帧都是同一个显示值，
 * 看起来就是卡了一下。取「相对变化 < 0.5% 就直接跳」这个判据 ——
 * 绝对阈值在这里没用：token 数是百万量级，金额是千分之一量级，同一个阈值
 * 对其中一方必然是错的。
 */
export function AnimatedNumber({
  value,
  format,
  className,
  durationMs = 520,
}: {
  value: number;
  format: (n: number) => string;
  className?: string;
  durationMs?: number;
}) {
  // 当前**显示**的数值。初值即目标值 —— 首次渲染不补间（见上）。
  const [shown, setShown] = useState(value);
  // 上一次的目标值。用 ref 而非 state：它只是比较基准，变化不该触发重渲染。
  const fromRef = useRef(value);
  const rafRef = useRef<number | null>(null);

  useEffect(() => {
    const from = fromRef.current;
    fromRef.current = value;

    if (from === value) return;

    // 三种情况直接跳到终值：用户要求减少动态效果、脏数据（补间会渲染出 NaN）、
    // 幅度小到补间期间显示值根本不变。
    const denom = Math.max(Math.abs(from), Math.abs(value), 1);
    const negligible = Math.abs(value - from) / denom < 0.005;
    if (prefersReducedMotion() || !Number.isFinite(value) || !Number.isFinite(from) || negligible) {
      setShown(value);
      return;
    }

    const started = performance.now();
    const tick = (now: number) => {
      const t = Math.min(1, (now - started) / durationMs);
      // easeOutCubic：起步快、收尾稳。数字补间用线性会显得机械。
      const eased = 1 - Math.pow(1 - t, 3);
      setShown(from + (value - from) * eased);
      if (t < 1) rafRef.current = requestAnimationFrame(tick);
    };
    rafRef.current = requestAnimationFrame(tick);

    // 卸载/值再变时必须取消：否则切页后仍有 rAF 在跑并对已卸载组件 setState。
    return () => {
      if (rafRef.current !== null) cancelAnimationFrame(rafRef.current);
      rafRef.current = null;
    };
  }, [value, durationMs]);

  // 🔴 收尾必须显示**目标值本身**而不是补间到的浮点数：`format` 多为 toFixed，
  // 补间末帧的 0.19999999 会渲染成与静止态差一位的数字。
  const done = shown === value;
  return <span className={className}>{format(done ? value : shown)}</span>;
}
