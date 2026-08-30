import { Link } from "react-router-dom";
import { Mail } from "lucide-react";
import { Container } from "@/components/ui/Section";
import { siteConfig } from "@/config/site";
import { useMarkdown } from "@/hooks/useMarkdown";
import { useSeo } from "@/hooks/useSeo";
import { useLang, useLocalizedPath, useT } from "@/hooks/useLang";
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
 *
 * ## 页头页尾为什么由组件渲染，而不是全交给 Markdown
 *
 * 原先这一页是「一行 12px 淡色更新日期 + 一整块 Markdown」，没有页头也没有页尾：
 * 更新日期顶在标题**之上**（读起来像是页面的第一句话）、正文末尾直接断掉、
 * 而 `useMarkdown` 已经算好的 `toc` 被整个丢掉。条款页往往是最长的一堵字墙，
 * 恰恰最需要「这是什么、多久更新的、有哪几节、有问题找谁」这四件事。
 *
 * 所以：h1 与日期胶囊由组件出，章节锚点用 toc 的 h2 渲染一行，末尾补一个带邮箱
 * 与另一份条款互链的页尾。正文里那行 `# 标题` 随之剥掉（否则会出现两个标题）。
 */
const LAST_UPDATED = "2026-08-04";

const CONTENT: Record<"privacy" | "terms", Record<Lang, string>> = {
  privacy: { zh: privacyZh, en: privacyEn },
  terms: { zh: termsZh, en: termsEn },
};

/**
 * 去掉正文开头那行 `# 标题`。
 * 页头由组件渲染（标题下面要放更新日期胶囊，Markdown 里表达不了这种结构）。
 * 文件里没有这一行时是 no-op —— 不会因为某份文本换了写法就报错。
 */
function stripLeadingH1(md: string): string {
  return md.replace(/^\s*#\s+.*\r?\n+/, "");
}

export default function LegalPage({ kind }: { kind: "privacy" | "terms" }) {
  const t = useT();
  const lang = useLang();
  const path = useLocalizedPath();
  const { html, toc } = useMarkdown(stripLeadingH1(CONTENT[kind][lang]));

  // 另一份条款：两页互链，读完一份能直接跳到另一份
  const other = kind === "privacy" ? "terms" : "privacy";

  useSeo({
    title: t(`${kind}.title`),
    description: t(`${kind}.title`),
    lang,
    type: "article",
  });

  return (
    <Container className="py-28 sm:py-32">
      <div className="mx-auto max-w-prose">
        <header>
          <h1 className="text-3xl font-bold tracking-tight text-text-primary sm:text-4xl">
            {t(`${kind}.title`)}
          </h1>
          <p className="mt-4 inline-flex items-center rounded-pill border border-border bg-surface-hover px-3 py-1 text-xs text-text-secondary">
            {t(`${kind}.updated`, { date: LAST_UPDATED })}
          </p>
        </header>

        {/* 章节锚点：一堵字墙前面先给一张目录。只取 h2（h3 太碎，条款页 h2 就是「一、二、三」） */}
        {toc.filter((i) => i.level === 2).length > 1 && (
          <nav aria-label={t("docs.onThisPage")} className="mt-6 border-t border-border pt-4">
            <ul className="flex flex-wrap gap-x-4 gap-y-2">
              {toc
                .filter((i) => i.level === 2)
                .map((item) => (
                  <li key={item.id}>
                    <a
                      href={`#${item.id}`}
                      className="text-sm text-text-secondary underline decoration-border underline-offset-4 transition-colors hover:text-text-primary hover:decoration-primary"
                    >
                      {item.text}
                    </a>
                  </li>
                ))}
            </ul>
          </nav>
        )}

        <div className="prose-doc mt-8" dangerouslySetInnerHTML={{ __html: html }} />

        <footer className="mt-12 flex flex-col gap-3 border-t border-border pt-6 sm:flex-row sm:items-center sm:justify-between">
          <a
            href={`mailto:${siteConfig.author.email}`}
            className="inline-flex items-center gap-1.5 text-sm text-text-secondary transition-colors hover:text-text-primary"
          >
            <Mail size={14} aria-hidden="true" />
            {siteConfig.author.email}
          </a>
          <Link
            to={path(other)}
            className="text-sm text-primary underline underline-offset-4 hover:opacity-80"
          >
            {t(`footer.${other}`)}
          </Link>
        </footer>
      </div>
    </Container>
  );
}
