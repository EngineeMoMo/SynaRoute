import { Link, useLocation, useNavigate } from "react-router-dom";
import { Github, Mail } from "lucide-react";
import { Logo } from "@/components/ui/Logo";
import { Container } from "@/components/ui/Section";
import { footerNav } from "@/data/nav";
import { siteConfig } from "@/config/site";
import { useLocalizedPath, useT } from "@/hooks/useLang";
import { externalLinkProps } from "@/lib/utils";
import { scrollToId } from "@/lib/motion";

export function Footer() {
  const t = useT();
  const path = useLocalizedPath();
  const navigate = useNavigate();
  const location = useLocation();
  const homePath = path("");

  // 与顶栏同样的锚点处理：不在首页时先回首页
  function goToHash(hash: string) {
    if (location.pathname === homePath) {
      scrollToId(hash);
    } else {
      navigate(`${homePath}#${hash}`);
    }
  }

  return (
    <footer className="border-t border-border bg-surface">
      <Container className="py-12 lg:py-16">
        <div className="grid gap-10 lg:grid-cols-[1.6fr_repeat(3,1fr)]">
          <div>
            <Link to={homePath} aria-label={siteConfig.name} className="inline-block rounded-control">
              <Logo size={30} />
            </Link>
            <p className="mt-4 max-w-sm text-sm leading-relaxed text-text-secondary">{t("footer.tagline")}</p>
            <p className="mt-3 text-xs leading-relaxed text-text-secondary">{t("footer.sourceNote")}</p>
          </div>

          {/*
            三个链接分组在 <1024px 时自己排成一个横向网格。
            原先分栏要等到 `lg`，于是 768px 那一档整个页脚是单列：8 条链接
            纵向拉了约 580px，右边 620px 全空。
            `lg:contents` 让这层包装在桌面端从布局里消失，三个分组回到外层网格
            的直接子项 —— 1024px 以上的排布一个像素都不变。
          */}
          <div className="grid grid-cols-2 gap-x-6 gap-y-8 sm:grid-cols-3 lg:contents">
            {footerNav.map((group) => (
              <div key={group.titleKey}>
                <h3 className="text-sm font-semibold text-text-primary">{t(group.titleKey)}</h3>
                {/*
                  🔴 `min-h-11` 是移动端触控目标的硬要求（44px），但它在桌面端也生效 ——
                  于是一行 14px 的文字占 54px 高，三列的高度差到 108px，页脚看起来空得
                  像没写完。`lg:min-h-0 lg:py-1` 只在桌面端解除，触控档位一字不动。
                */}
                <ul className="mt-4 space-y-2.5 lg:space-y-1.5">
                  {group.items.map((item) => (
                    <li key={item.id}>
                      {item.hash ? (
                        <button
                          type="button"
                          onClick={() => goToHash(item.hash!)}
                          className="inline-flex min-h-11 items-center text-sm text-text-secondary transition-colors hover:text-text-primary lg:min-h-0 lg:py-1"
                        >
                          {t(item.labelKey)}
                        </button>
                      ) : (
                        <Link
                          to={path(item.path!)}
                          className="inline-flex min-h-11 items-center text-sm text-text-secondary transition-colors hover:text-text-primary lg:min-h-0 lg:py-1"
                        >
                          {t(item.labelKey)}
                        </Link>
                      )}
                    </li>
                  ))}
                </ul>
              </div>
            ))}
          </div>
        </div>

        <div className="mt-10 flex flex-col gap-4 border-t border-border pt-6 sm:flex-row sm:items-center sm:justify-between">
          <p className="text-xs text-text-secondary">
            {t("footer.copyright", { year: new Date().getFullYear() })}
          </p>

          <div className="flex flex-wrap items-center gap-x-5 gap-y-2">
            <a
              href={siteConfig.github.url}
              {...externalLinkProps}
              className="inline-flex min-h-11 items-center gap-1.5 text-xs text-text-secondary transition-colors hover:text-text-primary lg:min-h-0 lg:py-1"
            >
              <Github size={14} aria-hidden="true" />
              GitHub
            </a>
            <a
              href={`mailto:${siteConfig.author.email}`}
              className="inline-flex min-h-11 items-center gap-1.5 text-xs text-text-secondary transition-colors hover:text-text-primary lg:min-h-0 lg:py-1"
            >
              <Mail size={14} aria-hidden="true" />
              {siteConfig.author.email}
            </a>
            <a
              href={siteConfig.author.url}
              {...externalLinkProps}
              className="inline-flex min-h-11 items-center gap-1.5 text-xs text-text-secondary transition-colors hover:text-text-primary lg:min-h-0 lg:py-1"
            >
              <Github size={14} aria-hidden="true" />
              {t("footer.authorSite", { name: siteConfig.author.name })}
            </a>
          </div>
        </div>
      </Container>
    </footer>
  );
}
