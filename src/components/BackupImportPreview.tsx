import { useState } from "react";
import { LegacyBackupWarning } from "@/components/LegacyBackupWarning";
import { api } from "@/lib/bridge";
import type { TFunc } from "@/lib/i18n";
import type { ImportPreview, SanitizedBackupOutcome } from "@/types";

export function BackupImportPreview({
  path,
  preview,
  busy,
  onBusyChange,
  legacyConsent = false,
  onLegacyConsent,
  t,
}: {
  path: string;
  preview: ImportPreview;
  busy: boolean;
  onBusyChange: (busy: boolean) => void;
  legacyConsent?: boolean;
  onLegacyConsent?: (allowed: boolean) => void;
  t: TFunc;
}) {
  const [sanitized, setSanitized] = useState<SanitizedBackupOutcome | null>(null);
  const [error, setError] = useState<string | null>(null);

  const sanitize = async () => {
    if (busy) return;
    onBusyChange(true);
    setError(null);
    try {
      const outcome = await api.sanitizeLegacyBackup(path);
      if (outcome) setSanitized(outcome);
    } catch (error) {
      setError(String((error as Error)?.message ?? error));
    } finally {
      onBusyChange(false);
    }
  };

  return (
    <>
      <div className="space-y-1 rounded-control bg-surface-elevated px-3 py-2">
        <div className="break-all font-mono text-[11px] text-text-secondary">{path}</div>
        <div className="text-[11px] text-text-secondary">
          {preview.localConfig ? t("backup.localConfigInfo") : t("backup.fileInfo", { version: preview.appVersion, time: preview.exportedAt })}
        </div>
        <div className="text-xs text-text-primary">
          {t("backup.summary", {
            keys: preview.keyCount,
            vendors: preview.vendorCount,
            brain: preview.brainCount,
          })}
        </div>
        <div className="text-[11px] text-text-secondary">
          {preview.hasSecrets ? t("backup.hasSecrets") : t("backup.noSecrets")}
        </div>
      </div>
      {preview.localConfig && (
        <div role="note" className="rounded-control bg-info/10 px-3 py-2 text-xs leading-relaxed text-text-secondary">
          {t("backup.localConfigHint")}
        </div>
      )}
      {preview.unauthenticatedSecrets && (
        <div role="alert" className="space-y-2 rounded-control border border-warning/30 bg-warning/10 px-3 py-2 text-xs text-warning">
          <p>{t("backup.unauthenticatedWarning")}</p>
          <label className="flex items-start gap-2">
            <input type="checkbox" checked={legacyConsent} disabled={busy || !onLegacyConsent} onChange={(event) => onLegacyConsent?.(event.target.checked)} />
            {t("backup.legacyConsent")}
          </label>
        </div>
      )}
      <LegacyBackupWarning
        count={preview.plaintextBalanceTokens}
        busy={busy}
        onSanitize={() => void sanitize()}
        t={t}
      />
      {sanitized && (
        <div role="status" className="break-all rounded-control bg-info/10 px-3 py-2 text-[11px] leading-relaxed text-text-secondary">
          {t("backup.sanitizedDone", { n: sanitized.removedTokens, path: sanitized.path })}
        </div>
      )}
      {error && (
        <div role="alert" className="whitespace-pre-line rounded-control bg-danger/10 px-3 py-2 text-xs leading-relaxed text-danger">
          {error}
        </div>
      )}
    </>
  );
}
