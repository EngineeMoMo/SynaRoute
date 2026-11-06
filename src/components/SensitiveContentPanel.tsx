import { useRef, useState, useEffect } from "react";
import { Button } from "@/components/ui/Button";
import { api } from "@/lib/bridge";
import { useT } from "@/lib/useT";
import type { CategoryType } from "@/types";
import type { PrivacyConfig, PrivacyMode, SensitiveScan } from "@/lib/routingPolicies";
import { RoutingField, RoutingSelect, routingInput } from "./routing/RoutingFields";

export function SensitiveContentPanel({ category, value, onChange, terms, onTermsChange, locked, busy, onError }: {
  category: CategoryType; value: PrivacyConfig; onChange: (value: PrivacyConfig) => void;
  terms: string | null; onTermsChange: (terms: string | null) => void; locked: boolean; busy: boolean; onError: (error: unknown) => void;
}) {
  const t = useT();
  const [text, setText] = useState("");
  const [result, setResult] = useState<SensitiveScan | null>(null);
  const [scanning, setScanning] = useState(false);
  const generation = useRef(0);
  useEffect(() => () => { generation.current++; }, []);
  const scan = async () => {
    const token = ++generation.current; setScanning(true); setResult(null);
    try { const next = await api.previewSensitiveContent(category, text, value.credentials, value.emails, value.phones); if (generation.current === token) setResult(next); }
    catch (error) { if (generation.current === token) onError(error); }
    finally { if (generation.current === token) setScanning(false); }
  };
  return <section className="space-y-4">
    <p className="max-w-3xl text-sm leading-6 text-text-secondary">{t("routing.privacy.hint")}</p>
    <div className="max-w-lg"><RoutingField label={t("routing.privacy")}><RoutingSelect value={value.mode} disabled={busy} options={["off", "warn", "block"].map((value) => ({ value, label: t(`routing.privacy.${value}`) }))} onChange={(e) => onChange({ ...value, mode: e.target.value as PrivacyMode })} /></RoutingField></div>
    <div className="space-y-2">{(["credentials", "emails", "phones"] as const).map((field) => <label key={field} className="flex items-center gap-2 text-sm"><input type="checkbox" checked={value[field]} disabled={busy} onChange={(e) => { setResult(null); onChange({ ...value, [field]: e.target.checked }); }} />{t(`routing.privacy.${field}`)}</label>)}</div>
    <div className="space-y-2 border-t border-border pt-4">
      <p className="text-xs text-text-secondary">{t("routing.privacy.termCount", { n: value.termCount })}</p>
      <label className="flex items-center gap-2 text-sm"><input type="checkbox" checked={terms !== null} disabled={locked || busy} onChange={(e) => onTermsChange(e.target.checked ? "" : null)} />{t("routing.privacy.replaceTerms")}</label>
      {terms !== null && <RoutingField label={t("routing.privacy.terms")}><textarea className={routingInput} rows={4} autoComplete="off" spellCheck={false} value={terms} disabled={locked || busy} onChange={(e) => onTermsChange(e.target.value)} /></RoutingField>}
      <p className="text-xs leading-5 text-text-secondary">{t("routing.privacy.termsHint")}</p>
      <p className="text-xs leading-5 text-text-secondary">{t("routing.privacy.replaceHint")}</p>
    </div>
    <div className="space-y-3 border-t border-border pt-4">
      <h3 className="text-sm font-semibold">{t("routing.privacy.test")}</h3>
      <p className="max-w-3xl text-xs leading-5 text-text-secondary">{t("routing.privacy.testHint")}</p>
      <p className="text-xs leading-5 text-text-secondary">{t("routing.privacy.savedTermsTest")}</p>
      <RoutingField label={t("routing.privacy.testText")}><textarea className={routingInput} rows={4} value={text} autoComplete="off" spellCheck={false} maxLength={65536} disabled={scanning} onChange={(e) => { setText(e.target.value); setResult(null); }} /></RoutingField>
      <Button variant="outline" disabled={busy || scanning || !text || locked} onClick={() => void scan()}>{t(scanning ? "common.loading" : "routing.privacy.testRun")}</Button>
      {result && <div className="space-y-2 rounded-control border border-border p-3 text-xs leading-5" role="status">
        {Object.entries(result.counts).map(([kind, count]) => <p key={kind}>{t(`routing.finding.${["api_key", "private_key", "token"].includes(kind) ? "credential" : kind}`)}: {count}</p>)}
        {!Object.keys(result.counts).length && <p>{t("routing.privacy.testEmpty")}</p>}
        {result.limited ? <p>{t("routing.privacy.testLimited")}</p> : <pre className="whitespace-pre-wrap break-all">{result.redacted}</pre>}
      </div>}
    </div>
    <p className="max-w-3xl text-xs leading-5 text-text-secondary">{t("routing.privacy.limits")}</p>
    <p className="max-w-3xl text-xs leading-5 text-text-secondary">{t("routing.privacy.logNote")}</p>
  </section>;
}
