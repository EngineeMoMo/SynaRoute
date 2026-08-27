import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

/**
 * 「算不出花费的原因」的**跨语言不变量**。
 *
 * 后端 `usage_cost::UnpricedReason` 用 `#[serde(rename_all = "camelCase", tag = "kind")]`
 * 序列化，前端 `types.ts` 的 `UnpricedReason` 是与之对应的判别式联合，
 * `UsagePage.tsx` 按 `reason.kind` 分派文案。**三处都要同一套变体名。**
 *
 * 分叉的表现是静默的：
 * - 后端加一个变体、前端没加 → `switch` 落到 default，用户看到那句最泛化的旧提示
 *   （对新成因几乎必然是假话），而没有任何报错；
 * - 前端少一条 i18n 文案 → tooltip 渲染出原始 key。
 *
 * 编译器管不到跨语言这条缝，故用机械判据钉住。判据取自 **Rust 源码里的变体名本身**，
 * 不是「我记得有四个」—— 改名或增删时这条必须变红。
 *
 * 放在 `tests/` 而非 `src/`：要用 node:fs 读 Rust 源码（src 侧 tsconfig 是浏览器目标）。
 */

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const USAGE_COST_RS = readFileSync(join(ROOT, "src-tauri", "src", "usage_cost.rs"), "utf8");
const TYPES_TS = readFileSync(join(ROOT, "src", "types.ts"), "utf8");
const USAGE_PAGE = readFileSync(join(ROOT, "src", "pages", "UsagePage.tsx"), "utf8");
const I18N_USAGE = readFileSync(join(ROOT, "src", "lib", "i18n.usage.ts"), "utf8");

/** Rust 侧 `enum UnpricedReason { .. }` 的变体名，转成 serde 的 camelCase 形态。 */
function rustVariants(): string[] {
  const start = USAGE_COST_RS.indexOf("pub(crate) enum UnpricedReason {");
  expect(start, "usage_cost.rs 里应有 `enum UnpricedReason`").toBeGreaterThan(0);
  const body = USAGE_COST_RS.slice(start, USAGE_COST_RS.indexOf("\n}", start));
  // 变体行：缩进 4 格的大写开头标识符，后跟 `,` 或 ` {`
  const names = [...body.matchAll(/^ {4}([A-Z][A-Za-z0-9]*)\s*(?:,|\{)/gm)].map((m) => m[1]);
  return names.map((n) => n[0].toLowerCase() + n.slice(1));
}

/** 前端 `types.ts` 的 `UnpricedReason` 联合里出现的 `kind: "x"`。 */
function tsVariants(): string[] {
  const start = TYPES_TS.indexOf("export type UnpricedReason");
  expect(start, "types.ts 里应有 `UnpricedReason`").toBeGreaterThan(0);
  const body = TYPES_TS.slice(start, TYPES_TS.indexOf("\n\n", start));
  return [...body.matchAll(/kind:\s*"([a-zA-Z0-9]+)"/g)].map((m) => m[1]);
}

describe("UnpricedReason 跨语言判据", () => {
  it("Rust 变体与 TS 联合成员完全一致", () => {
    const rust = rustVariants();
    const ts = tsVariants();
    // 反向判据：解析到 0 个就说明上面那套正则坏了，别让一个空转的门悄悄通过
    // （同 CLAUDE.md 里 `invoke-command-must-exist` 那条教训）。
    expect(rust.length, "Rust 侧应解析出变体").toBeGreaterThan(2);
    expect([...rust].sort()).toEqual([...ts].sort());
  });

  it("每个变体在 UsagePage 里都有分派，且有对应的 zh/en 文案", () => {
    for (const v of rustVariants()) {
      expect(
        USAGE_PAGE.includes(`case "${v}"`),
        `UsagePage 的 unpricedHint 没有处理 \`${v}\` —— 会落到 default，显示一句对它不成立的泛化提示`,
      ).toBe(true);
      // 文案 key 命名约定：usage.reason.<variant>
      const key = `"usage.reason.${v}"`;
      const hits = I18N_USAGE.split(key).length - 1;
      expect(hits, `i18n.usage.ts 里 ${key} 应各有 zh/en 两条，实测 ${hits} 条`).toBe(2);
    }
  });

  it("横幅的分组计数覆盖全部变体", () => {
    for (const v of rustVariants()) {
      expect(
        USAGE_PAGE.includes(`unpricedGroups.${v}`),
        `横幅没有 \`${v}\` 这一组 —— 那几行会被算进总数却不出现在任何一条说明里`,
      ).toBe(true);
      const key = `"usage.unpricedGroup.${v}"`;
      expect(I18N_USAGE.split(key).length - 1, `${key} 应各有 zh/en 两条`).toBe(2);
    }
  });
});
