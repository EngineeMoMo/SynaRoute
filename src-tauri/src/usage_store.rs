//! 用量快照（`usage.json`）的**格式、读取与落点**；内存里的计数器在 [`ledger`]。
//!
//! 快照类型（`TokenUsageByKey` / `UsageSnapshot` / `DailyUsageBucket`）原在 model.rs，
//! 2026-10-12 加模型维度时搬到这里（model.rs 棘轮余量为 1）；model.rs 留了 `pub use`，
//! 既有的 `crate::model::…` 路径全部不变。
//!
//! ⚠️ 这里最要紧的语义是 `read_only`：磁盘上那份是**更新的**格式时，
//! 本次运行一个字节都不许写回，否则旧程序会把新维度永久抹掉（用量是纯累加值，
//! 没有别处可恢复）。三种降级的写回权限刻意不同，别合并成一个分支。

use crate::model::{CategoryType, TokenUsage};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[path = "usage_ledger.rs"] mod ledger; // 内存计数器 + 落盘 + 删除；来由见该文件模块注释
pub(crate) use ledger::UsageLedger;

/// 计数器的键：`(分类, key_id, 真实模型名)`。模型名空串 = v3 及更早的历史行（没记模型）。
pub(crate) type UsageKey = (CategoryType, String, String);

/// 用量统计：按「分类 × Key × 模型」聚合的一行。
///
/// `model` 是 v4 新增的维度（2026-10-12）。没有它时一条 Key 只能按一个「代表模型」
/// 估价：实测一条 Key 86% 的请求跑的是 $10/$50 的模型，却按 $0.20/$1.20 的代表模型算，
/// 输入低估约 50 倍。**单价表本身没错，是账本少了这一维。**
///
/// 空串的两种含义（都不是「未知模型」，别拿空串去查价）：v3 及更早的历史行；
/// 以及按 Key 汇总的视图（`UsageLedger::by_key`）里本来就不区分模型。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsageByKey {
    pub category_id: CategoryType,
    /// 空串 = 该分类的系统级事件（无具体 Key）。
    pub key_id: String,
    /// 上游真实模型名（映射之后的那个，计费按它）。空串见类型文档。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model: String,
    pub usage: TokenUsage,
}

impl TokenUsageByKey {
    pub(crate) fn slot(&self) -> UsageKey {
        (self.category_id, self.key_id.clone(), self.model.clone())
    }
    pub(crate) fn from_slot((category_id, key_id, model): UsageKey, usage: TokenUsage) -> Self {
        Self { category_id, key_id, model, usage }
    }
}

/// 用量累计的**落盘快照**（`usage.json`）。
///
/// 刻意与 `config.json` 分开成独立文件，不塞进 `AppConfig`：
/// - 用量是**运行期遥测**，不是用户配置。混进去会让「每分钟落一次用量」变成
///   「每分钟重写一次用户的全部 Key 与设置」——把一份只该增长计数器的写入，
///   放大成对用户最宝贵数据的反复覆写，任何一次坏写都可能带走 Key 配置。
/// - 体积与节奏都不同：config 是事件驱动、~20KB；usage 是定时、通常几百字节。
///
/// 版本沿革：v1 = 全局 `entries`；v2 = 按日分桶 + 90 天滚动；v3 = `retired`；
/// v4 = 每行多一个 `model`，且日桶按**本机时区**切天（见 [`local_date_string`]）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageSnapshot {
    #[serde(default = "usage_snapshot_version")]
    pub version: u32,
    /// 统计起始时刻（毫秒时间戳）。首次创建时写入，之后原样保留 ——
    /// 面板据此显示「统计自 X 起」，否则一个只增不减的数字用户无从判断它覆盖多长时间。
    #[serde(default)]
    pub since_ms: i64,
    /// 最后一次落盘时刻，仅供人工排查「这份文件是不是卡住不更新了」。
    #[serde(default)]
    pub updated_ms: i64,
    /// 按日期分桶的用量，最多保留 90 天，降序排列（最新的在前）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub daily_buckets: Vec<DailyUsageBucket>,
    /// **v3 数据**：已被 90 天滚动淘汰的桶的累计。
    ///
    /// 为什么必须有：`daily_buckets` 只留 90 天，而启动时的「累计总量」= 各存活桶之和。
    /// 于是每有一个桶过期，**下次启动读出来的累计就变小**——一个「累计用量」面板越用
    /// 数字越少，用户据此估额度会严重低估。淘汰桶的量在删除前折进这里，故
    /// **累计总量 = retired + 各存活桶之和**，单调不减；按日视图仍只看 `daily_buckets`。
    /// 日维度如实丢弃（不编造一个假日期把整段历史堆到某天）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub retired: Vec<TokenUsageByKey>,
    /// **v1 兼容字段**：v2 起写出时留空，只在读 v1 文件时有值（读入后归进 `retired`）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entries: Vec<TokenUsageByKey>,
}

/// 一天的用量分桶。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DailyUsageBucket {
    /// 日期 `"YYYY-MM-DD"`。v4 起按**本机时区**切天；v3 及更早写下的桶是 UTC 日期，
    /// 原样保留、不重新切分（拿不到逐笔时刻，切不了）—— 故升级那一天前后最多有
    /// 「时区差」那么几个小时的用量落在相邻那天，写明免得被当 bug 重查。
    pub date: String,
    /// 该天内按 `(分类, key_id, 模型)` 聚合的用量。Vec 而非 map：JSON 的 map 键只能是字符串。
    pub entries: Vec<TokenUsageByKey>,
}

/// `usage.json` 的当前格式版本。**读写共用这一个常量**，别两边各写字面量 ——
/// 那样改了写侧忘了改读侧，版本门就会把自己刚写出去的文件判为"来自未来"。
///
/// 改动规则：只有当**旧程序按旧结构解析新文件会得到错误结果**时才递增。
/// - v1→v2：v1 程序会忽略 `daily_buckets`，读出空 `entries` 再写回 → 历史清零。
/// - v2→v3：v2 程序会忽略 `retired`，写回后 90 天以前那段**永久消失**。
/// - v3→v4：v3 程序会忽略 `model`，把同一 Key 的各模型行并成一行 —— 总量对，
///   但它下一次写回就把**模型维度抹掉了**，而那正是 v4 存在的全部理由（抹掉之后
///   金额退回「按代表模型估」的老错误，且不可恢复）。代价：用户若再开旧版本，
///   那次运行用量页只读不写（版本门），升级回来即恢复。
pub const USAGE_SNAPSHOT_VERSION: u32 = 4;

fn usage_snapshot_version() -> u32 {
    USAGE_SNAPSHOT_VERSION
}

/// 毫秒时间戳 → **本机时区**的日期 `"YYYY-MM-DD"`。分桶、「今日」、90 天窗口的唯一判据。
///
/// 🔴 v3 及更早用 UTC 切天，于是 UTC+8 的用户每天 0~8 点的用量算在「昨天」——
/// 「今日花费」一天里有三分之一的时间是错的。写盘（flush）、读视图（并入当天的未落盘增量）、
/// 滚动删桶三处**必须**都走这一个函数：任何一处退回 UTC，零点前后就会出现两个「今日」。
pub(crate) fn local_date_string(ms: i64) -> String {
    date_string_at(ms, &chrono::Local)
}

/// [`local_date_string`] 的可测形态（测试里传固定时区，结果不随跑测试的机器变）。
fn date_string_at<Tz: chrono::TimeZone>(ms: i64, tz: &Tz) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let utc = chrono::DateTime::from_timestamp_millis(ms).unwrap_or_default();
    utc.with_timezone(tz).format("%Y-%m-%d").to_string()
}
/// `load_usage` 的结果。
///
/// 用具名结构体而不是元组：三个字段里有两个是 `i64`/`bool` 这种「位置一错就静默出错」
/// 的类型，元组解构写反了编译器不会拦。`read_only` 尤其不能错 —— 它反了就等于
/// 把版本门变成破坏源。
pub(crate) struct UsageLoad {
    /// v3 及更早的行读进来时 `model` 为空串（那时没记模型），见 [`TokenUsageByKey`]。
    pub(crate) totals: std::collections::BTreeMap<UsageKey, TokenUsage>,
    pub(crate) since: i64,
    /// 磁盘上那份文件**比本程序认识的格式更新**，本次运行只读不写。
    pub(crate) read_only: bool,
    /// 已落盘的按日分桶（v2）。v1 文件读出来是空的 —— 它没有日期维度，
    /// 那部分历史只体现在 `totals` 里，无法反推每天各花了多少（如实丢弃日维度，
    /// 不编造一个假日期把整段历史堆到某一天）。
    pub(crate) daily_buckets: Vec<DailyUsageBucket>,
    /// 已被 90 天滚动淘汰的桶的累计（v3）。参与 `totals`，但不进按日视图。
    /// 见 `UsageSnapshot::retired`：没有它，累计总量每过一个 90 天就往下掉一截。
    pub(crate) retired: Vec<TokenUsageByKey>,
}

/// `usage.json` 的路径：与 `config.json` 同目录。
///
/// 跟着 config 走而不是另找一个「数据目录」，是为了继承 config 已经确立的落点语义 ——
/// Windows 上 `%APPDATA%\SynaRoute\` 受 MSIX 虚拟化影响、mac 上是
/// `~/Library/Application Support/SynaRoute/`。两个文件同目录，用户备份/迁移时
/// 不会只带走一个。
/// `usage.json` 的落点：**跟着 `config.json` 走**，不是跟着日志走。
///
/// 这是刻意的分野，别「顺手统一」：日志与 `mcp-port` 放 exe 同级是为了绕开 MSIX 的 AppData
/// 虚拟化（要让「用户双击启动的实例」和「包内进程启动的实例」看到同一份文件）；用量属**用户数据**，
/// 就该跟 `config.json`/`secrets.enc` 同域、同一套备份与导入导出边界。
/// 代价是它同样会被虚拟化 —— 与 config 一致，符合预期，不是缺陷。
pub(crate) fn usage_file_path(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .map(|d| d.join("usage.json"))
        .unwrap_or_else(|| PathBuf::from("usage.json"))
}

/// 启动时读取用量累计。
///
/// **一律不上抛错误**：文件缺失（全新安装）、内容损坏（断电写坏）都只是「没有历史累计」，
/// 绝不能因为一个统计文件读不出来就让应用起不来。损坏时落 warn 并从空开始，
/// 下一次 flush 会用好的内容覆盖它。
///
/// **v2 改动（2026-08-11）**：读到 v2 文件时把所有 `daily_buckets` 里的 entries
/// 合并进内存累加器,这样重启后历史累计不会丢。v1 文件继续按老逻辑读 `entries`。
pub(crate) fn load_usage(path: &Path) -> UsageLoad {
    let now = chrono::Utc::now().timestamp_millis();
    // 三种降级，**写回权限不同**（别合并成一个分支）：
    // - 文件缺失（全新安装）：从零累计，允许写回；
    // - 内容解析失败（断电写坏）：从零累计，允许写回 —— 用好数据覆盖坏数据是对的；
    // - **读失败**（杀软/备份/同步盘独占、ACL 抖动）：从零累计但**禁止写回**。
    //   文件极可能完好，只是这一瞬打不开；若允许写回，60s 后一次 flush 就用空日桶
    //   把最多 90 天历史覆盖掉。详见 `UsageLoad::fresh_read_only`。
    let raw = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return UsageLoad::fresh(now),
        Err(e) => {
            tracing::error!(
                "读取用量统计失败（本次从零累计，且**本次运行不写回**以免覆盖磁盘上完好的历史；\
                 重启读取成功即恢复）: {e} · 路径={path:?}"
            );
            return UsageLoad::fresh_read_only(now);
        }
    };
    let snap: UsageSnapshot = match serde_json::from_slice(&raw) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("用量统计文件解析失败（本次从零开始累计）: {e}");
            return UsageLoad::fresh(now);
        }
    };
    // 版本门：只认自己认识的格式。
    //
    // `version` 若只写不读，就是个**假的**兼容位 —— 将来把 entries 改成按日分桶后，
    // 旧版本程序（比如用户降级、或两台机器同步了这个文件）会用旧结构去解析新文件：
    // serde 对未知字段默认忽略、缺失字段取 default，于是**解析"成功"但内容是空的**，
    // 紧接着下一次 flush 就用空数据把用户攒了几个月的累计覆盖掉。
    // 宁可这一次不显示统计（返回空、且**不动原文件**），也不能销毁数据。
    if snap.version > USAGE_SNAPSHOT_VERSION {
        tracing::warn!(
            "用量统计文件版本为 {}，本程序只认到 {} —— 跳过本次累计并保留原文件，\
             不做解析也不覆盖（避免用旧结构解析新格式后把历史清零）",
            snap.version,
            USAGE_SNAPSHOT_VERSION
        );
        // 起算时刻仍沿用文件里的：面板显示的起点是对的，只是这次没读到明细。
        let since = if snap.since_ms > 0 { snap.since_ms } else { now };
        // read_only = true：**必须**同时禁掉写回，否则这道门自己就是破坏源 ——
        // 返回空 map 后第一个请求就会把账本标脏，60s 后的 flush 会拿
        // 「空累加器 + 旧 version」覆写这个更高版本的文件，正好干成它要防的事。
        return UsageLoad {
            totals: std::collections::BTreeMap::new(),
            since,
            read_only: true,
            daily_buckets: Vec::new(),
            retired: Vec::new(),
        };
    }

    let mut map = std::collections::BTreeMap::new();

    // v3：先把「已淘汰桶的累计」垫进底。
    //
    // 累计总量 = retired + 各存活桶之和。没有这一段，每有一个桶过 90 天被删，
    // 下次启动读出来的累计就少一截 —— 一个「累计用量」面板越用数字越小，
    // 用户据此估额度会严重低估。与当年「按事件环算总量」是同症状、不同成因。
    //
    // **v1 的 `entries` 也归到这里**：那是「过去某段时间的累计」、没有日期维度，
    // 与「已淘汰的桶」是同一种数据（有总量、无日维度），`retired` 正是它的归宿。
    // 此前它只进内存累加器、不落任何桶，于是**重启一次就永久消失**
    // （旧测试把这个损失当成既定行为记着，理由是「无从得知属于哪天」——
    // 那个理由只否定「造一个假日期」，不构成「必须丢掉总量」）。
    let mut retired: std::collections::BTreeMap<UsageKey, TokenUsage> =
        std::collections::BTreeMap::new();
    // v1 兼容：`entries` 非空 = 读到的是 v1 文件（v2+ 写出时该字段恒空）。
    for row in snap.retired.iter().chain(&snap.entries) {
        retired.entry(row.slot()).or_default().add(&row.usage);
    }
    for (slot, u) in retired.iter() {
        map.entry(slot.clone()).or_insert_with(TokenUsage::default).add(u);
    }
    let retired: Vec<TokenUsageByKey> =
        retired.into_iter().map(|(slot, u)| TokenUsageByKey::from_slot(slot, u)).collect();

    // v2：合并所有 daily_buckets 里的 entries
    if snap.version >= 2 {
        for bucket in &snap.daily_buckets {
            for row in &bucket.entries {
                map.entry(row.slot()).or_insert_with(TokenUsage::default).add(&row.usage);
            }
        }
    }

    // v1 兼容：`entries` 已在上面并进 `retired` 并计入 `map`，这里不能再加一次
    // （否则 v1 迁移那次启动的累计会翻倍）。

    // since_ms 为 0 = 旧版本文件或被手工清空过，退回「现在」而不是 1970，
    // 否则面板会显示「统计自 1970-01-01 起」这种明显错误的起始时间。
    let since = if snap.since_ms > 0 { snap.since_ms } else { now };
    UsageLoad {
        totals: map,
        since,
        read_only: false,
        daily_buckets: snap.daily_buckets,
        retired,
    }
}

impl UsageLoad {
    /// 「没有历史可继承，但可以正常写回」—— 全新安装 / 文件**内容已损坏**走这个。
    ///
    /// 注意不含「读失败」：那条走 [`Self::fresh_read_only`]，理由见那里。
    fn fresh(now: i64) -> Self {
        Self {
            totals: std::collections::BTreeMap::new(),
            since: now,
            read_only: false,
            daily_buckets: Vec::new(),
            retired: Vec::new(),
        }
    }

    /// 「没有历史可继承，且**本次不许写回**」—— 读文件失败（非 NotFound）走这个。
    ///
    /// 为什么必须与 [`Self::fresh`] 分开：读失败与解析失败是两件不同的事。
    /// - **解析失败** = 内容确已损坏（断电写坏），用好数据覆盖它是对的；
    /// - **读失败** = 文件很可能**完好无损**，只是这一瞬间打不开：Windows 上杀软扫描、
    ///   备份程序、OneDrive 同步都会短暂独占（ERROR_SHARING_VIOLATION），ACL 抖动则是
    ///   ACCESS_DENIED。此时若按「从零累计且允许写回」处理，60 秒后第一次 flush 就用
    ///   空日桶把用户最多 90 天的用量历史整份覆盖掉 —— 文件本来是好的，是我们自己毁了它，
    ///   且不可自愈（用量是纯累加值，没有别处可恢复）。
    ///
    /// 这与「文件版本比我新」那道门是**同一条破坏链**，只是入口不同，故复用同一套
    /// 自我保护：本次运行照常记账（面板显示从零开始），但一个字节都不写回磁盘，
    /// 下次启动读成功即恢复全部历史。
    fn fresh_read_only(now: i64) -> Self {
        Self {
            totals: std::collections::BTreeMap::new(),
            since: now,
            read_only: true,
            daily_buckets: Vec::new(),
            retired: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 🔴 切天按时区，不按 UTC。用固定时区测，结果不随跑测试的机器变。
    ///
    /// 2026-10-12 00:30（UTC+8）= 2026-10-11 16:30 UTC。按 UTC 切会落在 10-11 ——
    /// 那就是 v3 的老缺陷：东八区每天 0~8 点的用量算进「昨天」。
    #[test]
    fn day_boundary_follows_the_time_zone_not_utc() {
        let ms = chrono::DateTime::parse_from_rfc3339("2026-10-12T00:30:00+08:00")
            .unwrap()
            .timestamp_millis();
        let east8 = chrono::FixedOffset::east_opt(8 * 3600).unwrap();
        assert_eq!(date_string_at(ms, &east8), "2026-10-12", "东八区零点半已经是新的一天");
        assert_eq!(date_string_at(ms, &chrono::Utc), "2026-10-11", "同一时刻在 UTC 还是前一天");
        let west5 = chrono::FixedOffset::west_opt(5 * 3600).unwrap();
        assert_eq!(date_string_at(ms, &west5), "2026-10-11");
    }

    /// v3 文件（行上没有 `model`）照常读入，模型维度为空串 —— 历史行不丢、不编造模型名。
    #[test]
    fn a_v3_row_without_a_model_loads_as_a_legacy_row() {
        let row: TokenUsageByKey = serde_json::from_str(
            r#"{"categoryId":"claude-cli","keyId":"k","usage":{"input":5,"output":1}}"#,
        )
        .unwrap();
        assert_eq!(row.model, "", "v3 行没有 model 字段 → 空串（历史行）");
        // 空串不写出去：v4 文件里历史行与 v3 时长得一样，按 Key 汇总的 IPC 输出也不多一个空字段。
        let back = serde_json::to_string(&row).unwrap();
        assert!(!back.contains("model"), "空模型名不该序列化出来: {back}");
    }
}
