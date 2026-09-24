import { describe, it, expect } from "vitest";
import { computeQuickFill, usableAggregateKeys } from "@/lib/brainQuickStart";
import type { BrainConfig, ProviderKey } from "@/types";

// 「一键会诊」的落点是**不可逆的付费调用**（会诊要真打上游），而计算它的这段逻辑同时被
// BrainPage 的「一键配置」按钮与 BrainQuickStart 的「一键开始」共用。故它的边界都要钉住：
// 少认一个 allowInAggregate 的 Key → 用户报「没有可用 Key」而它其实可用；
// 幂等破了 → 重复点一次成员翻倍；决策者没保留 → 覆盖掉用户已选的那个。

function key(over: Partial<ProviderKey> & { id: string }): ProviderKey {
  return {
    categoryId: "claude-cli",
    name: over.id,
    vendor: "anthropic",
    baseUrl: "https://x",
    protocol: "anthropic",
    hasSecret: true,
    enabled: true,
    priority: 0,
    params: {},
    models: [{ realName: `${over.id}-model`, source: "manual" }],
    mappings: [],
    health: { status: "unknown", failCount: 0 },
    ...over,
  };
}

function cfg(over: Partial<BrainConfig> = {}): BrainConfig {
  return {
    categoryId: "claude-cli",
    enabled: false,
    aggregateMode: "compressed",
    concurrencyLimit: 3,
    totalTimeoutMs: 60000,
    members: [],
    retrievalEnabled: false,
    ...over,
  };
}

describe("usableAggregateKeys", () => {
  it("含 allowInAggregate 的禁用 Key，排除无模型的 Key", () => {
    const keys = [
      key({ id: "on" }), // 启用 + 有模型 → 可用
      key({ id: "off", enabled: false }), // 纯禁用 → 不可用
      key({ id: "agg", enabled: false, allowInAggregate: true }), // 禁用但勾了允许聚合 → 可用
      key({ id: "nomodel", models: [] }), // 无模型 → 不可用
    ];
    expect(usableAggregateKeys(keys).map((k) => k.id)).toEqual(["on", "agg"]);
  });
});

describe("computeQuickFill", () => {
  it("没有可用 Key 时返回 null（而不是写一份空配置）", () => {
    expect(computeQuickFill(cfg(), [key({ id: "off", enabled: false })])).toBeNull();
    expect(computeQuickFill(cfg(), [])).toBeNull();
  });

  it("每个可用 Key 取首个模型加为成员，并开启 + 选首个作决策者", () => {
    const keys = [
      key({ id: "a", models: [{ realName: "a1", source: "manual" }, { realName: "a2", source: "manual" }] }),
      key({ id: "b", models: [{ realName: "b1", source: "manual" }] }),
    ];
    const out = computeQuickFill(cfg(), keys)!;
    expect(out.enabled).toBe(true);
    expect(out.members.map((m) => `${m.keyId}::${m.modelName}`)).toEqual(["a::a1", "b::b1"]);
    expect(out.deciderRef).toBe("a::a1");
  });

  it("幂等：已在成员里的不重复添加，保留已有成员", () => {
    const keys = [key({ id: "a", models: [{ realName: "a1", source: "manual" }] }), key({ id: "b" })];
    const existing = cfg({ members: [{ id: "bm_a_a1", keyId: "a", modelName: "a1" }] });
    const out = computeQuickFill(existing, keys)!;
    // a::a1 已存在 → 不重复；只新增 b
    expect(out.members.map((m) => `${m.keyId}::${m.modelName}`)).toEqual(["a::a1", "b::b-model"]);
  });

  it("已选决策者时保留，不覆盖成用户没选的那个", () => {
    const keys = [key({ id: "a" }), key({ id: "b" })];
    const out = computeQuickFill(cfg({ deciderRef: "b::b-model" }), keys)!;
    expect(out.deciderRef).toBe("b::b-model");
  });
});
