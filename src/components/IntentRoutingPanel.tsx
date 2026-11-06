import { useState } from "react";
import { ArrowDown, ArrowUp, Copy, Plus, Trash2 } from "lucide-react";
import { Button } from "@/components/ui/Button";
import { ConfirmDialog } from "@/components/ui/ConfirmDialog";
import { useT } from "@/lib/useT";
import { groupModelId, reorderRule, type RouteGroup, type RoutingCategoryView } from "@/lib/routingPolicies";
import { RoutingField, RoutingSelect, routingInput } from "./routing/RoutingFields";

export function IntentRoutingPanel({ row, groups, onChange, busy, onError }: {
  row: RoutingCategoryView; groups: RouteGroup[]; onChange: (groups: RouteGroup[]) => void; busy: boolean; onError: (error: unknown) => void;
}) {
  const t = useT();
  const [remove, setRemove] = useState<string | null>(null);
  const [copied, setCopied] = useState<string | null>(null);
  const models = [{ value: "", label: t("routing.intent.choose") }, ...row.models.map((value) => ({ value, label: value }))];
  const patch = (id: string, change: Partial<RouteGroup>) => onChange(groups.map((group) => group.id === id ? { ...group, ...change } : group));
  return <section className="space-y-4">
    <p className="max-w-3xl text-sm leading-6 text-text-secondary">{t("routing.intent.hint")}</p>
    <p className="max-w-3xl text-xs leading-5 text-text-secondary">{t("routing.intent.cost")}</p>
    {!groups.length && <p className="rounded-control border border-dashed border-border p-5 text-sm text-text-secondary">{t("routing.intent.empty")}</p>}
    {groups.map((group) => <article key={group.id} className="space-y-4 rounded-card border border-border p-4">
      <div className="flex flex-wrap items-end gap-4">
        <div className="min-w-48 flex-1"><RoutingField label={t("routing.intent.name")}><input className={routingInput} maxLength={120} value={group.name} disabled={busy} onChange={(e) => patch(group.id, { name: e.target.value })} /></RoutingField></div>
        <label className="flex items-center gap-2 py-2 text-sm"><input type="checkbox" checked={group.enabled} disabled={busy} onChange={(e) => patch(group.id, { enabled: e.target.checked })} />{t("routing.intent.enable")}</label>
        <Button variant="ghost" disabled={busy} aria-label={t("routing.intent.removeGroup")} onClick={() => setRemove(group.id)}><Trash2 size={16} /></Button>
      </div>
      <div className="flex items-center gap-2 text-xs text-text-secondary"><span>{t("routing.intent.entry")}</span><code className="min-w-0 flex-1 break-all">{groupModelId(group)}</code><Button size="sm" variant="ghost" aria-label={t("routing.intent.copy")} onClick={() => { void navigator.clipboard.writeText(groupModelId(group)).then(() => setCopied(group.id)).catch(onError); }}><Copy size={14} /></Button>{copied === group.id && <span role="status">{t("routing.intent.copied")}</span>}</div>
      <div className="grid gap-3 lg:grid-cols-3">
        <RoutingField label={t("routing.intent.default")}><RoutingSelect value={group.defaultModel} options={models} disabled={busy} onChange={(e) => patch(group.id, { defaultModel: e.target.value })} /></RoutingField>
        <RoutingField label={t("routing.intent.judgeKey")}><RoutingSelect value={group.judge?.keyId ?? ""} disabled={busy} options={[{ value: "", label: t("routing.intent.choose") }, ...row.keys.filter((k) => k.enabled).map((k) => ({ value: k.id, label: k.name }))]} onChange={(e) => patch(group.id, { judge: e.target.value ? { keyId: e.target.value, model: "" } : null })} /></RoutingField>
        <RoutingField label={t("routing.intent.judgeModel")}><RoutingSelect value={group.judge?.model ?? ""} disabled={busy || !group.judge} options={[{ value: "", label: t("routing.intent.choose") }, ...(row.keys.find((k) => k.id === group.judge?.keyId)?.models ?? []).map((value) => ({ value, label: value }))]} onChange={(e) => { if (group.judge) patch(group.id, { judge: { ...group.judge, model: e.target.value } }); }} /></RoutingField>
      </div>
      <div className="space-y-3">{group.rules.map((rule, index) => <div key={rule.id} className="grid gap-3 border-t border-border pt-3 lg:grid-cols-[minmax(0,1fr)_minmax(0,1fr)_auto]">
        <RoutingField label={t("routing.intent.rule", { n: index + 1 })}><textarea rows={2} className={routingInput} placeholder={t("routing.intent.rulePlaceholder")} maxLength={512} disabled={busy} value={rule.intent} onChange={(e) => patch(group.id, { rules: group.rules.map((r) => r.id === rule.id ? { ...r, intent: e.target.value } : r) })} /></RoutingField>
        <RoutingField label={t("routing.intent.target")}><RoutingSelect value={rule.model} options={models} disabled={busy} onChange={(e) => patch(group.id, { rules: group.rules.map((r) => r.id === rule.id ? { ...r, model: e.target.value } : r) })} /></RoutingField>
        <div className="flex items-center gap-1"><Button variant="ghost" size="sm" disabled={busy || index === 0} aria-label={t("routing.intent.moveUp")} onClick={() => patch(group.id, { rules: reorderRule(group.rules, index, -1) })}><ArrowUp size={14} /></Button><Button variant="ghost" size="sm" disabled={busy || index === group.rules.length - 1} aria-label={t("routing.intent.moveDown")} onClick={() => patch(group.id, { rules: reorderRule(group.rules, index, 1) })}><ArrowDown size={14} /></Button><Button variant="ghost" size="sm" disabled={busy} aria-label={t("routing.intent.removeRule")} onClick={() => patch(group.id, { rules: group.rules.filter((r) => r.id !== rule.id) })}><Trash2 size={14} /></Button></div>
      </div>)}</div>
      <Button variant="outline" size="sm" disabled={busy || group.rules.length >= 16} onClick={() => patch(group.id, { rules: [...group.rules, { id: crypto.randomUUID(), intent: "", model: "" }] })}><Plus size={14} />{t("routing.intent.addRule")}</Button>
    </article>)}
    <Button variant="outline" disabled={busy || groups.length >= 16} onClick={() => onChange([...groups, { id: crypto.randomUUID(), name: "", enabled: false, defaultModel: "", judge: null, rules: [] }])}><Plus size={15} />{t("routing.intent.add")}</Button>
    <p className="text-xs leading-5 text-text-secondary">{t("routing.intent.limit")}</p>
    {remove && <ConfirmDialog title={t("routing.intent.removeTitle")} description={t("routing.intent.removeBody")} confirmLabel={t("common.remove")} cancelLabel={t("common.cancel")} onCancel={() => setRemove(null)} onConfirm={() => { onChange(groups.filter((g) => g.id !== remove)); setRemove(null); }} />}
  </section>;
}
