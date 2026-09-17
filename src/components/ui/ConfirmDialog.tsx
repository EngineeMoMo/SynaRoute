import { useEffect } from "react";
import type { ReactNode } from "react";
import { AlertTriangle } from "lucide-react";
import { Button } from "@/components/ui/Button";
import { DialogBody, DialogFooter, DialogFrame, DialogHeader } from "@/components/ui/DialogFrame";

export function ConfirmDialog({
  title,
  description,
  confirmLabel,
  cancelLabel,
  onConfirm,
  onCancel,
  busy = false,
}: {
  title: string;
  description: ReactNode;
  confirmLabel: string;
  cancelLabel: string;
  onConfirm: () => void;
  onCancel: () => void;
  busy?: boolean;
}) {
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape" && !busy) onCancel();
    };
    window.addEventListener("keydown", onKeyDown, true);
    return () => window.removeEventListener("keydown", onKeyDown, true);
  }, [busy, onCancel]);

  return (
    <DialogFrame
      size="sm"
      role="alertdialog"
      onBackdropMouseDown={(event) => {
        if (event.target === event.currentTarget && !busy) onCancel();
      }}
    >
      <DialogHeader>
        <div className="flex items-center gap-2">
          <span className="flex h-8 w-8 items-center justify-center rounded-control bg-danger/12 text-danger">
            <AlertTriangle size={16} aria-hidden="true" />
          </span>
          <h2 className="text-sm font-semibold text-text-primary">{title}</h2>
        </div>
      </DialogHeader>
      <DialogBody>
        <p className="text-sm leading-relaxed text-text-secondary">{description}</p>
      </DialogBody>
      <DialogFooter>
        <Button variant="ghost" onClick={onCancel} disabled={busy} autoFocus>
          {cancelLabel}
        </Button>
        <Button variant="danger" onClick={onConfirm} disabled={busy}>
          {confirmLabel}
        </Button>
      </DialogFooter>
    </DialogFrame>
  );
}
