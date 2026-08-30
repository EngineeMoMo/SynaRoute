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
                  <Icon size={20} aria-hidden="true" />
                </span>
                <h3 className="mt-4 text-base font-semibold text-text-primary">{t(`${b.i18nPrefix}.name`)}</h3>
                {/* 🔴 `lg:min-h` 是为了把下面那条分隔线钉在同一水平线上。
                    四列并排时这段说明有 2~3 行之差（实测第 4 张比其余三张少一行），
                    而分隔线跟着文流走 → 四条线里有一条高 23px，一眼就是没对齐。
                    只在 `lg` 加：≤1023px 是 2 列或 1 列，同一行内行数本来就一致，
                    加了反而在手机上凭空多出空白。
                    **刻意不用 `mt-auto`** —— 那只会把 23px 的错位翻到分隔线另一侧，
                    并在中间留下一个新的洞。 */}
                <p className="mt-2 text-sm leading-relaxed text-text-secondary lg:min-h-[4.5rem]">
                  {t(`${b.i18nPrefix}.desc`)}
                </p>
                {/* 补充说明比上面一句更次要，但仍要能读 —— 靠字号（13px）和分隔线
                    区分层级，不用 text-muted（对比度 2.56:1 不达标）。
                    同样只在 lg 钉一个下限：四列并排时这段是 3~4 行之差，钉住之后
                    四张卡的底边也齐了（否则会变成「分隔线齐了、卡底差 21px」）。 */}
                <p className="mt-3 border-t border-border pt-3 text-sm leading-relaxed text-text-secondary sm:text-[13px] lg:min-h-[6.125rem]">
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
