import { useEffect } from "react";
import { Link, useParams } from "react-router-dom";
import { ArrowLeft, ExternalLink } from "lucide-react";
import { Container } from "@/components/ui/Section";
import { docs, findDoc } from "@/data/docs";
import { siteConfig } from "@/config/site";
import { useMarkdown } from "@/hooks/useMarkdown";
import { useSeo } from "@/hooks/useSeo";
import { useLang, useLocalizedPath, useT } from "@/hooks/useLang";
import { externalLinkProps } from "@/lib/utils";
import NotFoundPage from "@/pages/NotFoundPage";

export default function DocPage() {
  const { slug } = useParams();
  const lang = useLang();
  const t = useT();
  const path = useLocalizedPath();
  const doc = findDoc(slug);

  // Hook 必须无条件调用且顺序固定，所以即使文档不存在也要走一遍（喂空串）。
  // 判空放在渲染分支里，不放在 hook 之前。
  const { html, toc } = useMarkdown(doc ? doc.body[lang] : "");

  // 切文档时回到顶部，否则从长文档跳到短文档会停在半空
  useEffect(() => {
    window.scrollTo({ top: 0 });
  }, [slug, lang]);

  useSeo({
    title: doc ? t(`${doc.i18nPrefix}.title`) : t("notFound.title"),
    description: doc ? t(`${doc.i18nPrefix}.desc`) : t("notFound.desc"),
    lang,
    type: "article",
  });

  if (!doc) return <NotFoundPage />;

  return (
    <Container className="py-28 sm:py-32">
      {/*
        🔴 列宽收成「正文 640 + 目录 240 + 间距 48 = 928」并整体居中。
        原先是 `[minmax(0,1fr)_15rem]`，于是正文列有 848px 宽、而正文自己又被
        `max-w-prose` 限到 583px —— **同一列限了两次**，中间恒定空出约 313px；
        更糟的是页脚「查看原文」与上下篇卡片没被限，它们铺到 848px，
        于是同一列里出现三条不同的右端。现在只限一次，附属块自动与正文同宽。
      */}
      <div className="mx-auto max-w-[58rem] lg:grid lg:grid-cols-[minmax(0,40rem)_15rem] lg:gap-12">
        <article className="min-w-0 max-w-prose lg:max-w-none">
          <Link
            to={path("docs")}
            className="inline-flex items-center gap-1.5 text-sm text-text-secondary transition-colors hover:text-text-primary"
          >
            <ArrowLeft size={15} aria-hidden="true" />
            {t("docs.backToDocs")}
          </Link>

          {/* 内容来源是仓库内自己写的 Markdown，且已过 DOMPurify 清洗（见 useMarkdown） */}
          <div className="prose-doc mt-6" dangerouslySetInnerHTML={{ __html: html }} />

          <footer className="mt-12 border-t border-border pt-6">
            <a
              href={`${siteConfig.github.url}/blob/master/${encodeURI(doc.sourcePath)}`}
              {...externalLinkProps}
              className="inline-flex items-center gap-1.5 text-sm text-text-secondary transition-colors hover:text-text-primary"
            >
              {t("docs.editOnGithub")}
              <ExternalLink size={14} aria-hidden="true" />
            </a>
          </footer>

          <DocPager slug={doc.slug} />
        </article>

        {/* 右侧目录：窄屏隐藏（挤下来会把正文推得很窄），桌面端固定跟随 */}
        {toc.length > 1 && (
          <nav aria-label={t("docs.onThisPage")} className="hidden lg:block">
            <div className="sticky top-24">
              {/* 去掉 `uppercase`：这个词在中文下完全无效，而 en 下把它变成
                  全大写小字又与全站其它小标题不是一套。text-muted 提到 secondary
                  —— 它是导航栏的标题，承载信息。 */}
              <p className="text-xs font-semibold tracking-wide text-text-secondary">
                {t("docs.onThisPage")}
              </p>
              <ul className="mt-3 space-y-1.5 border-l border-border">
                {toc.map((item) => (
                  <li key={item.id}>
                    <a
                      href={`#${item.id}`}
                      className={`-ml-px block border-l-2 border-transparent py-0.5 text-sm leading-snug text-text-secondary transition-colors hover:border-primary hover:text-text-primary ${
                        item.level === 3 ? "pl-6" : "pl-3"
                      }`}
                    >
                      {item.text}
                    </a>
                  </li>
                ))}
              </ul>
            </div>
          </nav>
        )}
      </div>
    </Container>
  );
}

function DocPager({ slug }: { slug: string }) {
  const t = useT();
  const path = useLocalizedPath();
  const i = docs.findIndex((d) => d.slug === slug);
  const prev = i > 0 ? docs[i - 1] : null;
  const next = i >= 0 && i < docs.length - 1 ? docs[i + 1] : null;
  if (!prev && !next) return null;

  /*
   * 🔴 用 flex + justify-between，**不再用两列网格 + 空占位**。
   * 原先只有「下一篇」时会渲染一个 `<span />` 占住左列、next 再 `sm:col-start-2`
   * 靠右 —— 于是文档链的第一篇末尾是「左边一个空格子、右边一张卡」，
   * 而最后一篇（只有 prev）末尾是「左边一张卡、右边空一半」。两头都是孤卡。
   * flex 下只有一张时它自然占据自己那一侧，没有空占位。
   */
  return (
    <div className="mt-10 flex flex-col gap-4 border-t border-border pt-8 sm:flex-row sm:justify-between">
      {prev && (
        <Link
          to={path(`docs/${prev.slug}`)}
          className="rounded-card border border-border bg-surface p-4 transition-all hover:border-border-strong hover:shadow-card-hover sm:max-w-[calc(50%-0.5rem)]"
        >
          <span className="text-xs text-text-secondary" aria-hidden="true">
            ←
          </span>
          <span className="mt-1 block text-sm font-medium text-text-primary">{t(`${prev.i18nPrefix}.title`)}</span>
        </Link>
      )}
      {next && (
        <Link
          to={path(`docs/${next.slug}`)}
          className="rounded-card border border-border bg-surface p-4 transition-all hover:border-border-strong hover:shadow-card-hover sm:ml-auto sm:max-w-[calc(50%-0.5rem)] sm:text-right"
        >
          <span className="text-xs text-text-secondary" aria-hidden="true">
            →
          </span>
          <span className="mt-1 block text-sm font-medium text-text-primary">{t(`${next.i18nPrefix}.title`)}</span>
        </Link>
      )}
    </div>
  );
}
