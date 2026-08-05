import { cn } from "@/lib/utils";

/**
 * SynaRoute 品牌标记。
 *
 * 图形与 src-tauri/icons 里的应用图标一致：一条对角线上串起三个节点，
 * 中间那个最大 —— 表达「请求经由中枢节点在多个上游之间路由」。
 *
 * 用内联 SVG 而不是直接放 PNG：任意尺寸都清晰，且深色模式下可以只换底色
 * 不换图片。favicon 与社交分享图仍用 public/ 下的 PNG。
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
      <path
        d="M9 8.5 16 16l7 7.5"
        stroke="rgb(var(--primary-foreground))"
        strokeWidth="2.4"
        strokeLinecap="round"
      />
      <circle cx="9" cy="8.5" r="2.9" fill="rgb(var(--primary-foreground))" />
      <circle cx="16" cy="16" r="4.4" fill="rgb(var(--primary-foreground))" />
      <circle cx="23" cy="23.5" r="2.9" fill="rgb(var(--primary-foreground))" />
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
