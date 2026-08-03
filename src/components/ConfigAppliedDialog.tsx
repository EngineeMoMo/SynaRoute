import { useStore } from "@/store";
import { useT } from "@/lib/useT";
import { Button } from "@/components/ui/Button";
import { CheckCircle } from "lucide-react";

export function ConfigAppliedDialog() {
  const configAppliedCategory = useStore((s) => s.configAppliedCategory);
  const clearConfigApplied = useStore((s) => s.clearConfigApplied);
  const t = useT();

  if (!configAppliedCategory) return null;

  const toolName = t(`nav.${configAppliedCategory}`);

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/40">
      <div className="mx-4 max-w-md rounded-card border border-border bg-surface p-6 shadow-xl">
        <div className="flex flex-col items-center gap-4 text-center">
          <CheckCircle size={40} className="text-success" />
          <h2 className="text-lg font-semibold text-text-primary">
            {t("proxy.configAppliedTitle")}
          </h2>
          <p className="text-sm leading-relaxed text-text-secondary">
            {t("proxy.configAppliedDesc", { tool: toolName })}
          </p>
          <Button onClick={clearConfigApplied}>{t("common.confirm")}</Button>
        </div>
      </div>
    </div>
  );
}
