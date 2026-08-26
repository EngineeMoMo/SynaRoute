import { describe, it, expect } from "vitest";
import { MAX_COST_MULTIPLIER, isValidCostMultiplier } from "./costMultiplier";

/**
 * 计费倍率校验。这些规则此前埋在 `KeyEditor` 的一个模块级函数里 ——
 * 唯一能验证它的办法是渲染整个抽屉，于是实际上没有测试。
 */
describe("isValidCostMultiplier", () => {
  it("空串 = 未填，合法（用官方原价）", () => {
    expect(isValidCostMultiplier("")).toBe(true);
    expect(isValidCostMultiplier("   ")).toBe(true);
  });

  it("正常折扣区间放行", () => {
    for (const v of ["0.3", "0.5", "1", "1.0", "2", "5", "0.001"]) {
      expect(isValidCostMultiplier(v), v).toBe(true);
    }
  });

  it("非数字、零、负数一律拦下", () => {
    // 用户真会填这些（「三折」「30%」「0.3折」），而后端对非法值静默退回 1.0 ——
    // 前端不拦，界面上就什么都不说，用量页按原价算而他以为打了三折。
    for (const v of ["abc", "三折", "30%", "0.3折", "0", "-1", "-0.5"]) {
      expect(isValidCostMultiplier(v), v).toBe(false);
    }
  });

  it("Infinity 的各种写法必须挡掉", () => {
    // `Number("inf")` 是 NaN，但 `Number("Infinity")` / `Number("1e400")` 都是 Infinity，
    // 而 `Infinity > 0` 为真 —— 只判 `> 0` 会让它穿过去。
    // 后端踩过同一个坑：`f64::INFINITY` 过了那道门，把金额撑成 $18446744073。
    for (const v of ["Infinity", "-Infinity", "1e400", "NaN", "inf"]) {
      expect(isValidCostMultiplier(v), v).toBe(false);
    }
    expect(Number("1e400")).toBe(Infinity); // 锚住上面那句话的前提
  });

  it("上界用 <=（边界值本身合法）", () => {
    expect(isValidCostMultiplier(String(MAX_COST_MULTIPLIER))).toBe(true);
    expect(isValidCostMultiplier(String(MAX_COST_MULTIPLIER + 1))).toBe(false);
  });
});
