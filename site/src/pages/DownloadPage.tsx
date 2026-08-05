import { ShieldAlert, RefreshCw, ExternalLink } from "lucide-react";
import { Container } from "@/components/ui/Section";
import { PlatformGrid } from "@/components/DownloadUI";
import { siteConfig } from "@/config/site";
import { useSeo } from "@/hooks/useSeo";
import { useLang, useT } from "@/hooks/useLang";
import { externalLinkProps } from "@/lib/utils";

export default function DownloadPage() {
  const t = useT();
  const lang = useLang();

  useSeo({
    title: t("download.pageTitle"),
    description: t("download.subtitle"),
    lang,
  });

  return (
    <Container className="py-28 sm:py-32">
      <div className="mx-auto max-w-3xl">
        <header className="text-center">
          <h1 className="text-3xl font-bold tracking-tight text-text-primary sm:text-4xl">
            {t("download.pageTitle")}
          </h1>
          <p className="mt-4 text-base text-text-secondary">{t("download.subtitle")}</p>
        </header>

        <PlatformGrid className="mt-12" />

        <div className="mt-10 grid gap-4 sm:grid-cols-2">
          <div className="rounded-card border border-border bg-surface p-5">
            <div className="flex items-center gap-2.5">
              <ShieldAlert size={17} className="shrink-0 text-warning" aria-hidden="true" />
              <h2 className="text-sm font-semibold text-text-primary">{t("download.verifyTitle")}</h2>
            </div>
            <p className="mt-2.5 text-sm leading-relaxed text-text-secondary">{t("download.verifyDesc")}</p>
          </div>

          <div className="rounded-card border border-border bg-surface p-5">
            <div className="flex items-center gap-2.5">
              <RefreshCw size={17} className="shrink-0 text-info" aria-hidden="true" />
              <h2 className="text-sm font-semibold text-text-primary">{t("download.updateTitle")}</h2>
            </div>
            <p className="mt-2.5 text-sm leading-relaxed text-text-secondary">{t("download.updateDesc")}</p>
          </div>
        </div>

        <p className="mt-8 text-center">
          <a
            href={siteConfig.github.releases}
            {...externalLinkProps}
            className="inline-flex items-center gap-1.5 text-sm text-primary underline underline-offset-4 hover:opacity-80"
          >
            {t("download.allReleases")}
            <ExternalLink size={14} aria-hidden="true" />
          </a>
        </p>
      </div>
    </Container>
  );
}
