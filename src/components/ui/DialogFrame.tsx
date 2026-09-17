import type { MouseEvent, ReactNode, RefObject } from "react";
import { cn } from "@/lib/utils";

export type DialogSize = "sm" | "md" | "lg" | "xl";

const sizeClasses: Record<DialogSize, string> = {
  sm: "w-[min(420px,100%)]",
  md: "w-[min(520px,100%)]",
  lg: "w-[min(640px,100%)]",
  xl: "w-[min(760px,100%)]",
};

/** 统一弹窗的视觉外壳；关闭、Esc 与焦点策略由调用方按业务场景控制。 */
export function DialogFrame({
  children,
  size = "md",
  role = "dialog",
  ariaLabel,
  ariaLabelledBy,
  className,
  overlayClassName,
  panelRef,
  onBackdropMouseDown,
}: {
  children: ReactNode;
  size?: DialogSize;
  role?: "dialog" | "alertdialog";
  ariaLabel?: string;
  ariaLabelledBy?: string;
  className?: string;
  overlayClassName?: string;
  panelRef?: RefObject<HTMLDivElement>;
  onBackdropMouseDown?: (event: MouseEvent<HTMLDivElement>) => void;
}) {
  return (
    <div
      className={cn("fixed inset-0 z-50 flex items-center justify-center p-4 sr-overlay", overlayClassName)}
      onMouseDown={onBackdropMouseDown}
    >
      <div
        ref={panelRef}
        role={role}
        aria-modal="true"
        aria-label={ariaLabel}
        aria-labelledby={ariaLabelledBy}
        className={cn("sr-dialog flex max-h-[90vh] flex-col overflow-hidden", sizeClasses[size], className)}
      >
        {children}
      </div>
    </div>
  );
}

export function DialogHeader({ children, className }: { children: ReactNode; className?: string }) {
  return <div className={cn("flex shrink-0 items-center justify-between gap-3 border-b border-border/80 px-5 py-4", className)}>{children}</div>;
}

export function DialogBody({ children, className }: { children: ReactNode; className?: string }) {
  return <div className={cn("min-h-0 flex-1 overflow-y-auto px-5 py-4", className)}>{children}</div>;
}

export function DialogFooter({ children, className }: { children: ReactNode; className?: string }) {
  return <div className={cn("flex shrink-0 items-center justify-end gap-2 border-t border-border/80 px-5 py-3", className)}>{children}</div>;
}
