//! 配置持久化与内存态管理。
//! 配置文件（不含密钥）存 JSON；密钥存 SecretStore（加密）。
//! 所有写操作走原子写（NFR-011）。

use crate::error::{AppError, AppResult};
use crate::model::*;
use crate::secret::{atomic_write, SecretStore};
use parking_lot::RwLock;
use std::path::PathBuf;

pub struct Store {
    config_path: PathBuf,
    config: RwLock<AppConfig>,
    pub secrets: RwLock<SecretStore>,
    /// 内存态事件日志（FR-020），每分类最多保留 N 条
    events: RwLock<Vec<EventLogEntry>>,
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
    pub fn init() -> AppResult<Self> {
        let data_dir = dirs::data_dir()
            .ok_or_else(|| AppError::Other("无法定位数据目录".into()))?
            .join("SynaRoute");
        std::fs::create_dir_all(&data_dir)?;

        let config_path = data_dir.join("config.json");
        let secrets_path = data_dir.join("secrets.enc");

        let mut config: AppConfig = if config_path.exists() {
            let raw = std::fs::read(&config_path)?;
            serde_json::from_slice(&raw).unwrap_or_default()
        } else {
            AppConfig::default()
        };

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

        let store = Self {
            config_path,
            config: RwLock::new(config),
            secrets: RwLock::new(secrets),
            events: RwLock::new(Vec::new()),
        };
        if seeded || migrated {
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

    fn persist(&self) -> AppResult<()> {
        // 仅在序列化期间持读锁；随后释放锁再做阻塞落盘。
        // atomic_write 含进程级互斥 + 最长数百毫秒 sleep 重试，若持锁期间执行，会经
        // parking_lot「写者优先」把后续所有读者（代理转发的 enabled_keys_sorted /
        // secrets 读等）挡在等待的写者之后，阻塞 tokio worker 线程。
        let data = {
            let cfg = self.config.read();
            serde_json::to_vec_pretty(&*cfg)?
        };
        atomic_write(&self.config_path, &data)
    }

    // ---- Key CRUD ----

    pub fn list_keys(&self, category: CategoryType) -> Vec<ProviderKey> {
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
        {
            let mut cfg = self.config.write();
            if let Some(existing) = cfg.keys.iter_mut().find(|k| k.id == key.id) {
                *existing = key.clone();
            } else {
                cfg.keys.push(key.clone());
            }
        }
        self.persist()?;
        Ok(key)
    }

    pub fn delete_key(&self, key_id: &str) -> AppResult<()> {
        {
            let mut cfg = self.config.write();
            cfg.keys.retain(|k| k.id != key_id);
        }
        self.secrets.write().remove(key_id).ok();
        self.persist()
    }

    pub fn toggle_key(&self, key_id: &str, enabled: bool) -> AppResult<()> {
        {
            let mut cfg = self.config.write();
            if let Some(k) = cfg.keys.iter_mut().find(|k| k.id == key_id) {
                k.enabled = enabled;
                // 禁用时清空健康状态：否则遗留的 Down/熔断会一直显示「不可用」，
                // 而禁用的 Key 已不再被探测、无从自愈。重置为 Unknown，重新启用后再探测。
                if !enabled {
                    k.health = HealthState::default();
                }
            } else {
                return Err(AppError::NotFound(key_id.into()));
            }
        }
        self.persist()
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
        {
            let mut cfg = self.config.write();
            if let Some(k) = cfg.keys.iter_mut().find(|k| k.id == key_id) {
                k.models = models;
            }
        }
        self.persist()
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
            // 若直接覆盖会在每次切主题/语言时清空注册记录。故保留已有值，只由后端注册逻辑更新。
            settings.mcp_registered_categories =
                std::mem::take(&mut cfg.settings.mcp_registered_categories);
            cfg.settings = settings;
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
        Ok(Self {
            config_path,
            config: RwLock::new(config),
            secrets: RwLock::new(secrets),
            events: RwLock::new(Vec::new()),
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
}
