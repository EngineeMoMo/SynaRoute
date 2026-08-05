import { Moon, Sun, Languages, Check } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { useNavigate } from "react-router-dom";
import { LANGS, type Lang } from "@/i18n";
import { useLang, useSwitchLangPath, useT } from "@/hooks/useLang";
import { useTheme } from "@/hooks/useTheme";
import { cn } from "@/lib/utils";

/** 深浅主题切换 */
export function ThemeToggle({ className }: { className?: string }) {
  const { theme, toggle } = useTheme();
  const t = useT();
  return (
    <button
      type="button"
      onClick={toggle}
      aria-label={t("common.toggleTheme")}
      title={t("common.toggleTheme")}
      className={cn(
        "inline-flex h-10 w-10 items-center justify-center rounded-control text-text-secondary transition-colors hover:bg-surface-hover hover:text-text-primary",
        className
      )}
    >
      {theme === "dark" ? <Sun size={18} aria-hidden="true" /> : <Moon size={18} aria-hidden="true" />}
    </button>
  );
}

/**
 * 语言切换。
 *
 * 切换是路由跳转而不是改状态 —— 语言在 URL 里，跳到对应语言的同一路径，
 * 保持用户当前所在页面（在 /zh/docs/cli 切英文会到 /en/docs/cli）。
 */
export function LangToggle({ className }: { className?: string }) {
  const lang = useLang();
  const t = useT();
  const navigate = useNavigate();
  const switchPath = useSwitchLangPath();
  const [open, setOpen] = useState(false);
  const wrapRef = useRef<HTMLDivElement>(null);

  // 点外面 / 按 Esc 都要能关，否则在移动端会变成一个关不掉的浮层
  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      if (!wrapRef.current?.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  function pick(next: Lang) {
    setOpen(false);
    // 滚动位置保留：同一页面换语言，内容结构一致，不应该被弹回顶部
    navigate(switchPath(next), { preventScrollReset: true });
  }

  return (
    <div ref={wrapRef} className={cn("relative", className)}>
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        aria-label={t("common.toggleLang")}
        aria-haspopup="menu"
        aria-expanded={open}
        className="inline-flex h-10 items-center justify-center gap-1.5 rounded-control px-2.5 text-sm text-text-secondary transition-colors hover:bg-surface-hover hover:text-text-primary"
      >
        <Languages size={18} aria-hidden="true" />
        <span className="hidden sm:inline">{LANGS.find((l) => l.value === lang)?.label}</span>
      </button>

      {open && (
        <div
          role="menu"
          className="absolute right-0 top-full z-50 mt-1 min-w-[9rem] overflow-hidden rounded-control border border-border bg-surface py-1 shadow-card-hover"
        >
          {LANGS.map((l) => (
            <button
              key={l.value}
              role="menuitem"
              type="button"
              onClick={() => pick(l.value)}
              className="flex w-full items-center justify-between gap-3 px-3 py-2 text-left text-sm text-text-primary transition-colors hover:bg-surface-hover"
            >
              {l.label}
              {l.value === lang && <Check size={15} className="text-primary" aria-hidden="true" />}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
