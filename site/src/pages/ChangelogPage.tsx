import { ExternalLink, AlertCircle } from "lucide-react";
import { Container } from "@/components/ui/Section";
import { ButtonExternal } from "@/components/ui/Button";
import { siteConfig } from "@/config/site";
import { useReleaseNotes, type ReleaseNote } from "@/hooks/useRelease";
import { useMarkdown } from "@/hooks/useMarkdown";
import { useSeo } from "@/hooks/useSeo";
import { useLang, useT } from "@/hooks/useLang";
import { externalLinkProps, formatDate } from "@/lib/utils";

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

        {loading && <p className="mt-12 text-sm text-text-muted">{t("common.loading")}</p>}

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
          <p className="mt-12 text-sm text-text-muted">{t("changelog.empty")}</p>
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
  const { html } = useMarkdown(note.body, { headingOffset: 1 });

  return (
    <article className="rounded-card border border-border bg-surface p-6 shadow-card sm:p-8">
      <header className="flex flex-wrap items-baseline justify-between gap-x-4 gap-y-1">
        <h2 className="font-mono text-lg font-semibold text-text-primary">{note.version}</h2>
        {note.publishedAt && (
          <time dateTime={note.publishedAt} className="text-xs text-text-muted">
            {formatDate(note.publishedAt, lang)}
          </time>
        )}
      </header>

      {note.name && note.name !== note.version && (
        <p className="mt-2 text-[15px] font-medium text-text-secondary">{note.name}</p>
      )}

      {html ? (
        <div className="prose-doc mt-5" dangerouslySetInnerHTML={{ __html: html }} />
      ) : (
        <p className="mt-5 text-sm text-text-muted">—</p>
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
