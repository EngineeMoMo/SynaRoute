import { Section, SectionTitle, Reveal } from "@/components/ui/Section";
import { features } from "@/data/features";
import { useT } from "@/hooks/useLang";
import { cn } from "@/lib/utils";

/**
 * 核心功能区，Bento 布局。
 *
 * 刻意不让九个功能长成一模一样的卡片（模板第 6.4 节）：
 * 前两个是主卖点，各占半行、字号更大、有详细描述；其余七个走三列小格。
 * 这样扫一眼就知道哪两件事最重要。
 */
export function Features() {
  const t = useT();
  const wide = features.filter((f) => f.span === "half");
  const small = features.filter((f) => f.span === "third");

  return (
    <Section id="features">
      <SectionTitle title={t("features.title")} subtitle={t("features.subtitle")} />

      <div className="mt-12 grid gap-5 md:grid-cols-2">
        {wide.map((f, i) => {
          const Icon = f.icon;
          return (
            <Reveal key={f.id} delay={i * 70} className="h-full">
              <article className="flex h-full flex-col rounded-card border border-border bg-surface p-7 shadow-card transition-shadow hover:shadow-card-hover">
                <span className="inline-flex h-12 w-12 items-center justify-center rounded-control bg-primary/12 text-primary">
                  <Icon size={23} aria-hidden="true" />
                </span>
                <h3 className="mt-5 text-xl font-semibold text-text-primary">{t(`${f.i18nPrefix}.name`)}</h3>
                <p className="mt-2 text-[15px] font-medium leading-relaxed text-text-secondary">
                  {t(`${f.i18nPrefix}.short`)}
                </p>
                <p className="mt-4 text-sm leading-relaxed text-text-muted">{t(`${f.i18nPrefix}.desc`)}</p>
              </article>
            </Reveal>
          );
        })}
      </div>

      <div className="mt-5 grid gap-5 sm:grid-cols-2 lg:grid-cols-3">
        {small.map((f, i) => {
          const Icon = f.icon;
          return (
            <Reveal key={f.id} delay={i * 50} className="h-full">
              <article
                className={cn(
                  "flex h-full flex-col rounded-card border border-border bg-surface p-6 shadow-card transition-shadow hover:shadow-card-hover"
                )}
              >
                <div className="flex items-center gap-3">
                  <span className="inline-flex h-9 w-9 shrink-0 items-center justify-center rounded-control bg-surface-hover text-text-secondary">
                    <Icon size={18} aria-hidden="true" />
                  </span>
                  <h3 className="text-base font-semibold text-text-primary">{t(`${f.i18nPrefix}.name`)}</h3>
                </div>
                <p className="mt-3 text-sm font-medium leading-relaxed text-text-secondary">
                  {t(`${f.i18nPrefix}.short`)}
                </p>
                <p className="mt-2.5 text-xs leading-relaxed text-text-muted">{t(`${f.i18nPrefix}.desc`)}</p>
              </article>
            </Reveal>
          );
        })}
      </div>
    </Section>
  );
}
