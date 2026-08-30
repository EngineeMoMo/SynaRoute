import { useState } from "react";
import { Maximize2 } from "lucide-react";
import { Section, SectionTitle, Reveal } from "@/components/ui/Section";
import { ImageLightbox, type LightboxImage } from "@/components/ui/ImageLightbox";
import { Screenshot } from "@/components/ui/Screenshot";
import { screenshots } from "@/data/screenshots";
import { useT } from "@/hooks/useLang";
import { useTheme } from "@/hooks/useTheme";
import { cn } from "@/lib/utils";

export function Screenshots() {
  const t = useT();
  const { theme } = useTheme();
  const [openIndex, setOpenIndex] = useState<number | null>(null);

  // 截图随站点主题切换明暗版本 —— 深色页面配浅色截图会很扎眼
  const images: LightboxImage[] = screenshots.map((s) => ({
    src: theme === "dark" ? s.dark : s.light,
    alt: `${t(`${s.i18nPrefix}.title`)}：${t(`${s.i18nPrefix}.desc`)}`,
    width: s.width,
    height: s.height,
    caption: t(`${s.i18nPrefix}.title`),
  }));

  return (
    // tight 节奏：「界面一览」是上一段功能清单的延续，不是新话题
    <Section id="screenshots" rhythm="tight">
      <SectionTitle title={t("screenshots.title")} subtitle={t("screenshots.subtitle")} />

      {/*
        ≥640px：首图占满整行、其余两列 —— 主界面值得给更大的展示面积。
        <640px：改成**横向滑动条**。原先五张 1440×900 的密集界面图在 390px 下被缩到
        约 350px（23.6%），一个字都读不出来，却占掉约 2000px 的纵向滚动。
        横排后同样五张只占约 300px，露出下一张的边缘作为「可以滑」的提示，
        点开仍然进灯箱（那里才是真的能看清的地方）。
        用 `w-[80%]` 而不是 `vw` —— 后者含滚动条宽度，窄屏上会算出比容器更宽的值。
        `snap-proximity` 而不是 mandatory：吸附但不黏手，不干扰纵向滑动。
      */}
      <div className="mt-12 flex snap-x snap-proximity gap-4 overflow-x-auto pb-2 sm:grid sm:snap-none sm:grid-cols-2 sm:gap-x-6 sm:gap-y-10 sm:overflow-visible sm:pb-0">
        {screenshots.map((s, i) => (
          <Reveal
            key={s.id}
            delay={i * 60}
            className={cn(
              "max-sm:w-[80%] max-sm:shrink-0 max-sm:snap-start",
              i === 0 && "sm:col-span-2"
            )}
          >
            <figure className="group h-full">
              <button
                type="button"
                onClick={() => setOpenIndex(i)}
                aria-label={`${t(`${s.i18nPrefix}.title`)} — ${t("screenshots.enlarge")}`}
                // 可点卡片的 hover 走**两条通道**：阴影 + 描边。
                // 深色模式下阴影本来就弱，只给一条会出现「hover 了但看不出来」。
                className="relative block w-full overflow-hidden rounded-card border border-border bg-surface shadow-card transition-all hover:border-border-strong hover:shadow-card-hover"
              >
                <Screenshot
                  src={images[i].src}
                  alt={images[i].alt}
                  width={s.width}
                  height={s.height}
                />
                {/*
                  「放大」角标原先是 hover-only —— 触屏设备**永远**看不到它，
                  于是「点一下能放大」这件事在手机上完全没有提示。
                  按输入能力分而不是按屏幕宽度分：有真 hover 的设备（鼠标）保持
                  悬停才显示，触屏（含 768px+ 的平板）常显。
                */}
                <span
                  aria-hidden="true"
                  className="absolute right-3 top-3 inline-flex h-9 w-9 items-center justify-center rounded-control bg-black/50 text-white opacity-100 backdrop-blur-sm transition-opacity duration-250 [@media(hover:hover)]:opacity-0 [@media(hover:hover)]:group-hover:opacity-100"
                >
                  <Maximize2 size={16} />
                </span>
              </button>
              {/* 图注贴紧自己的图（8px），与下一张图之间由 gap-y-10（40px）拉开 ——
                  原先 12px 对 24px，归属关系只靠 2:1 撑着，扫视时容易读串行 */}
              <figcaption className="mt-2">
                <span className="text-sm font-medium text-text-primary">{t(`${s.i18nPrefix}.title`)}</span>
                <span className="mt-1 block text-sm leading-relaxed text-text-secondary sm:text-[13px]">
                  {t(`${s.i18nPrefix}.desc`)}
                </span>
              </figcaption>
            </figure>
          </Reveal>
        ))}
      </div>

      <ImageLightbox
        images={images}
        index={openIndex}
        onClose={() => setOpenIndex(null)}
        onIndexChange={setOpenIndex}
      />
    </Section>
  );
}
