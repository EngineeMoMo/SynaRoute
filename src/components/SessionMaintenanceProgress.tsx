import { useT } from "@/lib/useT";
import type { SessionMaintenanceProgress as Progress } from "@/lib/sessionMaintenance";

export function SessionMaintenanceProgress({ value }: { value: Progress | null }) {
  const t = useT();
  if (!value) return null;
  return (
    <div role="status" aria-live="polite" className="text-xs text-text-secondary">
      {t(`sessions.phase.${value.phase}`)}
      {value.total > 0 && <span className="ml-2 tabular-nums">{value.completed} / {value.total}</span>}
    </div>
  );
}
