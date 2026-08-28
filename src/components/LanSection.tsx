// 局域网接入令牌的显示 / 复制 / 重新生成（B5，docs/14 §21.1）。
//
// 抽成独立组件而不是写进 SettingsPage：那边冻结在棘轮上、余量为 0。
// 同 McpAddressList / CustomHeadersField 的做法。
//
// # 为什么需要这块 UI
//
// 局域网鉴权 v0.1.41 就发出去了，但用户拿令牌的**唯一**通道是「去日志页搜『接入令牌』」
// —— `settings.lanDesc` 当时就是这么写的。那对一个安全功能来说门槛太高：
// 用户开了开关、客户端连不上、报 401，而「去哪拿凭据」得先读懂一句提示。
//
// 🔴 **本组件现在是完整令牌的唯一出口**（2026-08-27）：日志与事件里只留指纹（前 8 位）。
// 原因是明文进事件等于同时进了三个用户会分享出去的地方 —— 诊断报告（用途就是发给别人，
// 且开头声明「不含任何 API 密钥明文」）、`logs/*.jsonl`（非虚拟化、留 30 天、用户会 tail
// 并贴出来）、日志页截图。详见 `lan_guard::token_or_create`。
// 故这块 UI 从「省事」变成了**必需**：删掉它，用户就再也没有任何途径拿到令牌。

import * as React from "react";
import { Copy, Eye, EyeOff, KeyRound, RefreshCw } from "lucide-react";
import { Button } from "@/components/ui/Button";
import { api } from "@/lib/bridge";
import { ToggleRow } from "@/components/ToggleRow";

/** 局域网开关 + 令牌面板。两者是一件事：开了才有令牌可配，故收在一处。 */
export function LanSection({
  enabled,
  onToggle,
  t,
  onToast,
}: {
  enabled: boolean;
  onToggle: (v: boolean) => void;
  t: (k: string, p?: Record<string, string>) => string;
  onToast: (kind: "success" | "error", msg: string) => void;
}) {
  return (
    <>
      <ToggleRow
        icon={KeyRound}
        title={t("settings.lanTitle")}
        desc={t("settings.lanDesc")}
        checked={enabled}
        onChange={onToggle}
        danger
      />
      <TokenPanel enabled={enabled} t={t} onToast={onToast} />
    </>
  );
}

function TokenPanel({
  enabled,
  t,
  onToast,
}: {
  enabled: boolean;
  t: (k: string, p?: Record<string, string>) => string;
  onToast: (kind: "success" | "error", msg: string) => void;
}) {
  const [token, setToken] = React.useState<string | null>(null);
  const [err, setErr] = React.useState<string | null>(null);
  const [shown, setShown] = React.useState(false);
  const [confirming, setConfirming] = React.useState(false);
  const [busy, setBusy] = React.useState(false);

  const load = React.useCallback(async () => {
    try {
      setToken(await api.getLanToken());
      setErr(null);
    } catch (e) {
      // 🔴 锁定态的错误必须**原样报出**，不能退化成「还没有令牌」——
      // 那会让用户点「重新生成」，于是所有已配好的客户端立刻 401。
      setErr(String((e as Error)?.message ?? e));
      setToken(null);
    }
  }, []);

  React.useEffect(() => {
    if (enabled) void load();
  }, [enabled, load]);

  // 开关关着时整块不显示：没开局域网就没有「局域网客户端」要配，
  // 显示一个用不上的令牌只会让人以为本机也要填它。
  if (!enabled) return null;

  const regenerate = async () => {
    setBusy(true);
    try {
      const next = await api.regenerateLanToken();
      setToken(next);
      setShown(true); // 刚换的必须直接可见，否则用户得再点一次「显示」才能抄
      setErr(null);
      onToast("success", t("lanToken.regenerated"));
    } catch (e) {
      onToast("error", String((e as Error)?.message ?? e));
    } finally {
      setBusy(false);
      setConfirming(false);
    }
  };

  const copy = async () => {
    if (!token) return;
    try {
      await navigator.clipboard.writeText(token);
      onToast("success", t("lanToken.copied"));
    } catch {
      onToast("error", t("lanToken.copyFailed"));
    }
  };

  // 底色用 bg-surface-hover：`bg-surface-subtle` 在 tailwind.config.js 里**不存在**，
  // 写了不报错、一条 CSS 都不生成（同 CLAUDE.md 记的 `bg-warning/8` 那个坑）。
  // 第一版就写错了，被 `npm run build` 的颜色类检查当场抓出来。
  return (
    <div className="ml-9 mt-2 rounded-control border border-border bg-surface-hover p-3">
      <div className="mb-2 flex items-center justify-between gap-2">
        <span className="text-xs font-medium text-text-secondary">{t("lanToken.label")}</span>
        <div className="flex items-center gap-1">
          {token && (
            <>
              <Button size="sm" variant="ghost" onClick={() => setShown((v) => !v)}
                title={t(shown ? "lanToken.hide" : "lanToken.show")}>
                {shown ? <EyeOff className="h-3.5 w-3.5" /> : <Eye className="h-3.5 w-3.5" />}
              </Button>
              <Button size="sm" variant="ghost" onClick={copy} title={t("lanToken.copy")}>
                <Copy className="h-3.5 w-3.5" />
              </Button>
            </>
          )}
          <Button size="sm" variant="ghost" disabled={busy}
            onClick={() => setConfirming(true)} title={t("lanToken.regenerate")}>
            <RefreshCw className="h-3.5 w-3.5" />
          </Button>
        </div>
      </div>

      {err ? (
        <p className="text-[11px] leading-relaxed text-danger">{err}</p>
      ) : token ? (
        <code className="block break-all font-mono text-[11px] leading-relaxed text-text-primary">
          {shown ? token : "•".repeat(48)}
        </code>
      ) : (
        <p className="text-[11px] leading-relaxed text-text-muted">{t("lanToken.none")}</p>
      )}

      {confirming ? (
        // 重新生成是破坏性的，必须先确认。文案要说清后果（旧令牌立即失效、
        // 每个局域网客户端都得改），否则用户会当成「刷新一下」。
        <div className="mt-2 rounded-control border border-warning/40 bg-warning/8 p-2">
          <p className="mb-2 text-[11px] leading-relaxed text-text-primary">
            {t("lanToken.confirmDesc")}
          </p>
          <div className="flex gap-2">
            <Button size="sm" variant="danger" onClick={regenerate} disabled={busy}>
              {t("lanToken.confirmYes")}
            </Button>
            <Button size="sm" variant="ghost" onClick={() => setConfirming(false)} disabled={busy}>
              {t("common.cancel")}
            </Button>
          </div>
        </div>
      ) : (
        <p className="mt-2 text-[11px] leading-relaxed text-text-muted">{t("lanToken.hint")}</p>
      )}
    </div>
  );
}
