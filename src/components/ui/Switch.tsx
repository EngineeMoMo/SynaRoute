import { cn } from "@/lib/utils";

interface SwitchProps {
  checked: boolean;
  onCheckedChange: (checked: boolean) => void;
  disabled?: boolean;
  "aria-label"?: string;
}

/** 轻量开关（无第三方依赖），用于 Key 启用、聚合开关等 */
export function Switch({
  checked,
  onCheckedChange,
  disabled,
  ...rest
}: SwitchProps) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      disabled={disabled}
      onClick={() => !disabled && onCheckedChange(!checked)}
      className={cn(
        "relative inline-flex h-5 w-9 shrink-0 cursor-pointer items-center rounded-full border border-transparent transition-all duration-150 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-1 focus-visible:ring-offset-background disabled:cursor-not-allowed disabled:opacity-50",
        checked
          ? "border-route bg-route shadow-sm shadow-route/25"
          : "border-border-strong/70 bg-surface-hover hover:border-border-strong"
      )}
      {...rest}
    >
      <span
        className={cn(
          "inline-block h-4 w-4 transform rounded-full bg-white shadow-md shadow-black/10 transition-transform duration-150",
          checked ? "translate-x-4" : "translate-x-0.5"
        )}
      />
    </button>
  );
}
