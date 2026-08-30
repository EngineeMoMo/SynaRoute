import { Section, SectionTitle } from "@/components/ui/Section";
import { Accordion } from "@/components/ui/Accordion";
import { faqs } from "@/data/faq";
import { useT } from "@/hooks/useLang";

export function FAQ() {
  const t = useT();
  const items = faqs.map((f) => ({
    id: f.id,
    question: t(f.questionKey),
    answer: t(f.answerKey),
  }));

  return (
    <Section id="faq">
      <SectionTitle title={t("faq.title")} subtitle={t("faq.subtitle")} />
      <Accordion items={items} className="mx-auto mt-12 max-w-3xl" />
    </Section>
  );
}
