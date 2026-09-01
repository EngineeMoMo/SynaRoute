import { Section, SectionTitle, Reveal } from "@/components/ui/Section";
import { steps } from "@/data/features";
import { useT } from "@/hooks/useLang";

export function GettingStarted() {
  const t = useT();

  return (
    // tight 节奏：这一段是「下载完之后怎么用」，与上面的下载区是同一件事的延续
    <Section id="getting-started" rhythm="tight">
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

      {/* 「照常使用就行」+ cc-switch 迁移路径 + CLI 版本闸门：
          三段补充信息，不带序号、不在 `data/features.ts` 里，收在 steps 列表尾部。
          间距从步骤的 pb-9 拉到 mt-12（约 48px → 约 48px 留白 + 步骤的 pb-9 = 84px），
          用空间分隔「有序的步骤」与「其余值得一提的」。 */}
      <div className="mx-auto mt-12 max-w-3xl space-y-8 border-t border-border pt-10">
        <Reveal delay={steps.length * 70}>
          <div>
            <h3 className="text-base font-semibold text-text-primary">{t("steps.afterStart")}</h3>
            <p className="mt-1.5 text-sm leading-relaxed text-text-secondary">{t("steps.afterStartDesc")}</p>
          </div>
        </Reveal>

        <Reveal delay={(steps.length + 1) * 70}>
          <div>
            <h3 className="text-base font-semibold text-text-primary">{t("steps.fromCcSwitch")}</h3>
            <p className="mt-1.5 text-sm leading-relaxed text-text-secondary">{t("steps.fromCcSwitchDesc")}</p>
          </div>
        </Reveal>

        <Reveal delay={(steps.length + 2) * 70}>
          <div>
            <h3 className="text-base font-semibold text-text-primary">{t("steps.cliMinVersion")}</h3>
            <p className="mt-1.5 text-sm leading-relaxed text-text-secondary">{t("steps.cliMinVersionDesc")}</p>
          </div>
        </Reveal>
      </div>
    </Section>
  );
}
