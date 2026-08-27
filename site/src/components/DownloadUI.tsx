import { useEffect, useState } from "react";
import { Download, Apple, Monitor, Clock, Terminal } from "lucide-react";
import { platforms, detectPlatform, type Platform, type PlatformId } from "@/data/platforms";
import {
  resolveDownload,
  resolveExtraDownloads,
  useLatestRelease,
  type ReleaseInfo,
} from "@/hooks/useRelease";
import { Link } from "react-router-dom";
import { useLocalizedPath, useT } from "@/hooks/useLang";
import { siteConfig } from "@/config/site";
import { ButtonExternal } from "@/components/ui/Button";
import { cn, externalLinkProps, formatBytes } from "@/lib/utils";

const PLATFORM_ICON: Record<PlatformId, typeof Monitor> = {
  windows: Monitor,
  macos: Apple,
  linux: Terminal,
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
 * **按访客所在平台给包**，而不是恒推 Windows。三个平台都已发布（v0.1.33 起
 * 同时出 exe / dmg×2 / AppImage+deb+rpm），恒推 Windows 会让 Mac 与 Linux 访客
 * 下完才发现装不上 —— 而这一屏是绝大多数人唯一会点的下载入口。
 *
 * 检测不出平台（或该平台尚未发布）时回落到 Windows，并在按钮下方给出
 * 「其它平台」入口，任何情况下都不把人堵死。
 */
/**
 * 主按钮与其下方那条链接共用的「给哪个包」决策。
 *
 * 抽成 hook 是为了让两者能**分别**渲染到不同的父容器里（理由见 `HeroDownloadButton`
 * 上方那段关于垂直错位的注释），同时又不把这段判断抄成两份 —— 抄两份必然漂移，
 * 而漂移的表现是「按钮给的是 macOS 包、链接文案却说在推荐 Windows」这类静默不一致。
 */
function useHeroTarget() {
  const { release } = useLatestRelease();
  const detected = useDetectedPlatform();
  const windows = platforms.find((p) => p.id === "windows")!;
  const detectedPlatform = detected ? platforms.find((p) => p.id === detected) : undefined;
  // 该平台已发布 → 用它；未发布或认不出 → 回落 Windows
  const target =
    detectedPlatform && detectedPlatform.status === "available" ? detectedPlatform : windows;
  return {
    release,
    windows,
    target,
    // 检测到的平台确实还没发布（当前三个平台都发布了，这一支是给将来新增平台留的）
    comingSoon: detectedPlatform?.status === "coming-soon",
    isFallbackPlatform: target.id !== detected,
  };
}

/**
 * Hero 区的主下载按钮。**只渲染按钮本身**，不含下方的「其它平台」链接。
 *
 * 🔴 为什么两者必须分开渲染：原先它们同在一个 `flex-col` 里，于是这个组件的高度是
 * 88px（按钮 56 + gap 8 + 链接 24），而 Hero 的按钮行是 `items-center` ——
 * 并排的「查看 GitHub」按钮（56px）会在 88px 行高里垂直居中，**比主按钮低 16px**。
 * 两个并排按钮就此错开，实测 `top=482` vs `top=498`。
 * 把链接移出这一行后，行高回到 56px，两个按钮才真正对齐。
 *
 * 顺带修掉第二个问题：那条链接原先带 `sm:items-start`，在整体居中的 hero 版式里
 * 单独左对齐到主按钮左边缘，看着像挂在左下角。现在它在按钮行下方独立居中。
 */
export function HeroDownloadButton() {
  const t = useT();
  const { release, target, comingSoon } = useHeroTarget();

  if (comingSoon) {
    return (
      <span className="inline-flex h-14 w-full cursor-not-allowed items-center justify-center gap-2 rounded-control border border-dashed border-border px-7 text-base font-medium text-text-secondary sm:w-auto">
        <Clock size={18} aria-hidden="true" />
        {t("hero.ctaPrimaryMacHint")}
      </span>
    );
  }

  const dl = resolveDownload(target, release);
  return (
    <ButtonExternal
      href={dl.href ?? siteConfig.github.latestRelease}
      size="xl"
      className="w-full sm:w-auto"
    >
      <Download size={19} aria-hidden="true" />
      {t("hero.ctaFor", { platform: t(`platform.${target.id}.name`) })}
    </ButtonExternal>
  );
}

/**
 * 主按钮下方那条「其它平台 / 换个平台」出路，独立一行居中。
 *
 * 平台判断可能错（UA 会被改、也有人替别人下载）→ 永远留一条出路。
 * 这一条不是装饰：认错平台而没有出路 = 用户下到装不上的包。
 *
 * 用 `Link` + `useLocalizedPath` 而不是裸 `<a href>`：裸 a 会整页重载
 * （首屏白一下、丢掉 sessionStorage 里的 release 缓存），而且语言前缀得自己拼 ——
 * 从 `document.documentElement.lang` 取会在语言切换后与当前路由不一致。
 */
export function HeroPlatformLink() {
  const t = useT();
  const path = useLocalizedPath();
  const { release, windows, comingSoon, isFallbackPlatform } = useHeroTarget();

  // 检测到的平台还没发布时，这条链接换成「直接下 Windows 版」——
  // 那种情况下把人送去下载页要多点一次，而他多半就是想要一个能装的包。
  if (comingSoon) {
    const win = resolveDownload(windows, release);
    if (!win.href) return null;
    return (
      <a
        href={win.href}
        {...externalLinkProps}
        className="inline-flex min-h-6 items-center text-sm text-primary underline underline-offset-4 hover:opacity-80"
      >
        {t("hero.ctaWindows")}
      </a>
    );
  }

  return (
    <Link
      to={path("download")}
      className="inline-flex min-h-6 items-center text-sm text-text-secondary underline underline-offset-4 hover:text-text-primary"
    >
      {isFallbackPlatform ? t("hero.ctaPickPlatform") : t("hero.ctaOtherPlatforms")}
    </Link>
  );
}

/** Hero 下方那行「支持平台」图标，未发布的平台配「即将推出」小标签 */
export function PlatformBadges({ className }: { className?: string }) {
  const t = useT();
  return (
    <ul className={cn("flex flex-wrap items-center gap-x-5 gap-y-2", className)}>
      {platforms.map((p) => {
        const Icon = PLATFORM_ICON[p.id];
        const soon = p.status === "coming-soon";
        return (
          // 「未发布」不靠调淡文字表达（深色模式下 text-muted 只有 2.5:1 读不清），
          // 而是靠右侧那个明确写着「即将推出」的标签 + 图标降透明度
          <li key={p.id} className="inline-flex items-center gap-1.5 text-sm text-text-secondary">
            <Icon size={16} aria-hidden="true" className={cn(soon && "opacity-50")} />
            <span>{t(`platform.${p.id}.name`)}</span>
            {soon && (
              <span className="rounded-pill border border-border px-1.5 py-0.5 text-[11px] text-text-secondary">
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
  const extras = resolveExtraDownloads(platform, release);
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
        <span className="absolute -top-2.5 left-6 rounded-pill bg-primary-solid px-2.5 py-0.5 text-[11px] font-medium text-primary-foreground">
          {t("download.recommended")}
        </span>
      )}

      <div className="flex items-center gap-3">
        <span
          className={cn(
            "inline-flex h-11 w-11 items-center justify-center rounded-control",
            soon ? "bg-surface-hover text-text-secondary" : "bg-primary/12 text-primary"
          )}
        >
          <Icon size={22} aria-hidden="true" />
        </span>
        <div>
          <h3 className="text-base font-semibold text-text-primary">{t(`platform.${platform.id}.name`)}</h3>
          <p className="text-[13px] text-text-secondary">{t(`platform.${platform.id}.format`)}</p>
        </div>
      </div>

      <dl className="mt-5 space-y-2 text-sm">
        <div className="flex justify-between gap-3">
          <dt className="text-text-secondary">{t("download.version")}</dt>
          <dd className="font-mono text-text-primary">{soon ? "—" : release.version}</dd>
        </div>
        <div className="flex justify-between gap-3">
          <dt className="shrink-0 text-text-secondary">{t("download.minOS")}</dt>
          <dd className="text-right text-text-primary">{platform.minOS}</dd>
        </div>
        {!soon && dl.size > 0 && (
          <div className="flex justify-between gap-3">
            <dt className="text-text-secondary">{t("download.size")}</dt>
            <dd className="text-text-primary">{formatBytes(dl.size)}</dd>
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
            className="inline-flex h-12 w-full cursor-not-allowed items-center justify-center gap-2 rounded-control border border-dashed border-border px-6 text-[15px] font-medium text-text-secondary"
          >
            <Clock size={17} aria-hidden="true" />
            {t("download.buttonComingSoon")}
          </span>
        )}
      </div>

      {soon && (
        <p className="mt-3 text-[13px] leading-relaxed text-text-secondary">
          {platform.id === "macos" ? t("download.macNote") : t("download.linuxNote")}
        </p>
      )}

      {/* 其它架构 / 包格式，与主下载并列。
          macOS 一次发布有 aarch64 与 x64 两个 dmg、Linux 有 AppImage/deb/rpm ——
          只给一个就必然有人拿错，而拿错架构的 dmg 在 Mac 上报的是「已损坏，无法打开」，
          与真正的损坏一模一样，用户几乎不可能自己诊断出来。 */}
      {extras.length > 0 && (
        <p className="mt-3 flex flex-wrap items-center gap-x-3 gap-y-1 text-[13px] text-text-secondary">
          <span className="text-text-muted">{t("download.otherBuilds")}</span>
          {extras.map((x) => (
            <a
              key={x.label}
              href={x.href}
              {...externalLinkProps}
              className="inline-flex min-h-6 items-center text-primary underline underline-offset-4 hover:opacity-80"
            >
              {x.label}
              {x.size > 0 && <span className="ml-1 text-text-muted">({formatBytes(x.size)})</span>}
            </a>
          ))}
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
        <p className="mt-5 text-center text-[13px] leading-relaxed text-text-secondary">
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
