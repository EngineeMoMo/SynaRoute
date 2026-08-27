import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { resolveBrand } from "../src/components/brandIcons";

/**
 * **每个内置厂商 id 都必须解析回自己的品牌** —— 用例从 Rust 侧的厂商清单**自动生成**。
 *
 * # 这条判据补的是一个用户直接看得见的错
 *
 * `resolveBrand` 取**最长**关键词命中。内置厂商 `icon: None`，界面走的正是这条启发式。
 * 于是 `azure-openai` 这个 id 会命中 `openai`（6 字符）而不是 `azure`（5）——
 * 「Azure OpenAI」那一行渲染成 **OpenAI 的绿色花瓣 logo**。
 *
 * # 为什么必须「生成」而不是手写列表
 *
 * `brandIcons.test.ts` 里本就有一组手写的冲突用例，但它**没有** `azure-openai` 这一项 ——
 * 漏掉它的原因恰恰是「靠人往列表里补」。而 Rust 侧那条
 * `every_vendor_id_appears_in_the_frontend_brand_keywords` 也盯不住：
 * 它只问「有没有某个关键词是这个 id 的子串」，`azure-openai` 含 `azure` → 通过，
 * 它看不出**哪个关键词赢**。
 *
 * 从 id 集合生成之后，新增厂商时若 id 与别家关键词冲突，这条会立刻变红。
 *
 * 放在 `tests/` 而非 `src/`：要用 node:fs 读 Rust 源码，而应用侧 tsconfig 不含 node 类型
 * （放 src 会让 `npm run build` 报 TS2307）—— 同 mcpEndpointParity / vendorSeedParity /
 * costMultiplierParity 的位置理由。
 */

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const VENDORS_RS = readFileSync(join(ROOT, "src-tauri", "src", "vendors.rs"), "utf8");

/** 从 `vendors.rs` 生产段里抠出每个 `mk(...)` 的第一个字符串实参 = 厂商 id。 */
function builtinVendorIds(): string[] {
  const cut = VENDORS_RS.indexOf("#[cfg(test)]");
  const prod = cut < 0 ? VENDORS_RS : VENDORS_RS.slice(0, cut);
  const ids: string[] = [];
  for (let i = 0; ; ) {
    const k = prod.indexOf("mk(", i);
    if (k < 0) break;
    i = k + 3;
    // 跳过 `fn mk(` 定义本身
    if (/\bfn\s+$/.test(prod.slice(Math.max(0, k - 6), k))) continue;
    const q = prod.indexOf('"', k);
    const e = prod.indexOf('"', q + 1);
    if (q < 0 || e < 0) continue;
    ids.push(prod.slice(q + 1, e));
  }
  return ids;
}

/**
 * id 与品牌 key 不同名的那几个（其余一律同名）。
 *
 * 这张表本身也是判据的一部分：往 `vendors.rs` 加一个 id 与品牌 key 不同名的厂商时，
 * 忘了在这里登记就会变红 —— 那正是提醒「去确认它解析对了没有」的时机。
 */
const ID_TO_BRAND: Record<string, string> = {
  "meta-llama": "meta",
  "azure-openai": "azure",
  ernie: "baidu",
};

/** `custom` 是「以上都不是」的兜底项，没有品牌。 */
const NO_BRAND = new Set(["custom"]);

describe("内置厂商 id 不得解析到别家品牌", () => {
  const ids = builtinVendorIds();

  it("解析器没空转", () => {
    // 反向判据：抠不出足够多 id 就说明上面的解析坏了，别让一个恒绿的门悄悄放行。
    expect(ids.length).toBeGreaterThan(25);
    expect(ids).toContain("azure-openai");
    expect(ids).toContain("custom");
  });

  it("每个 id 都解析回自己", () => {
    const wrong: string[] = [];
    for (const id of ids) {
      if (NO_BRAND.has(id)) continue;
      const want = ID_TO_BRAND[id] ?? id;
      const got = resolveBrand(id)?.key;
      if (got !== want) wrong.push(`${id} → ${got ?? "(none)"}，应为 ${want}`);
    }
    expect(wrong, "这些内置厂商 id 解析到了别家品牌 —— 界面上那一行会显示错的 logo").toEqual([]);
  });

  it("厂商的**显示名**也不得解析错（界面有些地方用名字当 hint）", () => {
    // `KeyEditor` / `VendorPage` 传的 hint 有时是 `vendor.id`、有时是 `vendor.name`，
    // 两条路都得对。Azure 那条的显示名是「Azure OpenAI（需填自己的资源名）」，
    // 同样会命中 `openai` 而不是 `azure`。
    expect(resolveBrand("Azure OpenAI（需填自己的资源名）")?.key).toBe("azure");
  });
});
