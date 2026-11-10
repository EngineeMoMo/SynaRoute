import { AlertTriangle } from "lucide-react";
import { Button } from "@/components/ui/Button";
import type { TFunc } from "@/lib/i18n";

export function LegacyBackupWarning({
  count,
  busy,
  onSanitize,
  t,
}: {
  count: number;
  busy: boolean;
  onSanitize: () => void;
  t: TFunc;
}) {
  if (!(count > 0)) return null;
  return (
    <div role="alert" className="space-y-2 rounded-control border border-warning/30 bg-warning/8 px-3 py-2">
      <div className="flex items-start gap-2 text-xs font-medium text-warning">
        <AlertTriangle size={13} className="mt-0.5 shrink-0" />
        <span>{t("backup.legacyTokensTitle", { n: count })}</span>
      </div>
      <p className="text-[11px] leading-relaxed text-text-secondary">
        {t("backup.legacyTokensWarning")}
      </p>
      <p className="text-[11px] leading-relaxed text-text-secondary">
        {t("backup.sanitizeHint")}
      </p>
      <Button type="button" variant="secondary" size="sm" disabled={busy} onClick={onSanitize}>
        {t("backup.sanitizeAction")}
      </Button>
    </div>
  );
}
