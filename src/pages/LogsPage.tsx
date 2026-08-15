import * as React from "react";
import { useMemo, useState } from "react";
import { useStore } from "@/store";
import { Badge } from "@/components/ui/Badge";
import { api } from "@/lib/bridge";
import { useT } from "@/lib/useT";
import { usePolling } from "@/lib/usePolling";
import { useBackendEvent, FALLBACK_POLL_MS } from "@/lib/useBackendEvents";
import type { CategoryType, EventLogEntry, RequestTrace } from "@/types";
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
  Search,
  KeyRound,
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
  // 细粒度订阅（勿改回 `useStore()` 整店解构）：本页每 2s 刷新 events，
  // 而整店解构会让**所有**这样写的组件跟着每 2s 全量重渲染。
  const events = useStore((s) => s.events);
  const lang = useStore((s) => s.lang);
  const refreshEvents = useStore((s) => s.refreshEvents);
  const t = useT();
  // 类型分组筛选：null = 全部；否则只看某一组。
  const [filter, setFilter] = useState<LogGroup | null>(null);
  // 分类筛选：null = 全部分类；否则只看某一来源分类。与分组筛选正交。
  const [catFilter, setCatFilter] = useState<CategoryType | null>(null);
  // 搜索词（UX#10）：此前 500 条日志只能靠「类型分组 + 来源分类」两维筛选肉眼扫，
  // 排障时找一条具体记录（某个 Key、某个模型、某条错误）非常费劲。
  const [query, setQuery] = useState("");

  // 实时刷新（UX#5）：改由后端在**有新事件写入时**推送，日志页收到即拉。
  // 这是全应用最密的轮询（原 2s），也是最值得改成推送的一处：空闲时降到 30s 兜底，
  // 而有流量时反而比 2s 更及时。
  //
  // 后端对 `logs` 主题限速 1500ms（见 events.rs 的 min_interval_ms）——
  // 转发热路径每次要写好几条事件，不限速的话这里会被打成比原来还高的频率，
  // 而本页对每次推送的反应是**重拉 500 条事件**。
  //
  // 走 usePolling 兜底：窗口不可见时自动停表，切回来立即补一次（见该 hook 的文档）。
  useBackendEvent(["logs"], () => void refreshEvents());
  usePolling(() => void refreshEvents(), FALLBACK_POLL_MS);

  // 计数与可见列表**一趟算完**（原先是 5 个各自遍历 events 的 useMemo：两个 Record 累加 +
  // 两个 filter().length + 一次 reverse+filter，合计 5 次全量遍历 + 2 次数组拷贝。日志每 2s
  // 刷一次、最多 500 条，没必要为几个计数把数组走 5 遍）。
  const { counts, catCounts, allByGroup, allByCat, visible } = useMemo(() => {
    const counts: Record<LogGroup, number> = { system: 0, brain: 0, route: 0, failover: 0, error: 0 };
    const catCounts: Record<CategoryType, number> = { "claude-cli": 0, "claude-desktop": 0, codex: 0 };
    let allByGroup = 0;
    let allByCat = 0;
    const visible: EventLogEntry[] = [];
    // 搜索词（UX#10）：与两个筛选维度**正交叠加**——先按搜索缩小全集，再在其中做分组/分类计数，
    // 这样徽标数字始终与用户眼前看到的列表一致（否则会出现「徽标说有 20 条、列表只显示 3 条」）。
    // 500 条上限下纯前端过滤足够，不必回后端。
    const needle = query.trim().toLowerCase();
    for (const e of events) {
      // 匹配面覆盖用户实际会搜的东西：详情正文（含 Key 名、模型名、错误原因）、事件类型、分类。
      // detail 里已经拼了「Key 名 · 模型段 · 动词」，故搜 Key 名或模型名都能命中。
      const hit =
        !needle ||
        e.detail.toLowerCase().includes(needle) ||
        e.type.toLowerCase().includes(needle) ||
        e.categoryId.toLowerCase().includes(needle);
      if (!hit) continue;
      const g = GROUP_OF[e.type] ?? "system";
      const catOk = !catFilter || e.categoryId === catFilter;
      const groupOk = !filter || g === filter;
      // 交叉联动：每个维度的计数落在**另一个**维度的筛选范围内
      if (catOk) counts[g] += 1;
      if (groupOk) catCounts[e.categoryId] += 1;
      if (catOk) allByGroup += 1;
      if (groupOk) allByCat += 1;
      if (catOk && groupOk) visible.push(e);
    }
    visible.reverse(); // 最新在前（就地反转，省一次数组拷贝）
    return { counts, catCounts, allByGroup, allByCat, visible };
  }, [events, filter, catFilter, query]);

  return (
    <div className="flex h-full flex-col">
      <div className="border-b border-border px-6 py-4">
        <div className="flex items-start justify-between gap-4">
          <div>
            {/* 标题配图标（UI-1）：与 BrainPage 的 `<Brain size={20} className="text-primary"/>`
                同一形制，让各页顶部有一致的识别锚点。 */}
            <div className="flex items-center gap-2">
              <ScrollText size={20} className="text-primary" />
              <h1 className="text-lg font-semibold text-text-primary">{t("logs.title")}</h1>
            </div>
            <p className="mt-1 text-xs text-text-muted">{t("logs.subtitle")}</p>
          </div>
          {/* 搜索（UX#10）：与下方两维筛选正交叠加，徽标计数会跟着搜索结果走 */}
          <div className="relative shrink-0">
            <Search
              size={13}
              className="pointer-events-none absolute left-2 top-1/2 -translate-y-1/2 text-text-muted"
            />
            <input
              type="search"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder={t("logs.searchPlaceholder")}
              aria-label={t("logs.searchPlaceholder")}
              className="w-56 rounded-control border border-border bg-surface py-1.5 pl-7 pr-2 text-xs text-text-primary placeholder:text-text-muted focus:border-primary focus:outline-none"
            />
          </div>
        </div>

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
            {/* 区分「真的没有事件」与「搜索没命中」：后者若也显示「暂无事件」，
                用户会以为日志功能坏了，而实际只是搜索词太窄。 */}
            {query.trim() ? (
              <>
                <p className="text-sm">{t("logs.noMatch", { q: query.trim() })}</p>
                <button
                  onClick={() => setQuery("")}
                  className="rounded-control border border-border px-2 py-1 text-xs text-text-secondary hover:bg-surface-hover"
                >
                  {t("logs.clearSearch")}
                </button>
              </>
            ) : (
              <p className="text-sm">{t("logs.empty")}</p>
            )}
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

/** token 数的紧凑展示：≥10k 用 k 缩写，避免长数字把行挤变形。与后端 fmt_compact 同口径。 */
function fmtTokens(n: number): string {
  return n >= 10_000 ? `${(n / 1000).toFixed(1)}k` : String(n);
}

/**
 * 单条日志行。
 *
 * **用 memo 包住（PERF-1）**：本页 2s 轮询一次 `listAllEvents`，而后端事件环上限 500 条。
 * 无 memo 时每次轮询都要重渲染全部 500 行（每行含展开态 useState、若干 Badge、图标），
 * 而轮询的常态是「只在末尾追加一两条、前面几百条一字未改」。
 *
 * 同样**依赖 store 侧的 `reuseUnchanged`**：IPC 每次返回全新对象，不复用未变对象的话
 * `prevProps.entry === nextProps.entry` 恒为 false、这个 memo 就是白加的。
 *
 * 没有上虚拟滚动是**权衡后的决定**，不是遗漏：500 条封顶实测插入+布局 27ms、滚动重排 1ms，
 * 而本行支持展开（展开后高度从 ~40px 变到数百 px，且高度取决于异步拉回的 trace 正文长度），
 * 变高虚拟滚动要接测量回调、易出现跳动与滚动位置漂移。为 27ms 的一次性开销引入这份复杂度
 * 与一个新依赖不划算 —— memo 消掉的是每 2s 复发的那份成本，收益更大且零依赖。
 */
const LogRow = React.memo(function LogRow({ entry, lang }: { entry: EventLogEntry; lang: string }) {
  const t = useT();
  const [open, setOpen] = useState(false);
  // 链路快照按需拉取：列表接口不带 trace 正文（每 2s 轮询 × 单条最坏 40000 字符会拖卡界面），
  // 展开时才按事件 id 单取一条。undefined = 还没取过；null = 已滚出保留窗口。
  const [trace, setTrace] = useState<RequestTrace | null | undefined>(undefined);
  const [loadingTrace, setLoadingTrace] = useState(false);
  const meta = TYPE_META[entry.type] ?? TYPE_META.route;
  const Icon = meta.icon;
  // 所有类型都可展开：有 trace 的展开完整链路快照；无 trace 的展开 detail 全文
  //（长 detail 在收起态被 truncate 截断，展开后不截断，解决「日志太长看不了」）。
  const expandable = !!entry.hasTrace || entry.detail.length > 0;
  const time = new Date(entry.ts).toLocaleTimeString(lang === "en" ? "en-US" : "zh-CN");

  const toggle = () => {
    const next = !open;
    setOpen(next);
    // 首次展开且这条有快照 → 拉一次并缓存在本行 state 里（收起再展开不重复请求）。
    if (next && entry.hasTrace && trace === undefined && !loadingTrace) {
      setLoadingTrace(true);
      void api
        .getEventTrace(entry.id)
        .then((tr) => setTrace(tr))
        .catch((e) => {
          console.error("getEventTrace failed", e);
          setTrace(null); // 取不到就按「无快照」渲染，不让展开态空着
        })
        .finally(() => setLoadingTrace(false));
    }
  };

  return (
    <div className="rounded-control border border-border bg-surface">
      <div
        className={`flex items-center gap-3 px-3 py-2 ${expandable ? "cursor-pointer hover:bg-surface-hover" : ""}`}
        onClick={expandable ? toggle : undefined}
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
        {/* 路径可视化（FR-028 之三）：结构化展示「走了哪个 Key」，不靠 detail 字符串解析。
            detail 在折叠态被 truncate 截断时 Key 名可能看不见，而 keyName 是后端按 id 回填
            的可读名，独立于展示文本。request/failover/route 是「真的走了上游」的事件。 */}
        {entry.keyName && (
          <Badge variant="info">
            <KeyRound size={10} />
            {entry.keyName}
          </Badge>
        )}
        <span className={`flex-1 text-sm text-text-primary ${open ? "whitespace-pre-wrap break-words" : "truncate"}`}>
          {entry.detail}
        </span>
        {/* token 用量徽标：额度消耗是用户最关心的成本信号，给它独立位置而不是只埋在 detail 里。
            折叠条目上是 N 次的累计值（见后端 append_event_full 的说明）。 */}
        {entry.usage && (entry.usage.input > 0 || entry.usage.output > 0) && (
          <span
            className="shrink-0 rounded-full bg-surface-hover px-1.5 py-0.5 font-mono text-[10px] tabular-nums text-text-muted"
            title={t("logs.usageHint", {
              input: entry.usage.input.toLocaleString(),
              output: entry.usage.output.toLocaleString(),
              total: (entry.usage.input + entry.usage.output).toLocaleString(),
              cache: (entry.usage.cacheRead ?? 0).toLocaleString(),
            })}
          >
            ↑{fmtTokens(entry.usage.input)} ↓{fmtTokens(entry.usage.output)}
          </span>
        )}
        {/* 折叠计数：连续同类事件被合成一条时显示「×N」。1 不显示（避免每行都挂个 ×1 的噪音）。 */}
        {(entry.repeat ?? 1) > 1 && (
          <span
            className="shrink-0 rounded-full bg-surface-hover px-1.5 py-0.5 font-mono text-[10px] tabular-nums text-text-muted"
            title={t("logs.repeatHint")}
          >
            ×{entry.repeat}
          </span>
        )}
      </div>

      {open && entry.hasTrace && (
        loadingTrace ? (
          <div className="px-3 pb-2.5 text-[11px] text-text-muted">{t("logs.trace.loading")}</div>
        ) : trace ? (
          <TraceDetail trace={trace} />
        ) : trace === null ? (
          <div className="px-3 pb-2.5 text-[11px] text-text-muted">{t("logs.trace.evicted")}</div>
        ) : null
      )}
    </div>
  );
});

function TraceDetail({ trace }: { trace: RequestTrace }) {
  const t = useT();
  return (
    <div className="border-t border-border px-3 py-3 text-xs">
      {/* 截断警告 */}
      {trace.wasTruncated && (
        <div className="mb-3 flex items-center gap-2 rounded-control border border-warning/30 bg-warning/8 px-3 py-2 text-warning">
          <span className="font-semibold">{t("logs.trace.truncatedWarning")}</span>
        </div>
      )}
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
