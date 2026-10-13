// 用量页「今日 / 本周 / 本月 / 近 7 日」的日期口径。
//
// 🔴 一律按**本机时区**切天，与后端 `usage_store::local_date_string` 同口径（2026-10-12 起）。
// 此前这里用 `toISOString().slice(0, 10)`（= UTC 日期），于是东八区的用户每天 0~8 点的用量
// 算在「昨天」，「今日花费」一天里有三分之一的时间是错的。
//
// 「本周」从周一算起、「本月」从 1 号算起（日历口径）。此前两格是「近 7 日」「近 30 日」的滚动窗口，
// 而标签写的是「本周 / 本月」时用户会按日历去对账单 —— 两种口径必须与标签一致。
// 近 7 日趋势图仍是滚动窗口（它要的是趋势，不是对账）。

/** 本机时区的日期键 `YYYY-MM-DD`。 */
export function localDateKey(d: Date): string {
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`;
}

/** 本周一的日期键（周一为一周的第一天）。 */
export function startOfWeekKey(now: Date): string {
  const d = new Date(now.getFullYear(), now.getMonth(), now.getDate());
  const sinceMonday = (d.getDay() + 6) % 7; // getDay(): 周日 = 0
  d.setDate(d.getDate() - sinceMonday);
  return localDateKey(d);
}

/** 本月 1 号的日期键。 */
export function startOfMonthKey(now: Date): string {
  return localDateKey(new Date(now.getFullYear(), now.getMonth(), 1));
}

/**
 * 截至今天（含）的最近 `n` 天，**最早的在前**。
 *
 * 按日历日回退（`setDate(-1)`），不按 `- 86_400_000` 毫秒回退：夏令时切换那天只有 23 或 25 小时，
 * 按毫秒减会跳过或重复一天。
 */
export function lastNDayKeys(now: Date, n: number): string[] {
  const out: string[] = [];
  const d = new Date(now.getFullYear(), now.getMonth(), now.getDate());
  for (let i = 0; i < n; i++) {
    out.unshift(localDateKey(d));
    d.setDate(d.getDate() - 1);
  }
  return out;
}

/** 日期键是否落在 `[fromKey, todayKey]` 区间内（`YYYY-MM-DD` 的字典序就是时间序）。 */
export function inRange(key: string, fromKey: string, todayKey: string): boolean {
  return key >= fromKey && key <= todayKey;
}
