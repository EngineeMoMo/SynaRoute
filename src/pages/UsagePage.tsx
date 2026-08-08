import { useEffect, useMemo, useState } from "react";
import { api } from "@/lib/bridge";
import { useT } from "@/lib/useT";
import type { TokenUsageByKey } from "@/types";
import { Badge } from "@/components/ui/Badge";

/**
 * 用量统计面板：按「分类 × Key」展示 token 消耗。
 *
 * 数据来自后端 `get_token_usage`，读的是 Store 里一份**独立累加器**，口径是「跨重启累计」。
 * 刻意不从事件环（最多 500 条）算总量：那样在环滚动后总量会停止增长（见
 * `token_usage_totals_never_shrink_when_event_ring_rotates`）。
 *
 * 累加器每分钟落盘一次（`usage.json`，仅在有新消耗时写），启动时读回 —— 所以标题下写的是
 * `usage.since` 的**起算时刻**而不是「本次启动」。这个区别不是措辞问题：若面板说「本次启动」
 * 而数字其实是跨重启累计的，用户会以为统计翻倍了。
 *
 * 仍**不含跨天/按日历史**：定位是「累计消耗」不是「账单」，逐日曲线属后续版本。
 *
 * 只统计 token、不换算钱：各中转商单价差异巨大，SynaRoute 无从得知；想算钱的用户按
 * token×单价自乘即可。
 */
export function UsagePage() {
  const t = useT();
  const [rows, setRows] = useState<TokenUsageByKey[] | null>(null);
  const [sinceMs, setSinceMs] = useState<number | null>(null);
  const [error, setError] = useState<string | null>(null);

  const load = async () => {
    try {
      // 两个请求并发发，避免起算时刻慢一拍导致标题闪一下旧值。
      const [next, since] = await Promise.all([api.getTokenUsage(), api.getUsageSince()]);
      setRows(next);
      setSinceMs(since);
      setError(null);
    } catch (e) {
      setError(String(e));
    }
  };

  useEffect(() => {
    void load();
    // 用量随事件变化，但面板是低频查看——进页面拉一次即可，不常驻轮询（日志页 2s 已是最密的轮询）。
    const id = window.setInterval(() => void load(), 30_000);
    return () => window.clearInterval(id);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const summary = useMemo(() => {
    let input = 0, output = 0, cacheRead = 0;
    for (const r of rows ?? []) {
      input += r.usage.input;
      output += r.usage.output;
      cacheRead += r.usage.cacheRead ?? 0;
    }
    return { input, output, cacheRead };
  }, [rows]);

  return (
    <div className="flex h-full flex-col">
      <div className="border-b border-border px-6 py-4">
        <div className="flex items-start justify-between gap-4">
          <div>
            <h1 className="text-lg font-semibold text-text-primary">{t("usage.title")}</h1>
            <p className="mt-1 text-xs text-text-muted">{t("usage.subtitle")}</p>
            {sinceMs !== null && (
              <p className="mt-0.5 text-xs text-text-muted" title={t("usage.sinceHint")}>
                {t("usage.since")}: {new Date(sinceMs).toLocaleString()}
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

        {/* 总计卡片 */}
        {rows && rows.length > 0 && (
          <div className="mt-3 grid grid-cols-3 gap-2">
            <div className="rounded-control border border-border bg-surface p-3">
              <div className="text-[11px] text-text-muted">{t("usage.input")}</div>
              <div className="mt-0.5 font-mono text-lg tabular-nums text-text-primary">
                {fmt(summary.input)}
              </div>
            </div>
            <div className="rounded-control border border-border bg-surface p-3">
              <div className="text-[11px] text-text-muted">{t("usage.output")}</div>
              <div className="mt-0.5 font-mono text-lg tabular-nums text-text-primary">
                {fmt(summary.output)}
              </div>
            </div>
            <div className="rounded-control border border-border bg-surface p-3">
              <div className="text-[11px] text-text-muted">{t("usage.cacheRead")}</div>
              <div className="mt-0.5 font-mono text-lg tabular-nums text-text-primary">
                {fmt(summary.cacheRead)}
              </div>
            </div>
          </div>
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
                </tr>
              </thead>
              <tbody className="divide-y divide-border">
                {rows.map((r) => (
                  <tr key={`${r.categoryId}/${r.keyId}`} className="hover:bg-surface-hover/40">
                    <td className="px-4 py-2">
                      <Badge variant="neutral">{t(`nav.${r.categoryId}`)}</Badge>
                    </td>
                    <td className="max-w-[200px] truncate px-4 py-2 font-mono text-xs text-text-secondary">
                      {r.keyId || t("usage.systemLevel")}
                    </td>
                    <td className="px-4 py-2 text-right font-mono tabular-nums">{fmt(r.usage.input)}</td>
                    <td className="px-4 py-2 text-right font-mono tabular-nums">{fmt(r.usage.output)}</td>
                    <td className="px-4 py-2 text-right font-mono tabular-nums">
                      {fmt(r.usage.cacheRead ?? 0)}
                    </td>
                    <td className="px-4 py-2 text-right font-mono tabular-nums font-medium">
                      {fmt((r.usage.cacheRead ?? 0) + r.usage.input + r.usage.output)}
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

/** 紧凑 token 数字：≥10000 用 k，≥1000000 用 M（与日志页 fmtTokens 同口径）。 */
function fmt(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(2)}M`;
  if (n >= 10_000) return `${(n / 1000).toFixed(1)}k`;
  return String(n);
}
