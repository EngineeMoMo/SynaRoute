import { Link } from "react-router-dom";
import { BookOpen, ArrowRight } from "lucide-react";
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
          {docs.map((doc) => (
            <li key={doc.slug}>
              <Link
                to={path(`docs/${doc.slug}`)}
                className="group flex items-start gap-4 rounded-card border border-border bg-surface p-6 shadow-card transition-shadow hover:shadow-card-hover"
              >
                <span className="inline-flex h-10 w-10 shrink-0 items-center justify-center rounded-control bg-primary/12 text-primary">
                  <BookOpen size={19} aria-hidden="true" />
                </span>
                <span className="min-w-0 flex-1">
                  <span className="flex items-center gap-2 text-base font-semibold text-text-primary">
                    {t(`${doc.i18nPrefix}.title`)}
                    <ArrowRight
                      size={16}
                      aria-hidden="true"
                      className="text-text-muted transition-transform duration-250 group-hover:translate-x-0.5"
                    />
                  </span>
                  <span className="mt-1.5 block text-sm leading-relaxed text-text-secondary">
                    {t(`${doc.i18nPrefix}.desc`)}
                  </span>
                </span>
              </Link>
            </li>
          ))}
        </ul>
      </div>
    </Container>
  );
}
