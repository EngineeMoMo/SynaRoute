// 「可靠性」页的类型（与 Rust `resilience.rs` 的 serde 结构对齐）。
//
// 放这里而不是 types.ts：那个文件正卡在棘轮新文件上限（900 行）附近，再塞几个接口就超了。
// 复用 types.ts 的 `HealthStatus` / `CategoryType`，不另定义。

import type { CategoryType, HealthStatus } from "@/types";

/** 逐 Key 的运行态明细（此刻读数）。 */
export interface KeyResilience {
  keyId: string;
  keyName: string;
  enabled: boolean;
  status: HealthStatus;
  failCount: number;
  /** Key 级熔断窗口此刻生效。 */
  breakerActive: boolean;
  /** 上游 Retry-After 开的配额窗口此刻生效。 */
  rateLimited: boolean;
  /** 余额闸门判「确定耗尽」（查不到 ≠ 耗尽）。 */
  balanceExhausted: boolean;
  /** 累计估算花费已达用户设的预算（软上限）。 */
  overBudget: boolean;
  inFlight: number;
  maxInFlight: number;
}

export interface CategoryResilience {
  categoryId: CategoryType;
  proxyRunning: boolean;
  enabledCount: number;
  healthyCount: number;
  breakerCount: number;
  rateLimitedCount: number;
  exhaustedCount: number;
  overBudgetCount: number;
  keys: KeyResilience[];
}

/** 近期事件构成（来自会滚动的 500 条环，是「近期」不是「累计」）。 */
export interface RecentActivity {
  totalEvents: number;
  routes: number;
  failovers: number;
  errors: number;
  warnings: number;
}

export interface ResilienceOverview {
  categories: CategoryResilience[];
  recent: RecentActivity;
}
