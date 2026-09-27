import { ShieldCheck } from "lucide-react";
import { ToggleRow } from "@/components/ToggleRow";
import { useT } from "@/lib/useT";
import type { MasterPasswordState } from "@/types";

export function MasterPasswordToggle({ state, onChange }: {
  state: MasterPasswordState | null;
  onChange: (enabled: boolean) => void;
}) {
  const t = useT();
  return (
    <ToggleRow
      icon={ShieldCheck}
      title={t("settings.masterPwTitle")}
      desc={t(state?.required ? "settings.masterPwRequired" : "settings.masterPwDesc")}
      checked={state?.enabled ?? false}
      disabled={state === null || (state.required && state.enabled)}
      onChange={onChange}
    />
  );
}
