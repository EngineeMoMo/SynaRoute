//! 用量统计面板的 IPC 命令。
//!
//! 从 `lib.rs` 抽出来的：那边是 79 个命令包装的大杂烩、且棘轮余量为 0，
//! 而按「域」分组本来就该做。这里只做 IPC 边界（取参、调 store/纯逻辑、返回），
//! 真正的计算都在 [`crate::usage_cost`] 与 [`crate::pricing`] —— 那边是可测的纯逻辑。
//!
//! 挂载见 `lib.rs` 的 `#[path]` 声明。命令名**不带模块前缀**地暴露给前端
//! （`tauri::generate_handler!` 用路径引用，注册出来的名字仍是函数名），
//! 故前端调用方与 `invoke-command-must-exist` 策略门都不受影响。

use crate::AppState;

/// 按「分类 × Key」聚合的 token 用量（用量统计面板）。
#[tauri::command]
pub fn get_token_usage(state: tauri::State<AppState>) -> Vec<crate::model::TokenUsageByKey> {
    state.store.token_usage_by_key()
}

/// 用量累计的**起算时刻**（毫秒）。面板拿它显示「自 X 起累计」。
///
/// 它是首次开始累计的时间、跨重启保留，不是本次启动时间 —— 面板若写「自本次启动」
/// 而数据其实是跨重启累计的，用户会以为统计漏了。
#[tauri::command]
pub fn get_usage_since(state: tauri::State<AppState>) -> i64 {
    state.store.usage_since_ms()
}

/// 按日分桶的用量（最近 90 天），供「今日 / 本周 / 近 7 日趋势」。
///
/// 🔴 **已含尚未落盘的增量**（`Store::daily_usage_buckets` 会把 `usage_totals - usage_baseline`
/// 并进当天的 UTC 桶，只读不抬基线）。原注释写着「不含尚未 flush 的增量（最多落后 60s）、
/// 面板应与 `get_token_usage` 配合使用」，**那是过时的**：`UsagePage` 从来没那么配合过，
/// 而实现早就在后端并好了。留着它的代价很具体 —— 排查「用量为什么不实时」的人会去改
/// 60s 的落盘周期，而那条路一点用都没有（陈旧发生在前端的刷新接线上）。
#[tauri::command]
pub fn get_daily_usage(state: tauri::State<AppState>) -> Vec<crate::model::DailyUsageBucket> {
    state.store.daily_usage_buckets()
}

/// 按「分类 × Key」聚合的用量 **+ 成本估算**。行构造与「为什么算不出」的成因判定
/// 都在 [`crate::usage_cost`]（那边是可测的纯逻辑，这里只做 IPC 边界）。
#[tauri::command]
pub fn get_usage_with_cost(state: tauri::State<AppState>) -> Vec<crate::usage_cost::UsageCostRow> {
    crate::usage_cost::rows(&state.store)
}

/// 按天的估算花费（「今日 / 本周 / 本月」与趋势图）。与 `get_usage_with_cost` 同一套按模型估价。
#[tauri::command]
pub fn get_daily_cost(state: tauri::State<AppState>) -> Vec<crate::usage_cost::DailyCost> {
    crate::usage_cost::daily_costs(&state.store)
}

/// 删掉一条**已不在配置里**的 Key 的全部用量记录（用量页「（已删除）」行的清理按钮）。不可逆。
///
/// 只认已离开配置的 Key：还在用的 Key 要删用量，正确的动作是删 Key 本身（那一步会一并删用量）。
/// 返回 `false` = 本次只从界面上去掉了，磁盘上没删成（只读 / 写盘失败），重启后可能还在。
#[tauri::command]
pub fn purge_key_usage(state: tauri::State<AppState>, key_id: String) -> crate::error::AppResult<bool> {
    if key_id.trim().is_empty() || state.store.get_key(&key_id).is_some() {
        return Err(crate::error::AppError::Invalid(
            "只能清理已删除 Key 的用量记录；要删除仍在使用的 Key 的记录，请删除该 Key 本身".into(),
        ));
    }
    Ok(crate::usage_cost::purge_key(&state.store, &key_id))
}

/// 单价表现状：内置表的**核对日期** + 在线表（`pricing::online`）有几个模型、何时从哪拉的、
/// 最近一次拉取是否失败。
///
/// 界面要显示它：金额是估算，取决于用的是哪张表、表有多旧。给用户这些，他就能自己判断
/// 「这个估算值得信几分」，而不是把它当账单。
#[tauri::command]
pub fn get_pricing_status() -> crate::pricing::online::PricingStatus {
    crate::pricing::online::status()
}

#[cfg(test)]
mod tests {
    /// 新命令必须进 `generate_handler!`。策略门 `invoke-command-must-exist` 只查正向（前端调的名字
    /// 在 Rust 有 `#[tauri::command]`）；反向漏注册只在用户打开用量页 / 点「清理」时才炸成 not found。
    #[test]
    fn the_new_usage_commands_are_registered() {
        let lib = include_str!("lib.rs");
        let handler = &lib[lib.find("generate_handler![").expect("lib.rs 里应有 generate_handler!")..];
        for cmd in ["usage_commands::get_daily_cost", "usage_commands::purge_key_usage", "usage_commands::get_pricing_status"] {
            assert!(handler.contains(cmd), "{cmd} 没进 generate_handler!：用量页会报 not found");
        }
    }
}
