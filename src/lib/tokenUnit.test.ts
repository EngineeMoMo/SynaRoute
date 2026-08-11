import { describe, it, expect } from "vitest";
import {
  type TokenUnit,
  MAX_CONTEXT_TOKENS,
  tokensFromAmount,
  preferredUnit,
  amountForUnit,
} from "./tokenUnit";

// 审计 A2-04：上下文窗口「数值 + 单位」换算此前只有人工向量，无自动化测试。
// 这套用例把那些人工向量固化，并覆盖二进制浮点会踩坑的边界。

describe("tokensFromAmount", () => {
  it("基本换算：整数 × 单位", () => {
    expect(tokensFromAmount("200", "K")).toBe(200_000);
    expect(tokensFromAmount("1", "M")).toBe(1_000_000);
    expect(tokensFromAmount("4096", "token")).toBe(4096);
  });

  it("小数在精度内：按十进制补零，不碰浮点", () => {
    // 1.000001M = 1,000,001 —— 正是 Number 乘法会算成 1000000.9999999999 的经典坑
    expect(tokensFromAmount("1.000001", "M")).toBe(1_000_001);
    expect(tokensFromAmount("200.5", "K")).toBe(200_500);
    expect(tokensFromAmount("0.128", "M")).toBe(128_000);
  });

  it("尾随零不影响结果（fraction 先 strip）", () => {
    expect(tokensFromAmount("1.500000", "M")).toBe(1_500_000);
    expect(tokensFromAmount("200.000", "K")).toBe(200_000);
  });

  it("小数位超过该单位精度 → 拒绝（宁可拒也不四舍五入成半个 token）", () => {
    expect(tokensFromAmount("1.0000001", "M")).toBeNull(); // M 只允许 6 位小数
    expect(tokensFromAmount("200.0001", "K")).toBeNull(); // K 只允许 3 位小数
    expect(tokensFromAmount("100.5", "token")).toBeNull(); // token 不允许小数
  });

  it("格式非法 → null（负号 / 多点 / 字母 / 空 / 前后缀）", () => {
    for (const bad of ["", " ", "-1", "1.2.3", "1e6", "abc", "1k", "0x10", "+5", ".5", "5."]) {
      expect(tokensFromAmount(bad, "K"), `"${bad}" 应判非法`).toBeNull();
    }
  });

  it("零与负数 → null（上下文窗口必须为正）", () => {
    expect(tokensFromAmount("0", "K")).toBeNull();
    expect(tokensFromAmount("0.0", "M")).toBeNull();
    expect(tokensFromAmount("0", "token")).toBeNull();
  });

  it("超过 u32 上限 → null（不能等 IPC 反序列化才失败）", () => {
    expect(tokensFromAmount(String(MAX_CONTEXT_TOKENS), "token")).toBe(MAX_CONTEXT_TOKENS);
    expect(tokensFromAmount(String(MAX_CONTEXT_TOKENS + 1), "token")).toBeNull();
    expect(tokensFromAmount("5", "M")).toBe(5_000_000); // 上限内的大值仍可
    expect(tokensFromAmount("4295", "M")).toBeNull(); // 4295M ≈ 4.295e9 > u32
  });

  it("前后空白允许（trim）", () => {
    expect(tokensFromAmount("  200  ", "K")).toBe(200_000);
  });
});

describe("preferredUnit", () => {
  it("按数量级选最大单位", () => {
    expect(preferredUnit(undefined)).toBe("K"); // 空值默认 K
    expect(preferredUnit(500)).toBe("token");
    expect(preferredUnit(999)).toBe("token");
    expect(preferredUnit(1_000)).toBe("K");
    expect(preferredUnit(200_000)).toBe("K");
    expect(preferredUnit(999_999)).toBe("K");
    expect(preferredUnit(1_000_000)).toBe("M");
  });
});

describe("amountForUnit", () => {
  it("整除时不带小数点", () => {
    expect(amountForUnit(200_000, "K")).toBe("200");
    expect(amountForUnit(1_000_000, "M")).toBe("1");
    expect(amountForUnit(4096, "token")).toBe("4096");
  });

  it("非整除时补零 + strip 尾零", () => {
    expect(amountForUnit(1_000_001, "M")).toBe("1.000001");
    expect(amountForUnit(200_500, "K")).toBe("200.5");
    expect(amountForUnit(1_500_000, "M")).toBe("1.5");
  });

  it("undefined → 空串", () => {
    expect(amountForUnit(undefined, "K")).toBe("");
  });
});

describe("往返严格无损（这是抽出来最想钉住的不变量）", () => {
  const units: TokenUnit[] = ["token", "K", "M"];
  const samples = [
    1, 999, 1_000, 4_096, 128_000, 200_000, 200_500, 1_000_000, 1_000_001,
    1_500_000, 200_000_000, MAX_CONTEXT_TOKENS,
  ];

  it("tokensFromAmount(amountForUnit(t, u), u) === t，对每个单位", () => {
    for (const t of samples) {
      for (const u of units) {
        const shown = amountForUnit(t, u);
        // 该单位下能无损表示（小数位不超精度）才要求往返；否则 UI 本就不会用该单位显示它。
        const back = tokensFromAmount(shown, u);
        if (back !== null) {
          expect(back, `t=${t} u=${u} shown="${shown}"`).toBe(t);
        }
      }
    }
  });

  it("preferredUnit 选出的单位一定能无损往返", () => {
    for (const t of samples) {
      const u = preferredUnit(t);
      const shown = amountForUnit(t, u);
      expect(tokensFromAmount(shown, u), `t=${t} 经 preferredUnit=${u} 往返`).toBe(t);
    }
  });
});
