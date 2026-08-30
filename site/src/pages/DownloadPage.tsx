import { ShieldAlert, RefreshCw, ExternalLink } from "lucide-react";
import { Container } from "@/components/ui/Section";
import { PlatformGrid } from "@/components/DownloadUI";
import { siteConfig } from "@/config/site";
import { useSeo } from "@/hooks/useSeo";
import { useLang, useT } from "@/hooks/useLang";
import { externalLinkProps } from "@/lib/utils";

export default function DownloadPage() {
  const t = useT();
  const lang = useLang();

  useSeo({
    title: t("download.pageTitle"),
    description: t("download.subtitle"),
    lang,
  });

  return (
    <Container className="py-28 sm:py-32">
      {/* 用满容器宽度（1136px）而不是 max-w-5xl（1024px）：三列下每张卡才有 365px，
          Linux 那两个附加包（deb / rpm）能排在一行里。1024px 时列宽只有 328px，
          它们会折成两行 → 那张卡的底部块更高 → 三个下载按钮又对不齐了。
          顺带让本页与首页共用同一条 1136px 纵向基准。
          标题段仍保持窄栏居中，长句不会拉成一行大字。 */}
      <div>
        <header className="mx-auto max-w-2xl text-center">
          <h1 className="text-balance text-3xl font-bold tracking-tight text-text-primary sm:text-4xl">
            {t("download.pageTitle")}
          </h1>
          <p className="mt-4 text-balance text-base text-text-secondary">{t("download.subtitle")}</p>
        </header>

        <PlatformGrid className="mt-12" />

        {/* `items-start`：右卡结构上多一节「v0.1.23 或更早」，两张卡的高度差是恒定的
            —— 默认的 stretch 会把左卡拉到同高，底下空出约 190px。 */}
        <div className="mt-10 grid items-start gap-5 sm:grid-cols-2">
          <div className="rounded-card border border-border bg-surface p-6 shadow-card">
            <div className="flex items-center gap-2.5">
              <ShieldAlert size={18} className="shrink-0 text-warning" aria-hidden="true" />
              {/* h2 提到 text-base：原先是 14px，比它下方卡片里的 h3（16px）还小，
                  同一页里标题层级是倒过来的 */}
              <h2 className="text-base font-semibold text-text-primary">{t("download.verifyTitle")}</h2>
            </div>
            <p className="mt-2.5 text-sm leading-relaxed text-text-secondary">{t("download.verifyDesc")}</p>
          </div>

          <div className="rounded-card border border-border bg-surface p-6 shadow-card">
            <div className="flex items-center gap-2.5">
              <RefreshCw size={18} className="shrink-0 text-info" aria-hidden="true" />
              <h2 className="text-base font-semibold text-text-primary">{t("download.updateTitle")}</h2>
            </div>
            <p className="mt-2.5 text-sm leading-relaxed text-text-secondary">{t("download.updateDesc")}</p>

            {/* 老版本用户的出路。刻意放在**这张卡片里**、紧跟上面那句之后 ——
                因为上面那句「无需手动来官网重新下载」对停在 v0.1.23 及更早的用户是**假的**,
                两句必须挨着，否则等于在同一页上留下一条自相矛盾的说明。

                也刻意不做成 warning 色的独立横幅：绝大多数访客用的是新版本，
                对他们这条不适用，做成显眼告警只会制造「我是不是有问题」的疑虑。 */}
            <div className="mt-4 border-t border-border pt-3.5">
              <h3 className="text-sm font-semibold text-text-primary">
                {t("download.updateLegacyTitle")}
              </h3>
              <p className="mt-1.5 text-sm leading-relaxed text-text-secondary">
                {t("download.updateLegacyDesc")}
              </p>
            </div>
          </div>
        </div>

        <p className="mt-8 text-center">
          <a
            href={siteConfig.github.releases}
            {...externalLinkProps}
            className="inline-flex items-center gap-1.5 text-sm text-primary underline underline-offset-4 hover:opacity-80"
          >
            {t("download.allReleases")}
            <ExternalLink size={14} aria-hidden="true" />
          </a>
        </p>
      </div>
    </Container>
  );
}
