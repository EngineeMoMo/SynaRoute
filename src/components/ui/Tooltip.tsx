import * as React from "react";
import { createPortal } from "react-dom";

interface TooltipProps {
  /** 触发元素 */
  children: React.ReactElement;
  /** 提示内容 */
  content: React.ReactNode;
  /** 位置偏好（会自动避让边界） */
  side?: "top" | "right" | "bottom" | "left";
  /** 延迟显示（ms） */
  delayMs?: number;
  /** 是否禁用 */
  disabled?: boolean;
}

/**
 * 轻量级 Tooltip 组件（无外部依赖，自动边界检测）。
 *
 * 用法：
 * ```tsx
 * <Tooltip content="这是提示" side="top">
 *   <button>悬停我</button>
 * </Tooltip>
 * ```
 *
 * 特性：
 * - 自动避让边界（优先用户指定方向，放不下时反向）
 * - 延迟显示（避免快速划过时闪烁）
 * - Portal 渲染（不受父容器 overflow 影响）
 * - 键盘友好（Esc 关闭、焦点时显示）
 */
export function Tooltip({
  children,
  content,
  side = "top",
  delayMs = 200,
  disabled = false,
}: TooltipProps) {
  const [visible, setVisible] = React.useState(false);
  const [actualSide, setActualSide] = React.useState(side);
  const [pos, setPos] = React.useState({ x: 0, y: 0 });
  const triggerRef = React.useRef<HTMLElement>(null);
  const tooltipRef = React.useRef<HTMLDivElement>(null);
  const timerRef = React.useRef<ReturnType<typeof setTimeout>>();

  const show = React.useCallback(() => {
    if (disabled || !content) return;
    timerRef.current = setTimeout(() => setVisible(true), delayMs);
  }, [disabled, content, delayMs]);

  const hide = React.useCallback(() => {
    if (timerRef.current) clearTimeout(timerRef.current);
    setVisible(false);
  }, []);

  // 计算位置（考虑边界避让）。
  //
  // **必须是 useLayoutEffect 而不是 useEffect**：位置要先量到触发元素与提示框的尺寸才能算，
  // 所以首帧渲染时 `pos` 还是初始的 {0,0}。useEffect 在**浏览器绘制之后**才跑，
  // 那一帧会把提示框画在屏幕左上角、下一帧才跳到触发元素旁边 —— 表现为快速划过时
  // 左上角闪一下黑框。useLayoutEffect 在绘制前同步跑完，首帧就是最终位置。
  React.useLayoutEffect(() => {
    if (!visible || !triggerRef.current || !tooltipRef.current) return;

    const trigger = triggerRef.current.getBoundingClientRect();
    const tooltip = tooltipRef.current.getBoundingClientRect();
    const gap = 8; // 触发元素与提示框间距
    const margin = 12; // 提示框与视口边界的最小间距

    // 候选位置计算（四个方向）
    const candidates = {
      top: {
        x: trigger.left + trigger.width / 2 - tooltip.width / 2,
        y: trigger.top - tooltip.height - gap,
      },
      bottom: {
        x: trigger.left + trigger.width / 2 - tooltip.width / 2,
        y: trigger.bottom + gap,
      },
      left: {
        x: trigger.left - tooltip.width - gap,
        y: trigger.top + trigger.height / 2 - tooltip.height / 2,
      },
      right: {
        x: trigger.right + gap,
        y: trigger.top + trigger.height / 2 - tooltip.height / 2,
      },
    };

    // 边界检测：优先用户指定方向，放不下时按 top→bottom→right→left 顺序 fallback
    const fits = (s: keyof typeof candidates) => {
      const p = candidates[s];
      return (
        p.x >= margin &&
        p.x + tooltip.width <= window.innerWidth - margin &&
        p.y >= margin &&
        p.y + tooltip.height <= window.innerHeight - margin
      );
    };

    let finalSide = side;
    if (!fits(side)) {
      const fallback: Array<keyof typeof candidates> = ["top", "bottom", "right", "left"];
      finalSide = fallback.find(fits) ?? side; // 实在没地方就用原方向（部分遮挡）
    }

    const final = candidates[finalSide];
    // 水平居中时做边界修正（避免箭头偏离触发元素）
    if (finalSide === "top" || finalSide === "bottom") {
      final.x = Math.max(margin, Math.min(final.x, window.innerWidth - tooltip.width - margin));
    }
    // 垂直居中时做边界修正
    if (finalSide === "left" || finalSide === "right") {
      final.y = Math.max(margin, Math.min(final.y, window.innerHeight - tooltip.height - margin));
    }

    setActualSide(finalSide);
    setPos({ x: final.x, y: final.y });
  }, [visible, side]);

  // Esc 关闭
  React.useEffect(() => {
    if (!visible) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") hide();
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [visible, hide]);

  // 克隆子元素并附加事件（保留原有 props）
  const trigger = React.cloneElement(children, {
    ref: triggerRef,
    onMouseEnter: (e: React.MouseEvent) => {
      show();
      children.props.onMouseEnter?.(e);
    },
    onMouseLeave: (e: React.MouseEvent) => {
      hide();
      children.props.onMouseLeave?.(e);
    },
    onFocus: (e: React.FocusEvent) => {
      show();
      children.props.onFocus?.(e);
    },
    onBlur: (e: React.FocusEvent) => {
      hide();
      children.props.onBlur?.(e);
    },
  });

  // 箭头方向（CSS 用）
  const arrowClass = {
    top: "bottom-[-4px] left-1/2 -translate-x-1/2 border-l-transparent border-r-transparent border-b-transparent",
    bottom: "top-[-4px] left-1/2 -translate-x-1/2 border-l-transparent border-r-transparent border-t-transparent",
    left: "right-[-4px] top-1/2 -translate-y-1/2 border-t-transparent border-b-transparent border-r-transparent",
    right: "left-[-4px] top-1/2 -translate-y-1/2 border-t-transparent border-b-transparent border-l-transparent",
  }[actualSide];

  return (
    <>
      {trigger}
      {visible &&
        content &&
        createPortal(
          <div
            ref={tooltipRef}
            className="pointer-events-none fixed z-[9999] animate-in fade-in-0 zoom-in-95 duration-150"
            style={{
              left: `${pos.x}px`,
              top: `${pos.y}px`,
            }}
            role="tooltip"
          >
            <div className="relative max-w-xs rounded-md bg-gray-900 px-3 py-1.5 text-xs text-white shadow-lg dark:bg-gray-100 dark:text-gray-900">
              {content}
              {/* 箭头（纯 CSS 实现，不依赖 SVG） */}
              <div
                className={`absolute h-0 w-0 border-4 border-gray-900 dark:border-gray-100 ${arrowClass}`}
              />
            </div>
          </div>,
          document.body
        )}
    </>
  );
}
