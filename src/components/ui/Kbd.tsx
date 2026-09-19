import { cn } from "@/lib/utils";

/**
 * 键位提示片（`Ctrl K`、`Esc`）—— **唯一事实来源**。
 *
 * 此前两处（侧栏命令面板入口、面板里的 Esc）各写了一份**几乎相同**的 className，
 * 用的是 `rounded`（4px 直角）+ `bg-background`。问题有二：
 * - 圆角与全应用控件的 `rounded-control`（10px）不成体系，凑在一排按钮里显得格格不入；
 * - `bg-background` 是**页面底色**，贴在 `bg-surface` 的侧栏/面板上像一个抠空的洞，
 *   而不是一枚浮起的键帽。
 *
 * 收成组件后统一成：`rounded`（键帽本就该比控件方一点，但用 6px 而非 4px 柔化）、
 * `bg-surface-elevated`（比所在面板高一档，读作「浮起的键」）、等宽字体（键位是字符不是文案）。
 * aria-hidden：它是给视力用户的记忆提示，读屏器已从按钮的可见文字/`aria-label` 得到信息，
 * 再念一遍 “Esc” 是噪音。
 */
export function Kbd({ children, className }: { children: React.ReactNode; className?: string }) {
  return (
    <kbd
      aria-hidden
      className={cn(
        "inline-flex shrink-0 items-center rounded-[6px] border border-border bg-surface-elevated px-1.5 py-0.5 font-mono text-[10px] font-medium leading-none text-text-muted",
        className,
      )}
    >
      {children}
    </kbd>
  );
}
