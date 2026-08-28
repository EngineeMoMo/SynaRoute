//! 日志文件的**体积**上限与滚动切分（docs/14 §21.1 B2）。
//!
//! 挂在 [`crate::store`] 下（`#[path]`），不是因为它和 store 有耦合，而是因为
//! `store.rs` 的棘轮余量是 **0**，而目录化是 docs/15 P2 刻意未做的大 diff。
//! 同 `data_dir.rs` / `lan_guard.rs` / `updater_msg.rs` 的挂法。
//!
//! # 为什么需要它
//!
//! 此前只有两条机制：按天滚动（写线程跨天重开）+ 按保留期删旧文件
//! （`LOG_RETAIN_DAYS = 30`）。两条管的都是「**留几天**」，**没有任何东西管「一天多大」**。
//!
//! 实测量级：最大单日 38 MB，多数 11~14 MB。没爆是运气 —— 用户一开
//! `logDownstreamRawEnabled`（单个 Codex 请求的下游原始 body 可达十几万字符），
//! 一天几个 GB 完全可能。而盘满的后果**不止是日志写不进去**：`config.json` 与
//! `secrets.enc` 的原子写也会失败，表现成一堆看不出关联的功能故障。
//!
//! # 两级上限（缺一级都不成立）
//!
//! 1. **单文件** [`FILE_MAX_BYTES`]：写满就滚到 `YYYY-MM-DD.1.jsonl`、`.2.jsonl`…
//! 2. **单日总量** [`DAY_MAX_FILES`]：滚到超出当天配额时，**删掉当天序号最小的那个**，
//!    形成滑窗、保住最近的。
//!
//! 🔴 **只做第 1 级是无效的**：滚动切分把一个大文件变成很多个，磁盘占用一分不少。
//! 而只做第 2 级（写满即停）在排障时最糟 —— 日志恰好在最需要的时刻消失，且用户不会知道。
//! 选「保最近、丢最旧」是因为开着原始日志的用户**正在排障**，他要的是刚刚发生的事。
//!
//! # 命名：第 0 个文件**刻意不带序号**
//!
//! `2026-08-27.jsonl` 而不是 `2026-08-27.0.jsonl`。绝大多数日子只有一个文件，
//! 而用户会直接 `tail` 它、冒烟脚本会遍历它、docs 里到处写着这个名字。
//! 让常见情形保持原样，序号只在真的滚动时出现。

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// 因**打不开日志文件**而丢掉的行数。
///
/// 🔴 这是第二条丢日志路径，而它此前**不计数**：`Store::append_log_line` 的 `try_send`
/// 队列满会计进 `log_dropped`，但写线程里 `OpenLog::open` 失败时走的是
/// `open = None; continue;` —— 那一行直接没了。
///
/// 危害不在丢几行，而在**诊断报告把 `log_dropped_count` 当成「是否丢过日志」的权威答案**
/// 打印出来。于是盘满 / 目录不可写时，日志在丢而那个数字读 0 ——
/// 属本仓自己那套分类里的「界面撒谎」，且排障时会把人引向完全错误的方向
/// （「日志没丢，那就是没记？」）。
///
/// 放在本模块而不是 `Store` 里：写线程是 `spawn_log_writer` 里的静态闭包、拿不到 `&self`，
/// 而 `store.rs` 的棘轮余量是 0。进程级 static 与 `log_dropped` 的
/// 「每进程一个 Store」语义一致。
static OPEN_FAILED_LINES: AtomicU64 = AtomicU64::new(0);

/// 记一次「因打不开文件而丢行」。**由写线程在 `OpenLog::open` 失败时调用。**
///
/// 调用点那一行没有注释，理由写在这里 —— `store.rs` 的棘轮余量是 0，而本文件有余量。
/// 那个分支原本只 `tracing::warn!` 一句就 `continue`，**那一行日志就此无声消失**：
/// 危害不在丢几行，而在诊断报告只打 `log_dropped_count` 一个数字，
/// 用户与排障者都拿它当「是否丢过日志」的答案 —— 盘满时它读 0 而日志正在丢。
///
/// 告警按量级点发（1/100/200…），同 `Store::append_log_line` 的取舍 ——
/// 磁盘僵死时告警本身不能变成刷屏源。原来那句 `warn!` 已并入这里，故调用点不必再打。
pub(crate) fn note_open_failed_line() {
    let n = OPEN_FAILED_LINES.fetch_add(1, Ordering::Relaxed) + 1;
    if n == 1 || n % 100 == 0 {
        tracing::warn!("打开日志文件失败，累计丢弃 {n} 条（磁盘可能已满或目录不可写）");
    }
}

/// 因打不开日志文件而丢掉的行数。诊断报告**单独打一行**，不与
/// `Store::log_dropped_count`（队列满）相加。
///
/// 分开的两个理由：
/// - 成因与处置都不同 —— 写得太慢（等等看）vs 压根写不了（去看磁盘和权限）。
///   合成一个数字会让排障的人拿不到方向。
/// - 本计数是**进程级** static（写线程拿不到 `&Store`），而 `log_dropped` 是每 Store 一份。
///   并进去会让「本 Store 没丢过日志」这类断言被同进程其它测试污染 ——
///   本仓 `flush_logs_drains_the_queue` 当场就红了（加这条时实际踩到）。
pub(crate) fn open_failed_line_count() -> u64 {
    OPEN_FAILED_LINES.load(Ordering::Relaxed)
}

/// 单个日志文件的字节上限。超过即滚到下一个序号。
///
/// 16 MB 的依据：实测最大单日 38 MB、多数 11~14 MB —— 也就是**典型的一天落在一个文件里**
/// （不改变绝大多数用户看到的样子），重日志的一天分成两三个。
/// 再小会让普通用户也天天见到滚动文件，再大则单个文件已超出编辑器舒适区。
pub(crate) const FILE_MAX_BYTES: u64 = 16 * 1024 * 1024;

/// 单日文件数上限（含第 0 个）。× [`FILE_MAX_BYTES`] = 单日总量上限 **256 MB**。
///
/// 与 `LOG_RETAIN_DAYS = 30` 相乘得最坏 7.5 GB —— 那是「连续 30 天每天都在跑满原始日志」
/// 的极端值，正常使用两个数量级以下。
/// **刻意不做成配置项**：同 `LOG_RETAIN_DAYS` 的理由，又一个开关意味着又一处要对账的状态。
pub(crate) const DAY_MAX_FILES: u32 = 16;

/// 当天第 `idx` 个文件的名字。`idx == 0` 时**不带序号**（见模块注释）。
pub(crate) fn file_name(date: &str, idx: u32) -> String {
    if idx == 0 {
        format!("{date}.jsonl")
    } else {
        format!("{date}.{idx}.jsonl")
    }
}

/// 从文件名解析出 `(日期, 序号)`。认 `YYYY-MM-DD.jsonl` 与 `YYYY-MM-DD.N.jsonl` 两种。
///
/// 🔴 **这个函数是本模块存在的第二个理由**：清理旧日志的判据原先是
/// `name.strip_suffix(".jsonl")` 再 `parse_from_str(.., "%Y-%m-%d")`，
/// 而 `"2026-08-27.1"` 解析**会失败** —— 于是滚动出来的文件**永远不会被清理**。
/// 那是个典型的静默失效：保留期看着在工作，实际只管住了每天的第一个文件，
/// 而磁盘占用这个方向**永不自愈**。加滚动就必须同时改这里，两件事是一件事。
///
/// 严格拒绝其它形态（用户自己放的、旧版遗留的 `events.jsonl`、`2026-99-99.jsonl`）——
/// **删错文件比留着旧日志严重得多**。
pub(crate) fn parse_name(name: &str) -> Option<(chrono::NaiveDate, u32)> {
    let stem = name.strip_suffix(".jsonl")?;
    // 先按「带序号」试：末尾一段全是数字且能剥出合法日期。
    if let Some((head, tail)) = stem.rsplit_once('.') {
        if !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()) {
            // 序号不接受前导零（`.01`）：那不是我们写出来的名字，来源不明就不碰。
            if tail.len() > 1 && tail.starts_with('0') {
                return None;
            }
            if let Ok(d) = chrono::NaiveDate::parse_from_str(head, "%Y-%m-%d") {
                // 序号 0 也不接受：我们从不写 `.0.jsonl`，出现即来源不明。
                let idx: u32 = tail.parse().ok()?;
                return if idx == 0 { None } else { Some((d, idx)) };
            }
        }
    }
    let d = chrono::NaiveDate::parse_from_str(stem, "%Y-%m-%d").ok()?;
    Some((d, 0))
}

/// 扫目录，返回某天已存在的序号（升序）。
fn existing_indices(dir: &Path, date: &chrono::NaiveDate) -> Vec<u32> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut v: Vec<u32> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name();
            let (d, idx) = parse_name(name.to_str()?)?;
            (d == *date).then_some(idx)
        })
        .collect();
    v.sort_unstable();
    v
}

/// 当前打开的日志文件。取代原先写线程里那个 `(目录, 日期, BufWriter)` 三元组，
/// 多带了「已写字节数」与「序号」—— 体积判据必须有这两个才成立。
pub(crate) struct OpenLog {
    pub(crate) dir: PathBuf,
    pub(crate) date: String,
    idx: u32,
    /// 已写字节数。**以 append 打开时的文件实际长度为起点**，不是从 0 起 ——
    /// 否则重启一次就把上限重置了，而「重启后继续写同一个文件」正是常态。
    size: u64,
    /// 是否已经放弃滚动（见 [`Self::give_up_rolling`]）。只为「告警发一次」服务。
    roll_failed: bool,
    w: BufWriter<File>,
}

impl OpenLog {
    /// 打开 `dir` 下 `date` 当天该写的那个文件。
    ///
    /// 「该写哪个」= 已存在的最大序号（续写它），没有则 0。
    /// 若它已经满了，这里**不滚** —— 滚动只发生在 [`Self::write_line`] 里，
    /// 保证「判断满没满」只有一处判据。
    pub(crate) fn open(dir: PathBuf, date: String) -> std::io::Result<Self> {
        std::fs::create_dir_all(&dir)?;
        let parsed = chrono::NaiveDate::parse_from_str(&date, "%Y-%m-%d").ok();
        let idx = parsed
            .as_ref()
            .and_then(|d| existing_indices(&dir, d).last().copied())
            .unwrap_or(0);
        let path = dir.join(file_name(&date, idx));
        let f = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
        let size = f.metadata().map(|m| m.len()).unwrap_or(0);
        Ok(Self { dir, date, idx, size, roll_failed: false, w: BufWriter::new(f) })
    }

    /// 写一行（含换行）。必要时先滚动到下一个文件。
    ///
    /// 判据用**写之前**的长度：一行本身可能就超过上限（trace 满载时单行几十 KB，
    /// 而 `logDownstreamRawEnabled` 下更大），若要求「写完不超」就永远写不下去。
    /// 故语义是「文件已达上限就换下一个」，单个文件可略微超出最后一行的长度。
    pub(crate) fn write_line(&mut self, line: String) {
        if self.size >= FILE_MAX_BYTES {
            self.roll();
        }
        let mut buf = line.into_bytes();
        buf.push(b'\n');
        // 正文与换行拼成同一 buffer 一次写出：既省一次系统调用，也与历史教训一致
        // （旧的 writeln! 会拆成两次写，多线程下产生粘行）。
        match self.w.write_all(&buf) {
            Ok(()) => self.size += buf.len() as u64,
            Err(e) => tracing::warn!("写入日志文件失败: {e}"),
        }
    }

    /// 滚到下一个序号；越过当天配额时先删掉当天最旧的那个。
    ///
    /// 全程 best-effort：滚不动就继续写当前文件（宁可超出上限，也不能让写日志
    /// 把转发流程搞挂）。同 `cleanup_old_logs_in` 的取舍。
    ///
    /// 🔴 **滚不动时必须让 `size` 退出「已达上限」状态**，否则每写一行都会再进来一次
    /// —— 而本函数开头就是 `existing_indices()` → `read_dir`。开着请求日志的转发热路径上
    /// 那是**每条日志一次目录扫描**。
    ///
    /// 讽刺的地方在于触发条件：`open` 失败最典型的成因就是**盘满**，
    /// 而盘满正是本模块存在的理由。也就是说不处理这条，本模块会在它要解决的那个场景里
    /// 变成性能悬崖。故失败时把 `size` 归零并只告警一次 —— 语义是「这个文件不再受
    /// 上限约束」，宁可超出上限（盘满时本来也写不进去）也不做无用的重试。
    fn roll(&mut self) {
        let _ = self.w.flush();
        let next = self.idx + 1;
        let Ok(date) = chrono::NaiveDate::parse_from_str(&self.date, "%Y-%m-%d") else {
            // 日期形态不对（不该发生）：不滚，继续写当前文件。
            // 同下面 open 失败那条 —— 必须退出「已达上限」状态，否则每行重试一次。
            self.give_up_rolling("日志文件名里的日期解析失败");
            return;
        };

        // 越过当天配额 → 删最旧的，保最近的（滑窗）。
        // 删的是**当天**的，跨天历史由 `cleanup_old_logs_in` 按保留期管，两者不重叠。
        let indices = existing_indices(&self.dir, &date);
        let mut over = (indices.len() as u32 + 1).saturating_sub(DAY_MAX_FILES);
        for old in indices {
            if over == 0 {
                break;
            }
            if old == next {
                continue; // 不删自己要写的那个
            }
            let victim = self.dir.join(file_name(&self.date, old));
            match std::fs::remove_file(&victim) {
                Ok(()) => {
                    // 这条**必须留**：自动删用户的日志是破坏性动作，不留痕迹的话
                    // 排障时会看到「日志从中间断了」而完全找不到原因。
                    tracing::warn!(
                        "单日日志已达 {DAY_MAX_FILES} 个文件（{}MB）上限，删除当天最旧的 {}",
                        DAY_MAX_FILES as u64 * FILE_MAX_BYTES / 1024 / 1024,
                        victim.display()
                    );
                    over -= 1;
                }
                Err(e) => {
                    tracing::warn!("删除旧日志分片 {} 失败: {e}", victim.display());
                    break; // 删不掉就别继续试，避免每次滚动都刷一遍 warn
                }
            }
        }

        let path = self.dir.join(file_name(&self.date, next));
        match std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            Ok(f) => {
                let size = f.metadata().map(|m| m.len()).unwrap_or(0);
                self.idx = next;
                self.size = size;
                self.w = BufWriter::new(f);
                tracing::info!("日志文件已滚动到 {}", path.display());
            }
            Err(e) => {
                self.give_up_rolling(&format!("打开 {} 失败: {e}", path.display()));
            }
        }
    }

    /// 放弃滚动：把 `size` 退出「已达上限」状态，继续写当前文件。
    ///
    /// 见 [`Self::roll`] 的说明 —— 不归零的话每写一行都会重扫一次目录。
    /// 告警只发一次（`roll_failed`），否则盘满时告警本身就是刷屏源
    /// （同 `append_log_line` 里按量级点告警的取舍）。
    fn give_up_rolling(&mut self, why: &str) {
        self.size = 0;
        if !self.roll_failed {
            self.roll_failed = true;
            tracing::warn!(
                "日志滚动失败（{why}）：已放弃对 {} 的体积限制，将继续写入当前文件。\
                 常见成因是磁盘已满或目录不可写。",
                self.dir.display()
            );
        }
    }

    pub(crate) fn flush(&mut self) {
        let _ = self.w.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 唯一临时目录：temp_dir + pid + 原子计数。
    /// **不用时间戳**：本仓实测 `timestamp_nanos_opt()` 的量化粒度只有 100ns，
    /// 并发下 88% 撞车（`db_copy_path` 那条缺陷就是这么来的）。
    fn tmp(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let d = std::env::temp_dir().join(format!(
            "synaroute_rot_{}_{}_{}",
            tag,
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn names_in(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn first_file_of_the_day_has_no_index_suffix() {
        // 绝大多数日子只有一个文件，而用户会直接 tail 它、冒烟脚本会遍历它、
        // docs 里到处写着这个名字。序号只在真的滚动时出现。
        assert_eq!(file_name("2026-08-27", 0), "2026-08-27.jsonl");
        assert_eq!(file_name("2026-08-27", 1), "2026-08-27.1.jsonl");
        assert_eq!(file_name("2026-08-27", 15), "2026-08-27.15.jsonl");
    }

    #[test]
    fn parse_name_accepts_both_forms_and_rejects_everything_else() {
        let d = chrono::NaiveDate::from_ymd_opt(2026, 8, 27).unwrap();
        assert_eq!(parse_name("2026-08-27.jsonl"), Some((d, 0)));
        assert_eq!(parse_name("2026-08-27.1.jsonl"), Some((d, 1)));
        assert_eq!(parse_name("2026-08-27.15.jsonl"), Some((d, 15)));

        // 不是我们写出来的名字，一律拒 —— 删错文件比留着旧日志严重得多。
        for bad in [
            "events.jsonl",          // 旧版遗留命名
            "2026-99-99.jsonl",      // 像日期但解析不出来
            "2026-08-27.0.jsonl",    // 我们从不写 .0（那是不带序号的那个）
            "2026-08-27.01.jsonl",   // 前导零：来源不明
            "2026-08-27.x.jsonl",    // 序号不是数字
            "2026-08-27.jsonl.bak",  // 不是 .jsonl 结尾
            "2026-08-27",            // 没有后缀
            "notes.txt",             // 用户自己放的
        ] {
            assert_eq!(parse_name(bad), None, "{bad} 不该被认成日志文件");
        }
    }

    /// 🔴 **本模块最要紧的一条**：滚动出来的分片**必须**被保留期清理管到。
    ///
    /// 这是加滚动切分时会顺手造出来的静默失效 —— 原先的判据是
    /// `strip_suffix(".jsonl")` 再按 `%Y-%m-%d` 解析，而 `"2026-08-27.1"` 解析**失败**，
    /// 于是分片永不被删。表现是「保留期看着在工作」，实际只管住了每天的第一个文件，
    /// 而磁盘占用这个方向**永不自愈**（正是本功能要解决的问题本身）。
    ///
    /// 注入验证：把 `parse_name` 换回 `strip_suffix + parse_from_str` → 本条变红。
    #[test]
    fn retention_also_deletes_rolled_shards_not_just_the_first_file() {
        let dir = tmp("retain_shards");
        let today = chrono::Utc::now().date_naive();
        let d = |off: i64| (today - chrono::Duration::days(off)).format("%Y-%m-%d").to_string();

        let expired_base = format!("{}.jsonl", d(super::super::LOG_RETAIN_DAYS + 1));
        let expired_shard = format!("{}.3.jsonl", d(super::super::LOG_RETAIN_DAYS + 1));
        let kept_shard = format!("{}.2.jsonl", d(0));
        for n in [&expired_base, &expired_shard, &kept_shard] {
            std::fs::write(dir.join(n), b"x").unwrap();
        }

        let removed = super::super::cleanup_old_logs_in(&dir);

        assert_eq!(removed, 2, "过期的基础文件与分片都该删");
        let left = names_in(&dir);
        assert_eq!(left, vec![kept_shard], "只该留下今天那个分片");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn writing_past_the_file_cap_rolls_to_the_next_index() {
        let dir = tmp("roll");
        let mut log = OpenLog::open(dir.clone(), "2026-08-27".into()).unwrap();

        // 先把第 0 个填到上限之上（一行一行写太慢，直接把内部计数推到上限）。
        log.size = FILE_MAX_BYTES;
        log.write_line("after-cap".into());
        log.flush();

        assert_eq!(
            names_in(&dir),
            vec!["2026-08-27.1.jsonl", "2026-08-27.jsonl"],
            "应滚出第 1 个分片，且原文件仍在"
        );
        let rolled = std::fs::read_to_string(dir.join("2026-08-27.1.jsonl")).unwrap();
        assert_eq!(rolled, "after-cap\n", "超限后的那一行应落在新分片里");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 单文件上限**不能**在重启后被重置 —— 「重启后继续写同一个文件」是常态，
    /// 若每次 `open` 都从 0 起算，那么频繁重启的用户单个文件可以无限大。
    #[test]
    fn reopening_counts_the_existing_file_length_not_zero() {
        let dir = tmp("resume_size");
        std::fs::write(dir.join("2026-08-27.jsonl"), vec![b'x'; 1024]).unwrap();

        let log = OpenLog::open(dir.clone(), "2026-08-27".into()).unwrap();
        assert_eq!(log.size, 1024, "应以磁盘上的实际长度为起点");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 重启后应续写**当天序号最大**的那个，而不是回到第 0 个 ——
    /// 回到第 0 个会让已经滚过的那天从头再滚一遍，且新旧内容在时间上交错。
    #[test]
    fn reopening_continues_at_the_highest_existing_index() {
        let dir = tmp("resume_idx");
        for n in ["2026-08-27.jsonl", "2026-08-27.1.jsonl", "2026-08-27.2.jsonl"] {
            std::fs::write(dir.join(n), b"x").unwrap();
        }
        // 别的日期不该影响本日的判断
        std::fs::write(dir.join("2026-08-26.9.jsonl"), b"x").unwrap();

        let mut log = OpenLog::open(dir.clone(), "2026-08-27".into()).unwrap();
        assert_eq!(log.idx, 2, "应续写 .2 而不是回到第 0 个");
        log.write_line("resumed".into());
        log.flush();
        let s = std::fs::read_to_string(dir.join("2026-08-27.2.jsonl")).unwrap();
        assert!(s.ends_with("resumed\n"), "应追加到 .2 上");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 越过当天配额时删**最旧**的、保**最近**的。
    ///
    /// 方向很重要：开着原始日志的用户**正在排障**，他要的是刚刚发生的事。
    /// 反过来（保最旧）等于「一出问题就只剩今天开头那点内容」。
    #[test]
    fn exceeding_the_day_budget_drops_the_oldest_shard_and_keeps_the_newest() {
        let dir = tmp("day_cap");
        // 造出当天已满配额的现场：第 0 个 + .1 ..= .{DAY_MAX_FILES-1}
        for i in 0..DAY_MAX_FILES {
            std::fs::write(dir.join(file_name("2026-08-27", i)), b"x").unwrap();
        }
        assert_eq!(names_in(&dir).len(), DAY_MAX_FILES as usize);

        let mut log = OpenLog::open(dir.clone(), "2026-08-27".into()).unwrap();
        log.size = FILE_MAX_BYTES; // 逼它滚
        log.write_line("newest".into());
        log.flush();

        let left = names_in(&dir);
        assert_eq!(
            left.len(),
            DAY_MAX_FILES as usize,
            "总数应仍等于配额（删一个、加一个）"
        );
        assert!(
            !left.contains(&"2026-08-27.jsonl".to_string()),
            "最旧的（不带序号那个）应被删掉"
        );
        let newest = file_name("2026-08-27", DAY_MAX_FILES);
        assert!(left.contains(&newest), "最新分片 {newest} 应存在");
        let s = std::fs::read_to_string(dir.join(&newest)).unwrap();
        assert_eq!(s, "newest\n");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 配额只数**当天**的：别的日期的文件不该把今天挤掉
    /// （跨天历史由保留期管，两者不重叠 —— 混在一起会让「昨天写得多」导致今天开头就丢日志）。
    #[test]
    fn the_day_budget_counts_only_that_days_files() {
        let dir = tmp("day_scope");
        for i in 0..DAY_MAX_FILES {
            std::fs::write(dir.join(file_name("2026-08-26", i)), b"x").unwrap();
        }
        std::fs::write(dir.join("2026-08-27.jsonl"), b"x").unwrap();

        let mut log = OpenLog::open(dir.clone(), "2026-08-27".into()).unwrap();
        log.size = FILE_MAX_BYTES;
        log.write_line("today".into());
        log.flush();

        let left = names_in(&dir);
        assert_eq!(
            left.iter().filter(|n| n.starts_with("2026-08-26")).count(),
            DAY_MAX_FILES as usize,
            "昨天的文件一个都不该被这次滚动删掉"
        );
        assert!(left.contains(&"2026-08-27.jsonl".to_string()), "今天的第 0 个还在配额内");
        assert!(left.contains(&"2026-08-27.1.jsonl".to_string()));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 🔴 **滚不动时必须退出「已达上限」状态**，否则每写一行都重扫一次目录。
    ///
    /// 判据不看日志、不看耗时（两者都不稳），而是看 `size` 这个决定性的内部状态：
    /// 只要它仍 ≥ 上限，`write_line` 下一次就必然再进 `roll()` → `existing_indices()`
    /// → `read_dir`。开着请求日志的转发热路径上那是每条日志一次目录扫描，
    /// 而触发条件（盘满 / 目录不可写）**正是本模块存在的理由**。
    ///
    /// 造「滚不动」的手法：把日期设成解析不出来的形态。它走的是 `roll()` 里
    /// 第一个 `return` 分支，与 `open` 失败那条共用 `give_up_rolling`
    /// —— 用它是因为「让 OpenOptions::open 确定性失败」在各平台上手法不一致
    /// （Windows 上目录占名、只读属性、句柄独占各有差异），而这条分支稳定可控。
    #[test]
    fn giving_up_on_a_roll_stops_rescanning_the_directory_every_line() {
        let dir = tmp("roll_giveup");
        let mut log = OpenLog::open(dir.clone(), "2026-08-27".into()).unwrap();
        // 日期形态不对 → roll() 走不下去
        log.date = "not-a-date".into();
        log.size = FILE_MAX_BYTES;

        log.write_line("first-after-cap".into());
        assert_eq!(
            log.size,
            "first-after-cap\n".len() as u64,
            "滚不动后 size 必须已退出「已达上限」状态（否则下一行又扫一次目录）"
        );
        assert!(log.roll_failed, "应记下已放弃，供告警只发一次");

        // 再写几行都不该再触发滚动尝试（size 远低于上限）
        for _ in 0..3 {
            log.write_line("more".into());
        }
        assert!(log.size < FILE_MAX_BYTES);
        log.flush();
        // 内容仍完整落在当前文件里 —— 放弃滚动不等于放弃写入。
        let s = std::fs::read_to_string(dir.join("2026-08-27.jsonl")).unwrap();
        assert!(s.contains("first-after-cap") && s.contains("more"));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 🔴 **打不开日志文件而丢掉的行必须被计数。**
    ///
    /// 这是第二条丢日志路径，此前不计数：`append_log_line` 的 `try_send` 队列满会计
    /// `log_dropped`，而写线程里 `OpenLog::open` 失败时 `open = None; continue;`
    /// —— 那一行无声消失。
    ///
    /// 危害不在丢几行，而在**诊断报告把 `log_dropped_count` 当权威答案打印**：
    /// 盘满时它读 0 而日志正在丢，排障的人会得出「日志没丢」这个反向结论。
    #[test]
    fn lines_dropped_because_the_file_could_not_be_opened_are_counted() {
        let before = open_failed_line_count();
        note_open_failed_line();
        note_open_failed_line();
        assert_eq!(
            open_failed_line_count(),
            before + 2,
            "打不开文件丢掉的行必须计数，否则诊断报告会在盘满时报 0"
        );
    }

    /// 🔴 **接线判据**：这条计数必须真的被写线程喂、并真的出现在诊断报告里。
    ///
    /// 上面那条只验证了计数器自己会加。而「加了但没人调」或「调了但报告不打」
    /// 照样让排障者以为没丢过日志 —— 同本仓反复踩的那类接线盲区
    /// （单元覆盖了组件 ≠ 覆盖了调用它的那条线）。
    ///
    /// ⚠️ 判据刻意**不要求**它并进 `log_dropped_count`：那两条成因与处置都不同
    /// （写得太慢 vs 压根写不了），合成一个数字会让排障拿不到方向。
    /// 且并进去会让进程级 static 污染「本 Store 没丢过日志」这类断言 ——
    /// `flush_logs_drains_the_queue` 当场就红了（加这条时实际踩到）。
    #[test]
    fn the_open_failure_count_is_fed_by_the_writer_and_shown_in_diagnostics() {
        let store_src = crate::proxy::custom_headers::production_slice(include_str!("store.rs"));
        assert!(
            store_src.contains("log_rotate::note_open_failed_line()"),
            "写线程 open 失败分支必须调 note_open_failed_line，否则计数恒 0"
        );
        let diag_src =
            crate::proxy::custom_headers::production_slice(include_str!("diagnostics.rs"));
        assert!(
            diag_src.contains("open_failed_line_count()"),
            "诊断报告必须打这个数字 —— 不打的话「是否丢过日志」在盘满时答错"
        );
    }

    /// 🔴 **接线判据**：写线程必须真的走 [`OpenLog`]，而不是自己开文件裸写。
    ///
    /// 上面那些用例全都直接构造 `OpenLog`，于是**把写线程改回裸 `write_all` 它们照样全绿**
    /// —— 而那就是「体积上限完全不生效」这个缺陷本身，且表现是静默的（日志照写、只是不再滚）。
    ///
    /// 同本仓反复踩过的那类盲区：`route_meta`（记得在每个出口挂头）、
    /// `lan_guard`（accept 的 peer 丢回 `_`）、`mcp::handle_http`（path → dispatch 那一步）。
    /// **单元覆盖了组件 ≠ 覆盖了调用它的那条线。**
    #[test]
    fn the_log_writer_thread_must_go_through_openlog() {
        // 同 lan_guard：否定断言下，被截断的生产段会让判据空洞通过。
        let prod = crate::proxy::custom_headers::production_slice(include_str!("store.rs"));
        assert!(
            prod.contains("log_rotate::OpenLog::open("),
            "写线程必须用 OpenLog::open 开文件（体积起点/续写序号都在里面）"
        );
        assert!(
            prod.contains("o.write_line(line)"),
            "写线程必须经 write_line 写（体积判据与滚动都在里面），不能裸 write_all"
        );
        // 旧实现的形态：自己拼 `{date}.jsonl` + 自己开 BufWriter。留着即说明有第二条写路径。
        assert!(
            !prod.contains(r#"dir.join(format!("{date}.jsonl"))"#),
            "写线程里不该再有自己拼日志文件名的旁路 —— 文件名判据要留在 log_rotate 一处"
        );
    }

    /// 端到端实测：**经真实 `Store` 的日志投递**写满 16 MB，确认真的滚出分片。
    ///
    /// `#[ignore]` 的理由同 `perf_probe.rs`：要真写 16 MB，跑一次约几秒，
    /// 不该进每次改动都跑的常规套件。但它是唯一能证明**整条链**在真实体量下成立的东西
    /// —— 上面那条接线判据只证明「调用点在源码里」，这条证明「跑起来真的会滚」。
    ///
    /// 跑法：`cargo test --lib openlog_rolls_end_to_end -- --ignored --nocapture`
    #[test]
    #[ignore = "端到端实测（真写 16MB），手动跑：cargo test --lib openlog_rolls_end_to_end -- --ignored --nocapture"]
    fn openlog_rolls_end_to_end() {
        let dir = tmp("e2e");
        let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let mut log = OpenLog::open(dir.clone(), date.clone()).unwrap();

        // 每行 1 KB，写到刚过单文件上限。
        let line = "y".repeat(1023);
        let rounds = FILE_MAX_BYTES / 1024 + 2;
        for _ in 0..rounds {
            log.write_line(line.clone());
        }
        log.flush();

        let base = dir.join(file_name(&date, 0));
        let shard = dir.join(file_name(&date, 1));
        let base_len = std::fs::metadata(&base).unwrap().len();
        println!(
            "第 0 个 = {} MB，分片存在 = {}",
            base_len / 1024 / 1024,
            shard.exists()
        );
        assert!(shard.exists(), "写过 16MB 后必须滚出分片");
        assert!(base_len >= FILE_MAX_BYTES, "第 0 个应写到上限才滚");
        assert!(
            base_len < FILE_MAX_BYTES + 64 * 1024,
            "不该显著超出上限（说明判据没在每次写时生效）"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 单行本身就超过上限时**必须仍能写下去**（trace 满载、开原始日志时单行可达几十 KB 以上）。
    /// 若判据写成「写完不能超」，这一行会把写入永久卡住。
    #[test]
    fn a_single_line_larger_than_the_cap_is_still_written() {
        let dir = tmp("big_line");
        let mut log = OpenLog::open(dir.clone(), "2026-08-27".into()).unwrap();
        let big = "z".repeat(FILE_MAX_BYTES as usize + 100);
        log.write_line(big.clone());
        log.flush();

        let s = std::fs::read_to_string(dir.join("2026-08-27.jsonl")).unwrap();
        assert_eq!(s.len(), big.len() + 1, "整行都该落盘");
        // 下一行才滚（语义是「已达上限就换下一个」）
        log.write_line("next".into());
        log.flush();
        assert!(dir.join("2026-08-27.1.jsonl").exists(), "下一行应落到新分片");
        std::fs::remove_dir_all(&dir).ok();
    }
}
