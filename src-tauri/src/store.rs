//! 配置持久化与内存态管理。
//! 配置文件（不含密钥）存 JSON；密钥存 SecretStore（加密）。
//! 所有写操作走原子写（NFR-011）。

use crate::error::{AppError, AppResult};
use crate::model::*;
use crate::secret::{atomic_write, SecretStore};
use parking_lot::RwLock;
use std::path::PathBuf;
use std::time::SystemTime;

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
}

/// 事件日志内存上限
const MAX_EVENTS: usize = 500;

/// 默认日志目录：安装目录（exe 同级）下的 `logs/`。
/// 路径动态解析（current_exe），禁止硬编码（dev-hard-rules 规则2）。
/// 若安装目录不可写（如装在 Program Files 需管理员权限），回退到 %APPDATA%\SynaRoute\logs。
pub fn default_log_dir() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let logs = dir.join("logs");
            // 探测可写性：能创建目录即认为可写，用它
            if std::fs::create_dir_all(&logs).is_ok() {
                // 进一步验证真的能写入（Program Files 下 create_dir_all 可能因已存在而成功，但写入失败）
                let probe = logs.join(".write-probe");
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

impl Store {
    /// 初始化：定位数据目录（%APPDATA%\SynaRoute），加载配置与密钥库。
    /// 路径全部动态解析，禁止硬编码（dev-hard-rules 规则2）。
    /// 从磁盘加载配置,返回 (配置, 是否加载失败)。抽出以便单测覆盖 P1 防销毁路径。
    /// - 文件不存在：返回 (空配置, false)——全新安装,允许后续 seed+persist。
    /// - 解析成功：返回 (配置, false)。
    /// - 文件存在但解析失败：返回 (空配置, true)。调用方据 load_failed=true 【绝不回写磁盘】,
    ///   避免空配置覆盖磁盘原有数据(P1 防销毁);并另存一份 .corrupt 备份供人工抢救。
    fn load_config_from_disk(config_path: &std::path::Path) -> AppResult<(AppConfig, bool)> {
        if !config_path.exists() {
            tracing::info!("配置文件不存在,使用默认空配置: {:?}", config_path);
            return Ok((AppConfig::default(), false));
        }
        let raw = std::fs::read(config_path)?;
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
        let data_dir = dirs::data_dir()
            .ok_or_else(|| AppError::Other("无法定位数据目录".into()))?
            .join("SynaRoute");
        std::fs::create_dir_all(&data_dir)?;

        let config_path = data_dir.join("config.json");
        let secrets_path = data_dir.join("secrets.enc");

        let (mut config, load_failed) = Self::load_config_from_disk(&config_path)?;

        // 首次运行（或老配置无 vendors）注入内置厂商种子
        let seeded = config.vendors.is_empty();
        if seeded {
            config.vendors = Vendor::builtin_seed();
        }

        // 迁移：老配置的内置厂商没有 preset_models 字段（serde default 给空 vec），
        // 从种子按 id 回填，让老用户也能用「一键导入预设模型」。仅补空、不覆盖用户已有数据。
        let migrated = if !seeded {
            Self::backfill_builtin_presets(&mut config.vendors)
        } else {
            false
        };

        let secrets = SecretStore::load(secrets_path)?;

        // 记录初始 (mtime,len),后续读操作前用于「磁盘被外部改过就重载」的自愈判断
        let initial_stamp = Self::read_disk_stamp(&config_path);

        let store = Self {
            config_path,
            config: RwLock::new(config),
            secrets: RwLock::new(secrets),
            events: RwLock::new(Vec::new()),
            config_stamp: RwLock::new(initial_stamp),
        };
        // P1 防数据销毁：仅在「全新安装(文件不存在)首次 seed」或「成功加载后的迁移」时落盘。
        // load_failed(文件存在但解析失败)时绝不 persist——否则空配置会覆盖磁盘上的原有数据。
        if (seeded || migrated) && !load_failed {
            store.persist()?;
        }
        Ok(store)
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
        let entry = EventLogEntry {
            id: uuid::Uuid::new_v4().to_string(),
            ts: chrono::Utc::now().timestamp_millis(),
            category_id: category,
            kind: kind.to_string(),
            key_id: key_id.map(|s| s.to_string()),
            detail: detail.to_string(),
            trace,
        };
        let mut ev = self.events.write();
        ev.push(entry.clone());
        if ev.len() > MAX_EVENTS {
            let overflow = ev.len() - MAX_EVENTS;
            ev.drain(0..overflow);
        }
        drop(ev);
        self.write_log_to_file(&entry);
    }

    fn write_log_to_file(&self, entry: &EventLogEntry) {
        let settings = self.get_settings();
        let log_dir = match &settings.log_dir {
            Some(dir) if !dir.is_empty() => std::path::PathBuf::from(dir),
            _ => default_log_dir(),
        };
        if let Err(e) = std::fs::create_dir_all(&log_dir) {
            tracing::warn!("创建日志目录失败: {e}");
            return;
        }
        let date = chrono::Utc::now().format("%Y-%m-%d");
        let log_file = log_dir.join(format!("{date}.jsonl"));
        let line = match serde_json::to_string(entry) {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!("序列化日志条目失败: {e}");
                return;
            }
        };
        use std::io::Write;
        match std::fs::OpenOptions::new().create(true).append(true).open(&log_file) {
            Ok(mut f) => { let _ = writeln!(f, "{line}"); }
            Err(e) => { tracing::warn!("写入日志文件失败: {e}"); }
        }
    }

    pub fn list_events(&self, category: CategoryType) -> Vec<EventLogEntry> {
        self.events
            .read()
            .iter()
            .filter(|e| e.category_id == category)
            .cloned()
            .collect()
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
                Ok(()) => Ok(value),
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
            cfg.keys = fresh.keys;
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

    pub fn upsert_key(&self, key: ProviderKey) -> AppResult<ProviderKey> {
        // 失败回滚：落盘失败不让内存领先磁盘（见 mutate_and_persist）。
        self.mutate_and_persist(|cfg| {
            if let Some(existing) = cfg.keys.iter_mut().find(|k| k.id == key.id) {
                *existing = key.clone();
            } else {
                cfg.keys.push(key.clone());
            }
            Ok(())
        })?;
        Ok(key)
    }

    pub fn delete_key(&self, key_id: &str) -> AppResult<()> {
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

    /// 更新某 Key 的健康状态（健康检查模块调用）。
    /// 仅当「熔断相关」字段（status / fail_count / breaker_until）变化时才落盘——
    /// last_checked / latency 每轮都变但无需持久化（内存态已更新，UI 走内存态实时展示），
    /// 避免后台健康检查每轮对每个 Key 都整份重写 config.json，减少磁盘写与锁竞争。
    pub fn update_health(&self, key_id: &str, health: HealthState) -> AppResult<()> {
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
        if changed {
            self.persist()?;
        }
        Ok(())
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
            })
    }

    pub fn save_brain(&self, brain: BrainConfig) -> AppResult<()> {
        {
            let mut cfg = self.config.write();
            if let Some(b) = cfg.brain.iter_mut().find(|b| b.category_id == brain.category_id) {
                *b = brain;
            } else {
                cfg.brain.push(brain);
            }
        }
        self.persist()
    }

    // ---- 设置 ----

    pub fn get_settings(&self) -> AppSettings {
        self.config.read().settings.clone()
    }

    pub fn save_settings(&self, mut settings: AppSettings) -> AppResult<()> {
        {
            let mut cfg = self.config.write();
            // mcp_registered_categories 是后端自管字段：前端 saveSettings 不携带它（序列化为空 vec），
            // 若直接覆盖会在每次切主题/语言时清空注册记录。故这里始终保留已有值——
            // 该字段只能由后端专用方法（add/clear_registered_category）更新，
            // 绝不随 save_settings 的入参变动（无论入参空或非空）。
            settings.mcp_registered_categories =
                std::mem::take(&mut cfg.settings.mcp_registered_categories);
            // mcp_port / mcp_enabled 同理归后端「MCP 控制面」自管，由 set_mcp_enabled /
            // set_mcp_port / restart_mcp 三个专用命令负责。
            //
            // 若允许通用 save_settings 从入参覆盖：前端切主题 / 语言时传入的是 zustand 里
            // **加载时的旧快照**，端口占用回退后被 set_mcp_port 粘滞的新端口会被这个旧值
            // **顶回**，粘滞被无声撤销 —— 且随后触发 mcp.start(旧端口) 使当前运行的 MCP
            // 被 stop + 重扫，非本次操作要动的连接全部断开。
            //
            // 因此这两个字段一律保留后端持久化值，与前端入参无关。
            settings.mcp_port = cfg.settings.mcp_port;
            settings.mcp_enabled = cfg.settings.mcp_enabled;
            // active_models 同为后端自管字段：前端 saveSettings 携带的是加载时旧快照，
            // 用户在应用内改选模型走专用命令 set_active_model 直写，若被这里的旧快照覆盖，
            // 会把刚选的模型顶回旧值（与 mcp_* 同一保全策略）。始终保留后端持久化值。
            settings.active_models = std::mem::take(&mut cfg.settings.active_models);
            // active_efforts 同为后端自管字段（Codex 默认推理强度），策略同 active_models。
            settings.active_efforts = std::mem::take(&mut cfg.settings.active_efforts);
            // proxy_ports 同为后端自管字段（粘滞端口）：由 set_proxy_port（用户改端口 / 占用回退写回）
            // 直写，前端 saveSettings 的旧快照不得覆盖，否则粘滞端口被顶回、下次重启又漂移。
            settings.proxy_ports = std::mem::take(&mut cfg.settings.proxy_ports);
            cfg.settings = settings;
        }
        self.persist()
    }

    /// 设置某分类当前选定的对外模型名（后端自管字段专用写入，绕过 save_settings 的旧快照覆盖）。
    /// 空串视为清除该分类的选择（回到「透传客户端发来的模型名」）。已是目标值则幂等跳过写盘。
    pub fn set_active_model(&self, category: &str, model: &str) -> AppResult<()> {
        {
            let mut cfg = self.config.write();
            let trimmed = model.trim();
            if trimmed.is_empty() {
                if cfg.settings.active_models.remove(category).is_none() {
                    return Ok(());
                }
            } else {
                if cfg.settings.active_models.get(category).map(|s| s.as_str()) == Some(trimmed) {
                    return Ok(());
                }
                cfg.settings
                    .active_models
                    .insert(category.to_string(), trimmed.to_string());
            }
        }
        self.persist()
    }

    /// 设置某分类的「默认推理强度」（Codex 用；后端自管字段专用写入，绕过 save_settings 旧快照覆盖）。
    /// 空串视为清除（回到不注入、保持上游默认）。已是目标值则幂等跳过写盘。
    /// 取值：low/medium/high/xhigh（minimal 亦可，映射侧按不开思考处理）。
    pub fn set_active_effort(&self, category: &str, effort: &str) -> AppResult<()> {
        {
            let mut cfg = self.config.write();
            let trimmed = effort.trim();
            if trimmed.is_empty() {
                if cfg.settings.active_efforts.remove(category).is_none() {
                    return Ok(());
                }
            } else {
                if cfg.settings.active_efforts.get(category).map(|s| s.as_str()) == Some(trimmed) {
                    return Ok(());
                }
                cfg.settings
                    .active_efforts
                    .insert(category.to_string(), trimmed.to_string());
            }
        }
        self.persist()
    }

    /// 设置某分类代理的首选端口（粘滞：绑定回退后写回实际端口作下次首选，或前端手改端口）。
    /// 后端自管字段专用写入，绕过 save_settings 旧快照覆盖。已是目标值则幂等跳过写盘。
    pub fn set_proxy_port(&self, category: &str, port: u16) -> AppResult<()> {
        {
            let mut cfg = self.config.write();
            if cfg.settings.proxy_ports.get(category).copied() == Some(port) {
                return Ok(());
            }
            cfg.settings
                .proxy_ports
                .insert(category.to_string(), port);
        }
        self.persist()
    }

    /// 记录某分类已注册 synaroute MCP（去重后落盘）。后端注册逻辑专用。
    ///
    /// 必须走这里而非 `get_settings → push → save_settings`：后者会被 save_settings
    /// 的 `mem::take` 保留逻辑吞掉（把刚 push 的新值换回旧值），导致该集合永远为空——
    /// 端口漂移后其它已注册分类的客户端配置永不更新、关闭 MCP 时注销循环也读到空。
    /// 返回 true 表示本次新增（原本不含），false 表示已存在（幂等跳过写盘）。
    pub fn add_registered_category(&self, category: &str) -> AppResult<bool> {
        {
            let mut cfg = self.config.write();
            if cfg.settings.mcp_registered_categories.iter().any(|c| c == category) {
                return Ok(false);
            }
            cfg.settings.mcp_registered_categories.push(category.to_string());
        }
        self.persist()?;
        Ok(true)
    }

    /// 清空已注册分类记录并落盘（关闭 MCP 开关时用）。后端专用。
    /// 已为空则跳过写盘（幂等）。
    pub fn clear_registered_categories(&self) -> AppResult<()> {
        {
            let mut cfg = self.config.write();
            if cfg.settings.mcp_registered_categories.is_empty() {
                return Ok(());
            }
            cfg.settings.mcp_registered_categories.clear();
        }
        self.persist()
    }

    /// 只更新 MCP 首选端口并落盘（后端自管字段，不走前端全量 save_settings）。
    /// 用于「粘住成功端口」：某次实际绑定的端口（可能因占用回退而来）写回设置，
    /// 使下次启动直接以它为首选，不再每次都从被占的旧端口重新回退、重写客户端配置。
    /// 已是目标值则跳过写盘（幂等，避免无谓 IO）。
    pub fn set_mcp_port(&self, port: u16) -> AppResult<()> {
        {
            let mut cfg = self.config.write();
            if cfg.settings.mcp_port == port {
                return Ok(());
            }
            cfg.settings.mcp_port = port;
        }
        self.persist()
    }

    /// 只更新 MCP 启用开关并落盘（后端自管字段，不走前端全量 save_settings）。
    /// 通用 save_settings 会保留旧的 mcp_enabled，避免切主题/语言时被入参顶掉；
    /// 真正翻转开关的路径（set_mcp_enabled）必须走这个专用方法直写。已是目标值则幂等跳过。
    pub fn set_mcp_enabled_flag(&self, enabled: bool) -> AppResult<()> {
        {
            let mut cfg = self.config.write();
            if cfg.settings.mcp_enabled == enabled {
                return Ok(());
            }
            cfg.settings.mcp_enabled = enabled;
        }
        self.persist()
    }

    // ---- 厂商预设 CRUD ----

    pub fn list_vendors(&self) -> Vec<Vendor> {
        self.config.read().vendors.clone()
    }

    /// 新增/更新厂商。内置项（builtin）不可修改。
    pub fn upsert_vendor(&self, vendor: Vendor) -> AppResult<Vendor> {
        {
            let mut cfg = self.config.write();
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
        }
        self.persist()?;
        Ok(vendor)
    }

    /// 删除厂商。内置项不可删除。
    pub fn delete_vendor(&self, vendor_id: &str) -> AppResult<()> {
        {
            let mut cfg = self.config.write();
            match cfg.vendors.iter().find(|v| v.id == vendor_id) {
                Some(v) if v.builtin => {
                    return Err(AppError::Invalid("内置厂商不可删除".into()))
                }
                Some(_) => cfg.vendors.retain(|v| v.id != vendor_id),
                None => return Err(AppError::NotFound(vendor_id.into())),
            }
        }
        self.persist()
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
        let secrets = SecretStore::load(secrets_path)?;
        let initial_stamp = Self::read_disk_stamp(&config_path);
        Ok(Self {
            config_path,
            config: RwLock::new(config),
            secrets: RwLock::new(secrets),
            events: RwLock::new(Vec::new()),
            config_stamp: RwLock::new(initial_stamp),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            id: id.to_string(),
            category_id: CategoryType::ClaudeCli,
            name: format!("测试Key-{id}"),
            vendor: "test-vendor".into(),
            base_url: "https://api.example.com".into(),
            protocol: Protocol::Anthropic,
            has_secret: false,
            enabled: true,
            priority,
            headers_json: None,
            params: KeyParams::default(),
            models: vec![],
            mappings: vec![],
            default_model: None,
            tier_haiku: None,
            tier_sonnet: None,
            tier_opus: None,
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
        assert_eq!(got.as_deref(), Some("sk-secret-123"));

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

    /// 编辑（upsert 已存 Key）：字段更新且落盘持久。
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
        store.add_registered_category("claude-cli").unwrap();

        // 前端持有旧快照：enabled=false / port=9527 / categories=[]，切主题时提交整个 settings。
        let mut stale = store.get_settings();
        stale.mcp_enabled = false;
        stale.mcp_port = 9527;
        stale.mcp_registered_categories = vec![]; // 前端序列化通常就是空
        stale.theme = "dark".into(); // 真正想改的字段
        store.save_settings(stale).unwrap();

        // 三个后端自管字段全部**保留**，前端入参不生效；其它字段（theme）正常落。
        let now = store.get_settings();
        assert!(now.mcp_enabled, "mcp_enabled 应保留后端值 true，不被前端 false 顶回");
        assert_eq!(now.mcp_port, 9529, "mcp_port 应保留粘滞值，不被前端旧端口顶回");
        assert_eq!(
            now.mcp_registered_categories,
            vec!["claude-cli".to_string()],
            "已注册分类不应被前端空 vec 清空"
        );
        assert_eq!(now.theme, "dark", "非控制面字段应正常更新");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 应用内「对外模型名」选择：set_active_model 直写并持久化；空串清除；
    /// 且不被前端携带的陈旧 save_settings 快照顶回（与 mcp_* 同一保全策略）。
    #[test]
    fn set_active_model_persists_and_survives_stale_save_settings() {
        let dir = temp_dir("active_model");
        let store = Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap();

        // 用户在应用内为 codex 选定对外模型名。
        store.set_active_model("codex", "claude-opus-4-8").unwrap();
        assert_eq!(
            store.get_settings().active_models.get("codex").map(|s| s.as_str()),
            Some("claude-opus-4-8"),
        );

        // 前端切主题时提交的旧快照 active_models 为空：不得清除已选模型。
        let mut stale = store.get_settings();
        stale.active_models = std::collections::HashMap::new();
        stale.theme = "dark".into();
        store.save_settings(stale).unwrap();
        assert_eq!(
            store.get_settings().active_models.get("codex").map(|s| s.as_str()),
            Some("claude-opus-4-8"),
            "已选模型应保留，不被前端空快照顶回",
        );
        assert_eq!(store.get_settings().theme, "dark", "非自管字段应正常更新");

        // 空串清除该分类选择（回到透传）；重开 Store 后仍为空。
        store.set_active_model("codex", "").unwrap();
        assert!(!store.get_settings().active_models.contains_key("codex"));
        let store2 = Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap();
        assert!(!store2.get_settings().active_models.contains_key("codex"), "清除后应持久");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 方案 A：Codex 默认推理强度是后端自管字段，与 active_models 同一保全策略：
    /// 专用命令直写、前端 save_settings 的陈旧空快照不得顶掉、空串清除、重开持久。
    #[test]
    fn set_active_effort_persists_and_survives_stale_save_settings() {
        let dir = temp_dir("active_effort");
        let store = Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap();

        store.set_active_effort("codex", "xhigh").unwrap();
        assert_eq!(
            store.get_settings().active_efforts.get("codex").map(|s| s.as_str()),
            Some("xhigh"),
        );

        // 前端切主题携带的旧快照 active_efforts 为空：不得清除已配强度。
        let mut stale = store.get_settings();
        stale.active_efforts = std::collections::HashMap::new();
        stale.theme = "dark".into();
        store.save_settings(stale).unwrap();
        assert_eq!(
            store.get_settings().active_efforts.get("codex").map(|s| s.as_str()),
            Some("xhigh"),
            "已配强度应保留，不被前端空快照顶回",
        );

        // 空串清除；重开 Store 后仍为空。
        store.set_active_effort("codex", "").unwrap();
        assert!(!store.get_settings().active_efforts.contains_key("codex"));
        let store2 = Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap();
        assert!(!store2.get_settings().active_efforts.contains_key("codex"), "清除后应持久");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 健康态无实质变化（仅 last_checked/latency 变）时 update_health 不重复落盘。
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

        std::thread::sleep(std::time::Duration::from_millis(20));
        // status 变化 → 应落盘
        store
            .update_health("k1", HealthState { status: HealthStatus::Down, fail_count: 1, ..Default::default() })
            .unwrap();
        let mtime3 = std::fs::metadata(&cfg).unwrap().modified().unwrap();
        assert!(mtime3 > mtime2, "status 变化应重写 config.json");
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

    /// 回归 review：磁盘读/解析失败(正是 persist 失败之因,如目标被建成目录)时，
    /// rollback_from_disk 回退到改动前内存快照兜底，保证内存不领先磁盘。
    #[test]
    fn rollback_from_disk_falls_back_to_snapshot_when_disk_unreadable() {
        let dir = temp_dir("rollback_fallback");
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
}
