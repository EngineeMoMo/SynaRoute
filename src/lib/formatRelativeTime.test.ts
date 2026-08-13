import { describe, it, expect } from "vitest";
import { formatRelativeTime } from "./utils";
import { translate } from "./i18n";

/**
 * 这组测试钉住的是一类**静默失效**：`formatRelativeTime` 曾直接返回硬编码中文串
 * （`${min} 分钟前`），而调用方还把结果套进 `t()` —— 外层翻译了、内层没翻，
 * 于是英文界面显示「Checked 3 分钟前」。tsc 抓不到（类型都是 string），
 * 只有真的切到英文才看得见。
 *
 * 判据刻意**不**写成「返回值等于某个中文串」—— 那样等于把缺陷钉死。
 * 而是校验「同一时刻在两种语言下必须给出不同的文案」，
 * 以及「结果里不含另一种语言的残留」。
 */

const zh = (k: string, v?: Record<string, string | number>) => translate("zh", k, v);
const en = (k: string, v?: Record<string, string | number>) => translate("en", k, v);

// 固定基准时刻，避免测试跨越分钟边界时抖动
const NOW = Date.now();

describe("formatRelativeTime", () => {
  it("各档位都取到了 i18n 词条，而不是原样吐 key", () => {
    const cases = [
      NOW - 5_000, // 秒
      NOW - 5 * 60_000, // 分钟
      NOW - 5 * 3600_000, // 小时
      NOW - 5 * 86400_000, // 天
    ];
    for (const ts of cases) {
      const out = zh("x", {}) === "x" ? formatRelativeTime(ts, zh) : formatRelativeTime(ts, zh);
      // 取词失败时 translate 会把 key 原样返回，于是结果里会残留 "time."
      expect(out).not.toContain("time.");
      expect(out).toMatch(/\d/); // 必须带上数字
    }
  });

  it("空值走 time.never，且两种语言不同", () => {
    expect(formatRelativeTime(null, zh)).toBe("从未");
    expect(formatRelativeTime(undefined, en)).toBe("never");
    expect(formatRelativeTime(0, zh)).toBe("从未"); // 0 也当「从未」（epoch 0 不是有效时刻）
  });

  it("**核心判据**：同一时刻在中英文下必须给出不同文案", () => {
    // 这一条就是防复发的锁：把 t 参数去掉、或在函数里写回中文硬编码，它必红。
    const cases = [NOW - 5_000, NOW - 5 * 60_000, NOW - 5 * 3600_000, NOW - 5 * 86400_000];
    for (const ts of cases) {
      expect(formatRelativeTime(ts, zh)).not.toBe(formatRelativeTime(ts, en));
    }
  });

  it("英文结果里不含中文残留，中文结果里不含英文单位", () => {
    const ts = NOW - 7 * 60_000;
    expect(formatRelativeTime(ts, en)).not.toMatch(/[一-龥]/);
    expect(formatRelativeTime(ts, zh)).toMatch(/[一-龥]/);
  });

  it("档位边界选对了单位（59s 走秒、60s 走分钟、24h 走天）", () => {
    expect(formatRelativeTime(NOW - 59_000, en)).toContain("s ago");
    expect(formatRelativeTime(NOW - 60_000, en)).toContain("min");
    expect(formatRelativeTime(NOW - 59 * 60_000, en)).toContain("min");
    expect(formatRelativeTime(NOW - 60 * 60_000, en)).toContain("h ago");
    expect(formatRelativeTime(NOW - 23 * 3600_000, en)).toContain("h ago");
    expect(formatRelativeTime(NOW - 24 * 3600_000, en)).toContain("d ago");
  });

  it("数量被真的代入，而不是留着 {n} 占位符", () => {
    expect(formatRelativeTime(NOW - 42 * 60_000, en)).toBe("42 min ago");
    expect(formatRelativeTime(NOW - 42 * 60_000, zh)).toBe("42 分钟前");
    expect(formatRelativeTime(NOW - 3 * 86400_000, zh)).not.toContain("{n}");
  });
});
