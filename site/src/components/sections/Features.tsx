import { Section, SectionTitle, Reveal } from "@/components/ui/Section";
import { features } from "@/data/features";
import { useT } from "@/hooks/useLang";
import { cn } from "@/lib/utils";

/**
 * 常规功能区，Bento 布局。
 *
 * 两个 `half`（占半行、字号更大）+ 十二个 `third`（三列小格），两组都排满整数行。
 * 数量约束写在 data/features.ts 的注释里，并有策略门
 * `home-grids-must-fill-whole-rows` 钉着（注释破过一次约，判据不会）。
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
                {/* 图标与标题同一行 —— 与下面 third 卡的头部骨架保持一致。
                    原先 half 是「图标在上、标题在下」，于是同一个区块里两种卡片
                    读起来像两个不同的组件（还在图标与标题之间空出 20px）。
                    两种卡的层级差交给宽度、图标尺寸与标题字号，不再多一个变化轴。 */}
                <div className="flex items-center gap-4">
                  <span className="inline-flex h-12 w-12 shrink-0 items-center justify-center rounded-control bg-primary/12 text-primary">
                    <Icon size={24} aria-hidden="true" />
                  </span>
                  <h3 className="text-xl font-semibold text-text-primary">{t(`${f.i18nPrefix}.name`)}</h3>
                </div>
                {/* 引导句用 primary 色 + medium，说明段用 secondary。
                    层级靠字号字重拉开 —— 别把说明段改成 text-muted，那一档
                    对比度只有 2.56:1，见 styles.css 顶部说明 */}
                <p className="mt-4 text-[15px] font-medium leading-relaxed text-text-primary">
                  {t(`${f.i18nPrefix}.short`)}
                </p>
                <p className="mt-3 text-sm leading-relaxed text-text-secondary">{t(`${f.i18nPrefix}.desc`)}</p>
              </article>
            </Reveal>
          );
        })}
      </div>

      {/*
        十二张小卡。<640px 刻意**不画卡片**：单列时 12 个白底圆角盒子连成约 3500px
        （占整页 22%）的同构色块，全程没有一次密度变化；改成分隔线清单后每条省掉
        约 68px，形态也与 BrainSpotlight 的能力列表一致。
        640px 起恢复卡片 —— 那时是两列/三列，盒子边界才真的在帮忙分组。
      */}
      <div className="mt-5 grid gap-5 max-sm:mt-8 max-sm:gap-0 sm:grid-cols-2 lg:grid-cols-3">
        {small.map((f, i) => {
          const Icon = f.icon;
          return (
            <Reveal
              key={f.id}
              delay={i * 50}
              // 最后一条不画那条分隔线 —— 挂在清单末尾的一根线正是「没做完」的样子。
              // 只影响 <640px 的清单形态，640px 起是卡片、这条变体不生效。
              className={cn("h-full", i === small.length - 1 && "max-sm:[&>article]:border-b-0")}
            >
              <article className="flex h-full flex-col rounded-card border border-border bg-surface p-6 shadow-card transition-shadow hover:shadow-card-hover max-sm:rounded-none max-sm:border-x-0 max-sm:border-t-0 max-sm:bg-transparent max-sm:p-0 max-sm:pb-5 max-sm:pt-5 max-sm:shadow-none">
                <div className="flex items-center gap-3">
                  {/* 主色底片而不是灰底灰线：这一节原先整段没有一点品牌色，
                      12 张灰图标读起来像附录。层级靠尺寸（36 vs half 的 48）区分。 */}
                  <span className="inline-flex h-9 w-9 shrink-0 items-center justify-center rounded-control bg-primary/12 text-primary">
                    <Icon size={18} aria-hidden="true" />
                  </span>
                  <h3 className="text-base font-semibold text-text-primary">{t(`${f.i18nPrefix}.name`)}</h3>
                </div>
                <p className="mt-3 text-sm font-medium leading-relaxed text-text-primary">
                  {t(`${f.i18nPrefix}.short`)}
                </p>
                <p className="mt-2 text-sm leading-relaxed text-text-secondary sm:text-[13px]">
                  {t(`${f.i18nPrefix}.desc`)}
                </p>
              </article>
            </Reveal>
          );
        })}
      </div>
    </Section>
  );
}
