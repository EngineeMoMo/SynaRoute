//! 「Key 删了，它的历史花费还得算得出来」—— 算金额所需事实的快照。
//!
//! 挂在 [`crate::usage_cost`] 下（`#[path]`），落点 `usage-keys.json`，
//! 与 `usage.json` / `config.json` 同目录（理由同 `usage_store::usage_file_path`：
//! 用量属**用户数据**，该跟 config 同域、同一套备份与导入导出边界）。
//!
//! # 它补的洞
//!
//! `usage_cost::rows()` 算金额要两样东西，而两样都**只存在于 `ProviderKey` 里**：
//! 代表模型（`default_model` 或 `models[0]`）与计费倍率（`cost_multiplier`）。
//! 用量本身是纯累加值、Key 删了也保留，于是删一条 Key 之后：
//! 它那些行的金额列永远显示「—」，成因写着 `KeyDeleted`、可行动项写着「无」。
//!
//! 用户的原话是「key 删除了用量要可看，价格应该是算好的」—— 这是对的：
//! **花掉的钱不该因为删了一条配置就从统计里消失。**
//!
//! # 🔴 存「事实」而不是存「金额」
//!
//! 两种都能让金额活下来，选前者的理由：单价表会被修正（本仓的表曾经整体偏 3 倍，
//! `opus` 用的是退役价），存死金额意味着**那批历史永远错着**，而存模型名+倍率
//! 则会随表的修正一起变准。而且它与「Key 还在」走的是同一套 `estimate_cost`，
//! 不会出现「删之前和删之后金额不一样」这种自相矛盾。
//!
//! # 🔴 刻意在**读**路径上顺带维护，而不是在删除那一刻写
//!
//! 直觉的挂点是 `Store::delete_key`，但 `store.rs` 棘轮余量为 0，而这件事
//! 本身不需要那个位置：只要「Key 还活着的时候」把事实记下来，删除之后那条记录
//! 自然就是墓碑。故 [`sync`] 挂在 `usage_cost::rows()`（用量页每次刷新都走它）。
//!
//! **只在内容真变了时才写盘**：`rows()` 被前端轮询调用，而 Key 的名字/代表模型/
//! 倍率极少变 —— 无脑写会让一个开着用量页的窗口每秒制造一次写入，
//! 与 `usage_dirty` 那条「空闲的应用一个字节都不写盘」纪律冲突。
//!
//! # 已知边界（写明免得被当 bug 重查）
//!
//! **本功能上线之前就已经删掉的 Key，救不回来。** 它们的模型名与倍率在删除那一刻
//! 就不在任何地方了 —— 不编造一个模型名去凑一个金额（那会给用户一个看起来精确、
//! 实际凭空捏造的数字，比「—」糟得多）。那些行的 token 数仍然计入总量。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// 表内条目上限。key_id 是有限集，正常远小于它；上限只为堵住
/// 「配置被反复重建导致 id 无限增长」这类只增不减的泄漏（同 `quota_window`）。
const MAX_ENTRIES: usize = 512;

/// 算一行金额需要的、来自 `ProviderKey` 的全部事实。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct KeyFacts {
    /// Key 的可读名。key_id 是 uuid，界面上没有它用户认不出这行是谁。
    pub(crate) name: String,
    /// 代表模型（`default_model` 优先，否则 `models[0].real_name`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) repr_model: Option<String>,
    /// 用户填的计费倍率原文（`None` = 没填，按 1.0 算）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) multiplier: Option<String>,
    /// 最后一次「这条 Key 还活着」的**那一天**（UTC 日首，毫秒）。超上限时按它淘汰最老的。
    ///
    /// 🔴 **按天取整不是偷懒，是为了让 [`sync`] 的「内容没变就不写」真的成立**：
    /// 用精确时刻的话每次读都会让这个字段变一点，于是 `merged == before` 永不成立、
    /// 用量页每轮询一次就写一次盘（第一版就是这样，被
    /// `repeated_reads_do_not_rewrite_the_tombstone_file` 当场抓住）。
    /// 取整到天之后：同一天内读多少次都不写，而活着的 Key 每天仍会刷新一次 ——
    /// 后者是淘汰策略的前提（活着的必须永远比墓碑新，否则会先被淘汰掉）。
    pub(crate) seen_day_ms: i64,
}

/// 今天的 UTC 日首（毫秒）。见 [`KeyFacts::seen_day_ms`] 为什么要取整。
pub(crate) fn today_ms(now_ms: i64) -> i64 {
    const DAY: i64 = 86_400_000;
    now_ms.div_euclid(DAY) * DAY
}

/// 落盘形态。带 `version` 是为了将来能加维度而不误读旧文件 ——
/// 同 `UsageSnapshot` 那条教训：只写不读的 `version` 是个**假的**兼容位。
#[derive(serde::Serialize, serde::Deserialize)]
struct Snapshot {
    version: u32,
    #[serde(default)]
    keys: BTreeMap<String, KeyFacts>,
}

const SNAPSHOT_VERSION: u32 = 1;

/// `usage-keys.json` 的路径：与 `config.json` 同目录。
pub(crate) fn file_path(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .map(|d| d.join("usage-keys.json"))
        .unwrap_or_else(|| PathBuf::from("usage-keys.json"))
}

/// 读表。**一律不上抛错误**：这是统计辅助数据，缺失/损坏只意味着
/// 「已删 Key 的金额算不出」，绝不能让用量页整个报错。
fn load(path: &Path) -> BTreeMap<String, KeyFacts> {
    let Ok(raw) = std::fs::read(path) else {
        return BTreeMap::new();
    };
    match serde_json::from_slice::<Snapshot>(&raw) {
        // 未来版本：只读不写这一条不适用（本文件可从活 Key 重建），但仍不解析
        // 它的未知维度 —— 按空表处理会让本次运行把它覆盖掉，故直接放弃合并。
        Ok(s) if s.version <= SNAPSHOT_VERSION => s.keys,
        Ok(s) => {
            tracing::warn!("usage-keys.json 版本 {} 高于本程序，本次不使用也不覆盖", s.version);
            BTreeMap::new()
        }
        Err(e) => {
            tracing::warn!("usage-keys.json 解析失败，按空表处理: {e}");
            BTreeMap::new()
        }
    }
}

/// 把「当前还活着的 Key 的事实」合并进表并返回合并结果。
///
/// - 活着的 Key **覆盖**同 id 的旧记录（名字/模型/倍率可能刚被用户改过）；
/// - 表里其余条目**原样保留** —— 那些正是墓碑，本函数的全部意义所在；
/// - 只在内容真的变化时写盘（见模块头）；写失败只记 warn。
pub(crate) fn sync(
    config_path: &Path,
    live: BTreeMap<String, KeyFacts>,
) -> BTreeMap<String, KeyFacts> {
    let path = file_path(config_path);
    let before = load(&path);
    let mut merged = before.clone();
    merged.extend(live);
    if merged.len() > MAX_ENTRIES {
        // 淘汰最老的（按 seen_day_ms 升序）。活着的 Key 每天都会刷新它，
        // 所以被淘汰的天然是最老的墓碑，而不是正在用的 Key。
        let mut by_age: Vec<(String, i64)> =
            merged.iter().map(|(k, v)| (k.clone(), v.seen_day_ms)).collect();
        by_age.sort_by_key(|(_, ms)| *ms);
        for (id, _) in by_age.into_iter().take(merged.len() - MAX_ENTRIES) {
            merged.remove(&id);
        }
    }
    if merged == before {
        return merged;
    }
    let snap = Snapshot { version: SNAPSHOT_VERSION, keys: merged.clone() };
    match serde_json::to_vec_pretty(&snap) {
        Ok(bytes) => {
            if let Err(e) = std::fs::write(&path, bytes) {
                tracing::warn!("写 usage-keys.json 失败（已删 Key 的金额将算不出）: {e}");
            }
        }
        Err(e) => tracing::warn!("序列化 usage-keys.json 失败: {e}"),
    }
    merged
}
