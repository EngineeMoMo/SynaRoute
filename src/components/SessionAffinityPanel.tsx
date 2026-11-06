import { useState } from "react";
import { Button } from "@/components/ui/Button";
import { ConfirmDialog } from "@/components/ui/ConfirmDialog";
import { useT } from "@/lib/useT";
import type { AffinityMode } from "@/lib/routingPolicies";
import { RoutingField, RoutingSelect } from "./routing/RoutingFields";

export function SessionAffinityPanel({ value, onChange, onClear, busy }: {
  value: AffinityMode; onChange: (value: AffinityMode) => void; onClear: () => Promise<void>; busy: boolean;
}) {
  const t = useT();
  const [confirm, setConfirm] = useState(false);
  return <section className="space-y-4">
    <p className="max-w-3xl text-sm leading-6 text-text-secondary">{t("routing.affinity.hint")}</p>
    <div className="max-w-sm"><RoutingField label={t("routing.affinity")}><RoutingSelect value={value} disabled={busy} onChange={(e) => onChange(e.target.value as AffinityMode)} options={["off", "turn", "session"].map((value) => ({ value, label: t(`routing.affinity.${value}`) }))} /></RoutingField></div>
    <p className="max-w-3xl text-xs leading-5 text-text-secondary">{t("routing.affinity.identityNote")}</p>
    <Button variant="outline" disabled={busy} onClick={() => setConfirm(true)}>{t("routing.affinity.clear")}</Button>
    {confirm && <ConfirmDialog title={t("routing.affinity.clearTitle")} description={t("routing.affinity.clearBody")} confirmLabel={t("common.confirm")} cancelLabel={t("common.cancel")} busy={busy} onCancel={() => setConfirm(false)} onConfirm={() => { void onClear().then(() => setConfirm(false)); }} />}
  </section>;
}
