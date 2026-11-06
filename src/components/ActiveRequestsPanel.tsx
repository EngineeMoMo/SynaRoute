import { useEffect, useRef, useState } from "react";
import { ChevronDown, ChevronRight, Square } from "lucide-react";
import type { CategoryType } from "@/types";
import { api } from "@/lib/bridge";
import { ActiveRequestFeed, requestBytes, requestDuration, type ActiveRequest } from "@/lib/activeRequests";
import { usePolling } from "@/lib/usePolling";
import { useT } from "@/lib/useT";
import { Button } from "./ui/Button";
import { ConfirmDialog } from "./ui/ConfirmDialog";

/** Keyed by category at the call site: rows, confirmations and pending IPC never cross categories. */
export function ActiveRequestsPanel({ category }: { category: CategoryType }) {
  const t = useT();
  const [rows, setRows] = useState<ActiveRequest[]>([]);
  const [loaded, setLoaded] = useState(false);
  const [error, setError] = useState("");
  const [note, setNote] = useState("");
  const [expanded, setExpanded] = useState(true);
  const [confirm, setConfirm] = useState<ActiveRequest | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [requested, setRequested] = useState<Set<string>>(new Set());
  const feed = useRef(new ActiveRequestFeed());
  const alive = useRef(true);
  const actionPending = useRef(false);
  useEffect(() => {
    alive.current = true;
    return () => { alive.current = false; feed.current.invalidate(); };
  }, []);
  const refresh = () => feed.current.refresh(
    () => api.listActiveRequests(category),
    (next) => {
      setRows(next); setLoaded(true); setError("");
      setRequested((previous) => new Set([...previous].filter((id) => next.some((row) => row.requestId === id))));
    },
    (reason) => { setLoaded(true); setError(String(reason)); },
  );
  usePolling(() => void refresh(), 1000);

  const stop = async () => {
    if (!confirm || actionPending.current) return;
    actionPending.current = true;
    const id = confirm.requestId;
    setBusy(id); setNote("");
    try {
      const result = await api.stopActiveRequest(category, id);
      if (!alive.current) return;
      setNote(t(result === "finished" ? "active.finished" : "active.requested"));
      if (result !== "finished") setRequested((previous) => new Set([...previous, id]));
      setConfirm(null);
      void refresh();
    } catch (reason) {
      if (alive.current) { setConfirm(null); setNote(String(reason)); }
    } finally {
      actionPending.current = false;
      if (alive.current) setBusy(null);
    }
  };

  return (
    <section className="rounded-card border border-border bg-surface" aria-label={t("active.title")} data-active-requests>
      <button type="button" onClick={() => setExpanded((value) => !value)} aria-expanded={expanded}
        className="flex w-full items-center gap-2 px-4 py-3 text-left text-sm font-medium text-text-primary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring">
        {expanded ? <ChevronDown size={15} /> : <ChevronRight size={15} />}
        {t("active.title")} <span className="text-xs tabular-nums text-text-muted">{loaded && !error ? rows.length : "—"}</span>
      </button>
      {expanded && <div className="border-t border-border px-4 pb-3">
        <p className="py-2 text-xs leading-relaxed text-text-muted">{t("active.scope")}</p>
        {error && <p role="alert" className="mb-2 break-words text-xs text-danger">{t("active.error", { error })}</p>}
        {note && <p role="status" className="mb-2 text-xs text-text-secondary">{note}</p>}
        {!loaded && !error && <p className="py-2 text-xs text-text-muted">{t("active.loading")}</p>}
        {loaded && !error && rows.length === 0 && <p className="py-2 text-xs text-text-muted">{t("active.empty")}</p>}
        {rows.length > 0 && <div className="max-h-64 overflow-auto">
          <table className="w-full min-w-[700px] text-left text-xs">
            <thead className="sticky top-0 bg-surface text-text-muted">
              <tr>{["active.model", "active.key", "active.phase", "active.time", "active.bytes"].map((key) => <th key={key} className="px-2 py-2 font-normal">{t(key)}</th>)}<th className="px-2 py-2"><span className="sr-only">{t("active.stop")}</span></th></tr>
            </thead>
            <tbody>{rows.map((row) => {
              const stopping = row.phase === "stopping" || requested.has(row.requestId) || busy === row.requestId;
              return <tr key={row.requestId} className="border-t border-border/60 align-top">
                <td className="max-w-64 px-2 py-3">
                  <div className="break-all font-medium text-text-primary">{row.requestedModel || t("active.pending")}</div>
                  {row.realModel && row.realModel !== row.requestedModel && <div className="mt-1 break-all text-text-secondary">→ {row.realModel}</div>}
                  <div className="mt-1 break-all font-mono text-[10px] text-text-muted" title={row.requestId}>{row.requestId}</div>
                </td>
                <td className="max-w-36 break-words px-2 py-3 text-text-secondary">{row.keyName ?? t("active.pending")}</td>
                <td className="px-2 py-3">
                  <div className={stopping ? "text-warning" : "text-text-primary"}>{t(`active.${stopping ? "stopping" : row.phase}`)}</div>
                  <div className="mt-1 text-text-muted">{t(row.streaming ? "active.stream" : "active.buffered")}</div>
                  {row.attempts > 0 && <div className="mt-1 text-text-muted">{t("active.attempts", { n: row.attempts })}</div>}
                </td>
                <td className="whitespace-nowrap px-2 py-3 font-mono tabular-nums text-text-secondary">{requestDuration(row.elapsedMs)} / {row.attemptMs === null ? "—" : requestDuration(row.attemptMs)}</td>
                <td className="whitespace-nowrap px-2 py-3 text-text-secondary">
                  <div>{requestBytes(row.bytesReceived)}</div>
                  <div className="mt-1 text-text-muted" title={t("active.lastRead")}>{row.lastReadMs === null ? t("active.noData") : `${t("active.lastRead")} ${requestDuration(row.lastReadMs)}`}</div>
                </td>
                <td className="px-2 py-3"><Button variant="danger-outline" size="sm" disabled={stopping || !!busy} onClick={() => setConfirm(row)}><Square size={12} />{t(stopping ? "active.stopping" : "active.stop")}</Button></td>
              </tr>;
            })}</tbody>
          </table>
        </div>}
      </div>}
      {confirm && <ConfirmDialog title={t("active.stopTitle")}
        description={<><span className="block">{t("active.stopBody")}</span><span className="mt-3 block break-all font-mono text-xs">{t("active.request", { id: confirm.requestId })}</span></>}
        confirmLabel={t("active.stop")} cancelLabel={t("common.cancel")}
        onConfirm={() => void stop()} onCancel={() => setConfirm(null)} busy={busy !== null} />}
    </section>
  );
}
