import { Section, SectionTitle, Reveal } from "@/components/ui/Section";
import { features } from "@/data/features";
import { useT } from "@/hooks/useLang";

/**
 * 常规功能区，Bento 布局。
 *
 * 两个 `half`（占半行、字号更大）+ 六个 `third`（三列小格），两行都排满。
 * 数量约束写在 data/features.ts 的注释里。
 *
 * 大脑聚合**不在这里** —— 它有独立的 BrainSpotlight 区块（紧跟 Benefits）。
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
                {/* 引导句用 primary 色 + medium，说明段用 secondary。
                    层级靠字号字重拉开 —— 别把说明段改成 text-muted，那一档
                    对比度只有 2.56:1，见 styles.css 顶部说明 */}
                <p className="mt-2 text-[15px] font-medium leading-relaxed text-text-primary">
                  {t(`${f.i18nPrefix}.short`)}
                </p>
                <p className="mt-4 text-sm leading-relaxed text-text-secondary">{t(`${f.i18nPrefix}.desc`)}</p>
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
              <article className="flex h-full flex-col rounded-card border border-border bg-surface p-6 shadow-card transition-shadow hover:shadow-card-hover">
                <div className="flex items-center gap-3">
                  <span className="inline-flex h-9 w-9 shrink-0 items-center justify-center rounded-control bg-surface-hover text-text-secondary">
                    <Icon size={18} aria-hidden="true" />
                  </span>
                  <h3 className="text-base font-semibold text-text-primary">{t(`${f.i18nPrefix}.name`)}</h3>
                </div>
                <p className="mt-3 text-sm font-medium leading-relaxed text-text-primary">
                  {t(`${f.i18nPrefix}.short`)}
                </p>
                <p className="mt-2 text-[13px] leading-relaxed text-text-secondary">{t(`${f.i18nPrefix}.desc`)}</p>
              </article>
            </Reveal>
          );
        })}
      </div>
    </Section>
  );
}
