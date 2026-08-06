import { useState } from "react";
import { Maximize2 } from "lucide-react";
import { Section, SectionTitle, Reveal } from "@/components/ui/Section";
import { ImageLightbox, type LightboxImage } from "@/components/ui/ImageLightbox";
import { Screenshot } from "@/components/ui/Screenshot";
import { screenshots } from "@/data/screenshots";
import { useT } from "@/hooks/useLang";
import { useTheme } from "@/hooks/useTheme";

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
    <Section id="screenshots">
      <SectionTitle title={t("screenshots.title")} subtitle={t("screenshots.subtitle")} />

      {/* 首图占满整行、其余两列 —— 主界面值得给更大的展示面积 */}
      <div className="mt-12 grid gap-6 sm:grid-cols-2">
        {screenshots.map((s, i) => (
          <Reveal
            key={s.id}
            delay={i * 60}
            className={i === 0 ? "sm:col-span-2" : undefined}
          >
            <figure className="group h-full">
              <button
                type="button"
                onClick={() => setOpenIndex(i)}
                aria-label={`${t(`${s.i18nPrefix}.title`)} — ${t("screenshots.enlarge")}`}
                className="relative block w-full overflow-hidden rounded-card border border-border bg-surface shadow-card transition-shadow hover:shadow-card-hover"
              >
                <Screenshot
                  src={images[i].src}
                  alt={images[i].alt}
                  width={s.width}
                  height={s.height}
                />
                <span
                  aria-hidden="true"
                  className="absolute right-3 top-3 inline-flex h-9 w-9 items-center justify-center rounded-control bg-black/50 text-white opacity-0 backdrop-blur-sm transition-opacity duration-250 group-hover:opacity-100"
                >
                  <Maximize2 size={16} />
                </span>
              </button>
              <figcaption className="mt-3">
                <span className="text-sm font-medium text-text-primary">{t(`${s.i18nPrefix}.title`)}</span>
                <span className="mt-1 block text-[13px] leading-relaxed text-text-secondary">
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
