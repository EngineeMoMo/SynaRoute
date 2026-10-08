import { useRef, useState } from "react";
import { api } from "@/lib/bridge";
import { useT } from "@/lib/useT";
import type { SharedImportResult, SharedTemplate } from "@/lib/sharedTemplate";
import { protocolLabel } from "@/types";
import { Card, CardContent, CardHeader, CardTitle } from "./ui/Card";
import { Button } from "./ui/Button";
import { InlineAlert } from "./ui/InlineAlert";

export function SharedConfigSection({ onChanged }: { onChanged: () => void }) {
  const t = useT();
  const [raw, setRaw] = useState("");
  const [preview, setPreview] = useState<SharedTemplate | null>(null);
  const [mode, setMode] = useState<"export" | "import">("import");
  const [busy, setBusy] = useState(false);
  const busyRef = useRef(false);
  const [error, setError] = useState("");
  const [message, setMessage] = useState("");
  const [receipt, setReceipt] = useState<SharedImportResult | null>(null);
  const fileRef = useRef<HTMLInputElement>(null);
  const run = async (action: () => Promise<void>) => {
    if (busyRef.current) return;
    busyRef.current = true; setBusy(true); setError(""); setMessage("");
    try { await action(); } catch (e) { setError(String(e)); }
    finally { busyRef.current = false; setBusy(false); }
  };
  const build = () => run(async () => {
    const p = await api.buildSharedTemplate();
    setMode("export"); setPreview(p); setRaw(JSON.stringify(p, null, 2));
  });
  return <Card>
    <CardHeader><CardTitle>{t("share.title")}</CardTitle></CardHeader>
    <CardContent className="space-y-3">
      <p className="text-xs leading-relaxed text-text-secondary">{t("share.description")}</p>
      <div className="flex flex-wrap gap-2">
        <Button size="sm" variant="secondary" disabled={busy} onClick={() => void build()}>{t("share.build")}</Button>
        <Button size="sm" variant="secondary" disabled={busy} onClick={() => { setMode("import"); setPreview(null); setMessage(""); setError(""); document.getElementById("shared-config-json")?.focus(); }}>{t("share.pasteImport")}</Button>
        <Button size="sm" variant="secondary" disabled={busy} onClick={() => fileRef.current?.click()}>{t("share.open")}</Button>
        <input ref={fileRef} type="file" accept=".json,application/json" className="hidden" aria-label={t("share.open")} disabled={busy} onChange={e => {
          const file = e.target.files?.[0]; e.target.value = "";
          if (file) void run(async () => {
            if (file.size > 1_048_576) throw new Error(t("share.tooLarge"));
            const text = await file.text();
            const p = await api.previewSharedTemplate(text);
            setMode("import"); setRaw(JSON.stringify(p, null, 2)); setPreview(p);
          });
        }} />
      </div>
      <label className="block text-xs text-text-secondary" htmlFor="shared-config-json">{t("share.json")}</label>
      <textarea id="shared-config-json" value={raw} disabled={busy} spellCheck={false} rows={5} maxLength={1_048_576}
        placeholder={t("share.placeholder")} className="w-full resize-y rounded-control border border-border bg-surface p-3 font-mono text-xs text-text-primary"
        onChange={e => { setRaw(e.target.value); setPreview(null); setMessage(""); setError(""); }} />
      {raw && !preview && <Button size="sm" variant="secondary" disabled={busy} onClick={() => void run(async () => { setPreview(await api.previewSharedTemplate(raw)); })}>{t("share.preview")}</Button>}
      {preview && <div className="space-y-3">
        <p className="text-sm font-medium text-text-primary">{t(mode === "export" ? "share.exportPreview" : "share.importPreview", { n: preview.routes.length })}</p>
        <ul className="max-h-48 overflow-y-auto divide-y divide-border text-xs text-text-secondary">
          {preview.routes.map((r, i) => <li key={i} className="py-2 leading-relaxed">
            {i + 1}. {t(`nav.${r.categoryId}`)} · {protocolLabel(r.protocol)} · {t("share.models", { n: r.models.length, m: r.mappings.length })}
            <div className="mt-1 break-all">{r.models.map(m => m.realName).join(", ") || "—"}</div>
          </li>)}
        </ul>
        <InlineAlert>{t(mode === "export" ? "share.review" : "share.effect")}</InlineAlert>
        <Button size="sm" disabled={busy} onClick={() => void run(async () => {
          const canonical = JSON.stringify(preview);
          if (mode === "export") {
            const path = await api.saveSharedTemplate(canonical);
            if (path) setMessage(t("share.saved", { path }));
          } else {
            const result = await api.applySharedTemplate(canonical);
            setReceipt(result); setPreview(null); setRaw("");
            setMessage(t("share.imported", { n: result.added })); onChanged();
          }
        })}>{busy ? t("share.busy") : t(mode === "export" ? "share.save" : "share.apply")}</Button>
      </div>}
      {message && <InlineAlert tone="success" role="status"><span className="break-all">{message}</span></InlineAlert>}
      {receipt && <div className="flex flex-wrap items-center gap-2">
        <Button size="sm" variant="secondary" disabled={busy} onClick={() => void run(async () => {
          await api.undoSharedTemplate(receipt.undoToken); setReceipt(null); setMessage(t("share.undone")); onChanged();
        })}>{t("share.undo")}</Button>
        <span className="text-xs text-text-secondary">{t("share.undoNote")}</span>
      </div>}
      {error && <InlineAlert tone="danger" role="alert">{error}</InlineAlert>}
    </CardContent>
  </Card>;
}
