import { cn } from "@/lib/utils";

/**
 * SynaRoute 品牌标记。
 *
 * 图形是 lucide 的 `Waypoints`（四节点 + 三段连线），与三处**同源**：
 * - 应用侧栏：`src/components/Sidebar.tsx` 直接用 `<Waypoints/>` 组件；
 * - 应用图标 / 托盘 / 任务栏：`src-tauri/icons/*`；
 * - favicon 与社交分享图：`site/public/*`，由 `scripts/gen-favicon.mjs` 生成。
 *
 * 坐标逐字抄自 lucide 的定义并按 20/24 缩放居中（与生成脚本同一套变换）。
 * **改图形要同时改 `gen-favicon.mjs` 并重跑它**，否则站内 logo 与标签页图标会分叉
 * —— 这种不一致不报错，只能靠肉眼比对发现（本次就是这么发现的）。
 *
 * 用内联 SVG 而不是直接放 PNG：任意尺寸都清晰，且深色模式下可以只换底色不换图片。
 */
export function LogoMark({ className, size = 32 }: { className?: string; size?: number }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 32 32"
      fill="none"
      aria-hidden="true"
      className={cn("shrink-0", className)}
    >
      <rect width="32" height="32" rx="7.5" className="fill-primary" />
      {/*
        内层是 lucide 24×24 坐标系，缩到 20 并居中（偏移 6）。
        `stroke-width` 也随之缩放：2 × 20/24 ≈ 1.667。
      */}
      <g
        transform="translate(6 6) scale(0.8333)"
        stroke="rgb(var(--primary-foreground))"
        strokeWidth="2"
        strokeLinecap="round"
        fill="rgb(var(--primary-foreground))"
      >
        {/* 连线：端点刻意不落在圆心上，那段留白是原设计的一部分 */}
        <path d="m10.2 6.3-3.9 3.9" />
        <path d="M7 12h10" />
        <path d="m13.8 17.7 3.9-3.9" />
        {/* 四个节点：上、左、右、下 */}
        <circle cx="12" cy="4.5" r="2.5" stroke="none" />
        <circle cx="4.5" cy="12" r="2.5" stroke="none" />
        <circle cx="19.5" cy="12" r="2.5" stroke="none" />
        <circle cx="12" cy="19.5" r="2.5" stroke="none" />
      </g>
    </svg>
  );
}

/** 标记 + 文字，用于顶栏与页脚 */
export function Logo({ className, size = 32 }: { className?: string; size?: number }) {
  return (
    <span className={cn("inline-flex items-center gap-2.5", className)}>
      <LogoMark size={size} />
      <span className="text-lg font-semibold tracking-tight text-text-primary">SynaRoute</span>
    </span>
  );
}
