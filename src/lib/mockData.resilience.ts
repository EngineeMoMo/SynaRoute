// 「可靠性」页的 mock：把逐 Key 的每一种弹性状态都铺开一次 —— 浏览器预览是本仓验布局的
// 唯一手段，没渲染到的状态样式与文案必然做漏（同 mockData.usage 那份的理由）。
//
// 按序号稳定分派状态（不随机），让每条 Key 每次刷新落在同一状态，改样式时不会误判「没生效」。

import type { CategoryType, ProviderKey } from "@/types";
import type { CategoryResilience, KeyResilience, ResilienceOverview } from "@/lib/resilience";

const MAX_INFLIGHT = 8;

/** 状态序号（**在启用 Key 中**的位次；停用的 Key 传 -1）→ 0 正常(有在途)、1 熔断、2 限流、
 * 3 余额耗尽、4 超预算，其余正常。按「启用位次」而非原始下标分派，保证前 5 种状态都落在
 * 启用的 Key 上、都能在预览里出现（否则某个状态恰好分给一条停用 Key 就被抑制、样式做漏）。 */
function mockKey(k: ProviderKey, i: number): KeyResilience {
  const breakerActive = i === 1;
  const rateLimited = i === 2;
  const balanceExhausted = i === 3;
  const overBudget = i === 4;
  return {
    keyId: k.id,
    keyName: k.name,
    enabled: k.enabled,
    status: !k.enabled ? "unknown" : breakerActive ? "down" : "up",
    failCount: breakerActive ? 3 : 0,
    breakerActive,
    rateLimited,
    balanceExhausted,
    overBudget,
    inFlight: i === 0 ? 2 : 0,
    maxInFlight: MAX_INFLIGHT,
  };
}

export function mockResilienceOverview(
  store: Record<CategoryType, ProviderKey[]>,
): ResilienceOverview {
  const cats: CategoryResilience[] = [];
  for (const cat of ["claude-cli", "claude-desktop", "codex"] as CategoryType[]) {
    let en = 0; // 启用 Key 的位次，停用的传 -1（mockKey 据此把所有 flag 置 false）
    const keys = (store[cat] ?? []).map((k) => mockKey(k, k.enabled ? en++ : -1));
    if (keys.length === 0) continue;
    const enabled = keys.filter((r) => r.enabled);
    cats.push({
      categoryId: cat,
      proxyRunning: cat === "claude-cli",
      enabledCount: enabled.length,
      // 与后端 build() 同口径：超预算不算「不健康」，healthy 只排除熔断/限流/余额耗尽/down。
      healthyCount: enabled.filter(
        (r) => !r.breakerActive && !r.rateLimited && !r.balanceExhausted && r.status !== "down",
      ).length,
      breakerCount: enabled.filter((r) => r.breakerActive).length,
      rateLimitedCount: enabled.filter((r) => r.rateLimited).length,
      exhaustedCount: enabled.filter((r) => r.balanceExhausted).length,
      overBudgetCount: enabled.filter((r) => r.overBudget).length,
      keys,
    });
  }
  return {
    categories: cats,
    recent: { totalEvents: 500, routes: 431, failovers: 12, errors: 5, warnings: 9 },
  };
}
