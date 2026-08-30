import { Section, SectionTitle } from "@/components/ui/Section";
import { PlatformGrid } from "@/components/DownloadUI";
import { useT } from "@/hooks/useLang";

export function Downloads() {
  const t = useT();
  return (
    <Section id="download">
      <SectionTitle title={t("download.title")} subtitle={t("download.subtitle")} />
      {/* 刻意不再套 max-w-3xl：三个平台在两列里必然留一个空位（Linux 独占一行、
          右半边全空），而 768px 的容器放不下三列。用满容器宽度后每张卡 365px，
          与今天两列时的卡宽一模一样，同时和页面上其余卡片网格共用同一条基准线。 */}
      <PlatformGrid className="mt-12" />
    </Section>
  );
}
