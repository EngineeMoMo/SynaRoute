import { Github } from "lucide-react";
import { Container } from "@/components/ui/Section";
import { ButtonExternal } from "@/components/ui/Button";
import { HeroDownloadButton, HeroPlatformLink, PlatformBadges } from "@/components/DownloadUI";
import { LogoMark } from "@/components/ui/Logo";
import { siteConfig } from "@/config/site";
import { useLatestRelease } from "@/hooks/useRelease";
import { useT } from "@/hooks/useLang";

export function FinalCTA() {
  const t = useT();
  const { release } = useLatestRelease();

  return (
    <section className="py-16 sm:py-20 lg:py-24">
      <Container>
        <div className="relative overflow-hidden rounded-card border border-border bg-surface px-6 py-14 text-center shadow-card sm:px-10">
          {/* 与 Hero 呼应的淡光晕，同样只有一处、无动画 */}
          <div
            aria-hidden="true"
            className="pointer-events-none absolute inset-x-0 top-0 -z-10 h-64 bg-[radial-gradient(50%_60%_at_50%_0%,rgb(var(--primary)/0.12),transparent_70%)]"
          />

          <LogoMark size={44} className="mx-auto" />
          <h2 className="mt-6 text-2xl font-bold tracking-tight text-text-primary sm:text-3xl">
            {t("cta.title")}
          </h2>
          <p className="mt-3 text-base text-text-secondary">{t("cta.desc")}</p>

          {/* 与 Hero 同构，故同样只放两个按钮 —— 那处 16px 垂直错位在这里也存在过。
              「其它平台」链接必须一并保留：这是最后一个转化点，
              认错平台而没有出路 = 用户下到装不上的包。 */}
          <div className="mt-8 flex flex-col items-center justify-center gap-3 sm:flex-row">
            <HeroDownloadButton />
            <ButtonExternal href={siteConfig.github.url} variant="outline" size="xl" className="w-full sm:w-auto">
              <Github size={19} aria-hidden="true" />
              {t("common.viewGithub")}
            </ButtonExternal>
          </div>

          <div className="mt-3 flex justify-center">
            <HeroPlatformLink />
          </div>

          <div className="mt-6 flex flex-col items-center gap-2">
            <PlatformBadges className="justify-center" />
            <p className="text-xs text-text-secondary">
              {t("hero.versionPrefix")} <span className="font-mono">{release.version}</span>
            </p>
          </div>
        </div>
      </Container>
    </section>
  );
}
