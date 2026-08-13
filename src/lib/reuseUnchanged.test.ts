import { describe, it, expect } from "vitest";
import { reuseUnchanged } from "./reuseUnchanged";

/**
 * `reuseUnchanged` 同时承担**性能**与**正确性**两副担子，两边都要测：
 *
 * - 性能侧：内容没变时必须复用旧对象（引用相等），否则 KeyCard / LogRow 的 memo 恒失效。
 * - 正确性侧：内容变了必须换成新对象。这条更要紧 —— 错误复用会让「开关点了不动」、
 *   「健康检查结果不刷新」这类 bug 出现，比性能问题严重得多。
 */

type Row = { id: string; name: string; enabled?: boolean };

describe("reuseUnchanged", () => {
  it("内容完全相同时，连数组引用一起保住", () => {
    const prev: Row[] = [{ id: "a", name: "A" }, { id: "b", name: "B" }];
    const next: Row[] = [{ id: "a", name: "A" }, { id: "b", name: "B" }]; // 全新对象，内容同
    const out = reuseUnchanged(prev, next);
    expect(out).toBe(prev); // 数组本身也复用，订阅整个数组的组件不白跑
  });

  it("逐条复用未变对象的引用（这是 memo 生效的前提）", () => {
    const prev: Row[] = [{ id: "a", name: "A" }, { id: "b", name: "B" }];
    const next: Row[] = [{ id: "a", name: "A" }, { id: "b", name: "B-改了" }];
    const out = reuseUnchanged(prev, next);
    expect(out).not.toBe(prev); // 有变更 → 数组换新
    expect(out[0]).toBe(prev[0]); // a 未变 → 复用旧引用
    expect(out[1]).not.toBe(prev[1]); // b 变了 → 用新对象
    expect(out[1].name).toBe("B-改了"); // 且新内容确实生效
  });

  it("真实变更必须传出去（错误复用会导致「点了不动」）", () => {
    const prev: Row[] = [{ id: "a", name: "A", enabled: true }];
    const next: Row[] = [{ id: "a", name: "A", enabled: false }];
    const out = reuseUnchanged(prev, next);
    expect(out[0].enabled).toBe(false);
    expect(out[0]).toBe(next[0]);
  });

  it("仅顺序变化（排序/上移下移）时，元素引用复用但数组换新", () => {
    const prev: Row[] = [{ id: "a", name: "A" }, { id: "b", name: "B" }];
    const next: Row[] = [{ id: "b", name: "B" }, { id: "a", name: "A" }];
    const out = reuseUnchanged(prev, next);
    // 数组必须换新，否则 UI 不会重排（上移/下移看起来失灵）
    expect(out).not.toBe(prev);
    expect(out.map((r) => r.id)).toEqual(["b", "a"]);
    // 元素内容没变，引用仍可复用
    expect(out[0]).toBe(prev[1]);
    expect(out[1]).toBe(prev[0]);
  });

  it("新增条目（日志追加是最常见的场景）", () => {
    const prev: Row[] = [{ id: "a", name: "A" }];
    const next: Row[] = [{ id: "a", name: "A" }, { id: "b", name: "B" }];
    const out = reuseUnchanged(prev, next);
    expect(out).toHaveLength(2);
    expect(out[0]).toBe(prev[0]); // 老的那条不该跟着重渲染
    expect(out[1]).toBe(next[1]);
  });

  it("删除条目", () => {
    const prev: Row[] = [{ id: "a", name: "A" }, { id: "b", name: "B" }];
    const next: Row[] = [{ id: "b", name: "B" }];
    const out = reuseUnchanged(prev, next);
    expect(out.map((r) => r.id)).toEqual(["b"]);
    expect(out[0]).toBe(prev[1]);
  });

  it("空数组与首次加载不炸", () => {
    // prev 与 next 都空：走 allSame 分支，返回 prev 本身
    expect(reuseUnchanged<Row>([], [])).toEqual([]);

    // 首次加载（prev 空、next 有数据）：数组本身必然是 map 出来的新数组，
    // 这里只要求**元素引用**直通（没有多余的拷贝），不要求数组同引用。
    const fresh: Row[] = [{ id: "a", name: "A" }];
    const loaded = reuseUnchanged<Row>([], fresh);
    expect(loaded).toEqual(fresh);
    expect(loaded[0]).toBe(fresh[0]);

    // 全部删空
    expect(reuseUnchanged<Row>(fresh, [])).toEqual([]);
  });

  it("id 相同但内容全变（Key 被整体改写）时不误复用", () => {
    const prev: Row[] = [{ id: "a", name: "旧名", enabled: true }];
    const next: Row[] = [{ id: "a", name: "新名", enabled: false }];
    const out = reuseUnchanged(prev, next);
    expect(out[0]).toBe(next[0]);
    expect(out[0].name).toBe("新名");
  });

  it("嵌套字段变化能被发现（health 这类嵌套对象最容易被逐字段比较器漏掉）", () => {
    type Nested = { id: string; health: { ok: boolean; latencyMs: number } };
    const prev: Nested[] = [{ id: "a", health: { ok: true, latencyMs: 100 } }];
    const next: Nested[] = [{ id: "a", health: { ok: true, latencyMs: 250 } }];
    const out = reuseUnchanged(prev, next);
    // 只有深比较才能发现 latencyMs 变了；用浅比较会误判成「没变」而复用旧对象，
    // 表现为健康检查延迟数字永远不刷新。
    expect(out[0]).toBe(next[0]);
    expect(out[0].health.latencyMs).toBe(250);
  });

  it("整列表未变时的复用不受元素顺序内的重复 id 影响", () => {
    // 防御性：后端理论上不该给重复 id，但真给了也不能崩或错配。
    const prev: Row[] = [{ id: "a", name: "A1" }, { id: "a", name: "A2" }];
    const next: Row[] = [{ id: "a", name: "A1" }, { id: "a", name: "A2" }];
    const out = reuseUnchanged(prev, next);
    expect(out.map((r) => r.name)).toEqual(["A1", "A2"]);
  });
});
