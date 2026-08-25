//! 大脑聚合的**阶段预算**计算：整轮墙钟预算怎么在「成员 → 压缩 → 决策者」三阶段之间切。
//!
//! 从 aggregate.rs 抽出来的：这几个函数是纯算术（入参出参都是 u64），与聚合流程零耦合，
//! 而 aggregate.rs 的棘轮余量为 0。抽出后它们的可测性也更清楚 ——
//! 这里每个常数都有一段「为什么是这个数」的账，那才是真正需要被钉住的东西。
/// 决策者阶段的保底预算（毫秒）。
///
/// 整轮墙钟预算下，串行的「成员 → 压缩 → 决策者」三阶段共享同一 deadline。为避免前面
/// 阶段（成员慢/压缩慢）把时间吃光、饿死最重要的决策者综合步骤，给决策者留一块地板：
/// 整轮预算的 35%，绝对不低于 90s；但小预算时 90s 可能超过整轮总量，故再用 45% 上限夹住
/// （保证成员+压缩至少还能分到 ~55%）。
///
/// 例：total=60000 → 35%=21000<90000，被 45%=27000 夹住 → 27000；
///     total=257000 → 35%=89950<90000 → 90000（<45%=115650）；
///     total=540000 → 35%=189000（>90000，<45%=243000）→ 189000；
///     total=600000 → 35%=210000 → 210000。
pub(crate) fn decider_floor_ms(total_ms: u64) -> u64 {
    let pct35 = total_ms * 35 / 100;
    pct35.max(90_000).min(total_ms * 45 / 100)
}

/// 成员/压缩阶段的可用预算（毫秒）：在整轮 deadline 剩余时间里扣掉决策者地板。
///
/// `remaining_ms` 为此刻距整轮 deadline 的剩余毫秒。扣掉 `decider_floor` 后仍给一个最小
/// 下限（`min_floor`，默认 5s）——即使前面阶段几乎耗尽预算，也让本阶段有机会发出请求，
/// 略微越界由客户端超时余量兜底，好过给 0ms 必然超时。
pub(crate) fn upstream_phase_budget_ms(remaining_ms: u64, decider_floor: u64, min_floor: u64) -> u64 {
    remaining_ms.saturating_sub(decider_floor).max(min_floor)
}

/// 决策者阶段的可用预算（毫秒）：整轮剩余时间全给决策者（前面省下的它全拿），
/// 同样给最小下限保护，避免 0ms。
pub(crate) fn decider_phase_budget_ms(remaining_ms: u64, min_floor: u64) -> u64 {
    remaining_ms.max(min_floor)
}

/// 阶段预算的最小下限（毫秒）：宁可略微越过整轮 deadline，也要让阶段有机会跑一次
/// （客户端超时留有 +余量，见 tools.rs 的 mcp 客户端超时联动）。
pub(crate) const PHASE_MIN_BUDGET_MS: u64 = 5_000;
