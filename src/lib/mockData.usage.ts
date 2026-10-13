// 用量页的 mock 数据：带成本估算的行，以及「算不出金额」的**四种成因**。
//
// 从 mockData.ts 拆出来的（那个文件在棘轮上余量为 0），但拆得也有道理：这一份的价值
// 全在「把所有分支都铺开」——浏览器预览模式下渲染不到的分支，样式与文案就必然做漏。
// 本轮新增的四种无价成因各有不同的界面文案与指路，故每一种都要在预览里出现一次。

import type { CategoryType, DailyCost, ProviderKey, TokenUsage, TokenUsageByKey, UsageCostRow, UsagePartCost } from "@/types";
import type { PricingSource } from "@/types";
import { lastNDayKeys } from "./usagePeriods";

/** 预览里「清理」过的已删除 Key（mock 没有磁盘，记在内存里，刷新页面即恢复）。 */
export const purgedUsageKeys = new Set<string>();

/** 把一行按 9:1 拆成两个模型（演示「按模型拆分」tooltip）。 */
function splitTwo(usage: TokenUsage, costNano: number, big: string, small: string): UsagePartCost[] {
  const part = (f: number): TokenUsage => ({
    input: Math.round(usage.input * f),
    output: Math.round(usage.output * f),
    cacheRead: Math.round((usage.cacheRead ?? 0) * f),
  });
  const bigCost = Math.round(costNano * 0.97); // 两格之和必须等于整行金额，否则 tooltip 与表格对不上
  return [
    { model: big, pricedAs: big, usage: part(0.9), costNano: bigCost, pricingSource: "exact" },
    { model: small, pricedAs: small, usage: part(0.1), costNano: costNano - bigCost, pricingSource: "exact" },
  ];
}

/**
 * 按天花费的 mock：近 14 天（本机时区），最新在前，与后端同口径。
 *
 * 刻意铺开三种形态：普通的一天、**有一部分算不出**的一天（今天，验证「部分未计入」标注）、
 * 没有用量的一天（后端不产出那天，前端按 0 处理）。
 */
export function mockDailyCost(): DailyCost[] {
  // 🔴 14 天合计（≈$2.47 / 2.2M token）必须小于下面表格的累计（≈$3.43 / 2.6M token）：真实数据里
  // 「本月」不可能超过「累计」，而官网截图取自演示模式 —— 反过来就是一张「统计算错了」的截图。
  const perDay = [0.26, 0.09, 0, 0.18, 0.33, 0.13, 0.42, 0.15, 0.07, 0, 0.3, 0.23, 0.12, 0.19];
  const days = lastNDayKeys(new Date(), perDay.length).reverse();
  const out: DailyCost[] = [];
  perDay.forEach((usd, i) => {
    if (usd === 0) return;
    const tokens = Math.round(usd * 900_000);
    out.push({ date: days[i], costNano: Math.round(usd * 1e9), tokens, unpricedTokens: i === 0 ? 6_900 : 0 });
  });
  return out;
}

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
 * - `keyDeleted` → 「—」+ keyName 为 null，表格显示 keyId（**没有墓碑**的历史行）
 * - 已删除但有墓碑 → 名字与金额照旧，只多一个「（已删除）」标记（现在删 Key 的常态）
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
    const costNano = total * (src === "exact" ? 3000 : 15000);
    return {
      categoryId: r.categoryId,
      keyId: r.keyId,
      keyName: store[r.categoryId]?.find((k) => k.id === r.keyId)?.name ?? r.keyId ?? null,
      usage: r.usage,
      hasRecordedUsage: true,
      costNano,
      pricingSource: src,
      multiplier: src === "family" ? "0.3" : "1.0",
      pricedByModel: src === "exact" ? "claude-opus-5-5" : "claude-opus-9-9",
      // exact：同一条 Key 跑两个模型（v4 按模型记账的常态）；family：升级前没记模型的历史用量。
      breakdown:
        src === "exact"
          ? splitTwo(r.usage, costNano, "claude-opus-5-5", "claude-haiku-5-5")
          : [{ model: "", pricedAs: "claude-opus-9-9", usage: r.usage, costNano, pricingSource: "family" }],
      // 预算「未超」形态：并排显示「$已花 / $预算」（muted），验证进度可读。
      budgetUsd: src === "exact" ? 100 : undefined,
      overBudget: false,
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
      hasRecordedUsage: true,
      costNano: 760_000_000,
      pricingSource: "family",
      multiplier: "0.3",
      pricedByModel: "claude-sonnet",
      // 预算「已超」形态：$0.76 ≥ 设的 $0.5 → 金额标红 + 「超预算」徽标 + 顶部告警条。
      budgetUsd: 0.5,
      overBudget: true,
    });
  }
  if (cli[1]) {
    // 成因③：有模型名但表里没有 → 界面必须**点出这个名字**，否则用户无从反馈补哪一条
    out.push({
      categoryId: "claude-cli",
      keyId: cli[1].id,
      keyName: cli[1].name,
      usage: { input: 9_100, output: 2_200, cacheRead: 0, cacheCreation: 0 },
      hasRecordedUsage: true,
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
      hasRecordedUsage: true,
      costNano: null,
      pricingSource: "unknown",
      multiplier: "1.0",
      unpricedReason: { kind: "noModelName" },
    });
  }
  // 🔴 **零用量的新 Key**（2026-09-09 用户实报「新加的 key 都看不见」）。
  //
  // 后端行集合现在是「配置里的 Key ∪ 历史桶」，这类行必须在预览里出现一次 ——
  // 它的呈现与其余各行都不同（金额显示「尚无用量」而**不是 $0**、不进无价横幅、
  // 不带 unpricedReason），而没渲染到的分支样式与文案必然做漏。
  if (cli[3]) {
    out.push({
      categoryId: "claude-cli",
      keyId: cli[3].id,
      keyName: cli[3].name,
      usage: { input: 0, output: 0, cacheRead: 0, cacheCreation: 0 },
      hasRecordedUsage: false,
      costNano: null,
      pricingSource: "unknown",
      multiplier: "1.0",
    });
  }
  // 部分算得出：一个模型在表里、一个不在 → 金额照显示（能算的那部分），旁边标「+?」，
  // 横幅照样按成因列出那个不认识的模型。
  if (cli[4]) {
    const known: TokenUsage = { input: 18_000, output: 2_400, cacheRead: 40_000 };
    const odd: TokenUsage = { input: 6_000, output: 900 };
    out.push({
      categoryId: "claude-cli",
      keyId: cli[4].id,
      keyName: cli[4].name,
      usage: { input: 24_000, output: 3_300, cacheRead: 40_000, cacheCreation: 0 },
      hasRecordedUsage: true,
      costNano: 136_000_000,
      pricingSource: "exact",
      multiplier: "1.0",
      pricedByModel: "glm-5-3",
      unpricedReason: { kind: "modelNotInTable", model: "glm-5-3-air-preview" },
      breakdown: [
        { model: "glm-5-3", pricedAs: "glm-5-3", usage: known, costNano: 136_000_000, pricingSource: "exact" },
        { model: "glm-5-3-air-preview", pricedAs: "glm-5-3-air-preview", usage: odd, pricingSource: "unknown" },
      ],
    });
  }

  // 成因①：旧版大脑聚合的用量，keyId 为空串 → 表格显示「（系统级）」。
  // 这一行不依赖 store 里有没有 Key（它本来就没有 Key），故无条件补。
  out.push({
    categoryId: "codex",
    keyId: "",
    keyName: null,
    usage: { input: 412_500, output: 29_500, cacheRead: 66_800, cacheCreation: 0 },
    hasRecordedUsage: true,
    costNano: null,
    pricingSource: "unknown",
    multiplier: "1.0",
    unpricedReason: { kind: "aggregate" },
  });

  // 成因②：Key 早就删了、当时没留档 —— 金额不可恢复，只能清理。
  // keyName 为 null，表格退回显示 keyId。这一支是历史遗留，不会再新增。
  out.push({
    categoryId: "claude-desktop",
    keyId: "2c24a048-401d-deleted",
    keyName: null,
    keyDeleted: true,
    usage: { input: 88_000, output: 5_400, cacheRead: 210_000, cacheCreation: 0 },
    hasRecordedUsage: true,
    costNano: null,
    pricingSource: "unknown",
    multiplier: "1.0",
    unpricedReason: { kind: "keyDeleted" },
  });

  // 已离开配置但**有墓碑**（导入时整份替换 / 手改 config.json 拿掉的）：名字与金额从留档还原，
  // 只多一个「（已删除）」标记 + 清理按钮。用删除按钮删的 Key 不会出现在这里（2026-10-12 起连用量一起删）。
  const tombUsage: TokenUsage = { input: 412_000, output: 26_500, cacheRead: 980_000, cacheCreation: 12_000 };
  out.push({
    categoryId: "claude-cli",
    keyId: "9f31c7be-old-relay",
    keyName: "厂商3（已下线中转）",
    keyDeleted: true,
    usage: tombUsage,
    hasRecordedUsage: true,
    costNano: 2_140_000_000,
    pricingSource: "family",
    multiplier: "1.4",
    pricedByModel: "claude-sonnet-4-5",
    breakdown: [
      { model: "", pricedAs: "claude-sonnet-4-5", usage: tombUsage, costNano: 2_140_000_000, pricingSource: "family" },
    ],
  });

  // 预览里点过「清理」的行不再出现（与后端同口径：清理 = 那条 Key 的用量整行消失）。
  return out.filter((r) => !purgedUsageKeys.has(r.keyId));
}
