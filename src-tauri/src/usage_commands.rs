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
/// 不含尚未 flush 的增量（最多落后 60s）：面板把这份历史与 `get_token_usage`
/// 的实时总量配合使用，故这点延迟不会让「今日」看起来停滞。
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

/// 内置单价表的**核对日期**（`YYYY-MM-DD`）。
///
/// 界面要显示它：这张表是人工核对各厂商定价页得来的，会随时间变旧，而变旧的表现是
/// 金额悄悄偏离真实账单。给用户一个日期，他就能自己判断「这个估算值得信几分」，
/// 而不是把它当账单。
#[tauri::command]
pub fn get_pricing_table_date() -> &'static str {
    crate::pricing::PRICE_TABLE_VERIFIED_ON
}
