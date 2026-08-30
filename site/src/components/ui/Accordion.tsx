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
 * 需要控制展开动画与「同时展开多条」，<details> 两者都不好做。
 * 键盘可达性由 button 天然保证（Enter/Space 都能触发）。
 *
 * ## 排版：一体化列表（此前是十张独立卡片）
 *
 * 旧写法是「每条独立成卡 + 12px 间距」，注释里的理由是「十条堆在一个大框里时，
 * 每行只有一条细线，扫视起来是一片密集的横线，看不出条目边界」。
 * **那个顾虑是对的，但它针对的是「矮行 + 细线」这种组合。** 实际效果是另一头的
 * 问题：十个等大的圆角白盒子连成约 790px 的同构横条，与页面上其余卡片网格
 * 完全同权重，读起来像「又一组功能卡」而不是一份可扫的问题清单。
 *
 * 现在的解法同时避开两头：
 * - 一个外框 + 分隔线（不是十个盒子）→ 它是一份清单，不再是一组卡片；
 * - 行高给到 68px 以上（py-5，比原来的 py-4 更高）→ 不会变成「密集的横线」；
 * - 展开的那条整行换底色 + 编号变实心主色 → 状态比原先靠边框变色更明确。
 *
 * 默认全收起：十条问题若展开第一条，首屏会被一段长答案占掉，反而看不清有哪些问题。
 */
export function Accordion({ items, className }: { items: AccordionEntry[]; className?: string }) {
  const [openIds, setOpenIds] = useState<string[]>([]);
  const baseId = useId();

  function toggle(id: string) {
    setOpenIds((prev) => (prev.includes(id) ? prev.filter((x) => x !== id) : [...prev, id]));
  }

  return (
    <div
      className={cn(
        "divide-y divide-border overflow-hidden rounded-card border border-border bg-surface shadow-card",
        className
      )}
    >
      {items.map((item, i) => {
        const open = openIds.includes(item.id);
        const btnId = `${baseId}-${item.id}-btn`;
        const panelId = `${baseId}-${item.id}-panel`;
        return (
          <div key={item.id} className={cn("transition-colors", open && "bg-surface-hover")}>
            <h3>
              <button
                id={btnId}
                type="button"
                aria-expanded={open}
                aria-controls={panelId}
                onClick={() => toggle(item.id)}
                className="flex w-full items-start gap-4 px-5 py-5 text-left transition-colors hover:bg-surface-hover sm:px-6"
              >
                {/* 编号：给十条问题一个可指认的序号，答复用户时能说「第 6 条」 */}
                <span
                  aria-hidden="true"
                  className={cn(
                    "mt-px inline-flex h-6 w-6 shrink-0 items-center justify-center rounded-full font-mono text-[11px] font-semibold transition-colors",
                    open ? "bg-primary-solid text-primary-foreground" : "bg-surface-hover text-text-secondary"
                  )}
                >
                  {i + 1}
                </span>
                <span className="flex-1 text-[15px] font-medium leading-6 text-text-primary">
                  {item.question}
                </span>
                <ChevronDown
                  size={18}
                  aria-hidden="true"
                  className={cn(
                    "mt-0.5 shrink-0 transition-transform duration-250",
                    open ? "rotate-180 text-primary" : "text-text-secondary"
                  )}
                />
              </button>
            </h3>
            {/* grid-rows 从 0fr 到 1fr 是唯一能对「未知高度」做纯 CSS 过渡的写法；
                收起时 aria-hidden，避免读屏和 Tab 键进到看不见的内容里 */}
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
                {/* 左内边距对齐问题文字，答案看起来是挂在问题下面的。
                    64px = 按钮内边距 24（sm:px-6）+ 编号 24（w-6）+ 间距 16（gap-4）。
                    改上面任何一个值，这里要跟着改，否则答案会和问题错开一点点。 */}
                <p className="px-5 pb-5 text-[15px] leading-7 text-text-secondary sm:pb-6 sm:pl-16 sm:pr-6">
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
