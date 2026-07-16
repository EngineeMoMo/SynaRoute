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

        let secrets = SecretStore::load(secrets_path)?;

        let store = Self {
            config_path,
            config: RwLock::new(config),
            secrets: RwLock::new(secrets),
            events: RwLock::new(Vec::new()),
        };
        if seeded {
            store.persist()?;
        }
        Ok(store)
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
        ev.push(entry);
        if ev.len() > MAX_EVENTS {
            let overflow = ev.len() - MAX_EVENTS;
            ev.drain(0..overflow);
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
        let cfg = self.config.read();
        let data = serde_json::to_vec_pretty(&*cfg)?;
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
            } else {
                return Err(AppError::NotFound(key_id.into()));
            }
        }
        self.persist()
    }

    /// 更新某 Key 的健康状态（健康检查模块调用）
    pub fn update_health(&self, key_id: &str, health: HealthState) -> AppResult<()> {
        {
            let mut cfg = self.config.write();
            if let Some(k) = cfg.keys.iter_mut().find(|k| k.id == key_id) {
                k.health = health;
            }
        }
        self.persist()
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

    pub fn save_settings(&self, settings: AppSettings) -> AppResult<()> {
        {
            let mut cfg = self.config.write();
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
    fn new_at(config_path: PathBuf, secrets_path: PathBuf) -> AppResult<Self> {
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
}
