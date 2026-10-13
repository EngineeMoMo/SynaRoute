import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { inRange, lastNDayKeys, localDateKey, startOfMonthKey, startOfWeekKey } from "@/lib/usagePeriods";

/**
 * 用量页的日期口径。全部用**本地时间构造的** `Date`（`new Date(y, m, d, h)`），
 * 所以断言与跑测试的机器在哪个时区无关 —— 而那正是这些函数要保证的性质。
 */
describe("usagePeriods", () => {
  it("日期键按本机时区取，且补零", () => {
    expect(localDateKey(new Date(2026, 0, 5, 0, 30))).toBe("2026-01-05");
    // 本地 23:59 仍是当天 —— 按 UTC 取的话东八区这一刻早就是「明天」（反之亦然）。
    expect(localDateKey(new Date(2026, 9, 12, 23, 59))).toBe("2026-10-12");
  });

  it("本周从周一算起，周日属于上一个周一", () => {
    expect(startOfWeekKey(new Date(2026, 9, 12, 9))).toBe("2026-10-12"); // 周一
    expect(startOfWeekKey(new Date(2026, 9, 15, 9))).toBe("2026-10-12"); // 周四
    expect(startOfWeekKey(new Date(2026, 9, 18, 23))).toBe("2026-10-12"); // 周日
    expect(startOfWeekKey(new Date(2026, 10, 1, 9))).toBe("2026-10-26"); // 跨月的那一周
  });

  it("本月从 1 号算起", () => {
    expect(startOfMonthKey(new Date(2026, 9, 12))).toBe("2026-10-01");
    expect(startOfMonthKey(new Date(2026, 0, 31))).toBe("2026-01-01");
  });

  it("最近 n 天：最早在前、含今天、跨月跨年不跳天", () => {
    expect(lastNDayKeys(new Date(2026, 0, 2, 10), 4)).toEqual(["2025-12-30", "2025-12-31", "2026-01-01", "2026-01-02"]);
    expect(lastNDayKeys(new Date(2026, 2, 1), 2)).toEqual(["2026-02-28", "2026-03-01"]);
  });

  it("区间判断包含两端", () => {
    expect(inRange("2026-10-12", "2026-10-12", "2026-10-18")).toBe(true);
    expect(inRange("2026-10-18", "2026-10-12", "2026-10-18")).toBe(true);
    expect(inRange("2026-10-11", "2026-10-12", "2026-10-18")).toBe(false);
  });

  it("🔴 用量页不许再用 UTC 日期（toISOString）切天", () => {
    // 行为用例测不到页面怎么取「今天」（本仓无 jsdom）。v3 的老缺陷就是页面里的
    // `toISOString().slice(0, 10)`：东八区每天 0~8 点的用量算进「昨天」。
    const root = join(dirname(fileURLToPath(import.meta.url)), "..");
    const page = readFileSync(join(root, "src", "pages", "UsagePage.tsx"), "utf8")
      .replace(/\/\*[\s\S]*?\*\//g, "")
      .split(/\r?\n/)
      .filter((l) => !/^\s*\/\//.test(l))
      .join("\n");
    expect(page, "用量页不许用 toISOString 取日期").not.toMatch(/toISOString\(\)/);
    expect(page, "要用 usagePeriods 的本地日期口径").toMatch(/from "@\/lib\/usagePeriods"/);
  });
});
