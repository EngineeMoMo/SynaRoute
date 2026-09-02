import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { inspectableLines } from "../scripts/lib/rust-source.mjs";

/**
 * `ModelMapping` 的**每一行映射**在前端也必须字段齐全 —— 漏一个字段就是
 * 「保存/一键修法之后，那一格自己空了」。
 *
 * # 为什么 providerKeyDraftParity 管不到这里
 *
 * 那条判据从 `buildDraftKey()` 的**顶层**键抽字段，而映射是 `mappings: ModelMapping[]`
 * 里面的东西 —— 它只能看到 `mappings` 这一个键在不在，看不到每一行长什么样。
 * 而这条缝里的失效形态与 `allowInAggregate` 那次一模一样：
 *
 * - `displayName`（菜单显示名）不在某个构造点里 → 用户填好 → 点一次「一键加映射」
 *   （它整份重建 mappings）→ 那一列全空，而用户确信自己填过。方向上永不自愈。
 * - 反向：前端写了个 Rust 不认识的键（拼错 `dispalyName`）→ **serde 默认忽略未知字段**、
 *   不报错 → 那个设置永远存不进去，界面上却看起来填成功了。
 *
 * # 边界（写明，免得被当成更强的判据）
 *
 * 只查 `ModelMappingSection.tsx` 里的对象字面量构造点。`id` 用模板串生成、
 * `expectedName`/`realName`/`displayName` 会以 `{ ...m, x: y }` 形态改写 ——
 * 后者是安全的（展开保留了所有字段），故本判据只要求**整份重建**的那些地方字段齐全。
 */

const here = dirname(fileURLToPath(import.meta.url));
const read = (rel: string) => readFileSync(join(here, rel), "utf8");

/** 只看生产段的非注释行 —— 本仓已 5 次栽在「注释里的字面量满足了断言」上。 */
function production(rel: string): string {
  return inspectableLines(read(rel), rel)
    .map((l: { text: string }) => l.text)
    .join("\n");
}

const snakeToCamel = (s: string) => s.replace(/_([a-z0-9])/g, (_m, c: string) => c.toUpperCase());

/** Rust `ModelMapping` 的字段名（`rename_all = "camelCase"` 后的形态）。 */
function rustFields(): string[] {
  const src = read("../src-tauri/src/model.rs");
  const at = src.indexOf("pub struct ModelMapping");
  expect(at, "model.rs 里找不到 ModelMapping —— 判据失去目标，先修判据").toBeGreaterThan(0);
  expect(
    src.slice(Math.max(0, at - 200), at),
    'ModelMapping 必须带 rename_all = "camelCase"，否则前端字段会被静默丢弃',
  ).toContain('rename_all = "camelCase"');
  const body = src.slice(at, src.indexOf("\n}", at));
  const names = [...body.matchAll(/^ {4}pub ([a-z0-9_]+):/gm)].map((m) => snakeToCamel(m[1]));
  expect(names.length, "抽到的字段太少，解析方式该跟着改").toBe(4);
  return names;
}

/**
 * 前端**整份构造**一条映射的地方，各自出现的键。
 *
 * 锚点是「`id:` 起头的对象字面量」—— 那正是整份构造（而不是 `{ ...m, x: y }` 改写）的形态。
 *
 * ⚠️ **必须先把模板串里的 `${…}` 挖掉**：`id: \`m_${Date.now()}\`` 里那个 `}` 会被
 * 「找配对右括号」当成对象字面量的结尾，于是只抽到 `id` 一个键、把另外三个报成缺失。
 * 第一版就是这样，报出来的是一条假警。
 */
function rebuildSites(): { line: number; keys: string[] }[] {
  const src = production("../src/components/ModelMappingSection.tsx").replace(
    /\$\{[^{}]*\}/g,
    "__EXPR__",
  );
  const sites: { line: number; keys: string[] }[] = [];
  for (const m of src.matchAll(/\bid: `m_/g)) {
    const open = src.lastIndexOf("{", m.index);
    const close = src.indexOf("}", m.index);
    expect(open, "找不到对象字面量的起始括号").toBeGreaterThan(0);
    expect(close, "找不到对象字面量的结束括号").toBeGreaterThan(m.index);
    const body = src.slice(open, close);
    sites.push({
      line: src.slice(0, open).split("\n").length,
      keys: [...body.matchAll(/([a-zA-Z][a-zA-Z0-9]*)\s*:/g)].map((k) => k[1]),
    });
  }
  expect(sites.length, "找不到映射行的构造点 —— 形态变了，判据要跟着改").toBe(2);
  return sites;
}

describe("ModelMapping 每一行的字段齐全性", () => {
  it("整份构造映射行时字段必须齐全（少一个 = 那一格自己空了）", () => {
    const fields = rustFields();
    for (const site of rebuildSites()) {
      const missing = fields.filter((f) => !site.keys.includes(f));
      expect(
        missing,
        `ModelMappingSection.tsx:${site.line} 构造映射行时漏了这些字段 ——` +
          `用户填过的值会在那一步被静默清空`,
      ).toEqual([]);
    }
  });

  it("构造点里不出现 Rust 不认识的键（serde 会静默丢弃拼错的名字）", () => {
    const fields = new Set(rustFields());
    for (const site of rebuildSites()) {
      expect(
        site.keys.filter((k) => !fields.has(k)),
        `第 ${site.line} 行：这些键在 Rust ModelMapping 里不存在，会被 serde 静默丢弃（多半是拼错）`,
      ).toEqual([]);
    }
  });

  it("显示名这一位必须在（它就是本判据的来由）", () => {
    // 单独钉一条：上面两条是通用判据，这条锁住那个具体的失效形态。
    expect(rustFields()).toContain("displayName");
    for (const site of rebuildSites()) expect(site.keys).toContain("displayName");
  });
});
