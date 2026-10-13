//! 用量计数器：内存累计 + 按日分桶 + 周期落盘 + 按 Key 删除。挂在 `usage_store` 下（`#[path]`）。
//!
//! 原先是 `Store` 上的 9 个字段 + 3 把锁的纪律，散在 store.rs 里（冻结、余量 0）。
//! 2026-10-12 加模型维度时整体搬出来：键从 `(分类, key_id)` 变成 `(分类, key_id, 模型)`，
//! 而那 9 个字段的读写散在 store.rs 的五六个函数里，没有地方容纳这一维。
//!
//! # 为什么要按模型记
//!
//! 一条 Key 的金额此前只能按它的「代表模型」估（`default_model` 或 `models[0]`）。
//! 实测用户一条 Key 86% 的请求跑的是 $10/$50 的模型，代表模型却是 $0.20/$1.20 的那个 →
//! 输入低估约 50 倍；另一条落到家族兜底价，缓存读高估 2.5 倍。**表没错，是账本少了一维。**
//!
//! # 三个数各是什么（写错会算出天文数字）
//!
//! - `totals` = **跨全部历史的总量**，热路径只往里累加，永不按天切分；
//! - `buckets` = 每天各是多少（今日 / 本周 / 本月 / 趋势）；
//! - `baseline` = 上一次**成功落盘**那一刻的 `totals`。
//!
//! 未落盘增量 = `totals − baseline`。**不能**把 `totals` 整份写进当天的桶 ——
//! 那是把历史总量当成当天消耗（实测踩过：v1 的 500 与当天新增的 200 相加成了 700）。
//!
//! # 🔴 锁顺序：`buckets → retired → totals → baseline`
//!
//! 要同时持有其中两把以上的地方，一律按这个顺序取（`write_locked` / `purge_key` /
//! `daily_buckets`）。反过来取一次就可能与 flush 交叉死锁 —— 而死锁在转发热路径上的
//! 表现是「所有请求卡住」，日志里一个字都没有。加新方法时照这个顺序。
//!
//! 写盘期间**不持** `totals` 的锁：先拍快照、放锁、再写盘，成功后把 `baseline` 抬到**快照**
//! （不是抬到当时的 `totals`）。原实现整个写盘期间持着 `totals` 的读锁，于是转发热路径上的
//! `record` 要等 `atomic_write`（含 fsync 与最多 ~1.2s 的退避）；而快照之后到来的那几笔
//! 留在 `totals − baseline` 里，下一轮照常落盘，一笔都不会丢。

use super::{
    local_date_string, DailyUsageBucket, TokenUsageByKey, UsageKey, UsageSnapshot,
    USAGE_SNAPSHOT_VERSION,
};
use crate::model::{CategoryType, TokenUsage};
use parking_lot::RwLock;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// 按日视图保留多少天（含今天）。更早的桶折进 `retired`，见 [`retire_expired`]。
const KEEP_DAYS: i64 = 90;

/// 用量账本。`Store` 上只有这一个字段（`store.usage`）。
pub(crate) struct UsageLedger {
    /// 跨重启累计的总量。**刻意与事件环分开存**：事件环满 MAX_EVENTS 条即丢最老的，
    /// 算在环上的话累计值在第 MAX_EVENTS 次请求之后不再增长 —— 一个「累计用量」面板
    /// 越用数字越小，用户据此估额度会严重低估。
    pub(crate) totals: RwLock<BTreeMap<UsageKey, TokenUsage>>,
    /// 有未落盘的变更。有它，**空闲的应用一个字节都不写盘**（用户明确关心过 SSD 寿命）。
    pub(crate) dirty: AtomicBool,
    /// 上一次「必须写」的落盘（删 Key 的用量）失败了：下一轮 flush 即使增量为空也要写。
    /// 没有它，删除只发生在内存里 —— 增量是空的，flush 永远不写，重启后被删的行原样复活。
    pub(crate) must_rewrite: AtomicBool,
    /// `usage.json` 的路径（与 `config.json` 同目录）。存字段而不是每次现推：读写两处各推
    /// 一次就是漂移的温床（`mcp.rs` 的端口文件正因此踩过）。
    path: PathBuf,
    /// 统计起始时刻（毫秒），随 `usage.json` 持久化。启动时定一次，运行期不变。
    since_ms: i64,
    /// 磁盘上的 `usage.json` 比本程序新（或这次读不出来）→ 本次运行**只读不写**。
    ///
    /// 没有这个开关，版本门就是自欺：它返回空累加器保护了「读」，但第一个请求就会标脏，
    /// 随后的 flush 拿空数据 + 旧 version 覆写那个新格式文件，用户攒的历史当场清零。
    pub(crate) read_only: bool,
    /// 按日分桶的**已落盘历史**（不含尚未 flush 的增量），降序。
    pub(crate) buckets: RwLock<Vec<DailyUsageBucket>>,
    /// 已被 90 天滚动淘汰的桶的累计。只在 flush / 删除里改，不进按日视图。
    pub(crate) retired: RwLock<Vec<TokenUsageByKey>>,
    /// 上一次成功落盘时的 `totals`（见模块头「三个数」）。
    pub(crate) baseline: RwLock<BTreeMap<UsageKey, TokenUsage>>,
    /// 上一次落盘落在哪天。**只是诊断信息，不参与分桶** —— 分桶看的是落盘那一刻的日期。
    pub(crate) baseline_date: RwLock<String>,
}

impl UsageLedger {
    /// 读 `config.json` 同目录的 `usage.json`。一律不失败（读不出来就从零开始，见 `load_usage`）。
    pub(crate) fn load(config_path: &Path) -> Self {
        let path = super::usage_file_path(config_path);
        let loaded = super::load_usage(&path);
        Self {
            // 基线 = 刚加载的历史总量：本次运行的增量从此开始累计。
            baseline: RwLock::new(loaded.totals.clone()),
            totals: RwLock::new(loaded.totals),
            dirty: AtomicBool::new(false),
            must_rewrite: AtomicBool::new(false),
            path,
            since_ms: loaded.since,
            read_only: loaded.read_only,
            buckets: RwLock::new(loaded.daily_buckets),
            retired: RwLock::new(loaded.retired),
            baseline_date: RwLock::new(local_date_string(chrono::Utc::now().timestamp_millis())),
        }
    }

    /// 记一笔用量。`model` 传上游真实模型名（映射之后、计费按它）；不知道就传 `""`。
    ///
    /// 只翻脏标记、不在这里落盘：转发热路径上的一次写盘要序列化整份快照再走
    /// `atomic_write`（含 fsync），合并给后台那趟 60s 的 flush 才是对的。
    pub(crate) fn record(&self, category: CategoryType, key_id: &str, model: &str, u: &TokenUsage) {
        let slot = (category, key_id.to_string(), model.trim().to_string());
        self.totals.write().entry(slot).or_default().add(u);
        self.mark_dirty();
    }

    pub(crate) fn mark_dirty(&self) {
        self.dirty.store(true, Ordering::Relaxed);
    }

    pub(crate) fn since_ms(&self) -> i64 {
        self.since_ms
    }

    /// 按「分类 × Key」汇总（各模型相加，`model` 为空串）。
    ///
    /// ⚠️ 返回的是**有过累计的桶**，不是配置里的全部 Key：正常采集刻意不建零桶，
    /// 一条没跑过请求的 Key 在这里没有条目。用量页要的并集在 `usage_cost::rows` 里做 ——
    /// 别在这里补零桶，它会污染 flush 的增量计算。
    pub(crate) fn by_key(&self) -> Vec<TokenUsageByKey> {
        let mut out: BTreeMap<(CategoryType, String), TokenUsage> = BTreeMap::new();
        for ((cat, kid, _), u) in self.totals.read().iter() {
            out.entry((*cat, kid.clone())).or_default().add(u);
        }
        out.into_iter()
            .map(|((cat, kid), u)| TokenUsageByKey::from_slot((cat, kid, String::new()), u))
            .collect()
    }

    /// 按「分类 × Key × 模型」的明细（估价用：每个模型按自己的单价算）。
    pub(crate) fn by_model(&self) -> Vec<TokenUsageByKey> {
        self.totals
            .read()
            .iter()
            .map(|(slot, u)| TokenUsageByKey::from_slot(slot.clone(), *u))
            .collect()
    }

    /// 按日分桶的只读快照，**已并入尚未落盘的增量**（落进本机时区的「今天」）。
    ///
    /// 必须在后端并：桶只在 flush 时更新（最多落后 60s），不并的话同一屏上「累计」涨了、
    /// 「今日」还是 0。**不动 baseline**：这是只读视图，抬了基线这段增量就再也不落盘了。
    ///
    /// 整段持着 `buckets` 的读锁：flush 在同一把锁的写侧里「并增量 + 抬基线」，于是这里读到的
    /// 落盘桶与增量一定出自同一侧 —— 否则 flush 恰好插在中间时，那段增量两边都不算（少一截）。
    pub(crate) fn daily_buckets(&self) -> Vec<DailyUsageBucket> {
        let guard = self.buckets.read();
        let pending = {
            let totals = self.totals.read();
            let baseline = self.baseline.read();
            pending_delta(&totals, &baseline)
        };
        let mut out = guard.clone();
        drop(guard);
        if !pending.is_empty() {
            merge_into_day(&mut out, &local_date_string(chrono::Utc::now().timestamp_millis()), pending);
        }
        out
    }

    /// 有未落盘变更就落盘一次并清标记（后台每 60s 一次 + 退出前一次）。返回是否真的写了盘。
    ///
    /// **先清标记再写**：写盘期间又有新消耗时，宁可下一轮多写一次，也不要形成
    /// 「标记已清、这次变更没落盘」的丢失窗口。只读判定**必须在清标记之前**，
    /// 否则标记被清掉、这段消耗既没写盘也不再重试。
    pub(crate) fn flush(&self) -> bool {
        if self.read_only {
            return false;
        }
        let forced = self.must_rewrite.swap(false, Ordering::Relaxed);
        if !self.dirty.swap(false, Ordering::Relaxed) && !forced {
            return false;
        }
        self.write_locked(forced)
    }
    /// 把增量并进今天的桶、滚掉过期的桶、整份写盘。`force` = 增量为空也写（删除之后）。
    ///
    /// 失败时内存里的桶与 `retired` **原样回退**、基线不抬 —— 下一轮重试的是同一批增量，
    /// 既不丢也不重复计。
    fn write_locked(&self, force: bool) -> bool {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let today = local_date_string(now_ms);
        let mut buckets = self.buckets.write();
        let mut retired = self.retired.write();
        // 快照之后立刻放掉 totals 的锁（见模块头「写盘期间不持 totals 的锁」）。
        let snapshot = self.totals.read().clone();
        let mut baseline = self.baseline.write();
        let delta = pending_delta(&snapshot, &baseline);
        // 增量为空 = 这次标脏没带来新消耗（或已被上一轮写掉）：不写盘。
        if delta.is_empty() && !force {
            return false;
        }
        let (buckets_before, retired_before) = (buckets.clone(), retired.clone());
        if !delta.is_empty() {
            merge_into_day(&mut buckets, &today, delta);
        }
        retire_expired(&mut buckets, &mut retired, &today);
        let snap = UsageSnapshot {
            version: USAGE_SNAPSHOT_VERSION,
            since_ms: self.since_ms,
            updated_ms: now_ms,
            daily_buckets: buckets.clone(),
            retired: retired.clone(),
            entries: Vec::new(), // v1 兼容字段，v2 起恒空
        };
        // 走与 config/secrets 同一个 atomic_write：临时文件 → fsync → rename。原子性来自 rename
        // （读者只会看到旧全量或新全量），持久性来自 rename 之前那次 fsync。
        let written = serde_json::to_vec_pretty(&snap)
            .map_err(crate::error::AppError::from)
            .and_then(|bytes| crate::secret::atomic_write(&self.path, &bytes));
        if let Err(e) = written {
            *buckets = buckets_before;
            *retired = retired_before;
            self.mark_dirty();
            if force {
                self.must_rewrite.store(true, Ordering::Relaxed);
            }
            tracing::warn!("用量统计落盘失败（下一轮重试）: {e}");
            return false;
        }
        *baseline = snapshot;
        *self.baseline_date.write() = today;
        true
    }
    /// 删掉某条 Key 的**全部**用量：各分类、各模型、每个日桶、`retired`，并**立刻落盘**。
    ///
    /// 🔴 2026-10-12 用户定的口径：「key 删除的时候对应的记录也可以删除了」。这推翻了
    /// 2026-09-02 的「删了 Key 用量仍可看、金额照算」（墓碑那一轮）。**不可逆**，只该由
    /// 「删除 Key」或用户在用量页上确认过的清理动作调用。
    ///
    /// 不等 60s 那趟 flush：删除是用户刚做的明确动作，拖到下一轮的话中途退出就会让被删的行
    /// 在重启后复活。写失败时置 `must_rewrite`，下一轮 flush 即使增量为空也重写。
    ///
    /// 返回 `true` = 磁盘上也已经没有它了；`false` = 只删了内存（本次运行只读、或写盘失败），
    /// 这时重启后它可能还在 —— 调用方据此决定是否告诉用户。
    pub(crate) fn purge_key(&self, key_id: &str) -> bool {
        let removed = {
            let mut buckets = self.buckets.write();
            let mut retired = self.retired.write();
            let mut totals = self.totals.write();
            let mut baseline = self.baseline.write();
            let before = totals.len() + retired.len() + buckets.iter().map(|b| b.entries.len()).sum::<usize>();
            // baseline 必须一起删：留着的话这条 Key 日后再有用量（同 id 重建），
            // 饱和减法会把新增量吃掉一截（`当前 − 旧基线` 被截成 0）。
            totals.retain(|(_, kid, _), _| kid != key_id);
            baseline.retain(|(_, kid, _), _| kid != key_id);
            retired.retain(|r| r.key_id != key_id);
            for b in buckets.iter_mut() {
                b.entries.retain(|r| r.key_id != key_id);
            }
            buckets.retain(|b| !b.entries.is_empty());
            let after = totals.len() + retired.len() + buckets.iter().map(|b| b.entries.len()).sum::<usize>();
            before != after
        };
        // 只读：内存里已删（本次运行的界面不再显示），磁盘一个字节都不许动。
        // **排在「删没删到」之前判**：读取失败时内存是空的，「什么都没删到」不等于
        // 「磁盘上也没有」—— 那时返回 true 就是在对用户说谎。
        if self.read_only {
            return false;
        }
        // 内存里本来就没有、也没有「上次删了没写成」的欠账 → 磁盘上也没有（磁盘内容只来自内存）。
        // 有欠账时顺手重写一次：否则对同一条 Key 再点一次删除会得到「已删」，而磁盘上还在。
        let owed = self.must_rewrite.swap(false, Ordering::Relaxed);
        if !removed && !owed {
            return true;
        }
        self.write_locked(true)
    }
}

/// 未落盘增量 = `totals − baseline`（逐字段饱和减法：防「外部把 usage.json 改小了」导致下溢 panic）。
fn pending_delta(
    totals: &BTreeMap<UsageKey, TokenUsage>,
    baseline: &BTreeMap<UsageKey, TokenUsage>,
) -> Vec<TokenUsageByKey> {
    totals
        .iter()
        .filter_map(|(slot, cur)| {
            let base = baseline.get(slot).copied().unwrap_or_default();
            let d = TokenUsage {
                input: cur.input.saturating_sub(base.input),
                output: cur.output.saturating_sub(base.output),
                cache_read: cur.cache_read.saturating_sub(base.cache_read),
                cache_creation: cur.cache_creation.saturating_sub(base.cache_creation),
            };
            (!d.is_empty()).then(|| TokenUsageByKey::from_slot(slot.clone(), d))
        })
        .collect()
}
/// 把若干行并进某一天的桶（没有就新建），并保持「最新在前」。
///
/// 同一格（分类 × Key × 模型）**相加**，不覆盖：手工编辑过的文件可能一天里同一格出现两行，
/// `collect()` 进 map 会让后一条静默顶掉前一条（v3 的实现就是那样写的）。
fn merge_into_day(buckets: &mut Vec<DailyUsageBucket>, date: &str, rows: Vec<TokenUsageByKey>) {
    let idx = match buckets.iter().position(|b| b.date == date) {
        Some(i) => i,
        None => {
            buckets.push(DailyUsageBucket { date: date.to_string(), entries: Vec::new() });
            buckets.len() - 1
        }
    };
    let bucket = &mut buckets[idx];
    let mut map: BTreeMap<UsageKey, TokenUsage> = BTreeMap::new();
    for r in bucket.entries.drain(..).chain(rows) {
        map.entry(r.slot()).or_default().add(&r.usage);
    }
    bucket.entries = map.into_iter().map(|(slot, u)| TokenUsageByKey::from_slot(slot, u)).collect();
    buckets.sort_by(|a, b| b.date.cmp(&a.date));
}

/// 90 天滚动：比 `today` 早 [`KEEP_DAYS`] 天及以上的桶，**先折进 `retired` 再删**。
///
/// 直接丢掉的话，启动时的累计总量（= retired + 各存活桶之和）每过一个 90 天就往下掉一截，
/// 而 `since_ms` 仍宣称「统计自 <安装日> 起」—— 数字覆盖的区间比它声称的短，且完全不可见。
///
/// 日期是 `YYYY-MM-DD`，字典序 = 时间序，故直接比字符串。解析不出的日期**保留**：
/// 手工编辑过的桶不该被静默删除。
fn retire_expired(buckets: &mut Vec<DailyUsageBucket>, retired: &mut Vec<TokenUsageByKey>, today: &str) {
    let Ok(t) = chrono::NaiveDate::parse_from_str(today, "%Y-%m-%d") else {
        return;
    };
    let cutoff = (t - chrono::Duration::days(KEEP_DAYS)).format("%Y-%m-%d").to_string();
    let mut folded: BTreeMap<UsageKey, TokenUsage> = BTreeMap::new();
    buckets.retain(|b| {
        let parsed = chrono::NaiveDate::parse_from_str(&b.date, "%Y-%m-%d").is_ok();
        let keep = !parsed || b.date.as_str() > cutoff.as_str();
        if !keep {
            for r in &b.entries {
                folded.entry(r.slot()).or_default().add(&r.usage);
            }
        }
        keep
    });
    if folded.is_empty() {
        return;
    }
    for r in retired.drain(..) {
        folded.entry(r.slot()).or_default().add(&r.usage);
    }
    *retired = folded.into_iter().map(|(slot, u)| TokenUsageByKey::from_slot(slot, u)).collect();
}
#[cfg(test)]
mod tests {
    use super::*;

    fn temp_cfg(tag: &str) -> PathBuf {
        use std::sync::atomic::AtomicU64;
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir()
            .join(format!("synaroute_ledger_{tag}_{}_{seq}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("config.json")
    }
    fn u(input: u64) -> TokenUsage {
        TokenUsage { input, output: input / 10, cache_read: 0, cache_creation: 0 }
    }
    fn on_disk(cfg: &Path) -> UsageSnapshot {
        serde_json::from_slice(&std::fs::read(super::super::usage_file_path(cfg)).unwrap()).unwrap()
    }
    fn disk_has_key(cfg: &Path, key: &str) -> bool {
        let s = on_disk(cfg);
        s.daily_buckets.iter().flat_map(|b| &b.entries).chain(&s.retired).any(|r| r.key_id == key)
    }

    /// 🔴 同一条 Key 上的两个模型必须分开记 —— 这是本模块存在的理由（见模块头「为什么要按模型记」）。
    #[test]
    fn two_models_on_one_key_stay_separate_and_survive_a_restart() {
        let cfg = temp_cfg("per_model");
        let l = UsageLedger::load(&cfg);
        l.record(CategoryType::Codex, "k", "gpt-6-astra", &u(9000));
        l.record(CategoryType::Codex, "k", "gpt-5.6-luna", &u(1000));
        let mut models: Vec<_> = l.by_model().into_iter().map(|r| (r.model, r.usage.input)).collect();
        models.sort();
        assert_eq!(models, vec![("gpt-5.6-luna".to_string(), 1000), ("gpt-6-astra".to_string(), 9000)]);
        let by_key = l.by_key();
        assert_eq!(by_key.len(), 1, "按 Key 汇总只有一行");
        assert_eq!((by_key[0].usage.input, by_key[0].model.as_str()), (10_000, ""));

        assert!(l.flush());
        drop(l);
        let again = UsageLedger::load(&cfg);
        assert_eq!(again.by_model().len(), 2, "重启后模型维度必须还在（被并成一行 = v4 白做了）");
        std::fs::remove_dir_all(cfg.parent().unwrap()).ok();
    }

    /// v3 文件（没有 model）读入成历史行，升级写回 v4 后历史行与新行**并存**，总量一笔不少。
    #[test]
    fn a_v3_file_upgrades_without_losing_its_model_less_history() {
        let cfg = temp_cfg("v3_upgrade");
        let path = super::super::usage_file_path(&cfg);
        // 日期取「3 天前」而不是写死：写死的日期过了 90 天就会被滚进 retired，用例自己过期。
        let day = local_date_string(chrono::Utc::now().timestamp_millis() - 3 * 86_400_000);
        std::fs::write(&path, format!(r#"{{"version":3,"sinceMs":1700000000000,"dailyBuckets":[
            {{"date":"{day}","entries":[{{"categoryId":"claude-cli","keyId":"k","usage":{{"input":500,"output":5}}}}]}}
        ],"retired":[{{"categoryId":"claude-cli","keyId":"k","usage":{{"input":70,"output":7}}}}]}}"#)).unwrap();

        let l = UsageLedger::load(&cfg);
        assert!(!l.read_only, "v3 比本程序旧，可以写回");
        assert_eq!(l.by_key()[0].usage.input, 570, "retired 70 + 桶 500");
        l.record(CategoryType::ClaudeCli, "k", "claude-opus-5-5", &u(30));
        assert!(l.flush());

        let snap = on_disk(&cfg);
        assert_eq!(snap.version, USAGE_SNAPSHOT_VERSION, "写回即升到 v4");
        let rows: Vec<_> = snap.daily_buckets.iter().flat_map(|b| &b.entries).collect();
        assert!(rows.iter().any(|r| r.model.is_empty() && r.usage.input == 500), "历史行原样保留");
        assert!(rows.iter().any(|r| r.model == "claude-opus-5-5" && r.usage.input == 30), "新行带模型");
        drop(l);
        assert_eq!(UsageLedger::load(&cfg).by_key()[0].usage.input, 600, "重启后总量 = 570 + 30");
        std::fs::remove_dir_all(cfg.parent().unwrap()).ok();
    }
    /// 删 Key 的用量：各分类、各模型、日桶、retired、未落盘增量 —— 一处都不能剩，且别的 Key 不受影响。
    #[test]
    fn purging_a_key_removes_every_trace_of_it_and_nothing_else() {
        let cfg = temp_cfg("purge_all");
        let path = super::super::usage_file_path(&cfg);
        let recent = local_date_string(chrono::Utc::now().timestamp_millis() - 2 * 86_400_000);
        // 一份已经有「被删的 Key 的 retired + 日桶」与「留下的 Key」的 v4 文件。
        std::fs::write(&path, format!(r#"{{"version":4,"sinceMs":1,"dailyBuckets":[
            {{"date":"{recent}","entries":[
                {{"categoryId":"claude-cli","keyId":"gone","model":"m1","usage":{{"input":10,"output":1}}}},
                {{"categoryId":"claude-cli","keyId":"stay","model":"m1","usage":{{"input":20,"output":2}}}}]}}],
            "retired":[{{"categoryId":"codex","keyId":"gone","usage":{{"input":30,"output":3}}}}]}}"#)).unwrap();
        let l = UsageLedger::load(&cfg);
        // 再来两笔未落盘的：一笔属于被删的 Key（另一分类、另一模型），一笔属于留下的。
        l.record(CategoryType::Codex, "gone", "m2", &u(40));
        l.record(CategoryType::ClaudeCli, "stay", "m1", &u(5));

        assert!(l.purge_key("gone"), "可写时必须真的落了盘");
        assert!(l.by_model().iter().all(|r| r.key_id != "gone"), "内存累计里不许剩");
        assert!(l.retired.read().iter().all(|r| r.key_id != "gone"), "retired 里不许剩");
        assert!(l.daily_buckets().iter().flat_map(|b| &b.entries).all(|r| r.key_id != "gone"), "日桶里不许剩");
        assert!(!disk_has_key(&cfg, "gone"), "磁盘上不许剩");
        let stay: u64 = l.by_key().iter().filter(|r| r.key_id == "stay").map(|r| r.usage.input).sum();
        assert_eq!(stay, 25, "别的 Key 一笔都不能少（含未落盘的 5）");

        drop(l);
        let again = UsageLedger::load(&cfg);
        assert!(again.by_model().iter().all(|r| r.key_id != "gone"), "重启后不许复活");
        assert_eq!(again.by_key().iter().map(|r| r.usage.input).sum::<u64>(), 25);
        std::fs::remove_dir_all(cfg.parent().unwrap()).ok();
    }

    /// 删除时写盘失败：下一轮 flush **即使没有新增量**也必须重写，否则被删的行重启后复活；
    /// 对同一条 Key 再点一次删除也不许报「已删」，除非这次真写成了。
    #[test]
    fn a_failed_purge_write_is_retried_even_without_new_usage() {
        let cfg = temp_cfg("purge_retry");
        let path = super::super::usage_file_path(&cfg);
        let l = UsageLedger::load(&cfg);
        l.record(CategoryType::ClaudeCli, "gone", "m", &u(10));
        l.record(CategoryType::ClaudeCli, "stay", "m", &u(20));
        assert!(l.flush());
        let good = std::fs::read(&path).unwrap();

        // 目录占位：真实 atomic_write 的 rename 与原地回退都会失败，且不依赖权限位。
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir_all(&path).unwrap();
        assert!(!l.purge_key("gone"), "写失败时不能报告成功");
        // 第二次删除：内存里已经没有了，但磁盘欠账还在 —— 写不成就仍然不许报「已删」。
        assert!(!l.purge_key("gone"), "磁盘上还在，不许报「已删」");

        std::fs::remove_dir_all(&path).unwrap();
        std::fs::write(&path, &good).unwrap();
        assert!(disk_has_key(&cfg, "gone"), "前提：磁盘上还是删之前的那份");
        assert!(l.flush(), "没有新增量也必须重写一次（删除的欠账）");
        assert!(!disk_has_key(&cfg, "gone"), "重写之后磁盘上不许再有被删的 Key");
        assert!(!l.flush(), "欠账还清之后不再重复写");
        std::fs::remove_dir_all(cfg.parent().unwrap()).ok();
    }

    /// 只读（磁盘那份比本程序新 / 这次读不出来）：删除只在内存里生效，**一个字节都不写**，并如实返回 false。
    #[test]
    fn purging_in_read_only_mode_never_writes() {
        let cfg = temp_cfg("purge_ro");
        let path = super::super::usage_file_path(&cfg);
        let future = format!(r#"{{"version":{},"sinceMs":1,"dailyBuckets":[]}}"#, USAGE_SNAPSHOT_VERSION + 1);
        std::fs::write(&path, &future).unwrap();
        let l = UsageLedger::load(&cfg);
        assert!(l.read_only);
        l.record(CategoryType::ClaudeCli, "gone", "m", &u(10));
        assert!(!l.purge_key("gone"), "只读时删不到磁盘，必须如实返回 false");
        assert!(!l.purge_key("never-seen"), "内存里没有 ≠ 磁盘上没有（只读时内存是空的）");
        assert!(l.by_model().is_empty(), "本次运行的界面里它已经没了");
        assert!(!l.flush());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), future, "更新格式的文件原样保留");
        std::fs::remove_dir_all(cfg.parent().unwrap()).ok();
    }
    /// 一天里同一格出现两行（手工编辑过的文件）时**相加**，不让后一条顶掉前一条。
    #[test]
    fn duplicate_rows_in_one_day_are_summed_not_overwritten() {
        let row = |n| TokenUsageByKey::from_slot((CategoryType::ClaudeCli, "k".into(), "m".into()), u(n));
        let mut buckets = vec![DailyUsageBucket { date: "2026-10-12".into(), entries: vec![row(3), row(4)] }];
        merge_into_day(&mut buckets, "2026-10-12", vec![row(5)]);
        assert_eq!(buckets[0].entries.len(), 1);
        assert_eq!(buckets[0].entries[0].usage.input, 12, "3 + 4 + 5，不是只剩最后一条");
    }

    /// 90 天窗口的边界（含今天共 90 天）：第 89 天前保留、第 90 天前滚进 retired，总量一笔不少。
    #[test]
    fn the_rolling_window_keeps_exactly_ninety_days_and_folds_the_rest() {
        let row = |n| TokenUsageByKey::from_slot((CategoryType::ClaudeCli, "k".into(), "m".into()), u(n));
        let today = chrono::NaiveDate::from_ymd_opt(2026, 10, 12).unwrap();
        let day = |back: i64| (today - chrono::Duration::days(back)).format("%Y-%m-%d").to_string();
        let mut buckets = vec![
            DailyUsageBucket { date: day(0), entries: vec![row(1)] },
            DailyUsageBucket { date: day(89), entries: vec![row(10)] },
            DailyUsageBucket { date: day(90), entries: vec![row(100)] },
            DailyUsageBucket { date: "not-a-date".into(), entries: vec![row(1000)] },
        ];
        let mut retired = vec![row(7)];
        retire_expired(&mut buckets, &mut retired, &day(0));
        let dates: Vec<_> = buckets.iter().map(|b| b.date.as_str()).collect();
        assert_eq!(dates, vec![day(0).as_str(), day(89).as_str(), "not-a-date"], "第 90 天前那个被滚掉");
        assert_eq!(retired.len(), 1);
        assert_eq!(retired[0].usage.input, 107, "滚掉的 100 折进 retired（原有 7），不是丢掉");
    }

    /// 🔴 「今天」只能从 [`local_date_string`] 来。
    ///
    /// 行为用例证明不了这一条：在 UTC 时区的机器上（CI 就是），本地日期与 UTC 日期恒相等，
    /// 把 flush 里那一行改回 `Utc::now().format(..)` 所有用例照样全绿 —— 而用户在东八区。
    /// 故用源码级判据钉住：生产段不许出现从 UTC 时刻直接取日期的写法。
    #[test]
    fn today_is_never_derived_from_utc_in_production_code() {
        let prod = crate::proxy::custom_headers::production_code_only(include_str!("usage_ledger.rs"));
        for banned in ["Utc::now().format", "Utc::now().date_naive", "and_utc()"] {
            assert!(!prod.contains(banned), "生产段出现了 `{banned}` —— 东八区每天 0~8 点会被算进昨天");
        }
        // 反向：三处取「今天」的地方都在用它（load / 读视图 / 写盘）。少于 3 说明判据在空转。
        assert!(prod.matches("local_date_string(").count() >= 3, "取「今天」的调用点变少了");
    }
}
