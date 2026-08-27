// 设置页的「一行一个开关」行。从 SettingsPage 抽出（那边冻结在棘轮上），
// 同时**断掉一个循环依赖**：LanSection 需要它，而 SettingsPage 又要导入 LanSection ——
// React 组件在模块循环里可能在求值时还是 undefined，属于会偶发、难查的那类故障。

import type { LucideIcon } from "lucide-react";
import { Switch } from "@/components/ui/Switch";

export function ToggleRow({
  icon: Icon,
  title,
  desc,
  cost,
  checked,
  onChange,
  danger,
  disabled,
  badge,
}: {
  icon?: LucideIcon;
  title: string;
  desc: string;
  /**
   * 开启这个开关要付出的代价（UX#15）。
   *
   * 与 `desc` 分开是因为两者用途不同：`desc` 讲「它是什么」，`cost` 讲「开了会怎样」。
   * **常驻显示**（不是只在开着时才显示）——关着的时候用户正是要靠这句判断该不该开；
   * 但**开着时转成 warning 色**，因为那时这句话的作用变成了「排障完记得关掉」。
   */
  cost?: string;
  checked: boolean;
  onChange: (v: boolean) => void;
  danger?: boolean;
  disabled?: boolean;
  badge?: string;
}) {
  return (
    <div className={`flex items-start justify-between gap-4 ${disabled ? "opacity-60" : ""}`}>
      <div className="flex gap-2.5">
        {Icon && <Icon size={16} className={`mt-0.5 shrink-0 ${danger ? "text-danger" : "text-text-secondary"}`} />}
        <div>
          <div className="flex items-center gap-2 text-sm font-medium text-text-primary">
            {title}
            {badge && (
              <span className="rounded-full border border-border px-1.5 py-0.5 text-[10px] font-normal text-text-muted">
                {badge}
              </span>
            )}
          </div>
          <div className="text-xs text-text-muted">{desc}</div>
          {cost && (
            <div
              className={`mt-1 text-[11px] leading-relaxed ${checked ? "text-warning" : "text-text-muted"}`}
            >
              {cost}
            </div>
          )}
        </div>
      </div>
      <Switch checked={checked} onCheckedChange={onChange} disabled={disabled} />
    </div>
  );
}
