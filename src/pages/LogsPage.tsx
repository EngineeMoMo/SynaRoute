import { useEffect, useMemo, useState } from "react";
import { useStore } from "@/store";
import { Badge } from "@/components/ui/Badge";
import { useT } from "@/lib/useT";
import type { CategoryType, EventLogEntry } from "@/types";
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
  Settings2,
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
  mcp: { tKey: "logs.type.mcp", variant: "primary", icon: Brain },
  config: { tKey: "logs.type.config", variant: "info", icon: Settings2 },
  error: { tKey: "logs.type.error", variant: "danger", icon: AlertCircle },
  request: { tKey: "logs.type.request", variant: "neutral", icon: ArrowUpRight },
};

/** 日志分组：把事件类型归到用户可辨识的大类。
 * 「路由成功」与「故障转移」分开成两组：正常成功归 route（绿）、真正的换 Key 归 failover（橙），
 * 这样想只看故障时点 failover 组即可，不再被大量正常成功刷屏。
 * request（调用快照）归 route 组，作为路由链路的明细。 */
type LogGroup = "system" | "brain" | "route" | "failover" | "error";

const GROUP_OF: Record<EventLogEntry["type"], LogGroup> = {
  config: "system",
  health: "system",
  aggregate: "brain",
  mcp: "brain",
  route: "route",
  request: "route",
  failover: "failover",
  error: "error",
};

const GROUP_ORDER: LogGroup[] = ["system", "brain", "route", "failover", "error"];

const GROUP_META: Record<LogGroup, { tKey: string; icon: LucideIcon; dot: string }> = {
  system: { tKey: "logs.group.system", icon: Settings2, dot: "bg-info" },
  brain: { tKey: "logs.group.brain", icon: Brain, dot: "bg-primary" },
  route: { tKey: "logs.group.route", icon: Radio, dot: "bg-success" },
  failover: { tKey: "logs.group.failover", icon: ArrowLeftRight, dot: "bg-warning" },
  error: { tKey: "logs.group.error", icon: AlertCircle, dot: "bg-danger" },
};

/** 分类维度：日志现已合并全部分类连续展示，每条自带来源分类标签，顶部可按分类再筛选。
 * 复用左侧导航的分类名（nav.*），Badge 用中性色以免与类型徽标抢视觉。 */
const CATEGORY_ORDER: CategoryType[] = ["claude-cli", "claude-desktop", "codex"];

/** 运行日志 / 可观测性视图（FR-020）。合并全部分类连续展示，按「类型分组」与「来源分类」两维筛选。 */
export function LogsPage() {
  const { events, lang, refreshEvents } = useStore();
  const t = useT();
  // 类型分组筛选：null = 全部；否则只看某一组。
  const [filter, setFilter] = useState<LogGroup | null>(null);
  // 分类筛选：null = 全部分类；否则只看某一来源分类。与分组筛选正交。
  const [catFilter, setCatFilter] = useState<CategoryType | null>(null);

  // 实时刷新：每 2s 拉一次事件（仅事件，不重载 keys/proxy，开销小）。
  useEffect(() => {
    void refreshEvents();
    const id = setInterval(() => void refreshEvents(), 2000);
    return () => clearInterval(id);
  }, [refreshEvents]);

  // 每组事件计数（在「当前分类筛选」范围内统计，交叉联动）。
  const counts = useMemo(() => {
    const c: Record<LogGroup, number> = { system: 0, brain: 0, route: 0, failover: 0, error: 0 };
    for (const e of events) {
      if (catFilter && e.categoryId !== catFilter) continue;
      c[GROUP_OF[e.type] ?? "system"] += 1;
    }
    return c;
  }, [events, catFilter]);

  // 每分类事件计数（在「当前分组筛选」范围内统计，交叉联动）。
  const catCounts = useMemo(() => {
    const c: Record<CategoryType, number> = { "claude-cli": 0, "claude-desktop": 0, codex: 0 };
    for (const e of events) {
      if (filter && (GROUP_OF[e.type] ?? "system") !== filter) continue;
      c[e.categoryId] += 1;
    }
    return c;
  }, [events, filter]);

  // 「全部」标签计数：分别落在各自维度的另一维筛选范围内。
  const allByGroup = useMemo(
    () => events.filter((e) => !catFilter || e.categoryId === catFilter).length,
    [events, catFilter],
  );
  const allByCat = useMemo(
    () => events.filter((e) => !filter || (GROUP_OF[e.type] ?? "system") === filter).length,
    [events, filter],
  );

  // 最新在前；同时按两个维度裁剪。
  const visible = useMemo(() => {
    const ordered = [...events].reverse();
    return ordered.filter(
      (e) =>
        (!filter || (GROUP_OF[e.type] ?? "system") === filter) &&
        (!catFilter || e.categoryId === catFilter),
    );
  }, [events, filter, catFilter]);

  return (
    <div className="flex h-full flex-col">
      <div className="border-b border-border px-6 py-4">
        <h1 className="text-lg font-semibold text-text-primary">{t("logs.title")}</h1>
        <p className="mt-1 text-xs text-text-muted">{t("logs.subtitle")}</p>

        {/* 分类筛选：全部 + 三个来源分类（带计数）。日志已合并连续展示，切换活动分类不再裁剪。 */}
        <div className="mt-3 flex flex-wrap gap-1.5">
          <FilterTab
            label={t("logs.filter.allCategories")}
            count={allByCat}
            active={catFilter === null}
            onClick={() => setCatFilter(null)}
          />
          {CATEGORY_ORDER.map((cat) => (
            <FilterTab
              key={cat}
              label={t(`nav.${cat}`)}
              count={catCounts[cat]}
              active={catFilter === cat}
              onClick={() => setCatFilter(cat)}
            />
          ))}
        </div>

        {/* 类型分组筛选：全部 + 4 个分组（带计数） */}
        <div className="mt-2 flex flex-wrap gap-1.5">
          <FilterTab
            label={t("logs.filter.all")}
            count={allByGroup}
            active={filter === null}
            onClick={() => setFilter(null)}
          />
          {GROUP_ORDER.map((g) => {
            const GIcon = GROUP_META[g].icon;
            return (
              <FilterTab
                key={g}
                label={t(GROUP_META[g].tKey)}
                icon={GIcon}
                count={counts[g]}
                active={filter === g}
                onClick={() => setFilter(g)}
              />
            );
          })}
        </div>
      </div>

      <div className="flex-1 overflow-y-auto p-6">
        {visible.length === 0 ? (
          <div className="flex flex-col items-center gap-3 py-16 text-text-muted">
            <ScrollText size={40} />
            <p className="text-sm">{t("logs.empty")}</p>
          </div>
        ) : (
          <div className="space-y-1.5">
            {visible.map((e) => (
              <LogRow key={e.id} entry={e} lang={lang} />
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

/** 顶部筛选标签（带左侧分组色点 + 计数）。 */
function FilterTab({
  label,
  icon: Icon,
  count,
  active,
  onClick,
}: {
  label: string;
  icon?: LucideIcon;
  count: number;
  active: boolean;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={`flex items-center gap-1.5 rounded-control border px-2.5 py-1 text-xs transition-colors ${
        active
          ? "border-primary bg-primary/10 text-primary"
          : "border-border text-text-secondary hover:bg-surface-hover"
      }`}
    >
      {Icon && <Icon size={12} />}
      <span>{label}</span>
      <span className={`rounded-full px-1.5 text-[10px] ${active ? "bg-primary/20" : "bg-surface-hover text-text-muted"}`}>
        {count}
      </span>
    </button>
  );
}

function LogRow({ entry, lang }: { entry: EventLogEntry; lang: string }) {
  const t = useT();
  const [open, setOpen] = useState(false);
  const meta = TYPE_META[entry.type] ?? TYPE_META.route;
  const Icon = meta.icon;
  // 所有类型都可展开：有 trace 的展开完整链路快照；无 trace 的展开 detail 全文
  //（长 detail 在收起态被 truncate 截断，展开后不截断，解决「日志太长看不了」）。
  const expandable = !!entry.trace || entry.detail.length > 0;
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
        <Badge variant="neutral">{t(`nav.${entry.categoryId}`)}</Badge>
        <Badge variant={meta.variant}>
          <Icon size={10} />
          {t(meta.tKey)}
        </Badge>
        <span className={`flex-1 text-sm text-text-primary ${open ? "whitespace-pre-wrap break-words" : "truncate"}`}>
          {entry.detail}
        </span>
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
