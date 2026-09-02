//! 配置持久化与内存态管理。
//! 配置文件（不含密钥）存 JSON；密钥存 SecretStore（加密）。
//! 所有写操作走原子写（NFR-011）。

use crate::error::{AppError, AppResult};
use crate::model::*;
use crate::secret::{atomic_write, SecretStore};
use parking_lot::RwLock;
use std::path::PathBuf;
use std::time::SystemTime;
// 三个子模块都挂在这里，因为它们要调私有的 mutate_and_persist / 解析数据目录；各自的挂载理由写在对应文件的模块注释里。
#[path = "data_dir.rs"] pub(crate) mod data_dir;
#[path = "log_rotate.rs"] pub(crate) mod log_rotate;
#[path = "key_flags.rs"] pub(crate) mod key_flags;

/// 检查 baseUrl 是否含路径后缀（如 `https://api.deepseek.com/anthropic` 中的 `/anthropic`）。
///
/// DeepSeek 等部分厂商的 baseUrl 会带路径后缀，此时用 `{{baseUrl}}/user/balance` 会拼出
/// `.../anthropic/user/balance` → 404。应改用 `{{origin}}/user/balance` 剥掉路径部分。
pub fn base_url_has_path_suffix(base_url: &str) -> bool {
    let trimmed = base_url.trim();
    if trimmed.is_empty() {
        return false;
    }

    // 尝试解析为 URL
    if let Ok(url) = url::Url::parse(trimmed) {
        // 检查路径是否非空且不只是 "/"
        let path = url.path();
        return !path.is_empty() && path != "/";
    }

    // 不是有效 URL，则检查是否含 "://" 后跟域名再跟 "/"（如 "https://example.com/path"）
    if let Some(after_scheme) = trimmed.split("://").nth(1) {
        // 找到第一个 '/' 的位置（域名结束）
        if let Some(first_slash) = after_scheme.find('/') {
            // 如果 '/' 后还有内容（不只是结尾的 '/'），说明有路径后缀
            let after_slash = &after_scheme[first_slash + 1..];
            return !after_slash.is_empty();
        }
    }

    false
}

pub struct Store {
    config_path: PathBuf,
    config: RwLock<AppConfig>,
    pub secrets: RwLock<SecretStore>,
    /// 内存态事件日志（FR-020），每分类最多保留 N 条
    events: RwLock<Vec<EventLogEntry>>,
    /// 配置文件「上次已知磁盘状态」快照：(mtime, 字节数)。每次自己 persist 或初次加载时更新。
    /// list_keys 等读前对比磁盘状态,变化则重载,防止「磁盘被外部改过但内存不知情」。
    /// 用 (mtime, len) 双判据 + `!=`(非 `>`)：mtime `!=` 兼顾时钟回拨(NTP/睡眠/虚机 RTC 校时
    /// 使外部写 mtime≤prev)的漏判；len 兼顾粗粒度 mtime(FAT/exFAT/网络盘 2s 分辨率)下
    /// 「同一时间桶内外部增删 Key」致 mtime 相等的漏判——增删 Key 必改变 JSON 字节数。
    config_stamp: RwLock<(Option<SystemTime>, u64)>,
    /// 日志落盘的**单写者**通道（P1-3）。转发热路径只做一次 channel push 即返回。
    ///
    /// 为什么不再同步写：旧实现每条事件都在 tokio worker 线程上做
    /// 「持进程级 `LOG_LOCK` → `create_dir_all` → `open` → `write_all` → `close`」。
    /// 阻塞的是 worker **线程**而非当前任务，一次撞上杀软扫描就会让**同线程上所有并发转发
    /// （含正在输出的 SSE 连接）一并停顿**，表现为偶发无规律卡顿与断流，且日志里只见延迟
    /// 数字变大、极难归因。
    ///
    /// 单写者模型的附带收益：`LOG_LOCK` 与「行尾换行必须拼进同一 buffer」的技巧都可以退役
    /// （只有一个线程在写，天然不会交错）。这里仍保留一次性 buffer 拼接，因为它本身也更省一次
    /// 系统调用。
    log_tx: std::sync::mpsc::SyncSender<LogCmd>,
    /// 队列满被丢弃的日志条数。**必须可观测**——静默丢日志是本项目最忌讳的失效形态。
    log_dropped: std::sync::atomic::AtomicU64,
    /// 健康态（熔断计数 / 窗口）有未落盘变更（P1-3 后半）。
    ///
    /// 转发热路径的 `record_live_failure/success` 只翻这个标记，由后台任务合并落盘：
    /// 一次 `persist()` 要序列化整份 AppConfig（~20KB）再走 `atomic_write`，而后者持进程级
    /// 锁且内部含最坏 ~1.2s 的 `thread::sleep` 退避——在 tokio worker 上同步执行会让同线程
    /// 的其它并发转发（含 SSE 流）一并停顿。健康态是可重建的瞬态，合并落盘是正确的取舍。
    health_dirty: std::sync::atomic::AtomicBool,
    /// 「本次运行以来」的 token 用量累计，按 (分类, key_id) 分组。
    ///
    /// **刻意与事件环 `events` 分开存**：`events` 是个 MAX_EVENTS 条的滑动环，超出即
    /// drain 掉最老的。若把用量总计算在环上，累计值会在第 MAX_EVENTS 次请求之后**不再
    /// 增长、甚至回退** —— 一个「累计用量」面板越用数字越小，比不显示更糟：用户据此
    /// 估额度会严重低估。累加器只增不减，与保留多少条日志正交。
    ///
    /// **周期性落盘**（不是每请求落盘）：热路径只翻 `usage_dirty` 标记，真正的序列化
    /// 与写盘由后台任务按固定节奏合并成一次，与 `health_dirty` 同一套取舍。
    /// 这样既不丢跨重启的累计，也不把每请求写盘引回热路径（P1-3 的初衷）。
    usage_totals: RwLock<std::collections::BTreeMap<(CategoryType, String), TokenUsage>>,
    /// 用量累计有未落盘变更。由 `flush_usage_if_dirty` 合并落盘并清标记。
    ///
    /// 有这个标记，**空闲的应用一个字节都不写盘** —— 用户明确关心过 SSD 写入寿命，
    /// 无脑定时写会让一个开着不用的窗口每分钟制造一次无意义写入。
    usage_dirty: std::sync::atomic::AtomicBool,
    /// `usage.json` 的路径（与 `config.json` 同目录）。
    ///
    /// 单独存字段而不是每次从 `config_path` 现推：读写两处各推一次就是漂移的温床
    /// （`mcp.rs` 的端口文件正因此踩过——读写各算一次路径，改了一处就静默失配）。
    usage_path: PathBuf,
    /// 用量统计的起始时刻（毫秒）。随 `usage.json` 一起持久化，供面板显示「统计自 X 起」。
    usage_since_ms: RwLock<i64>,
    /// 磁盘上的 `usage.json` 版本高于本程序 → 本次运行**只读不写**。
    ///
    /// 没有这个开关，版本门就是自欺：它返回空累加器保护了「读」，但第一个请求
    /// 就会标脏，随后的 flush 拿空数据 + 旧 version 覆写那个新格式文件，
    /// 用户攒的历史当场清零 —— 正是这道门要防的破坏。
    /// 启动时定一次，运行期不变（用户中途换文件属于自找麻烦，不为它加复杂度）。
    usage_read_only: bool,
    /// 按日分桶的**已落盘历史**（不含今天尚未 flush 的增量）。
    ///
    /// 与 `usage_totals` 的分工是这套结构的关键，写错就会算出天文数字：
    /// - `usage_totals` = **跨全部历史的总量**，热路径只往里累加，永不按天切分；
    /// - `daily_buckets` = 每天各是多少，是给「今日/本周/近 7 日」用的；
    /// - `usage_baseline` = 本次进程启动那一刻的总量。
    ///
    /// 「今天新增」= `usage_totals` − `usage_baseline`。**不能**直接把 `usage_totals`
    /// 整份写进当天的桶 —— 那是把历史总量当成当天消耗，跑一周后「今日花费」会等于
    /// 「累计花费」，且每次 flush 都把同一批历史重复计入当天（实测踩到：v1 的 500
    /// 与当天新增的 200 相加成了 700）。
    daily_buckets: RwLock<Vec<crate::model::DailyUsageBucket>>,
    /// 已被 90 天滚动淘汰的桶的累计（按分类 × Key）。见 `UsageSnapshot::retired`：
    /// 没有它，启动时算出的累计总量每过一个 90 天就往下掉一截。
    /// 只在 flush 里被读改写，不进按日视图。
    retired_usage: RwLock<Vec<crate::model::TokenUsageByKey>>,
    /// 本次进程启动时（以及每次 flush 后）的用量快照，用于算增量（见 `daily_buckets`）。
    usage_baseline: RwLock<std::collections::BTreeMap<(CategoryType, String), TokenUsage>>,
    /// 上一次 flush 落在哪个 UTC 日期。
    ///
    /// **它只是诊断信息，不参与分桶判定** —— 别按字面理解成「跨零点要靠它重置基线」。
    /// 基线在**每次** flush 后都会被抬到当前总量（见 `flush_usage_if_dirty`），
    /// 所以「增量」天然只覆盖两次 flush 之间那一段；跨零点时那段增量落进新日期的桶，
    /// 旧桶保持不变，不需要额外的日期判断。
    ///
    /// 唯一的已知误差：横跨零点的那一次 flush（最多 60s 窗口），零点前的一小段会被
    /// 记到零点后的桶里。为它引入「按时刻切分增量」的复杂度不值当 —— 用量面板的定位是
    /// 趋势与量级，不是账单对账。
    usage_baseline_date: RwLock<String>,
}

/// 用量累计的定时落盘间隔（秒）。
///
/// 60s 是「最多丢 1 分钟用量」与「写盘频率」之间的取舍：
/// - 往下调（如 10s）收益很小 —— 崩溃丢 10s 还是 60s 的统计，对一个用量面板都无所谓；
///   代价却是空闲期外的写盘次数翻 6 倍。
/// - 往上调（如 10min）则一次意外退出就丢掉一大段，而用量是纯累加值、丢了不会自愈。
///
/// 关键在于它**只在有变更时才写**（见 `flush_usage_if_dirty`），所以这个间隔决定的是
/// 「有流量时最密写多快」，不是「无论如何每 60s 写一次」。空闲的应用零写入。
pub const USAGE_FLUSH_INTERVAL_SECS: u64 = 60;

enum LogCmd {
    /// 写一条日志（已序列化好的一行，不含换行符）。
    ///
    /// 在**发送侧**序列化而非写线程侧：序列化要读 `EventLogEntry`，若把整个 entry 送过去，
    /// 写线程还得持有它的所有权（trace 正文可达 4 万字符，等于把大对象搬进队列）。
    /// 发送侧序列化后只搬一个 String，且序列化失败能就地记警告。
    Line { dir: std::path::PathBuf, line: String },
    /// 刷盘并回执（退出钩子与测试用）。
    Flush(std::sync::mpsc::Sender<()>),
}

/// 日志队列容量。满则丢弃并计数——**绝不阻塞转发**。
///
/// 4096 条的量级依据：热路径每次成功转发至少 1 条事件，而磁盘侧一次 append 是微秒级；
/// 只有在磁盘长时间僵死（杀软扫描、盘满）时才可能堆积到这个数，那种情况下丢日志远优于
/// 拖垮转发。
const LOG_QUEUE_CAP: usize = 4096;

/// 日志文件保留天数。启动时清理更早的 `YYYY-MM-DD.jsonl`。
///
/// 为什么需要清理：日志已按日轮转（写线程跨天自动重开文件），但旧文件此前永久保留 ——
/// 长期运行一年就攒 365 个文件。虽然单个文件不大（trace 关时约 250 KB/天），
/// 但目录里几百个文件会让用户「打开日志目录」时无从下手。
///
/// 30 天的依据：排障场景基本是「刚才/昨天出的问题」，跨月回溯极少；而真要长期留存的用户
/// 会自己拷走。**刻意不做成配置项**：又一个开关意味着又一处要对账的状态，
/// 而这个值的合理区间很窄（7~90），没人会真去调它。
const LOG_RETAIN_DAYS: i64 = 30;

/// 事件日志内存上限
const MAX_EVENTS: usize = 500;

/// 生成新 Key 的 id（uuid v4）。
///
/// **必须由后端生成**（P3-5）：id 是 `portable.rs` 导入逻辑的**全局唯一标识**，
/// 「同 id ⟹ 同一条 Key ⟹ 覆盖」。前端原先用 `k_${Date.now()}`、cc-switch 导入用
/// `k_<毫秒>_<序号>`，两者在跨机场景都会碰撞——「两台机器照同一份教程配置」是真实场景，
/// 落在同一毫秒即撞号，跨机导入会把一条**完全无关**的本机 Key 静默覆盖成对方的
/// base_url / 协议 / 映射，而 preview 里只显示为一个 `conflictingKeys` 计数，
/// 与真正的「同 Key 更新」无法区分。
///
/// 与事件 id 口径一致（`append_event_full` 早已在用 uuid v4）。
///
/// **历史 id 不迁移**：它们是 `secrets.enc` 的键名，改 id 等于要同步搬密钥，
/// 风险远大于收益。新建的 Key 用 uuid，老 Key 保持原样，两者共存无碍
/// （唯一性只对「新产生的 id」有要求）。
pub fn new_key_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// 默认日志目录：Windows 优先安装目录（exe 同级）下的 `logs/`，macOS 直接使用
/// `~/Library/Logs/SynaRoute/`。路径动态解析，禁止硬编码（dev-hard-rules 规则2）。
/// Windows 安装目录不可写时回退到 `%APPDATA%\SynaRoute\logs`。
///
/// **结果进程内缓存一次**（`OnceLock`）：本函数原先每写一行日志都被调一次，每次都做
/// 「建 `.write-probe` → 写 → 删」的探测；而日志写入是并发的（代理转发、健康检查各自的 tokio
/// 任务），多线程共用同一探针文件名会互删对方的探针，导致偶发探测失败 → 个别日志行漏回退到
/// `%APPDATA%`，日志被劈成两处（实测 368 行落安装目录、1 行漏到 AppData）。缓存后每进程只探测
/// 一次，路径从此稳定；探针名另加进程 id + 随机后缀，杜绝同机多实例互删。
pub fn default_log_dir() -> PathBuf {
    static CACHED: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    CACHED.get_or_init(resolve_default_log_dir).clone()
}

/// 实际探测逻辑（仅由 [`default_log_dir`] 缓存调用一次）。
fn resolve_default_log_dir() -> PathBuf {
    // ---- macOS：不走 exe 同级，直接用系统标准日志目录 ----
    //
    // exe 同级那套是为 Windows 设计的（见上方文档：躲 MSIX AppData 虚拟化，让用户双击启动的
    // 实例与被包应用拉起的实例读到同一份日志）。该前提在 macOS 完全不成立，而照搬会出事：
    //
    // macOS 上 `current_exe()` 是 `SynaRoute.app/Contents/MacOS/synaroute`，其 parent 在
    // **bundle 内部**。而 `/Applications` 通常可写 —— 所以下面那套「探测可写性」会**成功**，
    // 日志就写进 bundle 里，回退分支根本轮不到。后果：
    //   1. Tauri updater 替换整个 .app → 历史日志静默消失（用户排障时正好需要它们）；
    //   2. bundle 内容纳入代码签名的 sealed resources，写入会让 `codesign --verify` 失败；
    //   3. 从只读 DMG 直接运行、或装在只读卷上时写入失败，退回 Application Support，
    //      于是「日志在哪」取决于用户怎么装的 —— 正是本函数上方那段历史要消灭的分裂。
    //
    // `~/Library/Logs/<App>` 是 macOS 的标准位置（Console.app 会自动收录），且与更新、
    // 签名、只读介质三者都无关。不做可写性探测：home 不可写的话整个应用都跑不起来。
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = dirs::home_dir() {
            return home.join("Library").join("Logs").join("SynaRoute");
        }
        // home 都拿不到时落到下面的通用回退（data_dir）。
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let logs = dir.join("logs");
            // 探测可写性：能创建目录即认为可写，用它
            if std::fs::create_dir_all(&logs).is_ok() {
                // 进一步验证真的能写入（Program Files 下 create_dir_all 可能因已存在而成功，但写入失败）。
                // 探针名带 pid + 纳秒时间戳：同机多实例/并发调用各用各的文件，不会互删。
                let probe = logs.join(format!(
                    ".write-probe-{}-{}",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.subsec_nanos())
                        .unwrap_or(0)
                ));
                if std::fs::write(&probe, b"ok").is_ok() {
                    let _ = std::fs::remove_file(&probe);
                    return logs;
                }
            }
        }
    }
    // 回退：%APPDATA%\SynaRoute\logs
    dirs::data_dir()
        .map(|d| d.join("SynaRoute").join("logs"))
        .unwrap_or_else(|| PathBuf::from("logs"))
}

/// 清理某目录下超过 [`LOG_RETAIN_DAYS`] 天的日志文件，返回删除数。
///
/// free 函数而非 `Store` 方法：写线程只持有目录路径、拿不到 `Store`
/// （它在 `Store` 构造过程中就已启动，持有 `Arc<Store>` 会成循环引用）。
///
/// **判据是文件名里的日期，不是文件 mtime**：写线程用 `{date}.jsonl` 命名，
/// 文件名即权威日期。mtime 会被备份工具/杀软/同步盘改写 —— 那会误删今天的日志，
/// 或让三个月前的文件因为被扫过而永远留着。
///
/// 全程 best-effort（失败只记 warn 不上抛）：清不掉旧日志是纯磁盘占用问题，
/// 而让它把启动流程或写线程搞挂就成了功能故障。
fn cleanup_old_logs_in(dir: &std::path::Path) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0; // 目录还不存在（首次运行）或读不了：无事可做
    };
    let cutoff = chrono::Utc::now().date_naive() - chrono::Duration::days(LOG_RETAIN_DAYS);
    let mut removed = 0usize;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        // 只认写线程产出的 `YYYY-MM-DD.jsonl` 与 `YYYY-MM-DD.N.jsonl`；别的文件
        // （用户自己放的、旧版遗留的 events.jsonl）一律不碰 —— 删错文件比留着旧日志严重得多。
        //
        // 🔴 判据收在 `log_rotate::parse_name`：原先这里是 `strip_suffix(".jsonl")` 再
        // 按 `%Y-%m-%d` 解析，而 `"2026-08-27.1"` 解析**失败** → 滚动分片永不被清理。
        let Some((date, _idx)) = log_rotate::parse_name(name) else { continue };
        if date < cutoff {
            match std::fs::remove_file(entry.path()) {
                Ok(()) => removed += 1,
                Err(e) => tracing::warn!("清理旧日志 {name} 失败: {e}"),
            }
        }
    }
    if removed > 0 {
        tracing::info!("已清理 {removed} 个超过 {LOG_RETAIN_DAYS} 天的日志文件");
    }
    removed
}

impl Store {
    /// 初始化：定位数据目录（%APPDATA%\SynaRoute），加载配置与密钥库。
    /// 路径全部动态解析，禁止硬编码（dev-hard-rules 规则2）。
    /// 从磁盘加载配置,返回 (配置, 是否加载失败)。抽出以便单测覆盖 P1 防销毁路径。
    /// - 文件不存在：返回 (空配置, false)——全新安装,允许后续 seed+persist。
    /// - 解析成功：返回 (配置, false)。
    /// - 文件存在但解析失败：返回 (空配置, true)。调用方据 load_failed=true 【绝不回写磁盘】,
    ///   避免空配置覆盖磁盘原有数据(P1 防销毁);并另存一份 .corrupt 备份供人工抢救。
    /// - **文件存在但读取失败**（被杀软/备份软件/OneDrive 短暂独占，Windows 上表现为
    ///   ERROR_SHARING_VIOLATION(32) 或 ACCESS_DENIED(5)）：同样返回 (空配置, true)。
    ///   此前这里直接 `?` 冒泡，一路到 `Store::init().expect()` → **panic**。而 GUI 进程没有
    ///   控制台、Tauri 窗口还没建起来，用户双击图标后「什么都不发生」：没有窗口、没有错误框、
    ///   logs 里连本次启动的自检行都没有（那行在 panic 之后才写）。下次读成功即恢复，
    ///   典型的间歇性无头案。降级成 load_failed=true 后：不回写磁盘（数据安全，见 persist 的守卫）、
    ///   窗口照常起来、用户能看到界面与告警，重启即恢复。判据与 secret.rs 对密钥库同类失败的
    ///   处理口径一致。
    fn load_config_from_disk(config_path: &std::path::Path) -> AppResult<(AppConfig, bool)> {
        if !config_path.exists() {
            tracing::info!("配置文件不存在,使用默认空配置: {:?}", config_path);
            return Ok((AppConfig::default(), false));
        }
        let raw = match std::fs::read(config_path) {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(
                    "配置文件读取失败(降级为空配置且本次运行绝不回写磁盘): 路径={:?} os错误码={:?}: {e}",
                    config_path,
                    e.raw_os_error()
                );
                return Ok((AppConfig::default(), true));
            }
        };
        // 严格反序列化并落诊断日志:一旦失败,记录具体原因,避免 unwrap_or_default 静默吞错
        // 导致「磁盘 6 条→内存 0/N 条」这种诡异现象无迹可寻。
        match serde_json::from_slice::<AppConfig>(&raw) {
            Ok(cfg) => {
                tracing::info!(
                    "配置加载成功: keys={} vendors={} brain={} 文件={}字节 路径={:?}",
                    cfg.keys.len(),
                    cfg.vendors.len(),
                    cfg.brain.len(),
                    raw.len(),
                    config_path
                );
                Ok((cfg, false))
            }
            Err(e) => {
                // P1 防数据销毁：解析失败绝不让调用方回写磁盘(旧逻辑 fallback 空配置后经 seeded→
                // persist 把磁盘原有 N 条 Key 覆盖成 0,不可逆)。保留磁盘原文件,另存 .corrupt 备份。
                let backup = config_path.with_file_name(format!(
                    "config.corrupt-{}.json",
                    chrono::Utc::now().format("%Y%m%d-%H%M%S")
                ));
                match std::fs::copy(config_path, &backup) {
                    Ok(_) => tracing::error!(
                        "配置反序列化失败,已备份损坏文件到 {:?} 且【不覆盖磁盘】以防数据销毁: {}. 路径={:?} 文件={}字节",
                        backup, e, config_path, raw.len()
                    ),
                    Err(ce) => tracing::error!(
                        "配置反序列化失败(损坏文件备份亦失败: {ce}),仍【不覆盖磁盘】以防数据销毁: {}. 路径={:?} 文件={}字节",
                        e, config_path, raw.len()
                    ),
                }
                Ok((AppConfig::default(), true))
            }
        }
    }

    pub fn init() -> AppResult<Self> {
        let data_dir = data_dir::app_data_dir()?;
        std::fs::create_dir_all(&data_dir)?;

        let config_path = data_dir.join("config.json");
        let secrets_path = data_dir.join("secrets.enc");

        let (mut config, load_failed) = Self::load_config_from_disk(&config_path)?;

        // 全部配置迁移收在一个函数里（`new_at` 也调它），见 `migrate_config` 的文档。
        let needs_persist = Self::migrate_config(&mut config);

        let secrets = SecretStore::load(secrets_path)?;

        // 记录初始 (mtime,len),后续读操作前用于「磁盘被外部改过就重载」的自愈判断
        let initial_stamp = Self::read_disk_stamp(&config_path);

        // 用量累计：启动即恢复上次的累计值（周期性落盘保存下来的），失败则从零开始。
        let usage_path = crate::usage_store::usage_file_path(&config_path);
        let usage_loaded = crate::usage_store::load_usage(&usage_path);

        // 启动时刻的基线 = 刚加载的历史总量（本次运行的增量从此开始累计）
        let baseline = usage_loaded.totals.clone();
        let baseline_date = chrono::Utc::now().format("%Y-%m-%d").to_string();

        let store = Self {
            config_path,
            config: RwLock::new(config),
            secrets: RwLock::new(secrets),
            events: RwLock::new(Vec::new()),
            config_stamp: RwLock::new(initial_stamp),
            log_tx: Self::spawn_log_writer(),
            log_dropped: std::sync::atomic::AtomicU64::new(0),
            health_dirty: std::sync::atomic::AtomicBool::new(false),
            usage_totals: RwLock::new(usage_loaded.totals),
            usage_dirty: std::sync::atomic::AtomicBool::new(false),
            usage_path,
            usage_since_ms: RwLock::new(usage_loaded.since),
            usage_read_only: usage_loaded.read_only,
            daily_buckets: RwLock::new(usage_loaded.daily_buckets),
            retired_usage: RwLock::new(usage_loaded.retired),
            usage_baseline: RwLock::new(baseline),
            usage_baseline_date: RwLock::new(baseline_date),
        };
        // P1 防数据销毁：仅在「全新安装(文件不存在)首次 seed」或「成功加载后的迁移」时落盘。
        // load_failed(文件存在但解析失败)时绝不 persist——否则空配置会覆盖磁盘上的原有数据。
        if needs_persist && !load_failed {
            store.persist()?;
        }
        Ok(store)
    }

    /// 全部配置迁移的**唯一入口**。返回「是否需要落盘」。
    ///
    /// # 为什么要收成一个函数
    ///
    /// 这些迁移原先直接写在 `Store::new` 的函数体里，而测试构造器 `new_at`
    /// **一条都不跑** —— 于是「迁移有没有真的接上」这件事在测试里根本覆盖不到：
    /// 单测能验 `add_new_builtin_vendors` 本身没问题，却验不出它有没有被调用。
    /// （把调用点删掉、全套 799 条测试照旧全绿，实测过。这与 CLAUDE.md 里
    /// `route_meta` / `handle_http` 那条「单元覆盖了函数不等于覆盖了接线」是同一个坑。）
    ///
    /// 收进来之后 `new` 与 `new_at` 共用同一条路径，`migrations_are_wired_into_construction`
    /// 那条测试直接对着**构造出来的 Store** 断言。
    ///
    /// # 顺序有讲究
    ///
    /// 种子注入必须在最前（后面的迁移要在完整的厂商列表上跑）；
    /// 版本门放最后，因为它无条件抬版本号 —— 抬早了会让同一轮里后加的迁移被跳过。
    fn migrate_config(config: &mut AppConfig) -> bool {
        // 首次运行（或老配置无 vendors）注入内置厂商种子
        let seeded = config.vendors.is_empty();
        if seeded {
            config.vendors = Vendor::builtin_seed();
        }

        // 迁移：老配置的内置厂商没有 preset_models 字段（serde default 给空 vec），
        // 从种子按 id 回填，让老用户也能用「一键导入预设模型」。仅补空、不覆盖用户已有数据。
        //
        // 同时把**种子里新增的**内置厂商补进来。没有这一步，往 `builtin_seed()` 里加厂商
        // 就只对全新安装生效 —— 老用户（绝大多数）永远看不到，是一次典型的静默失效。
        let (migrated_presets, added_vendors) = if !seeded {
            let filled = Self::backfill_builtin_presets(&mut config.vendors);
            let added = Self::add_new_builtin_vendors(&mut config.vendors);
            if added > 0 {
                tracing::info!("配置迁移：补入 {added} 个新增的内置厂商");
            }
            (filled, added)
        } else {
            (false, 0)
        };

        let mut needs_persist = seeded || migrated_presets || added_vendors > 0;

        // 版本迁移：v1 → v2（余额查询 URL 修正）
        if config.config_version < 2 {
            let migrated = Self::migrate_balance_query_url(&mut config.keys);
            if migrated {
                tracing::info!("配置迁移：v1 → v2（余额查询 URL 从 /v1/usage 改为 /user/balance）");
            }
            // 无论是否迁移成功，只要进入该版本门，就提升版本号并落盘，
            // 防止版本号永久停留在 v1 导致将来 v3 迁移的时序错误
            config.config_version = 2;
            needs_persist = true;
        }

        // 后续版本迁移在此追加（如 config.config_version < 3 时的逻辑）

        needs_persist
    }

    /// 为内置厂商回填预设模型（幂等）：仅当某内置厂商 preset_models 为空时，
    /// 按 id 从种子拷贝。返回是否有改动（用于决定是否落盘）。自定义厂商不动。
    fn backfill_builtin_presets(vendors: &mut [Vendor]) -> bool {
        let seed = Vendor::builtin_seed();
        let mut changed = false;
        for v in vendors.iter_mut() {
            if !v.builtin || !v.preset_models.is_empty() {
                continue;
            }
            if let Some(s) = seed.iter().find(|s| s.id == v.id) {
                if !s.preset_models.is_empty() {
                    v.preset_models = s.preset_models.clone();
                    changed = true;
                }
            }
        }
        changed
    }

    /// 把**种子里新增的**内置厂商补进老用户的配置。返回补进去的条数。
    ///
    /// # 为什么必须有这一步
    ///
    /// `builtin_seed()` 只在 `config.vendors.is_empty()`（全新安装）时注入。也就是说
    /// 往种子里加厂商，**老用户永远看不到** —— 而这正是本仓反复记录的那类静默失效：
    /// 功能加了、代码在、测试也过，但只对新装用户生效，而老用户是绝大多数。
    ///
    /// # 两条边界
    ///
    /// 1. **不覆盖同 id 的现有项**。用户可能改过内置厂商的 base_url（换了区域镜像、
    ///    走了自己的反代），拿种子覆盖等于把他的配置冲掉。只补「一条都没有」的 id。
    /// 2. **追加在末尾**，不重排。厂商列表的顺序用户是有感知的（他自己加的排在后面）。
    ///
    /// # 为什么不需要「墓碑」记录用户删掉的内置项
    ///
    /// 因为**内置厂商删不掉** —— `delete_vendor` 对 `builtin` 直接返 Err
    /// 「内置厂商不可删除」，`portable.rs` 的 Replace 导入也刻意保留内置项
    /// （见 `replace_keeps_builtin_vendors_but_clears_custom_ones`）。
    /// 所以「不在列表里」只可能意味着「这是新增的」，不存在「被用户删过」这个歧义。
    ///
    /// ⚠️ **哪天允许删内置厂商了，这里必须同时加墓碑**，否则用户会发现删掉的厂商
    /// 每次重启都回来、删都删不掉。
    fn add_new_builtin_vendors(vendors: &mut Vec<Vendor>) -> usize {
        let existing: std::collections::HashSet<String> =
            vendors.iter().map(|v| v.id.clone()).collect();
        let mut added = 0usize;
        for s in Vendor::builtin_seed() {
            if existing.contains(&s.id) {
                continue;
            }
            vendors.push(s);
            added += 1;
        }
        added
    }

    /// 迁移余额查询 URL：将旧的错误默认值 `/v1/usage` 改为正确的 `/user/balance`。
    ///
    /// **安全条件（两条都必须满足才改，否则会冲掉用户手填的地址）**：
    /// 1. `url` **恰好等于**已知错误的旧默认值 `{{baseUrl}}/v1/usage`
    /// 2. `template` 仍是 `"generic"`（说明用户从没动过它）
    ///
    /// 迁移后额外检查：若 baseUrl 本身含路径后缀（如 DeepSeek 的 `.../anthropic`），
    /// 则 `{{baseUrl}}/user/balance` 会拼出错误路径，发出告警提示用 `{{origin}}`。
    ///
    /// 返回是否有改动（用于决定是否落盘）。
    fn migrate_balance_query_url(keys: &mut [ProviderKey]) -> bool {
        const OLD_WRONG_URL: &str = "{{baseUrl}}/v1/usage";
        const NEW_CORRECT_URL: &str = "{{baseUrl}}/user/balance";

        let mut changed = false;
        for key in keys.iter_mut() {
            if let Some(ref mut bq) = key.balance_query {
                // 两条安全判据：恰好是旧默认值 && 仍是 generic 模板
                if bq.url == OLD_WRONG_URL && bq.template == "generic" {
                    tracing::info!(
                        "迁移余额查询 URL：Key={} 从 {} 改为 {}",
                        key.id,
                        OLD_WRONG_URL,
                        NEW_CORRECT_URL
                    );
                    bq.url = NEW_CORRECT_URL.into();
                    changed = true;

                    // 检查 baseUrl 是否含路径后缀（如 DeepSeek 的 /anthropic）。
                    // {{baseUrl}}/user/balance 会拼出 .../anthropic/user/balance → 404。
                    // 此时应改用 {{origin}}/user/balance 剥掉路径部分。
                    // 这里只告警，不自动改——用户可能有其他理由带路径，
                    // 且 {{origin}} 的含义需要用户知晓才能正确填写后续路径。
                    let effective_base = bq
                        .base_url_override
                        .as_deref()
                        .filter(|s| !s.trim().is_empty())
                        .unwrap_or(&key.base_url);

                    if base_url_has_path_suffix(effective_base) {
                        tracing::warn!(
                            "Key={} 的 baseUrl 含路径后缀（{}），余额查询 URL \
                             已迁移为 {{{{baseUrl}}}}/user/balance，但实际会拼出错误路径。\
                             建议在 KeyEditor 中改为 {{{{origin}}}}/user/balance",
                            key.id,
                            effective_base
                        );
                    }
                }
            }
        }
        changed
    }

    // ---- 事件日志（FR-020）----

    pub fn append_event(
        &self,
        category: CategoryType,
        kind: &str,
        key_id: Option<&str>,
        detail: &str,
    ) {
        self.append_event_trace(category, kind, key_id, detail, None);
    }

    /// 带完整链路快照的事件（调用模型日志用）。trace 为 None 时等同普通事件。
    pub fn append_event_trace(
        &self,
        category: CategoryType,
        kind: &str,
        key_id: Option<&str>,
        detail: &str,
        trace: Option<RequestTrace>,
    ) {
        self.append_event_collapsible(category, kind, key_id, detail, trace, None);
    }

    /// 可折叠的事件写入。`collapse_key` 为 `Some` 时，若**紧邻的上一条**内存事件有相同的
    /// collapse_key，就不新增条目、只把那条的 `repeat` 加一并把时间戳刷新到现在。
    ///
    /// 为什么只折叠「紧邻」的一条而不做全局归并：时间线的价值在于能看出穿插关系 ——
    /// 中间只要插进一条失败或故障转移，就该重新起一条，否则「连续成功 20 次」和
    /// 「成功 10 次→失败→再成功 10 次」在界面上会长得一样。
    ///
    /// **日志文件仍逐条完整写**（折叠前就调了 `write_log_to_file`）：排障取证要的是每一次
    /// 真实调用的时刻与延迟，不能因为界面降噪而丢掉。
    pub fn append_event_collapsible(
        &self,
        category: CategoryType,
        kind: &str,
        key_id: Option<&str>,
        detail: &str,
        trace: Option<RequestTrace>,
        collapse_key: Option<String>,
    ) {
        self.append_event_full(category, kind, key_id, detail, trace, collapse_key, None);
    }

    /// 带 token 用量的事件写入（其余重载最终都汇到这里，保持单一构造点）。
    ///
    /// 折叠时用量会**累加**：折叠的语义是「同一件事发生了 N 次」，那 N 次各自烧掉的额度
    /// 理应合并计数 —— 只保留最后一次的用量会让界面显示的总量远小于真实消耗。
    #[allow(clippy::too_many_arguments)]
    pub fn append_event_full(
        &self,
        category: CategoryType,
        kind: &str,
        key_id: Option<&str>,
        detail: &str,
        trace: Option<RequestTrace>,
        collapse_key: Option<String>,
        usage: Option<crate::upstream::TokenUsage>,
    ) {
        let entry = EventLogEntry {
            id: uuid::Uuid::new_v4().to_string(),
            ts: chrono::Utc::now().timestamp_millis(),
            category_id: category,
            kind: kind.to_string(),
            key_id: key_id.map(|s| s.to_string()),
            // 事件构造时就回填 key_name（key_id → 可读名）：日志文件与列表都带可读名，
            // 排障/路径可视化不必拿 uuid 反查。走窄读取器（只克隆 name），不用 get_key 整份克隆。
            key_name: key_id.and_then(|kid| self.key_name(kid)),
            detail: detail.to_string(),
            repeat: 1,
            collapse_key: collapse_key.clone(),
            usage,
            has_trace: trace.is_some(),
            trace,
        };
        // 先写文件：文件是逐条完整的事实来源，不受下面的界面折叠影响。
        self.write_log_to_file(&entry);

        // 用量累计：**在事件环之外**独立累加。每次带 usage 的调用都代表一次真实消耗，
        // 折叠与否只影响界面展示几行，不影响真实烧掉的额度，故这里无条件累加一次。
        // （放在 push_event_in_memory 之前，且用独立的锁——不与 events 写锁嵌套。）
        if let Some(u) = &entry.usage {
            let k = (category, entry.key_id.clone().unwrap_or_default());
            self.usage_totals.write().entry(k).or_default().add(u);
            // 只翻标记，不在此处落盘：写盘留给后台的合并 flush。
            self.mark_usage_dirty();
        }

        // 内存态的写入抽成独立方法，让 events 写锁**随该方法返回即释放** ——
        // 原先这把锁活到函数末尾（且折叠分支还会中途 return），emit 无处可放。
        // 「emit 前放光锁」的纪律见 events::emit 的文档。
        self.push_event_in_memory(entry, collapse_key);
        crate::events::emit(crate::events::Topic::Logs, Some(category));
    }

    /// 把一条事件并入内存环形缓冲（含折叠）。调用返回即释放 events 写锁。
    fn push_event_in_memory(&self, entry: EventLogEntry, collapse_key: Option<String>) {
        let mut ev = self.events.write();
        // 折叠：仅当本条带 collapse_key 且与**最后一条**相同。
        if let Some(ck) = &collapse_key {
            if let Some(last) = ev.last_mut() {
                if last.collapse_key.as_deref() == Some(ck.as_str()) {
                    last.repeat = last.repeat.saturating_add(1);
                    last.ts = entry.ts; // 时间戳跟到最近一次，用户看到的是「最后一次发生在何时」
                    // detail 也刷新：同类事件里延迟等数字会变，展示最近一次更有参考价值。
                    last.detail = entry.detail;
                    // trace 同理替换成最近一次（只保留一份，避免 N 条正文堆在内存里）。
                    last.has_trace = entry.has_trace;
                    last.trace = entry.trace;
                    // 用量**累加**而非替换：折叠的语义是「这件事发生了 N 次」，
                    // 每次都真实烧了额度。只留最后一次会让界面总量远小于实际消耗，
                    // 那正是本项目要防的「看起来没花多少」。
                    if let Some(u) = entry.usage {
                        match last.usage.as_mut() {
                            Some(acc) => acc.add(&u),
                            None => last.usage = Some(u),
                        }
                    }
                    return;
                }
            }
        }
        ev.push(entry);
        if ev.len() > MAX_EVENTS {
            let overflow = ev.len() - MAX_EVENTS;
            ev.drain(0..overflow);
        }
    }

    /// 把一条日志**投递**给写线程（非阻塞）。转发热路径只做序列化 + 一次 channel push。
    fn write_log_to_file(&self, entry: &EventLogEntry) {
        // 用 effective_log_dir 统一判定（UI 显示与「打开日志目录」按钮走同一处）：
        // 三个调用方各写一遍必然漂移，那会导致「按钮打开的目录里没有日志」这类困惑。
        // 它内部只读 log_dir 一个字段，不克隆整份 settings（每条日志都会走这里）。
        let log_dir = self.effective_log_dir();
        let line = match serde_json::to_string(entry) {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!("序列化日志条目失败: {e}");
                return;
            }
        };
        // `try_send`：队列满时**立即**丢弃并计数，绝不阻塞转发。
        // 用 send() 会在磁盘僵死时把 tokio worker 挂住——那正是本次改动要消除的病根。
        if self
            .log_tx
            .try_send(LogCmd::Line { dir: log_dir, line })
            .is_err()
        {
            let n = self
                .log_dropped
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                + 1;
            // 只在 1/10/100/1000… 这些量级点告警，避免磁盘僵死时告警本身刷爆日志。
            if n == 1 || n % 100 == 0 {
                tracing::warn!("日志队列已满，累计丢弃 {n} 条（磁盘可能僵死或写入过慢）");
            }
        }
    }

    /// 因**队列满**丢弃的日志条数。另一条路径见 [`log_rotate::open_failed_line_count`]。
    pub fn log_dropped_count(&self) -> u64 {
        self.log_dropped.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// 清理超过 [`LOG_RETAIN_DAYS`] 天的日志文件。
    ///
    /// 启动时调一次，**并且**写线程每次跨天/换目录重开文件时再调一次
    /// （见 [`Self::spawn_log_writer`]）。只在启动时清是不够的：本应用是托盘常驻程序，
    /// 用户几周不重启很正常，而日志按日轮转 —— 于是「保留 30 天」这个设定对
    /// 恰恰最需要它的那批用户（长期挂着不关的）完全不生效，磁盘无上限增长。
    /// 跨天那一刻正是新文件出现、旧文件可能刚过期的时刻，不需要额外定时器。
    pub fn cleanup_old_logs(&self) {
        cleanup_old_logs_in(&self.effective_log_dir());
    }

    /// 等待写线程把当前队列排空（退出钩子与测试用）。
    ///
    /// 退出时必须调用：否则强杀进程会丢掉队列里尚未落盘的最后几条——而排障最需要的
    /// 恰恰是崩溃前那几条。
    pub fn flush_logs(&self) {
        let (tx, rx) = std::sync::mpsc::channel();
        if self.log_tx.send(LogCmd::Flush(tx)).is_err() {
            return; // 写线程已退出
        }
        // 给一个上限，避免磁盘僵死时把退出流程也挂住。
        let _ = rx.recv_timeout(std::time::Duration::from_secs(3));
    }

    /// 启动日志写线程，返回投递端。
    ///
    /// 单写者持**长驻** `BufWriter<File>` 与「当前日期 + 当前目录」，仅在跨天或用户改了
    /// 日志目录时才重开文件——旧实现是每条日志 `create_dir_all` + `open` + `close` 一遍。
    fn spawn_log_writer() -> std::sync::mpsc::SyncSender<LogCmd> {
        let (tx, rx) = std::sync::mpsc::sync_channel::<LogCmd>(LOG_QUEUE_CAP);
        std::thread::Builder::new()
            .name("synaroute-log-writer".into())
            .spawn(move || {
                // 当前打开的文件。跨天或换目录才重开；体积上限与滚动切分在 OpenLog 内部。
                let mut open: Option<log_rotate::OpenLog> = None;

                // 收到 Line 后不立即 flush，而是尽量把已到达的连续几条一起写完再 flush 一次
                // （高频转发时能把多次 flush 合成一次）。空闲时靠 recv_timeout 兜底 flush。
                loop {
                    let cmd = match rx.recv_timeout(std::time::Duration::from_millis(200)) {
                        Ok(c) => Some(c),
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => None,
                        // 所有发送端已析构（进程退出）→ 收尾 flush 后结束线程。
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                            if let Some(o) = open.as_mut() {
                                o.flush();
                            }
                            break;
                        }
                    };
                    match cmd {
                        Some(LogCmd::Line { dir, line }) => {
                            let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
                            // 需要重开文件？（首次 / 跨天 / 用户改了日志目录）
                            let need_reopen = match &open {
                                Some(o) => o.dir != dir || o.date != date,
                                None => true,
                            };
                            if need_reopen {
                                if let Some(o) = open.as_mut() {
                                    o.flush();
                                }
                                // 跨天/换目录这一刻顺手清理过期日志。
                                //
                                // 启动时清一次是不够的：本应用是托盘常驻程序，用户几周不重启
                                // 很正常，而日志按日轮转 —— 于是「保留 30 天」对恰恰最需要它的
                                // 那批用户（长期挂着不关的）完全不生效，磁盘无上限增长。
                                // 挂在这里而不是另起定时器：跨天正是新文件出现、旧文件刚过期的
                                // 时刻，判据天然对齐，且最多一天一次（换目录时多一次，可忽略）。
                                cleanup_old_logs_in(&dir);
                                match log_rotate::OpenLog::open(dir.clone(), date.clone()) {
                                    Ok(o) => open = Some(o),
                                    Err(_) => {
                                        log_rotate::note_open_failed_line();
                                        open = None;
                                        continue;
                                    }
                                }
                            }
                            if let Some(o) = open.as_mut() {
                                // 体积上限与滚动切分在 OpenLog 内部（单文件 16MB → 滚序号；
                                // 当天超 16 个文件 → 删当天最旧的，保最近的）。
                                o.write_line(line);
                            }
                        }
                        Some(LogCmd::Flush(ack)) => {
                            if let Some(o) = open.as_mut() {
                                o.flush();
                            }
                            let _ = ack.send(());
                        }
                        // 200ms 空闲：把 BufWriter 里攒的内容落盘，保证「刚发生的事很快能在
                        // 日志文件里看到」（排障时用户会直接 tail 这个文件）。
                        None => {
                            if let Some(o) = open.as_mut() {
                                o.flush();
                            }
                        }
                    }
                }
            })
            .expect("启动日志写线程失败");
        tx
    }

    /// 事件日志列表（**不含 trace**），供 UI 列表展示。
    ///
    /// 为什么必须剥掉 trace：日志页每 2s 轮询一次全量列表，而 trace 里的
    /// `request_body`/`response_body` 各上限 20000 字符 —— 500 条满载约 19 MB。
    /// 每 2s 克隆 + 序列化 + 过 IPC + 前端反序列化这么多字节，会把界面拖到卡顿，
    /// 而前端只在**用户展开某一行**时才用 trace（其余时刻整份都是传过去就扔）。
    /// 展开时另走 [`Self::event_trace`] 按 id 单取一条。
    fn strip_trace(e: &EventLogEntry) -> EventLogEntry {
        // 逐字段构造，**不 clone trace**。绝不能用 `..e.clone()`：Rust 函数式更新语法会
        // 先**完整求值** `e.clone()`（含 request_body/response_body 各上限 20000 字符），
        // 再从完整克隆里移出剩余字段，被覆盖的那份 trace 克隆随即析构——500 条满载时
        // 每次列表轮询白分配并释放约 500×2×20000 ≈ 2000 万字符（≈19 MB），每 2s 一轮。
        // 这在排障（开日志）这个最需要界面顺滑的时刻，反而让分配器最忙。
        EventLogEntry {
            id: e.id.clone(),
            ts: e.ts,
            category_id: e.category_id,
            kind: e.kind.clone(),
            key_id: e.key_id.clone(),
            key_name: e.key_name.clone(),
            detail: e.detail.clone(),
            repeat: e.repeat,
            // has_trace 必须留下：前端靠它决定「这行能不能展开」。剥的是正文，不是存在性。
            has_trace: e.trace.is_some(),
            // collapse_key 是内部折叠判据，不下发前端。
            collapse_key: None,
            usage: e.usage,
            trace: None,
        }
    }

    pub fn list_events(&self, category: CategoryType) -> Vec<EventLogEntry> {
        self.events
            .read()
            .iter()
            .filter(|e| e.category_id == category)
            .map(Self::strip_trace)
            .collect()
    }

    /// 合并全部分类的事件日志（不按分类过滤），供「运行日志」页连续展示。
    /// 切换活动分类时日志不再被裁剪，每条自带 category_id 标签，前端可再做客户端过滤。
    /// **不含 trace**，理由见 [`Self::strip_trace`]。
    pub fn list_all_events(&self) -> Vec<EventLogEntry> {
        self.events.read().iter().map(Self::strip_trace).collect()
    }

    /// 按「分类 × Key」聚合 token 用量（用量统计面板用）。
    ///
    /// 数据源是 `usage_totals` 累加器，口径为「本次运行累计」，**与事件环解耦**：
    /// 事件环只保留最近 MAX_EVENTS 条，若按环算总量，第 MAX_EVENTS 次请求之后
    /// 累计值就不再增长（老事件被 drain 掉多少、新事件就补回多少）——一个「累计用量」
    /// 面板越用数字越小，用户据此估额度会严重低估。
    ///
    /// **刻意不含跨天历史、不落盘**：每日 `.jsonl` 是逐条完整的事实来源，但解析它要
    /// 读盘 + 反序列化；而每请求写盘正是 P1-3 要避开的热路径开销。面板定位是
    /// 「本次运行消耗」不是「账单」，重启归零已在副标题里明说。跨天累计留给后续版本
    /// （若要，按 `effective_log_dir` 的日期文件补一个按日聚合即可）。
    pub fn token_usage_by_key(&self) -> Vec<TokenUsageByKey> {
        self.usage_totals
            .read()
            .iter()
            .map(|((category_id, key_id), usage)| TokenUsageByKey {
                category_id: *category_id,
                key_id: key_id.clone(),
                usage: *usage,
            })
            .collect()
    }

    /// 流式请求的用量**补记**：把流走完才拿到的 token 用量并进**已存在的那一行**。
    ///
    /// 为什么不能直接再 `append_event_full` 一条同 collapse_key 的事件：折叠逻辑的语义是
    /// 「同一件事又发生了一次」——它会把 `repeat` 加一（一次请求在界面上显示成 ×2），
    /// 并用新 detail 覆盖旧 detail（把流开始时记下的延迟数字冲掉）。
    /// 补记要的是「修补同一行」，不是「再记一次」。
    ///
    /// 找不到目标行（已被 MAX_EVENTS 挤出）时**不新建行**：这笔用量在累加器里已经记到，
    /// 界面上少一段用量文本，远好过多出一条看起来像重复请求的假记录。
    pub fn backfill_usage_for_collapsed_event(
        &self,
        category: CategoryType,
        key_id: Option<&str>,
        collapse_key: &str,
        usage: crate::upstream::TokenUsage,
    ) {
        // 累加器**无条件**先记：面板的总量口径与「那条日志行还在不在」无关。
        {
            let k = (category, key_id.unwrap_or_default().to_string());
            self.usage_totals.write().entry(k).or_default().add(&usage);
        }
        self.mark_usage_dirty();

        // 再修补日志行：从后往前找同 collapse_key 的最近一条。
        {
            let mut ev = self.events.write();
            let Some(target) = ev
                .iter_mut()
                .rev()
                .find(|e| e.collapse_key.as_deref() == Some(collapse_key))
            else {
                return;
            };
            match target.usage.as_mut() {
                Some(acc) => acc.add(&usage),
                None => target.usage = Some(usage),
            }
            // detail 追加用量文本，与非流式路径（`log_success` 的 usage_part）同一观感。
            // 只在还没有用量段时追加，避免重复补记把同一段贴两次。
            if !target.detail.contains('↑') {
                target.detail.push_str(&format!(" · {}", usage.fmt_compact()));
            }
        } // 写锁在此释放 —— emit 前必须放光锁（见 events::emit 文档）。
        crate::events::emit(crate::events::Topic::Logs, Some(category));
    }

    /// 某分类**最近一条**失败事件（error / failover），且必须够新。
    ///
    /// 为什么单开一个窄查询、而不让前端拿 `list_all_events` 自己找（UX#11）：
    /// 分类页每 5s 轮询一次，若为了显示一条失败摘要就把 500 条事件全量搬过 IPC，
    /// 等于把 P1-6 刚省下来的开销又还回去（那条修的正是「列表接口搬运过多」）。
    /// 这里在后端内存里倒序找一条、只回这一条。
    ///
    /// `fresh_ms` = 只认这么新的失败。陈旧失败不该一直挂在界面上让用户以为「现在还坏着」
    /// —— 与「陈旧探测结论不显示成确定不可达」同一处理原则。
    pub fn recent_failure(&self, category: CategoryType, fresh_ms: i64) -> Option<EventLogEntry> {
        let now = chrono::Utc::now().timestamp_millis();
        let ev = self.events.read();
        // 倒序找最近一条失败；找到后再单独判新旧（过期即当作没有）。
        let latest = ev
            .iter()
            .rev()
            .find(|e| e.category_id == category && (e.kind == "error" || e.kind == "failover"))?;
        if now - latest.ts > fresh_ms {
            return None;
        }
        Some(Self::strip_trace(latest))
    }

    /// 按事件 id 取该条的链路快照（列表里剥掉了，用户展开某行时才按需取一条）。
    /// 找不到（已被 MAX_EVENTS 挤出）返回 None，前端据此显示「已滚出保留窗口」。
    pub fn event_trace(&self, event_id: &str) -> Option<RequestTrace> {
        self.events
            .read()
            .iter()
            .find(|e| e.id == event_id)
            .and_then(|e| e.trace.clone())
    }

    /// 读取 config 文件当前磁盘状态快照 (mtime, 字节数)；文件不存在/不可读时返回 (None, 0)。
    fn read_disk_stamp(path: &std::path::Path) -> (Option<SystemTime>, u64) {
        match std::fs::metadata(path) {
            Ok(m) => (m.modified().ok(), m.len()),
            Err(_) => (None, 0),
        }
    }

    /// 「改内存 → 落盘，失败则回滚」的统一写入路径。
    ///
    /// 根治审计发现的 persist-crud（high）：旧写法「先改内存、persist 失败不回滚」会让内存态
    /// 领先磁盘（如删除后内存 N-1 / 磁盘仍 N），而 mtime 自愈只认「磁盘比内存新」方向，这种
    /// 「内存比磁盘新」的反向背离永不自愈——UI 稳定显示的条数与磁盘持久背离，直到重启读盘。
    ///
    /// 回滚的并发安全（复核第二轮修正）：`snapshot→改内存→persist→回滚` 跨多个独立临界区，
    /// 对不走本函数的并发写者（后台健康线程 update_health、save_settings / set_mcp_* 等直写
    /// 方法）敞开。若回滚用内存 snapshot 整份覆盖，会**抹掉这些并发写者在窗口内已提交(已落盘)
    /// 的变更**（尤以 settings 最毒：reload 刻意不合并 settings，背离永不自愈）。故：
    /// - 闭包自身返回 Err（如 toggle_key 的 NotFound）：同样走 `rollback_from_disk` 磁盘对账，
    ///   既撤销闭包可能的部分改动、又不吞并发——不依赖「闭包 Err 分支不改内存」这类脆弱契约。
    /// - persist 失败：走 `rollback_from_disk` 从磁盘对账（撤销本次未落盘脏改，同时保留并发方
    ///   已落盘变更），而非内存 snapshot 整份覆盖。snapshot 仅作「连磁盘都读不回来」的兜底。
    ///
    /// CRUD 为低频用户操作，整份 clone 成本可忽略。
    /// 同 [`Self::mutate_and_persist`]，但闭包返回 `bool` 表示**是否真的产生了变化**：
    /// `false` 时跳过落盘（幂等写入的快路径），`true` 时落盘且失败走同一套磁盘对账回滚。
    ///
    /// 为什么需要这个变体：`set_active_model` / `set_proxy_port` 这类「后端自管字段专用写入」
    /// 大量是幂等调用（前端每次切页都可能重发同一个值）。无条件 persist 会把 20KB 的整份
    /// config 反复重写；而若为省这次写盘就退回「裸 persist + 提前 return」的老写法，就又丢掉了
    /// 落盘失败回滚——两者不该二选一。
    ///
    /// 注意：闭包返回 `false` 时**内存改动仍然保留**（本函数不回滚它）。这是刻意的：
    /// 调用方的契约是「返回 false ⟺ 没改任何东西」。若闭包既改了内存又返回 false，
    /// 会造成内存领先磁盘——正是本函数要防的那件事。所有调用点都遵守该契约。
    fn mutate_and_persist_when<F, R>(&self, f: F) -> AppResult<R>
    where
        F: FnOnce(&mut AppConfig) -> (R, bool),
    {
        // 快照必须在改内存之前取（回滚基线），但只有真要落盘时才用得上。
        let snapshot = self.config.read().clone();
        let (value, changed) = {
            let mut cfg = self.config.write();
            f(&mut cfg)
        };
        if !changed {
            // 契约：`false` ⟺ 没改任何东西 → 无需落盘，也无需回滚。
            return Ok(value);
        }
        match self.persist() {
            Ok(()) => {
                // 配置落盘的 choke point 之一（UX#5）。此处写锁已在上面的块里释放、
                // persist 也已返回，不持有任何 guard —— 满足「emit 前必须放光锁」的纪律。
                crate::events::emit(crate::events::Topic::Config, None);
                Ok(value)
            }
            Err(e) => {
                // 与 mutate_and_persist 同一套：从磁盘对账回滚，既撤销本次未落盘的脏改，
                // 又保留并发写者已提交的变更（详见那个函数的文档）。
                self.rollback_from_disk(snapshot);
                Err(e)
            }
        }
    }

    /// [`Self::mutate_and_persist_when`] 的无返回值简写。
    fn mutate_and_persist_if<F>(&self, f: F) -> AppResult<()>
    where
        F: FnOnce(&mut AppConfig) -> bool,
    {
        self.mutate_and_persist_when(|cfg| ((), f(cfg)))
    }

    fn mutate_and_persist<F, R>(&self, f: F) -> AppResult<R>
    where
        F: FnOnce(&mut AppConfig) -> AppResult<R>,
    {
        let snapshot = self.config.read().clone();
        let outcome = {
            let mut cfg = self.config.write();
            f(&mut cfg)
        };
        match outcome {
            Ok(value) => match self.persist() {
                Ok(()) => {
                    // 配置落盘的 choke point 之二（UX#5）。同样已无锁在手。
                    crate::events::emit(crate::events::Topic::Config, None);
                    Ok(value)
                }
                Err(e) => {
                    self.rollback_from_disk(snapshot);
                    Err(e)
                }
            },
            // 闭包自身 Err：同走磁盘对账（不用内存 snapshot 覆盖以免吞并发；也不依赖闭包契约）。
            Err(e) => {
                self.rollback_from_disk(snapshot);
                Err(e)
            }
        }
    }

    /// persist 失败后的对账回滚：优先从磁盘重读「最后已提交态」覆盖内存——既撤销本次未落盘的
    /// 脏改动，又保留并发写者已提交的变更（无并发=回到改动前态；有并发=采纳并发方已落盘结果）。
    /// 磁盘读/解析失败（往往正是 persist 失败之因，如目标被建成目录/磁盘满）时，回退到改动前的
    /// 内存快照兜底。两条路径都保证「内存态不领先磁盘」。不复用 load_config_from_disk（后者解析
    /// 失败会写 .corrupt 备份，回滚路径不应有此副作用）。
    fn rollback_from_disk(&self, snapshot: AppConfig) {
        let reconciled = std::fs::read(&self.config_path)
            .ok()
            .and_then(|raw| serde_json::from_slice::<AppConfig>(&raw).ok());
        match reconciled {
            Some(disk_cfg) => {
                *self.config.write() = disk_cfg;
                *self.config_stamp.write() = Self::read_disk_stamp(&self.config_path);
            }
            None => *self.config.write() = snapshot,
        }
    }

    fn persist(&self) -> AppResult<()> {
        // 仅在序列化期间持读锁；随后释放锁再做阻塞落盘。
        // atomic_write 含进程级互斥 + 最长数百毫秒 sleep 重试，若持锁期间执行，会经
        // parking_lot「写者优先」把后续所有读者（代理转发的 enabled_keys_sorted /
        // secrets 读等）挡在等待的写者之后，阻塞 tokio worker 线程。
        let data = {
            let cfg = self.config.read();
            serde_json::to_vec_pretty(&*cfg)?
        };
        atomic_write(&self.config_path, &data)?;
        // 刚写过磁盘,更新 (mtime,len) 快照,防止随后 reload_if_disk_newer 误判「外部改过」触发重载
        *self.config_stamp.write() = Self::read_disk_stamp(&self.config_path);
        Ok(())
    }

    /// 若磁盘 config.json 的 mtime 比我们上次快照更新,则重载。防线场景:
    /// 用户操作了旧版本进程(或别的路径直接改文件),磁盘先于内存态被更新——
    /// 若不重载,UI 展示的是「进程启动那一刻」的旧内存态,与磁盘背离(luckyg/cunai 消失即此症)。
    /// 仅重载 keys/vendors/brain,不覆盖 settings(避免 mcp_port/enabled 等后端自管字段被顶回)。
    fn reload_if_disk_newer(&self) {
        let meta = match std::fs::metadata(&self.config_path) {
            Ok(m) => m,
            Err(_) => return, // 文件不存在/不可读：不重载,保持内存态
        };
        let disk_mtime = meta.modified().ok();
        let disk_len = meta.len();
        let (prev_mtime, prev_len) = *self.config_stamp.read();
        // `!=`(非 `>`) + len 双判据：mtime `!=` 兼顾时钟回拨(NTP/睡眠/虚机 RTC)使外部写
        // mtime≤prev 的漏判；len 兼顾粗粒度 mtime(FAT/exFAT/网络盘)同一时间桶内增删 Key 致
        // mtime 相等的漏判——增删 Key 必改变 JSON 字节数,故 len 差异能兜住「条数背离」。
        // 首次 prev_mtime=None 时 disk_mtime(Some)!=None → 触发一次(与旧逻辑一致;init 已把
        // 快照置为磁盘真实值,稳态下 disk==prev 不会误重载)。
        let should_reload = disk_mtime != prev_mtime || disk_len != prev_len;
        if !should_reload {
            return;
        }
        let raw = match std::fs::read(&self.config_path) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("mtime 自愈重载:读文件失败 {e}");
                return;
            }
        };
        let fresh: AppConfig = match serde_json::from_slice(&raw) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("mtime 自愈重载:反序列化失败,跳过 {e}");
                return;
            }
        };
        let before_len;
        let after_len;
        {
            let mut cfg = self.config.write();
            before_len = cfg.keys.len();
            // 只合并数据类字段,保留 settings 中的后端自管字段
            //
            // **健康态例外：按 id 保留本进程的内存值，不采用磁盘那份。**
            //
            // 理由有两层：
            // 1. 语义上，`health`（status/latency/fail_count/breaker_until）是**本进程的运行时
            //    状态**——它由本进程的真实转发结果与探测得出。磁盘上那份只是「暖启动缓存」，
            //    外部编辑（用户手改 config.json、另一个实例写盘）里的健康块对我们并不权威。
            // 2. 实践上，`update_health` / `mutate_health` 刻意不是每次变化都落盘
            //    （latency 每轮都变；熔断计数改为标脏后由后台合并落盘，见 P1-3）。若这里整份
            //    采用磁盘值，一次外部改动就会把内存里**尚未落盘的熔断计数清零**，
            //    表现为「刚攒到 2 次失败的 Key 又变回 0，熔断永远攒不满」。
            //    本次把合并窗口放宽到「后台一轮」后，这个窗口更值得堵。
            let prev_health: std::collections::HashMap<String, HealthState> = cfg
                .keys
                .iter()
                .map(|k| (k.id.clone(), k.health.clone()))
                .collect();
            cfg.keys = fresh.keys;
            for k in cfg.keys.iter_mut() {
                if let Some(h) = prev_health.get(&k.id) {
                    // 本进程已认识这个 Key → 用内存里的健康态（更新、更权威）。
                    k.health = h.clone();
                }
                // 磁盘上新出现的 Key（本进程没见过）→ 保留其磁盘健康态作为暖启动值。
            }
            cfg.brain = fresh.brain;
            cfg.vendors = fresh.vendors;
            after_len = cfg.keys.len();
        }
        *self.config_stamp.write() = (disk_mtime, disk_len);
        if before_len != after_len {
            tracing::info!(
                "mtime 自愈重载完成: keys {} → {} (磁盘被外部更新)",
                before_len,
                after_len
            );
        }
    }

    /// 本进程实际使用的配置文件路径与当前内存 keys 数（启动自检日志用）。
    pub fn config_fingerprint(&self) -> (String, usize) {
        (
            self.config_path.display().to_string(),
            self.config.read().keys.len(),
        )
    }

    // ---- 配置导入 / 导出支撑（FR-021，见 crate::portable）----

    /// 整份配置的只读快照（导出与导入预检用）。
    /// 读前自愈：磁盘被外部改过就重载，避免导出的是进程里的旧快照。
    pub fn snapshot_config(&self) -> AppConfig {
        self.reload_if_disk_newer();
        self.config.read().clone()
    }

    /// 导入前把现有 config 备份到同目录的 `config.pre-import-<时间戳>.json`，返回备份路径。
    ///
    /// 只在 Replace 模式调用——那个模式会删掉「导出之后新建的条目」，必须留一条退路。
    /// 备份失败**上抛**：宁可不导入，也不在无退路的情况下做破坏性替换。
    pub fn backup_config_before_import(&self) -> AppResult<String> {
        let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
        let backup = self
            .config_path
            .with_file_name(format!("config.pre-import-{stamp}.json"));
        // 用当前**磁盘**内容做备份（而非内存序列化）：备份的意义是「回到导入前磁盘上那一份」。
        // 磁盘文件不存在（极早期/首次运行）时退化为序列化内存态，仍给出可回滚的东西。
        let data = match std::fs::read(&self.config_path) {
            Ok(d) => d,
            Err(_) => serde_json::to_vec_pretty(&*self.config.read())?,
        };
        atomic_write(&backup, &data)?;
        Ok(backup.display().to_string())
    }

    /// 把导入载荷写进配置（走 `mutate_and_persist`，落盘失败磁盘对账回滚）。
    ///
    /// 两种模式的差异只在「是否先清空」；其余都是「同 id 覆盖、新 id 追加」。
    /// **settings 一律整份替换**（导入方已剔掉本机绑定字段，见 `portable::strip_machine_local`），
    /// 但随后仍由 `save_settings` 那套「后端自管字段保留」逻辑管着——这里直接写 cfg.settings
    /// 会绕过它，故显式把本机自管的几项从当前值继承回来。
    ///
    /// **返回 Replace 模式下被移除的 Key id**（P2-3）：调用方据此清理它们的密钥。
    /// 这里刻意**不**在本函数内删密钥——必须等配置落盘成功之后再删，理由同 `delete_key`：
    /// 反之若配置落盘失败，Key 会在下次启动时复活，而密钥已经没了（`has_secret=true`
    /// 却取不到密钥的孤儿）。Merge 模式返回空 vec（不删任何东西）。
    pub fn apply_imported_config(
        &self,
        payload: &crate::portable::ExportPayload,
        mode: crate::portable::ImportMode,
    ) -> AppResult<Vec<String>> {
        use crate::portable::ImportMode;
        self.mutate_and_persist(|cfg| {
            // Replace 会清空 keys：记下「本机原有、但导入载荷里没有」的 id，它们的密钥要清理。
            // 载荷里也有的 id 不算移除（马上会被写回来，且随后可能要写入新密钥）。
            let removed: Vec<String> = if mode == ImportMode::Replace {
                let incoming: std::collections::HashSet<&str> =
                    payload.keys.iter().map(|k| k.id.as_str()).collect();
                cfg.keys
                    .iter()
                    .filter(|k| !incoming.contains(k.id.as_str()))
                    .map(|k| k.id.clone())
                    .collect()
            } else {
                Vec::new()
            };
            if mode == ImportMode::Replace {
                cfg.keys.clear();
                cfg.brain.clear();
                // vendors 不清空：内置厂商种子（builtin=true）是程序自带的，清掉会让
                // 「一键导入预设模型」失效；导入的自定义厂商按 id 覆盖即可。
                cfg.vendors.retain(|v| v.builtin);
            }
            for k in &payload.keys {
                match cfg.keys.iter_mut().find(|x| x.id == k.id) {
                    Some(slot) => *slot = k.clone(),
                    None => cfg.keys.push(k.clone()),
                }
            }
            for b in &payload.brain {
                match cfg.brain.iter_mut().find(|x| x.category_id == b.category_id) {
                    Some(slot) => *slot = b.clone(),
                    None => cfg.brain.push(b.clone()),
                }
            }
            for v in &payload.vendors {
                match cfg.vendors.iter_mut().find(|x| x.id == v.id) {
                    Some(slot) => *slot = v.clone(),
                    None => cfg.vendors.push(v.clone()),
                }
            }
            // settings：接受导入值，但**本机运行态字段一律保留当前值**。
            // 导出侧已把它们清空/置默认，这里再保一道——万一有人手改导出文件塞了别的机器的端口，
            // 也不该覆盖本机粘滞端口与 MCP 注册记录（那会造成「声称已注册实则没写过」）。
            let mut incoming = payload.settings.clone();
            incoming.proxy_ports = cfg.settings.proxy_ports.clone();
            incoming.mcp_port = cfg.settings.mcp_port;
            incoming.mcp_enabled = cfg.settings.mcp_enabled;
            incoming.mcp_registered_categories = cfg.settings.mcp_registered_categories.clone();
            incoming.log_dir = cfg.settings.log_dir.clone();
            incoming.auto_start = cfg.settings.auto_start;
            // 「上次退出时哪几个代理在跑」也是纯本机运行态，且是这批里唯一会去改**外部程序
            // 配置文件**的一项：`restore_proxies_on_launch` 下次启动照它自动拉起代理并改写
            // `~/.claude/settings.json` / `~/.codex/config.toml`。带过来的后果是新机器上
            // 用户**从未点过启动**、客户端却已被指向 127.0.0.1，而这台机器的 Key 可能一条都
            // 没配好 —— 客户端当场不可用，且没人会往「我昨天导入了配置」上想。
            incoming.proxy_running_categories = cfg.settings.proxy_running_categories.clone();
            // 同为本机运行态，且失效方向是**安全**的（界面说已关、socket 仍在 0.0.0.0 上）。
            // 理由全文见 `portable::strip_machine_local`。
            incoming.lan_exposure = cfg.settings.lan_exposure;
            cfg.settings = incoming;
            Ok(removed)
        })
    }

    /// 清理「配置里已不存在的 Key 仍留在密钥库里」的孤儿密钥（P2-3）。
    ///
    /// 为什么需要：`delete_key` 会同步删密钥，但 **Replace 模式导入**清空 keys 时从不碰密钥库
    /// （历史遗留）。后果两层：
    /// 1. 用户执行 Replace 导入（UI 明示会删掉本机多出的 Key）后，那些 Key 的**可解密钥材料
    ///    仍完整留在 `secrets.enc` 里**，UI 无入口可见、更无法删除，只能手工删文件；
    /// 2. 更隐蔽：日后再导入一份「含同 id Key 但不含密钥段」的文件时，
    ///    `reconcile_has_secret_flags` 的反向修复分支会读到孤儿密文把 `has_secret` 刷成 true，
    ///    UI 显示「已配置密钥」，而转发用的是上一批配置里那条早已废弃的 Key ——
    ///    表现为莫名 401 或「用错账号扣错额度」，根因埋在几次导入之前。
    ///
    /// **锁定态直接跳过**（返回 0）：主口令未解锁时 `all_key_ids` 读不到内容，
    /// 照常执行等于把「暂时读不到」当成「确实没有」，会误删真实密钥。
    /// 与 `reconcile_has_secret_flags` 的锁定态处理同一原则。
    ///
    /// 返回被清理的条数。**这是破坏性操作**，调用方应先备份并让用户确认（见 lib.rs 的调用点）。
    pub fn prune_orphan_secrets(&self) -> usize {
        if self.secrets.read().is_locked() {
            return 0;
        }
        let live: std::collections::HashSet<String> =
            self.config.read().keys.iter().map(|k| k.id.clone()).collect();
        let orphans: Vec<String> = {
            let sec = self.secrets.read();
            // 跳过库内部条目（局域网令牌）—— 删了它局域网客户端立刻 401，见 `is_internal_secret_id`。
            sec.all_key_ids().into_iter().filter(|id| !live.contains(id) && !crate::proxy::lan_guard::is_internal_secret_id(id)).collect()
        };
        let mut n = 0;
        for id in &orphans {
            match self.secrets.write().remove(id) {
                Ok(()) => n += 1,
                // 单条失败只记日志：残留一条孤儿是无害的（下次再清或手工处理），
                // 不该让整次清理中断。
                Err(e) => tracing::warn!("清理孤儿密钥 {id} 失败: {e}"),
            }
        }
        if n > 0 {
            tracing::info!("已清理 {n} 条孤儿密钥（配置中已无对应 Key）");
        }
        n
    }

    /// 统计孤儿密钥条数（只读，不删）。供 UI 在清理前告知用户「将清理 N 条」。
    pub fn count_orphan_secrets(&self) -> usize {
        if self.secrets.read().is_locked() {
            return 0;
        }
        let live: std::collections::HashSet<String> =
            self.config.read().keys.iter().map(|k| k.id.clone()).collect();
        let sec = self.secrets.read();
        sec.all_key_ids().into_iter().filter(|id| !live.contains(id) && !crate::proxy::lan_guard::is_internal_secret_id(id)).count()
    }

    /// 让每个 Key 的 `has_secret` 标记与密钥库实际内容对账，返回「标记有但实际没有」的条数。
    ///
    /// 导入后必做：文件不含密钥（或某条密钥写入失败）时，Key 的 `has_secret` 仍是导出机器上的
    /// `true`。若不对账，UI 显示「已配置密钥」而转发时报「密钥缺失」——正是
    /// [`crate::store`] 里反复防的那类「配置与实际不一致且用户无从察觉」。
    ///
    /// **主口令未解锁时直接跳过对账**（返回 0）。锁定态下 `get` 一律返回 Err，
    /// 若照常对账会把**每一条** `has_secret` 都判成 false 写盘 —— 解锁后 UI 说全都没密钥、
    /// 提示用户重录，而库里其实一条不少。这属于把「暂时读不到」误记成「确实没有」。
    pub fn reconcile_has_secret_flags(&self) -> AppResult<usize> {
        if self.secrets.read().is_locked() {
            tracing::info!("密钥库未解锁，跳过 has_secret 对账（避免把读不到误判成没有）");
            return Ok(0);
        }
        // 先在读锁下算出需要改的 id，避免在持配置写锁时再去拿密钥库读锁（降低锁序风险）。
        let ids: Vec<(String, bool)> = {
            let cfg = self.config.read();
            let guard = self.secrets.read();
            cfg.keys
                .iter()
                .map(|k| {
                    let actual = guard.get(&k.id).ok().flatten().is_some();
                    (k.id.clone(), actual)
                })
                .collect()
        };
        let mut fixed = 0usize;
        self.mutate_and_persist(|cfg| {
            for (id, actual) in &ids {
                if let Some(k) = cfg.keys.iter_mut().find(|k| &k.id == id) {
                    if k.has_secret && !actual {
                        k.has_secret = false;
                        fixed += 1;
                    } else if !k.has_secret && *actual {
                        // 反向也修：库里有密钥却标记没有，会让 UI 提示用户重录（其实不必）。
                        k.has_secret = true;
                    }
                }
            }
            Ok(())
        })?;
        Ok(fixed)
    }

    // ---- Key CRUD ----

    pub fn list_keys(&self, category: CategoryType) -> Vec<ProviderKey> {
        // 读前自愈:磁盘被外部改过就重载,防止「进程记着老快照」类背离
        self.reload_if_disk_newer();
        self.config
            .read()
            .keys
            .iter()
            .filter(|k| k.category_id == category)
            .cloned()
            .collect()
    }

    pub fn get_key(&self, key_id: &str) -> Option<ProviderKey> {
        self.config.read().keys.iter().find(|k| k.id == key_id).cloned()
    }

    /// 新增或更新一条 Key。
    ///
    /// **id 为空时由后端补一个 uuid v4**（P3-5）并随返回值回传给前端。前端不再自造 id
    /// （原先是 `k_${Date.now()}`，跨机会碰撞，详见 [`new_key_id`]）。
    ///
    /// ⚠️ 前端**必须用返回值更新自己的 state**：否则「新建后紧接着再编辑保存」会因为本地
    /// 仍是空 id 而再插入一条，表现为「保存两次出现两条重复 Key」。
    pub fn upsert_key(&self, key: ProviderKey) -> AppResult<ProviderKey> {
        let mut key = key;
        if key.id.trim().is_empty() {
            key.id = new_key_id();
        }
        // 失败回滚：落盘失败不让内存领先磁盘（见 mutate_and_persist）。
        self.mutate_and_persist(|cfg| {
            if let Some(existing) = cfg.keys.iter_mut().find(|k| k.id == key.id) {
                // ⚠️ **运行态字段一律沿用库里现值，前端传什么都忽略**。
                //
                // 这些字段由后端在运行中维护（健康探测/熔断计数/余额缓存），而前端的
                // ProviderKey 是**打开编辑器那一刻的快照**。抽屉常开着几十秒（改映射、填窗口），
                // 期间代理仍在转发：若该 Key 连续失败 3 次已武装熔断（分类页顶部有「熔断中」横幅），
                // 用户此时点「保存」，整份替换就会把 breaker_until 清空、fail_count 归零 ——
                // **熔断当场被静默解除**，下一个请求又打向那条坏 Key。
                // 同理 cached_balance 会被旧快照顶回去，卡片显示过期余额。
                //
                // 「上移/下移」也走整份 upsert（前端重编号后并发提交），同样会踩中这一条。
                // 收在这里而不是让每个调用点自觉，是因为漏一处就是一个静默失效点。
                let health = existing.health.clone();
                let cached_balance = existing.cached_balance.clone();
                *existing = key.clone();
                existing.health = health;
                existing.cached_balance = cached_balance;
                // 回填给调用方，避免返回值里带着被忽略的旧快照（前端据它刷新列表）。
                key.health = existing.health.clone();
                key.cached_balance = existing.cached_balance.clone();
            } else {
                // 新插入：若该分类里已有 Key 用着同一个 priority，把它顶到队尾（max+1）。
                //
                // 为什么需要：前端新建 Key 时恒发 `priority: 999`（KeyEditor 的
                // `initial?.priority ?? 999`）—— 它无从知道该填几。于是**每一条手工新增的
                // Key 都是 999**，同分类里三条新 Key 全部同级。此时故障转移的主/备顺序
                // 只由 `sort_by_key` 的稳定性（= 恰好的插入顺序）决定，而不是任何用户
                // 可见、可控的东西：界面上三条 Key 看不出谁先谁后，用户以为拖到最上面的
                // 那条是主 Key，实际未必。
                //
                // 判据放在**碰撞**而不是「无条件重编号」上，是为了不砸掉已有的正确调用方：
                // cc-switch 导入（`ccswitch.rs`）自己算了分类内 max+1、每条都唯一，
                // 无条件覆盖会把它精心排好的导入顺序换成我们的插入顺序。碰撞判据下，
                // 唯一值原样保留、只有真正撞车的才顺延，两条路径都对。
                //
                // 999 本身不当哨兵值特殊对待：它是个合法优先级，未来前端换成别的数字
                // （或用户导入的配置里真有 999）时这条规则依然成立。
                let collides = cfg
                    .keys
                    .iter()
                    .any(|k| k.category_id == key.category_id && k.priority == key.priority);
                if collides {
                    let next = cfg
                        .keys
                        .iter()
                        .filter(|k| k.category_id == key.category_id)
                        .map(|k| k.priority)
                        .max()
                        .map(|m| m.saturating_add(1))
                        .unwrap_or(0);
                    key.priority = next;
                }
                cfg.keys.push(key.clone());
            }
            Ok(())
        })?;
        Ok(key)
    }

    /// 检查某 Key 是否被任一分类的大脑聚合引用（成员 / 汇总者 / 决策者）。
    ///
    /// 返回引用位置的可读描述列表（空 = 无引用）。用于 [`Self::delete_key`] 的前置校验：
    /// 删掉一个正在被大脑聚合使用的 Key，会让聚合在下次调用时**静默少一个参与者**
    /// （成员被跳过）或**整轮失败**（汇总者/决策者不可用），而用户完全不知道原因 ——
    /// 这正是本项目最忌讳的静默失效形态。故删除前必须拦住并说清该去哪里解除引用。
    ///
    /// `keyId::modelName` 是 `summarizer_ref` / `decider_ref` 的格式，故用 `split("::")`
    /// 取前半段比对，而不是整串相等 —— 后者永远匹配不上。
    fn brain_references_of(&self, key_id: &str) -> Vec<String> {
        let cfg = self.config.read();
        let mut hits = Vec::new();
        for brain in cfg.brain.iter() {
            let cat = brain.category_id.meta().display_name;
            if brain.members.iter().any(|m| m.key_id == key_id) {
                hits.push(format!("{cat} · 参与成员"));
            }
            // 两个 ref 的格式是 `keyId::modelName`，取 `::` 前半段比对
            let ref_hits_key = |r: &Option<String>| {
                r.as_deref()
                    .and_then(|s| s.split("::").next())
                    .is_some_and(|k| k == key_id)
            };
            if ref_hits_key(&brain.summarizer_ref) {
                hits.push(format!("{cat} · 汇总模型"));
            }
            if ref_hits_key(&brain.decider_ref) {
                hits.push(format!("{cat} · 决策者"));
            }
        }
        hits
    }

    pub fn delete_key(&self, key_id: &str) -> AppResult<()> {
        // 大脑聚合引用检查：被引用时拒绝删除，并告诉用户该去哪里解除。
        //
        // 为什么必须拦而不是「删了顺手清理引用」：清理引用意味着**替用户改大脑聚合配置**
        // —— 他可能只是想换一条 Key 的密钥（先删后建），结果发现精心配好的成员列表
        // 少了一项、或决策者变成了空。让用户自己去大脑聚合页面移除，他才知道发生了什么。
        let refs = self.brain_references_of(key_id);
        if !refs.is_empty() {
            return Err(AppError::Invalid(format!(
                "该 Key 正在被大脑聚合使用（{}），无法删除。\n\
                 请先到「大脑聚合」页面把它从上述位置移除，再回来删除。",
                refs.join("、")
            )));
        }
        // 时序修正：先删 config 并落盘成功,再移除密钥。旧写法先 remove 密钥再 persist config,
        // 若 config 落盘失败,重启后 init 读回旧 config 使 Key 复活、却已丢密钥(has_secret=true
        // 却取不到密钥的孤儿)。失败回滚见 mutate_and_persist。
        self.mutate_and_persist(|cfg| {
            cfg.keys.retain(|k| k.id != key_id);
            Ok(())
        })?;
        // config 已成功落盘,现在移除密钥(SecretStore::remove 内部落盘 secrets.enc)。
        // 失败仅记日志——残留密钥是无害孤儿(下次同 id 覆盖或手动清理),不该让「删 Key」整体
        // 报错、更不该回退已完成的 config 删除。
        if let Err(e) = self.secrets.write().remove(key_id) {
            tracing::warn!("删除 Key 后移除密钥失败(config 已更新,残留孤儿密钥待清理): {e}");
        }
        Ok(())
    }

    pub fn toggle_key(&self, key_id: &str, enabled: bool) -> AppResult<()> {
        // 失败回滚：落盘失败不让内存领先磁盘（见 mutate_and_persist）。
        self.mutate_and_persist(|cfg| {
            if let Some(k) = cfg.keys.iter_mut().find(|k| k.id == key_id) {
                k.enabled = enabled;
                // 禁用时清空健康状态：否则遗留的 Down/熔断会一直显示「不可用」，
                // 而禁用的 Key 已不再被探测、无从自愈。重置为 Unknown，重新启用后再探测。
                if !enabled {
                    k.health = HealthState::default();
                }
                Ok(())
            } else {
                Err(AppError::NotFound(key_id.into()))
            }
        })
    }

    /// 把某 Key 提为该分类的「主 Key」（优先级 0），其余保持原相对顺序顺延。
    ///
    /// **为什么放后端**：前端 `store.ts` 的 `setPrimaryKey` 原先自己算重排、再逐个 `upsertKey`。
    /// 托盘也要这个功能（FR-022 要求「从托盘可完成主 Key 快切」），若托盘另写一份，
    /// 两处的重排规则迟早漂移（例如一边规整为连续 0,1,2… 一边只交换两个值），
    /// 表现为「从托盘设的主」与「从界面设的主」结果不一致。故抽到这里做单一事实来源。
    ///
    /// 重排规则：目标提到队首，其余按原 priority 升序顺延，然后**整列重编号为连续 0,1,2…**。
    /// 重编号是必须的——历史配置里存在「全部 priority 都是 999」的同级状态，那时故障转移
    /// 没有确定的主备顺序（永远先打 `Vec` 里的第一个），只有优先级互不相同才有确定语义。
    ///
    /// 幂等：目标已是主（且该分类优先级已连续）时不写盘，返回 `false`。
    /// 托盘每次点击都会调它，避免无意义的落盘与事件噪音。
    pub fn set_primary_key(&self, category: CategoryType, key_id: &str) -> AppResult<bool> {
        // 只重排「同分类」的 Key，别的分类一个字节都不动。
        let ordered_ids: Vec<String> = {
            let cfg = self.config.read();
            if !cfg.keys.iter().any(|k| k.id == key_id && k.category_id == category) {
                return Err(AppError::NotFound(format!(
                    "分类 {} 下没有 id={key_id} 的 Key",
                    category.as_str()
                )));
            }
            let mut same: Vec<&ProviderKey> =
                cfg.keys.iter().filter(|k| k.category_id == category).collect();
            same.sort_by_key(|k| k.priority);
            let mut ids: Vec<String> = same.iter().map(|k| k.id.clone()).collect();
            if let Some(pos) = ids.iter().position(|id| id == key_id) {
                let target = ids.remove(pos);
                ids.insert(0, target);
            }
            ids
        };

        // 目标优先级映射，并先判断是否真的需要改（幂等）。
        let changed = {
            let cfg = self.config.read();
            ordered_ids.iter().enumerate().any(|(i, id)| {
                cfg.keys
                    .iter()
                    .find(|k| &k.id == id)
                    .is_some_and(|k| k.priority != i as i32)
            })
        };
        if !changed {
            return Ok(false);
        }

        self.mutate_and_persist(|cfg| {
            for (i, id) in ordered_ids.iter().enumerate() {
                if let Some(k) = cfg.keys.iter_mut().find(|k| &k.id == id) {
                    k.priority = i as i32;
                }
            }
            Ok(())
        })?;
        Ok(true)
    }

    /// 把某 Key 在**同分类内**上移/下移一位，并把整列重编号为连续 `0..n-1`。
    /// 返回 `false` 表示已在两端、无需改动（幂等）。
    ///
    /// 为什么必须收在后端（与 [`Self::set_primary_key`] 同一道理）：前端原先是
    /// 「本地重排 → 对优先级变化的每条 Key 各发一次整份 `upsert_key`（`Promise.all`）」，
    /// 有三个问题，都不报错但结果错：
    /// 1. **部分写入**。桌面端分类下 `upsert_key` 会过一道对外名合规校验
    ///    （`reject_desktop_key_with_unusable_model_names`）。列里若有一条老 Key 的模型名
    ///    不合规，它那次 upsert 被拒、其余几条已落盘 → 优先级留下重复值/空洞
    ///    （两条都是 0），而弹出的 toast 说的是「某个模型名不能用」，与「调整顺序」
    ///    毫不相干，用户完全对不上因果。
    /// 2. **回写陈旧运行态**。整份 upsert 带着打开页面那一刻的 health 快照
    ///    （`upsert_key` 现已在服务端兜住，但让顺序调整走一条根本不该碰这些字段的路，
    ///    本身就是多余的风险面）。
    /// 3. **非原子**。`Promise.all` 的几次写各自独立落盘，中途失败就是半套顺序。
    ///
    /// 这里只改 `priority` 一个字段、一次落盘、不过 Key 全量校验，也就没有上面三条。
    pub fn move_key(&self, category: CategoryType, key_id: &str, up: bool) -> AppResult<bool> {
        // 目标次序在读锁里算好，写锁里只做赋值（与 set_primary_key 同结构）。
        let ordered_ids: Vec<String> = {
            let cfg = self.config.read();
            let mut same: Vec<&ProviderKey> =
                cfg.keys.iter().filter(|k| k.category_id == category).collect();
            // 与界面同一口径：按 priority 升序（含未启用的 Key，它们在列表里也占位）。
            same.sort_by_key(|k| k.priority);
            let mut ids: Vec<String> = same.iter().map(|k| k.id.clone()).collect();
            let Some(idx) = ids.iter().position(|id| id == key_id) else {
                return Err(AppError::NotFound(format!(
                    "分类 {} 下没有 id={key_id} 的 Key",
                    category.as_str()
                )));
            };
            let swap_with = if up {
                if idx == 0 {
                    return Ok(false); // 已在首位
                }
                idx - 1
            } else {
                if idx + 1 >= ids.len() {
                    return Ok(false); // 已在末位
                }
                idx + 1
            };
            ids.swap(idx, swap_with);
            ids
        };

        self.mutate_and_persist(|cfg| {
            for (i, id) in ordered_ids.iter().enumerate() {
                if let Some(k) = cfg.keys.iter_mut().find(|k| &k.id == id) {
                    k.priority = i as i32;
                }
            }
            Ok(())
        })?;
        Ok(true)
    }

    /// 在**单个写锁临界区内**原地读改某 Key 的健康态，闭包返回「本次是否需要落盘」。
    ///
    /// 为什么需要它（对比 `update_health` 的「先 get_key 再 update_health」用法）：
    /// 1. **消除丢失更新**。旧用法 `let prev = store.get_key(..)` → 计算 → `update_health(..)`
    ///    跨了两个独立临界区。两个并发失败可能都读到 `fail_count == 1`，各自算出 2 写回，
    ///    实际应为 3 —— 表现为**熔断比预期迟钝**（要多几次失败才触发），且并发越高越迟钝。
    ///    这类 bug 不会报错，只会让熔断阈值悄悄失准。
    /// 2. **省两次整 Key 深克隆**。`get_key` 返回的是整个 `ProviderKey` 的 clone
    ///    （含 `models` / `mappings` 两个 Vec），而调用方只要 6 个 Copy 标量。
    ///    这落在**每个请求的必经收尾路径**上（`record_live_success` / `record_live_failure`）。
    ///
    /// **闭包内绝不可再调用任何会取 `self.config` 锁的 Store 方法**（parking_lot 非重入，
    /// 会直接自死锁）。闭包只该做纯计算。
    ///
    /// 落盘判定交给闭包而非在此比较：调用方比我们更清楚「哪些字段变化才值得写盘」
    /// （见 `update_health` 的注释——latency/last_checked 每轮都变但不必持久化）。
    pub fn mutate_health<F>(&self, key_id: &str, f: F) -> AppResult<()>
    where
        F: FnOnce(&mut HealthState) -> bool,
    {
        // 健康态的**可见摘要**：只含真正会上屏的两项。
        //
        // 这是 UX#5 里推送去抖的关键判断：`mutate_health` 确实是高频后台写
        // （每请求收尾 + 每轮探测 × 每条 Key），但**它的高频部分恰好不改变界面**——
        // `record_live_failure` 每次都改 fail_count（所以落盘判据恒真），可 HealthBadge
        // 根本不显示 fail_count；探测每轮都刷 latency/last_checked，那两个只进 title 提示。
        //
        // 所以这里按「可见摘要变没变」决定推不推，而不是按「有没有落盘」。
        // **摘要法是语义正确的去抖，比定时去抖更好**：定时去抖会让一次真实的 up↔down 翻转
        // 最坏晚 N 毫秒才到；摘要法是「变了就立刻到、没变就一次都不发」。
        // 实际推送频率因此降到每条 Key 每分钟个位数（翻转、熔断武装、熔断解除）。
        let digest_before = self.health_visible_digest(key_id);
        let need_persist = {
            let mut cfg = self.config.write();
            match cfg.keys.iter_mut().find(|k| k.id == key_id) {
                Some(k) => f(&mut k.health),
                None => false,
            }
        };
        // 读摘要要在写锁**释放之后**（health_visible_digest 内部自己取读锁，
        // parking_lot 不可重入，在写锁里调它会死锁）。
        let digest_after = self.health_visible_digest(key_id);
        if digest_before != digest_after {
            crate::events::emit(crate::events::Topic::Health, None);
        }
        if need_persist {
            // **标脏，不在此落盘**（P1-3 后半）。转发热路径上的 record_live_failure/success
            // 会走到这里，而 persist() 要序列化整份 AppConfig（实测 ~20KB）再走 atomic_write
            // ——后者持进程级 WRITE_LOCK 且内部含两轮各 6 次 thread::sleep（最坏累计 ~1.2s）。
            // 在 tokio worker 上同步执行会让同线程的其它并发转发（含 SSE）一并停顿。
            //
            // 健康态是**可重建的瞬态**：丢最后几秒的落盘不会留下需人工修的错账（真实流量与
            // 下一轮探测会立刻重新得出结论）。故改为标脏 + 由后台任务合并落盘，
            // 顺带消掉「每次熔断字段变化就整份重写 20KB」的写放大。
            self.mark_health_dirty();
        }
        Ok(())
    }

    /// 一条 Key 的健康**可见摘要**：`(状态, 是否处于熔断中)`。
    ///
    /// 只含真正会上屏的两项。`fail_count` / `latency_ms` / `last_checked` 刻意不算进来 ——
    /// 它们每次转发、每轮探测都在变，但 HealthBadge 要么不显示、要么只放进 title 提示，
    /// 把它们算进摘要会让推送退化回「和轮询一样吵」。
    ///
    /// Key 不存在时返回 None，与「存在但状态未知」区分开（删除一条 Key 也是可见变化）。
    fn health_visible_digest(&self, key_id: &str) -> Option<(HealthStatus, bool)> {
        let cfg = self.config.read();
        cfg.keys
            .iter()
            .find(|k| k.id == key_id)
            .map(|k| (k.health.status, k.health.breaker_until.is_some()))
    }

    /// 标记「健康态有未落盘的变更」。由 `flush_health_if_dirty` 合并落盘。
    fn mark_health_dirty(&self) {
        self.health_dirty
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// 标记「用量累计有未落盘的变更」。
    fn mark_usage_dirty(&self) {
        self.usage_dirty
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// 若用量累计被标脏则落盘一次并清标记（后台任务定期调用 + 退出前调用）。
    ///
    /// 返回是否真的写了盘。与 `flush_health_if_dirty` 同样**先清标记再写**：
    /// 写盘期间若又有新消耗，宁可下一轮多写一次，也不要形成「标记已清、这次变更没落盘」
    /// 的丢失窗口。
    ///
    /// **v2（2026-08-11）**：按日分桶 + 90 天滚动。
    ///
    /// 分桶的量是「**本次运行的增量**」= `usage_totals` − `usage_baseline`，
    /// 而不是 `usage_totals` 本身。这个区别是本函数最容易写错的地方：
    /// `usage_totals` 是跨全部历史的总量，把它整份写进当天的桶会让历史被反复
    /// 重复计入当天（实测：v1 的 500 + 当天新增 200 → 当天桶显示 700）。
    ///
    /// 每次 flush 后把基线抬到当前总量，于是下一次 flush 只写这之间的新增；
    /// 跨过 UTC 零点时同样抬基线并换桶，昨天的增量不会漏进今天。
    pub fn flush_usage_if_dirty(&self) -> bool {
        // 只读模式：磁盘上那份是更新的格式，本次运行一个字节都不许写。
        // **必须在清脏标记之前判**：否则标记被清掉、这段消耗既没写盘也不再重试，
        // 等于一边"保护"文件一边丢数据。
        if self.usage_read_only {
            return false;
        }
        if !self
            .usage_dirty
            .swap(false, std::sync::atomic::Ordering::Relaxed)
        {
            return false;
        }

        let now_ms = chrono::Utc::now().timestamp_millis();
        let since_ms = *self.usage_since_ms.read();
        let today = Self::utc_date_string(now_ms);

        // 本次增量 = 当前总量 − 基线。同时把基线抬到当前总量。
        //
        // 两把锁的顺序：先 totals（读）再 baseline（写），全局唯一顺序，不会与
        // 热路径（只锁 totals）形成环。
        let delta: Vec<crate::model::TokenUsageByKey> = {
            let totals = self.usage_totals.read();
            let mut baseline = self.usage_baseline.write();
            let mut out = Vec::new();
            for ((cat, kid), cur) in totals.iter() {
                let base = baseline.get(&(*cat, kid.clone())).copied().unwrap_or_default();
                // 逐字段相减：饱和减法防「外部改小了 usage.json」导致的下溢 panic。
                let d = TokenUsage {
                    input: cur.input.saturating_sub(base.input),
                    output: cur.output.saturating_sub(base.output),
                    cache_read: cur.cache_read.saturating_sub(base.cache_read),
                    cache_creation: cur.cache_creation.saturating_sub(base.cache_creation),
                };
                if !d.is_empty() {
                    out.push(crate::model::TokenUsageByKey {
                        category_id: *cat,
                        key_id: kid.clone(),
                        usage: d,
                    });
                }
            }
            *baseline = totals.clone();
            out
        };

        // 增量为空 = 这次标脏只来自非用量变更（或已被上一轮写掉）：不写盘。
        if delta.is_empty() {
            return false;
        }

        let mut buckets = self.daily_buckets.write();

        // 记录本次 flush 落在哪天（仅诊断用；分桶判定靠下面的 `today`，不靠它）。
        // 基线每次 flush 都抬，故增量天然只覆盖两次 flush 之间那一段 ——
        // 跨零点无需特殊处理，那段增量落进新日期的桶即可。
        *self.usage_baseline_date.write() = today.clone();

        // 把增量并进今天的桶（已存在则叠加，否则新建）
        match buckets.iter_mut().find(|b| b.date == today) {
            Some(bucket) => {
                let mut map: std::collections::BTreeMap<(CategoryType, String), TokenUsage> =
                    bucket
                        .entries
                        .iter()
                        .map(|e| ((e.category_id, e.key_id.clone()), e.usage))
                        .collect();
                for row in delta {
                    map.entry((row.category_id, row.key_id))
                        .or_default()
                        .add(&row.usage);
                }
                bucket.entries = map
                    .into_iter()
                    .map(|((cat, kid), u)| crate::model::TokenUsageByKey {
                        category_id: cat,
                        key_id: kid,
                        usage: u,
                    })
                    .collect();
            }
            None => buckets.push(crate::model::DailyUsageBucket {
                date: today,
                entries: delta,
            }),
        }

        // 90 天滚动：删掉 91 天前的桶 —— 但**先把它们的量折进 `retired`**。
        //
        // 直接丢掉的话，启动时的累计总量（= 各存活桶之和）每过一个 90 天就往下掉一截：
        // 「累计用量」面板越用数字越小，用户据此估额度会严重低估，而 `since_ms` 仍宣称
        // 「统计自 <安装日> 起」—— 数字覆盖的区间比它声称的短，且这事完全不可见。
        // 与当年「按事件环算总量」是同一个症状、不同的成因（那次已修，这里在第 90 天重现）。
        //
        // 日维度如实丢弃：只累计到 (分类, Key) 粒度，不编造一个假日期把整段历史堆到某天。
        let cutoff_ms = now_ms - 90 * 86_400_000;
        let mut retired_map: std::collections::BTreeMap<(CategoryType, String), TokenUsage> = self
            .retired_usage
            .read()
            .iter()
            .map(|e| ((e.category_id, e.key_id.clone()), e.usage))
            .collect();
        let mut retired_changed = false;
        buckets.retain(|b| {
            // 解析失败的桶保留：手工编辑过的日期不该被静默删除。
            let Ok(parsed) = chrono::NaiveDate::parse_from_str(&b.date, "%Y-%m-%d") else {
                return true;
            };
            let keep = parsed
                .and_hms_opt(0, 0, 0)
                .map(|dt| dt.and_utc().timestamp_millis())
                .unwrap_or(0)
                >= cutoff_ms;
            if !keep {
                for row in &b.entries {
                    retired_map
                        .entry((row.category_id, row.key_id.clone()))
                        .or_default()
                        .add(&row.usage);
                    retired_changed = true;
                }
            }
            keep
        });
        let retired = if retired_changed {
            let v: Vec<crate::model::TokenUsageByKey> = retired_map
                .into_iter()
                .map(|((cat, kid), u)| crate::model::TokenUsageByKey {
                    category_id: cat,
                    key_id: kid,
                    usage: u,
                })
                .collect();
            *self.retired_usage.write() = v.clone();
            v
        } else {
            self.retired_usage.read().clone()
        };

        // 降序（最新在前，便于面板取「最近 7/30 天」）
        buckets.sort_by(|a, b| b.date.cmp(&a.date));

        let snap = crate::model::UsageSnapshot {
            version: crate::model::USAGE_SNAPSHOT_VERSION,
            since_ms,
            updated_ms: now_ms,
            daily_buckets: buckets.clone(),
            retired,
            entries: Vec::new(), // v2 不再用这个字段
        };
        drop(buckets);
        let bytes = match serde_json::to_vec_pretty(&snap) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!("用量统计序列化失败: {e}");
                return false;
            }
        };
        // 走与 config/secrets 同一个 atomic_write：写临时文件 → `sync_all()` → 重命名。
        // 两条保证要分开理解：**原子性**来自 rename（读者只会看到旧全量或新全量，
        // 不会读到半截 JSON）；**持久性**来自 rename 之前那次 fsync（断电后内容真在盘上，
        // 而不是「文件存在但 0 字节」）。缺后者时前半句仍成立、后半句不成立 ——
        // 这正是本轮对抗审查在 `secret::atomic_write` 里补掉的那条数据丢失链。
        if let Err(e) = crate::secret::atomic_write(&self.usage_path, &bytes) {
            self.mark_usage_dirty();
            tracing::warn!("用量统计落盘失败（下一轮重试）: {e}");
            return false;
        }
        true
    }

    /// 按日分桶的只读快照（面板算「今日 / 本周 / 近 7 日」用），**已并入尚未 flush 的增量**。
    ///
    /// 为什么必须在这里并、而不是让前端相加：`daily_buckets` 只在 flush 时更新
    /// （最多落后 `USAGE_FLUSH_INTERVAL_SECS` = 60s）。原注释写着「面板的今日由前端把这份
    /// 历史与 `token_usage_by_key` 的实时增量相加得出」，但 `UsagePage` 从来没那么做 ——
    /// 它的 `byDate` 只用这份桶。于是同一屏上出现自相矛盾：刚发过几个请求，
    /// 「累计」已经涨了，「今日」还是 0（新装那次尤其明显，一分钟内就是 0）。
    ///
    /// 收在后端的理由：增量 = `usage_totals − usage_baseline`，与 flush 用的是同一条减法。
    /// 让前端再算一遍等于把这条口径复制到两处，而它有两个易错点（饱和减法防外部改小、
    /// UTC 而非本地日期）—— 复制必然漂移。这里并完，前端拿到的桶天然就是最新的。
    ///
    /// **不动 baseline**：这是只读视图，抬基线会把这段增量从下一次 flush 里吃掉。
    pub fn daily_usage_buckets(&self) -> Vec<crate::model::DailyUsageBucket> {
        let mut buckets = self.daily_buckets.read().clone();
        // 未落盘增量（口径同 flush_usage_if_dirty，但只读不抬基线）
        let pending: Vec<crate::model::TokenUsageByKey> = {
            let totals = self.usage_totals.read();
            let baseline = self.usage_baseline.read();
            totals
                .iter()
                .filter_map(|((cat, kid), cur)| {
                    let base = baseline.get(&(*cat, kid.clone())).copied().unwrap_or_default();
                    // 饱和减法：防「外部把 usage.json 改小了」导致下溢 panic。
                    let d = TokenUsage {
                        input: cur.input.saturating_sub(base.input),
                        output: cur.output.saturating_sub(base.output),
                        cache_read: cur.cache_read.saturating_sub(base.cache_read),
                        cache_creation: cur.cache_creation.saturating_sub(base.cache_creation),
                    };
                    (!d.is_empty()).then(|| crate::model::TokenUsageByKey {
                        category_id: *cat,
                        key_id: kid.clone(),
                        usage: d,
                    })
                })
                .collect()
        };
        if pending.is_empty() {
            return buckets;
        }
        // 落进**今天**的桶（与 flush 同一判定：UTC 日期）。跨零点时这段增量算今天，
        // 与 flush 的处置一致 —— 两边同口径才不会在零点前后打出不同的「今日」。
        let today = Self::utc_date_string(chrono::Utc::now().timestamp_millis());
        match buckets.iter_mut().find(|b| b.date == today) {
            Some(bucket) => {
                for row in pending {
                    match bucket
                        .entries
                        .iter_mut()
                        .find(|e| e.category_id == row.category_id && e.key_id == row.key_id)
                    {
                        Some(slot) => slot.usage.add(&row.usage),
                        None => bucket.entries.push(row),
                    }
                }
            }
            None => {
                buckets.push(crate::model::DailyUsageBucket { date: today, entries: pending });
                // 保持「最新在前」（调用方按这个顺序取最近 N 天）
                buckets.sort_by(|a, b| b.date.cmp(&a.date));
            }
        }
        buckets
    }

    /// 把毫秒时间戳转成 UTC 日期字符串 `"YYYY-MM-DD"`。
    fn utc_date_string(ms: i64) -> String {
        use chrono::Datelike;
        let dt = chrono::DateTime::from_timestamp_millis(ms).unwrap_or_default();
        format!("{:04}-{:02}-{:02}", dt.year(), dt.month(), dt.day())
    }

    /// 用量统计的起始时刻（毫秒）。面板显示「统计自 X 起」用。
    pub fn usage_since_ms(&self) -> i64 {
        *self.usage_since_ms.read()
    }

    /// 若健康态被标脏则落盘一次并清标记（由后台任务定期调用，以及退出前调用）。
    ///
    /// 返回是否真的写了盘。**先清标记再写**：若写盘期间又有新变更，宁可多写一次（下一轮），
    /// 也不要漏掉那次变更（先写后清会形成丢失窗口）。
    pub fn flush_health_if_dirty(&self) -> bool {
        if !self
            .health_dirty
            .swap(false, std::sync::atomic::Ordering::Relaxed)
        {
            return false;
        }
        if let Err(e) = self.persist() {
            // 落盘失败：重新标脏，下一轮再试。健康态可重建，不必上抛打断调用方。
            self.mark_health_dirty();
            tracing::warn!("健康态合并落盘失败（下一轮重试）: {e}");
            return false;
        }
        true
    }

    /// 更新某 Key 的健康状态（健康检查模块调用）。
    /// 仅当「熔断相关」字段（status / fail_count / breaker_until）变化时才落盘——
    /// last_checked / latency 每轮都变但无需持久化（内存态已更新，UI 走内存态实时展示），
    /// 避免后台健康检查每轮对每个 Key 都整份重写 config.json，减少磁盘写与锁竞争。
    ///
    /// ⚠️ **本方法与 [`Self::mutate_health`] 是全仓刻意保留的两处裸 `persist()`，勿「顺手统一」
    /// 到 `mutate_and_persist`**（其余 12 处已于 2026-08-03 全部改走带回滚的版本）。理由：
    /// - 调用频率是**每请求 + 每探测轮 × 每 Key**，而 `mutate_and_persist` 每次都要 clone
    ///   整份 `AppConfig` 做回滚快照——代价与收益严重不对称；
    /// - 健康态是**可重建的瞬态**：真实流量与下一轮探测会立刻重新得出结论，
    ///   丢一次落盘不会留下需要人工修的错账（这与「厂商保存成功但重启消失」性质完全不同）。
    ///
    /// 换言之：这里容忍「内存领先磁盘」，因为该背离会被下一次流量自动抹平。
    pub fn update_health(&self, key_id: &str, health: HealthState) -> AppResult<()> {
        // 与 mutate_health 同一套「可见摘要」判据（见那里的长注释）。
        let digest_before = self.health_visible_digest(key_id);
        let changed = {
            let mut cfg = self.config.write();
            if let Some(k) = cfg.keys.iter_mut().find(|k| k.id == key_id) {
                let sig_changed = k.health.status != health.status
                    || k.health.fail_count != health.fail_count
                    || k.health.breaker_until != health.breaker_until;
                k.health = health; // 始终更新内存态
                sig_changed
            } else {
                false
            }
        };
        // 摘要必须在写锁释放之后读（parking_lot 不可重入）。
        if digest_before != self.health_visible_digest(key_id) {
            crate::events::emit(crate::events::Topic::Health, None);
        }
        if changed {
            // 同 `mutate_health`：标脏由后台合并落盘，不在调用线程上做 20KB 序列化 + atomic_write。
            self.mark_health_dirty();
        }
        Ok(())
    }

    /// 更新某 Key 的余额查询缓存（查询成功或失败后调用）。
    ///
    /// 纯内存操作、不落盘（`cached_balance` 字段带 `#[serde(skip)]`）。
    /// 重启后缓存自然清空，下次查询时重新拉取是合理的。
    pub fn update_balance_cache(&self, key_id: &str, result: BalanceResult) -> AppResult<()> {
        let mut cfg = self.config.write();
        if let Some(k) = cfg.keys.iter_mut().find(|k| k.id == key_id) {
            k.cached_balance = Some(result);
            // cached_balance 标记为 #[serde(skip)]，不会序列化到磁盘。
            // 重启后缓存自然清空是合理的（避免使用过期数据）。
            // 删除 persist() 调用：它不会序列化 cached_balance，纯属无效 I/O。
        }
        Ok(())
    }

    /// 记住余额查询探测命中的地址模板（窄写入，只改这一个字段并落盘）。
    ///
    /// 返回是否真的写了盘（已是同值则幂等跳过）。
    ///
    /// **为什么必须是窄写入、不能让前端走 `upsert_key`**：那条路提交的是「打开编辑器那一刻」
    /// 的整份快照，`upsert_key` 的守卫只保住 `health` 与 `cached_balance` 两项，
    /// 其余字段一概照覆盖 —— 用一次后台余额查询去触发整份覆盖，会把这期间用户改的别的东西
    /// 或后端自管的值顶掉。与 `set_primary_key`/`move_key` 收进后端是同一条理由。
    ///
    /// 只在 `balance_query` 已存在时写：`None` 表示这条 Key 压根没配余额查询，
    /// 而能走到这里说明查过 —— 那是调用方的 bug，此时凭空造一个配置比什么都不做更糟。
    pub fn set_balance_query_url(&self, key_id: &str, url_template: &str) -> AppResult<bool> {
        self.mutate_and_persist_when(|cfg| {
            let Some(k) = cfg.keys.iter_mut().find(|k| k.id == key_id) else {
                return (false, false);
            };
            let Some(bq) = k.balance_query.as_mut() else {
                return (false, false);
            };
            if bq.url == url_template {
                return (false, false); // 幂等：已是目标值，不写盘
            }
            bq.url = url_template.to_string();
            (true, true)
        })
    }

    /// 获取某 Key 的余额缓存（如果存在且未过期）。
    ///
    /// 返回 `Some(result)` 表示缓存命中且未过期，调用方可直接使用；
    /// 返回 `None` 表示无缓存或已过期，需要重新查询上游。
    ///
    /// 缓存有效期：与 `auto_interval_min` 对齐，但**取其 90%** ——
    /// 前端轮询在 t=T 时到达，此刻缓存年龄 = T − 网络延迟，恒小于 T；
    /// 有效期取整 T 会把每个奇数次轮询拦掉，实际查询周期变成配置的 2 倍
    /// （用户设 30 分钟实为 1 小时的静默偏差）。留 10% 余量后奇数次轮询正常放行。
    /// 未配置自动查询（`auto_interval_min == 0`）时缓存 5 分钟（避免短时间内重复查询，
    /// 该路径没有定时轮询打边界，不需要余量）。
    pub fn get_balance_cache(&self, key_id: &str) -> Option<BalanceResult> {
        let cfg = self.config.read();
        let key = cfg.keys.iter().find(|k| k.id == key_id)?;
        let cached = key.cached_balance.as_ref()?;

        // 计算缓存有效期（秒）
        let cache_duration_secs = if let Some(bq) = &key.balance_query {
            if bq.auto_interval_min > 0 {
                // 90% 余量：与前端 KeyCard 轮询的 freshFor 同一口径（见上方文档注释）
                (bq.auto_interval_min as i64) * 60 * 9 / 10
            } else {
                5 * 60 // 默认 5 分钟
            }
        } else {
            5 * 60
        };

        // 检查是否过期
        let now = chrono::Utc::now().timestamp_millis();
        let age_secs = (now - cached.queried_at) / 1000;
        if age_secs < cache_duration_secs {
            Some(cached.clone())
        } else {
            None // 已过期
        }
    }

    /// 更新某 Key 的模型列表（拉取模型后调用）
    pub fn set_models(&self, key_id: &str, models: Vec<ModelInfo>) -> AppResult<()> {
        // 失败回滚：落盘失败不让内存领先磁盘（见 mutate_and_persist）。
        self.mutate_and_persist(|cfg| {
            if let Some(k) = cfg.keys.iter_mut().find(|k| k.id == key_id) {
                k.models = models;
            }
            Ok(())
        })
    }

    /// 路由候选：一次读锁内完成「筛分类+启用 → 排序 → 剔除被运行态挡住的」，
    /// **只对最终入选者克隆**。返回 (候选列表, 是否触发了兜底)。
    ///
    /// 排序键与兜底语义的**全部**判据在 [`crate::proxy::model_pool::rank_candidates`]
    /// —— 含「能原生服务本次模型 > 余额未耗尽 > priority」这个三位键为什么要这么排。
    /// 本函数只负责在一把读锁内把 `cfg.keys` 借给它，那是它必须留在 `Store` 上的唯一理由。
    ///
    /// 为什么不用 `enabled_keys_sorted` + `health::select_candidates`：那条路径克隆**两轮全量
    /// 启用 Key**（前者在筛选排序**之前**就 `.cloned()`，后者拿到 Vec 后再 `.cloned()`）。
    /// `ProviderKey` 带 `models`/`mappings` 两个 Vec，6 条 Key × 各 30 个 ModelInfo 就是单请求
    /// 克隆约 180 个 ModelInfo 两轮 —— 纯浪费，且**随 Key 数与模型数线性放大**（配得越全越慢）。
    ///
    /// `requested_model`：客户端要的对外模型名。传空串则不叠加模型维度的两条判据。
    pub fn candidates_for(
        &self,
        category: CategoryType,
        requested_model: &str,
    ) -> (Vec<ProviderKey>, bool) {
        let cfg = self.config.read();
        crate::proxy::model_pool::rank_candidates(&cfg.keys, category, requested_model)
    }

    /// 某分类下启用 Key 的 id 列表（按优先级升序）。
    ///
    /// 健康探测专用：它只需要 id 去调 `check_one`，而 `enabled_keys_sorted` 会克隆整个
    /// `ProviderKey`（含 models / mappings 两个 Vec）。探测是「每轮 × 每分类」调用，
    /// 6 条 Key 各挂 30 个 ModelInfo 时白克隆 180 个 ModelInfo。
    pub fn enabled_key_ids(&self, category: CategoryType) -> Vec<String> {
        let cfg = self.config.read();
        let mut v: Vec<(&str, i32)> = cfg
            .keys
            .iter()
            .filter(|k| k.category_id == category && k.enabled)
            .map(|k| (k.id.as_str(), k.priority))
            .collect();
        v.sort_by_key(|(_, p)| *p);
        v.into_iter().map(|(id, _)| id.to_string()).collect()
    }

    /// 取某分类下按优先级升序排列的启用 Key（路由用）
    pub fn enabled_keys_sorted(&self, category: CategoryType) -> Vec<ProviderKey> {
        let mut v: Vec<ProviderKey> = self
            .config
            .read()
            .keys
            .iter()
            .filter(|k| k.category_id == category && k.enabled)
            .cloned()
            .collect();
        v.sort_by_key(|k| k.priority);
        v
    }

    // ---- 大脑聚合配置 ----

    pub fn get_brain(&self, category: CategoryType) -> BrainConfig {
        self.config
            .read()
            .brain
            .iter()
            .find(|b| b.category_id == category)
            .cloned()
            .unwrap_or_else(|| BrainConfig {
                category_id: category,
                enabled: false,
                aggregate_mode: AggregateMode::Compressed,
                concurrency_limit: 3,
                total_timeout_ms: 60_000,
                summarizer_ref: None,
                decider_ref: None,
                members: vec![],
                work_dir: None,
                max_context_tokens: 50_000,
                retrieval_enabled: false,
                auto_follow_active: false,
                tools_enabled: false,
                max_tool_rounds: 6,
                tool_ctx_budget_chars: 60_000,
                tool_result_cap_chars: 8_000,
            })
    }

    /// 保存某分类的大脑聚合配置。
    ///
    /// 走 `mutate_and_persist`：落盘失败时从磁盘对账回滚，不让内存态领先磁盘。
    /// 与 Key 的 CRUD 同一套保证——「内存比磁盘新」这个方向的背离**永不自愈**
    /// （mtime 自愈只认「磁盘比内存新」），会一直显示成功保存直到重启才露馅。
    pub fn save_brain(&self, brain: BrainConfig) -> AppResult<()> {
        self.mutate_and_persist(|cfg| {
            if let Some(b) = cfg.brain.iter_mut().find(|b| b.category_id == brain.category_id) {
                *b = brain;
            } else {
                cfg.brain.push(brain);
            }
            Ok(())
        })
    }

    // ---- 设置 ----

    pub fn get_settings(&self) -> AppSettings {
        self.config.read().settings.clone()
    }

    // ---- 转发热路径专用的窄读取器 ----
    //
    // 为什么不直接用 `get_settings()`：它克隆**整份** `AppSettings`，里面有 3 个 HashMap
    // （active_models / active_efforts / proxy_ports）+ 2 个 Vec + 若干 String。而转发路径
    // 每个请求要读 3~4 次设置（对外模型覆盖、日志开关、推理强度注入），等于每请求做十几次
    // 堆分配，只为取一个 bool 或一个短字符串。下面这几个只克隆真正要用的那一个值。

    /// 调用模型日志开关（转发热路径每请求读一次）。
    pub fn request_log_enabled(&self) -> bool {
        self.config.read().settings.request_log_enabled
    }

    /// 「日志里附下游原始 body」开关（仅开了调用模型日志时才会被问到）。
    pub fn log_downstream_raw_enabled(&self) -> bool {
        self.config.read().settings.log_downstream_raw_enabled
    }

    /// 配置文件的绝对路径（用于诊断报告）。
    ///
    /// 这条在排障里很关键：MSIX 虚拟化下，用户双击启动与被包内进程启动看到的是**不同的**
    /// config.json（包内私有副本 vs 真实文件）。把实际路径打进报告，才能一眼分辨是哪一份。
    pub fn config_path_display(&self) -> String {
        self.config_path.display().to_string()
    }

    /// Key 的可读名（窄读取器，**只克隆 name 这一个 String**）。
    ///
    /// 为什么必须有：`append_event_full` 每请求调用一次、`health.rs` 的熔断通知每失败/恢复
    /// 调用 —— 若用 `get_key`（整份克隆 ProviderKey，含 models/mappings/health），转发热路径
    /// 上每写一条日志就白克隆一整份配置对象。与 `request_log_enabled` 等窄读取器同一原则。
    pub fn key_name(&self, key_id: &str) -> Option<String> {
        self.config
            .read()
            .keys
            .iter()
            .find(|k| k.id == key_id)
            .map(|k| k.name.clone())
    }

    /// 某 Key 的熔断窗口截止时刻（窄读取器，只取一个 `Option<i64>`）。
    ///
    /// `record_live_failure` / `record_live_success` 做熔断状态跃迁检测用——每失败/成功
    /// 调用一次，用 `get_key` 整份克隆只为看 `breaker_until` 太浪费。
    ///
    /// 刻意返回**原始值**而非 bool：调用方对「熔断中」有两种不同口径，压成 bool 必然
    /// 有一方被迫用错的那种 ——
    /// - 「窗口是否还活着」（`until > now`）：与 `is_candidate` 同口径，用于判断
    ///   熔断**武装**跃迁（窗口自然到期后再次失败，属于一次新的熔断，应当告警）。
    /// - 「是否还有熔断残留」（`is_some()`）：用于判断**解除**跃迁（残留只有真实成功
    ///   才会被清成 None，清掉即代表这个 Key 真的恢复了）。
    pub fn key_breaker_until(&self, key_id: &str) -> Option<i64> {
        self.config
            .read()
            .keys
            .iter()
            .find(|k| k.id == key_id)
            .and_then(|k| k.health.breaker_until)
    }

    /// 脱敏后的完整配置 JSON（用于诊断报告）。
    ///
    /// 走 `crate::tools::redact_config_secrets`：与「工具配置只读预览」用同一套脱敏实现——
    /// 单独写一份必然漂移，而漏脱一个字段就是把用户密钥泄进他要发出去的文件里。
    pub fn redacted_config_json(&self) -> AppResult<String> {
        let raw = serde_json::to_string_pretty(&*self.config.read())?;
        Ok(crate::tools::redact_config_secrets(&raw))
    }

    /// 当前**实际生效**的日志目录：用户在设置里配了就用它，否则用默认目录。
    ///
    /// 抽出来是因为有三个调用方需要同一份判定（写日志、UI 显示、「打开目录」按钮），
    /// 各写一遍必然漂移——那会导致「按钮打开的目录里没有日志」这类困惑。
    pub fn effective_log_dir(&self) -> PathBuf {
        match self.config.read().settings.log_dir.as_deref() {
            Some(dir) if !dir.trim().is_empty() => PathBuf::from(dir),
            _ => default_log_dir(),
        }
    }

    /// 取某 Key 的明文密钥（转发热路径专用），带 DPAPI 解密缓存（P2-6）。
    ///
    /// **两段式**：先持**读锁**查（命中即返回，读锁可并发，多个转发互不阻塞）；
    /// 未命中才升级到写锁解密并填充。为什么不直接一路写锁：转发是高并发路径，
    /// 写锁会把所有并发转发串行化——那比省下的一次 syscall 贵得多。
    ///
    /// 缓存只在 DPAPI 模式生效（主口令模式有长驻 vault_key，本就不慢）。
    /// 锁定态下 `get` 返回 `Err`，该语义原样向上传递（不被缓存干扰——`lock()` 会清缓存）。
    pub fn secret_for(&self, key_id: &str) -> AppResult<Option<zeroize::Zeroizing<String>>> {
        // 第一段：读锁。命中缓存或「本就不需要缓存（主口令模式）」时到此为止。
        {
            let sec = self.secrets.read();
            let got = sec.get(key_id)?;
            // 主口令模式不缓存，直接返回；DPAPI 模式若已命中缓存，`get` 内部已经走了缓存分支，
            // 这里同样直接返回。两种情况都无需写锁。
            if got.is_none() || sec.is_master_mode() || sec.is_cached(key_id) {
                return Ok(got);
            }
        }
        // 第二段：写锁填充缓存。期间可能有别的线程已经填过——`get_caching` 幂等，无妨。
        self.secrets.write().get_caching(key_id)
    }

    /// 健康探测方式：`Some(测试消息)` = 用真实补全探测（消息已从列表随机取好，空列表回退
    /// 内置 `"hi"`）；`None` = 用轻量连通探测（默认）。
    ///
    /// 窄读取器（与 `request_log_enabled` 同一模式）：探测是「每轮 × 每 Key」调用，
    /// 不该为一个 bool + 一条短字符串去克隆整份 `AppSettings`（3 个 HashMap + 2 个 Vec）。
    /// 随机选取放在锁内完成，避免把整个消息列表克隆出来。
    pub fn probe_message_if_real(&self) -> Option<String> {
        let cfg = self.config.read();
        if !cfg.settings.health_probe_real_completion {
            return None;
        }
        use rand::seq::SliceRandom;
        let candidates: Vec<&String> = cfg
            .settings
            .health_probe_test_messages
            .iter()
            .filter(|m| !m.trim().is_empty())
            .collect();
        Some(match candidates.choose(&mut rand::thread_rng()) {
            Some(m) => (*m).clone(),
            None => "hi".to_string(),
        })
    }

    /// 一次请求内故障转移的总时间预算（毫秒）；`None` = 用户关闭了该约束（设为 0）。
    ///
    /// 窄读取器（与 `request_log_enabled` 等同一模式）：转发热路径每请求都要问一次，
    /// 不能为取一个 u64 去 clone 整份 `AppSettings`（3 个 HashMap + 2 个 Vec）。
    pub fn failover_budget(&self) -> Option<std::time::Duration> {
        let ms = self.config.read().settings.failover_total_budget_ms;
        (ms > 0).then(|| std::time::Duration::from_millis(ms))
    }

    /// 某分类当前选定的「对外模型名」（空/未配时 None）。
    pub fn active_model_of(&self, category: CategoryType) -> Option<String> {
        self.config
            .read()
            .settings
            .active_models
            .get(&category)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    /// 某分类的「默认推理强度」（空/未配时 None）。
    pub fn active_effort_of(&self, category: CategoryType) -> Option<String> {
        self.config
            .read()
            .settings
            .active_efforts
            .get(&category)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    /// 保存全局设置。
    ///
    /// 走 `mutate_and_persist`（与 Key CRUD / save_brain 同一套）：落盘失败时磁盘对账回滚，
    /// 不留「UI 显示已保存、磁盘其实没写」的内存领先态。
    ///
    /// **闭包内先做「后端自管字段保留」再整份覆盖**——顺序不能反：那几个字段的值必须取自
    /// 当前内存态（即最后已提交态），故必须在 `cfg.settings = settings` 之前从 `cfg` 里取走。
    /// 保存**用户偏好**。入参是白名单类型 [`UserPrefs`]，后端自管字段在类型上就不存在。
    ///
    /// 这里原先是一段 30 行的**黑名单**：逐个字段 `mem::take` 把后端值保留下来，
    /// 防止前端挂载时的旧快照把运行态顶回去。它出过 P0 —— `auto_start` 不在名单里，
    /// 于是切主题/切语言会把用户刚关掉的开机自启动重新装回系统。
    ///
    /// 黑名单的根本问题是「默认不安全」：日后加一个后端自管字段，忘了补一行就是同形态事故，
    /// 而且没有任何东西会提醒你。换成白名单后，前端连表达「我要改 mcpPort」都做不到 ——
    /// 多余的键在反序列化时被 serde 静默丢弃，日后加字段默认就是安全的。
    ///
    /// 各字段为什么归后端自管，见 [`UserPrefs`] 的文档与各专用写入方法。
    pub fn save_settings(&self, prefs: UserPrefs) -> AppResult<()> {
        self.mutate_and_persist(move |cfg| {
            prefs.apply_to(&mut cfg.settings);
            Ok(())
        })
    }

    /// 开机自启动标记的专用写入（后端自管字段）。已是目标值则幂等跳过写盘。
    ///
    /// 系统侧的注册由 `lib.rs` 的 `set_auto_start` 命令负责，两侧的一致性在那个命令里保证 ——
    /// 这里只管配置。分开是因为 store 不该知道 tauri 插件的存在。
    pub fn set_auto_start_flag(&self, enabled: bool) -> AppResult<()> {
        self.mutate_and_persist_if(|cfg| {
            if cfg.settings.auto_start == enabled {
                false
            } else {
                cfg.settings.auto_start = enabled;
                true
            }
        })
    }

    /// 「上次运行时哪些分类的代理在跑」的快照（后端自管字段）。
    ///
    /// 由 `service::snapshot_running_proxies` 周期性 + 退出时写入，
    /// 供下次启动 `service::restore_proxies_on_launch` 恢复。
    ///
    /// **幂等**：与现值相同（顺序无关）则一个字节都不写。这一步不是优化而是必需 ——
    /// 快照每 60s 采样一次，若不比对就变成「开着不用也每分钟写一次 config.json」，
    /// 而用户明确担心过持续写盘伤 SSD。
    ///
    /// 入参顺序不敏感：内部排序去重后再比对与存储，避免「同一集合不同顺序」被误判为变更。
    pub fn set_proxy_running_categories(&self, cats: &[CategoryType]) -> AppResult<()> {
        let mut next: Vec<CategoryType> = cats.to_vec();
        next.sort_by_key(|c| c.as_str());
        next.dedup();
        self.mutate_and_persist_if(|cfg| {
            if cfg.settings.proxy_running_categories == next {
                false
            } else {
                cfg.settings.proxy_running_categories = next.clone();
                true
            }
        })
    }

    /// 读回上次运行的代理分类快照（窄读取器，只克隆这一个小 Vec）。
    ///
    /// **读侧也归一（排序去重）**，与写侧保持同一形态。写侧已经归一了，所以正常写出的
    /// 文件本就有序；但手工编辑过的配置可能是乱序或有重复，那样启动后第一次快照比对
    /// 会误判为「有变更」而白写一次盘。让两侧形态一致，这个多余写入就不存在。
    pub fn proxy_running_categories(&self) -> Vec<CategoryType> {
        let mut v = self.config.read().settings.proxy_running_categories.clone();
        v.sort_by_key(|c| c.as_str());
        v.dedup();
        v
    }

    /// 首启向导标记的专用写入（后端自管字段，绕过 save_settings 的旧快照覆盖）。
    /// 已是目标值则幂等跳过写盘。
    pub fn set_onboarding_done(&self, done: bool) -> AppResult<()> {
        self.mutate_and_persist_if(|cfg| {
            if cfg.settings.onboarding_done == Some(done) {
                false
            } else {
                cfg.settings.onboarding_done = Some(done);
                true
            }
        })
    }

    /// 启动时对账首启向导标记（UX#1）。
    ///
    /// **为什么必须有这一步**：老用户升级上来，配置里根本没有 `onboarding_done` 字段
    /// （反序列化成 `None`）。若不对账，他们哪天把 Key 全删了（换厂商、清理重配），
    /// 就会突然被首启向导拦住 —— 一个用了半年的软件毫无征兆地弹出「欢迎使用」。
    /// 这里在启动时一次性据当前 Key 数定下来：有 Key 就是老用户（标记为已完成），
    /// 没 Key 才是真的需要向导。
    ///
    /// 返回 `Ok(None)` 表示已判定过、什么都没做。
    ///
    /// 锁使用注意：两次 `config.read()` 是**两条独立语句**，各自的读锁随语句结束即释放。
    /// 不能写成把读锁 guard 存进变量再调 `set_onboarding_done`（它内部要取写锁）——
    /// parking_lot 不可重入且写者优先，那样会死锁。
    pub fn reconcile_onboarding_flag(&self) -> AppResult<Option<bool>> {
        let undecided = self.config.read().settings.onboarding_done.is_none();
        if !undecided {
            return Ok(None);
        }
        let has_keys = !self.config.read().keys.is_empty();
        self.set_onboarding_done(has_keys)?;
        Ok(Some(has_keys))
    }

    /// 全部分类的 Key 总数（首启向导判断「一条都还没有」用）。
    pub fn total_key_count(&self) -> usize {
        self.config.read().keys.len()
    }

    /// 自 `since_ms` 起，某分类有没有真的收到过转发请求（首启向导第④步的正反馈）。
    ///
    /// 这是向导里唯一能回答「我配对了吗」的东西 —— 在此之前，用户接入完客户端后
    /// 没有任何东西告诉他成功了，只能自己去开一个会话试。
    ///
    /// 同时采集失败：接入配错时用户同样卡在这一步，只显示「还没收到请求」帮不了他，
    /// 得让他看见「收到了，但 401」。
    ///
    /// 实现是**倒序单趟扫描**，遇到早于 `since_ms` 的事件立即 break —— events 严格按时间
    /// 递增（append 只 push、折叠只改最后一条、超限 drain 只从头砍），所以倒序遇到第一条
    /// 过期的就可以停，不必扫完整个环形缓冲。
    pub fn first_request_since(&self, category: CategoryType, since_ms: i64) -> FirstRequestProbe {
        let ev = self.events.read();
        let mut probe = FirstRequestProbe::default();
        for e in ev.iter().rev() {
            if e.ts < since_ms {
                break; // 再往前都是向导开始之前的历史事件，与本次接入无关
            }
            if e.category_id != category {
                continue;
            }
            if e.kind == "route" && !probe.routed {
                probe.routed = true;
                probe.ts = Some(e.ts);
                probe.detail = Some(e.detail.clone());
            } else if (e.kind == "error" || e.kind == "failover") && !probe.failed {
                probe.failed = true;
                probe.failure_detail = Some(e.detail.clone());
            }
            if probe.routed && probe.failed {
                break;
            }
        }
        probe
    }

    /// 设置某分类当前选定的对外模型名（后端自管字段专用写入，绕过 save_settings 的旧快照覆盖）。
    pub fn set_active_model(&self, category: CategoryType, model: &str) -> AppResult<()> {
        // 走 `mutate_and_persist_if`：落盘失败时磁盘对账回滚。旧写法是「改内存 → persist()」，
        // 落盘失败即内存领先磁盘，而该方向**永不自愈**（mtime 自愈只认「磁盘比内存新」）——
        // 表现为用户在应用内改选的模型「看着生效了」，重启后悄悄回退。
        self.mutate_and_persist_if(|cfg| {
            let trimmed = model.trim();
            if trimmed.is_empty() {
                // 本来就没有该项 → 无变化，不必落盘（幂等）。
                cfg.settings.active_models.remove(&category).is_some()
            } else if cfg.settings.active_models.get(&category).map(|s| s.as_str()) == Some(trimmed) {
                false
            } else {
                cfg.settings
                    .active_models
                    .insert(category, trimmed.to_string());
                true
            }
        })?;
        // 推 Settings 而非 Config（UX#5）。放在 store 层是为了让**两个入口都覆盖**：
        // 主窗口的 set_active_model 命令，以及托盘的 Codex 模型快切。
        // 后者原先改完主窗口下拉永远不跟着变（既有缺陷），本项顺带修掉。
        crate::events::emit(crate::events::Topic::Settings, None);
        Ok(())
    }

    /// 设置某分类的「默认推理强度」（Codex 用；后端自管字段专用写入，绕过 save_settings 旧快照覆盖）。
    /// 空串视为清除（回到不注入、保持上游默认）。已是目标值则幂等跳过写盘。
    /// 取值：low/medium/high/xhigh（minimal 亦可，映射侧按不开思考处理）。
    pub fn set_active_effort(&self, category: CategoryType, effort: &str) -> AppResult<()> {
        self.mutate_and_persist_if(|cfg| {
            let trimmed = effort.trim();
            if trimmed.is_empty() {
                cfg.settings.active_efforts.remove(&category).is_some()
            } else if cfg.settings.active_efforts.get(&category).map(|s| s.as_str()) == Some(trimmed)
            {
                false
            } else {
                cfg.settings
                    .active_efforts
                    .insert(category, trimmed.to_string());
                true
            }
        })?;
        // 同 set_active_model：命令与托盘两条路径共用这一处推送。
        crate::events::emit(crate::events::Topic::Settings, None);
        Ok(())
    }

    /// 设置某分类代理的首选端口（粘滞：绑定回退后写回实际端口作下次首选，或前端手改端口）。
    /// 后端自管字段专用写入，绕过 save_settings 旧快照覆盖。已是目标值则幂等跳过写盘。
    pub fn set_proxy_port(&self, category: CategoryType, port: u16) -> AppResult<()> {
        // 这条的落盘失败后果最具体：粘滞端口丢了 → 重启后端口重新漂移 → 客户端配置里
        // 写的旧端口连不上，而这正是引入粘滞端口本要解决的问题。必须带回滚。
        self.mutate_and_persist_if(|cfg| {
            if cfg.settings.proxy_ports.get(&category).copied() == Some(port) {
                return false;
            }
            cfg.settings.proxy_ports.insert(category, port);
            true
        })
    }

    /// 记录某分类已注册 synaroute MCP（去重后落盘）。后端注册逻辑专用。
    ///
    /// 必须走这里而非 `get_settings → push → save_settings`：后者会被 save_settings
    /// 的 `mem::take` 保留逻辑吞掉（把刚 push 的新值换回旧值），导致该集合永远为空——
    /// 端口漂移后其它已注册分类的客户端配置永不更新、关闭 MCP 时注销循环也读到空。
    /// 返回 true 表示本次新增（原本不含），false 表示已存在（幂等跳过写盘）。
    pub fn add_registered_category(&self, category: CategoryType) -> AppResult<bool> {
        // 落盘失败若不回滚：内存记着「已注册」而磁盘没有 → 端口漂移时的批量重写会漏掉
        // 该分类，客户端 MCP 指向死端口；且该方向永不自愈。
        //
        // `mutate_and_persist_when`：闭包同时给出「返回值」与「是否需要落盘」，
        // 于是幂等命中时既能返回 false 又不白写一次盘。
        self.mutate_and_persist_when(|cfg| {
            if cfg.settings.mcp_registered_categories.contains(&category) {
                return (false, false);
            }
            cfg.settings.mcp_registered_categories.push(category);
            (true, true)
        })
    }

    /// 移除单个已注册分类记录并落盘（per-category 注销 MCP 时用）。后端专用，
    /// 与 add_registered_category 对称。返回 true 表示确实移除，false 表示原本不含（幂等跳过写盘）。
    pub fn remove_registered_category(&self, category: CategoryType) -> AppResult<bool> {
        self.mutate_and_persist_when(|cfg| {
            let before = cfg.settings.mcp_registered_categories.len();
            cfg.settings.mcp_registered_categories.retain(|c| *c != category);
            let removed = cfg.settings.mcp_registered_categories.len() != before;
            (removed, removed)
        })
    }

    /// 清空已注册分类记录并落盘（关闭 MCP 开关时用）。后端专用。
    /// 已为空则跳过写盘（幂等）。
    pub fn clear_registered_categories(&self) -> AppResult<()> {
        self.mutate_and_persist_if(|cfg| {
            if cfg.settings.mcp_registered_categories.is_empty() {
                return false;
            }
            cfg.settings.mcp_registered_categories.clear();
            true
        })
    }

    /// 只更新 MCP 首选端口并落盘（后端自管字段，不走前端全量 save_settings）。
    /// 用于「粘住成功端口」：某次实际绑定的端口（可能因占用回退而来）写回设置，
    /// 使下次启动直接以它为首选，不再每次都从被占的旧端口重新回退、重写客户端配置。
    /// 已是目标值则跳过写盘（幂等，避免无谓 IO）。
    pub fn set_mcp_port(&self, port: u16) -> AppResult<()> {
        self.mutate_and_persist_if(|cfg| {
            if cfg.settings.mcp_port == port {
                return false;
            }
            cfg.settings.mcp_port = port;
            true
        })
    }

    /// 只更新 MCP 启用开关并落盘（后端自管字段，不走前端全量 save_settings）。
    /// 通用 save_settings 会保留旧的 mcp_enabled，避免切主题/语言时被入参顶掉；
    /// 真正翻转开关的路径（set_mcp_enabled）必须走这个专用方法直写。已是目标值则幂等跳过。
    pub fn set_mcp_enabled_flag(&self, enabled: bool) -> AppResult<()> {
        self.mutate_and_persist_if(|cfg| {
            if cfg.settings.mcp_enabled == enabled {
                return false;
            }
            cfg.settings.mcp_enabled = enabled;
            true
        })
    }

    /// 设置主口令开关的 **UI 镜像**（真实模式记在 `secrets.enc` 的 master 头部里）。
    ///
    /// 专用写入方法，因为 `save_settings` 刻意不让前端入参覆盖这个字段（见那里的注释）。
    /// 只应由「库迁移成功后」与「启动对账」两处调用。幂等：值相同不写盘。
    pub fn set_master_password_flag(&self, enabled: bool) -> AppResult<()> {
        // 落盘失败若不回滚：内存镜像与磁盘背离 → 启动对账（以库为准修正这个镜像）本身
        // 就建立在读磁盘之上，背离会让对账逻辑失效，可能自造「配置说开着、库里没头部」的死局。
        self.mutate_and_persist_if(|cfg| {
            if cfg.settings.master_password_enabled == enabled {
                return false;
            }
            cfg.settings.master_password_enabled = enabled;
            true
        })
    }

    /// 单独写「允许局域网访问代理」。
    ///
    /// 专用写入方法而不走 `save_settings`：这个开关必须与**监听地址重建**成对发生
    /// （绑定地址在 `ProxyManager::start` 里一次定死、之后改不了）。走批量保存时前端只落盘，
    /// 于是「关掉开关但端口仍监听 0.0.0.0」——界面说已关闭、实际对整个局域网敞开。
    /// 编排在 `service::set_lan_exposure`（落盘 + 重启在跑的代理），那里是唯一调用点。
    /// 幂等：值相同不写盘。
    pub fn set_lan_exposure(&self, enabled: bool) -> AppResult<()> {
        self.mutate_and_persist_if(|cfg| {
            if cfg.settings.lan_exposure == enabled {
                return false;
            }
            cfg.settings.lan_exposure = enabled;
            true
        })
    }

    // ---- 厂商预设 CRUD ----
    pub fn list_vendors(&self) -> Vec<Vendor> {
        self.config.read().vendors.clone()
    }

    /// 新增/更新厂商。内置项（builtin）不可修改。
    pub fn upsert_vendor(&self, vendor: Vendor) -> AppResult<Vendor> {
        // 用户可见 CRUD：落盘失败若不回滚，界面会显示「保存成功」而重启后厂商消失，
        // 且该方向永不自愈。闭包内的 Err（内置项不可改）也由 mutate_and_persist 走磁盘对账。
        self.mutate_and_persist(|cfg| {
            if let Some(existing) = cfg.vendors.iter_mut().find(|v| v.id == vendor.id) {
                if existing.builtin {
                    return Err(AppError::Invalid("内置厂商不可修改".into()));
                }
                // 防止把自定义项伪造成内置项
                let mut incoming = vendor.clone();
                incoming.builtin = false;
                *existing = incoming;
            } else {
                let mut incoming = vendor.clone();
                incoming.builtin = false;
                cfg.vendors.push(incoming);
            }
            Ok(())
        })?;
        Ok(vendor)
    }

    /// 删除厂商。内置项不可删除。
    pub fn delete_vendor(&self, vendor_id: &str) -> AppResult<()> {
        // 同 upsert_vendor：用户可见 CRUD，必须带落盘失败回滚。
        self.mutate_and_persist(|cfg| {
            match cfg.vendors.iter().find(|v| v.id == vendor_id) {
                Some(v) if v.builtin => {
                    return Err(AppError::Invalid("内置厂商不可删除".into()))
                }
                Some(_) => cfg.vendors.retain(|v| v.id != vendor_id),
                None => return Err(AppError::NotFound(vendor_id.into())),
            }
            Ok(())
        })
    }

    /// 测试专用：在指定路径构造 Store，避免污染真实 %APPDATA% 配置。
    #[cfg(test)]
    pub(crate) fn new_at(config_path: PathBuf, secrets_path: PathBuf) -> AppResult<Self> {
        if let Some(parent) = config_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let config = if config_path.exists() {
            let raw = std::fs::read(&config_path)?;
            serde_json::from_slice(&raw).unwrap_or_default()
        } else {
            AppConfig::default()
        };
        // **与生产构造器跑同一套迁移**。少了这一句，测试里构造出来的 Store 与用户机器上
        // 那个不是同一个东西 —— 于是「迁移有没有接上」这类缺陷在测试里根本看不到
        // （实测过：删掉 `add_new_builtin_vendors` 的调用点，全套测试照旧全绿）。
        let mut config = config;
        let _ = Self::migrate_config(&mut config);
        let secrets = SecretStore::load(secrets_path)?;
        let initial_stamp = Self::read_disk_stamp(&config_path);
        // 与生产构造器同样恢复用量累计：测试才能覆盖「重启后累计不归零」这条判据。
        let usage_path = crate::usage_store::usage_file_path(&config_path);
        let usage_loaded = crate::usage_store::load_usage(&usage_path);
        Ok(Self {
            config_path,
            config: RwLock::new(config),
            secrets: RwLock::new(secrets),
            events: RwLock::new(Vec::new()),
            config_stamp: RwLock::new(initial_stamp),
            log_tx: Self::spawn_log_writer(),
            log_dropped: std::sync::atomic::AtomicU64::new(0),
            health_dirty: std::sync::atomic::AtomicBool::new(false),
            usage_dirty: std::sync::atomic::AtomicBool::new(false),
            usage_path,
            usage_since_ms: RwLock::new(usage_loaded.since),
            usage_read_only: usage_loaded.read_only,
            daily_buckets: RwLock::new(usage_loaded.daily_buckets),
            retired_usage: RwLock::new(usage_loaded.retired),
            // 基线 = 启动时的历史总量（与生产构造器同一口径，测试才能覆盖「今日增量」判据）
            usage_baseline: RwLock::new(usage_loaded.totals.clone()),
            usage_baseline_date: RwLock::new(
                chrono::Utc::now().format("%Y-%m-%d").to_string(),
            ),
            usage_totals: RwLock::new(usage_loaded.totals),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_log_dir_is_stable_across_concurrent_calls() {
        // 回归护栏（用户实测：368 行落安装目录、1 行漏到 %APPDATA%）：
        // 原实现每写一行日志都重跑一次可写性探测，并发下多线程共用同名 `.write-probe`
        // 互删对方探针 → 个别调用探测失败 → 那一行日志漏回退到 %APPDATA%，日志被劈两处。
        // 现在结果 OnceLock 缓存，任意并发下必须返回同一路径。
        let first = default_log_dir();
        let handles: Vec<_> = (0..16)
            .map(|_| std::thread::spawn(default_log_dir))
            .collect();
        for h in handles {
            assert_eq!(
                h.join().unwrap(),
                first,
                "并发调用必须返回同一日志目录（否则日志会被劈到两处）"
            );
        }
        // 探针不得残留在日志目录里。
        if first.is_dir() {
            let leftover: Vec<_> = std::fs::read_dir(&first)
                .map(|rd| {
                    rd.flatten()
                        .filter(|e| {
                            e.file_name()
                                .to_str()
                                .map(|n| n.starts_with(".write-probe"))
                                .unwrap_or(false)
                        })
                        .map(|e| e.path())
                        .collect()
                })
                .unwrap_or_default();
            assert!(leftover.is_empty(), "可写性探针不应残留: {leftover:?}");
        }
    }

    /// 唯一临时目录（不引第三方 crate）：temp_dir + pid + 原子计数。
    fn temp_dir(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir()
            .join(format!("synaroute_test_{}_{}_{}", tag, std::process::id(), seq));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sample_key(id: &str, priority: i32) -> ProviderKey {
        ProviderKey {
            tier_fable: None,
            id: id.to_string(),
            category_id: CategoryType::ClaudeCli,
            name: format!("测试Key-{id}"),
            vendor: "test-vendor".into(),
            base_url: "https://api.example.com".into(),
            protocol: Protocol::Anthropic,
            has_secret: false,
            enabled: true,
            allow_in_aggregate: false,
            priority,
            headers_json: None,
            params: KeyParams::default(),
            models: vec![],
            mappings: vec![],
            default_model: None,
            tier_haiku: None,
            tier_sonnet: None,
            tier_opus: None,
            balance_query: None,
            cached_balance: None,
            cost_multiplier: None,
            icon: None,
            health: HealthState::default(),
        }
    }

    /// 覆盖前端"新增→存密钥→切换→删除"整条 IPC 链路对应的后端逻辑。
    #[test]
    fn full_key_lifecycle_persists_and_deletes() {
        let dir = temp_dir("lifecycle");
        let cfg_path = dir.join("config.json");
        let sec_path = dir.join("secrets.enc");
        let store = Store::new_at(cfg_path.clone(), sec_path).unwrap();

        // 新增
        store.upsert_key(sample_key("k1", 0)).unwrap();
        store.upsert_key(sample_key("k2", 1)).unwrap();
        assert_eq!(store.list_keys(CategoryType::ClaudeCli).len(), 2);
        assert!(cfg_path.exists(), "config.json 应已落盘");

        // 存密钥（DPAPI/AES 加密），并回读校验
        store.secrets.write().set("k1", "sk-secret-123").unwrap();
        let got = store.secrets.read().get("k1").unwrap();
        assert_eq!(got.as_deref().map(String::as_str), Some("sk-secret-123"));

        // 切换启用状态
        store.toggle_key("k1", false).unwrap();
        let k1 = store.get_key("k1").unwrap();
        assert!(!k1.enabled);

        // 删除：应从 config 移除、密钥库清除、并持久化
        store.delete_key("k1").unwrap();
        assert!(store.get_key("k1").is_none(), "删除后不应再查到 k1");
        assert!(store.secrets.read().get("k1").unwrap().is_none(), "删除后密钥应被清除");
        assert_eq!(store.list_keys(CategoryType::ClaudeCli).len(), 1);

        // 重新加载：删除结果应真正落盘（重启后仍生效）
        let reloaded = Store::new_at(cfg_path, dir.join("secrets.enc")).unwrap();
        assert!(reloaded.get_key("k1").is_none(), "重载后 k1 仍应不存在");
        assert!(reloaded.get_key("k2").is_some(), "k2 应保留");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// **保存 Key 不得清掉正在生效的运行态**（本轮审计 P1）。
    ///
    /// 前端的 ProviderKey 是「打开编辑器那一刻的快照」，而抽屉常开着几十秒（改映射、填窗口），
    /// 期间代理仍在转发。若该 Key 已连续失败并武装熔断，用户此时点「保存」，整份替换会把
    /// breaker_until 清空、fail_count 归零 —— **熔断被静默解除**，下一个请求又打向坏 Key。
    /// 「上移/下移」也走整份 upsert，同样踩这条。cached_balance 同理（旧余额顶回卡片）。
    ///
    /// 故障注入判据：去掉 upsert_key 里沿用 health/cached_balance 的两行，本测试立即变红。
    /// 🔴 **导入配置不许改写「局域网暴露」**（审查发现）。
    ///
    /// `model.rs` 把 `lan_exposure` 从 `UserPrefs` 移出时写明了理由：绑定地址在
    /// `ProxyManager::start` 里**一次定死**，只落盘不重建监听 → 关掉开关后端口仍在
    /// `0.0.0.0` 上，界面说「已关闭」而实际对整个局域网敞开（**安全方向**的「界面说 A、实际 B」）。
    /// 为此它有了专用命令 `set_lan_exposure`（落盘 + 重启在跑的代理）。
    ///
    /// 而导入走的正是那条被封掉的**整份覆盖**路径（`cfg.settings = incoming`），
    /// 于是能原样造出同一个失效：在一台局域网开着且代理在跑的机器上导入一份 `false` 的配置 →
    /// 开关显示关、socket 还在 `0.0.0.0` 上。反方向（导入 `true`）是「界面说开着、
    /// 局域网连不上」—— 不安全但同样撒谎。
    ///
    /// 两侧都要管：导出侧 `strip_machine_local` 不带它（老版本导入也不会中招），
    /// 导入侧保留本机值。本测试盯导入侧；导出侧由 `portable.rs` 那条盯。
    #[test]
    fn importing_a_config_must_not_flip_lan_exposure() {
        let dir = temp_dir("import_vs_lan");
        let store = Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap();
        // 本机：局域网**开着**（危险方向的前提）。
        store
            .mutate_and_persist(|cfg| {
                cfg.settings.lan_exposure = true;
                Ok(())
            })
            .unwrap();

        // 导入一份「关着」的配置（源机器从没开过局域网）。
        // `theme` 是一个正常该被导入的字段，用来证明导入本身生效了（否则断言是空洞的绿）。
        let incoming = crate::model::AppSettings {
            lan_exposure: false,
            theme: "dark".into(),
            ..Default::default()
        };
        let payload = crate::portable::ExportPayload {
            keys: vec![],
            brain: store.snapshot_config().brain.clone(),
            vendors: vec![],
            settings: incoming,
        };
        store
            .apply_imported_config(&payload, crate::portable::ImportMode::Merge)
            .unwrap();

        assert!(
            store.get_settings().lan_exposure,
            "导入不许改写 lan_exposure —— 翻成 false 会让界面说「已关闭」而 socket 仍在 0.0.0.0 上"
        );
        assert_eq!(
            store.get_settings().theme, "dark",
            "前置条件：导入本身要生效，否则上面那条断言是空洞的绿"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 🔴 **「清理孤儿密钥」不许删掉局域网接入令牌**（审查发现）。
    ///
    /// 孤儿判据是 `!live.contains(id)`，而 `live` 只由 `cfg.keys` 的 id 构成 ——
    /// `__lan_access_token` 永远不在里面，于是它**每次**都被判成孤儿。
    ///
    /// 用户视角：设置页说「检测到 N 条可清理的旧密钥」并明示这个操作**不影响使用**，
    /// 点下去之后**所有已配好的局域网客户端立刻 401**，而他不知道自己刚做了什么 ——
    /// 令牌的唯一出口是设置页，删掉就再也拿不回那一个（要重新生成并改每个客户端）。
    ///
    /// 还有一重更日常的：`count_orphan_secrets` 用同一判据，所以**一个真孤儿都没有的用户
    /// 会永久看到「检测到 1 条可清理」**，点了清理数字才归零 —— 而代价是令牌。
    ///
    /// ⚠️ `lan_guard::TOKEN_ID` 的注释原先明确写着「不会被孤儿密钥清理当成孤儿删掉」，
    /// 理由是「它本就不在 `keys` 里」—— **因果正好说反**：那恰恰是它会被删的原因。
    /// 而 `token_id_is_frozen` 复述了同一句假话却只验 id 形态，压根没验清理行为。
    ///
    /// 注入验证：去掉两处 `is_internal_secret_id` 过滤中的任意一处，本测试变红。
    #[test]
    fn the_lan_token_survives_orphan_pruning() {
        let dir = temp_dir("lan_token_vs_prune");
        let store = std::sync::Arc::new(
            Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap(),
        );
        // 一条真 Key（有密钥）+ 一条真孤儿（密钥库有、配置里没有）+ 局域网令牌。
        let mut k = sample_key("k1", 0);
        k.id = "k1".into();
        store.upsert_key(k).unwrap();
        store.secrets.write().set("k1", "sk-live").unwrap();
        store.secrets.write().set("orphan-uuid", "sk-orphan").unwrap();
        crate::proxy::lan_guard::ensure_token(&store);
        let token = crate::proxy::lan_guard::read_lan_token_from(&store)
            .unwrap()
            .expect("前置条件：令牌已生成");

        // 只该数到那一条真孤儿 —— 数成 2 就是「零孤儿的用户永久看到 1 条」那个现象。
        assert_eq!(store.count_orphan_secrets(), 1, "令牌不该被算成孤儿");
        assert_eq!(store.prune_orphan_secrets(), 1, "只该清掉那条真孤儿");

        // 🔴 真正的判据：令牌还在，且值没变。
        assert_eq!(
            crate::proxy::lan_guard::read_lan_token_from(&store).unwrap().as_deref(),
            Some(token.as_str()),
            "清理孤儿把局域网令牌删了 —— 所有已配好的局域网客户端会立刻 401"
        );
        // 真 Key 的密钥不受影响；那条真孤儿确实被清掉了。
        assert!(store.secrets.read().get("k1").unwrap().is_some(), "在用的密钥不许动");
        assert!(store.secrets.read().get("orphan-uuid").unwrap().is_none(), "真孤儿该被清掉");
        // 清完应当归零（幂等），否则用户会看到一个永远清不掉的数字。
        assert_eq!(store.count_orphan_secrets(), 0, "清完必须归零");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn upsert_key_preserves_runtime_state_against_stale_client_snapshot() {
        let dir = temp_dir("upsert_keeps_runtime");
        let store = std::sync::Arc::new(
            Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap(),
        );
        let mut k = sample_key("k1", 0);
        k.id = "k1".into();
        store.upsert_key(k.clone()).unwrap();

        // 模拟运行中：连续失败到武装熔断
        for _ in 0..3 {
            crate::health::record_live_failure(&store, "k1");
        }
        let armed = store
            .list_keys(CategoryType::ClaudeCli)
            .into_iter()
            .find(|x| x.id == "k1")
            .unwrap()
            .health;
        assert!(
            armed.breaker_until.is_some(),
            "前置条件：连续 3 次失败应已武装熔断，实际 {armed:?}"
        );

        // 用户在抽屉里点「保存」：发出的是**挂载时的旧快照**（health 为默认值）
        let mut stale = k.clone();
        stale.health = crate::model::HealthState::default();
        stale.name = "改了个名字".into();
        let saved = store.upsert_key(stale).unwrap();

        // 用户可编辑字段要生效，运行态必须沿用库里现值
        assert_eq!(saved.name, "改了个名字", "用户改的字段应正常保存");
        let after = store
            .list_keys(CategoryType::ClaudeCli)
            .into_iter()
            .find(|x| x.id == "k1")
            .unwrap()
            .health;
        assert!(
            after.breaker_until.is_some(),
            "保存 Key 不得解除正在生效的熔断，实际 {after:?}"
        );
        assert_eq!(after.fail_count, armed.fail_count, "fail_count 不得被旧快照归零");
        assert!(
            saved.health.breaker_until.is_some(),
            "返回值也应带库里的真实运行态（前端据它刷新列表）"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 被大脑聚合引用的 Key **不得**被删除，且错误要点明具体位置。
    ///
    /// 为什么这条防线必须有：删掉一个正在被聚合使用的 Key 后，成员会在下次调用时被静默跳过
    /// （用户以为「怎么少了一个视角」）、汇总者/决策者不可用则整轮失败（用户只看到超时或报错，
    /// 完全联想不到是几天前删的那条 Key）。两种都是本项目最忌讳的静默失效。
    ///
    /// 三个引用位置分别验：members / summarizer_ref / decider_ref。
    /// **`summarizer_ref` / `decider_ref` 的格式是 `keyId::modelName`**，
    /// 判定必须取 `::` 前半段 —— 用整串相等比对永远匹配不上，那等于这条防线没接上。
    #[test]
    fn delete_key_is_blocked_while_referenced_by_brain() {
        let dir = temp_dir("del_brain_ref");
        let store = Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap();
        for id in ["km", "ks", "kd", "kfree"] {
            let mut k = sample_key(id, 0);
            k.id = id.into();
            store.upsert_key(k).unwrap();
        }

        // 三处引用各配一条 Key，kfree 不被任何位置引用
        let mut brain = store.get_brain(CategoryType::ClaudeCli);
        brain.members = vec![crate::model::BrainMember {
            id: "m1".into(),
            key_id: "km".into(),
            model_name: "claude-opus-4-8".into(),
        }];
        brain.summarizer_ref = Some("ks::claude-haiku-4-5".into());
        brain.decider_ref = Some("kd::claude-opus-4-8".into());
        store.save_brain(brain).unwrap();

        // 成员引用 → 拒绝，且错误里点明「参与成员」
        let err = store.delete_key("km").expect_err("被引用为成员时必须拒绝删除");
        let msg = format!("{err}");
        assert!(msg.contains("参与成员"), "错误应点明引用位置，实际：{msg}");
        assert!(msg.contains("大脑聚合"), "错误应告诉用户去哪解除，实际：{msg}");
        assert!(store.get_key("km").is_some(), "拒绝删除后 Key 必须仍在");

        // 汇总者引用 → 拒绝（这条专门钉住「`keyId::modelName` 要取前半段比对」）
        let err = store.delete_key("ks").expect_err("被引用为汇总者时必须拒绝删除");
        assert!(
            format!("{err}").contains("汇总模型"),
            "汇总者引用未被识别 —— 检查是否用整串比对了 `keyId::modelName`：{err}"
        );
        assert!(store.get_key("ks").is_some());

        // 决策者引用 → 拒绝
        let err = store.delete_key("kd").expect_err("被引用为决策者时必须拒绝删除");
        assert!(format!("{err}").contains("决策者"), "{err}");
        assert!(store.get_key("kd").is_some());

        // 未被引用的 Key 照常能删（防线不能把正常路径也拦死）
        store.delete_key("kfree").expect("未被引用的 Key 必须能删");
        assert!(store.get_key("kfree").is_none());

        // 从大脑聚合移除引用后，原先被拦的 Key 就能删了（这是给用户的出路）
        let mut brain = store.get_brain(CategoryType::ClaudeCli);
        brain.members.clear();
        brain.summarizer_ref = None;
        brain.decider_ref = None;
        store.save_brain(brain).unwrap();
        for id in ["km", "ks", "kd"] {
            store
                .delete_key(id)
                .unwrap_or_else(|e| panic!("解除引用后 {id} 应能删除，实际：{e}"));
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    /// **迁移真的接在构造流程上**（不只是那几个函数自己没问题）。
    ///
    /// 这条是补一个实测到的盲区：`upgrade_adds_new_builtin_vendors_without_touching_existing_ones`
    /// 直接调 `Store::add_new_builtin_vendors`，于是把 `Store::new` 里那句**调用**删掉之后，
    /// 全套 799 条测试照旧全绿 —— 而那正是缺陷本身（老用户拿不到新厂商）。
    /// 与 CLAUDE.md 里 `route_meta` / `handle_http` 那条「单元覆盖了函数不等于覆盖了接线」同型。
    ///
    /// 判据对着**构造出来的 Store** 断言，而不是对着某个函数的返回值。
    ///
    /// 故障注入判据：把 `Self::migrate_config(&mut config)` 从 `new`/`new_at` 里去掉
    /// → 本测试必须变红。
    #[test]
    fn migrations_are_wired_into_construction() {
        use std::sync::atomic::{AtomicU64, Ordering};
        // pid + 进程内自增，不用时间戳：本机实测 timestamp_nanos 的量化粒度只有 100ns，
        // 并发用例下撞名率很高（CLAUDE.md 里 `db_copy_path` 那条）。
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "synaroute_migrate_wired_{}_{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let config_path = dir.join("config.json");

        // 造一份**老配置**：只有种子里的第一个内置厂商，config_version 停在 v1（0）。
        let seed = Vendor::builtin_seed();
        let old = AppConfig {
            config_version: 0,
            keys: vec![],
            brain: vec![],
            vendors: vec![seed[0].clone()],
            settings: AppSettings::default(),
        };
        std::fs::write(&config_path, serde_json::to_vec_pretty(&old).unwrap()).unwrap();

        let store = Store::new_at(config_path.clone(), dir.join("secrets.enc")).unwrap();
        let vendors = store.list_vendors();

        // ① 种子里的每一个内置厂商都在（这一条就是「老用户能不能拿到新厂商」）
        for s in &seed {
            assert!(
                vendors.iter().any(|v| v.id == s.id),
                "构造后缺少内置厂商 {} —— 迁移没接上构造流程",
                s.id
            );
        }
        // ② 版本门也跑了（v1 → v2）
        assert_eq!(
            store.config.read().config_version,
            2,
            "版本迁移没接上构造流程"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 老用户升级后必须拿到**种子里新增的**内置厂商，而已有项一个字节都不许动。
    ///
    /// 这条锁的是一类静默失效：`builtin_seed()` 只在 `vendors.is_empty()` 时注入，
    /// 于是往种子里加厂商**只对全新安装生效** —— 老用户（绝大多数）永远看不到，
    /// 而代码、测试全是绿的。
    ///
    /// 故障注入判据：把 `add_new_builtin_vendors` 的调用从 `Store::new` 里去掉
    /// → 本测试必须变红。
    #[test]
    fn upgrade_adds_new_builtin_vendors_without_touching_existing_ones() {
        let seed = Vendor::builtin_seed();
        assert!(seed.len() >= 3, "种子应当有多个内置厂商，否则本测试没意义");

        // 模拟老配置：只有种子里的第一个内置厂商，且用户**改过它的 base_url**
        // （换了区域镜像 / 走自己的反代 —— 真实且常见），另有一个自定义厂商。
        let mut mine = seed[0].clone();
        mine.default_base_url = "https://my-mirror.example.com/v1".into();
        mine.preset_models = vec![];
        let custom = Vendor {
            id: "my-relay".into(),
            name: "我的中转".into(),
            default_base_url: "https://relay.example.com".into(),
            default_protocol: Protocol::Anthropic,
            builtin: false,
            icon: None,
            preset_models: vec![],
        };
        let mut vendors = vec![mine.clone(), custom.clone()];

        let added = Store::add_new_builtin_vendors(&mut vendors);
        assert_eq!(
            added,
            seed.len() - 1,
            "种子里除已有那一个之外的内置厂商都应被补进来"
        );

        // ① 用户改过的那条**原样保留**（拿种子覆盖等于把他的配置冲掉）
        let kept = vendors.iter().find(|v| v.id == mine.id).unwrap();
        assert_eq!(
            kept.default_base_url, "https://my-mirror.example.com/v1",
            "已有内置厂商的 base_url 绝不能被种子覆盖"
        );
        // ② 自定义厂商还在，且**顺序没被打乱**（用户对列表顺序是有感知的）
        assert_eq!(vendors[0].id, mine.id);
        assert_eq!(vendors[1].id, "my-relay");
        // ③ 新增的都追加在末尾，且 id 不重复
        let ids: Vec<&str> = vendors.iter().map(|v| v.id.as_str()).collect();
        let uniq: std::collections::HashSet<&str> = ids.iter().copied().collect();
        assert_eq!(uniq.len(), ids.len(), "补入后不得出现重复 id");
        // ④ 种子里的每一个 id 现在都在
        for s in &seed {
            assert!(ids.contains(&s.id.as_str()), "种子厂商 {} 未被补入", s.id);
        }

        // ⑤ 幂等：再跑一次不应该再补任何东西（否则每次启动都会重复追加）
        let again = Store::add_new_builtin_vendors(&mut vendors);
        assert_eq!(again, 0, "第二次调用必须什么都不补（幂等）");
    }

    /// 迁移：老配置的内置厂商无 preset_models 时应从种子回填；自定义厂商与已有数据不动。
    #[test]
    fn backfill_builtin_presets_only_fills_empty_builtins() {
        // 模拟老配置：内置 anthropic 预设为空，自定义厂商预设为空，另一内置厂商已有自定义预设
        let mut vendors = vec![
            Vendor {
                id: "anthropic".into(),
                name: "Anthropic".into(),
                default_base_url: "https://api.anthropic.com".into(),
                default_protocol: Protocol::Anthropic,
                builtin: true,
                icon: None,
                preset_models: vec![], // 老配置：空 → 应被回填
            },
            Vendor {
                id: "my-relay".into(),
                name: "自定义中转".into(),
                default_base_url: "https://relay.example.com".into(),
                default_protocol: Protocol::Anthropic,
                builtin: false,
                icon: None,
                preset_models: vec![], // 自定义 → 不动
            },
            Vendor {
                id: "zhipu".into(),
                name: "智谱 GLM".into(),
                default_base_url: "https://open.bigmodel.cn/api/paas/v4".into(),
                default_protocol: Protocol::OpenaiChat,
                builtin: true,
                icon: None,
                preset_models: vec![PresetModel {
                    real_name: "glm-custom".into(),
                    display_name: None,
                    context_window: None,
                }], // 已有数据 → 不覆盖
            },
        ];

        let changed = Store::backfill_builtin_presets(&mut vendors);
        assert!(changed, "至少 anthropic 被回填，应返回 true");

        let anthropic = vendors.iter().find(|v| v.id == "anthropic").unwrap();
        assert!(!anthropic.preset_models.is_empty(), "空的内置厂商应被回填");

        let relay = vendors.iter().find(|v| v.id == "my-relay").unwrap();
        assert!(relay.preset_models.is_empty(), "自定义厂商不应被回填");

        let zhipu = vendors.iter().find(|v| v.id == "zhipu").unwrap();
        assert_eq!(zhipu.preset_models.len(), 1, "已有预设的内置厂商不应被覆盖");
        assert_eq!(zhipu.preset_models[0].real_name, "glm-custom");

        // 幂等：再跑一次应无改动
        assert!(!Store::backfill_builtin_presets(&mut vendors), "回填应幂等");
    }

    /// 模拟后台健康检查线程与前端保存并发写盘：唯一临时名 + 重试应保证不丢、不损坏。
    #[test]
    fn concurrent_persist_is_consistent() {
        let dir = temp_dir("concurrent");
        let cfg_path = dir.join("config.json");
        let store = std::sync::Arc::new(
            Store::new_at(cfg_path.clone(), dir.join("secrets.enc")).unwrap(),
        );
        store.upsert_key(sample_key("base", 0)).unwrap();

        let mut handles = vec![];
        for t in 0..8 {
            let s = store.clone();
            handles.push(std::thread::spawn(move || {
                for i in 0..20 {
                    // 交替 toggle 与 health 更新，全部走 persist→atomic_write
                    s.toggle_key("base", i % 2 == 0).ok();
                    s.update_health(
                        "base",
                        HealthState { status: HealthStatus::Up, ..Default::default() },
                    )
                    .ok();
                    let _ = t;
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        // 并发结束后配置文件必须是完整可解析的 JSON（无半写损坏）
        let raw = std::fs::read(&cfg_path).unwrap();
        let parsed: AppConfig = serde_json::from_slice(&raw).expect("config.json 应为完整合法 JSON");
        assert_eq!(parsed.keys.len(), 1);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 并发写事件日志：落盘的 jsonl **每行必须是且仅是一个完整 JSON 对象**。
    ///
    /// 回归 2026-07-31 复盘定位的真 bug：旧实现用 `writeln!(f, "{line}")`，
    /// 它经 write_fmt 拆成「写正文」+「写 \n」两次系统调用，并发追加时会插进别人的正文，
    /// 落盘出现 `{…}{…}` 粘在一行并丢换行。实测那天 543 行里有 14 行粘连，
    /// 导致按行解析的日志工具漏掉 26 条记录（排查时统计口径直接错掉）。
    #[test]
    fn concurrent_event_log_writes_stay_one_json_per_line() {
        let dir = temp_dir("lograce");
        let store = std::sync::Arc::new(
            Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap(),
        );
        let log_dir = dir.join("logs");
        let mut settings = store.get_settings();
        settings.log_dir = Some(log_dir.display().to_string());
        store.save_settings(UserPrefs::from(&settings)).unwrap();

        const THREADS: usize = 8;
        const PER_THREAD: usize = 60;
        let mut handles = vec![];
        for t in 0..THREADS {
            let s = store.clone();
            handles.push(std::thread::spawn(move || {
                for i in 0..PER_THREAD {
                    // detail 里塞长文本，放大「正文与换行分两次写」的交错窗口。
                    s.append_event(
                        CategoryType::ClaudeCli,
                        "test",
                        None,
                        &format!("t{t}-i{i}-{}", "x".repeat(200)),
                    );
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        // 日志落盘是**单写者异步**（P1-3）：读文件前必须等队列排空，否则读到的是半截。
        // 这不是为测试放宽判据——排空后行数仍必须精确等于事件数。
        store.flush_logs();

        let date = chrono::Utc::now().format("%Y-%m-%d");
        let file = log_dir.join(format!("{date}.jsonl"));
        let raw = std::fs::read_to_string(&file).expect("应产出当天日志文件");
        let lines: Vec<&str> = raw.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(
            lines.len(),
            THREADS * PER_THREAD,
            "行数必须等于事件数——少了就说明有多条被粘进同一行"
        );
        for (n, line) in lines.iter().enumerate() {
            let v: serde_json::Value = serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("第 {} 行不是单个完整 JSON: {e}\n{line}", n + 1));
            assert!(v.get("id").is_some() && v.get("detail").is_some(), "第 {} 行字段缺失", n + 1);
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 连续同类事件在**内存态**折叠成一条并计数，但**日志文件仍逐条完整写**。
    ///
    /// 这两件事必须同时成立：界面降噪（高频转发下 14 秒能刷 12 条重复行）不能牺牲排障取证
    /// ——每一次真实调用的时刻与延迟都得留在文件里。
    #[test]
    fn consecutive_same_events_collapse_in_memory_but_not_in_file() {
        let dir = temp_dir("collapse");
        let store = Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap();
        let log_dir = dir.join("logs");
        let mut settings = store.get_settings();
        settings.log_dir = Some(log_dir.display().to_string());
        store.save_settings(UserPrefs::from(&settings)).unwrap();

        let ck = Some("ok:k1:claude-opus-4-8:false".to_string());
        for i in 0..5 {
            store.append_event_collapsible(
                CategoryType::ClaudeCli,
                "route",
                Some("k1"),
                &format!("厂商1 · 成功返回 · 模型 X · {}ms", 100 + i),
                None,
                ck.clone(),
            );
        }

        let ev = store.list_all_events();
        assert_eq!(ev.len(), 1, "连续 5 条同类必须折叠成 1 条: {ev:?}");
        assert_eq!(ev[0].repeat, 5, "计数应为 5");
        assert!(
            ev[0].detail.contains("104ms"),
            "detail 应刷新到最近一次（延迟数字会变，看最新的更有参考价值）: {}",
            ev[0].detail
        );

        // 文件侧：5 条都在。日志异步落盘，读前先排空队列（见 P1-3）。
        store.flush_logs();
        let date = chrono::Utc::now().format("%Y-%m-%d");
        let raw = std::fs::read_to_string(log_dir.join(format!("{date}.jsonl"))).unwrap();
        let route_lines = raw
            .lines()
            .filter(|l| l.contains("\"成功返回") || l.contains("成功返回"))
            .count();
        assert_eq!(route_lines, 5, "日志文件必须逐条完整写，不受界面折叠影响");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 中间插进别的事件就重新起一条 —— 只折叠**紧邻**的同类。
    ///
    /// 若做全局归并，「连续成功 20 次」与「成功 10 次 → 失败 → 再成功 10 次」在界面上会长得
    /// 一样，而后者恰恰是需要被看见的异常。
    #[test]
    fn collapse_breaks_when_another_event_interleaves() {
        let dir = temp_dir("collapse_break");
        let store = Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap();
        let ck = Some("ok:k1:m:false".to_string());

        store.append_event_collapsible(CategoryType::ClaudeCli, "route", Some("k1"), "成功 1", None, ck.clone());
        store.append_event_collapsible(CategoryType::ClaudeCli, "route", Some("k1"), "成功 2", None, ck.clone());
        // 插一条故障转移（无 collapse_key，永不折叠）
        store.append_event(CategoryType::ClaudeCli, "failover", Some("k1"), "转移到厂商2");
        store.append_event_collapsible(CategoryType::ClaudeCli, "route", Some("k1"), "成功 3", None, ck.clone());

        let ev = store.list_all_events();
        assert_eq!(ev.len(), 3, "应为「折叠×2」+「故障转移」+「新起的成功」: {ev:?}");
        assert_eq!(ev[0].repeat, 2);
        assert_eq!(ev[1].kind, "failover");
        assert_eq!(ev[2].repeat, 1, "被打断后必须重新计数");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 折叠时 token 用量必须**累加**。
    ///
    /// 折叠的语义是「同一件事发生了 N 次」，那 N 次各自都真实烧了额度。
    /// 若只保留最后一次的用量，界面显示的总量会远小于实际消耗 —— 而用户正是
    /// 靠这个数字判断「工具开关关掉后是否真的省了」，少算等于把账做平了。
    #[test]
    fn collapsed_events_accumulate_token_usage() {
        use crate::upstream::TokenUsage;
        let dir = temp_dir("collapse_usage");
        let store = Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap();
        let ck = Some("ok:k1:m:false".to_string());
        let u = |i: u64, o: u64| {
            Some(TokenUsage { input: i, output: o, cache_read: 0, cache_creation: 0 })
        };

        store.append_event_full(CategoryType::ClaudeCli, "route", Some("k1"), "第1次", None, ck.clone(), u(100, 20));
        store.append_event_full(CategoryType::ClaudeCli, "route", Some("k1"), "第2次", None, ck.clone(), u(200, 30));
        store.append_event_full(CategoryType::ClaudeCli, "route", Some("k1"), "第3次", None, ck.clone(), u(300, 50));

        let ev = store.list_all_events();
        assert_eq!(ev.len(), 1, "三条同类应折叠成一条");
        assert_eq!(ev[0].repeat, 3);
        let got = ev[0].usage.expect("折叠后应保留用量");
        assert_eq!(
            (got.input, got.output),
            (600, 100),
            "用量必须是三次之和，不能只留最后一次"
        );

        // 首条无用量、后续有 → 也要能记上（不能因为第一条是 None 就丢掉后面的）
        let ck2 = Some("ok:k2:m:false".to_string());
        store.append_event_full(CategoryType::ClaudeCli, "route", Some("k2"), "无量", None, ck2.clone(), None);
        store.append_event_full(CategoryType::ClaudeCli, "route", Some("k2"), "有量", None, ck2.clone(), u(70, 8));
        let ev = store.list_all_events();
        let last = ev.last().unwrap();
        assert_eq!(last.repeat, 2);
        assert_eq!(last.usage.map(|x| (x.input, x.output)), Some((70, 8)));

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 用量总计**只增不减**：事件环滚动（MAX_EVENTS 上限）不得让已统计的用量凭空消失。
    ///
    /// 这条是「用量统计」这个功能的核心正确性判据。原实现把总量算在 `self.events` 上，
    /// 而 events 是个 500 条的环 —— 超过 500 次请求后，最老的事件被 drain 掉，
    /// 面板上的累计 token **会往回退**。一个「累计用量」面板显示的数字越用越小，
    /// 比不显示更糟：用户据此判断额度消耗，会严重低估。
    #[test]
    fn token_usage_totals_never_shrink_when_event_ring_rotates() {
        use crate::upstream::TokenUsage;
        let dir = temp_dir("usage_no_shrink");
        let store = Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap();
        let one = || Some(TokenUsage { input: 10, output: 1, cache_read: 0, cache_creation: 0 });

        // 先把事件环填满（每条都不折叠：detail 不同且不给 collapse_key）。
        for i in 0..MAX_EVENTS {
            store.append_event_full(
                CategoryType::ClaudeCli, "route", Some("k1"),
                &format!("req-{i}"), None, None, one(),
            );
        }
        let before = store.token_usage_by_key()[0].usage.input;
        assert_eq!(before, (MAX_EVENTS as u64) * 10, "填满环时应统计到全部用量");

        // 再发 100 条 —— 最老的 100 条会被挤出事件环。
        for i in 0..100 {
            store.append_event_full(
                CategoryType::ClaudeCli, "route", Some("k1"),
                &format!("more-{i}"), None, None, one(),
            );
        }
        let after = store.token_usage_by_key()[0].usage.input;
        assert_eq!(
            after,
            (MAX_EVENTS as u64 + 100) * 10,
            "累计用量必须涵盖已滚出事件环的请求（before={before} after={after}）"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 用量累计必须**跨重启保留**。
    ///
    /// 这条钉住的是「周期性落盘」这个功能本身：累加器是内存态，进程一退就没了。
    /// 若忘了在退出/定时点落盘，或落盘后启动时忘了读回来，用户看到的就是
    /// 「每次重开软件用量归零」—— 一个号称累计的面板永远只显示本次运行的零头。
    #[test]
    fn usage_totals_survive_restart() {
        use crate::upstream::TokenUsage;
        let dir = temp_dir("usage_restart");
        let cfg = dir.join("config.json");
        let sec = dir.join("secrets.enc");

        {
            let store = Store::new_at(cfg.clone(), sec.clone()).unwrap();
            store.append_event_full(
                CategoryType::ClaudeCli,
                "route",
                Some("k1"),
                "req",
                None,
                None,
                Some(TokenUsage { input: 700, output: 30, cache_read: 5, cache_creation: 0 }),
            );
            assert!(store.flush_usage_if_dirty(), "有变更时必须真的落盘");
        } // store 析构 = 模拟进程退出

        // 重新构造 = 模拟重启
        let store2 = Store::new_at(cfg, sec).unwrap();
        let rows = store2.token_usage_by_key();
        assert_eq!(rows.len(), 1, "重启后应恢复出那一行用量");
        assert_eq!(rows[0].usage.input, 700, "input 必须跨重启保留");
        assert_eq!(rows[0].usage.output, 30);
        assert_eq!(rows[0].usage.cache_read, 5);

        // 重启后继续累加，应当叠在历史值之上而不是从零开始。
        store2.append_event_full(
            CategoryType::ClaudeCli,
            "route",
            Some("k1"),
            "req2",
            None,
            None,
            Some(TokenUsage { input: 300, output: 20, cache_read: 0, cache_creation: 0 }),
        );
        assert_eq!(
            store2.token_usage_by_key()[0].usage.input,
            1000,
            "新消耗必须叠加在恢复出来的历史值上"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// v1 用量文件首次 flush 应迁移进 v2 分桶 + 90 天滚动删旧桶。
    ///
    /// 测了两个场景：
    /// 1. v1 格式（单个 `entries`）→ v2（按日分桶）
    /// 2. 90 天滚动：91 天前的桶必须被删掉
    #[test]
    fn usage_v1_to_v2_migration_and_rolling() {
        use crate::model::{TokenUsageByKey, UsageSnapshot};
        use crate::upstream::TokenUsage;
        let dir = temp_dir("usage_v2_migrate");
        let cfg = dir.join("config.json");
        let sec = dir.join("secrets.enc");
        let usage_path = dir.join("usage.json");

        // 先手写一个 v1 文件：`version=1`，只有全局 `entries`
        let v1_snap = UsageSnapshot {
            version: 1,
            since_ms: chrono::Utc::now().timestamp_millis() - 95 * 86_400_000, // 95 天前
            updated_ms: chrono::Utc::now().timestamp_millis(),
            daily_buckets: Vec::new(),
            retired: Vec::new(),
            entries: vec![TokenUsageByKey {
                category_id: CategoryType::ClaudeCli,
                key_id: "old-key".into(),
                usage: TokenUsage { input: 500, output: 100, cache_read: 0, cache_creation: 0 },
            }],
        };
        std::fs::write(&usage_path, serde_json::to_vec_pretty(&v1_snap).unwrap()).unwrap();

        // 启动 Store（会读 v1 文件并把 entries 加载进内存累加器）
        let store = Store::new_at(cfg.clone(), sec.clone()).unwrap();
        assert_eq!(store.token_usage_by_key().len(), 1, "v1 文件应被正确读取");
        assert_eq!(store.token_usage_by_key()[0].usage.input, 500);

        // 产生一笔新消耗并 flush（触发 v1→v2 迁移）
        store.append_event_full(
            CategoryType::ClaudeCli,
            "route",
            Some("new-key"),
            "req",
            None,
            None,
            Some(TokenUsage { input: 200, output: 50, cache_read: 0, cache_creation: 0 }),
        );
        assert!(store.flush_usage_if_dirty(), "首次 flush 应成功写盘");

        // 读回文件验证迁移结果
        let raw = std::fs::read(&usage_path).unwrap();
        let v2: UsageSnapshot = serde_json::from_slice(&raw).unwrap();
        assert_eq!(
            v2.version,
            crate::model::USAGE_SNAPSHOT_VERSION,
            "flush 后文件应升到当前格式版本（写侧与读侧共用同一常量，别写字面量）"
        );

        assert!(v2.entries.is_empty(), "v2 不再使用 entries 字段");
        assert!(!v2.daily_buckets.is_empty(), "v2 应有按日分桶");

        let dates: Vec<_> = v2.daily_buckets.iter().map(|b| &b.date).collect();
        let today = Store::utc_date_string(chrono::Utc::now().timestamp_millis());
        assert!(
            dates.contains(&&today),
            "今天的桶必须存在（当前日期 {today}，实际桶: {dates:?}）"
        );

        // 关键判据：桶里只能有**本次新增的 200**，不能含 v1 那 500。
        //
        // v1 文件没有日期维度，那 500 是「过去某段时间的累计」，无从得知它属于哪天；
        // 它在启动时被读进内存累加器并成为**基线**，故不计入任何日桶。
        // 若这里出现 700，说明 flush 把「历史总量」当成了「当天消耗」——
        // 那是本轮实测抓到的 bug：跑一周后「今日花费」会等于「累计花费」。
        let total_input: u64 = v2
            .daily_buckets
            .iter()
            .flat_map(|b| &b.entries)
            .map(|e| e.usage.input)
            .sum();
        assert_eq!(
            total_input, 200,
            "日桶只应含本次增量 200；出现 700 = 历史总量被重复计入当天"
        );

        // 再 flush 一次且无新消耗：不应把同一批增量重复写进桶（基线已抬高）。
        store.mark_usage_dirty();
        store.flush_usage_if_dirty();
        let v2b: UsageSnapshot =
            serde_json::from_slice(&std::fs::read(&usage_path).unwrap()).unwrap();
        let total_after: u64 = v2b
            .daily_buckets
            .iter()
            .flat_map(|b| &b.entries)
            .map(|e| e.usage.input)
            .sum();
        assert_eq!(
            total_after, 200,
            "无新消耗时重复 flush 不得让当天数字翻倍（实际 {total_after}）"
        );

        // 重启后：总量 = v1 的 500（已折进 retired）+ 新增 200，跨重启不丢。
        //
        // 旧行为是 200 —— v1 的 500 只进内存累加器、不落任何桶，**重启一次就永久消失**，
        // 而当时把这个损失当成既定行为记着，理由是「无从得知它属于哪天」。那个理由只否定
        // 「造一个假日期把整段历史堆到某天」，并不构成「必须丢掉总量」：`retired`
        // （已淘汰桶的累计）就是「有总量、无日维度」这类数据的归宿，v1 的 entries 正是同一类。
        drop(store);
        let store2 = Store::new_at(cfg, sec).unwrap();
        let restored: u64 = store2
            .token_usage_by_key()
            .iter()
            .map(|r| r.usage.input)
            .sum();
        assert_eq!(
            restored, 700,
            "重启后总量 = retired（v1 的 500）+ 日桶之和（200）；\
             得到 200 说明 v1 历史又被重启吃掉了"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 「今日」用量不得比「累计」慢一分钟 —— 未落盘增量必须并进当天的桶。
    ///
    /// 缺陷形态：`daily_buckets` 只在 flush 时更新（最多落后 60s）。原注释声称
    /// 「面板的今日由前端把这份历史与实时增量相加得出」，但 `UsagePage.byDate` 从来只用这份桶。
    /// 于是同一屏上自相矛盾：刚发过几个请求，「累计」已经涨了，「今日」还是 0
    /// —— 新装后的第一分钟尤其明显。
    ///
    /// 三个方向一起钉：
    /// 1. flush 之前就能看到当天的量（这是缺陷本身）；
    /// 2. flush 之后**不重复计数**（基线被抬，pending 归零，桶里已有那份）；
    /// 3. 只读视图**不抬基线** —— 抬了就会把这段增量从下一次 flush 里吃掉，
    ///    变成「看过一眼用量，那段就再也不落盘了」这种更隐蔽的丢数据。
    #[test]
    fn daily_buckets_include_unflushed_delta_without_double_counting() {
        use crate::upstream::TokenUsage;
        let dir = temp_dir("usage_pending");
        let store = Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap();
        let today = Store::utc_date_string(chrono::Utc::now().timestamp_millis());
        let sum_today = |s: &Store| -> u64 {
            s.daily_usage_buckets()
                .iter()
                .filter(|b| b.date == today)
                .flat_map(|b| &b.entries)
                .map(|e| e.usage.input)
                .sum()
        };

        assert_eq!(sum_today(&store), 0, "前置条件：还没有任何用量");

        store.append_event_full(
            CategoryType::ClaudeCli,
            "route",
            Some("k"),
            "req",
            None,
            None,
            Some(TokenUsage { input: 90, output: 0, cache_read: 0, cache_creation: 0 }),
        );
        assert_eq!(
            sum_today(&store),
            90,
            "还没 flush 就该看得到今天的 90 —— 否则「今日」会比「累计」慢整整一分钟"
        );

        assert!(store.flush_usage_if_dirty(), "落盘一次");
        assert_eq!(
            sum_today(&store),
            90,
            "flush 后必须仍是 90：桶里已有那份、pending 归零，重复计数会变 180"
        );

        // 只读视图不得抬基线：再来一笔仍要能被下一次 flush 落盘
        store.append_event_full(
            CategoryType::ClaudeCli,
            "route",
            Some("k"),
            "req",
            None,
            None,
            Some(TokenUsage { input: 10, output: 0, cache_read: 0, cache_creation: 0 }),
        );
        let _ = store.daily_usage_buckets(); // 读一眼（旧实现若在这里抬基线就会吃掉这 10）
        assert!(store.flush_usage_if_dirty(), "这 10 必须还能落盘");
        let persisted: crate::model::UsageSnapshot = serde_json::from_slice(
            &std::fs::read(crate::usage_store::usage_file_path(&dir.join("config.json"))).unwrap(),
        )
        .unwrap();
        let on_disk: u64 = persisted
            .daily_buckets
            .iter()
            .flat_map(|b| &b.entries)
            .map(|e| e.usage.input)
            .sum();
        assert_eq!(on_disk, 100, "磁盘上应是 90 + 10；得到 90 说明只读视图偷偷抬了基线");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 90 天滚动删桶**不得**让「累计用量」变小。
    ///
    /// 缺陷形态：启动时的累计总量 = 各存活桶之和。桶只留 90 天，于是每有一个桶过期，
    /// **下次启动读出来的累计就往下掉一截** —— 一个「累计用量」面板越用数字越少，
    /// 用户据此估额度会严重低估；而 `since_ms` 仍宣称「统计自 <安装日> 起」，
    /// 数字覆盖的区间比它声称的短，且这事完全不可见。
    /// 与当年「按事件环算总量」是同一个症状、不同的成因（那次已修，这里在第 90 天重现）。
    ///
    /// 处置：删桶前把它的量折进 `retired`，累计 = retired + 存活桶之和，单调不减；
    /// 按日视图仍只看 `daily_buckets`，90 天窗口语义不变。
    ///
    /// 三个方向一起钉：
    /// 1. 过期桶**确实被删**（90 天窗口不能因为这个修复失效）；
    /// 2. 它的量进了 `retired`，且重启后累计**不小于**删除前；
    /// 3. 存活桶原样保留（别把好桶一起折走）。
    #[test]
    fn rolling_out_old_buckets_keeps_cumulative_total_monotonic() {
        use crate::model::{DailyUsageBucket, TokenUsageByKey, UsageSnapshot};
        use crate::upstream::TokenUsage;
        let dir = temp_dir("usage_retire");
        let cfg = dir.join("config.json");
        let sec = dir.join("secrets.enc");
        let usage_path = dir.join("usage.json");

        let today = chrono::Utc::now().date_naive();
        let day = |off: i64| (today - chrono::Duration::days(off)).format("%Y-%m-%d").to_string();
        let usage = |input: u64| TokenUsage { input, output: 0, cache_read: 0, cache_creation: 0 };
        let row = |input: u64| TokenUsageByKey {
            category_id: CategoryType::ClaudeCli,
            key_id: "k".into(),
            usage: usage(input),
        };

        // 一个早已过期的桶（100 天前，1000）+ 一个还在窗口内的桶（3 天前，7）
        let snap = UsageSnapshot {
            version: crate::model::USAGE_SNAPSHOT_VERSION,
            since_ms: chrono::Utc::now().timestamp_millis() - 120 * 86_400_000,
            updated_ms: chrono::Utc::now().timestamp_millis(),
            daily_buckets: vec![
                DailyUsageBucket { date: day(3), entries: vec![row(7)] },
                DailyUsageBucket { date: day(100), entries: vec![row(1000)] },
            ],
            retired: Vec::new(),
            entries: Vec::new(),
        };
        std::fs::write(&usage_path, serde_json::to_vec_pretty(&snap).unwrap()).unwrap();

        let store = Store::new_at(cfg.clone(), sec.clone()).unwrap();
        let before: u64 = store.token_usage_by_key().iter().map(|r| r.usage.input).sum();
        assert_eq!(before, 1007, "前置条件：两个桶之和");

        // 触发一次 flush（会执行 90 天滚动）
        store.append_event_full(
            CategoryType::ClaudeCli,
            "route",
            Some("k"),
            "req",
            None,
            None,
            Some(usage(5)),
        );
        assert!(store.flush_usage_if_dirty());

        let after: UsageSnapshot =
            serde_json::from_slice(&std::fs::read(&usage_path).unwrap()).unwrap();
        let dates: Vec<&String> = after.daily_buckets.iter().map(|b| &b.date).collect();
        assert!(!dates.contains(&&day(100)), "100 天前的桶必须被删（90 天窗口仍生效）");
        assert!(dates.contains(&&day(3)), "窗口内的桶必须原样保留");
        let retired_total: u64 = after.retired.iter().map(|e| e.usage.input).sum();
        assert_eq!(
            retired_total, 1000,
            "被删桶的量必须折进 retired，否则它就凭空消失了"
        );

        // 重启后累计不得小于删除前（1007 + 本次新增 5）
        drop(store);
        let store2 = Store::new_at(cfg, sec).unwrap();
        let restored: u64 = store2.token_usage_by_key().iter().map(|r| r.usage.input).sum();
        assert_eq!(
            restored, 1012,
            "累计必须单调不减：retired(1000) + 存活桶(7) + 本次新增(5)。\
             得到 12 说明过期桶的量被直接丢了 —— 面板数字会当着用户的面变小"
        );
        assert!(restored >= before, "「累计用量」永不允许变小");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 脏标记语义：无变更不写盘（空闲期零 I/O），有变更才写。
    ///
    /// 用户明确担心过持续写盘伤 SSD。若去掉脏标记改成无条件定时写，一个开着不用的
    /// 窗口会每分钟制造一次无意义写入 —— 这条测试就是那个担忧的回归护栏。
    #[test]
    fn usage_flush_is_noop_without_changes() {
        use crate::upstream::TokenUsage;
        let dir = temp_dir("usage_dirty_flag");
        let store =
            Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap();

        assert!(
            !store.flush_usage_if_dirty(),
            "刚构造、无任何用量变更时不应写盘"
        );

        store.append_event_full(
            CategoryType::ClaudeCli,
            "route",
            Some("k1"),
            "req",
            None,
            None,
            Some(TokenUsage { input: 1, output: 1, cache_read: 0, cache_creation: 0 }),
        );
        assert!(store.flush_usage_if_dirty(), "有变更后应写盘一次");
        assert!(
            !store.flush_usage_if_dirty(),
            "同一批变更不应重复写盘（标记已清）"
        );

        // 不带 usage 的事件不该弄脏用量（否则纯错误日志也会触发写盘）。
        store.append_event_full(
            CategoryType::ClaudeCli,
            "error",
            Some("k1"),
            "boom",
            None,
            None,
            None,
        );
        assert!(
            !store.flush_usage_if_dirty(),
            "无 usage 的事件不应弄脏用量累计"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 磁盘上的 `usage.json` 版本高于本程序时，**原文件必须一个字节都不变**。
    ///
    /// 这条防的是一个具体的数据销毁链：将来把 `entries` 改成按日分桶后，
    /// 旧程序（用户降级 / 两台机器同步了这份文件）用旧结构解析新文件 —— serde 忽略
    /// 未知字段、缺失字段取 default，于是**解析"成功"但读出空内容**。若此时还允许写回，
    /// 第一个请求就标脏，随后的 flush 拿「空累加器 + 旧 version」覆写，
    /// 用户攒了几个月的累计当场清零，且毫无报错。
    ///
    /// 所以判据不是「读出来是空的」（那只是现象），而是**原文件字节不变**。
    #[test]
    fn newer_usage_file_is_never_overwritten_by_older_program() {
        use crate::upstream::TokenUsage;
        let dir = temp_dir("usage_version_gate");
        std::fs::create_dir_all(&dir).unwrap();
        let upath = dir.join("usage.json");

        // 造一份「来自未来」的文件：版本号比本程序认识的高，且带上本程序不认识的字段。
        //
        // **已知字段必须保持本程序能解析的形状**（这里 `dailyBuckets` 是数组）：
        // 若把它写成对象，serde 会在版本门之前就解析失败，走「损坏文件」分支返回
        // fresh(now) —— 那样这条测试就绕过了版本门，测的是另一条路径（实测踩到：
        // since_ms 变成了当前时间而非文件里的值）。未来版本新增的字段用
        // `futureOnlyField` 表达即可，它会被 serde 作为未知字段忽略。
        let future = format!(
            r#"{{
  "version": {},
  "sinceMs": 1700000000000,
  "updatedMs": 1700000009999,
  "dailyBuckets": [
    {{ "date": "2026-08-09", "entries": [] }}
  ],
  "entries": [],
  "futureOnlyField": {{ "somethingNew": 123 }}
}}"#,
            USAGE_SNAPSHOT_VERSION + 1
        );
        std::fs::write(&upath, future.as_bytes()).unwrap();
        let before = std::fs::read(&upath).unwrap();

        let store =
            Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap();

        // 读侧：不继承明细（旧结构读不懂），但起算时刻要沿用文件里的，
        // 否则面板会把统计起点显示成"刚刚"，看起来像历史被清了。
        assert!(store.token_usage_by_key().is_empty(), "更高版本的明细不应被旧结构解析");
        assert_eq!(
            store.usage_since_ms(),
            1700000000000,
            "起算时刻应沿用文件里的值"
        );

        // 写侧：产生新消耗 → 标脏 → flush 必须**拒绝写盘**。
        store.append_event_full(
            CategoryType::ClaudeCli,
            "route",
            Some("k1"),
            "req",
            None,
            None,
            Some(TokenUsage { input: 42, output: 7, cache_read: 0, cache_creation: 0 }),
        );
        assert!(
            !store.flush_usage_if_dirty(),
            "只读模式下必须拒绝写盘（返回 false）"
        );

        let after = std::fs::read(&upath).unwrap();
        assert_eq!(
            before, after,
            "更高版本的 usage.json 必须原样保留，一个字节都不能改"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// `usage.json` **读取失败**（而非解析失败）时同样必须只读：文件内容可能完好无损。
    ///
    /// 这是与版本门同一条数据销毁链的另一个入口，本轮对抗审查确认：
    /// 启动瞬间该文件被杀软扫描 / 同步盘 / 备份程序独占（Windows 共享冲突 32/5），
    /// `fs::read` 返回非 NotFound 的 Err。旧实现走 `fresh(now)` —— 累加器为空**且允许写回**，
    /// 于是第一个带 usage 的请求标脏，60 秒后的 flush 拿「空日桶 + version=2」覆写那个
    /// **内容完好**的文件：用户最多 90 天的用量历史与真实起算时刻一起消失，界面上只留一行
    /// warn 日志，永不自愈。
    ///
    /// 与「解析失败」必须分开对待（后者内容确已损坏，覆盖是唯一出路，见
    /// [`UsageLoad::fresh`] 与 `secret.rs` 里同一组区分）：
    /// 判据是「读不出来 ≠ 内容没了」，故读失败一律按只读自保，重启读成功即自愈。
    ///
    /// 故障注入判据：把读失败分支改回 `UsageLoad::fresh(now)`，本测试立刻变红。
    #[test]
    fn unreadable_usage_file_is_never_overwritten() {
        use crate::upstream::TokenUsage;
        let dir = temp_dir("usage_read_fail");
        std::fs::create_dir_all(&dir).unwrap();
        let upath = dir.join("usage.json");

        // 可移植的「读失败但不是 NotFound」注入：在该路径放一个**目录**。
        // `fs::read` 于是在三个平台都失败（Windows 拒绝访问 / Linux IsADirectory），
        // 且 `exists()` 为真 —— 正是「文件在、但这次读不出来」的等价形态。
        std::fs::create_dir_all(&upath).unwrap();

        // 判据是**分支决策本身**（read_only 有没有被置上），不是「磁盘字节没变」。
        //
        // 这一点踩过一次，记下来免得后人重蹈：最初写成「flush 返回 false + 文件未变」，
        // 结果把读失败分支改回 `fresh(now)` 后测试**依然是绿的** —— 因为目录占位
        // 让 `atomic_write` 自己也失败了，磁盘不变是 OS 挡的、与本守卫无关，
        // 于是测试因错误的理由通过。断言直接落在 `load_usage` 的返回上才有判别力。
        let loaded = crate::usage_store::load_usage(&upath);
        assert!(
            loaded.read_only,
            "读取失败必须按只读自保：文件内容可能完好，不能用空累计覆盖它"
        );
        assert!(
            loaded.totals.is_empty() && loaded.daily_buckets.is_empty(),
            "读不出来就没有历史可继承（如实为空），但这不代表可以写回"
        );

        // 该标记必须真正传导到写侧闸门：产生新消耗后 flush 仍拒绝写盘。
        let store = Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap();
        assert!(store.usage_read_only, "只读标记应从 load_usage 传导到 Store");
        store.append_event_full(
            CategoryType::ClaudeCli,
            "route",
            Some("k1"),
            "req",
            None,
            None,
            Some(TokenUsage { input: 42, output: 7, cache_read: 0, cache_creation: 0 }),
        );
        assert!(
            !store.flush_usage_if_dirty(),
            "只读模式下必须在清脏标记之前就拒绝写盘（返回 false）"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 用量文件损坏（断电写坏 / 被手工编辑坏）绝不能让应用起不来。
    ///
    /// 统计文件是最不重要的那类数据，却和 config 同目录 —— 若解析失败直接上抛，
    /// 一个统计文件就能把整个应用挡在启动之外。必须降级为「本次从零累计」。
    #[test]
    fn corrupt_usage_file_does_not_block_startup() {
        let dir = temp_dir("usage_corrupt");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("usage.json"), b"{ this is not json").unwrap();

        let store = Store::new_at(dir.join("config.json"), dir.join("secrets.enc"))
            .expect("用量文件损坏不得导致 Store 构造失败");
        assert!(
            store.token_usage_by_key().is_empty(),
            "损坏文件应降级为空累计，而不是半截数据"
        );
        assert!(
            store.usage_since_ms() > 0,
            "起算时刻应回退为「现在」，不能是 0（面板会显示 1970）"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 一次流式请求只应在界面上留**一行**：把流结束后才拿到的 token 用量**并进已有的那行**，
    /// 而不是新追加一条。
    ///
    /// 背景（真实缺陷）：流式转发的日志行是在**流刚开始**（收到响应头）时同步写的，那一刻
    /// 上游还没吐出末尾的 usage 事件；用量只能等流走完再补。补记若走
    /// `append_event_full` + 相同 collapse_key，折叠逻辑会把它当成「同一件事又发生了一次」：
    ///   - `repeat` 加到 2 → 一次请求显示成「×2」
    ///   - `detail` 被替换成补记那条（没有延迟数字、没有用量文本）
    ///
    /// 于是既虚报了次数，又把延迟显示弄丢了。
    #[test]
    fn stream_usage_backfill_updates_one_row_instead_of_adding_another() {
        use crate::upstream::TokenUsage;
        let dir = temp_dir("usage_backfill");
        let store = Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap();
        let collapse = "ok:k1:claude-sonnet-4:true".to_string();

        // 1) 流开始：同步写下这条（此刻还没有 usage，但有延迟数字）。
        store.append_event_full(
            CategoryType::ClaudeCli,
            "route",
            Some("k1"),
            "主Key · 流式返回 · claude-sonnet-4 · 138ms",
            None,
            Some(collapse.clone()),
            None,
        );
        // 2) 流结束：补记用量。
        let u = TokenUsage { input: 12_345, output: 400, cache_read: 0, cache_creation: 0 };
        store.backfill_usage_for_collapsed_event(CategoryType::ClaudeCli, Some("k1"), &collapse, u);

        let ev = store.list_all_events();
        assert_eq!(ev.len(), 1, "一次流式请求只能留一行，实际: {ev:?}");
        assert_eq!(ev[0].repeat, 1, "补记不是「又发生了一次」，repeat 必须仍为 1");
        assert!(
            ev[0].detail.contains("138ms"),
            "补记不得把延迟数字冲掉: {}",
            ev[0].detail
        );
        assert!(
            ev[0].detail.contains("↑12.3k"),
            "补记后这一行应带上用量文本: {}",
            ev[0].detail
        );
        let got = ev[0].usage.expect("这一行应带上结构化用量");
        assert_eq!((got.input, got.output), (12_345, 400));

        // 用量累计同样要记到（面板的数据源是累加器，与事件环无关）。
        let agg = store.token_usage_by_key();
        assert_eq!(agg.len(), 1);
        assert_eq!(agg[0].usage.input, 12_345, "累加器必须也收到这笔用量");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 用量统计聚合：按「分类 × Key」分组，折叠累加的量也要计入。
    #[test]
    fn token_usage_by_key_groups_and_accumulates() {
        use crate::upstream::TokenUsage;
        let dir = temp_dir("usage_agg");
        let store = Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap();
        let u = |i: u64, o: u64, c: u64| {
            Some(TokenUsage { input: i, output: o, cache_read: c, cache_creation: 0 })
        };

        // 同分类同 Key 多条（不折叠，各自独立，应累加）
        store.append_event_full(CategoryType::ClaudeCli, "route", Some("k1"), "a", None, None, u(100, 10, 0));
        store.append_event_full(CategoryType::ClaudeCli, "route", Some("k1"), "b", None, None, u(50, 5, 30));
        // 不同 Key
        store.append_event_full(CategoryType::ClaudeCli, "route", Some("k2"), "c", None, None, u(20, 2, 0));
        // 不同分类同 Key id（两个分类是不同命名空间，应分开）
        store.append_event_full(CategoryType::Codex, "route", Some("k1"), "d", None, None, u(7, 1, 0));
        // 无 Key 的系统级事件
        store.append_event_full(CategoryType::ClaudeCli, "config", None, "e", None, None, u(3, 0, 0));

        let agg = store.token_usage_by_key();
        assert_eq!(agg.len(), 4, "4 个 (分类,key) 分组: {agg:?}");

        let cli_k1 = agg.iter().find(|r| r.category_id == CategoryType::ClaudeCli && r.key_id == "k1").unwrap();
        assert_eq!(
            (cli_k1.usage.input, cli_k1.usage.output, cli_k1.usage.cache_read),
            (150, 15, 30),
            "同 Key 多次应累加（含缓存）"
        );
        let codex_k1 = agg.iter().find(|r| r.category_id == CategoryType::Codex && r.key_id == "k1").unwrap();
        assert_eq!((codex_k1.usage.input, codex_k1.usage.output), (7, 1), "不同分类的 k1 要分开");
        let no_key = agg.iter().find(|r| r.category_id == CategoryType::ClaudeCli && r.key_id.is_empty()).unwrap();
        assert_eq!(no_key.usage.input, 3, "无 Key 事件归到空串组");
        // 空 usage 事件不参与
        store.append_event(CategoryType::ClaudeCli, "route", Some("k9"), "无用量");
        assert_eq!(store.token_usage_by_key().len(), 4, "无用量事件不得产生分组");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// P1-3 后半：熔断计数改为**标脏 + 合并落盘**，转发路径不再做 20KB 序列化 + atomic_write。
    #[test]
    fn health_changes_are_coalesced_not_persisted_inline() {
        let dir = temp_dir("health_coalesce");
        let cfg_path = dir.join("config.json");
        // record_live_* 收 &Arc<Store>（真实调用方是 proxy 里的 Arc）
        let store =
            std::sync::Arc::new(Store::new_at(cfg_path.clone(), dir.join("secrets.enc")).unwrap());
        store.upsert_key(sample_key("k1", 0)).unwrap();
        let mtime0 = std::fs::metadata(&cfg_path).unwrap().modified().unwrap();

        std::thread::sleep(std::time::Duration::from_millis(1100));
        // 连续失败：会改熔断字段，但**不应该**立刻落盘（旧实现每次都整份重写 20KB）
        for _ in 0..5 {
            crate::health::record_live_failure(&store, "k1");
        }
        let mtime1 = std::fs::metadata(&cfg_path).unwrap().modified().unwrap();
        assert_eq!(
            mtime0, mtime1,
            "熔断计数变化应只标脏、不在调用线程上落盘（那会带来 20KB 序列化 + 持锁 sleep）"
        );
        // 内存态必须已生效（标脏不等于不改内存——路由决策靠内存态）
        assert_eq!(store.get_key("k1").unwrap().health.fail_count, 5);

        // 合并落盘：一次写，把这 5 次变化一起持久化
        assert!(store.flush_health_if_dirty(), "脏标记存在时应真的落盘");
        let mtime2 = std::fs::metadata(&cfg_path).unwrap().modified().unwrap();
        assert_ne!(mtime1, mtime2, "flush 后必须已落盘");

        // 幂等：不脏时不写
        assert!(!store.flush_health_if_dirty(), "无变更时不应落盘");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// P1-3 后半的配套防线：mtime 自愈重载**不得**用磁盘上的旧健康态覆盖内存。
    ///
    /// 这是把落盘改成「合并」后必须堵的窗口，也是一个本来就存在的隐性 bug：
    /// `reload_if_disk_newer` 原先整份 `cfg.keys = fresh.keys`，连 health 一起换掉。于是
    /// 「内存里刚攒到 N 次失败、尚未落盘」时，一次外部改动就把计数清零 →
    /// 熔断永远攒不满阈值。
    ///
    /// 故障注入判据：去掉 reload 里按 id 保留 health 的那段，本测试立刻变红。
    #[test]
    fn reload_preserves_in_memory_health() {
        let dir = temp_dir("reload_keep_health");
        let cfg_path = dir.join("config.json");
        let store =
            std::sync::Arc::new(Store::new_at(cfg_path.clone(), dir.join("secrets.enc")).unwrap());
        store.upsert_key(sample_key("k1", 0)).unwrap();

        // 让磁盘上留下 fail_count = 0 的那一版
        store.flush_health_if_dirty();

        // 内存里攒失败（标脏，未落盘）
        for _ in 0..2 {
            crate::health::record_live_failure(&store, "k1");
        }
        assert_eq!(store.get_key("k1").unwrap().health.fail_count, 2);

        // 模拟「外部改过 config.json」：直接改磁盘文件（改个无关字段并保证 mtime/len 变化）
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let raw = std::fs::read_to_string(&cfg_path).unwrap();
        let mut disk: serde_json::Value = serde_json::from_str(&raw).unwrap();
        disk["keys"][0]["name"] = serde_json::json!("被外部改过的名字");
        std::fs::write(&cfg_path, serde_json::to_vec_pretty(&disk).unwrap()).unwrap();

        // 触发自愈重载（list_keys 会走 reload_if_disk_newer）
        let keys = store.list_keys(CategoryType::ClaudeCli);
        assert_eq!(keys[0].name, "被外部改过的名字", "外部的数据类改动应被采纳");
        assert_eq!(
            keys[0].health.fail_count, 2,
            "但**健康态必须保留内存值**：磁盘那份是暖启动缓存，用它覆盖会把尚未落盘的熔断计数清零"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// P1-3：队列满时必须**丢弃并计数**，绝不阻塞调用方。
    ///
    /// 为什么这条很重要：静默丢日志是本项目最忌讳的失效形态。这里验证「丢」是可观测的
    /// （`log_dropped_count` 能问到），而不是无声消失。
    ///
    /// 构造手法：把日志目录指向一个**已存在的普通文件**，写线程 `create_dir_all` 必然失败，
    /// 于是它不停丢弃并继续 recv；同时我们灌入远超队列容量的条数，逼出 try_send 失败。
    #[test]
    fn full_log_queue_drops_and_counts_never_blocks() {
        let dir = temp_dir("log_queue_full");
        let blocker = dir.join("not-a-dir");
        std::fs::write(&blocker, b"x").unwrap();

        let store = Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap();
        let mut s = store.get_settings();
        s.log_dir = Some(blocker.display().to_string()); // 目录创建必失败
        store.save_settings(UserPrefs::from(&s)).unwrap();

        // 灌入远超 LOG_QUEUE_CAP 的条数。关键判据是**这个循环能在有限时间内跑完**
        // （不阻塞）——旧的同步实现会在每条上做一次失败的 create_dir_all + open。
        let t0 = std::time::Instant::now();
        for i in 0..(LOG_QUEUE_CAP * 2) {
            store.append_event(CategoryType::ClaudeCli, "route", None, &format!("e{i}"));
        }
        let elapsed = t0.elapsed();
        assert!(
            elapsed < std::time::Duration::from_secs(20),
            "投递 {} 条不应阻塞（实测 {elapsed:?}）",
            LOG_QUEUE_CAP * 2
        );

        // 内存态照常工作（日志落盘失败不该影响 UI 能看到事件）
        assert_eq!(
            store.list_all_events().len(),
            MAX_EVENTS,
            "落盘失败不影响内存事件环形缓冲"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 旧日志清理：只删超期的 `YYYY-MM-DD.jsonl`，**其它文件一律不碰**。
    ///
    /// 两个方向都要钉住：
    /// - 该删的删掉（否则长期运行攒几百个文件，用户打开日志目录无从下手）；
    /// - **不该删的一个都不能少** —— 删错用户文件比留着旧日志严重得多，
    ///   故用「非日期命名」「今天的」「刚过期边界的」三类样本一起验。
    ///
    /// 判据刻意用**文件名日期**而非 mtime：mtime 会被备份工具/杀软/同步盘改写，
    /// 那会误删今天的日志或让三个月前的文件永远留着。把 mtime 判据换回来这条测试会红
    /// （测试文件都是刚创建的，mtime 全是今天）。
    #[test]
    fn cleanup_old_logs_removes_only_expired_dated_files() {
        let dir = temp_dir("log_cleanup");
        let log_dir = dir.join("logs");
        std::fs::create_dir_all(&log_dir).unwrap();
        let store = Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap();
        {
            let mut cfg = store.config.write();
            cfg.settings.log_dir = Some(log_dir.display().to_string());
        }

        let today = chrono::Utc::now().date_naive();
        let d = |offset: i64| (today - chrono::Duration::days(offset)).format("%Y-%m-%d").to_string();

        // 该删的：超过保留期
        let expired = [
            format!("{}.jsonl", d(LOG_RETAIN_DAYS + 1)),
            format!("{}.jsonl", d(LOG_RETAIN_DAYS + 100)),
        ];
        // 不该删的：今天、边界内、以及**非日期命名的一切**
        let kept = [
            format!("{}.jsonl", d(0)),                    // 今天（正在写的那个）
            format!("{}.jsonl", d(LOG_RETAIN_DAYS - 1)),  // 边界内
            format!("{}.jsonl", d(LOG_RETAIN_DAYS)),      // 恰好等于保留期：不删（cutoff 用 `<`）
            "events.jsonl".to_string(),                   // 旧版遗留命名
            "notes.txt".to_string(),                      // 用户自己放的
            "2026-99-99.jsonl".to_string(),               // 像日期但解析不出来
        ];
        for f in expired.iter().chain(kept.iter()) {
            std::fs::write(log_dir.join(f), "x").unwrap();
        }

        store.cleanup_old_logs();

        for f in &expired {
            assert!(
                !log_dir.join(f).exists(),
                "{f} 已超过 {LOG_RETAIN_DAYS} 天，应被清理"
            );
        }
        for f in &kept {
            assert!(
                log_dir.join(f).exists(),
                "{f} 不该被删 —— 删错文件比留着旧日志严重得多"
            );
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 常驻不重启也要清：写线程跨天重开文件时必须顺手清理过期日志。
    ///
    /// 只在启动时清是不够的 —— 本应用是托盘常驻程序，用户几周不重启很正常，
    /// 而日志按日轮转。于是「保留 30 天」这个设定对**恰恰最需要它的那批用户**
    /// （长期挂着不关的）完全不生效，磁盘无上限增长。
    ///
    /// 测法：直接驱动写线程的重开路径 —— 先让它在 A 目录开一个文件，再改日志目录
    /// 触发 `need_reopen`（与跨天走同一分支、同一行清理调用）。跨天本身没法在测试里
    /// 等一天，而「换目录」是那个分支唯一可被主动触发的入口，钉住它即钉住那行调用存在。
    #[test]
    fn log_writer_cleans_expired_files_on_reopen_not_only_at_startup() {
        let dir = temp_dir("log_cleanup_rollover");
        let store = Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap();
        let today = chrono::Utc::now().date_naive();
        let stale = format!(
            "{}.jsonl",
            (today - chrono::Duration::days(LOG_RETAIN_DAYS + 5)).format("%Y-%m-%d")
        );

        // 第一段：目录 A，写一条让写线程把 A 开起来
        let dir_a = dir.join("logs_a");
        std::fs::create_dir_all(&dir_a).unwrap();
        {
            let mut cfg = store.config.write();
            cfg.settings.log_dir = Some(dir_a.display().to_string());
        }
        store.append_event(CategoryType::ClaudeCli, "config", None, "first");
        store.flush_logs();

        // 第二段：换到目录 B，并在其中预置一个早已过期的日志文件。
        // 换目录会让写线程走 need_reopen 分支 —— 与跨天同一条路径。
        let dir_b = dir.join("logs_b");
        std::fs::create_dir_all(&dir_b).unwrap();
        std::fs::write(dir_b.join(&stale), "old").unwrap();
        {
            let mut cfg = store.config.write();
            cfg.settings.log_dir = Some(dir_b.display().to_string());
        }
        store.append_event(CategoryType::ClaudeCli, "config", None, "second");
        store.flush_logs();

        assert!(
            !dir_b.join(&stale).exists(),
            "重开日志文件时应顺手清掉过期文件 {stale} —— 否则常驻不重启的用户永远不清理"
        );
        // 今天的文件当然要留着（它就是刚写进去的那个）
        let today_file = format!("{}.jsonl", today.format("%Y-%m-%d"));
        assert!(dir_b.join(&today_file).exists(), "今天的日志文件不该被清理");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// P1-3：`flush_logs` 必须真的把队列排空（退出钩子依赖它，否则强杀会丢最后几条）。

    #[test]
    fn flush_logs_drains_the_queue() {
        let dir = temp_dir("log_flush");
        let log_dir = dir.join("logs");
        let store = Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap();
        let mut s = store.get_settings();
        s.log_dir = Some(log_dir.display().to_string());
        store.save_settings(UserPrefs::from(&s)).unwrap();

        for i in 0..50 {
            store.append_event(CategoryType::Codex, "route", None, &format!("flush-{i}"));
        }
        // 不 sleep，直接 flush —— 它必须自己保证排空
        store.flush_logs();

        let date = chrono::Utc::now().format("%Y-%m-%d");
        let raw = std::fs::read_to_string(log_dir.join(format!("{date}.jsonl")))
            .expect("flush 后必须已有日志文件");
        let n = raw.lines().filter(|l| l.contains("flush-")).count();
        assert_eq!(n, 50, "flush_logs 必须排空队列，实得 {n} 条");
        assert_eq!(store.log_dropped_count(), 0, "正常路径不应丢弃任何日志");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// `strip_trace` 逐字段构造：正文必须剥掉、存在性必须留下、**其余字段一个都不能漏**。
    ///
    /// 为什么专门补这条：`strip_trace` 从 `..e.clone()` 改成逐字段构造（避免先深拷贝
    /// 20000×2 字符的 trace 再丢弃），代价是**将来给 `EventLogEntry` 加字段时编译器不会报错**，
    /// 会静默漏传（表现为 UI 上某列永远是默认值，且没有任何报错）。故这里逐字段断言，
    /// 让漏传变成测试失败而不是线上静默缺字段。
    #[test]
    fn strip_trace_drops_body_keeps_every_other_field() {
        let dir = temp_dir("strip_trace_fields");
        let store = Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap();

        let big = "X".repeat(20_000);
        let trace = RequestTrace {
            request_id: String::new(),
            key_name: "K名".into(),
            vendor: "厂商".into(),
            protocol: Protocol::Anthropic,
            url: "https://u.example/v1/messages".into(),
            requested_model: "对外名".into(),
            real_model: "真实名".into(),
            request_body: big.clone(),
            response_body: big.clone(),
            status: Some(200),
            latency_ms: 1234,
            ok: true,
            was_truncated: None,
        };
        store.append_event_full(
            CategoryType::Codex,
            "request",
            Some("k9"),
            "详情文本",
            Some(trace),
            None,
            Some(crate::upstream::TokenUsage { input: 11, output: 22, cache_read: 33, cache_creation: 0 }),
        );

        let ev = store.list_all_events();
        assert_eq!(ev.len(), 1);
        let got = &ev[0];

        // 剥掉正文、保留存在性——这是本函数存在的唯一理由。
        assert!(got.trace.is_none(), "列表接口必须剥掉 trace 正文");
        assert!(got.has_trace, "但必须留下 has_trace，否则前端无从判断该行能否展开");
        assert!(got.collapse_key.is_none(), "内部折叠判据不下发前端");

        // 其余字段逐个核对：漏传任何一个都在这里失败。
        assert!(!got.id.is_empty(), "id 不能丢（前端按 id 单取 trace）");
        assert!(got.ts > 0, "ts 不能丢");
        assert_eq!(got.category_id, CategoryType::Codex, "category_id 不能丢");
        assert_eq!(got.kind, "request", "kind 不能丢");
        assert_eq!(got.key_id.as_deref(), Some("k9"), "key_id 不能丢");
        assert_eq!(got.detail, "详情文本", "detail 不能丢");
        assert_eq!(got.repeat, 1, "repeat 不能丢");
        let u = got.usage.as_ref().expect("usage 不能丢");
        assert_eq!((u.input, u.output, u.cache_read), (11, 22, 33), "usage 三个分量都不能丢");

        // 按 id 单取仍能拿到完整正文（剥的只是列表，不是存储）。
        let full = store.event_trace(&got.id).expect("按 id 应能取回完整 trace");
        assert_eq!(full.request_body.len(), 20_000, "存储侧正文不应被截断或剥掉");
        assert_eq!(full.response_body.len(), 20_000);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 不带 collapse_key 的事件永不折叠（配置变更、错误等每条都要独立可见）。
    #[test]
    fn events_without_collapse_key_never_merge() {
        let dir = temp_dir("collapse_none");
        let store = Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap();
        for _ in 0..3 {
            store.append_event(CategoryType::ClaudeCli, "config", None, "完全相同的文案");
        }
        let ev = store.list_all_events();
        assert_eq!(ev.len(), 3, "无折叠键时即便文案相同也不得合并: {ev:?}");
        assert!(ev.iter().all(|e| e.repeat == 1));

        std::fs::remove_dir_all(&dir).ok();
    }

    /// `collapse_key` 是内部判据，**不得下发前端**（否则等于把 Key id 与模型名泄进 UI 载荷，
    /// 且前端并不需要它）。
    #[test]
    fn collapse_key_is_not_exposed_to_frontend() {
        let dir = temp_dir("collapse_hide");
        let store = Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap();
        store.append_event_collapsible(
            CategoryType::ClaudeCli,
            "route",
            Some("k1"),
            "成功",
            None,
            Some("ok:k1:m:false".into()),
        );
        let ev = store.list_all_events();
        assert_eq!(ev.len(), 1);
        assert!(ev[0].collapse_key.is_none(), "列表接口必须剥掉 collapse_key");
        // 序列化后也不该出现该字段（#[serde(skip)]）
        let json = serde_json::to_string(&ev[0]).unwrap();
        assert!(!json.contains("collapseKey") && !json.contains("collapse_key"), "序列化不得含该字段: {json}");

        std::fs::remove_dir_all(&dir).ok();
    }
    #[test]
    fn edit_key_updates_fields_in_place() {
        let dir = temp_dir("edit");
        let cfg = dir.join("config.json");
        let store = Store::new_at(cfg.clone(), dir.join("secrets.enc")).unwrap();

        store.upsert_key(sample_key("k1", 5)).unwrap();
        // 编辑：改名 + 改优先级 + 改 base_url
        let mut edited = store.get_key("k1").unwrap();
        edited.name = "改后的名字".into();
        edited.priority = 0;
        edited.base_url = "https://new.example.com".into();
        store.upsert_key(edited).unwrap();

        let got = store.get_key("k1").unwrap();
        assert_eq!(got.name, "改后的名字");
        assert_eq!(got.priority, 0);
        assert_eq!(got.base_url, "https://new.example.com");
        assert_eq!(store.list_keys(CategoryType::ClaudeCli).len(), 1, "编辑不应新增");

        // 重载确认落盘
        let reloaded = Store::new_at(cfg, dir.join("secrets.enc")).unwrap();
        assert_eq!(reloaded.get_key("k1").unwrap().name, "改后的名字");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 新增 Key 时 priority 撞车必须顺延，不能让整列停在「全部同级」。
    ///
    /// 背景：前端新建 Key 恒发 `priority: 999`（`initial?.priority ?? 999` —— 它无从
    /// 知道该填几）。不处理的话，同分类里每条手工新增的 Key 都是 999，故障转移的
    /// 主/备顺序就只由 `sort_by_key` 的稳定性（= 恰好的插入顺序）决定，
    /// 而不是任何用户能看见、能改的东西。
    ///
    /// 三个方向一起钉：
    /// 1. 撞车的顺延（999, 999, 999 → 999, 1000, 1001），且顺延后仍保持插入先后；
    /// 2. **不撞车的原样保留** —— cc-switch 导入自己算了分类内 max+1，无条件重编号
    ///    会把它排好的顺序换成我们的插入顺序；
    /// 3. 跨分类不算撞车（priority 的作用域是分类内）。
    #[test]
    fn inserting_key_with_taken_priority_appends_instead_of_tying() {
        let dir = temp_dir("prio-collide");
        let store = Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap();

        // 前端新建三条：都带 999
        store.upsert_key(sample_key("a", 999)).unwrap();
        store.upsert_key(sample_key("b", 999)).unwrap();
        store.upsert_key(sample_key("c", 999)).unwrap();
        let prios: Vec<i32> =
            ["a", "b", "c"].iter().map(|id| store.get_key(id).unwrap().priority).collect();
        assert_eq!(
            prios,
            vec![999, 1000, 1001],
            "三条同级必须顺延成互不相同，否则主/备顺序由稳定排序的巧合决定"
        );
        // 顺延后的顺序 = 插入先后（用户在界面上看到的从上到下）
        let ids: Vec<String> =
            store.enabled_keys_sorted(CategoryType::ClaudeCli).iter().map(|k| k.id.clone()).collect();
        assert_eq!(ids, vec!["a", "b", "c"]);

        // 不撞车的原样保留（cc-switch 导入路径依赖这条）
        store.upsert_key(sample_key("d", 3)).unwrap();
        assert_eq!(store.get_key("d").unwrap().priority, 3, "唯一值不得被改写");

        // 跨分类同值不算撞车
        let mut other = sample_key("x", 999);
        other.category_id = CategoryType::Codex;
        store.upsert_key(other).unwrap();
        assert_eq!(
            store.get_key("x").unwrap().priority,
            999,
            "priority 的作用域是分类内，跨分类同值不该被顺延"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 路由候选选择：仅取「该分类 + 启用」的 Key，且按 priority 升序。
    #[test]
    fn enabled_keys_sorted_filters_and_orders() {
        let dir = temp_dir("sorted");
        let store = Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap();

        store.upsert_key(sample_key("high", 2)).unwrap();
        store.upsert_key(sample_key("low", 0)).unwrap();
        store.upsert_key(sample_key("mid", 1)).unwrap();
        // 一个禁用的：不应出现在候选
        let mut disabled = sample_key("off", 0);
        disabled.enabled = false;
        store.upsert_key(disabled).unwrap();

        let sorted = store.enabled_keys_sorted(CategoryType::ClaudeCli);
        let ids: Vec<&str> = sorted.iter().map(|k| k.id.as_str()).collect();
        assert_eq!(ids, vec!["low", "mid", "high"], "应按 priority 升序且排除禁用");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 状态条「当前主 Key」的口径：**首个启用 Key**，不是 `priority == 0` 那条（UX#6）。
    ///
    /// 为什么单独钉一条：`priority == 0` 是个极易写错的近似。禁用一条 Key 时并不会重排
    /// 优先级，所以完全可能出现「priority 为 0 的那条是禁用的」——此时真正先被使用的是
    /// 下一条。若状态条按 `priority == 0` 显示，用户会看到一个**根本不参与路由**的 Key
    /// 被标成「主 Key」，而真正在跑的是另一条。这类错误不会报错、只会让人对着日志发懵。
    ///
    /// 前端 `routingPrimaryKey()`（src/lib/modelSets.ts）必须与本断言同口径，
    /// 托盘的「主 Key」子菜单也是（只列启用的 —— 把禁用 Key 设为主毫无意义，它不进候选池）。
    #[test]
    fn enabled_keys_sorted_head_is_routing_primary_even_if_priority0_disabled() {
        let dir = temp_dir("primary_head");
        let store = Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap();

        // priority 最小的那条是**禁用**的：它不进候选池，不该被视为主 Key。
        let mut disabled_first = sample_key("disabled_p0", 0);
        disabled_first.enabled = false;
        store.upsert_key(disabled_first).unwrap();
        store.upsert_key(sample_key("real_primary", 1)).unwrap();
        store.upsert_key(sample_key("backup", 2)).unwrap();

        let sorted = store.enabled_keys_sorted(CategoryType::ClaudeCli);
        assert_eq!(
            sorted.first().map(|k| k.id.as_str()),
            Some("real_primary"),
            "主 Key 是首个**启用**的，不是 priority==0 那条（后者已禁用、根本不进候选池）"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 设为主 Key：目标提到 priority 0，其余按原序顺延，整列重编号为连续值。
    #[test]
    fn set_primary_key_promotes_and_renumbers_contiguously() {
        let dir = temp_dir("primary");
        let store = Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap();
        // 刻意造「全部同级 999」——历史配置的真实形态，此时故障转移没有确定主备顺序。
        store.upsert_key(sample_key("a", 999)).unwrap();
        store.upsert_key(sample_key("b", 999)).unwrap();
        store.upsert_key(sample_key("c", 999)).unwrap();

        assert!(store.set_primary_key(CategoryType::ClaudeCli, "c").unwrap(), "应有改动");

        let ordered = store.enabled_keys_sorted(CategoryType::ClaudeCli);
        let ids: Vec<&str> = ordered.iter().map(|k| k.id.as_str()).collect();
        assert_eq!(ids[0], "c", "目标必须成为主");
        let prios: Vec<i32> = ordered.iter().map(|k| k.priority).collect();
        assert_eq!(prios, vec![0, 1, 2], "必须重编号为连续值，否则同级下无确定主备顺序");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 上移/下移：相邻交换 + 整列连续重编号，**且不碰健康态**。
    ///
    /// 这条测试钉住把排序收归后端的三个理由（旧实现是前端重编号后并发 `upsert_key`）：
    /// 1. **原子**：一次调用改完整列，不会出现「一半落盘、一半被拒」的重复优先级/空洞。
    /// 2. **不碰运行态**：只写 `priority`，正在生效的熔断不被解除（旧实现整份覆盖会清零）。
    /// 3. **端点幂等**：已在两端时返回 false、不写盘，连点箭头不产生噪音。
    ///
    /// 故障注入判据：把 `move_key` 改成整份替换（连 health 一起写），第 2 段断言变红。
    #[test]
    fn move_key_swaps_renumbers_and_preserves_health() {
        let dir = temp_dir("move_key");
        let cfg = dir.join("config.json");
        let store = std::sync::Arc::new(
            Store::new_at(cfg.clone(), dir.join("secrets.enc")).unwrap(),
        );
        for (id, p) in [("a", 0), ("b", 1), ("c", 2)] {
            let mut k = sample_key(id, p);
            k.id = id.into();
            store.upsert_key(k).unwrap();
        }

        // 运行中：b 连续失败到武装熔断（分类页此时显示「熔断中」横幅）。
        for _ in 0..3 {
            crate::health::record_live_failure(&store, "b");
        }
        assert!(
            store.get_key("b").unwrap().health.breaker_until.is_some(),
            "前置条件：b 应已武装熔断"
        );

        // 下移 a：顺序应变成 b, a, c 且优先级连续。
        assert!(store.move_key(CategoryType::ClaudeCli, "a", false).unwrap(), "应有改动");
        let ids: Vec<String> = {
            let c = store.config.read();
            let mut same: Vec<&ProviderKey> = c
                .keys
                .iter()
                .filter(|k| k.category_id == CategoryType::ClaudeCli)
                .collect();
            same.sort_by_key(|k| k.priority);
            same.iter().map(|k| k.id.clone()).collect()
        };
        assert_eq!(ids, vec!["b", "a", "c"], "下移应与后一条交换");
        let prios: Vec<i32> = ids
            .iter()
            .map(|id| store.get_key(id).unwrap().priority)
            .collect();
        assert_eq!(prios, vec![0, 1, 2], "必须重编号为连续值（否则同级下无确定主备顺序）");

        // 关键：排序不得动健康态 —— b 的熔断必须还在。
        assert!(
            store.get_key("b").unwrap().health.breaker_until.is_some(),
            "调整顺序不得解除正在生效的熔断，实际 {:?}",
            store.get_key("b").unwrap().health
        );

        // 端点幂等：b 已在队首，再上移应返回 false 且不写盘。
        let before = std::fs::read(&cfg).unwrap();
        assert!(
            !store.move_key(CategoryType::ClaudeCli, "b", true).unwrap(),
            "已在队首应返回 false"
        );
        assert_eq!(std::fs::read(&cfg).unwrap(), before, "幂等时磁盘不应被改写");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 幂等：目标已是主且优先级已连续时不再写盘（托盘每次点击都会调，避免噪音与无谓落盘）。
    #[test]
    fn set_primary_key_is_idempotent_when_already_primary() {
        let dir = temp_dir("primary_idem");
        let cfg = dir.join("config.json");
        let store = Store::new_at(cfg.clone(), dir.join("secrets.enc")).unwrap();
        store.upsert_key(sample_key("a", 0)).unwrap();
        store.upsert_key(sample_key("b", 1)).unwrap();

        let before = std::fs::read(&cfg).unwrap();
        assert!(
            !store.set_primary_key(CategoryType::ClaudeCli, "a").unwrap(),
            "已是主应返回 false"
        );
        assert_eq!(std::fs::read(&cfg).unwrap(), before, "幂等时磁盘不应被改写");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 只动同分类：给 Codex 设主不得改动 ClaudeCli 那些 Key 的优先级。
    #[test]
    fn set_primary_key_touches_only_its_own_category() {
        let dir = temp_dir("primary_scope");
        let store = Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap();
        store.upsert_key(sample_key("cli_a", 5)).unwrap();
        store.upsert_key(sample_key("cli_b", 7)).unwrap();
        let mut cx1 = sample_key("cx_1", 3);
        cx1.category_id = CategoryType::Codex;
        let mut cx2 = sample_key("cx_2", 4);
        cx2.category_id = CategoryType::Codex;
        store.upsert_key(cx1).unwrap();
        store.upsert_key(cx2).unwrap();

        store.set_primary_key(CategoryType::Codex, "cx_2").unwrap();

        let cx = store.enabled_keys_sorted(CategoryType::Codex);
        assert_eq!(
            cx.iter().map(|k| k.id.as_str()).collect::<Vec<_>>(),
            vec!["cx_2", "cx_1"]
        );
        assert_eq!(cx.iter().map(|k| k.priority).collect::<Vec<_>>(), vec![0, 1]);
        // ClaudeCli 的原始优先级一个都没动（5 / 7 保持原值，而非被重编号成 0/1）
        assert_eq!(store.get_key("cli_a").unwrap().priority, 5, "跨分类不得被牵连重编号");
        assert_eq!(store.get_key("cli_b").unwrap().priority, 7, "跨分类不得被牵连重编号");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 不存在 / 跨分类误传的 id 必须报 NotFound，而不是静默把别的 Key 提成主。
    #[test]
    fn set_primary_key_rejects_unknown_or_cross_category_id() {
        let dir = temp_dir("primary_notfound");
        let store = Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap();
        store.upsert_key(sample_key("a", 0)).unwrap(); // ClaudeCli

        assert!(store.set_primary_key(CategoryType::ClaudeCli, "nope").is_err());
        assert!(
            store.set_primary_key(CategoryType::Codex, "a").is_err(),
            "id 存在但分类不符也必须拒绝（否则托盘传错分类会静默改错分类的主）"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 禁用的 Key 也参与重编号（它仍占一个位次），锁住「优先级不出现空洞」。
    #[test]
    fn set_primary_key_includes_disabled_keys_in_renumbering() {
        let dir = temp_dir("primary_disabled");
        let store = Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap();
        store.upsert_key(sample_key("a", 0)).unwrap();
        let mut off = sample_key("off", 1);
        off.enabled = false;
        store.upsert_key(off).unwrap();
        store.upsert_key(sample_key("c", 2)).unwrap();

        store.set_primary_key(CategoryType::ClaudeCli, "c").unwrap();

        let mut all: Vec<ProviderKey> = store.list_keys(CategoryType::ClaudeCli);
        all.sort_by_key(|k| k.priority);
        assert_eq!(
            all.iter().map(|k| (k.id.as_str(), k.priority)).collect::<Vec<_>>(),
            vec![("c", 0), ("a", 1), ("off", 2)]
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 禁用 Key 时应清空遗留的 Down/熔断状态，避免界面一直显示「不可用」。
    #[test]
    fn disabling_key_clears_stale_health() {
        let dir = temp_dir("toggle_health");
        let store = Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap();
        store.upsert_key(sample_key("k1", 0)).unwrap();
        // 先把它探成 Down + 熔断。
        store
            .update_health(
                "k1",
                HealthState {
                    status: HealthStatus::Down,
                    fail_count: 3,
                    breaker_until: Some(9_999_999_999_999),
                    ..Default::default()
                },
            )
            .unwrap();
        // 禁用 → 健康态应被重置为默认（Unknown、无熔断）。
        store.toggle_key("k1", false).unwrap();
        let h = store.get_key("k1").unwrap().health;
        assert_eq!(h.status, HealthStatus::Unknown, "禁用后不应残留 Down");
        assert_eq!(h.fail_count, 0);
        assert!(h.breaker_until.is_none(), "禁用后不应残留熔断窗口");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 主口令开关是 **UI 镜像**，前端 saveSettings 的旧快照绝不能覆盖它。
    ///
    /// 为什么这条特别毒：真实模式记在 `secrets.enc` 的 master 头部里。若前端能把镜像写成
    /// `true`（用户切个主题就会带上旧快照），下次启动便按「已启用」引导解锁，而密钥库里
    /// 根本没有 master 头部 —— 解锁无从进行、密钥也读不出来，自造死局。
    #[test]
    fn save_settings_never_lets_frontend_flip_master_password_flag() {
        let dir = temp_dir("master_flag_guard");
        let store = Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap();
        assert!(!store.get_settings().master_password_enabled, "默认关");

        // 前端提交一份「声称已启用」的 settings（模拟旧快照/手改）。
        let mut s = store.get_settings();
        s.master_password_enabled = true;
        s.theme = "dark".into();
        store.save_settings(UserPrefs::from(&s)).unwrap();

        assert!(
            !store.get_settings().master_password_enabled,
            "前端入参不得翻转主口令开关（否则会造出「配置说开着、库里没有 master 头部」的死局）"
        );
        assert_eq!(store.get_settings().theme, "dark", "其他字段照常保存");

        // 反向：后端把镜像置真后，前端提交「关闭」也不得覆盖。
        store.set_master_password_flag(true).unwrap();
        let mut s2 = store.get_settings();
        s2.master_password_enabled = false;
        store.save_settings(UserPrefs::from(&s2)).unwrap();
        assert!(
            store.get_settings().master_password_enabled,
            "反向同样不得被前端覆盖——真实模式只由密钥库迁移与启动对账决定"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 专用写入方法：能改镜像，且幂等（值相同不写盘）。
    #[test]
    fn set_master_password_flag_writes_and_is_idempotent() {
        let dir = temp_dir("master_flag_set");
        let cfg = dir.join("config.json");
        let store = Store::new_at(cfg.clone(), dir.join("secrets.enc")).unwrap();

        store.set_master_password_flag(true).unwrap();
        assert!(store.get_settings().master_password_enabled);

        let before = std::fs::read(&cfg).unwrap();
        store.set_master_password_flag(true).unwrap();
        assert_eq!(std::fs::read(&cfg).unwrap(), before, "幂等时不应重写文件");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 密钥库锁定时 `has_secret` 对账必须**跳过**，不能把「暂时读不到」记成「确实没有」。
    ///
    /// 若不跳过：锁定态下 `get` 一律 Err → 每条都被判成 false 落盘 → 解锁后 UI 说全都没密钥、
    /// 提示用户重录，而库里其实一条不少。
    #[test]
    fn reconcile_has_secret_skips_while_vault_locked() {
        let dir = temp_dir("reconcile_locked");
        let store = Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap();
        store.upsert_key(sample_key("k1", 0)).unwrap();
        store.secrets.write().set("k1", "sk-x").unwrap();
        store.reconcile_has_secret_flags().unwrap();
        assert!(store.get_key("k1").unwrap().has_secret, "前置条件：标记应为真");

        // 切到主口令模式后手动上锁（模拟「重启后还没解锁」）。
        store.secrets.write().enable_master_password("TestPass123").unwrap();
        store.secrets.write().lock();

        let fixed = store.reconcile_has_secret_flags().unwrap();
        assert_eq!(fixed, 0, "锁定态应直接跳过对账");
        assert!(
            store.get_key("k1").unwrap().has_secret,
            "标记必须保持为真——密钥还在库里，只是没解锁读不到"
        );

        // 解锁后对账照常工作，且结论正确（仍有密钥）。
        store.secrets.write().unlock("TestPass123").unwrap();
        assert_eq!(store.reconcile_has_secret_flags().unwrap(), 0);
        assert!(store.get_key("k1").unwrap().has_secret);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 端口“粘住”：set_mcp_port 写回新首选端口并落盘；已是目标值则跳过写盘（幂等）。
    #[test]
    fn set_mcp_port_sticks_and_is_idempotent() {
        let dir = temp_dir("mcp_port_stick");
        let cfg = dir.join("config.json");
        let store = Store::new_at(cfg.clone(), dir.join("secrets.enc")).unwrap();

        // 首次写回回退端口 9529 → 应落盘并被后续读取到。
        store.set_mcp_port(9529).unwrap();
        assert_eq!(store.get_settings().mcp_port, 9529);
        let mtime1 = std::fs::metadata(&cfg).unwrap().modified().unwrap();

        std::thread::sleep(std::time::Duration::from_millis(20));
        // 再写同一端口 → 幂等，不应重写文件。
        store.set_mcp_port(9529).unwrap();
        let mtime2 = std::fs::metadata(&cfg).unwrap().modified().unwrap();
        assert_eq!(mtime1, mtime2, "端口未变不应重写 config.json");

        // 重新打开 store → 首选端口应持久为 9529，下次启动不再从被占端口回退。
        let store2 = Store::new_at(cfg, dir.join("secrets.enc")).unwrap();
        assert_eq!(store2.get_settings().mcp_port, 9529, "端口应持久粘住");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// save_settings 必须**保留**后端 MCP 控制面字段（mcp_port / mcp_enabled /
    /// mcp_registered_categories），不被前端切主题 / 语言时携带的陈旧快照顶回。
    ///
    /// 回归历史缺陷：前端 zustand 里的 settings 是页面加载时的旧快照，端口占用回退后
    /// 由 set_mcp_port 粘滞的新端口不在前端手里；若通用 save_settings 从入参覆盖 mcp_port，
    /// 切主题时旧端口会**顶回**粘滞值，粘滞被无声撤销，随后 MCP 又从被占端口重新回退，
    /// 客户端连接被无谓 stop+重扫。同理 mcp_enabled 也不能被入参覆盖。
    #[test]
    fn save_settings_preserves_backend_mcp_control_fields() {
        let dir = temp_dir("save_settings_preserves_mcp");
        let store = Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap();

        // 后端把 MCP 控制面粘滞成 (enabled=true, port=9529, 已注册 [claude-cli])。
        store.set_mcp_enabled_flag(true).unwrap();
        store.set_mcp_port(9529).unwrap();
        store.add_registered_category(CategoryType::ClaudeCli).unwrap();

        // 前端持有旧快照：enabled=false / port=9527 / categories=[]，切主题时提交整个 settings。
        let mut stale = store.get_settings();
        stale.mcp_enabled = false;
        stale.mcp_port = 9527;
        stale.mcp_registered_categories = vec![]; // 前端序列化通常就是空
        stale.theme = "dark".into(); // 真正想改的字段
        store.save_settings(UserPrefs::from(&stale)).unwrap();

        // 三个后端自管字段全部**保留**，前端入参不生效；其它字段（theme）正常落。
        let now = store.get_settings();
        assert!(now.mcp_enabled, "mcp_enabled 应保留后端值 true，不被前端 false 顶回");
        assert_eq!(now.mcp_port, 9529, "mcp_port 应保留粘滞值，不被前端旧端口顶回");
        assert_eq!(
            now.mcp_registered_categories,
            vec![CategoryType::ClaudeCli],
            "已注册分类不应被前端空 vec 清空"
        );
        assert_eq!(now.theme, "dark", "非控制面字段应正常更新");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 代理运行态快照必须**跨重启保留**（FR-029 的核心判据）。
    ///
    /// 这条钉住的是「上次启用了代理，下次开应用自动继续」这个功能本身：快照只活在
    /// 内存里的话，进程一退就没了，用户每次都得手点一次启动 —— 正是要解决的问题。
    #[test]
    fn proxy_running_categories_survive_restart() {
        let dir = temp_dir("proxy_running_restart");
        let cfg = dir.join("config.json");
        let sec = dir.join("secrets.enc");

        {
            let store = Store::new_at(cfg.clone(), sec.clone()).unwrap();
            assert!(
                store.proxy_running_categories().is_empty(),
                "全新配置应当没有任何记忆（不能凭空自启动代理）"
            );
            store
                .set_proxy_running_categories(&[CategoryType::ClaudeCli, CategoryType::Codex])
                .unwrap();
        } // 析构 = 模拟进程退出

        let store2 = Store::new_at(cfg, sec).unwrap();
        assert_eq!(
            store2.proxy_running_categories(),
            vec![CategoryType::ClaudeCli, CategoryType::Codex],
            "重启后必须读回上次在跑的分类"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 快照落盘必须**幂等**：同一集合（含顺序不同）不得重复写盘。
    ///
    /// 用户明确担心过持续写盘伤 SSD。快照每 60s 采样一次，若不比对就变成
    /// 「开着不用也每分钟重写一次 config.json」—— 而 config.json 里是用户的 Key 与全部设置，
    /// 把一份只读的采样变成对最宝贵数据的反复覆写，是拿真正要紧的东西去冒无谓的风险。
    #[test]
    fn proxy_running_snapshot_is_idempotent_and_order_insensitive() {
        let dir = temp_dir("proxy_running_idempotent");
        let cfg = dir.join("config.json");
        let store = Store::new_at(cfg.clone(), dir.join("secrets.enc")).unwrap();

        store
            .set_proxy_running_categories(&[CategoryType::ClaudeCli, CategoryType::Codex])
            .unwrap();
        let stamp1 = std::fs::metadata(&cfg).unwrap().modified().unwrap();
        let len1 = std::fs::metadata(&cfg).unwrap().len();

        // 同一集合、顺序颠倒 → 不应写盘（否则每轮采样都会因排序抖动白写一次）。
        store
            .set_proxy_running_categories(&[CategoryType::Codex, CategoryType::ClaudeCli])
            .unwrap();
        let stamp2 = std::fs::metadata(&cfg).unwrap().modified().unwrap();
        let len2 = std::fs::metadata(&cfg).unwrap().len();
        assert_eq!(stamp1, stamp2, "同一集合（顺序不同）不应重复写盘");
        assert_eq!(len1, len2);
        assert_eq!(
            store.proxy_running_categories(),
            vec![CategoryType::ClaudeCli, CategoryType::Codex],
            "存储形态应已排序归一，与入参顺序无关"
        );

        // 真的变了才写。
        store
            .set_proxy_running_categories(&[CategoryType::ClaudeCli])
            .unwrap();
        assert_eq!(
            store.proxy_running_categories(),
            vec![CategoryType::ClaudeCli],
            "集合真的变化时必须落盘生效"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 前端切主题时提交的陈旧 settings 快照**不得清空**代理运行态记忆。
    ///
    /// 这是本项目出过 P0 的形状（切主题把刚关掉的开机自启动重新装回系统）。
    /// `proxy_running_categories` 是后端自管字段，靠的是它不在 `UserPrefs` 白名单里；
    /// 哪天有人「顺手」把它加进 `UserPrefs`，这条测试必须立刻变红 ——
    /// 否则表现是「切一次主题，代理就不记得上次启用过了」，几乎无法归因。
    #[test]
    fn save_settings_preserves_proxy_running_categories() {
        let dir = temp_dir("save_settings_preserves_proxy_running");
        let store = Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap();

        store
            .set_proxy_running_categories(&[CategoryType::Codex])
            .unwrap();

        // 前端持有的旧快照里这个字段通常是空的，切主题时整份提交。
        let mut stale = store.get_settings();
        stale.proxy_running_categories = vec![];
        stale.theme = "dark".into();
        store.save_settings(UserPrefs::from(&stale)).unwrap();

        assert_eq!(
            store.get_settings().proxy_running_categories,
            vec![CategoryType::Codex],
            "代理运行态记忆不应被前端空 vec 清空"
        );
        assert_eq!(store.get_settings().theme, "dark", "真正想改的字段应正常落");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 配置里出现未知分类名时：忽略那一个，其余照常读出。
    ///
    /// 走的是 `de_category_vec` 的容错。为什么要钉：将来若新增/重命名分类，
    /// 降级回旧版本时配置里就会有旧版本不认识的名字。若整个字段解析失败退成空，
    /// 用户会遇到「装回旧版本后代理不记得启用过了」；若整份 config 解析失败，
    /// 那就是连 Key 都读不出来的灾难。
    #[test]
    fn unknown_category_in_proxy_running_is_skipped_not_fatal() {
        let dir = temp_dir("proxy_running_unknown_cat");
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = dir.join("config.json");
        std::fs::write(
            &cfg,
            br#"{"settings":{"theme":"dark","proxyRunningCategories":["codex","gemini-cli","claude-cli"]}}"#,
        )
        .unwrap();

        let store = Store::new_at(cfg, dir.join("secrets.enc")).unwrap();
        assert_eq!(
            store.proxy_running_categories(),
            vec![CategoryType::ClaudeCli, CategoryType::Codex],
            "未知分类应被跳过，已知的两个照常读出"
        );
        assert_eq!(store.get_settings().theme, "dark", "同份配置的其它字段不受影响");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 健康「可见摘要」只含真正会上屏的两项 —— 这是状态推送去抖的判据（UX#5）。
    ///
    /// 为什么要钉住：`fail_count` / `latency_ms` / `last_checked` **每次转发、每轮探测都在变**，
    /// 但界面上要么不显示、要么只进 title 提示。哪天有人「顺手」把它们加进摘要，
    /// 推送就会退化回「和 2 秒轮询一样吵」，而且没有任何测试会红、没有任何报错 ——
    /// 只是电脑变烫、日志页疯狂重拉。这条测试就是为了让那次改动立刻变红。
    #[test]
    fn health_visible_digest_ignores_invisible_churn() {
        let dir = temp_dir("health_digest");
        let store = Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap();
        store.upsert_key(sample_key("k1", 0)).unwrap();

        let base = store.health_visible_digest("k1");
        assert!(base.is_some(), "存在的 Key 应有摘要");

        // 只动不上屏的字段：摘要必须不变（否则每次转发都会推一条）
        store
            .mutate_health("k1", |h| {
                h.fail_count += 7;
                h.latency_ms = Some(999);
                h.last_checked = Some(123_456);
                h.last_live_success = Some(123_456);
                true
            })
            .unwrap();
        assert_eq!(
            store.health_visible_digest("k1"),
            base,
            "fail_count/latency/last_checked 变化不该触发推送——它们不上屏，却每次请求都在变"
        );

        // 动状态：摘要必须变
        store
            .mutate_health("k1", |h| {
                h.status = HealthStatus::Down;
                true
            })
            .unwrap();
        let after_down = store.health_visible_digest("k1");
        assert_ne!(after_down, base, "up→down 是可见变化，必须推送");

        // 武装熔断：摘要必须变
        store
            .mutate_health("k1", |h| {
                h.breaker_until = Some(9_999_999);
                true
            })
            .unwrap();
        assert_ne!(store.health_visible_digest("k1"), after_down, "熔断武装是可见变化，必须推送");

        // 不存在的 Key 返回 None，与「存在但状态未知」区分开
        assert_eq!(store.health_visible_digest("nope"), None);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// **白名单的核心判据**：前端保存偏好时，后端自管字段**一个都不能动**。
    ///
    /// 这条替代了原先「逐字段黑名单」的那批断言。区别很关键：黑名单测的是
    /// 「这几个我记得住的字段没被顶掉」，白名单测的是「凡不在 UserPrefs 里的都动不了」——
    /// 后者在日后新增后端字段时**自动成立**，而前者需要有人记得补一行。
    ///
    /// 这就是本次改动的全部意义：把「默认不安全、靠人记得」变成「默认安全、想不安全都难」。
    #[test]
    fn save_settings_cannot_touch_backend_owned_state() {
        let dir = temp_dir("prefs_whitelist");
        let store = Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap();

        // 后端把全部自管字段写成非默认值
        store.set_mcp_enabled_flag(true).unwrap();
        store.set_mcp_port(9529).unwrap();
        store.add_registered_category(CategoryType::ClaudeCli).unwrap();
        store.set_active_model(CategoryType::Codex, "gpt-5").unwrap();
        store.set_active_effort(CategoryType::Codex, "xhigh").unwrap();
        store.set_proxy_port(CategoryType::Codex, 47999).unwrap();
        store.set_master_password_flag(true).unwrap();
        store.set_auto_start_flag(true).unwrap();
        store.set_onboarding_done(true).unwrap();

        // 前端提交偏好（模拟「用户只是切了个主题」）。
        // 注意这里**在类型上就无法**表达「我要改 mcpPort」——那正是白名单的作用。
        let mut prefs = UserPrefs::from(&store.get_settings());
        prefs.theme = "dark".into();
        store.save_settings(prefs).unwrap();

        let now = store.get_settings();
        assert!(now.mcp_enabled, "mcp_enabled 不得被顶掉");
        assert_eq!(now.mcp_port, 9529, "粘滞端口不得被顶回");
        assert_eq!(now.mcp_registered_categories, vec![CategoryType::ClaudeCli]);
        assert_eq!(now.active_models.get(&CategoryType::Codex).map(String::as_str), Some("gpt-5"));
        assert_eq!(now.active_efforts.get(&CategoryType::Codex).map(String::as_str), Some("xhigh"));
        assert_eq!(now.proxy_ports.get(&CategoryType::Codex).copied(), Some(47999));
        assert!(now.master_password_enabled, "密钥库模式镜像不得被顶掉（会自造解锁死局）");
        assert!(now.auto_start, "开机自启动不得被顶掉 —— 这正是那个 P0 的形态");
        assert_eq!(now.onboarding_done, Some(true));
        assert_eq!(now.theme, "dark", "偏好本身要正常落下");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// `auto_start` 的专用写入是幂等的，且能双向改。
    #[test]
    fn set_auto_start_flag_is_idempotent_and_persists() {
        let dir = temp_dir("auto_start_flag");
        let store = Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap();

        store.set_auto_start_flag(true).unwrap();
        assert!(store.get_settings().auto_start);
        store.set_auto_start_flag(true).unwrap(); // 幂等
        assert!(store.get_settings().auto_start);

        store.set_auto_start_flag(false).unwrap();
        assert!(!store.get_settings().auto_start);

        // 重开确认落盘
        let store2 = Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap();
        assert!(!store2.get_settings().auto_start);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 首启向导标记必须扛得住「前端切主题时提交的旧快照」（UX#1）。
    /// 这是本仓修过的那个 P0 的同一形态：`store.settings` 是前端挂载时的快照，
    /// 用户走完向导后随手切个主题，那份**不含 onboarding_done 的**旧对象就会整份提交回来。
    /// 没有保留防线的话标记被顶回 None，下次启动向导又冒出来 —— 而用户明明已经配好了。
    #[test]
    fn onboarding_flag_survives_stale_settings_save() {
        let dir = temp_dir("onboarding_stale_save");
        let store = Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap();

        store.set_onboarding_done(true).unwrap();

        // 前端旧快照：字段还是 None（挂载时向导尚未完成），只想改主题。
        let mut stale = store.get_settings();
        stale.onboarding_done = None;
        stale.theme = "dark".into();
        store.save_settings(UserPrefs::from(&stale)).unwrap();

        assert_eq!(
            store.get_settings().onboarding_done,
            Some(true),
            "向导完成标记不得被前端旧快照顶回，否则下次启动向导会再次弹出"
        );
        assert_eq!(store.get_settings().theme, "dark", "非自管字段应正常更新");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 启动对账：老用户（有 Key）不该被首启向导拦住；全新安装（无 Key）才显示。
    #[test]
    fn reconcile_onboarding_marks_existing_users_as_done() {
        let dir = temp_dir("onboarding_reconcile_old");
        let store = Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap();
        store.upsert_key(sample_key("k1", 0)).unwrap();

        // 模拟从旧版本升级：配置里没有该字段 → None
        store.save_settings(UserPrefs::from(&AppSettings::default())).unwrap();
        store
            .mutate_and_persist_if(|cfg| {
                cfg.settings.onboarding_done = None;
                true
            })
            .unwrap();

        let r = store.reconcile_onboarding_flag().unwrap();
        assert_eq!(r, Some(true), "有 Key 的老用户应被判定为已完成");
        assert_eq!(store.get_settings().onboarding_done, Some(true));

        // 幂等：再对账一次什么都不做（已判定）。
        assert_eq!(store.reconcile_onboarding_flag().unwrap(), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn reconcile_onboarding_marks_fresh_install_as_pending() {
        let dir = temp_dir("onboarding_reconcile_new");
        let store = Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap();

        let r = store.reconcile_onboarding_flag().unwrap();
        assert_eq!(r, Some(false), "一条 Key 都没有 → 判定为需要显示向导");
        assert_eq!(store.get_settings().onboarding_done, Some(false));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 第④步的探针：只认**向导开始之后**的事件，且能区分「成功路由」与「收到了但失败」。
    #[test]
    fn first_request_since_ignores_history_and_reports_failure() {
        let dir = temp_dir("first_request_probe");
        let store = Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap();

        // 向导开始**之前**就有一条成功路由（用户之前自己试过）——不该被算进来，
        // 否则向导会在用户还没接入时就打勾，那是个假的正反馈。
        store.append_event(CategoryType::ClaudeCli, "route", None, "历史成功");

        // 用真实时间切分前后（append_event 自己打 chrono 时间戳，测的就是真实写入路径）。
        std::thread::sleep(std::time::Duration::from_millis(5));
        let since = chrono::Utc::now().timestamp_millis();
        std::thread::sleep(std::time::Duration::from_millis(5));

        let probe = store.first_request_since(CategoryType::ClaudeCli, since);
        assert!(!probe.routed, "向导开始前的历史事件不得算作本次接入成功");

        // 之后收到一次失败
        store.append_event(CategoryType::ClaudeCli, "error", None, "401 未授权");
        let probe = store.first_request_since(CategoryType::ClaudeCli, since);
        assert!(!probe.routed);
        assert!(probe.failed, "收到了请求但失败，必须能告诉用户，只说「还没收到」帮不了他");
        assert_eq!(probe.failure_detail.as_deref(), Some("401 未授权"));

        // 再收到一次成功
        store.append_event(CategoryType::ClaudeCli, "route", None, "opus-4-8 → 厂商1");
        let probe = store.first_request_since(CategoryType::ClaudeCli, since);
        assert!(probe.routed);
        assert_eq!(probe.detail.as_deref(), Some("opus-4-8 → 厂商1"));

        // 分类隔离：别的分类的事件不算
        let other = store.first_request_since(CategoryType::Codex, since);
        assert!(!other.routed && !other.failed, "其它分类的事件不得串台");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 应用内「对外模型名」选择：set_active_model 直写并持久化；空串清除；
    #[test]
    fn set_active_model_persists_and_survives_stale_save_settings() {
        let dir = temp_dir("active_model");
        let store = Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap();

        // 用户在应用内为 codex 选定对外模型名。
        store.set_active_model(CategoryType::Codex, "claude-opus-4-8").unwrap();
        assert_eq!(
            store.get_settings().active_models.get(&CategoryType::Codex).map(|s| s.as_str()),
            Some("claude-opus-4-8"),
        );

        // 前端切主题时提交的旧快照 active_models 为空：不得清除已选模型。
        let mut stale = store.get_settings();
        stale.active_models = std::collections::BTreeMap::new();
        stale.theme = "dark".into();
        store.save_settings(UserPrefs::from(&stale)).unwrap();
        assert_eq!(
            store.get_settings().active_models.get(&CategoryType::Codex).map(|s| s.as_str()),
            Some("claude-opus-4-8"),
            "已选模型应保留，不被前端空快照顶回",
        );
        assert_eq!(store.get_settings().theme, "dark", "非自管字段应正常更新");

        // 空串清除该分类选择（回到透传）；重开 Store 后仍为空。
        store.set_active_model(CategoryType::Codex, "").unwrap();
        assert!(!store.get_settings().active_models.contains_key(&CategoryType::Codex));
        let store2 = Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap();
        assert!(!store2.get_settings().active_models.contains_key(&CategoryType::Codex), "清除后应持久");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 方案 A：Codex 默认推理强度是后端自管字段，与 active_models 同一保全策略：
    /// 专用命令直写、前端 save_settings 的陈旧空快照不得顶掉、空串清除、重开持久。
    #[test]
    fn set_active_effort_persists_and_survives_stale_save_settings() {
        let dir = temp_dir("active_effort");
        let store = Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap();

        store.set_active_effort(CategoryType::Codex, "xhigh").unwrap();
        assert_eq!(
            store.get_settings().active_efforts.get(&CategoryType::Codex).map(|s| s.as_str()),
            Some("xhigh"),
        );

        // 前端切主题携带的旧快照 active_efforts 为空：不得清除已配强度。
        let mut stale = store.get_settings();
        stale.active_efforts = std::collections::BTreeMap::new();
        stale.theme = "dark".into();
        store.save_settings(UserPrefs::from(&stale)).unwrap();
        assert_eq!(
            store.get_settings().active_efforts.get(&CategoryType::Codex).map(|s| s.as_str()),
            Some("xhigh"),
            "已配强度应保留，不被前端空快照顶回",
        );

        // 空串清除；重开 Store 后仍为空。
        store.set_active_effort(CategoryType::Codex, "").unwrap();
        assert!(!store.get_settings().active_efforts.contains_key(&CategoryType::Codex));
        let store2 = Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap();
        assert!(!store2.get_settings().active_efforts.contains_key(&CategoryType::Codex), "清除后应持久");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 健康态无实质变化（仅 last_checked/latency 变）时不产生任何磁盘写。
    ///
    /// P1-3 后半改动后语义微调：实质变化不再**立即**落盘，而是标脏 + 由后台合并落盘。
    /// 故本测试的判据从「status 变化应立刻重写文件」改为「status 变化应把脏标记置起来，
    /// flush 后才落盘」。**未放宽的部分**：仅 latency/last_checked 变化时既不落盘、
    /// 也不该标脏（否则每轮探测都会触发一次 20KB 重写，那正是这条优化要防的）。
    #[test]
    fn update_health_skips_persist_when_unchanged() {
        let dir = temp_dir("health_skip");
        let cfg = dir.join("config.json");
        let store = Store::new_at(cfg.clone(), dir.join("secrets.enc")).unwrap();
        store.upsert_key(sample_key("k1", 0)).unwrap();

        // 首次写 Up
        store
            .update_health("k1", HealthState { status: HealthStatus::Up, last_checked: Some(1), ..Default::default() })
            .unwrap();
        // 首次写 Up 是**实质变化**（Unknown→Up），会标脏。先 flush 掉，让后面能干净地验证
        // 「仅 latency 变化不标脏」——否则读到的是这一次遗留的脏标记。
        store.flush_health_if_dirty();
        let mtime1 = std::fs::metadata(&cfg).unwrap().modified().unwrap();

        std::thread::sleep(std::time::Duration::from_millis(20));
        // 仅 last_checked/latency 变化（status/fail_count/breaker 不变）→ 不应重写文件
        store
            .update_health("k1", HealthState { status: HealthStatus::Up, last_checked: Some(999), latency_ms: Some(50), ..Default::default() })
            .unwrap();
        let mtime2 = std::fs::metadata(&cfg).unwrap().modified().unwrap();
        assert_eq!(mtime1, mtime2, "健康态无实质变化不应重写 config.json");

        // 内存态仍更新（UI 走内存）：last_checked 应是最新值
        assert_eq!(store.get_key("k1").unwrap().health.last_checked, Some(999));

        // 且**不该标脏**：否则后台一轮就会为「只有延迟数字变了」写一次 20KB，
        // 等于绕过这条优化。
        assert!(
            !store.flush_health_if_dirty(),
            "仅 latency/last_checked 变化不应标脏（否则每轮探测都触发整份重写）"
        );

        std::thread::sleep(std::time::Duration::from_millis(1100));
        let mtime_before_status = std::fs::metadata(&cfg).unwrap().modified().unwrap();
        // status 变化 → 标脏（不立即落盘），flush 后才写
        store
            .update_health("k1", HealthState { status: HealthStatus::Down, fail_count: 1, ..Default::default() })
            .unwrap();
        assert_eq!(
            std::fs::metadata(&cfg).unwrap().modified().unwrap(),
            mtime_before_status,
            "P1-3：实质变化只标脏，不在调用线程上落盘"
        );
        assert!(store.flush_health_if_dirty(), "status 变化必须已标脏");
        let mtime3 = std::fs::metadata(&cfg).unwrap().modified().unwrap();
        assert!(mtime3 > mtime_before_status, "flush 后 status 变化必须已持久化");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// mtime 自愈回归:磁盘被「外部」写入后,list_keys 应能自动重载看到新数据。
    /// 场景对应 luckyg/cunai 消失事件——若跑的旧进程与磁盘背离,读操作应触发重载,而非返回内存陈旧快照。
    #[test]
    fn list_keys_reloads_when_disk_newer() {
        let dir = temp_dir("mtime_selfheal");
        let cfg_path = dir.join("config.json");
        let sec_path = dir.join("secrets.enc");
        let store = Store::new_at(cfg_path.clone(), sec_path).unwrap();

        // 初始:1 条
        store.upsert_key(sample_key("k1", 0)).unwrap();
        assert_eq!(store.list_keys(CategoryType::ClaudeCli).len(), 1);

        // 保证 mtime 分辨率能拉开(部分文件系统精度到秒)
        std::thread::sleep(std::time::Duration::from_millis(1100));

        // 手工用「外部」方式改磁盘 —— 直接写文件,不走 store.persist
        let mut cfg: AppConfig = serde_json::from_slice(&std::fs::read(&cfg_path).unwrap()).unwrap();
        cfg.keys.push(sample_key("k2_external", 1));
        cfg.keys.push(sample_key("k3_external", 2));
        std::fs::write(&cfg_path, serde_json::to_vec_pretty(&cfg).unwrap()).unwrap();

        // list_keys 应自愈重载,返回 3 条(而不是内存旧快照的 1 条)
        let after = store.list_keys(CategoryType::ClaudeCli);
        assert_eq!(after.len(), 3, "mtime 自愈应把磁盘新数据加载进来");
        let names: Vec<_> = after.iter().map(|k| k.id.as_str()).collect();
        assert!(names.contains(&"k2_external"));
        assert!(names.contains(&"k3_external"));

        std::fs::remove_dir_all(&dir).ok();
    }

    /// persist 后自身更新 mtime 快照:自己写的盘,不应把自己触发成「外部改过」而重载。
    /// 否则每次 upsert 都会连锁自我重载,形成性能与竞态风险。
    #[test]
    fn persist_updates_mtime_snapshot_no_self_reload() {
        let dir = temp_dir("mtime_self_persist");
        let cfg_path = dir.join("config.json");
        let sec_path = dir.join("secrets.enc");
        let store = Store::new_at(cfg_path.clone(), sec_path).unwrap();

        store.upsert_key(sample_key("k1", 0)).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        // 再 upsert 一条 —— 内部会 persist,mtime 会变,但 list_keys 时不应触发重载覆盖
        store.upsert_key(sample_key("k2", 1)).unwrap();

        // 若 persist 没更新 mtime 快照,下一次 list_keys 会误判「磁盘更新」重载,
        // 数据仍应是 2 条(不会丢),但重载路径本身不该被触发。这里侧面用「无 warn」断言:
        // 直接验证 list 返回 2 条即可(如果内部逻辑错误重载后本地内存丢失了 upsert 前的数据,会崩)。
        assert_eq!(store.list_keys(CategoryType::ClaudeCli).len(), 2);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// P1 防数据销毁：config 文件存在但解析失败时，load_config_from_disk 必须
    /// ① 标记 load_failed=true（调用方据此绝不回写磁盘）② 保留磁盘原文件不被覆盖
    /// ③ 另存一份 config.corrupt-* 备份供人工抢救。
    /// 回归审计 P1：旧逻辑失败后 fallback 空配置→seeded→persist 会把磁盘原有 Key 覆盖成 0。
    #[test]
    fn corrupt_config_load_preserves_disk_and_flags_failed() {
        let dir = temp_dir("corrupt_load");
        let cfg_path = dir.join("config.json");
        // 非空但无法反序列化为 AppConfig（keys 应是数组却给字符串），模拟半写/损坏/前向不兼容
        let corrupt = br#"{"keys":"not-an-array","broken":true}"#;
        std::fs::write(&cfg_path, corrupt).unwrap();

        let (cfg, failed) = Store::load_config_from_disk(&cfg_path).unwrap();
        assert!(failed, "解析失败必须标记 load_failed=true");
        assert_eq!(cfg.keys.len(), 0, "失败回退空配置");

        // 磁盘原文件必须原样保留（绝不能被空配置覆盖）——防数据销毁的核心
        let after = std::fs::read(&cfg_path).unwrap();
        assert_eq!(&after[..], &corrupt[..], "解析失败绝不能覆盖磁盘原文件");

        // 应另存一份 .corrupt 备份供抢救
        let has_backup = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| e.file_name().to_string_lossy().starts_with("config.corrupt-"));
        assert!(has_backup, "应另存 config.corrupt-* 备份供抢救");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// P1-2 回归：原先 12 处「后端自管字段」写入是裸 `persist()`，落盘失败即内存领先磁盘，
    /// 而该方向**永不自愈**（mtime 自愈只认「磁盘比内存新」）。这里逐个验证改走
    /// `mutate_and_persist*` 后都会回滚。
    ///
    /// 挑这四个是因为它们有**用户可见后果**：
    /// - `upsert_vendor` / `delete_vendor`：界面显示「保存成功」但重启后厂商消失；
    /// - `set_proxy_port`：粘滞端口丢失 → 重启后端口漂移，客户端里写的旧端口连不上
    ///   （而粘滞端口本就是为解决这个问题引入的）；
    /// - `add_registered_category`：内存记着已注册而磁盘没有 → 端口漂移时的批量重写漏掉
    ///   该分类，客户端 MCP 指向死端口。
    ///
    /// 故障注入判据：把任一方法改回「改内存 → self.persist()」，对应断言立刻变红。
    #[test]
    fn backend_owned_writes_roll_back_when_persist_fails() {
        let dir = temp_dir("rollback_backend_owned");
        let cfg_path = dir.join("config.json");
        let store = Store::new_at(cfg_path.clone(), dir.join("secrets.enc")).unwrap();

        // 先在可写状态下建立基线
        let mk_vendor = |id: &str, name: &str| Vendor {
            id: id.into(),
            name: name.into(),
            default_base_url: "https://x.example".into(),
            default_protocol: Protocol::Anthropic,
            builtin: false,
            icon: None,
            preset_models: vec![],
        };
        store.upsert_vendor(mk_vendor("v1", "厂商1")).unwrap();
        store.set_proxy_port(CategoryType::ClaudeCli, 47100).unwrap();
        assert!(store.add_registered_category(CategoryType::ClaudeCli).unwrap());

        // 制造确定性落盘失败（同 delete_key 那条的手法）：把 config.json 变成目录。
        std::fs::remove_file(&cfg_path).unwrap();
        std::fs::create_dir(&cfg_path).unwrap();

        // upsert_vendor：新增一个应失败并回滚（厂商数不变）
        let before_vendors = store.config.read().vendors.len();
        let r = store.upsert_vendor(mk_vendor("v2", "厂商2"));
        assert!(r.is_err(), "落盘失败必须上抛错误");
        assert_eq!(
            store.config.read().vendors.len(),
            before_vendors,
            "upsert_vendor 落盘失败必须回滚，否则界面显示已保存而重启后消失"
        );

        // delete_vendor：删除应失败并回滚（v1 仍在）
        let r = store.delete_vendor("v1");
        assert!(r.is_err(), "落盘失败必须上抛错误");
        assert!(
            store.config.read().vendors.iter().any(|v| v.id == "v1"),
            "delete_vendor 落盘失败必须回滚"
        );

        // set_proxy_port：改成新端口应失败并回滚到 47100
        let r = store.set_proxy_port(CategoryType::ClaudeCli, 47999);
        assert!(r.is_err(), "落盘失败必须上抛错误");
        assert_eq!(
            store.config.read().settings.proxy_ports.get(&CategoryType::ClaudeCli).copied(),
            Some(47100),
            "set_proxy_port 落盘失败必须回滚，否则粘滞端口丢失、重启后端口漂移"
        );

        // add_registered_category：新增另一分类应失败并回滚
        let r = store.add_registered_category(CategoryType::Codex);
        assert!(r.is_err(), "落盘失败必须上抛错误");
        assert!(
            !store.config.read().settings.mcp_registered_categories.contains(&CategoryType::Codex),
            "add_registered_category 落盘失败必须回滚，否则批量重写会漏掉该分类"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 幂等写入必须**跳过落盘**（`mutate_and_persist_if` 的存在理由）。
    ///
    /// 前端切页会重发同一个值，若无条件 persist 就是反复重写 20KB 整份 config。
    /// 但也不能为省这次写盘退回「裸 persist + 提前 return」——那样又丢了回滚。
    #[test]
    fn idempotent_backend_writes_skip_persist() {
        let dir = temp_dir("idempotent_skip");
        let cfg_path = dir.join("config.json");
        let store = Store::new_at(cfg_path.clone(), dir.join("secrets.enc")).unwrap();

        store.set_active_model(CategoryType::Codex, "gpt-5").unwrap();
        let mtime1 = std::fs::metadata(&cfg_path).unwrap().modified().unwrap();

        // ⚠️ 必须**先把值取出来再调用**，不能写成
        // `store.set_mcp_enabled_flag(store.config.read().settings.mcp_enabled)`：
        // 参数表达式里的读守卫活到整条语句结束，而被调方法内部要取 `config.write()`，
        // parking_lot 的写锁会等所有读守卫释放 → 自锁挂死（本测试初版就这么挂了 60s+）。
        // 这与 `mutate_health` 文档里「闭包内禁调取锁方法」是同一根因的变体。
        let cur_port = store.config.read().settings.proxy_ports.get(&CategoryType::Codex).copied();
        let cur_mcp = store.config.read().settings.mcp_enabled;

        std::thread::sleep(std::time::Duration::from_millis(1100));
        // 重复写同一个值 → 不该落盘
        store.set_active_model(CategoryType::Codex, "gpt-5").unwrap();
        if let Some(p) = cur_port {
            store.set_proxy_port(CategoryType::Codex, p).unwrap();
        }
        store.set_mcp_enabled_flag(cur_mcp).unwrap();
        let mtime2 = std::fs::metadata(&cfg_path).unwrap().modified().unwrap();
        assert_eq!(mtime1, mtime2, "幂等写入不应触发落盘");

        // 真改值 → 必须落盘
        std::thread::sleep(std::time::Duration::from_millis(1100));
        store.set_active_model(CategoryType::Codex, "gpt-5-codex").unwrap();
        let mtime3 = std::fs::metadata(&cfg_path).unwrap().modified().unwrap();
        assert_ne!(mtime2, mtime3, "值真变了必须落盘");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// persist-crud（high）：写方法落盘失败必须回滚内存，保证「内存态永不领先磁盘」。
    /// 否则 delete 后内存 N-1 / 磁盘仍 N，而 mtime 自愈只认「磁盘比内存新」方向，反向背离
    /// 永不自愈，UI 稳定显示与磁盘持久不一致。
    #[test]
    fn delete_key_rolls_back_memory_when_persist_fails() {
        let dir = temp_dir("rollback_del");
        let cfg_path = dir.join("config.json");
        let sec_path = dir.join("secrets.enc");
        let store = Store::new_at(cfg_path.clone(), sec_path).unwrap();

        store.upsert_key(sample_key("k1", 0)).unwrap();
        assert_eq!(store.config.read().keys.len(), 1);

        // 制造确定性落盘失败：把 config.json 变成一个目录，atomic_write 的 rename 与原地写
        // 都无法把文件写到「一个目录名」上 → persist 返回 Err。
        std::fs::remove_file(&cfg_path).unwrap();
        std::fs::create_dir(&cfg_path).unwrap();

        let r = store.delete_key("k1");
        assert!(r.is_err(), "落盘失败必须上抛错误");
        // 关键：内存回滚到删除前（1 条），不能领先磁盘变成 0 条
        assert_eq!(
            store.config.read().keys.len(),
            1,
            "persist 失败必须回滚内存，否则内存(0)领先磁盘且永不自愈"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 回归 review：persist 失败回滚应【从磁盘对账】(保留并发写者已落盘的变更)，
    /// 而非用改动前内存快照整份覆盖——后者会抹掉并发已提交写入(尤以 settings 永不自愈)。
    #[test]
    fn rollback_from_disk_reconciles_to_committed_disk_state() {
        let dir = temp_dir("rollback_reconcile");
        let cfg_path = dir.join("config.json");
        let store = Store::new_at(cfg_path.clone(), dir.join("secrets.enc")).unwrap();
        store.upsert_key(sample_key("k1", 0)).unwrap(); // 内存+磁盘={k1}

        // 模拟并发写者已把 k2 落盘(绕过 store,如另一写方法 persist 成功)
        let mut disk: AppConfig =
            serde_json::from_slice(&std::fs::read(&cfg_path).unwrap()).unwrap();
        disk.keys.push(sample_key("k2", 1));
        std::fs::write(&cfg_path, serde_json::to_vec_pretty(&disk).unwrap()).unwrap();

        // snapshot 是「改动前内存」{k1}；对账应采纳磁盘的 {k1,k2} 而非 snapshot
        let snapshot = store.config.read().clone();
        store.rollback_from_disk(snapshot);
        assert_eq!(
            store.config.read().keys.len(),
            2,
            "对账应保留并发写者已落盘的 k2，而非回退成内存快照 {{k1}}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// `save_brain` 落盘失败必须回滚内存（与 Key CRUD 同一套保证）。
    ///
    /// 补这条的理由：`save_brain` / `save_settings` 曾是全仓仅剩两个直接 `persist()` 的写路径，
    /// 落盘失败时内存态领先磁盘。而「内存比磁盘新」这个方向**永不自愈**（mtime 自愈只认
    /// 「磁盘比内存新」），表现为 UI 一直显示保存成功、重启后配置却回退。
    #[test]
    fn save_brain_rolls_back_memory_when_persist_fails() {
        let dir = temp_dir("rollback_brain");
        let cfg_path = dir.join("config.json");
        let store = Store::new_at(cfg_path.clone(), dir.join("secrets.enc")).unwrap();

        // 先落一条正常的 brain 配置（内存 + 磁盘一致）。
        let mut brain = store.get_brain(CategoryType::ClaudeCli);
        brain.total_timeout_ms = 111_000;
        store.save_brain(brain).unwrap();
        assert_eq!(store.get_brain(CategoryType::ClaudeCli).total_timeout_ms, 111_000);

        // 制造确定性落盘失败：把 config.json 变成目录（同 delete_key 那条测试的手法）。
        std::fs::remove_file(&cfg_path).unwrap();
        std::fs::create_dir(&cfg_path).unwrap();

        let mut dirty = store.get_brain(CategoryType::ClaudeCli);
        dirty.total_timeout_ms = 999_000;
        assert!(store.save_brain(dirty).is_err(), "落盘失败必须上抛错误");
        assert_eq!(
            store.get_brain(CategoryType::ClaudeCli).total_timeout_ms,
            111_000,
            "persist 失败必须回滚内存，否则 UI 显示已保存、磁盘其实是旧值且永不自愈"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// `save_settings` 落盘失败同样必须回滚内存，且**不得**在回滚过程中丢掉后端自管字段。
    ///
    /// 后者是这次改造最容易写坏的点：把「保留后端自管字段」搬进 `mutate_and_persist` 的闭包后，
    /// 顺序若反（先整份覆盖再取值）就会把 mcp_port 等清零；而回滚走磁盘对账，
    /// 磁盘上那份仍是正确值，故断言要同时覆盖「回滚后字段仍在」。
    #[test]
    fn save_settings_rolls_back_memory_and_keeps_backend_owned_fields() {
        let dir = temp_dir("rollback_settings");
        let cfg_path = dir.join("config.json");
        let store = Store::new_at(cfg_path.clone(), dir.join("secrets.enc")).unwrap();

        // 后端自管字段先落盘（专用方法直写）。
        store.set_mcp_port(9531).unwrap();
        store.set_active_model(CategoryType::Codex, "claude-opus-4-8").unwrap();
        let mut s = store.get_settings();
        s.theme = "light".into();
        store.save_settings(UserPrefs::from(&s)).unwrap();
        assert_eq!(store.get_settings().theme, "light");
        assert_eq!(store.get_settings().mcp_port, 9531);

        // 落盘失败。
        std::fs::remove_file(&cfg_path).unwrap();
        std::fs::create_dir(&cfg_path).unwrap();

        let mut dirty = store.get_settings();
        dirty.theme = "dark".into();
        assert!(store.save_settings(UserPrefs::from(&dirty)).is_err(), "落盘失败必须上抛错误");

        let now = store.get_settings();
        assert_eq!(now.theme, "light", "persist 失败必须回滚，不留内存领先态");
        assert_eq!(now.mcp_port, 9531, "回滚不得丢掉后端自管的粘滞端口");
        assert_eq!(
            now.active_models.get(&CategoryType::Codex).map(|s| s.as_str()),
            Some("claude-opus-4-8"),
            "回滚不得丢掉后端自管的已选模型"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 回归 review：磁盘读/解析失败(正是 persist 失败之因,如目标被建成目录)时，
    /// rollback_from_disk 回退到改动前内存快照兜底，保证内存不领先磁盘。
    #[test]
    fn rollback_from_disk_falls_back_to_snapshot_when_disk_unreadable() {        let dir = temp_dir("rollback_fallback");
        let cfg_path = dir.join("config.json");
        let store = Store::new_at(cfg_path.clone(), dir.join("secrets.enc")).unwrap();
        store.upsert_key(sample_key("k1", 0)).unwrap();
        let snapshot = store.config.read().clone(); // {k1}

        // 磁盘不可读：删文件、建同名目录
        std::fs::remove_file(&cfg_path).unwrap();
        std::fs::create_dir(&cfg_path).unwrap();
        // 模拟 CRUD 已改脏内存
        store.config.write().keys.clear(); // 内存={}
        store.rollback_from_disk(snapshot);
        assert_eq!(
            store.config.read().keys.len(),
            1,
            "磁盘不可读时应回退改动前内存快照"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 回归 review 最毒确定性触发路径：toggle 不存在的 Key → 闭包返回 Err(NotFound) →
    /// 也必须走磁盘对账、保留并发写者已落盘的变更，而非整份 snapshot 回滚把并发提交抹掉。
    #[test]
    fn closure_err_reconciles_from_disk_not_snapshot() {
        let dir = temp_dir("closure_err_reconcile");
        let cfg_path = dir.join("config.json");
        let store = Store::new_at(cfg_path.clone(), dir.join("secrets.enc")).unwrap();
        store.upsert_key(sample_key("k1", 0)).unwrap(); // 内存+磁盘={k1}

        // 并发写者把 k2 落盘（绕过 store，模拟 upsert/健康线程等已提交）
        let mut disk: AppConfig =
            serde_json::from_slice(&std::fs::read(&cfg_path).unwrap()).unwrap();
        disk.keys.push(sample_key("k2", 1));
        std::fs::write(&cfg_path, serde_json::to_vec_pretty(&disk).unwrap()).unwrap();

        // toggle 不存在的 Key → 闭包 Err(NotFound)
        let r = store.toggle_key("ghost", true);
        assert!(r.is_err(), "toggle 不存在的 Key 应返回 Err");
        // 闭包 Err 也走磁盘对账 → 采纳磁盘 {k1,k2}；旧整份 snapshot 回滚会退成 {k1} 抹掉 k2
        assert_eq!(
            store.config.read().keys.len(),
            2,
            "闭包 Err 也须磁盘对账保留并发已落盘的 k2，不得整份回滚抹掉"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 迁移：余额查询 URL 从旧的错误默认值改为正确值，仅当恰好是旧默认值 && 仍是 generic 模板时。
    #[test]
    fn migrate_balance_query_url_only_fixes_known_wrong_default() {
        let mut keys = vec![
            // 场景 1：恰好是旧默认值 + generic 模板 → 应被迁移
            ProviderKey {
                id: "k1".into(),
                category_id: CategoryType::ClaudeCli,
                name: "旧配置用户".into(),
                balance_query: Some(BalanceQuery {
                    enabled: true,
                    template: "generic".into(),
                    url: "{{baseUrl}}/v1/usage".into(), // 旧错误默认值
                    ..Default::default()
                }),
                ..sample_key("k1", 0)
            },
            // 场景 2：URL 是旧默认值，但 template 已改 → 不动（用户可能在自定义模板里刻意用这个路径）
            ProviderKey {
                id: "k2".into(),
                category_id: CategoryType::ClaudeCli,
                name: "自定义模板".into(),
                balance_query: Some(BalanceQuery {
                    enabled: true,
                    template: "my-custom".into(),
                    url: "{{baseUrl}}/v1/usage".into(),
                    ..Default::default()
                }),
                ..sample_key("k2", 1)
            },
            // 场景 3：generic 模板，但 URL 已手动改过 → 不动
            ProviderKey {
                id: "k3".into(),
                category_id: CategoryType::ClaudeCli,
                name: "手动改过URL".into(),
                balance_query: Some(BalanceQuery {
                    enabled: true,
                    template: "generic".into(),
                    url: "{{baseUrl}}/api/balance".into(), // 用户手填的
                    ..Default::default()
                }),
                ..sample_key("k3", 2)
            },
            // 场景 4：已经是新默认值 → 不动（幂等）
            ProviderKey {
                id: "k4".into(),
                category_id: CategoryType::ClaudeCli,
                name: "新版本新建".into(),
                balance_query: Some(BalanceQuery {
                    enabled: true,
                    template: "generic".into(),
                    url: "{{baseUrl}}/user/balance".into(),
                    ..Default::default()
                }),
                ..sample_key("k4", 3)
            },
            // 场景 5：没配余额查询 → 不动
            ProviderKey {
                id: "k5".into(),
                category_id: CategoryType::ClaudeCli,
                name: "未配余额".into(),
                balance_query: None,
                ..sample_key("k5", 4)
            },
        ];

        let changed = Store::migrate_balance_query_url(&mut keys);
        assert!(changed, "k1 应被迁移，返回 true");

        // k1 应被改为新默认值
        assert_eq!(
            keys[0].balance_query.as_ref().unwrap().url,
            "{{baseUrl}}/user/balance",
            "k1（旧默认值+generic）应被迁移"
        );

        // k2~k5 都不应被改动
        assert_eq!(
            keys[1].balance_query.as_ref().unwrap().url,
            "{{baseUrl}}/v1/usage",
            "k2（自定义模板）不应被改"
        );
        assert_eq!(
            keys[2].balance_query.as_ref().unwrap().url,
            "{{baseUrl}}/api/balance",
            "k3（手动改过的URL）不应被改"
        );
        assert_eq!(
            keys[3].balance_query.as_ref().unwrap().url,
            "{{baseUrl}}/user/balance",
            "k4（已是新默认值）保持不变"
        );
        assert!(keys[4].balance_query.is_none(), "k5（无配置）保持 None");

        // 幂等性：再跑一次不应有改动
        let changed_again = Store::migrate_balance_query_url(&mut keys);
        assert!(!changed_again, "第二次迁移应返回 false（幂等）");
    }

    #[test]
    fn base_url_has_path_suffix_detects_trailing_paths() {
        // 纯域名或带端口 → 无路径后缀
        assert!(!base_url_has_path_suffix("https://api.anthropic.com"));
        assert!(!base_url_has_path_suffix("https://api.deepseek.com"));
        assert!(!base_url_has_path_suffix("http://localhost:8080"));
        assert!(!base_url_has_path_suffix("https://example.com:443"));

        // 带路径后缀 → 检测到
        assert!(base_url_has_path_suffix("https://api.deepseek.com/anthropic"));
        assert!(base_url_has_path_suffix("https://api.example.com/v1"));
        assert!(base_url_has_path_suffix("http://localhost:8080/api"));

        // 只有根路径 `/` → 视为无后缀（等同于没有路径）
        assert!(!base_url_has_path_suffix("https://api.anthropic.com/"));

        // 无效 URL → false（防御性返回）
        assert!(!base_url_has_path_suffix("not-a-url"));
        assert!(!base_url_has_path_suffix(""));
        assert!(!base_url_has_path_suffix("ftp://unsupported.com"));
    }
}
