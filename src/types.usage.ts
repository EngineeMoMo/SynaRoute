// 用量统计页的类型。从 types.ts 拆出来的（那个文件 898/900，加模型维度放不下）；
// types.ts 里 re-export 了这里的全部类型，既有的 `from "@/types"` 一处都不用改。
//
// ⚠️ 跨语言判据读的是**这个文件**（tests/unpricedReasonParity、tests/usageRefresh）。

import type { CategoryType, TokenUsage } from "./types";

/** 用量统计：按「分类 × Key（× 模型）」聚合的一行（后端 get_token_usage / 日桶条目）。 */
export interface TokenUsageByKey {
  categoryId: CategoryType;
  /** 空串 = 该分类的系统级事件（无具体 Key） */
  keyId: string;
  /** 上游真实模型名（v4 起）。缺省 = 升级前没记模型的历史用量，或按 Key 汇总的视图。 */
  model?: string;
  usage: TokenUsage;
}

/** 一天的用量分桶（后端 get_daily_usage 返回，最近 90 天，最新在前）。 */
export interface DailyUsageBucket {
  /** 日期 `YYYY-MM-DD`。v4 起按本机时区切天；更早写下的桶是 UTC 日期，原样保留。 */
  date: string;
  entries: TokenUsageByKey[];
}

/** 某一天的估算花费（后端 get_daily_cost 返回；与表格金额同一套按模型估价）。 */
export interface DailyCost {
  /** 日期 `YYYY-MM-DD`，与日桶同口径（本机时区）。 */
  date: string;
  /** 那天能算出的那几格之和（纳美元）。 */
  costNano: number;
  /** 那天的 token 合计（输入 + 输出 + 缓存读 + 缓存写）。 */
  tokens: number;
  /** 其中算不出金额的 token 数。非 0 时界面要标「部分未计入」。 */
  unpricedTokens: number;
}

/**
 * 单价来源，界面据此如实标注估算精度。
 *
 * - `exact`：命中内置官方单价表的具体型号
 * - `family`：按家族名兜底，或升级前没记模型、按 Key 的代表模型估（界面打「≈」）
 * - `unknown`：没有可用单价，金额显示「—」而非 0
 */
export type PricingSource = "exact" | "family" | "unknown";

/**
 * 为什么这一行（或其中一部分）算不出金额。
 *
 * **必须与 Rust 的 `usage_cost::UnpricedReason` 变体一一对应**（后端用
 * `#[serde(tag = "kind")]` 序列化，故这里是判别式联合）。两边分叉是静默的 ——
 * 编译器管不到这条缝，而漏一支的表现是界面渲染出空白提示。
 * `tests/unpricedReasonParity.test.ts` 机械校验两侧的变体集合一致。
 */
export type UnpricedReason =
  /** 大脑聚合的历史用量（keyId 为空串，无 Key 归属） */
  | { kind: "aggregate" }
  /** 这一行的 Key 已不在配置里且没有留档，那部分又没记模型 */
  | { kind: "keyDeleted" }
  /** Key 在，但那部分没记模型、Key 也没有代表模型名 */
  | { kind: "noModelName" }
  /** 有名字可查，但单价表里没有它 */
  | { kind: "modelNotInTable"; model: string };

/** 一行里的一格：某个模型的用量与估价（「按模型拆分」tooltip 用）。 */
export interface UsagePartCost {
  /** 上游真实模型名。空串 = 升级前没记模型的历史用量。 */
  model: string;
  /** 实际拿去查价的名字（历史格是 Key 的代表模型）。缺省 = 无名可查。 */
  pricedAs?: string;
  usage: TokenUsage;
  /** 纳美元。缺省 = 这一格算不出。 */
  costNano?: number;
  pricingSource: PricingSource;
}

/** 带成本估算的用量行（后端 get_usage_with_cost 返回）。 */
export interface UsageCostRow {
  categoryId: CategoryType;
  keyId: string;
  /** Key 可读名；已不在配置里时从留档还原，连留档也没有才是 null。 */
  keyName: string | null;
  /** 🔴 这一行的 Key 已不在配置里。`keyName` 对这类 Key 也有值，故界面必须靠这一位区分。 */
  keyDeleted?: boolean;
  usage: TokenUsage;
  /**
   * 🔴 是否真的捕获过上游回报的 usage —— **不能用「四个 token 全 0」代替**。
   * 行集合是「配置里的 Key ∪ 历史累计桶」，故新 Key 也有行；`false` = 尚无用量
   * （金额显示「尚无用量」而**不是 $0**、不计进「未计入」条数），`true` = 真有过消耗。
   */
  hasRecordedUsage?: boolean;
  /** 估算成本（纳美元）= 能算出的那几格之和。null = 一格都算不出 */
  costNano: number | null;
  pricingSource: PricingSource;
  /** 实际生效的倍率（回显，空则 "1.0"） */
  multiplier: string;
  /**
   * 有某一格算不出时的成因。`costNano === null` ⟹ 一定有；反之不成立 ——
   * 金额与成因同时存在 = 部分未计入（界面标「+?」）。
   */
  unpricedReason?: UnpricedReason;
  /** 算出金额的那几格里花费最多的查价名。界面显示「主要按 X 估算」。 */
  pricedByModel?: string;
  /** 逐个模型的用量与估价（花费从高到低）。占位行为空。 */
  breakdown?: UsagePartCost[];
  /** 用户为这条 Key 设的花费预算（美元）。界面据此画「$已花 / $预算」；未设则不画。 */
  budgetUsd?: number;
  /** 🔴 累计估算花费是否已达/超预算。后端单一事实来源，前端不自己比。 */
  overBudget?: boolean;
}

/**
 * 单价表现状（后端 get_pricing_status）：内置表核对日期 + 在线表（LiteLLM 公开价格，每天拉一次）。
 * 合并规则在后端（`pricing::online`）：同名内置赢，在线只补内置表不认得的、更具体的名字。
 */
export interface PricingStatus {
  /** 内置表的核对日期 `YYYY-MM-DD` */
  builtinDate: string;
  /** 在线表当前有几个模型。0 = 还没有在线表 */
  onlineRows: number;
  /** 在线表拉取成功的时刻（毫秒） */
  onlineFetchedAtMs?: number;
  /** 从哪个镜像拉的（主机名） */
  onlineSource?: string;
  /** 最近一次拉取失败的原因。与 `onlineRows > 0` 同时出现 = 正在沿用上一份 */
  onlineError?: string;
}
