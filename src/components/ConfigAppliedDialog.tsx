import { useStore } from "@/store";
import { useT } from "@/lib/useT";
import { Button } from "@/components/ui/Button";
import { CheckCircle } from "lucide-react";
import { DialogBody, DialogFooter, DialogFrame } from "@/components/ui/DialogFrame";

export function ConfigAppliedDialog() {
  const configAppliedCategory = useStore((s) => s.configAppliedCategory);
  const clearConfigApplied = useStore((s) => s.clearConfigApplied);
  const t = useT();

  if (!configAppliedCategory) return null;

  const toolName = t(`nav.${configAppliedCategory}`);

  return (
    <DialogFrame size="sm" onBackdropMouseDown={(e) => {
      if (e.target === e.currentTarget) clearConfigApplied();
    }}>
      <DialogBody className="flex flex-col items-center gap-4 text-center py-7">
        <span className="flex h-14 w-14 items-center justify-center rounded-full border border-success/25 bg-success/12 text-success">
          <CheckCircle size={28} />
        </span>
        <div>
          <h2 className="text-lg font-semibold text-text-primary">
            {t("proxy.configAppliedTitle")}
          </h2>
          <p className="mt-1.5 text-sm leading-relaxed text-text-secondary">
            {t("proxy.configAppliedDesc", { tool: toolName })}
          </p>
        </div>
      </DialogBody>
      <DialogFooter>
        <Button onClick={clearConfigApplied}>{t("common.confirm")}</Button>
      </DialogFooter>
    </DialogFrame>
  );
}
