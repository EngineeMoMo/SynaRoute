//! 密钥加密存储（FR-018 / NFR-006）
//!
//! 双模式（arch-decisions §3）：
//! - 默认 DPAPI 免口令：用 Windows DPAPI 绑定当前用户账户加密，无需口令。
//! - 可选主口令增强：用 Argon2 从主口令派生密钥，AES-256-GCM 加密。
//!
//! 密钥单独存于 secrets 加密文件，绝不随 ProviderKey 经 IPC 下发前端。

use crate::error::{AppError, AppResult};
use aes_gcm::aead::{Aead, KeyInit, OsRng};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::{engine::general_purpose::STANDARD, Engine};
use rand::RngCore;
use std::collections::HashMap;
use std::path::PathBuf;

/// 加密后的密钥库文件结构（落盘为 JSON）
#[derive(serde::Serialize, serde::Deserialize, Default)]
struct SecretVault {
    /// keyId -> base64(密文)，密文本身由所选模式加密
    entries: HashMap<String, String>,
    /// 是否使用主口令模式
    master_mode: bool,
}

pub struct SecretStore {
    path: PathBuf,
    vault: SecretVault,
    /// 主口令模式下的 AES 密钥（内存态，不落盘）
    master_key: Option<[u8; 32]>,
}

impl SecretStore {
    pub fn load(path: PathBuf) -> AppResult<Self> {
        let vault = if path.exists() {
            let raw = std::fs::read(&path)?;
            serde_json::from_slice(&raw).unwrap_or_default()
        } else {
            SecretVault::default()
        };
        Ok(Self { path, vault, master_key: None })
    }

    #[allow(dead_code)] // 主口令模式能力，前端接线后启用
    pub fn is_master_mode(&self) -> bool {
        self.vault.master_mode
    }

    /// 设置主口令（用于主口令模式解锁/初始化）
    #[allow(dead_code)] // 主口令模式能力，前端接线后启用
    pub fn unlock_with_master(&mut self, password: &str) -> AppResult<()> {
        let key = derive_key(password)?;
        self.master_key = Some(key);
        Ok(())
    }

    /// 保存一条密钥（加密）
    pub fn set(&mut self, key_id: &str, secret: &str) -> AppResult<()> {
        let cipher = self.encrypt(secret.as_bytes())?;
        self.vault.entries.insert(key_id.to_string(), STANDARD.encode(cipher));
        self.persist()
    }

    /// 读取一条明文密钥（仅在代理转发时后端内部使用，绝不返回前端）
    pub fn get(&self, key_id: &str) -> AppResult<Option<String>> {
        let Some(b64) = self.vault.entries.get(key_id) else {
            return Ok(None);
        };
        let cipher = STANDARD.decode(b64).map_err(|e| AppError::Crypto(e.to_string()))?;
        let plain = self.decrypt(&cipher)?;
        Ok(Some(String::from_utf8_lossy(&plain).to_string()))
    }

    pub fn remove(&mut self, key_id: &str) -> AppResult<()> {
        self.vault.entries.remove(key_id);
        self.persist()
    }

    fn persist(&self) -> AppResult<()> {
        let data = serde_json::to_vec_pretty(&self.vault)?;
        atomic_write(&self.path, &data)
    }

    // ---- 加解密分派 ----

    fn encrypt(&self, plain: &[u8]) -> AppResult<Vec<u8>> {
        if let Some(key) = &self.master_key {
            aes_encrypt(key, plain)
        } else {
            dpapi_encrypt(plain)
        }
    }

    fn decrypt(&self, cipher: &[u8]) -> AppResult<Vec<u8>> {
        if let Some(key) = &self.master_key {
            aes_decrypt(key, cipher)
        } else {
            dpapi_decrypt(cipher)
        }
    }
}

/// 用 Argon2 从主口令派生 32 字节 AES 密钥（固定盐简化演示；正式版应存随机盐）
#[allow(dead_code)] // 主口令模式能力，前端接线后启用
fn derive_key(password: &str) -> AppResult<[u8; 32]> {
    use argon2::{Argon2, PasswordHasher};
    use argon2::password_hash::SaltString;
    // 固定盐：为使同一口令每次派生同一密钥（否则无法解密旧数据）
    let salt = SaltString::from_b64("c3luYXJvdXRlc2FsdA")
        .map_err(|e| AppError::Crypto(e.to_string()))?;
    let argon2 = Argon2::default();
    let hash = argon2
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| AppError::Crypto(e.to_string()))?;
    let hash_bytes = hash.hash.ok_or_else(|| AppError::Crypto("派生失败".into()))?;
    let mut key = [0u8; 32];
    let src = hash_bytes.as_bytes();
    key.copy_from_slice(&src[..32.min(src.len())]);
    Ok(key)
}

/// AES-256-GCM 加密：输出 [12B nonce | ciphertext]
fn aes_encrypt(key: &[u8; 32], plain: &[u8]) -> AppResult<Vec<u8>> {
    let cipher = Aes256Gcm::new(key.into());
    let mut nonce_bytes = [0u8; 12];
    OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ct = cipher.encrypt(nonce, plain).map_err(|e| AppError::Crypto(e.to_string()))?;
    let mut out = nonce_bytes.to_vec();
    out.extend_from_slice(&ct);
    Ok(out)
}

fn aes_decrypt(key: &[u8; 32], data: &[u8]) -> AppResult<Vec<u8>> {
    if data.len() < 12 {
        return Err(AppError::Crypto("密文过短".into()));
    }
    let (nonce_bytes, ct) = data.split_at(12);
    let cipher = Aes256Gcm::new(key.into());
    let nonce = Nonce::from_slice(nonce_bytes);
    cipher.decrypt(nonce, ct).map_err(|e| AppError::Crypto(e.to_string()))
}

// ---- DPAPI（仅 Windows）----

#[cfg(windows)]
fn dpapi_encrypt(plain: &[u8]) -> AppResult<Vec<u8>> {
    use windows::Win32::Security::Cryptography::{CryptProtectData, CRYPT_INTEGER_BLOB};
    unsafe {
        let mut in_blob = CRYPT_INTEGER_BLOB {
            cbData: plain.len() as u32,
            pbData: plain.as_ptr() as *mut u8,
        };
        let mut out_blob = CRYPT_INTEGER_BLOB::default();
        CryptProtectData(&mut in_blob, None, None, None, None, 0, &mut out_blob)
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
        let mut in_blob = CRYPT_INTEGER_BLOB {
            cbData: cipher.len() as u32,
            pbData: cipher.as_ptr() as *mut u8,
        };
        let mut out_blob = CRYPT_INTEGER_BLOB::default();
        CryptUnprotectData(&mut in_blob, None, None, None, None, 0, &mut out_blob)
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
    // 回退：无 DPAPI 时用固定内建密钥，仅供非 Windows 开发调试
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
