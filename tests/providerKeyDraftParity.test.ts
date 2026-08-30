import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { inspectableLines } from "../scripts/lib/rust-source.mjs";

/**
 * `ProviderKey` 的**保存载荷**必须字段齐全 —— 漏一个字段就是「保存一次，那个设置自己消失」。
 *
 * # 已经这样坏过一次
 *
 * `allowInAggregate`（「允许大脑聚合使用」）的入口在 **Key 卡片**上，而 `KeyEditor` 的
 * `buildDraftKey()` 里没有它。于是：用户在卡片上勾好 → 大脑聚合正常 → 之后为**任何**别的
 * 原因打开这条 Key 的编辑器点一次保存（甚至只点一下「测试查询」，那条路 `:423` 也 upsert）
 * → 勾选被清成 `false`。下一轮聚合这位成员又被静默跳过，而用户确信自己开过。
 * 方向上永不自愈。
 *
 * # 判据是双向的
 *
 * 后端 `store.rs::upsert_key` 刻意**沿用库里现值**的那几个运行态字段（`health` /
 * `cached_balance`）不该出现在草稿里，其余每个字段都必须出现。于是这条判据同时钉住两侧：
 * - Rust 加字段 → 前端草稿缺它 → 红（就是上面那个缺陷）
 * - 后端把某字段改成「运行态沿用」却没同步 → 红
 *
 * # 边界（写明，免得被当成更强的判据）
 *
 * 只查 `buildDraftKey()` 那一个对象。`:280` 那个探测草稿**刻意不查** ——
 * 它只喂 `fetchModelsDraft`（只读的模型发现探针），不落盘，缺字段不会造成任何丢失。
 *
 * 形式照 [tests/userPrefsParity.test.ts]：从 Rust 源码抽字段名，不靠「我记得它长这样」。
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

/** Rust `ProviderKey` 的字段名（`#[serde(rename_all = "camelCase")]` 后的形态）。 */
function rustFields(): string[] {
  const src = read("../src-tauri/src/model.rs");
  const at = src.indexOf("pub struct ProviderKey");
  expect(at, "model.rs 里找不到 ProviderKey —— 判据失去目标，先修判据").toBeGreaterThan(0);
  // rename_all 是这条判据成立的前提：没有它，前端发的 camelCase 键会被后端**静默丢弃**。
  expect(
    src.slice(Math.max(0, at - 400), at),
    "ProviderKey 必须带 rename_all = \"camelCase\"，否则前端字段会被静默丢弃",
  ).toContain('rename_all = "camelCase"');
  const body = src.slice(at, src.indexOf("\n}", at));
  const names = [...body.matchAll(/^ {4}pub ([a-z0-9_]+):/gm)].map((m) => snakeToCamel(m[1]));
  expect(names.length, "抽到的字段太少，解析方式该跟着改").toBeGreaterThan(15);
  return names;
}

/** `upsert_key` 里「一律沿用库里现值」的运行态字段（同样从 Rust 源码抽）。 */
function backendOwnedFields(): string[] {
  const src = production("../src-tauri/src/store.rs");
  const at = src.indexOf("pub fn upsert_key");
  expect(at, "store.rs 里找不到 upsert_key").toBeGreaterThan(0);
  const body = src.slice(at, at + 2500);
  const names = [...body.matchAll(/existing\.([a-z0-9_]+) = /g)].map((m) => snakeToCamel(m[1]));
  const uniq = [...new Set(names)];
  // 解析到 0 个就主动失败：那会让整条判据退化成「所有字段都必须在草稿里」——
  // 方向相反的假警，且会把人引去给探测草稿补字段。
  expect(uniq.length, "upsert_key 的运行态沿用清单解析为空，判据已失效").toBeGreaterThan(1);
  return uniq;
}

/** `buildDraftKey()` 返回的对象字面量里出现的顶层键。 */
function draftKeys(): string[] {
  const src = production("../src/components/KeyEditor.tsx");
  const at = src.indexOf("const buildDraftKey = (): ProviderKey => ({");
  expect(at, "KeyEditor.tsx 里找不到 buildDraftKey —— 判据失去目标").toBeGreaterThan(0);
  const body = src.slice(at, src.indexOf("\n  });", at));
  // 顶层键固定缩进 4 空格（嵌套的 balanceQuery 内层更深，不会误收）。
  // **两种形态都要认**：`key: value` 与 ES 简写 `key,` —— 只认前者会把
  // `vendor` / `protocol` / `models` / `icon` 这 4 个简写字段误报成「缺失」（第一版就是这样）。
  const names = [...body.matchAll(/^ {4}([a-zA-Z][a-zA-Z0-9]*)\s*[:,]/gm)].map((m) => m[1]);
  expect(names.length, "抽到的草稿键太少，解析方式该跟着改").toBeGreaterThan(15);
  return names;
}

describe("ProviderKey 保存载荷的字段齐全性", () => {
  it("每个字段要么在草稿里，要么是后端自管的运行态", () => {
    const draft = new Set(draftKeys());
    const owned = new Set(backendOwnedFields());
    const missing = rustFields().filter((f) => !draft.has(f) && !owned.has(f));
    expect(
      missing,
      "这些字段保存一次就会丢：要么补进 buildDraftKey，要么在 upsert_key 里列为运行态沿用",
    ).toEqual([]);
  });

  it("草稿里不出现后端自管的运行态字段之外的未知键", () => {
    // serde 默认忽略未知字段 → 草稿里写错一个键名**不报错**，那个设置永远存不进去。
    const fields = new Set(rustFields());
    expect(
      draftKeys().filter((k) => !fields.has(k)),
      "这些草稿键在 Rust ProviderKey 里不存在，会被 serde 静默丢弃（多半是拼错）",
    ).toEqual([]);
  });

  it("「允许大脑聚合使用」这一位必须在草稿里（它就是本判据的来由）", () => {
    // 单独钉一条：上面两条是通用判据，这条锁住那个真实发生过的缺陷本身。
    expect(draftKeys()).toContain("allowInAggregate");
  });
});
