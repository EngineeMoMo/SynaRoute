import { cn } from "@/lib/utils";
import { useT } from "@/lib/useT";
import { QuickToggles } from "@/components/QuickToggles";
import { useStore } from "@/store";
import type { CategoryType } from "@/types";
import {
  Terminal,
  MonitorSmartphone,
  Code2,
  Brain,
  ScrollText,
  Settings,
  Building2,
  UserRound,
  Waypoints,
  type LucideIcon,
} from "lucide-react";

export type NavKey = CategoryType | "brain" | "logs" | "vendors" | "settings" | "about";

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
  { key: "about", tKey: "nav.about", icon: UserRound, group: "system" },
];

interface SidebarProps {
  active: NavKey;
  onSelect: (key: NavKey) => void;
}

export function Sidebar({ active, onSelect }: SidebarProps) {
  const t = useT();
  const updateCheck = useStore((s) => s.updateCheck);
  const hasUpdate = updateCheck?.status === "available";
  const updateVer = updateCheck?.version;

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
        const showDot = item.key === "settings" && hasUpdate;
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
            <span className="relative shrink-0">
              <Icon size={16} />
              {showDot && (
                <span
                  className="absolute -right-0.5 -top-0.5 h-2 w-2 rounded-full bg-primary ring-2 ring-surface"
                  aria-hidden
                />
              )}
            </span>
            <span className="truncate">{t(item.tKey)}</span>
            {showDot && (
              <span className="ml-auto shrink-0 rounded-full bg-primary/15 px-1.5 py-0.5 text-[10px] font-medium text-primary">
                {updateVer ? `v${updateVer}` : t("settings.updateBadge")}
              </span>
            )}
          </button>
        );
      })}
    </div>
  );

  return (
    <aside className="flex h-full w-56 shrink-0 flex-col border-r border-border bg-surface">
      {/* Logo 区 */}
      <div className="flex items-center gap-2 px-4 py-4">
        {/*
          这里原先挂着一个「有新版本」角标（ArrowUpCircle）。已移除：它与 Logo 图形挤在同一个
          32px 方块里，实测用户看不见。更新提示改由顶部整宽横幅承担（见 UpdateBanner），
          常驻入口仍保留在下方「设置」导航项的圆点 + 版本徽章上。
        */}
        <div className="flex h-8 w-8 shrink-0 items-center justify-center rounded-control bg-primary text-primary-foreground">
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
