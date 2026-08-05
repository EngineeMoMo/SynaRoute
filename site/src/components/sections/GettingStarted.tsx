import { Section, SectionTitle, Reveal } from "@/components/ui/Section";
import { steps } from "@/data/features";
import { useT } from "@/hooks/useLang";

export function GettingStarted() {
  const t = useT();

  return (
    <Section id="getting-started">
      <SectionTitle title={t("steps.title")} subtitle={t("steps.subtitle")} />

      <ol className="mx-auto mt-12 max-w-3xl">
        {steps.map((s, i) => (
          <Reveal key={s.id} delay={i * 70}>
            <li className="relative flex gap-5 pb-9 last:pb-0">
              {/* 连接线：最后一步不画，否则会有一截悬空的线 */}
              {i < steps.length - 1 && (
                <span
                  aria-hidden="true"
                  className="absolute left-[19px] top-11 h-[calc(100%-2.75rem)] w-px bg-border"
                />
              )}
              <span className="relative z-10 inline-flex h-10 w-10 shrink-0 items-center justify-center rounded-full border border-border bg-surface text-sm font-semibold text-primary">
                {i + 1}
              </span>
              <div className="pt-1.5">
                <h3 className="text-base font-semibold text-text-primary">{t(`${s.i18nPrefix}.title`)}</h3>
                <p className="mt-1.5 text-sm leading-relaxed text-text-secondary">{t(`${s.i18nPrefix}.desc`)}</p>
              </div>
            </li>
          </Reveal>
        ))}
      </ol>
    </Section>
  );
}
