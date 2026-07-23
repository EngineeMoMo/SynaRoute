//! 密钥加密存储（FR-018 / NFR-006）
//!
//! 当前仅实现 Windows DPAPI 免口令加密（绑定当前用户账户）。
//! 主口令增强模式 UI 标注「开发中」且从未接线，相关解锁/派生代码已移除，避免死代码。
//! vault 仍保留 master_mode/salt 字段以兼容旧 secrets 文件反序列化。
//!
//! 密钥单独存于 secrets 加密文件，绝不随 ProviderKey 经 IPC 下发前端。

use crate::error::{AppError, AppResult};
use base64::{engine::general_purpose::STANDARD, Engine};
use std::collections::HashMap;
use std::path::PathBuf;

/// 加密后的密钥库文件结构（落盘为 JSON）
#[derive(serde::Serialize, serde::Deserialize, Default)]
struct SecretVault {
    /// keyId -> base64(密文)
    entries: HashMap<String, String>,
    /// 历史主口令模式标记（当前运行时忽略，仅兼容旧文件）
    #[serde(default)]
    master_mode: bool,
    /// 历史主口令盐（当前运行时忽略，仅兼容旧文件）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    salt: Option<String>,
}

pub struct SecretStore {
    path: PathBuf,
    vault: SecretVault,
    /// 加载时读/解析失败而降级为空库的标记。置位后首次落盘前必须先备份磁盘原文件——
    /// 否则「空库 + 用户新存的一条」整份覆盖会销毁其余全部密文（与 config 的 init
    /// fallback 覆盖同类风险）。备份失败则拒绝写入。
    load_failed: bool,
}

impl SecretStore {
    pub fn load(path: PathBuf) -> AppResult<Self> {
        // 读失败与解析失败都降级为空库并留诊断日志，绝不让 app 起不来：
        // 旧实现 `std::fs::read(&path)?` 会把瞬时读失败（杀软/备份独占锁、权限抖动）
        // 一路冒泡到 Store::init().expect → panic、窗口永不创建，用户无从排障。
        // 降级后 UI 正常、Key 可见，密钥暂不可用（转发时报「密钥缺失」），重启读成功即恢复。
        let (vault, load_failed) = if path.exists() {
            match std::fs::read(&path) {
                Ok(raw) => match serde_json::from_slice::<SecretVault>(&raw) {
                    Ok(v) => (v, false),
                    Err(e) => {
                        tracing::error!("密钥库解析失败,降级为空库(磁盘文件保持原样,不自动覆盖): {e}. 路径={path:?}");
                        (SecretVault::default(), true)
                    }
                },
                Err(e) => {
                    tracing::error!("密钥库读取失败,降级为空库(磁盘文件保持原样,不自动覆盖): {e}. 路径={path:?}");
                    (SecretVault::default(), true)
                }
            }
        } else {
            (SecretVault::default(), false)
        };
        Ok(Self { path, vault, load_failed })
    }

    /// 保存一条密钥（加密）
    pub fn set(&mut self, key_id: &str, secret: &str) -> AppResult<()> {
        let cipher = dpapi_encrypt(secret.as_bytes())?;
        self.vault.entries.insert(key_id.to_string(), STANDARD.encode(cipher));
        self.persist()
    }

    /// 读取一条明文密钥（仅在代理转发时后端内部使用，绝不返回前端）
    pub fn get(&self, key_id: &str) -> AppResult<Option<String>> {
        let Some(b64) = self.vault.entries.get(key_id) else {
            return Ok(None);
        };
        let cipher = STANDARD.decode(b64).map_err(|e| AppError::Crypto(e.to_string()))?;
        let plain = dpapi_decrypt(&cipher)?;
        Ok(Some(String::from_utf8_lossy(&plain).to_string()))
    }

    pub fn remove(&mut self, key_id: &str) -> AppResult<()> {
        self.vault.entries.remove(key_id);
        self.persist()
    }

    fn persist(&mut self) -> AppResult<()> {
        // 降级空库后的首次写入：先备份磁盘原文件（内含用户全部既有密文），备份失败则拒绝写，
        // 防止「空库+单条新密钥」把原密钥库整份覆盖销毁。
        if self.load_failed {
            if self.path.exists() {
                let ts = chrono::Utc::now().format("%Y%m%d-%H%M%S");
                let bak = self.path.with_extension(format!("enc.corrupt-{ts}"));
                std::fs::copy(&self.path, &bak).map_err(|e| {
                    AppError::Other(format!(
                        "密钥库处于降级态,备份原文件失败,已拒绝覆盖写入(避免销毁既有密文): {e}"
                    ))
                })?;
                tracing::warn!("密钥库降级态首次写入,已备份原文件到 {bak:?}");
            }
            self.load_failed = false;
        }
        let data = serde_json::to_vec_pretty(&self.vault)?;
        atomic_write(&self.path, &data)
    }
}

// ---- DPAPI（仅 Windows）----

#[cfg(windows)]
fn dpapi_encrypt(plain: &[u8]) -> AppResult<Vec<u8>> {
    use windows::Win32::Security::Cryptography::{CryptProtectData, CRYPT_INTEGER_BLOB};
    unsafe {
        let in_blob = CRYPT_INTEGER_BLOB {
            cbData: plain.len() as u32,
            pbData: plain.as_ptr() as *mut u8,
        };
        let mut out_blob = CRYPT_INTEGER_BLOB::default();
        CryptProtectData(&in_blob, None, None, None, None, 0, &mut out_blob)
            .map_err(|e| AppError::Crypto(format!("DPAPI 加密失败: {e}")))?;
        let slice = std::slice::from_raw_parts(out_blob.pbData, out_blob.cbData as usize);
        let out = slice.to_vec();
        let _ = windows::Win32::Foundation::LocalFree(windows::Win32::Foundation::HLOCAL(
            out_blob.pbData as *mut _,
        ));
        Ok(out)
    }
}

#[cfg(windows)]
fn dpapi_decrypt(cipher: &[u8]) -> AppResult<Vec<u8>> {
    use windows::Win32::Security::Cryptography::{CryptUnprotectData, CRYPT_INTEGER_BLOB};
    unsafe {
        let in_blob = CRYPT_INTEGER_BLOB {
            cbData: cipher.len() as u32,
            pbData: cipher.as_ptr() as *mut u8,
        };
        let mut out_blob = CRYPT_INTEGER_BLOB::default();
        CryptUnprotectData(&in_blob, None, None, None, None, 0, &mut out_blob)
            .map_err(|e| AppError::Crypto(format!("DPAPI 解密失败: {e}")))?;
        let slice = std::slice::from_raw_parts(out_blob.pbData, out_blob.cbData as usize);
        let out = slice.to_vec();
        let _ = windows::Win32::Foundation::LocalFree(windows::Win32::Foundation::HLOCAL(
            out_blob.pbData as *mut _,
        ));
        Ok(out)
    }
}

// 非 Windows 平台回退（开发期在其他平台也能编译；正式仅 Windows）
#[cfg(not(windows))]
fn dpapi_encrypt(plain: &[u8]) -> AppResult<Vec<u8>> {
    aes_encrypt(&fallback_key(), plain)
}

#[cfg(not(windows))]
fn dpapi_decrypt(cipher: &[u8]) -> AppResult<Vec<u8>> {
    aes_decrypt(&fallback_key(), cipher)
}

#[cfg(not(windows))]
fn fallback_key() -> [u8; 32] {
    *b"synaroute-dev-fallback-key-32byte"
}

/// AES-256-GCM（仅非 Windows 开发回退使用）
#[cfg(not(windows))]
fn aes_encrypt(key: &[u8; 32], plain: &[u8]) -> AppResult<Vec<u8>> {
    use aes_gcm::aead::{Aead, KeyInit, OsRng};
    use aes_gcm::{Aes256Gcm, Nonce};
    use rand::RngCore;
    let cipher = Aes256Gcm::new(key.into());
    let mut nonce_bytes = [0u8; 12];
    OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ct = cipher.encrypt(nonce, plain).map_err(|e| AppError::Crypto(e.to_string()))?;
    let mut out = nonce_bytes.to_vec();
    out.extend_from_slice(&ct);
    Ok(out)
}

#[cfg(not(windows))]
fn aes_decrypt(key: &[u8; 32], data: &[u8]) -> AppResult<Vec<u8>> {
    use aes_gcm::aead::{Aead, KeyInit};
    use aes_gcm::{Aes256Gcm, Nonce};
    if data.len() < 12 {
        return Err(AppError::Crypto("密文过短".into()));
    }
    let (nonce_bytes, ct) = data.split_at(12);
    let cipher = Aes256Gcm::new(key.into());
    let nonce = Nonce::from_slice(nonce_bytes);
    cipher.decrypt(nonce, ct).map_err(|e| AppError::Crypto(e.to_string()))
}

/// 原子写：写临时文件再重命名替换，避免半写损坏（NFR-011 / dev-hard-rules）。
///
/// 关键健壮性处理（企业管控机实战问题，真实 app 内 CDP 抓到的根因）：
/// 1. 唯一临时文件名（pid + 原子计数器）——避免后台健康检查线程与保存操作
///    共用同一个 `.tmp` 文件产生写-改名竞态。
/// 2. **跨设备 rename 回退**：本机 `%APPDATA%`（企业文件夹重定向/配额卷绑定）上，
///    `fs::rename(tmp, target)` 即使同目录也确定性抛 `os error 17`
///    (ERROR_NOT_SAME_DEVICE)。这是确定性失败，重试无效。检测到后（或多次重试
///    仍失败）直接**原地写目标文件**，牺牲严格原子性换取"一定能存进去"——
///    对配置文件而言，能持久化是硬需求，原子性是加分项。
pub fn atomic_write(path: &std::path::Path, data: &[u8]) -> AppResult<()> {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Mutex;
    static SEQ: AtomicU64 = AtomicU64::new(0);
    // 全局写锁：atomic_write 是所有落盘（config + secrets）的唯一入口，
    // 用一把进程级锁把并发写串行化。根因就是后台健康检查线程的 update_health→persist
    // 与 IPC 线程的 toggle_key→persist 并发原地写同一文件，Windows 抛
    // ERROR_SHARING_VIOLATION(32) 导致其中一个静默失败。串行化后彻底消除该竞态。
    static WRITE_LOCK: Mutex<()> = Mutex::new(());
    let _guard = WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    // 错误上下文封装：把裸 io::Error 包成带路径+os码的可读信息，便于事件日志定位。
    let ctx = |stage: &str, e: &std::io::Error| -> AppError {
        AppError::Other(format!(
            "落盘失败[{stage}] 路径={} os错误码={:?}: {e}",
            path.display(),
            e.raw_os_error()
        ))
    };

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| ctx("建目录", &e))?;
    }

    // 唯一临时名：<原名>.<pid>.<seq>.tmp，避免并发写同一 .tmp
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "synaroute".into());
    let tmp = match path.parent() {
        Some(parent) => parent.join(format!("{file_name}.{}.{seq}.tmp", std::process::id())),
        None => std::path::PathBuf::from(format!("{file_name}.{}.{seq}.tmp", std::process::id())),
    };

    std::fs::write(&tmp, data).map_err(|e| ctx("写临时文件", &e))?;

    // ERROR_NOT_SAME_DEVICE：跨设备移动，重试无意义，直接回退原地写。
    const ERROR_NOT_SAME_DEVICE: i32 = 17;
    // ERROR_ACCESS_DENIED(5) / ERROR_SHARING_VIOLATION(32)：杀软/并发瞬时锁，值得短暂重试。
    let mut delay_ms = 5u64;
    for attempt in 0..6 {
        match std::fs::rename(&tmp, path) {
            Ok(()) => return Ok(()),
            Err(e) => {
                if e.raw_os_error() == Some(ERROR_NOT_SAME_DEVICE) {
                    break; // 跨设备：立即回退
                }
                if attempt < 5 {
                    std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                    delay_ms = (delay_ms * 2).min(200);
                }
            }
        }
    }

    // 回退：原地直接写目标文件（绕开跨设备 rename）。tmp 里已有完整数据作为最后保障。
    // 同样对瞬时锁（ACCESS_DENIED/SHARING_VIOLATION）短暂重试，避免单次写被瞬时锁打断即失败。
    let mut delay_ms = 5u64;
    let mut last_err: Option<std::io::Error> = None;
    for attempt in 0..6 {
        match std::fs::write(path, data) {
            Ok(()) => {
                let _ = std::fs::remove_file(&tmp);
                return Ok(());
            }
            Err(e) => {
                last_err = Some(e);
                if attempt < 5 {
                    std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                    delay_ms = (delay_ms * 2).min(200);
                }
            }
        }
    }

    let _ = std::fs::remove_file(&tmp);
    Err(ctx(
        "原地写(回退)",
        &last_err.unwrap_or_else(|| std::io::Error::other("未知落盘错误")),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir()
            .join(format!("synaroute_secret_test_{}_{}_{}", tag, std::process::id(), seq));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 损坏的密钥库文件:load 不得报错(降级空库,app 能起),磁盘原文件保持原样。
    #[test]
    fn corrupt_vault_degrades_without_error_and_keeps_disk() {
        let dir = temp_dir("corrupt_load");
        let path = dir.join("secrets.enc");
        std::fs::write(&path, b"{ not valid json !!").unwrap();

        let store = SecretStore::load(path.clone()).expect("损坏文件不得让 load 报错(否则 init panic,app 打不开)");
        assert!(store.vault.entries.is_empty(), "应降级为空库");
        assert!(store.load_failed, "应置降级标记");
        // 磁盘原文件未被动过
        assert_eq!(std::fs::read(&path).unwrap(), b"{ not valid json !!", "load 阶段不得改写磁盘");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 降级态首次写入:必须先把原文件备份成 .enc.corrupt-*,防「空库+新单条」整份覆盖销毁既有密文。
    #[test]
    fn degraded_first_persist_backs_up_original() {
        let dir = temp_dir("degraded_backup");
        let path = dir.join("secrets.enc");
        let original = b"{ corrupted but contains user ciphertexts }".to_vec();
        std::fs::write(&path, &original).unwrap();

        let mut store = SecretStore::load(path.clone()).unwrap();
        assert!(store.load_failed);
        store.set("k_new", "sk-test-secret").expect("降级态写入应成功(先备份后写)");

        // 原文件字节должны保留在 .enc.corrupt-* 备份里
        let backup = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .find(|e| e.file_name().to_string_lossy().contains("enc.corrupt-"))
            .expect("应生成 corrupt 备份文件");
        assert_eq!(std::fs::read(backup.path()).unwrap(), original, "备份必须是原文件字节");

        // 新库可解析、含且仅含新条目;标记已清除,二次写不再重复备份
        let v: SecretVault = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(v.entries.len(), 1);
        assert!(v.entries.contains_key("k_new"));
        assert!(!store.load_failed, "首次成功写入后应清除降级标记");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 正常路径:文件不存在→空库、无降级标记;写入无备份副作用。
    #[test]
    fn fresh_vault_set_get_roundtrip() {
        let dir = temp_dir("fresh");
        let path = dir.join("secrets.enc");
        let mut store = SecretStore::load(path.clone()).unwrap();
        assert!(!store.load_failed);

        store.set("k1", "sk-abc").unwrap();
        assert_eq!(store.get("k1").unwrap().as_deref(), Some("sk-abc"), "DPAPI 加解密应可逆");
        // 无 corrupt 备份产生
        let has_bak = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| e.file_name().to_string_lossy().contains("corrupt"));
        assert!(!has_bak, "正常路径不应产生 corrupt 备份");

        std::fs::remove_dir_all(&dir).ok();
    }
}
