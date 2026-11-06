import { useState } from "react";
import { Button } from "@/components/ui/Button";
import { ConfirmDialog } from "@/components/ui/ConfirmDialog";
import { api } from "@/lib/bridge";
import { useT } from "@/lib/useT";
import { ROUTING_EFFORTS, routingChoices, sceneMatches, type RoutingPolicyView, type RoutingScene, type RoutingSyncResult, type ScenePreview } from "@/lib/routingPolicies";
import { RoutingField, RoutingSelect, routingInput } from "./routing/RoutingFields";

export function SceneProfilesPanel({ data, accept, onResult, busy, run }: {
  data: RoutingPolicyView; accept: (view: RoutingPolicyView) => void;
  onResult: (message: string, sync: RoutingSyncResult[]) => void; busy: boolean;
  run: (work: () => Promise<void>) => Promise<void>;
}) {
  const t = useT();
  const [draft, setDraft] = useState<RoutingScene | null>(null);
  const [draftRevision, setDraftRevision] = useState(data.revision);
  const [preview, setPreview] = useState<ScenePreview | null>(null);
  const [confirm, setConfirm] = useState<"save" | string | null>(null);
  const open = (scene: RoutingScene) => { setDraft(structuredClone(scene)); setDraftRevision(data.revision); };
  const save = async () => { if (!draft) return; accept(await api.saveRoutingScene(draft, draftRevision)); setDraft(null); setConfirm(null); onResult(t("routing.scene.saved"), []); };
  const routeLabel = (route: RoutingScene["routes"][number]) => {
    const row = data.categories.find((r) => r.categoryId === route.categoryId);
    return [row?.keys.find((k) => k.id === route.primaryKeyId)?.name ?? route.primaryKeyId ?? t("routing.scene.keepPrimary"), route.model ?? t("routing.scene.follow"), ...(route.categoryId === "codex" ? [route.effort ?? t("routing.scene.noEffort")] : [])].join(" · ");
  };
  return <section className="space-y-4">
    <p className="max-w-3xl text-sm leading-6 text-text-secondary">{t("routing.scene.hint")}</p>
    <Button variant="outline" disabled={busy || !!draft || data.scenes.length >= 32} onClick={() => open({ id: "", name: "", routes: data.categories.map((row) => ({ ...row.current })) })}>{t("routing.scene.new")}</Button>
    {!data.scenes.length && !draft && <p className="rounded-control border border-dashed border-border p-5 text-sm text-text-secondary">{t("routing.scene.empty")}</p>}
    {data.scenes.map((scene) => <article key={scene.id} className="flex flex-wrap items-center justify-between gap-3 border-b border-border py-3">
      <div className="min-w-0 flex-1"><h3 className="break-words text-sm font-semibold">{scene.name}</h3><p className="mt-1 text-xs text-text-secondary">{t(sceneMatches(scene, data.categories) ? "routing.scene.matches" : "routing.scene.differs")}</p></div>
      <div className="flex flex-wrap gap-2"><Button size="sm" variant="outline" disabled={busy || !!draft} onClick={() => void run(async () => setPreview(await api.previewRoutingScene(scene.id)))}>{t("routing.scene.preview")}</Button><Button size="sm" variant="ghost" disabled={busy || !!draft} onClick={() => open(scene)}>{t("common.edit")}</Button><Button size="sm" variant="ghost" disabled={busy || !!draft} onClick={() => setConfirm(scene.id)}>{t("common.delete")}</Button></div>
    </article>)}
    {draft && <div className="space-y-4 rounded-card border border-primary/30 p-4">
      <h3 className="text-sm font-semibold">{t(draft.id ? "routing.scene.edit" : "routing.scene.new")}</h3>
      <RoutingField label={t("routing.scene.name")}><input className={routingInput} value={draft.name} maxLength={120} disabled={busy} placeholder={t("routing.scene.namePlaceholder")} onChange={(e) => setDraft({ ...draft, name: e.target.value })} /></RoutingField>
      {data.categories.map((row) => {
        const route = draft.routes.find((r) => r.categoryId === row.categoryId);
        const patch = (change: Partial<RoutingScene["routes"][number]>) => setDraft({ ...draft, routes: draft.routes.map((r) => r.categoryId === row.categoryId ? { ...r, ...change } : r) });
        return <div key={row.categoryId} className="space-y-3 border-t border-border pt-3">
          <label className="flex items-center gap-2 text-sm font-medium"><input type="checkbox" checked={!!route} disabled={busy} onChange={(e) => setDraft({ ...draft, routes: e.target.checked ? [...draft.routes, { ...row.current }] : draft.routes.filter((r) => r.categoryId !== row.categoryId) })} />{t("routing.scene.include", { category: t(`nav.${row.categoryId}`) })}</label>
          {route && <div className="grid gap-3 lg:grid-cols-3">
            <RoutingField label={t("routing.scene.primary")}><RoutingSelect value={route.primaryKeyId ?? ""} disabled={busy} options={[{ value: "", label: t("routing.scene.keepPrimary") }, ...row.keys.filter((k) => k.enabled).map((k) => ({ value: k.id, label: k.name }))]} onChange={(e) => patch({ primaryKeyId: e.target.value || null })} /></RoutingField>
            <RoutingField label={t("routing.scene.model")}><RoutingSelect value={route.model ?? ""} disabled={busy} options={[{ value: "", label: t("routing.scene.follow") }, ...routingChoices(row)]} onChange={(e) => patch({ model: e.target.value || null })} /></RoutingField>
            {row.categoryId === "codex" && <RoutingField label={t("routing.scene.effort")}><RoutingSelect value={route.effort ?? ""} disabled={busy} options={[{ value: "", label: t("routing.scene.noEffort") }, ...ROUTING_EFFORTS.map((value) => ({ value, label: value }))]} onChange={(e) => patch({ effort: e.target.value || null })} /></RoutingField>}
          </div>}
        </div>;
      })}
      {!draft.routes.length && <p className="text-xs text-danger">{t("routing.scene.selectOne")}</p>}
      {draftRevision !== data.revision && <p className="text-xs text-warning">{t("routing.changed")}</p>}
      <div className="flex gap-2"><Button disabled={busy || !draft.name.trim() || !draft.routes.length || draftRevision !== data.revision} onClick={() => { if (draft.id) setConfirm("save"); else void run(save); }}>{t("common.save")}</Button><Button variant="ghost" disabled={busy} onClick={() => setDraft(null)}>{t("common.cancel")}</Button></div>
    </div>}
    {confirm && <ConfirmDialog title={t(confirm === "save" ? "routing.scene.replaceTitle" : "routing.scene.deleteTitle")} description={t(confirm === "save" ? "routing.scene.replaceBody" : "routing.scene.deleteBody")} confirmLabel={t(confirm === "save" ? "common.save" : "common.delete")} cancelLabel={t("common.cancel")} busy={busy} onCancel={() => setConfirm(null)} onConfirm={() => void run(async () => { if (confirm === "save") await save(); else { accept(await api.deleteRoutingScene(confirm, data.revision)); setConfirm(null); } })} />}
    {preview && <ConfirmDialog tone="primary" title={t("routing.scene.previewTitle", { name: preview.name })} description={<span className="block space-y-3"><span className="block">{t("routing.scene.previewHint")}</span>{preview.changes.map((change) => <span key={change.after.categoryId} className="block space-y-1 border-t border-border pt-2"><strong className="block">{t(`nav.${change.after.categoryId}`)}</strong><span className="block break-all">{t("routing.scene.before")}: {routeLabel(change.before)}</span><span className="block break-all">{t("routing.scene.after")}: {routeLabel(change.after)}</span><span className="block">{change.client.message}</span>{change.paths.map((path) => <span key={path} className="block break-all text-xs">{path}</span>)}</span>)}</span>} confirmLabel={t("routing.scene.apply")} cancelLabel={t("common.cancel")} busy={busy} onCancel={() => setPreview(null)} onConfirm={() => void run(async () => { const token = preview.token; setPreview(null); const result = await api.applyRoutingScene(token); accept(result.view); onResult(t(result.changed ? "routing.scene.applied" : "routing.scene.noChange") + (result.backupPath ? ` ${t("routing.backup", { path: result.backupPath })}` : ""), result.sync); })} />}
  </section>;
}
