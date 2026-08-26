//! 用量快照（`usage.json`）的**读取**与落点解析。
//!
//! 从 store.rs 抽出来的：这两个函数与 `Store` 的内部状态无关（入参一个路径，
//! 出参一份纯数据），而 store.rs 的棘轮余量为 0。写侧（`flush_usage_if_dirty`）
//! 仍留在 store.rs —— 它要读写 `usage_totals` / `usage_baseline` 两把锁，搬不动。
//!
//! ⚠️ 这里最要紧的语义是 `read_only`：磁盘上那份是**更新的**格式时，
//! 本次运行一个字节都不许写回，否则旧程序会把新维度永久抹掉（用量是纯累加值，
//! 没有别处可恢复）。三种降级的写回权限刻意不同，别合并成一个分支。

use crate::model::{CategoryType, TokenUsage, UsageSnapshot, USAGE_SNAPSHOT_VERSION};
use std::path::{Path, PathBuf};
/// `load_usage` 的结果。
///
/// 用具名结构体而不是元组：三个字段里有两个是 `i64`/`bool` 这种「位置一错就静默出错」
/// 的类型，元组解构写反了编译器不会拦。`read_only` 尤其不能错 —— 它反了就等于
/// 把版本门变成破坏源。
pub(crate) struct UsageLoad {
    pub(crate) totals: std::collections::BTreeMap<(CategoryType, String), TokenUsage>,
    pub(crate) since: i64,
    /// 磁盘上那份文件**比本程序认识的格式更新**，本次运行只读不写。
    pub(crate) read_only: bool,
    /// 已落盘的按日分桶（v2）。v1 文件读出来是空的 —— 它没有日期维度，
    /// 那部分历史只体现在 `totals` 里，无法反推每天各花了多少（如实丢弃日维度，
    /// 不编造一个假日期把整段历史堆到某一天）。
    pub(crate) daily_buckets: Vec<crate::model::DailyUsageBucket>,
    /// 已被 90 天滚动淘汰的桶的累计（v3）。参与 `totals`，但不进按日视图。
    /// 见 `UsageSnapshot::retired`：没有它，累计总量每过一个 90 天就往下掉一截。
    pub(crate) retired: Vec<crate::model::TokenUsageByKey>,
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
        // 返回空 map 后第一个请求就会 mark_usage_dirty，60s 后的 flush 会拿
        // 「空累加器 + version=1」覆写这个更高版本的文件，正好干成它要防的事。
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
    let mut retired: std::collections::BTreeMap<(CategoryType, String), TokenUsage> =
        std::collections::BTreeMap::new();
    for row in snap.retired.iter() {
        retired
            .entry((row.category_id, row.key_id.clone()))
            .or_default()
            .add(&row.usage);
    }
    // v1 兼容：`entries` 非空 = 读到的是 v1 文件（v2+ 写出时该字段恒空）。
    for row in &snap.entries {
        retired
            .entry((row.category_id, row.key_id.clone()))
            .or_default()
            .add(&row.usage);
    }
    for ((cat, kid), u) in retired.iter() {
        map.entry((*cat, kid.clone())).or_insert_with(TokenUsage::default).add(u);
    }
    let retired: Vec<crate::model::TokenUsageByKey> = retired
        .into_iter()
        .map(|((cat, kid), u)| crate::model::TokenUsageByKey {
            category_id: cat,
            key_id: kid,
            usage: u,
        })
        .collect();

    // v2：合并所有 daily_buckets 里的 entries
    if snap.version >= 2 {
        for bucket in &snap.daily_buckets {
            for row in &bucket.entries {
                map.entry((row.category_id, row.key_id.clone()))
                    .or_insert_with(TokenUsage::default)
                    .add(&row.usage);
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
