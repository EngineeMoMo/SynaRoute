import { Link } from "react-router-dom";
import { ArrowRight } from "lucide-react";
import { Container } from "@/components/ui/Section";
import { docs } from "@/data/docs";
import { useSeo } from "@/hooks/useSeo";
import { useLang, useLocalizedPath, useT } from "@/hooks/useLang";

export default function DocsIndexPage() {
  const t = useT();
  const lang = useLang();
  const path = useLocalizedPath();

  useSeo({
    title: t("docs.title"),
    description: t("docs.subtitle"),
    lang,
  });

  return (
    <Container className="py-28 sm:py-32">
      <div className="mx-auto max-w-3xl">
        <header>
          <h1 className="text-3xl font-bold tracking-tight text-text-primary sm:text-4xl">{t("docs.title")}</h1>
          <p className="mt-4 text-base text-text-secondary">{t("docs.subtitle")}</p>
        </header>

        <ul className="mt-10 space-y-4">
          {docs.map((doc) => {
            const Icon = doc.icon;
            return (
              <li key={doc.slug}>
                <Link
                  to={path(`docs/${doc.slug}`)}
                  // hover 两条通道（阴影 + 描边），深色下阴影本来就弱
                  className="group flex items-start gap-4 rounded-card border border-border bg-surface p-6 shadow-card transition-all hover:border-border-strong hover:shadow-card-hover"
                >
                  <span className="inline-flex h-10 w-10 shrink-0 items-center justify-center rounded-control bg-primary/12 text-primary">
                    <Icon size={20} aria-hidden="true" />
                  </span>
                  <span className="min-w-0 flex-1">
                    {/* 箭头靠 `ml-auto` 钉在右端。原先它紧跟标题文字，于是三张卡的
                        箭头横坐标随标题长短各不相同，排成一列锯齿；而卡片右侧
                        近一半是空的，那个位置本来就该给它。 */}
                    <span className="flex items-center gap-2 text-base font-semibold text-text-primary">
                      {t(`${doc.i18nPrefix}.title`)}
                      <ArrowRight
                        size={16}
                        aria-hidden="true"
                        className="ml-auto shrink-0 text-text-secondary transition-transform duration-250 group-hover:translate-x-0.5"
                      />
                    </span>
                    <span className="mt-1.5 block text-sm leading-relaxed text-text-secondary">
                      {t(`${doc.i18nPrefix}.desc`)}
                    </span>
                  </span>
                </Link>
              </li>
            );
          })}
        </ul>
      </div>
    </Container>
  );
}
