import { useState } from "react";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/Card";
import { Button } from "@/components/ui/Button";
import { useT } from "@/lib/useT";
import { openExternalUrl } from "@/lib/openExternal";
import { useStore } from "@/store";
import { Github, Globe, Mail, Copy, Check, Heart, ExternalLink } from "lucide-react";

/** 作者的 GitHub 用户名 —— 头像、主页都由它派生，改人只需改这一处。 */
const GH_USER = "EngineeMoMo";
/** 项目仓库（Release / Issue 都在这里）。 */
const REPO_URL = `https://github.com/${GH_USER}/SynaRoute`;
const AUTHOR_URL = `https://github.com/${GH_USER}`;
/**
 * 头像走 GitHub 的 `.png` 短链而非 API 返回的 avatars.githubusercontent.com 直链：
 * 前者是稳定的永久地址（换头像自动跟随），后者带 `?v=` 版本参数、换头像后旧链可能失效。
 */
const AVATAR_URL = `https://github.com/${GH_USER}.png?size=160`;

/**
 * 联系方式。空串 = 未提供，`ContactRow` 会渲染成灰色「待补充」而不是一个点了没反应的链接。
 * 换人/换域名只改这两行。
 */
const AUTHOR_EMAIL = "mhm292117@163.com";
const SITE_URL = "https://www.mofamilys.com";

/**
 * 在系统默认浏览器/邮件客户端里打开外部链接。
 *
 * **必须走 shell 插件**，不能用 `window.open` 或 `<a target="_blank">`：WebView 里那些会
 * 试图在应用自己的窗口内导航（或被拦掉），用户会看到主界面被一个网页顶替、且回不去。
 * 失败时弹 toast —— 静默失败会让用户以为链接是坏的。
 */
async function openExternal(url: string, onError: (msg: string) => void) {
  try {
    // 走共用 helper：它统一挡掉非 http(s) scheme，并把「为何外链能前端开、
    // 本地目录必须后端开」的判据集中在一处（见 lib/openExternal.ts）。
    await openExternalUrl(url);
  } catch (e) {
    onError(String((e as Error)?.message ?? e));
  }
}

/** 一行「可复制 + 可打开」的联系信息。值为空时渲染成「待补充」灰字，不给可点的假链接。 */
function ContactRow({
  icon: Icon,
  label,
  value,
  href,
  t,
}: {
  icon: typeof Github;
  label: string;
  value: string;
  href?: string;
  t: (k: string, vars?: Record<string, string | number>) => string;
}) {
  const showToast = useStore((s) => s.showToast);
  const [copied, setCopied] = useState(false);

  if (!value) {
    return (
      <div className="flex items-center gap-2.5 py-1.5">
        <Icon size={15} className="shrink-0 text-text-muted" />
        <span className="w-20 shrink-0 text-xs text-text-muted">{label}</span>
        <span className="text-xs italic text-text-muted">{t("about.tbd")}</span>
      </div>
    );
  }

  const copy = () => {
    void navigator.clipboard?.writeText(value).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 1400);
    });
  };

  return (
    <div className="flex items-center gap-2.5 py-1.5">
      <Icon size={15} className="shrink-0 text-text-secondary" />
      <span className="w-20 shrink-0 text-xs text-text-muted">{label}</span>
      <span className="min-w-0 flex-1 truncate font-mono text-xs text-text-primary">{value}</span>
      <button
        onClick={copy}
        className="shrink-0 rounded p-1 text-text-muted hover:bg-surface-hover hover:text-text-primary"
        title={t("about.copy")}
        aria-label={t("about.copy")}
      >
        {copied ? <Check size={13} className="text-success" /> : <Copy size={13} />}
      </button>
      {href && (
        <button
          onClick={() => void openExternal(href, (m) => showToast("error", m))}
          className="shrink-0 rounded p-1 text-text-muted hover:bg-surface-hover hover:text-text-primary"
          title={t("about.openInBrowser")}
          aria-label={t("about.openInBrowser")}
        >
          <ExternalLink size={13} />
        </button>
      )}
    </div>
  );
}

/** 关于作者页：头像 + 联系方式 + 项目/主页链接。 */
export function AboutPage() {
  const t = useT();
  const showToast = useStore((s) => s.showToast);
  const [avatarFailed, setAvatarFailed] = useState(false);

  return (
    <div className="h-full overflow-y-auto">
      <div className="border-b border-border px-6 py-4">
        <h1 className="text-lg font-semibold text-text-primary">{t("about.title")}</h1>
        <p className="mt-1 text-xs text-text-muted">{t("about.subtitle")}</p>
      </div>

      <div className="space-y-4 p-6">
        <Card>
          <CardContent>
            <div className="flex items-start gap-4">
              {/*
                头像加载失败要有兜底：离线、公司网络屏蔽 github.com、或用户改了用户名时，
                broken image 图标比首字母占位难看得多，且看不出是网络问题。
              */}
              {avatarFailed ? (
                <div className="flex h-20 w-20 shrink-0 items-center justify-center rounded-full bg-primary/15 text-2xl font-semibold text-primary">
                  {GH_USER.slice(0, 1).toUpperCase()}
                </div>
              ) : (
                <img
                  src={AVATAR_URL}
                  alt={t("about.avatarAlt", { user: GH_USER })}
                  className="h-20 w-20 shrink-0 rounded-full border border-border object-cover"
                  onError={() => setAvatarFailed(true)}
                />
              )}
              <div className="min-w-0 flex-1">
                <div className="text-base font-semibold text-text-primary">MoMo</div>
                <div className="mt-0.5 font-mono text-xs text-text-muted">@{GH_USER}</div>
                <p className="mt-2 text-xs leading-relaxed text-text-secondary">
                  {t("about.authorBlurb")}
                </p>
                <div className="mt-3 flex flex-wrap gap-2">
                  <Button
                    size="sm"
                    variant="outline"
                    onClick={() => void openExternal(AUTHOR_URL, (m) => showToast("error", m))}
                  >
                    <Github size={14} /> {t("about.viewProfile")}
                  </Button>
                  <Button
                    size="sm"
                    variant="outline"
                    onClick={() => void openExternal(REPO_URL, (m) => showToast("error", m))}
                  >
                    <Heart size={14} /> {t("about.starRepo")}
                  </Button>
                </div>
              </div>
            </div>
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle>{t("about.contactTitle")}</CardTitle>
          </CardHeader>
          <CardContent>
            <div className="divide-y divide-border">
              <ContactRow icon={Github} label={t("about.github")} value={AUTHOR_URL} href={AUTHOR_URL} t={t} />
              <ContactRow icon={Github} label={t("about.repo")} value={REPO_URL} href={REPO_URL} t={t} />
              <ContactRow
                icon={Globe}
                label={t("about.site")}
                value={SITE_URL}
                href={SITE_URL || undefined}
                t={t}
              />
              <ContactRow
                icon={Mail}
                label={t("about.email")}
                value={AUTHOR_EMAIL}
                href={AUTHOR_EMAIL ? `mailto:${AUTHOR_EMAIL}` : undefined}
                t={t}
              />
            </div>
            <div className="mt-3 rounded-control bg-info/8 px-3 py-2 text-[11px] leading-relaxed text-text-secondary">
              {t("about.issueHint")}
            </div>
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle>{t("about.thanksTitle")}</CardTitle>
          </CardHeader>
          <CardContent>
            <p className="text-xs leading-relaxed text-text-secondary">{t("about.thanksBody")}</p>
          </CardContent>
        </Card>
      </div>
    </div>
  );
}
