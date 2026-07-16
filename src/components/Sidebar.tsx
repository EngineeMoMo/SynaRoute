import { cn } from "@/lib/utils";
import { useT } from "@/lib/useT";
import { QuickToggles } from "@/components/QuickToggles";
import type { CategoryType } from "@/types";
import {
  Terminal,
  MonitorSmartphone,
  Code2,
  Brain,
  ScrollText,
  Settings,
  Building2,
  Waypoints,
  type LucideIcon,
} from "lucide-react";

export type NavKey = CategoryType | "brain" | "logs" | "vendors" | "settings";

interface NavItem {
  key: NavKey;
  tKey: string;
  icon: LucideIcon;
  group: "category" | "feature" | "system";
}

const NAV: NavItem[] = [
  { key: "claude-cli", tKey: "nav.claude-cli", icon: Terminal, group: "category" },
  { key: "claude-desktop", tKey: "nav.claude-desktop", icon: MonitorSmartphone, group: "category" },
  { key: "codex", tKey: "nav.codex", icon: Code2, group: "category" },
  { key: "brain", tKey: "nav.brain", icon: Brain, group: "feature" },
  { key: "logs", tKey: "nav.logs", icon: ScrollText, group: "feature" },
  { key: "vendors", tKey: "nav.vendors", icon: Building2, group: "system" },
  { key: "settings", tKey: "nav.settings", icon: Settings, group: "system" },
];

interface SidebarProps {
  active: NavKey;
  onSelect: (key: NavKey) => void;
}

export function Sidebar({ active, onSelect }: SidebarProps) {
  const t = useT();
  const renderGroup = (group: NavItem["group"], title?: string) => (
    <div className="mb-4">
      {title && (
        <div className="px-3 pb-1 text-[11px] font-medium uppercase tracking-wider text-text-muted">
          {title}
        </div>
      )}
      {NAV.filter((n) => n.group === group).map((item) => {
        const Icon = item.icon;
        const isActive = active === item.key;
        return (
          <button
            key={item.key}
            onClick={() => onSelect(item.key)}
            className={cn(
              "flex w-full items-center gap-2.5 rounded-control px-3 py-2 text-sm transition-colors",
              isActive
                ? "bg-primary/12 font-medium text-primary"
                : "text-text-secondary hover:bg-surface-hover hover:text-text-primary"
            )}
          >
            <Icon size={16} className="shrink-0" />
            <span className="truncate">{t(item.tKey)}</span>
          </button>
        );
      })}
    </div>
  );

  return (
    <aside className="flex h-full w-56 shrink-0 flex-col border-r border-border bg-surface">
      {/* Logo 区 */}
      <div className="flex items-center gap-2 px-4 py-4">
        <div className="flex h-8 w-8 items-center justify-center rounded-control bg-primary text-primary-foreground">
          <Waypoints size={18} />
        </div>
        <div className="min-w-0 leading-tight">
          <div className="text-sm font-semibold text-text-primary">SynaRoute</div>
          <div className="truncate text-[11px] text-text-muted">{t("app.tagline")}</div>
        </div>
        {/* 语言 / 主题快捷切换：无需进设置（FR-快捷切换） */}
        <div className="ml-auto shrink-0">
          <QuickToggles />
        </div>
      </div>

      <nav className="flex-1 overflow-y-auto px-2 py-2">
        {renderGroup("category", t("sidebar.groupTools"))}
        {renderGroup("feature", t("sidebar.groupFeature"))}
        {renderGroup("system", t("sidebar.groupSystem"))}
      </nav>
    </aside>
  );
}
