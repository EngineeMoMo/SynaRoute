import { useEffect, useState } from "react";
import { Link, useLocation, useNavigate } from "react-router-dom";
import { Github, Menu, X, Download } from "lucide-react";
import { Logo } from "@/components/ui/Logo";
import { Button, ButtonLink } from "@/components/ui/Button";
import { ThemeToggle, LangToggle } from "@/components/ui/Toggles";
import { Container } from "@/components/ui/Section";
import { navItems, type NavItem } from "@/data/nav";
import { siteConfig } from "@/config/site";
import { useLang, useLocalizedPath, useT } from "@/hooks/useLang";
import { cn, externalLinkProps } from "@/lib/utils";

export function Header() {
  const t = useT();
  const lang = useLang();
  const path = useLocalizedPath();
  const location = useLocation();
  const navigate = useNavigate();
  const [scrolled, setScrolled] = useState(false);
  const [menuOpen, setMenuOpen] = useState(false);

  const homePath = path("");
  const onHome = location.pathname === homePath || location.pathname === `${homePath}/`;

  // 滚动后给顶栏加背景模糊与描边，首屏保持通透
  useEffect(() => {
    const onScroll = () => setScrolled(window.scrollY > 8);
    onScroll();
    window.addEventListener("scroll", onScroll, { passive: true });
    return () => window.removeEventListener("scroll", onScroll);
  }, []);

  // 路由一变就收起移动端菜单，否则跳过去了菜单还盖在上面
  useEffect(() => setMenuOpen(false), [location.pathname]);

  // 菜单打开时锁住背景滚动，并支持 Esc 关闭
  useEffect(() => {
    if (!menuOpen) return;
    const prev = document.body.style.overflow;
    document.body.style.overflow = "hidden";
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && setMenuOpen(false);
    document.addEventListener("keydown", onKey);
    return () => {
      document.body.style.overflow = prev;
      document.removeEventListener("keydown", onKey);
    };
  }, [menuOpen]);

  /**
   * 锚点导航：在首页就平滑滚动，不在首页就先回首页再滚。
   * 直接用 `<a href="#features">` 在子页面上会跳到子页面自己的 #features（不存在），
   * 所以这里必须接管。
   */
  function goToHash(hash: string) {
    setMenuOpen(false);
    if (onHome) {
      document.getElementById(hash)?.scrollIntoView({ behavior: "smooth", block: "start" });
      // 让地址栏反映当前位置，但不产生历史记录堆积
      window.history.replaceState(null, "", `${homePath}#${hash}`);
    } else {
      navigate(`${homePath}#${hash}`);
    }
  }

  function renderNavItem(item: NavItem, mobile = false) {
    const cls = mobile
      ? "flex w-full items-center rounded-control px-3 py-3 text-base text-text-primary transition-colors hover:bg-surface-hover"
      : "rounded-control px-3 py-2 text-sm text-text-secondary transition-colors hover:bg-surface-hover hover:text-text-primary";

    if (item.hash) {
      return (
        <button key={item.id} type="button" onClick={() => goToHash(item.hash!)} className={cls}>
          {t(item.labelKey)}
        </button>
      );
    }
    const to = path(item.path!);
    const active = location.pathname.startsWith(to);
    return (
      <Link
        key={item.id}
        to={to}
        className={cn(cls, active && "text-text-primary", active && !mobile && "bg-surface-hover")}
        aria-current={active ? "page" : undefined}
      >
        {t(item.labelKey)}
      </Link>
    );
  }

  return (
    <header
      className={cn(
        "fixed inset-x-0 top-0 z-50 transition-all duration-250",
        scrolled ? "border-b border-border bg-background/80 backdrop-blur-md" : "border-b border-transparent"
      )}
    >
      <Container>
        <div className="flex h-16 items-center justify-between gap-3">
          <Link to={homePath} aria-label={siteConfig.name} className="rounded-control">
            <Logo size={30} />
          </Link>

          <nav aria-label={t("nav.home")} className="hidden items-center gap-0.5 lg:flex">
            {navItems.map((i) => renderNavItem(i))}
          </nav>

          <div className="flex items-center gap-1">
            <LangToggle />
            <ThemeToggle />

            {/* GitHub 与下载比普通导航项更突出（模板第 6.1 节要求） */}
            <a
              href={siteConfig.github.url}
              {...externalLinkProps}
              aria-label={t("nav.github")}
              title={t("nav.github")}
              className="hidden h-10 w-10 items-center justify-center rounded-control text-text-secondary transition-colors hover:bg-surface-hover hover:text-text-primary sm:inline-flex"
            >
              <Github size={18} aria-hidden="true" />
            </a>
            <ButtonLink to={path("download")} size="sm" className="hidden h-10 px-4 text-sm sm:inline-flex">
              <Download size={16} aria-hidden="true" />
              {t("nav.download")}
            </ButtonLink>

            <Button
              variant="ghost"
              size="icon"
              className="lg:hidden"
              aria-label={menuOpen ? t("common.closeMenu") : t("common.openMenu")}
              aria-expanded={menuOpen}
              onClick={() => setMenuOpen((v) => !v)}
            >
              {menuOpen ? <X size={20} aria-hidden="true" /> : <Menu size={20} aria-hidden="true" />}
            </Button>
          </div>
        </div>
      </Container>

      {menuOpen && (
        <div className="border-t border-border bg-background lg:hidden">
          <Container className="py-3">
            <nav className="flex flex-col gap-0.5" aria-label={t("common.openMenu")}>
              {navItems.map((i) => renderNavItem(i, true))}
              <a
                href={siteConfig.github.url}
                {...externalLinkProps}
                className="flex w-full items-center gap-2 rounded-control px-3 py-3 text-base text-text-primary transition-colors hover:bg-surface-hover"
              >
                <Github size={18} aria-hidden="true" />
                {t("nav.github")}
              </a>
            </nav>
            <ButtonLink to={path("download")} size="lg" className="mt-3 w-full">
              <Download size={18} aria-hidden="true" />
              {t("common.download")}
            </ButtonLink>
          </Container>
        </div>
      )}

      {/* 语言标注给读屏用：中英切换时顶栏内容语言也随之变化 */}
      <span className="sr-only" lang={lang === "zh" ? "zh-CN" : "en"} />
    </header>
  );
}
