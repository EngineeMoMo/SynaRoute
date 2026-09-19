import { cn } from "@/lib/utils";
import { useT } from "@/lib/useT";
import { QuickToggles } from "@/components/QuickToggles";
import { Kbd } from "@/components/ui/Kbd";
import { useStore } from "@/store";
import type { CategoryType } from "@/types";
import {
  Brain,
  ScrollText,
  Settings,
  Building2,
  UserRound,
  Gauge,
  History,
  Waypoints,
  Search,
  type LucideIcon,
} from "lucide-react";
import { CATEGORY_ICONS } from "@/lib/categoryIcons";

export type NavKey =
  | CategoryType
  | "brain"
  | "sessions"
  | "logs"
  | "usage"
  | "vendors"
  | "settings"
  | "about";

interface NavItem {
  key: NavKey;
  tKey: string;
  icon: LucideIcon;
  group: "category" | "feature" | "system";
}

const NAV: NavItem[] = [
  { key: "claude-cli", tKey: "nav.claude-cli", icon: CATEGORY_ICONS["claude-cli"], group: "category" },
  { key: "claude-desktop", tKey: "nav.claude-desktop", icon: CATEGORY_ICONS["claude-desktop"], group: "category" },
  { key: "codex", tKey: "nav.codex", icon: CATEGORY_ICONS.codex, group: "category" },
  { key: "brain", tKey: "nav.brain", icon: Brain, group: "feature" },
  // Codex 专属：本地历史对话（每条自带 provider，与当前不一致时打开会走错上游）
  { key: "sessions", tKey: "nav.sessions", icon: History, group: "feature" },
  { key: "logs", tKey: "nav.logs", icon: ScrollText, group: "feature" },
  { key: "usage", tKey: "nav.usage", icon: Gauge, group: "feature" },
  { key: "vendors", tKey: "nav.vendors", icon: Building2, group: "system" },
  { key: "settings", tKey: "nav.settings", icon: Settings, group: "system" },
  { key: "about", tKey: "nav.about", icon: UserRound, group: "system" },
];

interface SidebarProps {
  active: NavKey;
  onSelect: (key: NavKey) => void;
  onOpenPalette: () => void;
}

export function Sidebar({ active, onSelect, onOpenPalette }: SidebarProps) {
  const t = useT();
  const updateCheck = useStore((s) => s.updateCheck);
  const hasUpdate = updateCheck?.status === "available";
  const updateVer = updateCheck?.version;

  const renderGroup = (group: NavItem["group"], title?: string) => (
    <div className="mb-5">
      {title && (
        <div className="sr-section-label px-3 pb-1.5">{title}</div>
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
              "relative flex w-full items-center gap-2.5 rounded-control px-3 py-2 text-sm transition-[color] duration-150 active:scale-[0.99]",
              isActive
                ? "font-medium text-primary"
                : "text-text-secondary hover:bg-surface-hover hover:text-text-primary"
            )}
          >
            {/* 选中态的背景条是一个**独立元素**，只在选中项上渲染，并带
                `view-transition-name: nav-pill`。于是切页时浏览器把它当成同一个元素
                的前后两态，**自己**把它从旧行平滑移动到新行 —— 不需要 JS 算坐标、
                不需要 FLIP 库、不需要测量 DOM。

                🔴 为什么不直接把名字挂在 <button> 上：那样整个按钮（含图标与文字）
                都成了共享元素，两个不同的标签会在移动过程中交叉淡化，读起来像重影。
                只让背景条参与过渡，文字各自淡入淡出，视觉上就是「滑块滑过去」。 */}
            {isActive && (
              <span
                aria-hidden
                className="absolute inset-0 rounded-control border border-primary/20 bg-primary/12"
                style={{ viewTransitionName: "nav-pill" }}
              />
            )}
            <span className="relative shrink-0">
              <Icon size={16} />
              {showDot && (
                <span
                  className="absolute -right-0.5 -top-0.5 h-2 w-2 rounded-full bg-primary ring-2 ring-surface"
                  aria-hidden
                />
              )}
            </span>
            <span className="relative truncate">{t(item.tKey)}</span>
            {showDot && (
              <span className="ml-auto shrink-0 rounded-full border border-primary/20 bg-primary/12 px-1.5 py-0.5 text-[10px] font-medium text-primary">
                {updateVer ? `v${updateVer}` : t("settings.updateBadge")}
              </span>
            )}
          </button>
        );
      })}
    </div>
  );

  return (
    <aside className="flex h-full w-56 shrink-0 flex-col border-r border-border/80 bg-surface/85 backdrop-blur-xl">
      {/* Logo 区 */}
      <div className="flex items-center gap-2 border-b border-border/80 px-4 py-4">
        {/*
          这里原先挂着一个「有新版本」角标（ArrowUpCircle）。已移除：它与 Logo 图形挤在同一个
          32px 方块里，实测用户看不见。更新提示改由顶部整宽横幅承担（见 UpdateBanner），
          常驻入口仍保留在下方「设置」导航项的圆点 + 版本徽章上。
        */}
        <div className="flex h-8 w-8 shrink-0 items-center justify-center rounded-control bg-gradient-to-br from-primary to-primary-deep text-primary-foreground shadow-sm shadow-primary/25">
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

      <nav className="flex-1 overflow-y-auto px-2.5 py-3">
        {/*
          命令面板入口（UX#7）。纯快捷键的功能等于没做 —— 没人会去猜一个没有任何视觉线索的
          Ctrl+K。这一条同时承担「告诉用户有这个功能」和「鼠标用户也能用」两件事，
          右侧的 kbd 片是让用户学会快捷键、之后就不必再来点它。
        */}
        <button
          onClick={onOpenPalette}
          className="mb-4 flex w-full items-center gap-2.5 rounded-control border border-transparent px-3 py-2 text-sm text-text-secondary transition-all duration-150 hover:border-border hover:bg-surface-hover hover:text-text-primary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/50"
        >
          <Search size={16} className="shrink-0" />
          <span className="truncate">{t("palette.launcher")}</span>
          <Kbd className="ml-auto">Ctrl K</Kbd>
        </button>

        {renderGroup("category", t("sidebar.groupTools"))}
        {renderGroup("feature", t("sidebar.groupFeature"))}
        {renderGroup("system", t("sidebar.groupSystem"))}
      </nav>
    </aside>
  );
}
