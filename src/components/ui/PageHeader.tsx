import type { ReactNode } from "react";
import type { LucideIcon } from "lucide-react";

/**
 * 页头：图标 + 标题 + 说明 + 右侧操作区。
 *
 * # 图标与标题是「共享元素」
 *
 * 两者各带一个 `view-transition-name`，于是切页时浏览器把它们当成**同一个元素的
 * 前后两态**，做位置与尺寸的连续补间 —— 标题从「Claude CLI」变成「运行日志」时
 * 是平滑移动 + 交叉淡化，而不是闪一下重绘。动画曲线在 styles.css 里统一定义。
 *
 * 🔴 **名字必须全局唯一**：重复的 `view-transition-name` 会让浏览器**中止整场过渡**
 * （不只是这一个元素不动，是切页的所有动画一起没了，且控制台不一定报错）。
 * 这里安全的前提是 App.tsx 的 if 链同一时刻只渲染一个页面、也就只有一个 PageHeader。
 * 将来若出现「一屏两个页头」（比如分栏布局），必须改成按页面传入不同的名字。
 */
export function PageHeader({
  icon: Icon,
  title,
  description,
  actions,
}: {
  icon?: LucideIcon;
  title: string;
  description?: ReactNode;
  actions?: ReactNode;
}) {
  return (
    <header className="flex shrink-0 items-start justify-between gap-4 border-b border-border/80 px-6 py-4">
      <div className="min-w-0">
        <div className="flex items-center gap-2.5">
          {Icon && (
            <span
              className="flex h-8 w-8 shrink-0 items-center justify-center rounded-control border border-primary/20 bg-primary/12 text-primary"
              style={{ viewTransitionName: "page-icon" }}
            >
              <Icon size={17} aria-hidden="true" />
            </span>
          )}
          <h1
            className="truncate text-lg font-semibold tracking-[-0.01em] text-text-primary"
            style={{ viewTransitionName: "page-title" }}
          >
            {title}
          </h1>
        </div>
        {description && <div className="mt-1.5 max-w-2xl text-xs leading-relaxed text-text-secondary">{description}</div>}
      </div>
      {actions && <div className="flex shrink-0 items-center gap-2">{actions}</div>}
    </header>
  );
}
