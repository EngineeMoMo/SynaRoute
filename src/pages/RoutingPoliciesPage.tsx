import { useEffect, useRef, useState } from "react";
import { RefreshCw, SlidersHorizontal } from "lucide-react";
import { Button } from "@/components/ui/Button";
import { PageHeader } from "@/components/ui/PageHeader";
import { InlineAlert } from "@/components/ui/InlineAlert";
import { ConfirmDialog } from "@/components/ui/ConfirmDialog";
import { SceneProfilesPanel } from "@/components/SceneProfilesPanel";
import { SessionAffinityPanel } from "@/components/SessionAffinityPanel";
import { IntentRoutingPanel } from "@/components/IntentRoutingPanel";
import { SensitiveContentPanel } from "@/components/SensitiveContentPanel";
import { RoutingSyncResults } from "@/components/routing/RoutingFields";
import { useRoutingPolicies } from "@/lib/useRoutingPolicies";
import { useT } from "@/lib/useT";
import { api } from "@/lib/bridge";
import { activityKey, parseSensitiveTerms, samePolicy, type CategoryPolicy, type RoutingSyncResult } from "@/lib/routingPolicies";
import type { CategoryType } from "@/types";

export function RoutingPoliciesPage() {
  const t = useT();
  const { data, failed, loading, refresh, accept } = useRoutingPolicies();
  const [tab, setTab] = useState("scenes");
  const [category, setCategory] = useState<CategoryType>("claude-cli");
  const [draft, setDraft] = useState<{ policy: CategoryPolicy; revision: number } | null>(null);
  const [terms, setTerms] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const inFlight = useRef(false);
  const [error, setError] = useState("");
  const [note, setNote] = useState("");
  const [sync, setSync] = useState<RoutingSyncResult[]>([]);
  const [confirm, setConfirm] = useState<"save" | "sync" | "discard" | null>(null);
  const [nextCategory, setNextCategory] = useState<CategoryType | null>(null);
  const row = data?.categories.find((row) => row.categoryId === category);
  const policy = draft?.policy ?? row?.policy;
  const dirty = !!row && ((!!draft && !samePolicy(draft.policy, row.policy)) || terms !== null);
  const stale = !!draft && !!data && draft.revision !== data.revision;
  const onError = (error: unknown) => setError(String(error));
  const run = async (work: () => Promise<void>) => {
    if (inFlight.current) return;
    inFlight.current = true; setBusy(true); setError(""); setNote("");
    try { await work(); } catch (error) { onError(error); } finally { inFlight.current = false; setBusy(false); }
  };
  const result = (message: string, results: RoutingSyncResult[]) => { setNote(message); setSync(results); };
  const change = (policy: CategoryPolicy) => { if (data) setDraft({ policy, revision: draft?.revision ?? data.revision }); };
  const discard = () => { setDraft(null); setTerms(null); setError(""); if (nextCategory) { setCategory(nextCategory); setSync([]); setNote(""); } setNextCategory(null); setConfirm(null); void refresh(); };
  useEffect(() => {
    const warn = (event: BeforeUnloadEvent) => { if (dirty) { event.preventDefault(); event.returnValue = ""; } };
    window.addEventListener("beforeunload", warn); return () => window.removeEventListener("beforeunload", warn);
  }, [dirty]);
  return <div className="h-full overflow-y-auto" data-testid="routing-policies">
    <PageHeader icon={SlidersHorizontal} title={t("nav.policies")} description={t("routing.description")} actions={<Button size="sm" variant="outline" disabled={loading || busy} onClick={() => void refresh()}><RefreshCw size={14} />{t("overview.refresh")}</Button>} />
    <div className="mx-auto max-w-6xl space-y-4 px-6 py-5">
      {failed && <InlineAlert tone="warning" role="alert">{t(data ? "routing.stale" : "routing.loadFailed")}</InlineAlert>}
      {error && <InlineAlert tone="warning" role="alert">{error}</InlineAlert>}
      {note && <p className="break-words text-sm" role="status">{note}</p>}
      <RoutingSyncResults results={sync} />
      <div className="flex flex-wrap gap-2 border-b border-border pb-3" role="tablist" aria-label={t("nav.policies")}>{["scenes", "affinity", "intent", "privacy"].map((value) => <Button key={value} role="tab" aria-selected={tab === value} variant={tab === value ? "primary" : "ghost"} disabled={busy} onClick={() => setTab(value)}>{t(`routing.${value}`)}</Button>)}</div>
      {!data && !failed && <p className="text-sm text-text-secondary">{t("common.loading")}</p>}
      {data && <>
        <div hidden={tab !== "scenes"}><SceneProfilesPanel data={data} accept={accept} onResult={result} busy={busy || dirty} run={run} /></div>
        <div hidden={tab === "scenes"} className="space-y-4">
          <div className="flex flex-wrap items-center justify-between gap-3">
            <div className="flex flex-wrap gap-2" aria-label={t("routing.category")}>{data.categories.map((item) => <Button size="sm" key={item.categoryId} variant={category === item.categoryId ? "primary" : "outline"} disabled={busy} onClick={() => { if (item.categoryId === category) return; if (dirty) { setNextCategory(item.categoryId); setConfirm("discard"); } else { setCategory(item.categoryId); setDraft(null); setTerms(null); setSync([]); setNote(""); setError(""); } }}>{t(`nav.${item.categoryId}`)}</Button>)}</div>
            <div className="flex gap-2"><Button size="sm" disabled={busy || failed || !dirty || stale} onClick={() => setConfirm("save")}>{t("routing.save")}</Button><Button size="sm" variant="ghost" disabled={busy || (!draft && terms === null)} onClick={() => setConfirm("discard")}>{t("routing.discard")}</Button></div>
          </div>
          {dirty && <p className="text-xs text-warning">{t("routing.dirty")}</p>}
          {stale && <InlineAlert tone="warning">{t("routing.changed")}</InlineAlert>}
          {data.vaultLocked && <InlineAlert tone="warning">{t("routing.locked")}</InlineAlert>}
          {row && !row.keys.length && <InlineAlert>{t("routing.noKeys")}</InlineAlert>}
          {row?.issues.map((issue) => <InlineAlert key={issue} tone="warning">{issue}</InlineAlert>)}
          {row && policy && <>
            <div hidden={tab !== "affinity"}><SessionAffinityPanel value={policy.affinity} onChange={(affinity) => change({ ...policy, affinity })} busy={busy} onClear={() => run(async () => { await api.clearRoutingSessions(category); setNote(t("routing.affinity.cleared")); void refresh(); })} /></div>
            <div hidden={tab !== "intent"}><IntentRoutingPanel row={row} groups={policy.groups} onChange={(groups) => change({ ...policy, groups })} busy={busy} onError={onError} /></div>
            <div hidden={tab !== "privacy"}><SensitiveContentPanel key={category} category={category} value={policy.privacy} onChange={(privacy) => change({ ...policy, privacy })} terms={terms} onTermsChange={(next) => { change(policy); setTerms(next); }} locked={data.vaultLocked} busy={busy} onError={onError} /></div>
          </>}
          <div className="space-y-2 border-t border-border pt-4"><Button variant="outline" size="sm" disabled={busy || dirty} onClick={() => setConfirm("sync")}>{t("routing.sync")}</Button><p className="text-xs leading-5 text-text-secondary">{t("routing.syncNote")}</p></div>
          <section className="space-y-2 border-t border-border pt-4"><h3 className="text-sm font-semibold">{t("routing.activity")}</h3>{row?.activity ? <dl className="grid gap-2 text-xs sm:grid-cols-2">{(["identity", "selection", "privacy"] as const).map((kind) => <div key={kind}><dt className="text-text-secondary">{t(`routing.activity.${kind}`)}</dt><dd>{t(activityKey(kind, row.activity![kind]))}</dd></div>)}<div><dt>{t("routing.activity.model")}</dt><dd className="break-all">{row.activity.selectedModel ?? "—"}</dd></div><div><dt>{t("routing.activity.key")}</dt><dd className="break-all">{row.keys.find((k) => k.id === row.activity?.keptKey)?.name ?? row.activity.keptKey ?? "—"}</dd></div>{Object.entries(row.activity.findings).map(([kind, count]) => <div key={kind}>{t(`routing.finding.${["api_key", "token", "private_key"].includes(kind) ? "credential" : kind}`)}: {count}</div>)}</dl> : <p className="text-xs text-text-secondary">{t("routing.activity.empty")}</p>}</section>
        </div>
      </>}
    </div>
    {confirm && <ConfirmDialog tone={confirm === "discard" ? "danger" : "primary"} title={t(`routing.${confirm === "discard" ? "discardConfirm" : confirm === "sync" ? "syncConfirm" : "saveConfirm"}`)} description={t(`routing.${confirm === "discard" ? "discardBody" : confirm === "sync" ? "syncConfirmBody" : "saveConfirmBody"}`)} confirmLabel={t("common.confirm")} cancelLabel={t("common.cancel")} busy={busy} onCancel={() => { setConfirm(null); setNextCategory(null); }} onConfirm={() => { if (confirm === "discard") discard(); else void run(async () => { if (confirm === "sync") setSync(await api.syncRoutingClients([category])); else if (policy && data) { const saved = await api.saveRoutingPolicy(category, policy, terms === null ? null : parseSensitiveTerms(terms), draft?.revision ?? data.revision); accept(saved.view); setDraft(null); setTerms(null); result(t("routing.saved"), saved.sync); } setConfirm(null); }); }} />}
  </div>;
}
