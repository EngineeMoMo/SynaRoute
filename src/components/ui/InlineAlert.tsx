import { forwardRef, type ForwardedRef, type ReactNode } from "react";
import type { LucideIcon } from "lucide-react";
import { AlertCircle, AlertTriangle, CheckCircle2, Info } from "lucide-react";
import { cn } from "@/lib/utils";

export type InlineAlertTone = "info" | "success" | "warning" | "danger";

const toneClasses: Record<InlineAlertTone, string> = {
  info: "border-info/25 bg-info/8 text-info",
  success: "border-success/25 bg-success/8 text-success",
  warning: "border-warning/30 bg-warning/8 text-warning",
  danger: "border-danger/30 bg-danger/8 text-danger",
};

const toneIcons: Record<InlineAlertTone, LucideIcon> = {
  info: Info,
  success: CheckCircle2,
  warning: AlertTriangle,
  danger: AlertCircle,
};

export interface InlineAlertProps {
  tone?: InlineAlertTone;
  icon?: LucideIcon;
  action?: ReactNode;
  children: ReactNode;
  className?: string;
  role?: "alert" | "status";
}

function InlineAlertBase(
  {
    tone = "info",
    icon: CustomIcon,
    action,
    children,
    className,
    role,
  }: InlineAlertProps,
  ref: ForwardedRef<HTMLDivElement>,
) {
  const Icon = CustomIcon ?? toneIcons[tone];
  return (
    <div ref={ref} className={cn("flex items-start gap-2 rounded-control border px-3 py-2 text-xs leading-relaxed", toneClasses[tone], className)} role={role}>
      <Icon size={14} className="mt-0.5 shrink-0" aria-hidden="true" />
      <div className="min-w-0 flex-1">{children}</div>
      {action && <div className="shrink-0">{action}</div>}
    </div>
  );
}

export const InlineAlert = forwardRef(InlineAlertBase);
InlineAlert.displayName = "InlineAlert";
