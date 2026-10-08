import { useMemo, useState } from "react";
import type { ResilienceOverview } from "@/lib/resilience";
import { profileHint, summarizeProfiles } from "@/lib/routeProfiles";
import { useT } from "@/lib/useT";
import { InlineAlert } from "./ui/InlineAlert";

export function RouteProfiles({ data }: { data: ResilienceOverview }) {
  const t = useT();
  const [days, setDays] = useState(7);
  const [search, setSearch] = useState("");
  const [mode, setMode] = useState("all");
  const names = new Map(data.categories.flatMap(c => c.keys.map(k => [k.keyId, k.keyName] as const)));
  const snapshot = data.profiles;
  const rows = useMemo(() => summarizeProfiles(snapshot, days), [snapshot, days]);
  const visible = rows.filter(r => (mode === "all" || r.streaming === (mode === "stream")) &&
    [names.get(r.keyId) ?? r.keyId, r.requestedModel, r.realModel, t(`nav.${r.categoryId}`)].join(" ").toLowerCase().includes(search.trim().toLowerCase()));
  return <section className="sr-panel p-4" aria-labelledby="route-profile-title">
    <h2 id="route-profile-title" className="text-base font-semibold text-text-primary">{t("profiles.title")}</h2>
    <p className="mt-1 max-w-3xl text-xs leading-relaxed text-text-secondary">{t("profiles.description")}</p>
    <p className="mt-1 text-xs text-text-muted">{t("profiles.since", { date: new Date(snapshot.since).toLocaleString() })}</p>
    {(snapshot.readOnly || snapshot.dropped > 0) && <InlineAlert tone="warning" className="mt-3">{snapshot.readOnly ? t("profiles.readOnly") : t("profiles.dropped", { n: snapshot.dropped })}</InlineAlert>}
    <div className="my-4 flex flex-wrap items-center gap-3">
      <label className="flex items-center gap-2 text-xs text-text-secondary">{t("profiles.window")}
        <select className="rounded-control border border-border bg-surface px-2 py-1.5 text-text-primary" value={days} onChange={e => setDays(Number(e.target.value))}>
          {[1, 7, 30].map(n => <option key={n} value={n}>{t("profiles.days", { n })}</option>)}
        </select>
      </label>
      <label className="flex items-center gap-2 text-xs text-text-secondary">{t("profiles.mode")}
        <select className="rounded-control border border-border bg-surface px-2 py-1.5 text-text-primary" value={mode} onChange={e => setMode(e.target.value)}>
          <option value="all">{t("profiles.all")}</option><option value="stream">{t("profiles.stream")}</option><option value="buffered">{t("profiles.buffered")}</option>
        </select>
      </label>
      <input aria-label={t("profiles.search")} placeholder={t("profiles.search")} value={search} onChange={e => setSearch(e.target.value)} className="min-w-0 flex-1 rounded-control border border-border bg-surface px-3 py-1.5 text-sm text-text-primary" />
    </div>
    {visible.length === 0 ? <p className="py-6 text-sm text-text-secondary">{t(rows.length ? "profiles.noMatch" : "profiles.empty")}</p> : <div className="overflow-x-auto">
      <table className="w-full min-w-[880px] text-left text-xs">
        <thead className="border-b border-border text-text-secondary"><tr>
          {["route", "model", "mode", "attempts", "success", "latency", "limits", "hint"].map(k => <th key={k} scope="col" className="whitespace-nowrap px-2 py-2 font-medium">{t(`profiles.${k}`)}</th>)}
        </tr></thead>
        <tbody className="divide-y divide-border">
          {visible.map(r => <tr key={JSON.stringify([r.categoryId, r.keyId, r.requestedModel, r.realModel, r.streaming])} className="text-text-primary hover:bg-surface-hover/40">
            <td className="max-w-48 break-words px-2 py-3">{names.get(r.keyId) ?? t("profiles.deleted")}<div className="mt-1 text-text-muted">{t(`nav.${r.categoryId}`)}</div></td>
            <td className="max-w-56 break-all px-2 py-3">{r.requestedModel || "—"}<div className="mt-1 text-text-muted">→ {r.realModel || "—"}</div></td>
            <td className="whitespace-nowrap px-2 py-3">{t(r.streaming ? "profiles.stream" : "profiles.buffered")}</td>
            <td className="px-2 py-3 tabular-nums">{r.attempts}</td>
            <td className="whitespace-nowrap px-2 py-3 tabular-nums">{r.attempts ? `${(100 * r.successes / r.attempts).toFixed(1)}%` : "—"}<div className="mt-1 text-text-muted">{r.successes}/{r.attempts}</div></td>
            <td className="whitespace-nowrap px-2 py-3 tabular-nums">{r.successes ? `${Math.round(r.successLatencyMs / r.successes)} ms` : "—"}</td>
            <td className="px-2 py-3 tabular-nums">{r.rateLimits}</td>
            <td className="min-w-36 max-w-56 px-2 py-3 leading-relaxed text-text-secondary">{t(`profiles.hint.${profileHint(r)}`)}</td>
          </tr>)}
        </tbody>
      </table>
    </div>}
    <p className="mt-3 max-w-3xl text-xs leading-relaxed text-text-secondary">{t("profiles.metricNote")}</p>
  </section>;
}
