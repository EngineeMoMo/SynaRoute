import { ExternalLink, AlertCircle } from "lucide-react";
import { Container } from "@/components/ui/Section";
import { ButtonExternal } from "@/components/ui/Button";
import { siteConfig } from "@/config/site";
import { useReleaseNotes, type ReleaseNote } from "@/hooks/useRelease";
import { useMarkdown } from "@/hooks/useMarkdown";
import { useSeo } from "@/hooks/useSeo";
import { useLang, useT } from "@/hooks/useLang";
import { externalLinkProps, formatDate } from "@/lib/utils";

/**
 * 每个版本的发布说明里都重复的样板文字，展示时剥掉。
 *
 * 为什么值得剥：实测每张版本卡都以「Download the installer…」开头、以
 * 「Already using SynaRoute?…」那整段固定尾注结尾 —— 一页八个版本就是同一段话
 * 重复八遍，把「这个版本到底改了什么」挤在中间。那段尾注的内容在下载页
 * （`download.updateLegacy*`）已经有一份，是那里的正文。
 *
 * 只是**展示层过滤**：原文一字不改，每张卡片右下角就是「在 GitHub 上查看这个版本」。
 * 匹配不到时什么都不做（新版本换了模板也不会出错）。
 */
const BOILERPLATE: RegExp[] = [
  /^[ \t]*Download the installer for your platform from the assets below\.[ \t]*$/gim,
  /(?:\n[ \t]*-{3,}[ \t]*)?\n[ \t]*\*\*Already using SynaRoute\?[\s\S]*$/i,
];

function stripBoilerplate(body: string): string {
  let out = body;
  for (const re of BOILERPLATE) out = out.replace(re, "");
  // 剥完可能留下连续空行与孤零零的分隔线
  return out
    .replace(/^[ \t]*-{3,}[ \t]*$/gm, "")
    .replace(/\n{3,}/g, "\n\n")
    .trim();
}

/**
 * 卡头第二行该显示什么。
 *
 * GitHub 的 `name` 实测就是「SynaRoute v0.1.42」——与上一行那个大号版本号
 * 完全重复，而它的字号还更大更醒目。原先的判据是 `name !== version`，
 * 挡不住带品牌前缀的这种形态。剥掉品牌名与版本号后为空就不渲染这一行。
 */
function releaseSubtitle(name: string, version: string): string {
  const bare = version.replace(/^v/i, "");
  return name
    .replace(/SynaRoute/gi, "")
    .replace(new RegExp(`v?${bare.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}`, "gi"), "")
    .replace(/^[\s·:：\-—|]+|[\s·:：\-—|]+$/g, "")
    .trim();
}

export default function ChangelogPage() {
  const t = useT();
  const lang = useLang();
  const { notes, loading, failed } = useReleaseNotes();

  useSeo({ title: t("changelog.title"), description: t("changelog.subtitle"), lang });

  return (
    <Container className="py-28 sm:py-32">
      <div className="mx-auto max-w-3xl">
        <header>
          <h1 className="text-3xl font-bold tracking-tight text-text-primary sm:text-4xl">
            {t("changelog.title")}
          </h1>
          <p className="mt-4 text-base text-text-secondary">{t("changelog.subtitle")}</p>
        </header>

        {loading && <p className="mt-12 text-sm text-text-secondary">{t("common.loading")}</p>}

        {/* 拿不到就老实说拿不到，并给一个能用的去处 —— 不留白屏 */}
        {!loading && failed && (
          <div className="mt-12 rounded-card border border-warning/40 bg-warning/8 p-6">
            <div className="flex items-center gap-2.5">
              <AlertCircle size={18} className="shrink-0 text-warning" aria-hidden="true" />
              <p className="text-sm font-medium text-text-primary">{t("changelog.loadFailed")}</p>
            </div>
            <p className="mt-2 text-sm leading-relaxed text-text-secondary">{t("changelog.loadFailedHint")}</p>
            <ButtonExternal href={siteConfig.github.releases} variant="outline" size="md" className="mt-4">
              {t("common.openInGithub")}
              <ExternalLink size={15} aria-hidden="true" />
            </ButtonExternal>
          </div>
        )}

        {!loading && !failed && notes.length === 0 && (
          <p className="mt-12 text-sm text-text-secondary">{t("changelog.empty")}</p>
        )}

        <div className="mt-12 space-y-10">
          {notes.map((note) => (
            <ReleaseEntry key={note.version} note={note} lang={lang} />
          ))}
        </div>
      </div>
    </Container>
  );
}

function ReleaseEntry({ note, lang }: { note: ReleaseNote; lang: string }) {
  const t = useT();
  // 发布说明整体降一级：页面 h1 是「更新日志」、版本号是 h2，
  // 正文里的 `##` 必须落到 h3 才不会与版本号平级。
  const { html } = useMarkdown(stripBoilerplate(note.body), { headingOffset: 1 });
  const subtitle = releaseSubtitle(note.name, note.version);

  return (
    <article className="rounded-card border border-border bg-surface p-6 shadow-card sm:p-8">
      <header className="flex flex-wrap items-baseline justify-between gap-x-4 gap-y-1">
        <h2 className="font-mono text-lg font-semibold text-text-primary">{note.version}</h2>
        {note.publishedAt && (
          <time dateTime={note.publishedAt} className="text-xs text-text-secondary">
            {formatDate(note.publishedAt, lang)}
          </time>
        )}
      </header>

      {subtitle && <p className="mt-2 text-[15px] font-medium text-text-secondary">{subtitle}</p>}

      {html ? (
        // 行宽上限：同一个 .prose-doc 在文档页被列宽限到 640px，这里原先没有上限，
        // 英文正文一行能到约 94 个字符（舒适区是 60~75）。
        <div className="prose-doc mt-5 max-w-prose" dangerouslySetInnerHTML={{ __html: html }} />
      ) : (
        // 剥掉样板后确实没有正文的版本（只发了包、没写说明）：收成一行，
        // 不再撑出一张与「内容丰富的版本」等体量的空卡。
        <p className="mt-3 text-sm text-text-secondary">{t("changelog.noNotes")}</p>
      )}

      <a
        href={note.htmlUrl}
        {...externalLinkProps}
        className="mt-6 inline-flex items-center gap-1.5 text-sm text-primary underline underline-offset-4 hover:opacity-80"
      >
        {t("changelog.viewOnGithub")}
        <ExternalLink size={14} aria-hidden="true" />
      </a>
    </article>
  );
}
