import { Brain, Users, GitMerge, Sparkles, FileSearch, ImageIcon, Plug, ArrowRight } from "lucide-react";
import type { LucideIcon } from "lucide-react";
import { Section, Reveal } from "@/components/ui/Section";
import { ButtonLink } from "@/components/ui/Button";
import { Screenshot } from "@/components/ui/Screenshot";
import { screenshots } from "@/data/screenshots";
import { useLang, useT } from "@/hooks/useLang";
import { useTheme } from "@/hooks/useTheme";

/**
 * 大脑聚合专区。
 *
 * 为什么单独开一个区块、而不是留在 Features 的九宫格里：这是本产品唯一「别处没有」的
 * 能力（多模型并行 + 决策者汇总），塞进等大的卡片阵列里和「托盘与自启动」同等权重，
 * 等于把最强的差异点藏起来了。所以给它整屏、给它流程图、给它截图。
 *
 * 流程图是纯 DOM + Tailwind 画的，不是图片：中英文案长度不同，图片会写死语言；
 * 而且深色模式要跟着换色。
 */

const capabilities: { id: string; icon: LucideIcon; key: string }[] = [
  { id: "strategy", icon: GitMerge, key: "brain.cap.strategy" },
  { id: "retrieval", icon: FileSearch, key: "brain.cap.retrieval" },
  { id: "images", icon: ImageIcon, key: "brain.cap.images" },
  { id: "mcp", icon: Plug, key: "brain.cap.mcp" },
];

/** 流程图里的一个节点框 */
function FlowBox({
  icon: Icon,
  title,
  hint,
  tone = "plain",
}: {
  icon: LucideIcon;
  title: string;
  hint: string;
  tone?: "plain" | "primary";
}) {
  const primary = tone === "primary";
  return (
    <div
      className={[
        "flex flex-1 flex-col items-center gap-2 rounded-card border px-4 py-5 text-center",
        primary
          ? "border-primary/40 bg-primary/8"
          : // 🔴 深色下 `bg-surface` 与本区块的底色只差 ΔL* 0.76，前两个框实际上
            // 读不出「是个框」，全靠 1px 边线撑着 —— 而这里正是要让人看出「三步」。
            // 底色下沉半档 + 边框提亮，两档都在既有标度内。
            "border-border bg-surface dark:border-white/12 dark:bg-surface-hover/60",
      ].join(" ")}
    >
      <span
        className={[
          "inline-flex h-10 w-10 items-center justify-center rounded-control",
          primary ? "bg-primary/12 text-primary" : "bg-surface-hover text-text-secondary",
        ].join(" ")}
      >
        <Icon size={20} aria-hidden="true" />
      </span>
      <span className="text-sm font-semibold text-text-primary">{title}</span>
      {/* text-balance：768px 下这三个框各约 180px 宽，不加会折出「型」这种单字行 */}
      <span className="text-balance text-xs leading-relaxed text-text-secondary">{hint}</span>
    </div>
  );
}

/**
 * 节点之间的箭头。
 * 竖排（手机与平板）时旋转 90°，横排（桌面）时朝右 —— 一个元素两种朝向，
 * 比渲染两套 DOM 再互相 hidden 省事，也不会有重复的 aria 节点。
 *
 * 断点从 `sm`(640) 推到 `md`(768)：640~767px 横排时每个框只有约 180px，
 * 说明文字要折两行、还会折出单字行。
 */
function FlowArrow() {
  return (
    <span aria-hidden="true" className="flex shrink-0 items-center justify-center text-text-secondary">
      <ArrowRight size={18} className="rotate-90 md:rotate-0" />
    </span>
  );
}

export function BrainSpotlight() {
  const t = useT();
  const lang = useLang();
  const { theme } = useTheme();
  const shot = screenshots.find((s) => s.id === "brain");

  return (
    // 独立底色：这是整页唯一换背景的区块，用来把「这一段不一样」直接说出来。
    //
    // 🔴 原先是 `bg-surface-hover/40`，而它合成后是 #F8F8F8 —— 页面底色是 #FAFAFA，
    // **两者只差 2/255**，也就是这个「刻意换底色」的设计意图在浅色模式下压根没兑现
    // （深色下差 8/255，勉强能看出）。换成 5% 主色淡底：浅色算出来是 #F3F2FA、
    // 深色是 #14131A，两个主题都真的看得出是一条带。
    // 5 在 Tailwind 默认透明度标度内（5 的倍数），不会像 /8 那样静默失效。
    //
    // 对比度已核：text-secondary 压在 primary/5 上比压在 primary/8 上更宽松，
    // 而 styles.css 已量过 primary/8 那一档是 4.98~5.34，达标。
    // 节奏给 loose —— 全页唯一一处，让「换底色」与「留白更大」两个信号叠在一起。
    <Section id="brain" rhythm="loose" className="border-y border-primary/12 bg-primary/5">
      <div className="mx-auto max-w-3xl text-center">
        <Reveal>
          {/* 徽标底色用 surface 而不是 primary/8：主色文字压在自己的 8% 淡色底上
              只有 3.89:1（浅色模式），换成纯白底后 4.79:1 达标。边框保留主色，
              视觉上仍是「品牌色徽标」 */}
          <span className="inline-flex items-center gap-2 rounded-pill border border-primary/30 bg-surface px-3.5 py-1.5 text-xs font-medium text-primary">
            <Sparkles size={14} aria-hidden="true" />
            {t("brain.badge")}
          </span>
          <h2 className="mt-5 text-3xl font-bold tracking-tight text-text-primary sm:text-section-title">
            {t("brain.title")}
          </h2>
          <p className="mx-auto mt-4 max-w-2xl text-balance text-base leading-relaxed text-text-secondary">
            {t("brain.subtitle")}
          </p>
        </Reveal>
      </div>

      {/* 流程：成员并行 → 汇总 → 决策者。横排从 md 开始，理由见 FlowArrow */}
      <Reveal delay={80}>
        <div className="mx-auto mt-12 flex max-w-4xl flex-col items-stretch gap-2 md:flex-row md:items-center md:gap-3">
          <FlowBox icon={Users} title={t("brain.flow.members")} hint={t("brain.flow.membersHint")} />
          <FlowArrow />
          <FlowBox icon={GitMerge} title={t("brain.flow.merge")} hint={t("brain.flow.mergeHint")} />
          <FlowArrow />
          <FlowBox
            icon={Brain}
            title={t("brain.flow.decider")}
            hint={t("brain.flow.deciderHint")}
            tone="primary"
          />
        </div>
      </Reveal>

      {/* `lg:items-start`：原先 items-center 让右侧截图上下各比左栏差 31px ——
          两处错位。顶对齐后只剩底部一处参差，读起来干净得多。 */}
      <div className="mt-14 grid items-center gap-10 lg:grid-cols-2 lg:items-start lg:gap-14">
        <Reveal delay={120}>
          <ul className="space-y-5">
            {capabilities.map((c) => {
              const Icon = c.icon;
              return (
                <li key={c.id} className="flex gap-4">
                  <span className="mt-0.5 inline-flex h-9 w-9 shrink-0 items-center justify-center rounded-control bg-primary/12 text-primary">
                    <Icon size={18} aria-hidden="true" />
                  </span>
                  <div>
                    <h3 className="text-[15px] font-semibold text-text-primary">{t(`${c.key}.title`)}</h3>
                    <p className="mt-1 text-sm leading-relaxed text-text-secondary">{t(`${c.key}.desc`)}</p>
                  </div>
                </li>
              );
            })}
          </ul>
        </Reveal>

        {shot && (
          <Reveal delay={160}>
            <div className="overflow-hidden rounded-card border border-border bg-surface shadow-raised">
              <Screenshot
                src={theme === "dark" ? shot.dark : shot.light}
                alt={t("brain.screenshotAlt")}
                width={shot.width}
                height={shot.height}
              />
            </div>
          </Reveal>
        )}
      </div>

      {/* 两个入口放在整个区块的末尾居中，而不是塞在左栏能力列表下面：
          在左栏时它们会卡在右侧截图的中段旁边，既不像左栏的一部分、
          也不像整段的收尾，读起来是悬空的。居中后是明确的「看完这段，去这里」。 */}
      <Reveal delay={200}>
        <div className="mt-12 flex flex-col items-center gap-3 sm:flex-row sm:justify-center">
          <ButtonLink to={`/${lang}/docs/brain`} variant="primary" size="lg" className="w-full sm:w-auto">
            {t("brain.ctaDocs")}
          </ButtonLink>
          <ButtonLink to={`/${lang}/docs/mcp`} variant="outline" size="lg" className="w-full sm:w-auto">
            {t("brain.ctaMcp")}
          </ButtonLink>
        </div>
      </Reveal>
    </Section>
  );
}
