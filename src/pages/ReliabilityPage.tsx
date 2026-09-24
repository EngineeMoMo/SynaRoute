import { useRef, useState } from "react";
import { api } from "@/lib/bridge";
import { useBackendEvent, FALLBACK_POLL_MS } from "@/lib/useBackendEvents";
import { usePolling } from "@/lib/usePolling";
import { useT } from "@/lib/useT";
import type { TFunc } from "@/lib/i18n";
import type { CategoryResilience, KeyResilience, ResilienceOverview } from "@/lib/resilience";
import { RefreshButton } from "@/components/ui/RefreshButton";
import { InlineAlert } from "@/components/ui/InlineAlert";
import { PageHeader } from "@/components/ui/PageHeader";
import { ShieldCheck } from "lucide-react";

/**
 * 可靠性总览：把五层弹性容错（熔断 / 限流 / 余额 / 预算 / 并发）的**当前**状态摊开。
 *
 * 数据是后端 `resilience_overview` 的只读快照，与 `candidates_for` 读同几个函数、口径一致。
 * 实时性靠 `logs`/`config` 事件推送 + `usePolling` 兜底（同用量页），窗口不可见时停表。
 */
export function ReliabilityPage() {
  const t = useT();
  const [data, setData] = useState<ResilienceOverview | null>(null);
  const [error, setError] = useState<string | null>(null);
  const genRef = useRef(0);

  const load = async () => {
    const gen = ++genRef.current;
    try {
      const next = await api.resilienceOverview();
      if (gen !== genRef.current) return; // 只允许最新那轮提交（同用量页的代际号防串台）
      setData(next);
      setError(null);
    } catch (e) {
      if (gen !== genRef.current) return;
      setError(String(e));
    }
  };
  useBackendEvent(["logs", "config"], () => void load());
  usePolling(() => void load(), FALLBACK_POLL_MS);

  const cats = data?.categories ?? [];
  return (
    <div className="flex h-full flex-col">
      <PageHeader
        icon={ShieldCheck}
        title={t("resilience.title")}
        description={<p>{t("resilience.subtitle")}</p>}
        actions={
          <RefreshButton onRefresh={load} idleLabel={t("usage.refresh")} busyLabel={t("usage.refreshing")} />
        }
      />
      <div className="flex-1 space-y-4 overflow-y-auto p-6">
        {error && <InlineAlert tone="danger">{error}</InlineAlert>}
        {!data ? (
          <div className="py-16 text-center text-sm text-text-muted">{t("resilience.loading")}</div>
        ) : cats.length === 0 ? (
          <div className="py-16 text-center text-sm text-text-muted">{t("resilience.empty")}</div>
        ) : (
          <>
            {data.recent.totalEvents > 0 && (
              <div className="rounded-control border border-border bg-surface p-3">
                <div className="text-xs text-text-secondary">
                  {t("resilience.recent", {
                    n: String(data.recent.totalEvents),
                    routes: String(data.recent.routes),
                    failovers: String(data.recent.failovers),
                    errors: String(data.recent.errors),
                    warnings: String(data.recent.warnings),
                  })}
                </div>
                <div className="mt-1 text-[11px] text-text-muted">{t("resilience.recentNote")}</div>
              </div>
            )}
            {cats.map((c) => (
              <CategoryCard key={c.categoryId} c={c} t={t} />
            ))}
          </>
        )}
      </div>
    </div>
  );
}

/** 汇总小片。`ok` 那片总显示；异常几片只在 count>0 时由调用方决定是否渲染。 */
function Chip({ label, count, tone }: { label: string; count: number; tone: "ok" | "warn" | "danger" | "muted" }) {
  const cls =
    tone === "danger"
      ? "text-danger"
      : tone === "warn"
        ? "text-warning"
        : tone === "ok"
          ? "text-primary"
          : "text-text-muted";
  return (
    <span className={`text-xs ${cls}`}>
      {label} <span className="font-mono font-medium tabular-nums">{count}</span>
    </span>
  );
}

function CategoryCard({ c, t }: { c: CategoryResilience; t: TFunc }) {
  return (
    <div className="sr-panel overflow-hidden">
      <div className="flex flex-wrap items-center gap-x-4 gap-y-1 border-b border-border/80 bg-surface-hover/50 px-4 py-2.5">
        <span className="text-sm font-medium text-text-primary">{t(`nav.${c.categoryId}`)}</span>
        <span className={`text-[11px] ${c.proxyRunning ? "text-primary" : "text-text-muted"}`}>
          ● {c.proxyRunning ? t("resilience.proxyRunning") : t("resilience.proxyStopped")}
        </span>
        <span className="text-[11px] text-text-muted">
          {t("resilience.enabledCount", { n: String(c.enabledCount) })}
        </span>
        <div className="ml-auto flex flex-wrap items-center gap-x-3 gap-y-1">
          <Chip label={t("resilience.state.healthy")} count={c.healthyCount} tone="ok" />
          {c.breakerCount > 0 && <Chip label={t("resilience.state.breaker")} count={c.breakerCount} tone="danger" />}
          {c.rateLimitedCount > 0 && (
            <Chip label={t("resilience.state.rateLimited")} count={c.rateLimitedCount} tone="warn" />
          )}
          {c.exhaustedCount > 0 && (
            <Chip label={t("resilience.state.exhausted")} count={c.exhaustedCount} tone="danger" />
          )}
          {c.overBudgetCount > 0 && (
            <Chip label={t("resilience.state.overBudget")} count={c.overBudgetCount} tone="warn" />
          )}
        </div>
      </div>
      <div className="divide-y divide-border">
        {c.keys.map((k) => (
          <KeyRow key={k.keyId} k={k} t={t} />
        ))}
      </div>
    </div>
  );
}

/** 逐 Key 一行：状态点 + 名字 + 生效中的弹性标记 + 在途。标记为空 = 正常。 */
function KeyRow({ k, t }: { k: KeyResilience; t: TFunc }) {
  const flags: { label: string; cls: string }[] = [];
  if (!k.enabled) flags.push({ label: t("resilience.state.disabled"), cls: "text-text-muted" });
  if (k.breakerActive) flags.push({ label: t("resilience.state.breaker"), cls: "text-danger" });
  if (k.rateLimited) flags.push({ label: t("resilience.state.rateLimited"), cls: "text-warning" });
  if (k.balanceExhausted) flags.push({ label: t("resilience.state.exhausted"), cls: "text-danger" });
  if (k.overBudget) flags.push({ label: t("resilience.state.overBudget"), cls: "text-warning" });
  if (k.status === "down" && k.enabled) flags.push({ label: t("resilience.state.down"), cls: "text-warning" });
  const healthy = flags.length === 0;
  const dot = !k.enabled
    ? "bg-border"
    : healthy
      ? "bg-primary"
      : k.breakerActive || k.balanceExhausted
        ? "bg-danger"
        : "bg-warning";
  return (
    <div className="flex flex-wrap items-center gap-x-3 gap-y-1 px-4 py-2 text-sm hover:bg-surface-hover/40">
      <span className={`h-2 w-2 shrink-0 rounded-full ${dot}`} aria-hidden />
      <span className="max-w-[220px] truncate text-text-secondary">{k.keyName}</span>
      {healthy && k.enabled && <span className="text-xs text-text-muted">{t("resilience.state.healthy")}</span>}
      {flags.map((f) => (
        <span key={f.label} className={`text-xs ${f.cls}`}>
          {f.label}
        </span>
      ))}
      {k.failCount > 0 && !healthy && (
        <span className="text-[11px] text-text-muted">{t("resilience.failCount", { n: String(k.failCount) })}</span>
      )}
      {k.inFlight > 0 && (
        <span className="ml-auto font-mono text-[11px] tabular-nums text-text-muted">
          {t("resilience.inFlight", { n: String(k.inFlight), max: String(k.maxInFlight) })}
        </span>
      )}
    </div>
  );
}
