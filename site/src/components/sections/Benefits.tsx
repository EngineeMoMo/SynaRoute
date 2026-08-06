import { Section, SectionTitle, Reveal } from "@/components/ui/Section";
import { benefits } from "@/data/features";
import { useT } from "@/hooks/useLang";

export function Benefits() {
  const t = useT();

  return (
    <Section id="benefits">
      <SectionTitle title={t("benefits.title")} subtitle={t("benefits.subtitle")} />

      {/* 桌面 4 列 / 平板 2 列 / 手机单列；卡片用 flex 撑满等高，避免文案长短造成参差 */}
      <div className="mt-12 grid gap-5 sm:grid-cols-2 lg:grid-cols-4">
        {benefits.map((b, i) => {
          const Icon = b.icon;
          return (
            <Reveal key={b.id} delay={i * 70} className="h-full">
              <div className="flex h-full flex-col rounded-card border border-border bg-surface p-6 shadow-card transition-shadow hover:shadow-card-hover">
                <span className="inline-flex h-11 w-11 items-center justify-center rounded-control bg-primary/12 text-primary">
                  <Icon size={21} aria-hidden="true" />
                </span>
                <h3 className="mt-4 text-base font-semibold text-text-primary">{t(`${b.i18nPrefix}.name`)}</h3>
                <p className="mt-2 text-sm leading-relaxed text-text-secondary">{t(`${b.i18nPrefix}.desc`)}</p>
                {/* 补充说明比上面一句更次要，但仍要能读 —— 靠字号（13px）和分隔线
                    区分层级，不用 text-muted（对比度 2.56:1 不达标） */}
                <p className="mt-3 border-t border-border pt-3 text-[13px] leading-relaxed text-text-secondary">
                  {t(`${b.i18nPrefix}.more`)}
                </p>
              </div>
            </Reveal>
          );
        })}
      </div>
    </Section>
  );
}
