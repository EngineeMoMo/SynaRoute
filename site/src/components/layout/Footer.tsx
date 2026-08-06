import { Link, useLocation, useNavigate } from "react-router-dom";
import { Github, Mail } from "lucide-react";
import { Logo } from "@/components/ui/Logo";
import { Container } from "@/components/ui/Section";
import { footerNav } from "@/data/nav";
import { siteConfig } from "@/config/site";
import { useLocalizedPath, useT } from "@/hooks/useLang";
import { externalLinkProps } from "@/lib/utils";

export function Footer() {
  const t = useT();
  const path = useLocalizedPath();
  const navigate = useNavigate();
  const location = useLocation();
  const homePath = path("");

  // 与顶栏同样的锚点处理：不在首页时先回首页
  function goToHash(hash: string) {
    if (location.pathname === homePath) {
      document.getElementById(hash)?.scrollIntoView({ behavior: "smooth", block: "start" });
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

          {footerNav.map((group) => (
            <div key={group.titleKey}>
              <h3 className="text-sm font-semibold text-text-primary">{t(group.titleKey)}</h3>
              <ul className="mt-4 space-y-2.5">
                {group.items.map((item) => (
                  <li key={item.id}>
                    {item.hash ? (
                      <button
                        type="button"
                        onClick={() => goToHash(item.hash!)}
                        className="text-sm text-text-secondary transition-colors hover:text-text-primary"
                      >
                        {t(item.labelKey)}
                      </button>
                    ) : (
                      <Link
                        to={path(item.path!)}
                        className="text-sm text-text-secondary transition-colors hover:text-text-primary"
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

        <div className="mt-10 flex flex-col gap-4 border-t border-border pt-6 sm:flex-row sm:items-center sm:justify-between">
          <p className="text-xs text-text-secondary">
            {t("footer.copyright", { year: new Date().getFullYear() })}
          </p>

          <div className="flex flex-wrap items-center gap-x-5 gap-y-2">
            <a
              href={siteConfig.github.url}
              {...externalLinkProps}
              className="inline-flex items-center gap-1.5 text-xs text-text-secondary transition-colors hover:text-text-primary"
            >
              <Github size={14} aria-hidden="true" />
              GitHub
            </a>
            <a
              href={`mailto:${siteConfig.author.email}`}
              className="inline-flex items-center gap-1.5 text-xs text-text-secondary transition-colors hover:text-text-primary"
            >
              <Mail size={14} aria-hidden="true" />
              {siteConfig.author.email}
            </a>
            <a
              href={siteConfig.author.url}
              {...externalLinkProps}
              className="inline-flex items-center gap-1.5 text-xs text-text-secondary transition-colors hover:text-text-primary"
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
