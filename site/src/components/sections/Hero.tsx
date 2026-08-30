import { Github } from "lucide-react";
import { Container } from "@/components/ui/Section";
import { ButtonExternal } from "@/components/ui/Button";
import { HeroDownloadButton, HeroPlatformLink, PlatformBadges } from "@/components/DownloadUI";
import { Screenshot } from "@/components/ui/Screenshot";
import { heroScreenshot } from "@/data/screenshots";
import { siteConfig } from "@/config/site";
import { useLatestRelease } from "@/hooks/useRelease";
import { useLang, useT } from "@/hooks/useLang";
import { useTheme } from "@/hooks/useTheme";

export function Hero() {
  const t = useT();
  const lang = useLang();
  const { theme } = useTheme();
  const { release } = useLatestRelease();
  // 只有中文需要锁住换行点，理由见下面 h1 处的注释
  const lockWrap = lang === "zh" ? "whitespace-nowrap" : undefined;

  return (
    <section className="relative overflow-hidden pt-28 sm:pt-32 lg:pt-36">
      {/* 背景装饰：一团很淡的主色光晕。刻意只有一处、无动画 ——
          模板第 7.2 节禁止大面积高饱和渐变，这里只用来把视线引向首屏中部。
          0.14 → 0.18：全站唯一一处色彩深度，原值在 #FAFAFA 上几乎看不出来，
          而它承担的正是「首屏不是一张白纸」这件事。仍远低于「大面积高饱和」。 */}
      <div
        aria-hidden="true"
        className="pointer-events-none absolute inset-x-0 top-0 -z-10 h-[520px] bg-[radial-gradient(60%_60%_at_50%_0%,rgb(var(--primary)/0.18),transparent_70%)]"
      />

      <Container>
        <div className="mx-auto max-w-4xl text-center">
          <p className="animate-fade-in inline-flex items-center rounded-pill border border-border bg-surface px-3.5 py-1.5 text-xs font-medium text-text-secondary">
            {t("hero.badge")}
          </p>

          {/* 标题排版，中英各有各的坑：
              · text-balance 让浏览器均分各行宽度 —— 没有它，桌面端会把最后
                两三个字挤成孤零零的一行
              · 中文额外把两段各锁成整体：中文没有词边界，浏览器可在任意两字之间断行，
                会断出「多个 Key 互为备 / 份，」这种半个词。英文**不能**锁 ——
                实测 320px 下单段就要 325px，锁死会被 section 的 overflow-hidden 裁掉
              · 三档字号：320px 用 hero-xs(30px)、360px 起 hero-sm(36px)、
                640px 起 hero(56px)。锁了 nowrap 的中文段在 36px 下要 325px，
                而 320px 视口的容器只有 280px，不降档就会被裁掉右半句
              · max-w-4xl（不是 3xl）给 3.5rem 的字号留够横向空间 */}
          <h1 className="animate-fade-up mt-6 text-balance text-hero-xs font-bold text-text-primary xs:text-hero-sm sm:text-hero">
            <span className={lockWrap}>{t("hero.titleLead")}</span>
            <span className={lockWrap}>{t("hero.titleTail")}</span>
          </h1>

          {/* 层级：引导句用 primary，补充句用 secondary。
              原先两段都是 secondary，于是标题之下是 4~5 行同色同字重的灰字 ——
              styles.css 顶部自己定的「引导句 primary / 说明 secondary」阶梯
              恰恰没用在首屏这最该有层级的地方。字号不动，只换颜色与间距。 */}
          <p className="animate-fade-up mx-auto mt-6 max-w-2xl text-balance text-base leading-relaxed text-text-primary sm:text-lg">
            {t("hero.desc")}
          </p>
          <p className="animate-fade-up mx-auto mt-4 max-w-xl text-balance text-sm leading-relaxed text-text-secondary sm:text-[15px]">
            {t("hero.descSecond")}
          </p>

          {/* 这一行**只放两个按钮**，「其它平台」链接单独一行。
              原先链接嵌在主按钮组件内部（同一个 flex-col），把这一行撑到 88px 高，
              而本行是 items-center → 56px 的「查看 GitHub」被垂直居中、比主按钮低 16px，
              两个并排按钮就此错开。详见 DownloadUI.tsx 里 HeroDownloadButton 上方的注释。 */}
          <div className="animate-fade-up mt-9 flex flex-col items-center justify-center gap-3 sm:flex-row">
            <HeroDownloadButton />
            <ButtonExternal href={siteConfig.github.url} variant="outline" size="xl" className="w-full sm:w-auto">
              <Github size={19} aria-hidden="true" />
              {t("hero.ctaSecondary")}
            </ButtonExternal>
          </div>

          <div className="animate-fade-up mt-3 flex justify-center">
            <HeroPlatformLink />
          </div>

          {/* 平台徽标与版本号收成**一行**。
              原先是三层堆叠（其它平台链接 / 三个平台徽标 / 当前版本），
              加上上面那条链接一共四层字号相近的小字挂在主按钮下面，
              视觉上是一坨、谁也不突出。徽标与版本号本就是同一类元信息，合并即可。 */}
          <div className="mt-6 flex flex-wrap items-center justify-center gap-x-4 gap-y-2">
            <PlatformBadges />
            <span aria-hidden="true" className="hidden h-3.5 w-px bg-border sm:block" />
            <p className="text-xs text-text-secondary">
              {t("hero.versionPrefix")} <span className="font-mono">{release.version}</span>
            </p>
          </div>
        </div>

        {/* 主截图：首屏不滚动就能看到一部分，把「这是个什么软件」直接摆出来。
            宽度与下方所有卡片网格共用 1136px（原先是 max-w-5xl=1024，于是页面上
            出现了 1136 / 1024 / 896 三条纵向基准线，其中 1024 只服务这一个元素）。
            海拔用最高那一档 —— 它是整页唯一「浮在页面之上」的主视觉。 */}
        <div className="animate-fade-up mt-14 sm:mt-16">
          <div className="overflow-hidden rounded-card border border-border bg-surface shadow-raised">
            <Screenshot
              src={theme === "dark" ? heroScreenshot.dark : heroScreenshot.light}
              alt={t("hero.screenshotAlt")}
              width={heroScreenshot.width}
              height={heroScreenshot.height}
              eager
            />
          </div>
        </div>
      </Container>
    </section>
  );
}
