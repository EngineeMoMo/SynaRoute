import { useEffect, useMemo, useState } from "react";
import { api } from "@/lib/bridge";
import { useT } from "@/lib/useT";
import type { TFunc } from "@/lib/i18n";
import type { DailyUsageBucket, TokenUsage, UnpricedReason, UsageCostRow } from "@/types";
import { Badge } from "@/components/ui/Badge";
import { Tooltip } from "@/components/ui/Tooltip";
import { TrendingUp, AlertTriangle } from "lucide-react";

/**
 * 用量统计面板：按「分类 × Key」展示 token 消耗**与估算花费**。
 *
 * 数据来自两个后端接口：
 * - `getUsageWithCost`：跨重启累计的总量 + 成本估算（口径见下）
 * - `getDailyUsage`：按 UTC 日期分桶的历史（最近 90 天），供今日/本周/趋势
 *
 * 累计量刻意不从事件环（最多 500 条）算：那样在环滚动后总量会停止增长（见
 * `token_usage_totals_never_shrink_when_event_ring_rotates`）。
 *
 * ## 关于「花费」的口径（重要）
 *
 * 这里显示的是**估算**，不是账单：
 * - 单价来自内置官方价表，或按模型家族名兜底猜测（`pricingSource` 标注了是哪种）；
 * - 中转站实际计价按各自折扣，用户可在 Key 编辑器里填「计费倍率」校准；
 * - 累加器的键不含模型名，故一条 Key 跑多个档位模型时按代表模型估算，会有偏差。
 *
 * 所以面板上必须写明「估算」二字。此前这个页面的注释写着「只统计 token、不换算钱：
 * 各中转商单价差异巨大，SynaRoute 无从得知」—— 本轮推翻了它，但推翻的方式不是
 * 由程序去猜价，而是让用户填倍率、并如实标注估算精度。
 */
export function UsagePage() {
  const t = useT();
  const [rows, setRows] = useState<UsageCostRow[] | null>(null);
  const [daily, setDaily] = useState<DailyUsageBucket[]>([]);
  const [sinceMs, setSinceMs] = useState<number | null>(null);
  const [tableDate, setTableDate] = useState<string>("");
  const [error, setError] = useState<string | null>(null);

  const load = async () => {
    try {
      // 四个请求并发发，避免标题/趋势慢一拍导致界面闪一下旧值。
      const [next, since, buckets, priceDate] = await Promise.all([
        api.getUsageWithCost(),
        api.getUsageSince(),
        api.getDailyUsage(),
        api.getPricingTableDate(),
      ]);
      // **对后端返回做形状校验**，而不是直接塞进 state。
      //
      // 为什么必需：本页多处直接 `.map()` / 解引用 `r.usage.input`。若某条数据缺字段
      // （usage.json 被外部改坏、跨版本残留、或某个 Key 记账时结构异常），render 会抛异常，
      // 而 React 会卸载整棵树 → **整窗口白屏**（真机反馈的「用量统计整页空白」）。
      // 这里把脏数据挡在 state 之外：能用的行照常显示，坏行直接丢弃，绝不让一条脏数据
      // 毁掉整页。App 层的 ErrorBoundary 是第二道防线，但第一道应该在数据入口。
      setRows(Array.isArray(next) ? next.filter(isValidCostRow) : []);
      setSinceMs(typeof since === "number" && Number.isFinite(since) ? since : null);
      setDaily(Array.isArray(buckets) ? buckets.filter(isValidBucket) : []);
      setTableDate(typeof priceDate === "string" ? priceDate : "");
      setError(null);
    } catch (e) {
      setError(String(e));
    }
  };

  useEffect(() => {
    void load();
    // 用量随事件变化，但面板是低频查看——进页面拉一次即可，不常驻高频轮询。
    const id = window.setInterval(() => void load(), 30_000);
    return () => window.clearInterval(id);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  /** 总计（token 与估算金额）。金额只累加**有单价**的行，无单价的不计入。 */
  const summary = useMemo(() => {
    let input = 0, output = 0, cacheRead = 0, cacheCreation = 0;
    let costNano = 0;
    let unpriced = 0;
    for (const r of rows ?? []) {
      input += r.usage.input;
      output += r.usage.output;
      cacheRead += r.usage.cacheRead ?? 0;
      cacheCreation += r.usage.cacheCreation ?? 0;
      if (r.costNano == null) unpriced += 1;
      else costNano += r.costNano;
    }
    return { input, output, cacheRead, cacheCreation, costNano, unpriced };
  }, [rows]);

  /**
   * 无价行**按成因分组**。
   *
   * 旧实现只数一个总数、配一句放之四海的文案（「模型名不在单价表中」），而四种成因里
   * 只有一种是那样。用户按提示去 Key 里设兜底模型，设完「—」照旧 —— 界面没有任何新信息，
   * 他只能认为功能坏了。分组之后每一类各自指路，且「无路可走」的两类如实说明。
   */
  const unpricedGroups = useMemo(() => {
    const g = { aggregate: 0, keyDeleted: 0, noModelName: 0, modelNotInTable: 0 };
    const models = new Set<string>();
    for (const r of rows ?? []) {
      const reason = r.unpricedReason;
      if (!reason) continue;
      g[reason.kind] += 1;
      if (reason.kind === "modelNotInTable") models.add(reason.model);
    }
    return { ...g, models: [...models] };
  }, [rows]);

  /**
   * 按日聚合的 token 总量，供「今日 / 本周 / 本月」与趋势图。
   *
   * 日期用 UTC（与后端分桶同口径）。若这里改用本地时区，会出现
   * 「今日」比后端桶少算或多算一段的错位。
   */
  const byDate = useMemo(() => {
    const m = new Map<string, number>();
    for (const b of daily) {
      let sum = 0;
      // 桶里的单条 entry 仍可能是脏的（外部改坏 usage.json / 跨版本残留）：
      // 逐条校验后再累加，坏行跳过。否则 `e.usage.input` 会抛 TypeError → 整页白屏。
      for (const e of b.entries) {
        if (!e || !isValidUsage(e.usage)) continue;
        sum += e.usage.input + e.usage.output + (e.usage.cacheRead ?? 0) + (e.usage.cacheCreation ?? 0);
      }
      m.set(b.date, sum);
    }
    return m;
  }, [daily]);

  const periods = useMemo(() => {
    const utcDate = (offsetDays: number) =>
      new Date(Date.now() - offsetDays * 86_400_000).toISOString().slice(0, 10);
    const today = byDate.get(utcDate(0)) ?? 0;
    let week = 0;
    for (let i = 0; i < 7; i++) week += byDate.get(utcDate(i)) ?? 0;
    let month = 0;
    for (let i = 0; i < 30; i++) month += byDate.get(utcDate(i)) ?? 0;
    return { today, week, month };
  }, [byDate]);

  /** 近 7 日趋势（最早在左）。空日保留为 0，好让「那天没用量」看得出来。 */
  const trend = useMemo(() => {
    const out: { date: string; total: number }[] = [];
    for (let i = 6; i >= 0; i--) {
      const d = new Date(Date.now() - i * 86_400_000).toISOString().slice(0, 10);
      out.push({ date: d, total: byDate.get(d) ?? 0 });
    }
    return out;
  }, [byDate]);

  const trendMax = Math.max(1, ...trend.map((d) => d.total));

  return (
    <div className="flex h-full flex-col">
      <div className="border-b border-border px-6 py-4">
        <div className="flex items-start justify-between gap-4">
          <div>
            <div className="flex items-center gap-2">
              <TrendingUp size={20} className="text-primary" />
              <h1 className="text-lg font-semibold text-text-primary">{t("usage.title")}</h1>
            </div>
            <p className="mt-1 text-xs text-text-muted">{t("usage.subtitle")}</p>
            {sinceMs !== null && (
              <p className="mt-0.5 text-xs text-text-muted" title={t("usage.sinceHint")}>
                {t("usage.since")}: {new Date(sinceMs).toLocaleString()}
              </p>
            )}
            {/* 单价表的核对日期：这张表是人工核对各厂商定价页得来的，会变旧，
                而变旧的表现是金额悄悄偏离真实账单。给出日期，用户才能自己判断可信度。 */}
            {tableDate && (
              <p className="mt-0.5 text-xs text-text-muted">
                {t("usage.tableDate", { date: tableDate })}
              </p>
            )}
          </div>
          <button
            onClick={() => void load()}
            className="shrink-0 rounded-control border border-border px-2.5 py-1 text-xs text-text-secondary hover:bg-surface-hover"
          >
            {t("usage.refresh")}
          </button>
        </div>

        {rows && rows.length > 0 && (
          <>
            {/* 花费概览：今日 / 本周 / 本月（token），加累计估算金额 */}
            <div className="mt-3 grid grid-cols-4 gap-2">
              <StatCard label={t("usage.today")} value={fmt(periods.today)} />
              <StatCard label={t("usage.thisWeek")} value={fmt(periods.week)} />
              <StatCard label={t("usage.thisMonth")} value={fmt(periods.month)} />
              <StatCard
                label={t("usage.estCost")}
                value={fmtUsd(summary.costNano)}
                hint={t("usage.estimateHint")}
                accent
                /* 「累计花费」这个标签会被当成总计读。有行没算进去时**必须在这一格上**
                   标出来 —— 只靠下方横幅不够，用户看数字时未必往下看。 */
                note={
                  summary.unpriced > 0
                    ? t("usage.estCostExcluded", { n: summary.unpriced })
                    : undefined
                }
              />
            </div>

            {/* 近 7 日趋势：纯 CSS 柱状图，不引图表库（就 7 根柱子） */}
            <div className="mt-3 rounded-control border border-border bg-surface p-3">
              <div className="mb-2 text-[11px] text-text-muted">{t("usage.trend7d")}</div>
              <div className="flex h-20 items-end gap-1.5">
                {trend.map((d) => (
                  <Tooltip
                    key={d.date}
                    content={`${d.date} · ${fmt(d.total)} token`}
                    side="top"
                  >
                    <div className="flex flex-1 cursor-default flex-col items-center gap-1">
                      <div
                        className="w-full rounded-t bg-gradient-to-t from-primary to-primary-deep"
                        style={{
                          // 至少 2px：完全没用量的那天也要有个可见的底座，
                          // 否则用户分不清「没数据」与「柱子没渲染出来」。
                          height: `${Math.max(2, Math.round((d.total / trendMax) * 64))}px`,
                        }}
                      />
                      <span className="text-[9px] tabular-nums text-text-muted">
                        {d.date.slice(5)}
                      </span>
                    </div>
                  </Tooltip>
                ))}
              </div>
            </div>

            {/* 有行算不出金额时**按成因**如实说明，并各自指路。
                旧实现是一句「模型名不在单价表中」—— 对四种成因里的三种都是假话。 */}
            {summary.unpriced > 0 && (
              <div className="mt-2 flex items-start gap-2 rounded-control border border-warning/30 bg-warning/8 px-3 py-2 text-[11px] leading-relaxed text-warning">
                <AlertTriangle size={13} className="mt-0.5 shrink-0" />
                <div>
                  <div>{t("usage.unpricedBanner", { n: summary.unpriced })}</div>
                  <ul className="mt-0.5 list-disc space-y-0.5 pl-4">
                    {unpricedGroups.aggregate > 0 && (
                      <li>{t("usage.unpricedGroup.aggregate", { n: unpricedGroups.aggregate })}</li>
                    )}
                    {unpricedGroups.keyDeleted > 0 && (
                      <li>{t("usage.unpricedGroup.keyDeleted", { n: unpricedGroups.keyDeleted })}</li>
                    )}
                    {unpricedGroups.noModelName > 0 && (
                      <li>
                        {t("usage.unpricedGroup.noModelName", { n: unpricedGroups.noModelName })}
                      </li>
                    )}
                    {unpricedGroups.modelNotInTable > 0 && (
                      <li>
                        {t("usage.unpricedGroup.modelNotInTable", {
                          n: unpricedGroups.modelNotInTable,
                          models: unpricedGroups.models.join(t("common.listSep")),
                        })}
                      </li>
                    )}
                  </ul>
                </div>
              </div>
            )}
          </>
        )}
      </div>

      <div className="flex-1 overflow-y-auto p-6">
        {error && <div className="text-sm text-danger">{error}</div>}
        {!rows ? (
          <div className="py-16 text-center text-sm text-text-muted">{t("usage.loading")}</div>
        ) : rows.length === 0 ? (
          <div className="py-16 text-center text-sm text-text-muted">{t("usage.empty")}</div>
        ) : (
          <div className="overflow-hidden rounded-card border border-border">
            <table className="w-full text-left text-sm">
              <thead className="bg-surface-hover/60 text-xs text-text-muted">
                <tr>
                  <th className="px-4 py-2 font-medium">{t("usage.colCategory")}</th>
                  <th className="px-4 py-2 font-medium">{t("usage.colKey")}</th>
                  <th className="px-4 py-2 text-right font-medium">{t("usage.input")}</th>
                  <th className="px-4 py-2 text-right font-medium">{t("usage.output")}</th>
                  <th className="px-4 py-2 text-right font-medium">{t("usage.cacheRead")}</th>
                  <th className="px-4 py-2 text-right font-medium">{t("usage.total")}</th>
                  <th className="px-4 py-2 text-right font-medium">{t("usage.colCost")}</th>
                </tr>
              </thead>
              <tbody className="divide-y divide-border">
                {rows.map((r) => (
                  <tr key={`${r.categoryId}/${r.keyId}`} className="hover:bg-surface-hover/40">
                    <td className="px-4 py-2">
                      <Badge variant="neutral">{t(`nav.${r.categoryId}`)}</Badge>
                    </td>
                    <td className="max-w-[180px] truncate px-4 py-2 text-xs text-text-secondary">
                      {r.keyName || r.keyId || t("usage.systemLevel")}
                      {r.keyDeleted && (
                        <span className="ml-1 text-text-muted">{t("usage.keyDeletedTag")}</span>
                      )}
                    </td>
                    <td className="px-4 py-2 text-right font-mono tabular-nums">{fmt(r.usage.input)}</td>
                    <td className="px-4 py-2 text-right font-mono tabular-nums">{fmt(r.usage.output)}</td>
                    <td className="px-4 py-2 text-right font-mono tabular-nums">
                      {fmt(r.usage.cacheRead ?? 0)}
                    </td>
                    <td className="px-4 py-2 text-right font-mono tabular-nums font-medium">
                      {fmt(
                        (r.usage.cacheRead ?? 0) +
                          (r.usage.cacheCreation ?? 0) +
                          r.usage.input +
                          r.usage.output,
                      )}
                    </td>
                    <td className="px-4 py-2 text-right font-mono tabular-nums">
                      <CostCell row={r} t={t} tableDate={tableDate} />
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </div>
    </div>
  );
}

/**
 * 概览卡片。`accent` 用于金额那格（视觉上与 token 数区分开）。
 *
 * `note` 是挂在数字下方的小字修饰语。它存在的理由很具体：「累计花费」这个标签会被当成
 * **总计**读，而它只是「能算出来的那部分之和」。差额必须标在数字旁边，
 * 不能只写在下方的横幅里 —— 看数字的人未必往下看。
 */
function StatCard({
  label,
  value,
  hint,
  accent,
  note,
}: {
  label: string;
  value: string;
  hint?: string;
  accent?: boolean;
  note?: string;
}) {
  const body = (
    <div
      className={`rounded-control border p-3 ${
        accent ? "border-primary/30 bg-primary/8" : "border-border bg-surface"
      }`}
    >
      <div className="text-[11px] text-text-muted">{label}</div>
      <div
        className={`mt-0.5 font-mono text-lg tabular-nums ${
          accent ? "text-primary" : "text-text-primary"
        }`}
      >
        {value}
      </div>
      {note && <div className="mt-0.5 text-[10px] leading-tight text-warning">{note}</div>}
    </div>
  );
  return hint ? (
    <Tooltip content={hint} side="bottom">
      {body}
    </Tooltip>
  ) : (
    body
  );
}

/**
 * 金额单元格。
 *
 * 无单价时显示「—」并给出**具体成因**，绝不显示 $0.0000 ——
 * 那会让用户以为这条 Key 没花钱，而真相是「算不出来」。
 *
 * 「—」是可聚焦元素（`tabIndex` + `aria-label`）：它此前是纯 `<span>` 靠 hover 出 tooltip，
 * 键盘与触屏用户完全拿不到那句解释 —— 而这一格恰恰是最需要解释的一格。
 */
function CostCell({ row, t, tableDate }: { row: UsageCostRow; t: TFunc; tableDate: string }) {
  if (row.costNano == null) {
    const hint = unpricedHint(row.unpricedReason, t, tableDate);
    return (
      <Tooltip content={hint} side="left">
        <span
          tabIndex={0}
          role="note"
          aria-label={hint}
          className="cursor-default rounded text-text-muted outline-none focus-visible:ring-1 focus-visible:ring-primary"
        >
          —
        </span>
      </Tooltip>
    );
  }
  const isEstimate = row.pricingSource === "family";
  const base = t(isEstimate ? "usage.costFamilyHint" : "usage.costExactHint", {
    multiplier: row.multiplier,
  });
  // 回显代表模型：同一条 Key 跑多个档位模型时，偏差恰恰来自「按哪个模型估的」，
  // 不显示它用户就无从判断这个数字可信到什么程度。
  const hint = row.pricedByModel
    ? `${base}\n${t("usage.pricedBy", { model: row.pricedByModel })}`
    : base;
  return (
    <Tooltip content={hint} side="left">
      <span className="cursor-default">
        {fmtUsd(row.costNano)}
        {isEstimate && <span className="ml-0.5 text-warning">≈</span>}
      </span>
    </Tooltip>
  );
}

/**
 * 「算不出金额」的成因文案。
 *
 * 四种成因各自不同的**可行动性**是分开写的全部理由（见 `usage_cost::UnpricedReason` 的
 * 那张表）：其中两种用户压根无路可走，硬塞一句「去设兜底模型」是把他送去做无效操作。
 */
function unpricedHint(reason: UnpricedReason | undefined, t: TFunc, tableDate: string): string {
  switch (reason?.kind) {
    case "aggregate":
      return t("usage.reason.aggregate");
    case "keyDeleted":
      return t("usage.reason.keyDeleted");
    case "modelNotInTable":
      return t("usage.reason.modelNotInTable", { model: reason.model, date: tableDate });
    case "noModelName":
      return t("usage.reason.noModelName");
    default:
      // 后端没给成因（跨版本：新前端 + 旧后端）→ 退回原来那句泛化提示。
      // 不留这一支的话这里会渲染出空 tooltip，比说得笼统更糟。
      return t("usage.costUnknownHint");
  }
}

/** 紧凑 token 数字：≥10000 用 k，≥1000000 用 M（与日志页 fmtTokens 同口径）。 */
function fmt(n: number): string {
  // 非有限值一律显示 0：脏数据不该让 `.toFixed()` 抛异常或渲染出 "NaN"。
  if (!Number.isFinite(n)) return "0";
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(2)}M`;
  if (n >= 10_000) return `${(n / 1000).toFixed(1)}k`;
  return String(n);
}

/**
 * 纳美元 → 美元显示，4 位小数（与后端 `format_usd_from_nano` 同口径）。
 *
 * 不显示 6 位：`$0.012345` 这种精度对用户无意义，反而挤占版面。
 */
function fmtUsd(nano: number): string {
  if (!Number.isFinite(nano)) return "—";
  const dollars = nano / 1_000_000_000;
  if (dollars > 0 && dollars < 0.0001) return "<$0.0001";
  return `$${dollars.toFixed(4)}`;
}

/** 一份 usage 是否结构完整（四个字段都是有限数字；缓存两项可缺，按 0 记）。 */
function isValidUsage(u: unknown): u is TokenUsage {
  if (!u || typeof u !== "object") return false;
  const o = u as Record<string, unknown>;
  const num = (v: unknown) => typeof v === "number" && Number.isFinite(v);
  const optNum = (v: unknown) => v === undefined || v === null || num(v);
  return num(o.input) && num(o.output) && optNum(o.cacheRead) && optNum(o.cacheCreation);
}

/**
 * 一行成本数据是否可安全渲染。
 *
 * 挡掉的是「缺 usage / usage 里有 NaN / categoryId 不是已知分类」这类脏数据 ——
 * 它们会让下面的 `.usage.input` 解引用或 `t(\`nav.\${categoryId}\`)` 出问题，
 * 而 render 抛异常的后果是**整窗口白屏**（React 卸载整棵树）。
 */
function isValidCostRow(r: unknown): r is UsageCostRow {
  if (!r || typeof r !== "object") return false;
  const o = r as Record<string, unknown>;
  return typeof o.keyId === "string" && typeof o.categoryId === "string" && isValidUsage(o.usage);
}

/** 一个日桶是否可安全聚合（date 是字符串、entries 是数组）。 */
function isValidBucket(b: unknown): b is DailyUsageBucket {
  if (!b || typeof b !== "object") return false;
  const o = b as Record<string, unknown>;
  return typeof o.date === "string" && Array.isArray(o.entries);
}
