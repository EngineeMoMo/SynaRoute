import { Github } from "lucide-react";
import { Container } from "@/components/ui/Section";
import { ButtonExternal } from "@/components/ui/Button";
import { HeroDownloadButton, HeroPlatformLink } from "@/components/DownloadUI";
import { LogoMark } from "@/components/ui/Logo";
import { siteConfig } from "@/config/site";
import { useLang, useT } from "@/hooks/useLang";

export function FinalCTA() {
  const t = useT();
  const lang = useLang();
  // 只有中文需要锁住换行点，理由同 Hero 的 h1
  const lockWrap = lang === "zh" ? "whitespace-nowrap" : undefined;

  return (
    <section className="py-16 sm:py-20 lg:py-24">
      <Container>
        {/*
          强调带，不再是「Hero 下半段的白卡复制」。三处改动：

          1. `isolate` —— 🔴 原先那团光晕**一个像素都没渲染出来**：本容器有
             `relative` 但不构成层叠上下文，于是 `-z-10` 的子元素被自己的
             `bg-surface` 盖住了。Hero 那处同样写法能生效，只因为它的 section
             没有背景色。加 `isolate` 让 z-index 在本容器内解算。
          2. 底色从 `bg-surface` 换成 `bg-primary/8` + 主色描边 —— 白卡压在
             #FAFAFA 上几乎看不出边界，而这是全页最后一个转化点，它需要有分量。
             对比度已核：styles.css 量过 primary/8 这一档上 text-secondary 有
             4.98~5.34，达标。
          3. 海拔给 raised —— 它应该看起来是「浮起来的一块」。
        */}
        <div className="relative isolate overflow-hidden rounded-card border border-primary/30 bg-primary/8 px-6 py-14 text-center shadow-raised sm:px-10">
          {/* 与 Hero 呼应的淡光晕，同样只有一处、无动画 */}
          <div
            aria-hidden="true"
            className="pointer-events-none absolute inset-x-0 top-0 -z-10 h-64 bg-[radial-gradient(50%_60%_at_50%_0%,rgb(var(--primary)/0.16),transparent_70%)]"
          />

          <LogoMark size={44} className="mx-auto" />
          {/* 标题拆两段并对中文锁 nowrap：不锁的话 390px 下会断成
              「让多／个模型一起想」这种半个词（同 Hero 的 h1）。 */}
          <h2 className="mt-6 text-balance text-2xl font-bold tracking-tight text-text-primary sm:text-3xl">
            <span className={lockWrap}>{t("cta.titleLead")}</span>
            <span className={lockWrap}>{t("cta.titleTail")}</span>
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

          {/* 平台徽标与版本号**刻意不再重复一遍**：它们在 Hero 里已经出现过，
              而这里连着「其它平台」链接一共三层小字，把这一块从「最后一推」
              读成了首屏的复印件。真正不能省的是那条出路链接（见上面的理由）。 */}
          <div className="mt-4 flex justify-center">
            <HeroPlatformLink />
          </div>
        </div>
      </Container>
    </section>
  );
}
