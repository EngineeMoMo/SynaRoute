import { Container } from "@/components/ui/Section";
import { useMarkdown } from "@/hooks/useMarkdown";
import { useSeo } from "@/hooks/useSeo";
import { useLang, useT } from "@/hooks/useLang";
import type { Lang } from "@/i18n";

import privacyZh from "@/content/zh/privacy.md?raw";
import privacyEn from "@/content/en/privacy.md?raw";
import termsZh from "@/content/zh/terms.md?raw";
import termsEn from "@/content/en/terms.md?raw";

/**
 * 条款类页面（隐私政策 / 用户协议）共用一个渲染器。
 *
 * 正文与文档页一样走 Markdown，方便后续单独修订法律文本而不动代码。
 * `LAST_UPDATED` 手工维护 —— 改了正文记得同步这一行，否则页面会声称一个
 * 与内容不符的更新日期。
 */
const LAST_UPDATED = "2026-08-04";

const CONTENT: Record<"privacy" | "terms", Record<Lang, string>> = {
  privacy: { zh: privacyZh, en: privacyEn },
  terms: { zh: termsZh, en: termsEn },
};

export default function LegalPage({ kind }: { kind: "privacy" | "terms" }) {
  const t = useT();
  const lang = useLang();
  const { html } = useMarkdown(CONTENT[kind][lang]);

  useSeo({
    title: t(`${kind}.title`),
    description: t(`${kind}.title`),
    lang,
    type: "article",
  });

  return (
    <Container className="py-28 sm:py-32">
      <div className="mx-auto max-w-prose">
        <p className="text-xs text-text-muted">{t(`${kind}.updated`, { date: LAST_UPDATED })}</p>
        <div className="prose-doc mt-4" dangerouslySetInnerHTML={{ __html: html }} />
      </div>
    </Container>
  );
}
