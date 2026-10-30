import { useEffect, useRef, useState } from "react";
import { Archive, Copy, Download, Trash2, X } from "lucide-react";
import type { CodexSessionRow } from "@/types";
import { useT } from "@/lib/useT";
import { Button } from "@/components/ui/Button";
import { DialogBody, DialogFooter, DialogFrame, DialogHeader } from "@/components/ui/DialogFrame";

export function SessionDetailDialog({ row, disabled, onClose, onExport, onArchive, onDelete }: {
  row: CodexSessionRow;
  disabled: boolean;
  onClose: () => void;
  onExport: (row: CodexSessionRow) => void;
  onArchive: (row: CodexSessionRow) => void;
  onDelete: (row: CodexSessionRow) => void;
}) {
  const t = useT();
  const panelRef = useRef<HTMLDivElement>(null);
  const [notice, setNotice] = useState("");
  const validId = /^[\da-f]{8}(?:-[\da-f]{4}){3}-[\da-f]{12}$/i.test(row.threadId);
  const command = validId ? `codex resume ${row.threadId}` : "";
  useEffect(() => {
    const previous = document.activeElement as HTMLElement | null;
    panelRef.current?.querySelector<HTMLButtonElement>("button")?.focus();
    const keydown = (event: KeyboardEvent) => {
      if (event.key === "Escape") { event.preventDefault(); onClose(); }
      if (event.key !== "Tab") return;
      const buttons = [...(panelRef.current?.querySelectorAll<HTMLButtonElement>("button:not(:disabled)") ?? [])];
      const first = buttons[0];
      const last = buttons[buttons.length - 1];
      if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last?.focus(); }
      if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first?.focus(); }
    };
    window.addEventListener("keydown", keydown);
    return () => { window.removeEventListener("keydown", keydown); if (previous?.isConnected) previous.focus(); };
  }, [onClose]);
  const copy = async (text: string) => {
    try { await navigator.clipboard.writeText(text); setNotice(t("sessions.copied")); }
    catch { setNotice(t("sessions.copyFailed")); }
  };
  const formatTime = (value: string) => Number.isFinite(Date.parse(value)) ? new Date(value).toLocaleString() : "—";
  const entries = [
    [t("sessions.sessionId"), row.threadId],
    [t("sessions.colCwd"), row.cwd],
    [t("sessions.projectLabel"), row.project],
    [t("sessions.colProvider"), row.provider],
    [t("sessions.colModel"), [row.model, row.effort].filter(Boolean).join(" · ")],
    [t("sessions.createdAt"), formatTime(row.timestamp)],
    [t("sessions.updatedAt"), formatTime(row.updatedAt || row.timestamp)],
    [t("sessions.archiveFilter"), t(row.archived ? "sessions.archived" : "sessions.activeOnly")],
    [t("sessions.rolloutPath"), row.relPath],
  ];
  return <DialogFrame size="xl" panelRef={panelRef} ariaLabelledBy="session-detail-title"
    onBackdropMouseDown={(event) => { if (event.target === event.currentTarget) onClose(); }}>
    <DialogHeader>
      <h2 id="session-detail-title" className="min-w-0 break-words text-base font-semibold text-text-primary">{row.title || t("sessions.noTitle")}</h2>
      <Button variant="ghost" size="icon" aria-label={t("sessions.close")} onClick={onClose}><X size={18} /></Button>
    </DialogHeader>
    <DialogBody className="space-y-4">
      <dl className="grid grid-cols-1 gap-x-4 gap-y-2 text-sm sm:grid-cols-[8rem_1fr]">
        {entries.map(([label, value]) => <div key={label} className="contents"><dt className="text-text-muted">{label}</dt><dd className="min-w-0 break-words text-text-primary">{value || "—"}</dd></div>)}
      </dl>
      {command && <div className="space-y-2 rounded-control border border-border bg-surface-hover p-3">
        <p className="text-xs text-text-muted">{t("sessions.resumeHint")}</p>
        <code className="block break-all text-sm text-text-primary">{command}</code>
        <div className="flex flex-wrap gap-2">
          <Button variant="outline" size="sm" onClick={() => void copy(row.threadId)}><Copy size={14} />{t("sessions.copyId")}</Button>
          <Button variant="outline" size="sm" onClick={() => void copy(command)}><Copy size={14} />{t("sessions.copyResume")}</Button>
        </div>
      </div>}
      {notice && <p role="status" className="text-sm text-text-muted">{notice}</p>}
    </DialogBody>
    <DialogFooter className="flex-wrap">
      <Button variant="outline" size="sm" disabled={disabled} onClick={() => onExport(row)}><Download size={14} />{t("sessions.export")}</Button>
      <Button variant="outline" size="sm" disabled={disabled} onClick={() => onArchive(row)}><Archive size={14} />{t(row.archived ? "sessions.restore" : "sessions.archive")}</Button>
      <Button variant="danger-outline" size="sm" disabled={disabled} onClick={() => onDelete(row)}><Trash2 size={14} />{t("sessions.delete")}</Button>
    </DialogFooter>
  </DialogFrame>;
}
