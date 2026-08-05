import { Github } from "lucide-react";
import { Container } from "@/components/ui/Section";
import { ButtonExternal } from "@/components/ui/Button";
import { HeroDownloadButton, PlatformBadges } from "@/components/DownloadUI";
import { Screenshot } from "@/components/ui/Screenshot";
import { heroScreenshot } from "@/data/screenshots";
import { siteConfig } from "@/config/site";
import { useLatestRelease } from "@/hooks/useRelease";
import { useT } from "@/hooks/useLang";
import { useTheme } from "@/hooks/useTheme";

export function Hero() {
  const t = useT();
  const { theme } = useTheme();
  const { release } = useLatestRelease();

  return (
    <section className="relative overflow-hidden pt-28 sm:pt-32 lg:pt-36">
      {/* 背景装饰：一团很淡的主色光晕。刻意只有一处、无动画 ——
          模板第 7.2 节禁止大面积高饱和渐变，这里只用来把视线引向首屏中部 */}
      <div
        aria-hidden="true"
        className="pointer-events-none absolute inset-x-0 top-0 -z-10 h-[520px] bg-[radial-gradient(60%_60%_at_50%_0%,rgb(var(--primary)/0.14),transparent_70%)]"
      />

      <Container>
        <div className="mx-auto max-w-3xl text-center">
          <p className="animate-fade-in inline-flex items-center rounded-pill border border-border bg-surface px-3.5 py-1.5 text-xs font-medium text-text-secondary">
            {t("hero.badge")}
          </p>

          <h1 className="animate-fade-up mt-6 text-hero-sm font-bold text-text-primary sm:text-hero">
            {t("hero.title")}
          </h1>

          <p className="animate-fade-up mt-6 text-base leading-relaxed text-text-secondary sm:text-lg">
            {t("hero.desc")}
          </p>
          <p className="animate-fade-up mt-3 text-sm leading-relaxed text-text-muted sm:text-base">
            {t("hero.descSecond")}
          </p>

          <div className="animate-fade-up mt-9 flex flex-col items-center justify-center gap-3 sm:flex-row">
            <HeroDownloadButton />
            <ButtonExternal href={siteConfig.github.url} variant="outline" size="xl" className="w-full sm:w-auto">
              <Github size={19} aria-hidden="true" />
              {t("hero.ctaSecondary")}
            </ButtonExternal>
          </div>

          <div className="mt-6 flex flex-col items-center gap-3">
            <PlatformBadges className="justify-center" />
            <p className="text-xs text-text-muted">
              {t("hero.versionPrefix")} <span className="font-mono">{release.version}</span>
            </p>
          </div>
        </div>

        {/* 主截图：首屏不滚动就能看到一部分，把「这是个什么软件」直接摆出来 */}
        <div className="animate-fade-up mt-14 sm:mt-16">
          <div className="mx-auto max-w-5xl overflow-hidden rounded-card border border-border bg-surface shadow-card-hover">
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
