import { Link } from "react-router-dom";
import { Section, SectionTitle } from "@/components/ui/Section";
import { useLocalizedPath, useT } from "@/hooks/useLang";

export function RoutingCapabilities() {
  const t = useT();
  const to = useLocalizedPath();
  return <Section id="routing-policies">
    <SectionTitle title={t("routingSite.title")} subtitle={t("routingSite.intro")} />
    <dl className="mt-8 divide-y divide-border">
      {(["overview", "scenes", "affinity", "intent", "privacy"] as const).map((item) => <div key={item} className="grid gap-2 py-5 sm:grid-cols-[12rem_minmax(0,1fr)] sm:gap-6">
        <dt className="font-semibold text-text-primary">{t(`routingSite.${item}`)}</dt>
        <dd className="min-w-0 text-sm leading-relaxed text-text-secondary">
          <p className="max-w-3xl">{t(`routingSite.${item}Body`)}</p>
          <details className="mt-3">
            <summary className="cursor-pointer font-medium text-primary">{t("routingSite.showExample")}</summary>
            <figure className="mt-3">
              <a href={`/screenshots/routing-${item}.jpg`} target="_blank" rel="noopener noreferrer" aria-label={t(`routingSite.${item}`)}>
                <img src={`/screenshots/routing-${item}.jpg`} alt={t(`routingSite.${item}`)} loading="lazy" className="h-auto w-full rounded-control border border-border" />
              </a>
              <figcaption className="mt-2 text-xs">{t("routingSite.exampleNote")}</figcaption>
            </figure>
          </details>
        </dd>
      </div>)}
    </dl>
    <Link to={to("docs/cli")} className="mt-6 inline-block rounded-control font-medium text-primary underline underline-offset-4 focus-visible:outline focus-visible:outline-2 focus-visible:outline-primary">{t("routingSite.guide")}</Link>
  </Section>;
}
