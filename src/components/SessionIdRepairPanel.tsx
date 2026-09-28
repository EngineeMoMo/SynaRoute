import { useEffect, useRef, useState } from "react";
import { api } from "@/lib/bridge";
import { useT } from "@/lib/useT";
import type { SessionIdRepairPreview, SessionIdRepairResult, SessionMaintenanceProgress as Progress } from "@/lib/sessionMaintenance";
import { Button } from "@/components/ui/Button";
import { InlineAlert } from "@/components/ui/InlineAlert";
import { SessionMaintenanceProgress } from "./SessionMaintenanceProgress";

export function SessionIdRepairPanel({ disabled, onBusyChange, onDone }: {
  disabled: boolean;
  onBusyChange: (busy: boolean) => void;
  onDone: () => Promise<void>;
}) {
  const t = useT();
  const [preview, setPreview] = useState<SessionIdRepairPreview | null>(null);
  const [result, setResult] = useState<SessionIdRepairResult | null>(null);
  const [progress, setProgress] = useState<Progress | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const inFlight = useRef(false);
  const mounted = useRef(true);
  useEffect(() => { mounted.current = true; return () => { mounted.current = false; }; }, []);
  useEffect(() => { if (disabled) setPreview(null); }, [disabled]);

  const run = async (apply: boolean) => {
    if (disabled || inFlight.current || (apply && !preview?.files.length)) return;
    const token = preview?.token;
    inFlight.current = true;
    setBusy(true);
    onBusyChange(true);
    setError(null);
    setResult(null);
    setPreview(null);
    setProgress({ phase: apply ? "validating" : "scanning", completed: 0, total: 0 });
    const update = (value: Progress) => { if (mounted.current) setProgress(value); };
    try {
      if (apply && token) {
        const next = await api.applyCodexSessionIdRepair(token, update);
        if (mounted.current) {
          setResult(next);
          setProgress({ phase: next.issues.length ? "partial" : "complete", completed: next.changedFiles, total: next.changedFiles + next.remainingFiles });
        }
        await onDone();
      } else {
        const next = await api.previewCodexSessionIdRepair(update);
        if (mounted.current) {
          setPreview(next);
          setProgress({ phase: "complete", completed: next.scanned, total: next.scanned });
        }
      }
    } catch (failure) {
      if (mounted.current) { setError(String(failure)); setProgress(null); }
    } finally {
      inFlight.current = false;
      if (mounted.current) { setBusy(false); onBusyChange(false); }
    }
  };

  return (
    <section aria-labelledby="session-id-repair-title" aria-busy={busy} className="space-y-3 rounded-control border border-border p-4">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div className="min-w-0 flex-1">
          <h2 id="session-id-repair-title" className="text-sm font-medium text-text-primary">{t("sessions.repair.title")}</h2>
          <p className="mt-1 max-w-prose text-xs leading-relaxed text-text-secondary">{t("sessions.repair.hint")}</p>
        </div>
        <Button size="sm" variant="outline" disabled={disabled || busy} onClick={() => void run(false)}>
          {busy ? t("sessions.repair.busy") : t("sessions.repair.scan")}
        </Button>
      </div>
      <SessionMaintenanceProgress value={progress} />
      {error && <InlineAlert tone="danger" role="alert">{error}</InlineAlert>}
      {preview && <div className="space-y-3">
        <p className="text-sm text-text-primary">{t("sessions.repair.summary", {
          scanned: preview.scanned, files: preview.files.length, ids: preview.files.reduce((sum, file) => sum + file.ids, 0),
        })}</p>
        {preview.files.length > 0 ? <>
          <ul aria-label={t("sessions.repair.files")} className="max-h-48 overflow-y-auto divide-y divide-border text-xs">
            {preview.files.map((file) => <li key={file.relPath} className="flex items-start justify-between gap-4 py-2">
              <code className="min-w-0 break-all text-text-secondary">{file.relPath}</code>
              <span className="shrink-0 tabular-nums text-text-primary">{t("sessions.repair.ids", { n: file.ids })}</span>
            </li>)}
          </ul>
          <InlineAlert tone="warning" className="text-text-primary">{t("sessions.repair.confirm")}</InlineAlert>
          <Button size="sm" disabled={disabled || busy} onClick={() => void run(true)}>{t("sessions.repair.apply", { n: preview.files.length })}</Button>
        </> : <p className="text-xs text-text-secondary">{t("sessions.repair.none")}</p>}
        {preview.issues.length > 0 && <details open className="text-xs text-text-secondary">
          <summary className="cursor-pointer text-text-primary">{t("sessions.repair.issues", { n: preview.issues.length })}</summary>
          <ul className="mt-2 max-h-40 space-y-2 overflow-y-auto">{preview.issues.map((issue, index) => <li key={`${issue.relPath}-${index}`} className="break-all">
            {issue.relPath && <code>{issue.relPath}: </code>}{issue.message}
          </li>)}</ul>
        </details>}
      </div>}
      {result && <InlineAlert tone={result.issues.length ? "warning" : "success"} className="text-text-primary" role="status">
        <p>{t("sessions.repair.result", { files: result.changedFiles, ids: result.changedIds, remaining: result.remainingFiles })}</p>
        {result.backupDir && <p className="mt-1 break-all">{t("sessions.repair.backup")} <code>{result.backupDir}</code></p>}
        {result.issues.map((issue, index) => <p key={index} className="mt-1 break-all">{issue.relPath}: {issue.message}</p>)}
        <p className="mt-1">{t("sessions.repair.after")}</p>
      </InlineAlert>}
    </section>
  );
}
