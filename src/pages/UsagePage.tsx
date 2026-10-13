import { useMemo, useRef, useState } from "react";
import { api } from "@/lib/bridge";
import { useBackendEvent, FALLBACK_POLL_MS } from "@/lib/useBackendEvents";
import { usePolling } from "@/lib/usePolling";
import { useT } from "@/lib/useT";
import type { TFunc } from "@/lib/i18n";
import { inRange, lastNDayKeys, localDateKey, startOfMonthKey, startOfWeekKey } from "@/lib/usagePeriods";
import type { DailyCost, PricingStatus, TokenUsage, UnpricedReason, UsageCostRow, UsagePartCost } from "@/types";
import { AnimatedNumber } from "@/components/ui/AnimatedNumber";
import { Badge } from "@/components/ui/Badge";
import { RefreshButton } from "@/components/ui/RefreshButton";
import { InlineAlert } from "@/components/ui/InlineAlert";
import { PageHeader } from "@/components/ui/PageHeader";
import { Tooltip } from "@/components/ui/Tooltip";
import { TrendingUp } from "lucide-react";

/**
 * 用量统计面板：按「分类 × Key」展示 token 消耗**与估算花费**。
 *
 * 数据来自两个后端接口：
 * - `getUsageWithCost`：跨重启累计的总量 + 按模型逐格的成本估算（口径见下）
 * - `getDailyCost`：按天的花费与 token（最近 90 天，本机时区切天），供今日/本周/本月/趋势
 *
 * 累计量刻意不从事件环（最多 500 条）算：那样在环滚动后总量会停止增长（见
 * `token_usage_totals_never_shrink_when_event_ring_rotates`）。
 *
 * ## 关于「花费」的口径（重要）
 *
 * 这里显示的是**估算**，不是账单：
 * - 单价来自内置官方价表（缺的由每天拉取的在线价格表补，同名内置赢），或按模型家族名兜底猜测
 *   （`pricingSource` 标注了是哪种）；
 * - 中转站实际计价按各自折扣，用户可在 Key 编辑器里填「计费倍率」校准；
 * - 每个模型按自己的单价算（2026-10-12 起账本记了模型）；只有升级前没记模型的历史用量
 *   才按 Key 的代表模型估，并标「≈」。
 *
 * 所以面板上必须写明「估算」二字。此前这个页面的注释写着「只统计 token、不换算钱：
 * 各中转商单价差异巨大，SynaRoute 无从得知」—— 本轮推翻了它，但推翻的方式不是
 * 由程序去猜价，而是让用户填倍率、并如实标注估算精度。
 */
export function UsagePage() {
  const t = useT();
  const [rows, setRows] = useState<UsageCostRow[] | null>(null);
  const [daily, setDaily] = useState<DailyCost[]>([]);
  const [sinceMs, setSinceMs] = useState<number | null>(null);
  /** 单价表现状（内置表核对日期 + 在线表）。取不到时为 null，表头那一行就不画。 */
  const [pricing, setPricing] = useState<PricingStatus | null>(null);
  const [error, setError] = useState<string | null>(null);
  /** 「清理」的两步确认：第一下只把这一行换成「确认清理 / 取消」（同 Key 卡片的删除）。 */
  const [confirmingPurge, setConfirmingPurge] = useState<string | null>(null);
  /** 清理成功但没落盘时的说明（不是错误：界面上那一行确实已经没了）。 */
  const [notice, setNotice] = useState<string | null>(null);

  /**
   * 在途请求的代际号。**只允许最新那一轮提交结果。**
   *
   * 没有它时的竞态：事件推送与兜底轮询几乎同时触发两轮 load，先发的那轮后到
   * （IPC 之间没有先后保证）→ 它把**更旧**的快照写进 state。表现是「刚发的请求，
   * 用量数字反而退回去了」，而下一轮才纠正。本仓在 `loadCategory` / 分类页横幅上
   * 已经为同一形态修过两次。
   */
  const genRef = useRef(0);

  const load = async () => {
    const gen = ++genRef.current;
    try {
      // 四个请求并发发，避免标题/趋势慢一拍导致界面闪一下旧值。
      const [next, since, days, priceStatus] = await Promise.all([
        api.getUsageWithCost(),
        api.getUsageSince(),
        api.getDailyCost(),
        api.getPricingStatus(),
      ]);
      // 过期结果丢弃（见 genRef）。
      if (gen !== genRef.current) return;
      // **对后端返回做形状校验**，而不是直接塞进 state。
      //
      // 为什么必需：本页多处直接 `.map()` / 解引用 `r.usage.input`。若某条数据缺字段
      // （usage.json 被外部改坏、跨版本残留、或某个 Key 记账时结构异常），render 会抛异常，
      // 而 React 会卸载整棵树 → **整窗口白屏**（真机反馈的「用量统计整页空白」）。
      // 这里把脏数据挡在 state 之外：能用的行照常显示，坏行直接丢弃，绝不让一条脏数据
      // 毁掉整页。App 层的 ErrorBoundary 是第二道防线，但第一道应该在数据入口。
      setRows(Array.isArray(next) ? next.filter(isValidCostRow) : []);
      setSinceMs(typeof since === "number" && Number.isFinite(since) ? since : null);
      setDaily(Array.isArray(days) ? days.filter(isValidDailyCost) : []);
      setPricing(isValidPricingStatus(priceStatus) ? priceStatus : null);
      setError(null);
    } catch (e) {
      if (gen !== genRef.current) return;
      setError(String(e));
    }
  };

  /**
   * 🔴 **实时性靠后端推送，不靠缩短轮询**（2026-09-09 用户实报「用量统计也不是实时」）。
   *
   * 后端在收到 usage 的**同一次调用内**就更新了内存累计（`append_event_full`），
   * 而 `daily_usage_buckets` 也会把尚未落盘的增量并进当天桶 —— 也就是说数据早就是新的，
   * **陈旧完全发生在前端**：本页此前只在挂载、手点刷新、以及一个裸 `setInterval(30s)` 时拉数据，
   * 而全仓早就有 `useBackendEvent`（日志页、分类页都在用），本页从来没接。
   * 所以改的不是 60s 落盘周期（那只关系到异常退出丢多少历史），而是这里的接线。
   *
   * 订阅两个主题，各自对应一类用户可感知的变化：
   * - `logs`：一次转发记完账就会发（流式在流末补记时也发）→ 用量数字跟着涨。
   *   后端对该主题限速 1500ms，密集请求下不会把这四个 IPC 打成高频。
   * - `config`：Key 增删改名、改计费倍率会发 → 表格里的名字/金额跟着变，
   *   **新加的 Key 也在这时出现**（后端现在为零用量 Key 也产出占位行）。
   *
   * 轮询降级为兜底并改用 `usePolling`：它自带「窗口不可见时停表、重新可见立即补一次」，
   * 而裸 `setInterval` 在最小化到托盘后仍在跑（本应用的常态使用姿势就是丢后台）。
   */
  useBackendEvent(["logs", "config"], () => void load());
  usePolling(() => void load(), FALLBACK_POLL_MS);

  /** 清理一条已离开配置的 Key 的用量（不可逆，两步确认）。 */
  const purge = async (keyId: string) => {
    setConfirmingPurge(null);
    try {
      const persisted = await api.purgeKeyUsage(keyId);
      setNotice(persisted ? null : t("usage.purgeNotPersisted"));
      setError(null);
    } catch (e) {
      setError(String((e as Error)?.message ?? e));
    }
    await load();
  };

  /**
   * 总计（token 与估算金额）。金额累加**能算出的部分**；一格都算不出的行（`unpriced`）与
   * 只算出一部分的行（`partial`）都要在「累计花费」那一格上标出来 —— 否则它会被当成总计读。
   */
  const summary = useMemo(() => {
    let input = 0, output = 0, cacheRead = 0, cacheCreation = 0;
    let costNano = 0;
    let unpriced = 0;
    let partial = 0;
    for (const r of rows ?? []) {
      input += r.usage.input;
      output += r.usage.output;
      cacheRead += r.usage.cacheRead ?? 0;
      cacheCreation += r.usage.cacheCreation ?? 0;
      // 🔴 占位行不计入「未计入金额的条数」：那个角标是在说「有 N 行的钱没算进这个总数」，
      // 而它们压根没有消耗可算。把它们算进去会让一个刚加了 3 条 Key 的用户看到
      // 「＋3 行未计入」，然后去翻横幅找原因 —— 而横幅里一条也没有（它们没有成因），
      // 那是一条指向空处的提示。
      if (!r.hasRecordedUsage) continue;
      if (r.costNano == null) unpriced += 1;
      else {
        costNano += r.costNano;
        if (r.unpricedReason) partial += 1;
      }
    }
    return { input, output, cacheRead, cacheCreation, costNano, unpriced, partial };
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
    // 占位行（还没捕获过 usage）刻意不出现在这里：后端不给它们 unpricedReason，
    // 因为那张表的每一支都在指挥用户去改配置，而这里没有任何配置需要改。
    return { ...g, models: [...models] };
  }, [rows]);

  /** 超预算的 Key 名（后端已判好 `overBudget`，前端只汇总展示、不自己比大小）。 */
  const overBudgetKeys = useMemo(
    () => (rows ?? []).filter((r) => r.overBudget).map((r) => r.keyName || r.keyId),
    [rows],
  );

  /**
   * 「今日 / 本周 / 本月」：日历口径、**本机时区**（与后端 v4 的日桶同口径，见 `usagePeriods`）。
   * 每一格是 `{ 花费, token, 算不出的 token }` 三个数之和 —— 后者非 0 时卡片上要标出来。
   */
  const periods = useMemo(() => {
    const now = new Date();
    const today = localDateKey(now);
    const sum = (from: string) =>
      daily
        .filter((d) => inRange(d.date, from, today))
        .reduce(
          (acc, d) => ({ cost: acc.cost + d.costNano, tokens: acc.tokens + d.tokens, unpriced: acc.unpriced + d.unpricedTokens }),
          { cost: 0, tokens: 0, unpriced: 0 },
        );
    return { today: sum(today), week: sum(startOfWeekKey(now)), month: sum(startOfMonthKey(now)) };
  }, [daily]);

  /** 近 7 日趋势（最早在左，滚动窗口）。没有用量的那天后端不产出，这里补成 0，好让「那天没用」看得出来。 */
  const trend = useMemo(() => {
    const byDate = new Map(daily.map((d) => [d.date, d]));
    return lastNDayKeys(new Date(), 7).map((date) => {
      const d = byDate.get(date);
      return { date, tokens: d?.tokens ?? 0, cost: d?.costNano ?? 0, unpriced: d?.unpricedTokens ?? 0 };
    });
  }, [daily]);

  const trendMax = Math.max(1, ...trend.map((d) => d.tokens));

  return (
    <div className="flex h-full flex-col">
      <PageHeader
        icon={TrendingUp}
        title={t("usage.title")}
        description={
          <>
            <p>{t("usage.subtitle")}</p>
            {sinceMs !== null && <p className="mt-0.5" title={t("usage.sinceHint")}>{t("usage.since")}: {new Date(sinceMs).toLocaleString()}</p>}
            {pricing && <PricingLine status={pricing} t={t} />}
          </>
        }
        actions={
          <RefreshButton
            onRefresh={load}
            idleLabel={t("usage.refresh")}
            busyLabel={t("usage.refreshing")}
          />
        }
      />

      <div className="px-6">
        {rows && rows.length > 0 && (
          <>
            {/* 花费概览：今日 / 本周 / 本月（日历口径，主数字是花费、token 作副行），加累计估算金额 */}
            <div className="mt-3 grid grid-cols-4 gap-2">
              {(["today", "week", "month"] as const).map((k) => (
                <StatCard
                  key={k}
                  label={t(k === "today" ? "usage.today" : k === "week" ? "usage.thisWeek" : "usage.thisMonth")}
                  value={periods[k].cost}
                  format={fmtUsd}
                  hint={t("usage.periodHint")}
                  sub={t("usage.periodTokens", { tokens: fmt(periods[k].tokens) })}
                  /* 有一部分算不出时这个金额只是下限 —— 必须标在数字旁边，否则会被当成全部。 */
                  note={
                    periods[k].unpriced > 0
                      ? t("usage.periodUnpriced", { tokens: fmt(periods[k].unpriced) })
                      : undefined
                  }
                />
              ))}
              <StatCard
                label={t("usage.estCost")}
                value={summary.costNano}
                format={fmtUsd}
                hint={t("usage.estimateHint")}
                accent
                /* 「累计花费」这个标签会被当成总计读。有行没（全部）算进去时**必须在这一格上**
                   标出来 —— 只靠下方横幅不够，用户看数字时未必往下看。 */
                note={
                  summary.unpriced + summary.partial > 0
                    ? t("usage.estCostExcluded", { n: summary.unpriced + summary.partial })
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
                    content={t(d.unpriced > 0 ? "usage.trendTipPartial" : "usage.trendTip", {
                      date: d.date,
                      tokens: fmt(d.tokens),
                      cost: fmtUsd(d.cost),
                      unpriced: fmt(d.unpriced),
                    })}
                    side="top"
                  >
                    <div className="flex flex-1 cursor-default flex-col items-center gap-1">
                      <div
                        className="w-full rounded-t bg-gradient-to-t from-primary to-primary-deep"
                        style={{
                          // 至少 2px：完全没用量的那天也要有个可见的底座，
                          // 否则用户分不清「没数据」与「柱子没渲染出来」。
                          height: `${Math.max(2, Math.round((d.tokens / trendMax) * 64))}px`,
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
            {summary.unpriced + summary.partial > 0 && (
              <InlineAlert tone="warning" className="mt-2">
                <div>{t("usage.unpricedBanner", { n: summary.unpriced + summary.partial })}</div>
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
              </InlineAlert>
            )}

            {/* 超预算告警：后端已按每 Key 的 budgetUsd 判好 overBudget，这里只汇总。
                tone=danger（红），与「算不出金额」那条 warning（橙）区分开 —— 一个是钱到顶了、
                要行动，一个只是估不出、无害。 */}
            {overBudgetKeys.length > 0 && (
              <InlineAlert tone="danger" className="mt-2">
                {t("usage.overBudgetBanner", {
                  n: String(overBudgetKeys.length),
                  keys: overBudgetKeys.join(t("common.listSep")),
                })}
              </InlineAlert>
            )}
          </>
        )}
      </div>

      <div className="flex-1 overflow-y-auto p-6">
        {error && <InlineAlert tone="danger">{error}</InlineAlert>}
        {notice && <InlineAlert tone="warning" className="mb-2">{notice}</InlineAlert>}
        {!rows ? (
          <div className="py-16 text-center text-sm text-text-muted">{t("usage.loading")}</div>
        ) : rows.length === 0 ? (
          <div className="py-16 text-center text-sm text-text-muted">{t("usage.empty")}</div>
        ) : (
          <div className="sr-panel overflow-hidden">
            <table className="w-full text-left text-sm">
              <thead className="border-b border-border/80 bg-surface-hover/60 text-xs text-text-muted">
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
                  <tr key={`${r.categoryId}/${r.keyId}`} className="transition-colors hover:bg-surface-hover/50">
                    <td className="px-4 py-2">
                      <Badge variant="neutral">{t(`nav.${r.categoryId}`)}</Badge>
                    </td>
                    <td className="max-w-[240px] px-4 py-2 text-xs text-text-secondary">
                      <div className="flex items-center gap-1">
                        <span className="min-w-0 truncate">{r.keyName || r.keyId || t("usage.systemLevel")}</span>
                        {r.keyDeleted && <span className="shrink-0 text-text-muted">{t("usage.keyDeletedTag")}</span>}
                        {r.overBudget && (
                          <span className="shrink-0 font-medium text-danger">{t("usage.overBudget")}</span>
                        )}
                        {/* 只给已离开配置的 Key：用删除按钮删的 Key 会自动连用量一起删，不会出现在这里。 */}
                        {r.keyDeleted && r.keyId && (
                          <PurgeControl
                            confirming={confirmingPurge === r.keyId}
                            onAsk={() => setConfirmingPurge(r.keyId)}
                            onCancel={() => setConfirmingPurge(null)}
                            onConfirm={() => void purge(r.keyId)}
                            t={t}
                          />
                        )}
                      </div>
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
                      <CostCell row={r} t={t} pricing={pricing} />
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
  format,
  hint,
  accent,
  sub,
  note,
}: {
  label: string;
  /** 原始数值。**不传格式化后的字符串** —— 补间需要能做算术的数。 */
  value: number;
  format: (n: number) => string;
  hint?: string;
  accent?: boolean;
  /** 数字下方的灰色副行（花费卡片用它显示 token 数）。 */
  sub?: string;
  note?: string;
}) {
  const body = (
    <div
      className={`rounded-control border p-3 ${
        accent ? "border-primary/30 bg-primary/8" : "border-border bg-surface"
      }`}
    >
      <div className="text-[11px] text-text-muted">{label}</div>
      {/* `tabular-nums` 在这里是**必须**的，不是排版偏好：补间期间每一帧的数字宽度
          不同，比例字宽会让这一格左右横跳。等宽数字让它只有值在变、位置不动。 */}
      <div
        className={`mt-0.5 font-mono text-lg tabular-nums ${
          accent ? "text-primary" : "text-text-primary"
        }`}
      >
        <AnimatedNumber value={value} format={format} />
      </div>
      {sub && <div className="mt-0.5 font-mono text-[11px] tabular-nums text-text-muted">{sub}</div>}
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
function CostCell({ row, t, pricing }: { row: UsageCostRow; t: TFunc; pricing: PricingStatus | null }) {
  // 还没捕获过 usage：显示「尚无用量」而不是 $0.0000。
  //
  // 🔴 后者读起来是「已经在服务、而且免费」，而真相是我们一次上游回报都没收到
  // （可能尚未使用、流仍在进行、或这个中转站不回 usage）。三种成因我们**分辨不出**，
  // 所以 tooltip 写成条件句、把「取决于什么」如实交给用户，不猜一个原因。
  if (!row.hasRecordedUsage) {
    const hint = t("usage.noUsageYetHint");
    return (
      <Tooltip content={hint} side="left">
        <span
          tabIndex={0}
          role="note"
          aria-label={hint}
          className="cursor-default rounded text-text-muted outline-none focus-visible:ring-1 focus-visible:ring-primary"
        >
          {t("usage.noUsageYet")}
        </span>
      </Tooltip>
    );
  }
  if (row.costNano == null) {
    const hint = unpricedHint(row.unpricedReason, t, pricing);
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
  // 金额与成因同时存在 = 只算出了一部分（按模型估价之后才有的形态）。
  const partial = !!row.unpricedReason;
  const parts = (row.breakdown ?? []).filter((p) => p && isValidUsage(p.usage));
  // tooltip 逐行渲染（Tooltip 不保留 `\n`）：精度说明 → 主要按谁估 → 逐模型拆分 → 部分未计入的成因。
  const lines = [
    t(isEstimate ? "usage.costFamilyHint" : "usage.costExactHint", { multiplier: row.multiplier }),
    ...(row.pricedByModel ? [t("usage.pricedBy", { model: row.pricedByModel })] : []),
    ...(parts.length > 1 || parts.some((p) => !p.model)
      ? [t("usage.breakdownTitle"), ...parts.map((p) => `· ${fmtPart(p, t)}`)]
      : []),
    ...(partial ? [t("usage.partialHint"), unpricedHint(row.unpricedReason, t, pricing)] : []),
  ];
  return (
    <Tooltip content={<div className="space-y-0.5">{lines.map((l, i) => <div key={i}>{l}</div>)}</div>} side="left">
      <span
        tabIndex={0}
        role="note"
        aria-label={lines.join(" ")}
        className={`cursor-default rounded outline-none focus-visible:ring-1 focus-visible:ring-primary ${row.overBudget ? "font-medium text-danger" : ""}`}
      >
        {fmtUsd(row.costNano)}
        {isEstimate && <span className="ml-0.5 text-warning">≈</span>}
        {partial && <span className="ml-0.5 text-warning">{t("usage.partialMark")}</span>}
        {/* 设了预算就并排显示「已花 / $预算」，让进度一眼可见；超了整串标红。 */}
        {row.budgetUsd != null && (
          <span className="ml-0.5 text-text-muted">{` / $${row.budgetUsd}`}</span>
        )}
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
function unpricedHint(reason: UnpricedReason | undefined, t: TFunc, pricing: PricingStatus | null): string {
  switch (reason?.kind) {
    case "aggregate":
      return t("usage.reason.aggregate");
    case "keyDeleted":
      return t("usage.reason.keyDeleted");
    case "modelNotInTable":
      return t("usage.reason.modelNotInTable", { model: reason.model, tables: tablesChecked(pricing, t) });
    case "noModelName":
      return t("usage.reason.noModelName");
    default:
      // 后端没给成因（跨版本：新前端 + 旧后端）→ 退回原来那句泛化提示。
      // 不留这一支的话这里会渲染出空 tooltip，比说得笼统更糟。
      return t("usage.costUnknownHint");
  }
}

/**
 * 「不在单价表里」那句话的括注：查过哪几张表。
 *
 * 🔴 在线表还没拉到时**不能**说「在线表也没有」—— 那张表压根没查过，这么写会让用户以为
 * 等也没用，而它可能下一轮拉到就有了。
 */
function tablesChecked(pricing: PricingStatus | null, t: TFunc): string {
  const date = pricing?.builtinDate ?? "";
  return pricing && pricing.onlineRows > 0
    ? t("usage.reason.tablesBoth", { date, n: pricing.onlineRows })
    : t("usage.reason.tablesBuiltinOnly", { date });
}

/**
 * 表头那一行：内置表核对日期 · 在线表状态。说明与失败原因放在 tooltip 里（可聚焦）——
 * 镜像的报错可能有几百字，挤进表头会把整页往下推。
 */
function PricingLine({ status, t }: { status: PricingStatus; t: TFunc }) {
  const when = status.onlineFetchedAtMs != null ? new Date(status.onlineFetchedAtMs).toLocaleString() : "";
  const online =
    status.onlineRows > 0
      ? t(status.onlineError ? "usage.priceOnlineStale" : "usage.priceOnline", { n: status.onlineRows, time: when })
      : t(status.onlineError ? "usage.priceOnlineFailed" : "usage.priceOnlinePending");
  const text = `${t("usage.priceBuiltin", { date: status.builtinDate })} · ${online}`;
  const hint = [t("usage.priceHint"), ...(status.onlineError ? [t("usage.priceErrorHint", { error: status.onlineError })] : [])];
  return (
    <Tooltip content={<div className="space-y-1">{hint.map((l, i) => <div key={i}>{l}</div>)}</div>} side="bottom">
      <p
        tabIndex={0}
        role="note"
        aria-label={`${text}. ${hint.join(" ")}`}
        className="mt-0.5 cursor-default rounded outline-none focus-visible:ring-1 focus-visible:ring-primary"
      >
        {text}
      </p>
    </Tooltip>
  );
}

/** 单价表现状的形状校验（同 `isValidCostRow`：脏数据不进 state）。 */
function isValidPricingStatus(s: unknown): s is PricingStatus {
  if (!s || typeof s !== "object") return false;
  const o = s as Record<string, unknown>;
  return typeof o.builtinDate === "string" && typeof o.onlineRows === "number" && Number.isFinite(o.onlineRows);
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

/** 一天的花费是否可安全聚合（date 是字符串、三个数都是有限数字）。 */
function isValidDailyCost(d: unknown): d is DailyCost {
  if (!d || typeof d !== "object") return false;
  const o = d as Record<string, unknown>;
  const num = (v: unknown) => typeof v === "number" && Number.isFinite(v);
  return typeof o.date === "string" && num(o.costNano) && num(o.tokens) && num(o.unpricedTokens);
}

/** 「按模型拆分」里的一行：`模型 · 1.2M token · $0.1234`（历史格说明它是按谁估的）。 */
function fmtPart(p: UsagePartCost, t: TFunc): string {
  const u = p.usage;
  const tokens = u.input + u.output + (u.cacheRead ?? 0) + (u.cacheCreation ?? 0);
  const name = p.model
    ? p.model
    : p.pricedAs
      ? t("usage.breakdownLegacy", { model: p.pricedAs })
      : t("usage.breakdownLegacyUnknown");
  const cost = p.costNano != null ? fmtUsd(p.costNano) : t("usage.breakdownUnpriced");
  return `${name} · ${fmt(tokens)} token · ${cost}`;
}

/**
 * 已离开配置的 Key 的「清理」按钮。两步确认，同 Key 卡片的删除：这是本页唯一不可逆的动作。
 * 判据（只给 `keyDeleted` 的行）在调用处；后端也会拒绝清理仍在用的 Key。
 */
function PurgeControl({
  confirming,
  onAsk,
  onCancel,
  onConfirm,
  t,
}: {
  confirming: boolean;
  onAsk: () => void;
  onCancel: () => void;
  onConfirm: () => void;
  t: TFunc;
}) {
  const btn = "shrink-0 rounded px-1 text-[11px] outline-none focus-visible:ring-1 focus-visible:ring-primary";
  if (!confirming) {
    return (
      <Tooltip content={t("usage.purgeHint")} side="top">
        <button type="button" aria-label={t("usage.purgeHint")} className={`${btn} text-text-muted underline hover:text-danger`} onClick={onAsk}>
          {t("usage.purge")}
        </button>
      </Tooltip>
    );
  }
  return (
    <>
      <button type="button" className={`${btn} font-medium text-danger underline`} onClick={onConfirm}>
        {t("usage.purgeConfirm")}
      </button>
      <button type="button" className={`${btn} text-text-muted underline`} onClick={onCancel}>
        {t("common.cancel")}
      </button>
    </>
  );
}
