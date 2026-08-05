import { Section, SectionTitle } from "@/components/ui/Section";
import { PlatformGrid } from "@/components/DownloadUI";
import { useT } from "@/hooks/useLang";

export function Downloads() {
  const t = useT();
  return (
    <Section id="download">
      <SectionTitle title={t("download.title")} subtitle={t("download.subtitle")} />
      <PlatformGrid className="mx-auto mt-12 max-w-3xl" />
    </Section>
  );
}
