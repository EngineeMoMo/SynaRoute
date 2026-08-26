import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { MAX_COST_MULTIPLIER } from "../src/lib/costMultiplier";

/**
 * 计费倍率上界的**跨语言不变量**：前端必须与 Rust `pricing::MAX_COST_MULTIPLIER` 同值。
 *
 * 分叉的两个方向都是静默的：
 * - 前端更宽 → 用户填的值被后端悄悄按 1.0 算，界面却显示着他填的数（他以为生效了）；
 * - 前端更严 → 凭空少掉一个后端本来支持的能力。
 *
 * 编译器管不到这条缝。放在 `tests/` 而非 `src/`：要用 node:fs 读 Rust 源码，
 * 而应用侧的 tsconfig 不含 node 类型（`npm run build` 会报 TS2307）——
 * 同 mcpEndpointParity / vendorSeedParity 的位置理由。
 */
describe("计费倍率上界：前后端不得分叉", () => {
  const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
  const RS = readFileSync(join(ROOT, "src-tauri", "src", "pricing", "mod.rs"), "utf8");

  it("与 Rust 侧同值", () => {
    const m = RS.match(/pub const MAX_COST_MULTIPLIER: f64 = ([\d_.]+)/);
    expect(m, "Rust 侧的 MAX_COST_MULTIPLIER 找不到了 —— 这条判据在空转").not.toBeNull();
    expect(Number(m![1].replace(/_/g, ""))).toBe(MAX_COST_MULTIPLIER);
  });
});
