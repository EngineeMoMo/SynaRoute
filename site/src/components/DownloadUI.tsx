import { useEffect, useState } from "react";
import { Download, Apple, Monitor, Clock } from "lucide-react";
import { platforms, detectPlatform, type Platform, type PlatformId } from "@/data/platforms";
import { resolveDownload, useLatestRelease, type ReleaseInfo } from "@/hooks/useRelease";
import { useT } from "@/hooks/useLang";
import { siteConfig } from "@/config/site";
import { ButtonExternal } from "@/components/ui/Button";
import { cn, externalLinkProps, formatBytes } from "@/lib/utils";

const PLATFORM_ICON: Record<PlatformId, typeof Monitor> = {
  windows: Monitor,
  macos: Apple,
  linux: Monitor,
};

/** 访客所在平台。放 state 里而不是直接调用，是因为它依赖 navigator，首帧要保持稳定。 */
function useDetectedPlatform(): PlatformId | null {
  const [id, setId] = useState<PlatformId | null>(null);
  useEffect(() => setId(detectPlatform()), []);
  return id;
}

/**
 * Hero 区的主下载按钮。
 *
 * 对 Mac 访客的处理是这里最要紧的一点：**不能**默认推 Windows 包让人下完才发现装不上。
 * 检测到 macOS 时按钮直接说明「macOS 版即将推出」且不可点，下面另给 Windows 的入口。
 */
export function HeroDownloadButton() {
  const t = useT();
  const { release } = useLatestRelease();
  const detected = useDetectedPlatform();

  const windows = platforms.find((p) => p.id === "windows")!;
  const detectedPlatform = detected ? platforms.find((p) => p.id === detected) : undefined;
  const isUnavailablePlatform = detectedPlatform?.status === "coming-soon";

  if (isUnavailablePlatform && detectedPlatform) {
    const win = resolveDownload(windows, release);
    return (
      <div className="flex flex-col items-center gap-2 sm:items-start">
        <span className="inline-flex h-14 cursor-not-allowed items-center justify-center gap-2 rounded-control border border-dashed border-border px-7 text-base font-medium text-text-muted">
          <Clock size={18} aria-hidden="true" />
          {t("hero.ctaPrimaryMacHint")}
        </span>
        {win.href && (
          <a
            href={win.href}
            {...externalLinkProps}
            className="text-sm text-primary underline underline-offset-4 hover:opacity-80"
          >
            {t("hero.ctaPrimary")}
          </a>
        )}
      </div>
    );
  }

  const win = resolveDownload(windows, release);
  return (
    <ButtonExternal href={win.href ?? siteConfig.github.latestRelease} size="xl" className="w-full sm:w-auto">
      <Download size={19} aria-hidden="true" />
      {t("hero.ctaPrimary")}
    </ButtonExternal>
  );
}

/** Hero 下方那行「支持平台」图标，未发布的平台半透明并标注 */
export function PlatformBadges({ className }: { className?: string }) {
  const t = useT();
  return (
    <ul className={cn("flex flex-wrap items-center gap-x-5 gap-y-2", className)}>
      {platforms.map((p) => {
        const Icon = PLATFORM_ICON[p.id];
        const soon = p.status === "coming-soon";
        return (
          <li
            key={p.id}
            className={cn(
              "inline-flex items-center gap-1.5 text-sm",
              soon ? "text-text-muted" : "text-text-secondary"
            )}
          >
            <Icon size={16} aria-hidden="true" className={cn(soon && "opacity-60")} />
            <span>{t(`platform.${p.id}.name`)}</span>
            {soon && (
              <span className="rounded-pill bg-surface-hover px-1.5 py-0.5 text-[11px] text-text-muted">
                {t("common.comingSoon")}
              </span>
            )}
          </li>
        );
      })}
    </ul>
  );
}

/**
 * 单个平台的下载卡片。
 *
 * `status: "coming-soon"` 时渲染的是 `<span>` 而不是 `<a>` —— DOM 里根本不存在
 * href，从物理上杜绝「点了跳到 404」。模板第 6.6 节对此有明确要求。
 */
export function PlatformCard({
  platform,
  release,
  recommended,
}: {
  platform: Platform;
  release: ReleaseInfo;
  recommended: boolean;
}) {
  const t = useT();
  const Icon = PLATFORM_ICON[platform.id];
  const dl = resolveDownload(platform, release);
  const soon = platform.status === "coming-soon";

  return (
    <div
      className={cn(
        "relative flex flex-col rounded-card border bg-surface p-6 transition-shadow",
        recommended ? "border-primary shadow-card-hover" : "border-border shadow-card hover:shadow-card-hover",
        soon && "opacity-90"
      )}
    >
      {recommended && (
        <span className="absolute -top-2.5 left-6 rounded-pill bg-primary px-2.5 py-0.5 text-[11px] font-medium text-primary-foreground">
          {t("download.recommended")}
        </span>
      )}

      <div className="flex items-center gap-3">
        <span
          className={cn(
            "inline-flex h-11 w-11 items-center justify-center rounded-control",
            soon ? "bg-surface-hover text-text-muted" : "bg-primary/12 text-primary"
          )}
        >
          <Icon size={22} aria-hidden="true" />
        </span>
        <div>
          <h3 className="text-base font-semibold text-text-primary">{t(`platform.${platform.id}.name`)}</h3>
          <p className="text-xs text-text-muted">{platform.format}</p>
        </div>
      </div>

      <dl className="mt-5 space-y-2 text-sm">
        <div className="flex justify-between gap-3">
          <dt className="text-text-muted">{t("download.version")}</dt>
          <dd className="font-mono text-text-secondary">{soon ? "—" : release.version}</dd>
        </div>
        <div className="flex justify-between gap-3">
          <dt className="shrink-0 text-text-muted">{t("download.minOS")}</dt>
          <dd className="text-right text-text-secondary">{platform.minOS}</dd>
        </div>
        {!soon && dl.size > 0 && (
          <div className="flex justify-between gap-3">
            <dt className="text-text-muted">{t("download.size")}</dt>
            <dd className="text-text-secondary">{formatBytes(dl.size)}</dd>
          </div>
        )}
      </dl>

      <div className="mt-6">
        {dl.href ? (
          <ButtonExternal href={dl.href} size="lg" className="w-full">
            <Download size={18} aria-hidden="true" />
            {t("download.button")}
          </ButtonExternal>
        ) : (
          <span
            aria-disabled="true"
            className="inline-flex h-12 w-full cursor-not-allowed items-center justify-center gap-2 rounded-control border border-dashed border-border px-6 text-[15px] font-medium text-text-muted"
          >
            <Clock size={17} aria-hidden="true" />
            {t("download.buttonComingSoon")}
          </span>
        )}
      </div>

      {soon && (
        <p className="mt-3 text-xs leading-relaxed text-text-muted">
          {platform.id === "macos" ? t("download.macNote") : t("download.linuxNote")}
        </p>
      )}
    </div>
  );
}

/** 平台卡片组，Hero 之外的下载区与下载页共用 */
export function PlatformGrid({ className }: { className?: string }) {
  const { release } = useLatestRelease();
  const detected = useDetectedPlatform();
  const t = useT();

  return (
    <div className={className}>
      <div className="grid gap-5 sm:grid-cols-2">
        {platforms.map((p) => (
          <PlatformCard
            key={p.id}
            platform={p}
            release={release}
            // 只高亮「访客所在且真的能下」的那一个，猜错也只是少个高亮
            recommended={detected === p.id && p.status === "available"}
          />
        ))}
      </div>

      {release.source === "fallback" && (
        <p className="mt-5 text-center text-xs leading-relaxed text-text-muted">
          {t("download.fallbackNote")}{" "}
          <a
            href={siteConfig.github.releases}
            {...externalLinkProps}
            className="text-primary underline underline-offset-2"
          >
            {t("download.allReleases")}
          </a>
        </p>
      )}
    </div>
  );
}
