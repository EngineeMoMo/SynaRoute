// 用量页的 mock 数据：带成本估算的行，以及「算不出金额」的**四种成因**。
//
// 从 mockData.ts 拆出来的（那个文件在棘轮上余量为 0），但拆得也有道理：这一份的价值
// 全在「把所有分支都铺开」——浏览器预览模式下渲染不到的分支，样式与文案就必然做漏。
// 本轮新增的四种无价成因各有不同的界面文案与指路，故每一种都要在预览里出现一次。

import type { CategoryType, ProviderKey, TokenUsageByKey, UsageCostRow } from "@/types";
import type { PricingSource } from "@/types";

/**
 * 造出用量页那张表的 mock 行。
 *
 * `store` 由调用方传入（原实现直接闭包引用 mockData.ts 的模块级 store），
 * `rows` 是 `mockBridge.tokenUsage()` 的结果。
 *
 * 覆盖矩阵（每一项都对应界面上一处不同的呈现）：
 * - `exact` → 金额**不带** ≈
 * - `family` → 金额带 ≈ + tooltip 说明按家族名估的
 * - `modelNotInTable` → 「—」+ 点出那个模型名
 * - `aggregate` → 「—」+ 说明是旧版聚合用量、**没有**「去 Key 里设兜底模型」那句
 *   （那句对它是无效操作）
 * - `keyDeleted` → 「—」+ keyName 为 null，表格显示 keyId
 * - `noModelName` → 「—」+ 唯一一句「去设兜底模型」真正成立的成因
 */
export function mockUsageWithCost(
  store: Record<CategoryType, ProviderKey[]>,
  rows: TokenUsageByKey[],
): UsageCostRow[] {
  const sources: PricingSource[] = ["exact", "family"];
  const out: UsageCostRow[] = rows.map((r, i) => {
    const src = sources[i % sources.length];
    const total = r.usage.input + r.usage.output;
    return {
      categoryId: r.categoryId,
      keyId: r.keyId,
      keyName: store[r.categoryId]?.find((k) => k.id === r.keyId)?.name ?? r.keyId ?? null,
      usage: r.usage,
      costNano: total * (src === "exact" ? 3000 : 15000),
      pricingSource: src,
      multiplier: src === "family" ? "0.3" : "1.0",
      pricedByModel: src === "exact" ? "claude-opus-5" : "claude-opus-9-9",
    };
  });

  // mock 的事件里只有一条 Key 有用量，故上面只产出 1 行。下面把其余分支各补一行。
  //
  // **必须挑没出现过的 keyId**：直接取 cli[1] 会与上面 rows 里已有的那条撞号，
  // React 的 key 重复会让表格行错乱（实测报过 "two children with the same key"）。
  const used = new Set(out.map((r) => `${r.categoryId}/${r.keyId}`));
  const cli = (store["claude-cli"] ?? []).filter((k) => !used.has(`claude-cli/${k.id}`));

  if (cli[0]) {
    out.push({
      categoryId: "claude-cli",
      keyId: cli[0].id,
      keyName: cli[0].name,
      usage: { input: 42_000, output: 8_800, cacheRead: 12_000, cacheCreation: 3_100 },
      costNano: 760_000_000,
      pricingSource: "family",
      multiplier: "0.3",
      pricedByModel: "claude-sonnet",
    });
  }
  if (cli[1]) {
    // 成因③：有模型名但表里没有 → 界面必须**点出这个名字**，否则用户无从反馈补哪一条
    out.push({
      categoryId: "claude-cli",
      keyId: cli[1].id,
      keyName: cli[1].name,
      usage: { input: 9_100, output: 2_200, cacheRead: 0, cacheCreation: 0 },
      costNano: null,
      pricingSource: "unknown",
      multiplier: "1.0",
      unpricedReason: { kind: "modelNotInTable", model: "ernie-5-turbo" },
    });
  }
  if (cli[2]) {
    // 成因④：Key 在、但没有任何代表模型名 —— 唯一一种「去设兜底模型」真正有用的情形
    out.push({
      categoryId: "claude-cli",
      keyId: cli[2].id,
      keyName: cli[2].name,
      usage: { input: 3_400, output: 900, cacheRead: 0, cacheCreation: 0 },
      costNano: null,
      pricingSource: "unknown",
      multiplier: "1.0",
      unpricedReason: { kind: "noModelName" },
    });
  }

  // 成因①：旧版大脑聚合的用量，keyId 为空串 → 表格显示「（系统级）」。
  // 这一行不依赖 store 里有没有 Key（它本来就没有 Key），故无条件补。
  out.push({
    categoryId: "codex",
    keyId: "",
    keyName: null,
    usage: { input: 412_500, output: 29_500, cacheRead: 66_800, cacheCreation: 0 },
    costNano: null,
    pricingSource: "unknown",
    multiplier: "1.0",
    unpricedReason: { kind: "aggregate" },
  });

  // 成因②：指向一条已被删除的 Key（keyName 为 null，表格退回显示 keyId）
  out.push({
    categoryId: "claude-desktop",
    keyId: "2c24a048-401d-deleted",
    keyName: null,
    usage: { input: 88_000, output: 5_400, cacheRead: 210_000, cacheCreation: 0 },
    costNano: null,
    pricingSource: "unknown",
    multiplier: "1.0",
    unpricedReason: { kind: "keyDeleted" },
  });

  return out;
}
