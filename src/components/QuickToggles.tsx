import { useStore } from "@/store";
import { useT } from "@/lib/useT";
import type { Lang } from "@/lib/i18n";
import type { ThemePref } from "@/types";
import { Sun, Moon, MonitorCog, Languages } from "lucide-react";

/** 主题循环顺序：浅色 → 深色 → 跟随系统 */
const THEME_ORDER: ThemePref[] = ["light", "dark", "system"];
const THEME_ICON: Record<ThemePref, typeof Sun> = {
  light: Sun,
  dark: Moon,
  system: MonitorCog,
};

/**
 * 顶部快捷切换：语言 + 主题，一键切换无需进设置（放在侧栏顶部，全局可见）。
 * - 语言：中 ⇄ EN 直接切换
 * - 主题：浅色 → 深色 → 跟随系统 循环
 */
export function QuickToggles() {
  const theme = useStore((s) => s.theme);
  const setTheme = useStore((s) => s.setTheme);
  const lang = useStore((s) => s.lang);
  const setLang = useStore((s) => s.setLang);
  const t = useT();

  const cycleTheme = () => {
    const next = THEME_ORDER[(THEME_ORDER.indexOf(theme) + 1) % THEME_ORDER.length];
    setTheme(next);
  };

  const toggleLang = () => {
    const next: Lang = lang === "zh" ? "en" : "zh";
    setLang(next);
  };

  const ThemeIcon = THEME_ICON[theme];
  const themeLabel = t(`settings.theme.${theme}`);

  const btnCls =
    "flex h-7 w-7 items-center justify-center rounded-control text-text-muted transition-colors hover:bg-surface-hover hover:text-text-primary";

  return (
    <div className="flex items-center gap-0.5">
      <button
        onClick={toggleLang}
        className={btnCls}
        title={`${t("settings.language")}: ${lang === "zh" ? "中文" : "English"}`}
        aria-label={t("settings.language")}
      >
        <Languages size={15} />
      </button>
      <button
        onClick={cycleTheme}
        className={btnCls}
        title={`${t("settings.appearance")}: ${themeLabel}`}
        aria-label={t("settings.appearance")}
      >
        <ThemeIcon size={15} />
      </button>
    </div>
  );
}
