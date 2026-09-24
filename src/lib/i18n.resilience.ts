// 「可靠性」页的本地化词条（把五层弹性做成可见面板）。
//
// 独立成片、且经由 i18n.ts 的合并 import + 合并 spread 挂进主字典（那边冻结在棘轮上，
// 故 import 追加到已有的合并行、spread 追加到已有的多项 spread 行，i18n.ts 净零改动）。
//
// ⚠️ zh 与 en 的 key 集合必须完全一致（由 i18n.ts 的对称性判据覆盖，因为它们已并进主字典）。

type Dict = Record<string, string>;

export const resilienceZh: Dict = {
  "nav.resilience": "可靠性",
  "resilience.title": "可靠性总览",
  "resilience.subtitle": "五层弹性容错的「当前」状态：熔断 / 限流 / 余额 / 预算 / 并发。它们默默把请求从坏 Key 移开，这里把「此刻谁在什么状态」一次摊开。",
  "resilience.loading": "加载中…",
  "resilience.empty": "还没有配置 Key",
  "resilience.proxyRunning": "代理运行中",
  "resilience.proxyStopped": "代理已停",
  "resilience.enabledCount": "{n} 个已启用",
  // 汇总 / 逐 Key 共用的状态词
  "resilience.state.healthy": "正常",
  "resilience.state.breaker": "熔断中",
  "resilience.state.rateLimited": "限流中",
  "resilience.state.exhausted": "余额耗尽",
  "resilience.state.overBudget": "超预算",
  "resilience.state.disabled": "已停用",
  "resilience.state.down": "探测不可达",
  "resilience.colKey": "Key",
  "resilience.colState": "运行态",
  "resilience.inFlight": "在途 {n}/{max}",
  "resilience.failCount": "连续失败 {n}",
  // 近期活动（来自会滚动的 500 条事件环，是「近期」不是「累计」）
  "resilience.recent": "近期事件（近 {n} 条环内）：路由 {routes} · 故障转移 {failovers} · 错误 {errors} · 告警 {warnings}",
  "resilience.recentNote": "这些是近 500 条事件里的构成，会滚动，不是自统计起点的累计。",
};

export const resilienceEn: Dict = {
  "nav.resilience": "Reliability",
  "resilience.title": "Reliability",
  "resilience.subtitle": "The current state of the five resilience layers: circuit breaker / rate limit / balance / budget / concurrency. They quietly steer requests away from bad keys — this lays out who is in what state right now.",
  "resilience.loading": "Loading…",
  "resilience.empty": "No keys configured yet",
  "resilience.proxyRunning": "Proxy running",
  "resilience.proxyStopped": "Proxy stopped",
  "resilience.enabledCount": "{n} enabled",
  "resilience.state.healthy": "Healthy",
  "resilience.state.breaker": "Circuit-broken",
  "resilience.state.rateLimited": "Rate-limited",
  "resilience.state.exhausted": "Balance out",
  "resilience.state.overBudget": "Over budget",
  "resilience.state.disabled": "Disabled",
  "resilience.state.down": "Probe unreachable",
  "resilience.colKey": "Key",
  "resilience.colState": "State",
  "resilience.inFlight": "in-flight {n}/{max}",
  "resilience.failCount": "{n} consecutive failures",
  "resilience.recent": "Recent events (last {n} in ring): routes {routes} · failovers {failovers} · errors {errors} · warnings {warnings}",
  "resilience.recentNote": "Composition of the last 500 events; it rotates and is not an all-time total.",
};
