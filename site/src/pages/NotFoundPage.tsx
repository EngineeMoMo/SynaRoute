import { Link } from "react-router-dom";
import { Home, Download } from "lucide-react";
import { Container } from "@/components/ui/Section";
import { ButtonLink } from "@/components/ui/Button";
import { useSeo } from "@/hooks/useSeo";
import { useLang, useLocalizedPath, useT } from "@/hooks/useLang";
import { docs } from "@/data/docs";

export default function NotFoundPage() {
  const t = useT();
  const lang = useLang();
  const path = useLocalizedPath();

  useSeo({ title: t("notFound.title"), description: t("notFound.desc"), lang });

  return (
    <Container className="flex min-h-[70vh] flex-col items-center justify-center py-28 text-center">
      <p className="font-mono text-6xl font-bold text-primary/40">404</p>
      <h1 className="mt-6 text-2xl font-bold tracking-tight text-text-primary sm:text-3xl">
        {t("notFound.title")}
      </h1>
      <p className="mt-3 max-w-md text-base leading-relaxed text-text-secondary">{t("notFound.desc")}</p>

      <div className="mt-8 flex flex-col gap-3 sm:flex-row">
        <ButtonLink to={path("")} size="lg">
          <Home size={17} aria-hidden="true" />
          {t("common.backHome")}
        </ButtonLink>
        <ButtonLink to={path("download")} variant="outline" size="lg">
          <Download size={17} aria-hidden="true" />
          {t("common.download")}
        </ButtonLink>
      </div>

      <nav className="mt-10" aria-label={t("docs.title")}>
        <p className="text-xs uppercase tracking-wide text-text-muted">{t("docs.title")}</p>
        <ul className="mt-3 flex flex-wrap justify-center gap-x-5 gap-y-2">
          {docs.map((d) => (
            <li key={d.slug}>
              <Link
                to={path(`docs/${d.slug}`)}
                className="text-sm text-primary underline underline-offset-4 hover:opacity-80"
              >
                {t(`${d.i18nPrefix}.title`)}
              </Link>
            </li>
          ))}
        </ul>
      </nav>
    </Container>
  );
}
