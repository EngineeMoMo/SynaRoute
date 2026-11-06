import { Activity, ArrowRight, CheckCircle2, Circle, CircleAlert, Clock3, FileCheck2, LockKeyhole, RefreshCw, ScrollText, ShieldCheck, Waypoints } from "lucide-react";
import type { ReactNode } from "react";
import { Button } from "@/components/ui/Button";
import { PageHeader } from "@/components/ui/PageHeader";
import { InlineAlert } from "@/components/ui/InlineAlert";
import { CATEGORY_ICONS } from "@/lib/categoryIcons";
import { useT } from "@/lib/useT";
import { useStore } from "@/store";
import { useRoutingOverview } from "@/lib/useRoutingOverview";
import { attemptLabel, connectionReason, type CategoryOverview, type LatestAttempt } from "@/lib/routingOverview";
import type { CategoryType } from "@/types";

function Field({ label, children }: { label: string; children: ReactNode }) {
  return <div className="grid grid-cols-[minmax(110px,0.7fr)_minmax(0,1fr)] gap-3 text-xs leading-5">
    <dt className="text-text-secondary">{label}</dt>
    <dd className="min-w-0 break-words font-medium text-text-primary [overflow-wrap:anywhere]">{children}</dd>
  </div>;
}

function Attempt({ attempt }: { attempt: LatestAttempt | null }) {
  const t = useT();
  const lang = useStore((s) => s.lang);
  if (!attempt) return <div className="flex min-h-36 items-center gap-3 rounded-control border border-dashed border-border-strong px-4 py-5">
    <Activity size={21} className="shrink-0 text-text-secondary" aria-hidden />
    <div><p className="text-sm font-medium">{t("overview.empty")}</p><p className="mt-1 max-w-lg text-xs leading-5 text-text-secondary">{t("overview.emptyHint")}</p></div>
  </div>;
  const Icon = !attempt.ok ? CircleAlert : attempt.streaming ? Activity : CheckCircle2;
  return <div className="space-y-3">
    <div className="flex flex-wrap items-start justify-between gap-2">
      <span className="min-w-0 break-all text-base font-semibold" title={t("overview.keyId", { id: attempt.keyId })}>{attempt.keyName}</span>
      <span className="flex items-center gap-1.5 text-xs text-text-primary"><Icon size={14} className={attempt.ok ? "text-primary" : "text-danger"} aria-hidden />{t(`overview.${attemptLabel(attempt)}`)}</span>
    </div>
    <div className="grid grid-cols-[minmax(0,1fr)_auto_minmax(0,1fr)] items-center gap-3 rounded-control bg-surface-hover px-3 py-3">
      <div className="min-w-0"><p className="mb-1 text-xs text-text-secondary">{t("overview.requested")}</p><p className="break-all text-sm font-medium">{attempt.requestedModel || "—"}</p></div>
      <ArrowRight size={16} className="text-text-secondary" aria-hidden />
      <div className="min-w-0"><p className="mb-1 text-xs text-text-secondary">{t("overview.actual")}</p><p className="break-all text-sm font-medium">{attempt.realModel || "—"}</p></div>
    </div>
    <div className="flex flex-wrap gap-x-4 gap-y-1 text-xs text-text-secondary">
      <span>{attempt.status === null ? t("overview.noStatus") : `HTTP ${attempt.status}`}</span>
      <span className="tabular-nums">{t("overview.latency", { ms: attempt.latencyMs })}</span>
      <time dateTime={new Date(attempt.observedAt).toISOString()}>{t("overview.observed", { time: new Date(attempt.observedAt).toLocaleString(lang === "zh" ? "zh-CN" : "en-US") })}</time>
    </div>
  </div>;
}

function ClientRow({ row, onManage }: { row: CategoryOverview; onManage: (category: CategoryType) => void }) {
  const t = useT();
  const Icon = CATEGORY_ICONS[row.categoryId];
  const connection = row.connection;
  const warning = connection.state === "needs_attention" || connection.state === "unknown";
  const ConnectionIcon = warning ? CircleAlert : connection.state === "configured" ? FileCheck2 : Circle;
  return <article className="overflow-hidden rounded-card border border-border bg-surface" aria-labelledby={`overview-${row.categoryId}`} data-category={row.categoryId}>
    <header className="flex flex-wrap items-center justify-between gap-3 border-b border-border px-5 py-3.5">
      <div className="flex min-w-0 flex-wrap items-center gap-x-4 gap-y-2">
        <h2 id={`overview-${row.categoryId}`} className="flex items-center gap-2.5 text-base font-semibold"><Icon size={19} className="text-primary" aria-hidden />{t(`nav.${row.categoryId}`)}</h2>
        <span className="flex items-center gap-1.5 text-xs"><span className={`h-2 w-2 rounded-full ${row.running ? "bg-primary" : "bg-border-strong"}`} aria-hidden />{t(row.running ? "overview.running" : "overview.stopped")}</span>
        <code className="break-all text-xs text-text-secondary">{row.endpoint}</code>
      </div>
      <Button size="sm" variant="outline" onClick={() => onManage(row.categoryId)} aria-label={`${t("overview.manage")} · ${t(`nav.${row.categoryId}`)}`}>{t("overview.manage")}</Button>
    </header>
    <div className="grid grid-cols-1 divide-y divide-border lg:grid-cols-[minmax(0,0.9fr)_minmax(0,1.1fr)] lg:divide-x lg:divide-y-0">
      <section className="min-w-0 px-5 py-4" aria-label={t("overview.configTitle")}>
        <div className="mb-3 flex flex-wrap items-center justify-between gap-2"><h3 className="text-sm font-semibold">{t("overview.configTitle")}</h3><span className="text-xs tabular-nums text-text-secondary">{t("overview.keys", { enabled: row.enabledCount, total: row.keyCount })}</span></div>
        <div className={`mb-4 rounded-control px-3 py-2.5 ${warning ? "bg-warning/8" : "bg-surface-hover"}`}>
          <p className="flex items-center gap-2 text-xs font-medium"><ConnectionIcon size={15} className={warning ? "text-warning" : "text-text-secondary"} aria-hidden />{t(`overview.${connection.state}`)}</p>
          <p className="mt-1 text-xs leading-5 text-text-secondary">{t(connectionReason(connection.reason))}</p>
        </div>
        <dl className="space-y-1.5">
          <Field label={t("overview.primary")}>{row.primaryKey ?? t("overview.noPrimary")}</Field>
          <Field label={t(row.categoryId === "codex" ? "overview.modelFallback" : "overview.model")}>{row.configuredModel ?? t("overview.followClient")}</Field>
          {row.categoryId === "codex" && <Field label={t("overview.effort")}>{row.configuredEffort ?? t("overview.followClient")}</Field>}
          <Field label={t("overview.clientModel")}>{connection.clientModel ?? t(connection.state === "unknown" ? "overview.clientUnknown" : row.categoryId === "claude-desktop" ? "overview.clientChoice" : "overview.clientUnset")}</Field>
        </dl>
      </section>
      <section className="min-w-0 px-5 py-4" aria-label={t("overview.latestTitle")}>
        <h3 className="mb-4 flex items-center gap-2 text-sm font-semibold"><Clock3 size={15} className="text-text-secondary" aria-hidden />{t("overview.latestTitle")}</h3>
        <Attempt attempt={row.latest} />
      </section>
    </div>
  </article>;
}

export function RoutingOverviewPage({ onManage, onOpenLogs, onOpenReliability, onOpenPolicies }: {
  onOpenPolicies: () => void;
  onManage: (category: CategoryType) => void;
  onOpenLogs: () => void;
  onOpenReliability: () => void;
}) {
  const t = useT();
  const lang = useStore((s) => s.lang);
  const { data, failed, loading, refresh } = useRoutingOverview();
  return <div className="h-full overflow-y-auto" data-testid="routing-overview">
    <PageHeader icon={Waypoints} title={t("nav.overview")} description={t("overview.description")} actions={
      <Button size="sm" variant="outline" onClick={() => void refresh()} disabled={loading}><RefreshCw size={14} className={loading ? "animate-spin motion-reduce:animate-none" : ""} aria-hidden />{t("overview.refresh")}</Button>
    } />
    <div className="mx-auto max-w-7xl space-y-4 px-6 py-5">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <p className="text-xs leading-5 text-text-secondary">{t("overview.readOnly")}</p>
        <div className="flex flex-wrap gap-2"><Button size="sm" variant="outline" onClick={onOpenPolicies}>{t("routing.manage")}</Button><Button size="sm" variant="ghost" onClick={onOpenLogs}><ScrollText size={14} aria-hidden />{t("overview.logs")}</Button><Button size="sm" variant="ghost" onClick={onOpenReliability}><ShieldCheck size={14} aria-hidden />{t("overview.reliability")}</Button></div>
      </div>
      {failed && <InlineAlert tone="warning" className="text-text-primary" role="alert">{t(data ? "overview.stale" : "overview.error")}</InlineAlert>}
      {data?.vaultLocked && <InlineAlert tone="warning" className="text-text-primary" icon={LockKeyhole}>{t("overview.vaultLocked")}</InlineAlert>}
      {!data && !failed && <p role="status" className="py-8 text-sm text-text-secondary">{t("common.loading")}</p>}
      {data && <>
        {data.categories.map((row) => <ClientRow key={row.categoryId} row={row} onManage={onManage} />)}
        <div className="grid gap-3 text-xs leading-5 text-text-secondary lg:grid-cols-2"><p>{t("overview.configNote")}</p><p>{t("overview.latestNote")}</p></div>
        <p className="text-xs tabular-nums text-text-secondary">{t("overview.updated", { time: new Date(data.capturedAt).toLocaleString(lang === "zh" ? "zh-CN" : "en-US") })}</p>
      </>}
    </div>
  </div>;
}
