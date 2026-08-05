import { useId, useState } from "react";
import { ChevronDown } from "lucide-react";
import { cn } from "@/lib/utils";

export interface AccordionEntry {
  id: string;
  question: string;
  answer: string;
}

/**
 * FAQ 手风琴。
 *
 * 用原生 <button> + aria-expanded/aria-controls，而不是 <details>：
 * 需要控制「默认展开第一条」以及展开动画，<details> 两者都不好做。
 * 键盘可达性由 button 天然保证（Enter/Space 都能触发）。
 */
export function Accordion({ items, className }: { items: AccordionEntry[]; className?: string }) {
  // 默认展开第一条：进来就有内容可读，比全收起更友好（模板第 6.9 节允许二选一）
  const [openIds, setOpenIds] = useState<string[]>(() => (items[0] ? [items[0].id] : []));
  const baseId = useId();

  function toggle(id: string) {
    setOpenIds((prev) => (prev.includes(id) ? prev.filter((x) => x !== id) : [...prev, id]));
  }

  return (
    <div className={cn("divide-y divide-border overflow-hidden rounded-card border border-border bg-surface", className)}>
      {items.map((item) => {
        const open = openIds.includes(item.id);
        const btnId = `${baseId}-${item.id}-btn`;
        const panelId = `${baseId}-${item.id}-panel`;
        return (
          <div key={item.id}>
            <h3>
              <button
                id={btnId}
                type="button"
                aria-expanded={open}
                aria-controls={panelId}
                onClick={() => toggle(item.id)}
                className="flex w-full items-center justify-between gap-4 px-5 py-4 text-left transition-colors hover:bg-surface-hover sm:px-6 sm:py-5"
              >
                <span className="text-[15px] font-medium text-text-primary">{item.question}</span>
                <ChevronDown
                  size={18}
                  aria-hidden="true"
                  className={cn(
                    "shrink-0 text-text-muted transition-transform duration-250",
                    open && "rotate-180"
                  )}
                />
              </button>
            </h3>
            {/* grid-rows 从 0fr 到 1fr 是唯一能对「未知高度」做纯 CSS 过渡的写法；
                收起时 aria-hidden + 不可聚焦，避免读屏和 Tab 键进到看不见的内容里 */}
            <div
              id={panelId}
              role="region"
              aria-labelledby={btnId}
              aria-hidden={!open}
              className={cn(
                "grid transition-all duration-250 ease-out",
                open ? "grid-rows-[1fr] opacity-100" : "grid-rows-[0fr] opacity-0"
              )}
            >
              <div className="overflow-hidden">
                <p className="px-5 pb-5 text-[15px] leading-7 text-text-secondary sm:px-6 sm:pb-6">
                  {item.answer}
                </p>
              </div>
            </div>
          </div>
        );
      })}
    </div>
  );
}
