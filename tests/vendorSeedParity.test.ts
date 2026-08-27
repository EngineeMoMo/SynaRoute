import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { MOCK_VENDORS } from "../src/lib/mockData.vendors";

/**
 * 内置厂商清单的**跨语言不变量**。
 *
 * 事实来源是 Rust 的 `vendors.rs::builtin_seed()`。前端另有一份
 * `src/lib/mockData.vendors.ts`，只在**没有 Tauri 后端**时用到（`npm run dev` 直接开浏览器、
 * 官网截图、演示站）。
 *
 * 两份分叉过一次，而代价不是「开发时少看到几条」这么轻：官网
 * `site/public/screenshots/vendors-*.png` 就是从那个页面截的 —— 于是官网展示 6 条内置厂商、
 * 产品里其实有 33 条，**对外宣称与产品不一致**，且没有任何东西会报错。
 *
 * # 判据的边界（必须写清，否则下一个人会以为全同步）
 *
 * 只比 **id 集合（含顺序）** 与 **base_url**。
 * **不比预设模型清单** —— 模型名变动频繁，纳入判据会让「Rust 侧改一个模型名」连带要改
 * 演示数据，而演示数据的精度要求本就不同。一个部分为真的判据，边界必须显式。
 *
 * 顺序也纳入：`builtin_seed()` 的注释写明「顺序即界面顺序」（国际原厂 → 国内原厂 →
 * 聚合/中转 → 本机推理 → 自定义），演示模式若顺序不同，截出来的图与产品也不是一回事。
 */

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const VENDORS_RS = readFileSync(join(ROOT, "src-tauri", "src", "vendors.rs"), "utf8");

/** Rust 侧的 `Protocol` 变体 → IPC 上的 serde 名（`#[serde(rename_all="snake_case")]`）。 */
const PROTO: Record<string, string> = {
  Anthropic: "anthropic",
  OpenaiChat: "openai_chat",
  OpenaiResponses: "openai_responses",
};

/**
 * 从 `vendors.rs` 的生产段里抠出每个 `mk(...)` 调用的 (id, base_url, protocol)。
 *
 * 用括号配对切实参段，而不是写一个跨行的大正则：那种正则在「某条厂商多了一层 `vec![]` 嵌套」
 * 时会静默漏掉几条 —— 实测第一版就漏了 9 条（deepseek / openrouter / ollama 等），
 * 而漏掉的表现是判据**变松**（少比几条），正是最坏的失效方向。
 */
function parseRustSeed(): { id: string; url: string; proto: string }[] {
  const cut = VENDORS_RS.indexOf("#[cfg(test)]");
  const prod = cut < 0 ? VENDORS_RS : VENDORS_RS.slice(0, cut);
  const out: { id: string; url: string; proto: string }[] = [];
  for (let i = 0; ; ) {
    const k = prod.indexOf("mk(", i);
    if (k < 0) break;
    i = k + 3;
    // 跳过 `fn mk(` 定义本身
    if (/\bfn\s+$/.test(prod.slice(Math.max(0, k - 6), k))) continue;
    let depth = 1;
    let j = k + 3;
    for (; j < prod.length && depth > 0; j++) {
      if (prod[j] === "(") depth++;
      else if (prod[j] === ")") depth--;
    }
    const body = prod.slice(k + 3, j - 1);
    i = j;
    const strs = [...body.matchAll(/"((?:[^"\\]|\\.)*)"/g)].map((m) => m[1]);
    const proto = body.match(/,\s*(Anthropic|OpenaiChat|OpenaiResponses)\s*,/);
    if (strs.length < 3 || !proto) continue;
    out.push({ id: strs[0], url: strs[2], proto: PROTO[proto[1]] });
  }
  // 去重保序（同一个 id 只应出现一次；重复即 Rust 侧写错了，由下面的用例报出来）
  return out;
}

describe("内置厂商清单：Rust 与演示数据不得分叉", () => {
  const rust = parseRustSeed();

  it("解析器本身没空转", () => {
    // 反向判据：抠不出足够多条就说明解析坏了，别让一个恒绿的门悄悄放行
    // （同 `invoke-command-must-exist` 那条教训）。
    expect(rust.length).toBeGreaterThan(25);
    expect(rust.map((v) => v.id)).toContain("anthropic");
    expect(rust.map((v) => v.id)).toContain("custom");
  });

  it("Rust 侧的 id 没有重复", () => {
    const dup = rust.map((v) => v.id).filter((id, i, a) => a.indexOf(id) !== i);
    expect(dup, `vendors.rs 里有重复的厂商 id：${dup.join("、")}`).toEqual([]);
  });

  it("id 集合与顺序完全一致", () => {
    expect(MOCK_VENDORS.map((v) => v.id)).toEqual(rust.map((v) => v.id));
  });

  it("每条 base_url 与协议一致", () => {
    const mismatched: string[] = [];
    for (const r of rust) {
      const m = MOCK_VENDORS.find((v) => v.id === r.id);
      if (!m) continue; // 上一条用例已覆盖「缺条目」
      if (m.defaultBaseUrl !== r.url) {
        mismatched.push(`${r.id}: base_url mock=${m.defaultBaseUrl} rust=${r.url}`);
      }
      if (m.defaultProtocol !== r.proto) {
        mismatched.push(`${r.id}: protocol mock=${m.defaultProtocol} rust=${r.proto}`);
      }
    }
    expect(mismatched, mismatched.join("\n")).toEqual([]);
  });

  it("`custom` 排在最后（它是「以上都不是」的兜底项）", () => {
    expect(rust.at(-1)?.id).toBe("custom");
    expect(MOCK_VENDORS.at(-1)?.id).toBe("custom");
  });
});
