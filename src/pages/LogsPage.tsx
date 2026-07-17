import { useEffect, useState } from "react";
import { useStore } from "@/store";
import { Badge } from "@/components/ui/Badge";
import { useT } from "@/lib/useT";
import type { EventLogEntry } from "@/types";
import {
  ArrowLeftRight,
  Activity,
  Radio,
  Brain,
  AlertCircle,
  ScrollText,
  ArrowUpRight,
  ChevronRight,
  Copy,
  Check,
  type LucideIcon,
} from "lucide-react";

const TYPE_META: Record<
  EventLogEntry["type"],
  { tKey: string; variant: "neutral" | "success" | "warning" | "danger" | "info" | "primary"; icon: LucideIcon }
> = {
  route: { tKey: "logs.type.route", variant: "success", icon: Radio },
  failover: { tKey: "logs.type.failover", variant: "warning", icon: ArrowLeftRight },
  health: { tKey: "logs.type.health", variant: "info", icon: Activity },
  aggregate: { tKey: "logs.type.aggregate", variant: "primary", icon: Brain },
  error: { tKey: "logs.type.error", variant: "danger", icon: AlertCircle },
  request: { tKey: "logs.type.request", variant: "neutral", icon: ArrowUpRight },
};

/** 运行日志 / 可观测性视图（FR-020）。request 类型可展开查看完整调用链路。 */
export function LogsPage() {
  const { events, lang, refreshEvents } = useStore();
  const t = useT();

  // 实时刷新：每 2s 拉一次事件（仅事件，不重载 keys/proxy，开销小）。
  useEffect(() => {
    void refreshEvents();
    const id = setInterval(() => void refreshEvents(), 2000);
    return () => clearInterval(id);
  }, [refreshEvents]);

  return (
    <div className="flex h-full flex-col">
      <div className="border-b border-border px-6 py-4">
        <h1 className="text-lg font-semibold text-text-primary">{t("logs.title")}</h1>
        <p className="mt-1 text-xs text-text-muted">{t("logs.subtitle")}</p>
      </div>

      <div className="flex-1 overflow-y-auto p-6">
        {events.length === 0 ? (
          <div className="flex flex-col items-center gap-3 py-16 text-text-muted">
            <ScrollText size={40} />
            <p className="text-sm">{t("logs.empty")}</p>
          </div>
        ) : (
          <div className="space-y-1.5">
            {[...events].reverse().map((e) => (
              <LogRow key={e.id} entry={e} lang={lang} />
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

function LogRow({ entry, lang }: { entry: EventLogEntry; lang: string }) {
  const t = useT();
  const [open, setOpen] = useState(false);
  const meta = TYPE_META[entry.type];
  const Icon = meta.icon;
  const expandable = entry.type === "request" && !!entry.trace;
  const time = new Date(entry.ts).toLocaleTimeString(lang === "en" ? "en-US" : "zh-CN");

  return (
    <div className="rounded-control border border-border bg-surface">
      <div
        className={`flex items-center gap-3 px-3 py-2 ${expandable ? "cursor-pointer hover:bg-surface-hover" : ""}`}
        onClick={expandable ? () => setOpen((v) => !v) : undefined}
      >
        {expandable ? (
          <ChevronRight
            size={14}
            className={`shrink-0 text-text-muted transition-transform ${open ? "rotate-90" : ""}`}
          />
        ) : (
          <span className="w-[14px] shrink-0" />
        )}
        <span className="font-mono text-[11px] text-text-muted">{time}</span>
        <Badge variant={meta.variant}>
          <Icon size={10} />
          {t(meta.tKey)}
        </Badge>
        <span className="flex-1 truncate text-sm text-text-primary">{entry.detail}</span>
      </div>

      {open && entry.trace && <TraceDetail trace={entry.trace} />}
    </div>
  );
}

function TraceDetail({ trace }: { trace: NonNullable<EventLogEntry["trace"]> }) {
  const t = useT();
  return (
    <div className="border-t border-border px-3 py-3 text-xs">
      {/* 概要信息栏 */}
      <div className="mb-3 grid grid-cols-[auto_1fr] gap-x-4 gap-y-1.5">
        <Field label={t("logs.trace.vendor")} value={`${trace.keyName} · ${trace.vendor}`} />
        <Field label={t("logs.trace.protocol")} value={trace.protocol} />
        <Field
          label={t("logs.trace.model")}
          value={
            trace.requestedModel === trace.realModel
              ? trace.realModel || "?"
              : `${trace.requestedModel || "?"} → ${trace.realModel}`
          }
        />
        <Field label={t("logs.trace.url")} value={trace.url || "—"} mono />
        <Field
          label={t("logs.trace.status")}
          value={
            <span className={trace.ok ? "text-success" : "text-danger"}>
              {trace.status != null ? `HTTP ${trace.status}` : t("logs.trace.noResponse")}
              {` · ${trace.latencyMs}ms`}
            </span>
          }
        />
      </div>

      {/* 请求 / 响应体 */}
      <CodeBlock title={t("logs.trace.requestBody")} body={trace.requestBody} />
      <div className="h-2" />
      <CodeBlock title={t("logs.trace.responseBody")} body={trace.responseBody} danger={!trace.ok} />
    </div>
  );
}

function Field({ label, value, mono }: { label: string; value: React.ReactNode; mono?: boolean }) {
  return (
    <>
      <span className="text-text-muted">{label}</span>
      <span className={`break-all text-text-primary ${mono ? "font-mono" : ""}`}>{value}</span>
    </>
  );
}

function CodeBlock({ title, body, danger }: { title: string; body: string; danger?: boolean }) {
  const t = useT();
  const [copied, setCopied] = useState(false);
  const copy = () => {
    void navigator.clipboard?.writeText(body).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 1200);
    });
  };
  return (
    <div>
      <div className="mb-1 flex items-center justify-between">
        <span className="text-text-muted">{title}</span>
        <button
          onClick={copy}
          className="flex items-center gap-1 rounded px-1.5 py-0.5 text-text-muted hover:bg-surface-hover hover:text-text-primary"
          title={t("logs.trace.copy")}
        >
          {copied ? <Check size={12} /> : <Copy size={12} />}
        </button>
      </div>
      <pre
        className={`max-h-72 overflow-auto rounded-control border border-border bg-bg p-2.5 font-mono text-[11px] leading-relaxed ${
          danger ? "text-danger" : "text-text-primary"
        }`}
      >
        {body || "—"}
      </pre>
    </div>
  );
}
