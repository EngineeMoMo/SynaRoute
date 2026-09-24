import { describe, it, expect } from "vitest";
import { isValidBudget, parseBudget } from "./budget";

// 与后端 usage_cost::budget_status 同口径：正数、有限即合法；空 = 不限。
// 前端也判的理由见 budget.ts 模块头（后端对无效值静默视同「未设」，不拦用户就无从知道填错了）。

describe("isValidBudget", () => {
  it("空串 = 未设，合法（表示不限）", () => {
    expect(isValidBudget("")).toBe(true);
    expect(isValidBudget("   ")).toBe(true);
  });
  it("正的有限数合法", () => {
    expect(isValidBudget("50")).toBe(true);
    expect(isValidBudget("0.5")).toBe(true);
    expect(isValidBudget("12.34")).toBe(true);
    expect(isValidBudget(" 100 ")).toBe(true);
  });
  it("0 / 负数 / 非数字 / 无穷 一律不合法", () => {
    expect(isValidBudget("0")).toBe(false);
    expect(isValidBudget("-5")).toBe(false);
    expect(isValidBudget("abc")).toBe(false);
    // 🔴 Infinity 能过 `> 0`，必须靠 Number.isFinite 挡掉（后端 budget_status 用 is_finite 同口径）。
    expect(isValidBudget("inf")).toBe(false);
    expect(isValidBudget("Infinity")).toBe(false);
    expect(isValidBudget("1e400")).toBe(false);
  });
});

describe("parseBudget", () => {
  it("空 / 非法 → undefined（即不限）", () => {
    expect(parseBudget("")).toBeUndefined();
    expect(parseBudget("0")).toBeUndefined();
    expect(parseBudget("-1")).toBeUndefined();
    expect(parseBudget("abc")).toBeUndefined();
    expect(parseBudget("1e400")).toBeUndefined();
  });
  it("正数 → 该数（落库前归一）", () => {
    expect(parseBudget("50")).toBe(50);
    expect(parseBudget(" 12.5 ")).toBe(12.5);
  });
});
