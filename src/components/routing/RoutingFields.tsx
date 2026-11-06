import type { ReactNode, SelectHTMLAttributes } from "react";
import { useT } from "@/lib/useT";
import type { RoutingSyncResult } from "@/lib/routingPolicies";

export const routingInput = "w-full min-w-0 rounded-control border border-border bg-surface px-3 py-2 text-sm text-text-primary focus:outline-none focus:ring-2 focus:ring-primary/50 disabled:opacity-50";
export function RoutingField({ label, children }: { label: string; children: ReactNode }) {
  return <label className="block min-w-0 space-y-1.5"><span className="text-xs font-medium text-text-secondary">{label}</span>{children}</label>;
}
export function RoutingSelect({ options, ...props }: SelectHTMLAttributes<HTMLSelectElement> & { options: { value: string; label: string }[] }) {
  const t = useT();
  const value = String(props.value ?? "");
  return <select {...props} className={routingInput}>
    {value && !options.some((o) => o.value === value) && <option value={value}>{t("routing.scene.unavailable", { value })}</option>}
    {options.map((o) => <option key={o.value} value={o.value}>{o.label}</option>)}
  </select>;
}
export function RoutingSyncResults({ results }: { results: RoutingSyncResult[] }) {
  const t = useT();
  return <div className="space-y-2" role="status">{results.map((result) => <div key={result.categoryId} className="rounded-control border border-border p-3 text-xs leading-5">
    <p className="font-medium">{t(`nav.${result.categoryId}`)}: {t(`routing.sync.${["synced", "managed", "unchanged", "stopped", "not_managed", "failed"].includes(result.status) ? result.status : "failed"}`)}</p>
    <p className="break-words text-text-secondary">{result.message}</p>
    {result.backupDir && <p className="break-all text-text-secondary">{t("routing.backup", { path: result.backupDir })}</p>}
  </div>)}</div>;
}
