//! 口令派生密钥的加密信封（Argon2id + AES-256-GCM）。
//!
//! **为什么需要这一层**：现有密钥库走 Windows DPAPI（[`crate::secret`]），它把密钥绑定到
//! **当前 Windows 账户**——安全、免口令，但密文换机器/换账户一律解不出。两个场景因此必须
//! 另走口令派生：
//!
//! 1. **配置导出（FR-021）**：导出的意义就是备份与**迁移**。若照搬 DPAPI 密文，用户在新机器
//!    导入后每个 Key 都报「密钥缺失」——文件看着导成了、实际全废。
//! 2. **主口令增强模式（FR-018 可选增强）**：用户显式选择「不依赖系统账户，自己记口令」。
//!
//! 两处共用本模块，避免各写一套 KDF 参数与信封格式（那必然会出现「导出的解不开、
//! 主口令的又是另一套」这类分叉）。
//!
//! ## 两组 API，别混用
//!
//! | 场景 | API | 每次调用的成本 |
//! |---|---|---|
//! | 导出/导入（一次性，整份载荷一个信封） | [`seal`] / [`open`] | 跑一次 Argon2（约百毫秒） |
//! | 主口令密钥库（每条密钥各一个盒子） | [`derive_vault_key`] + [`seal_with_key`] / [`open_with_key`] | 只有 AES-GCM（微秒级） |
//!
//! 后者存在的理由：密钥库里每条密钥都要独立加解密，而代理转发每次取密钥都要解一次。
//! 若每条都跑 KDF，一次多 Key 故障转移就是几百毫秒纯 CPU。故解锁时派生一次
//! [`VaultKey`] 常驻内存，各条只做对称加解密。
//!
//! ## 信封格式（自描述、带版本）
//!
//! ```text
//! { v: 1, kdf: "argon2id", m: <KiB>, t: <iters>, p: <lanes>,
//!   salt: base64(16B), nonce: base64(12B), ct: base64(密文+GCM tag) }
//! ```
//!
//! KDF 参数**写进信封而非硬编码在解密侧**：日后调强参数（硬件变快）时，老文件仍能用它
//! 自己记录的参数解开。这是「能重新导入同版本导出文件」这条验收标准的前提。

use crate::error::{AppError, AppResult};
use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::{engine::general_purpose::STANDARD, Engine};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

/// 信封格式版本。解密时严格校验——不认识的版本宁可报错，也不要按旧格式硬解出垃圾。
const ENVELOPE_VERSION: u32 = 1;

/// KDF 标识。目前只有 argon2id；留字符串而非 bool 是为了将来能加别的而不破格式。
const KDF_ARGON2ID: &str = "argon2id";

/// Argon2id 参数。取值理由：
/// - `m = 64 MiB`：够抗 GPU 暴力，又不至于让低配机（或同时开多个应用的机器）分配失败。
///   OWASP 对交互式场景的建议区间正在此量级。
/// - `t = 3`：配合 64 MiB 后单次派生约百毫秒级——用户输一次口令等这点时间无感，
///   而攻击者的每次尝试都要付同样代价。
/// - `p = 1`：单线程。派生只发生在「导出/导入/解锁」这种一次性操作上，不值得为并行度
///   引入线程数差异（不同机器 lanes 不同会让同一口令派生出不同密钥，除非把 p 也写进信封——
///   我们确实写了，但固定 1 更省心）。
const ARGON2_M_COST_KIB: u32 = 64 * 1024;
const ARGON2_T_COST: u32 = 3;
const ARGON2_P_COST: u32 = 1;

/// 盐长度（字节）。16B 是 Argon2 推荐下限，足以杜绝彩虹表与跨文件复用。
const SALT_LEN: usize = 16;
/// AES-GCM nonce 长度（字节）。GCM 标准 96 位。
const NONCE_LEN: usize = 12;

/// 口令加密信封。序列化后作为导出文件的一个字段 / 密钥库的容器。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope {
    /// 格式版本（见 [`ENVELOPE_VERSION`]）
    pub v: u32,
    /// KDF 名（当前恒 `argon2id`）
    pub kdf: String,
    /// Argon2 内存开销（KiB）
    pub m: u32,
    /// Argon2 迭代次数
    pub t: u32,
    /// Argon2 并行度
    pub p: u32,
    /// base64(盐)
    pub salt: String,
    /// base64(nonce)
    pub nonce: String,
    /// base64(密文 || GCM tag)
    pub ct: String,
}

// ============ 长驻密钥模式（主口令用）============
//
// [`seal`] / [`open`] 每次调用都跑一遍 Argon2（约百毫秒），适合「导出/导入」这种一次性操作。
// 主口令模式不能这么用：密钥库里每条密钥都要独立加解密，而代理转发每次都要取密钥——
// 若每条都跑 KDF，一次多 Key 故障转移就是几百毫秒纯 CPU。
//
// 故拆成两段：解锁时**派生一次** [`VaultKey`] 常驻内存，之后各条密钥用
// [`seal_with_key`] / [`open_with_key`] 走纯 AES-GCM（微秒级）。
// 每条各有自己的 nonce —— 同密钥下复用 nonce 会直接泄露明文异或。

/// KDF 参数 + 盐。存在密钥库头部，解锁时据此派生 [`VaultKey`]。
///
/// 与 [`Envelope`] 一样把参数写进文件而非硬编码在读取侧：日后调强参数，
/// 老密钥库仍能用它自己记录的参数解开（否则用户一升级就打不开自己的密钥库）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KdfHeader {
    /// KDF 名（当前恒 `argon2id`）
    pub kdf: String,
    /// Argon2 内存开销（KiB）
    pub m: u32,
    /// Argon2 迭代次数
    pub t: u32,
    /// Argon2 并行度
    pub p: u32,
    /// base64(盐)
    pub salt: String,
}

impl KdfHeader {
    /// 用当前推荐参数 + 全新随机盐生成一份头部。
    pub fn new_random() -> Self {
        let mut salt = [0u8; SALT_LEN];
        rand::rngs::OsRng.fill_bytes(&mut salt);
        Self {
            kdf: KDF_ARGON2ID.into(),
            m: ARGON2_M_COST_KIB,
            t: ARGON2_T_COST,
            p: ARGON2_P_COST,
            salt: STANDARD.encode(salt),
        }
    }
}

/// 解锁后常驻内存的库密钥。Drop 时清零。
///
/// 不实现 `Clone`/`Debug`：避免无意中复制或打进日志。
pub struct VaultKey(DerivedKey);

/// 一条用 [`VaultKey`] 加密的数据（nonce 与密文各自 base64）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SealedBox {
    /// base64(nonce)
    pub nonce: String,
    /// base64(密文 || GCM tag)
    pub ct: String,
}

/// 按库头部派生常驻密钥。口令错在这一步**察觉不到**（KDF 不校验），
/// 由调用方用 [`make_verifier`] / [`check_verifier`] 判定。
pub fn derive_vault_key(password: &str, hdr: &KdfHeader) -> AppResult<VaultKey> {
    if password.is_empty() {
        return Err(AppError::Invalid("口令不能为空".into()));
    }
    if hdr.kdf != KDF_ARGON2ID {
        return Err(AppError::Invalid(format!(
            "未知的口令派生算法「{}」，本程序只支持 {KDF_ARGON2ID}",
            hdr.kdf
        )));
    }
    let salt = STANDARD
        .decode(&hdr.salt)
        .map_err(|e| AppError::Invalid(format!("密钥库损坏（盐字段不是合法 base64）: {e}")))?;
    Ok(VaultKey(derive_key(password, &salt, hdr.m, hdr.t, hdr.p)?))
}

/// 用常驻密钥加密一段数据（每次全新 nonce）。
pub fn seal_with_key(key: &VaultKey, plain: &[u8]) -> AppResult<SealedBox> {
    let mut nonce_bytes = [0u8; NONCE_LEN];
    rand::rngs::OsRng.fill_bytes(&mut nonce_bytes);
    let cipher = Aes256Gcm::new((&key.0 .0).into());
    let ct = cipher
        .encrypt(Nonce::from_slice(&nonce_bytes), plain)
        .map_err(|e| AppError::Crypto(format!("加密失败: {e}")))?;
    Ok(SealedBox {
        nonce: STANDARD.encode(nonce_bytes),
        ct: STANDARD.encode(&ct),
    })
}

/// 用常驻密钥解开一段数据。认证失败（口令错 / 数据被改）与格式坏分开报。
pub fn open_with_key(key: &VaultKey, boxed: &SealedBox) -> AppResult<Vec<u8>> {
    let nonce = STANDARD
        .decode(&boxed.nonce)
        .map_err(|e| AppError::Invalid(format!("密钥库损坏（nonce 不是合法 base64）: {e}")))?;
    let ct = STANDARD
        .decode(&boxed.ct)
        .map_err(|e| AppError::Invalid(format!("密钥库损坏（密文不是合法 base64）: {e}")))?;
    if nonce.len() != NONCE_LEN {
        return Err(AppError::Invalid(format!(
            "密钥库损坏（nonce 长度应为 {NONCE_LEN} 字节，实际 {}）",
            nonce.len()
        )));
    }
    let cipher = Aes256Gcm::new((&key.0 .0).into());
    cipher
        .decrypt(Nonce::from_slice(&nonce), ct.as_ref())
        .map_err(|_| AppError::Invalid("口令错误，或密钥库已被修改/损坏。".into()))
}

/// 校验串的固定明文。内容本身不敏感，作用只是「能否解开」这一个比特。
const VERIFIER_PLAINTEXT: &[u8] = b"synaroute-master-password-v1";

/// 生成校验串，随库头部一起存。
///
/// **为什么必须有**：解锁时若靠「试解某条密钥」判断口令对不对，密钥库为空（用户还没存 Key）
/// 时就无从判断，会把错口令当成解锁成功、随后新存的密钥用错密钥加密、旧密钥永久解不开。
pub fn make_verifier(key: &VaultKey) -> AppResult<SealedBox> {
    seal_with_key(key, VERIFIER_PLAINTEXT)
}

/// 校验口令是否正确（能解开且明文匹配）。
///
/// 这里用普通 `==` 比较而非常量时间比较，是**刻意的**：
/// - 真正的判据是上一步 GCM 的认证标签（`open_with_key` 里），那一步由 `aes-gcm` 用常量时间
///   比较完成 —— 口令错时根本走不到这行。
/// - 走到这行说明认证已通过，此时比对的是一段**公开的固定明文**（`VERIFIER_PLAINTEXT`），
///   它不是秘密，比较耗时泄露不了任何信息。
///
/// 若把这行改成「拿某条真实密钥试解并比对」，那才需要常量时间比较。
pub fn check_verifier(key: &VaultKey, verifier: &SealedBox) -> bool {
    matches!(open_with_key(key, verifier), Ok(p) if p == VERIFIER_PLAINTEXT)
}

/// 派生出的 32 字节对称密钥。Drop 时清零，缩短明文密钥在内存里的驻留窗口。
///
/// **这条防护的实际范围要说清，别当成保证**：
/// - 清零只覆盖**本结构体持有的那份**字节。`Aes256Gcm::new(&key)` 会把密钥拷进 cipher 对象
///   （`aes-gcm` 内部展开成轮密钥），那份副本由 `aes-gcm` 自己管理，`Zeroize` 管不到；
///   本模块的用法是「每次加解密现场构造 cipher、用完即 drop」，故那份副本生命周期极短，
///   但**不为零**。
/// - Rust 的 move 语义可能在栈上留下未清零的副本；操作系统也可能把页换出到页文件。
///
/// 所以真正的安全边界仍是既定信任模型：**本机单用户、密钥自管**。清零的价值在于缩短窗口、
/// 减少「进程崩溃后 dump 里恰好还留着明文密钥」这类偶发暴露，不是防御有本机管理员权限的攻击者。
#[derive(Zeroize)]
#[zeroize(drop)]
struct DerivedKey([u8; 32]);

/// 用口令 + 盐 + 参数派生 32 字节密钥。
///
/// 参数从调用方传入（而非读常量）：解密时必须用**信封里记录的**参数，否则日后调参会让
/// 老文件解不开。
fn derive_key(password: &str, salt: &[u8], m: u32, t: u32, p: u32) -> AppResult<DerivedKey> {
    use argon2::{Algorithm, Argon2, Params, Version};
    let params = Params::new(m, t, p, Some(32))
        .map_err(|e| AppError::Crypto(format!("Argon2 参数非法(m={m},t={t},p={p}): {e}")))?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut out = [0u8; 32];
    argon
        .hash_password_into(password.as_bytes(), salt, &mut out)
        .map_err(|e| AppError::Crypto(format!("口令派生失败: {e}")))?;
    Ok(DerivedKey(out))
}

/// 用口令加密任意字节，产出自描述信封。
pub fn seal(password: &str, plain: &[u8]) -> AppResult<Envelope> {
    if password.is_empty() {
        return Err(AppError::Invalid("口令不能为空".into()));
    }
    let mut salt = [0u8; SALT_LEN];
    let mut nonce_bytes = [0u8; NONCE_LEN];
    // OsRng：走系统 CSPRNG。盐与 nonce 都必须每次全新——GCM 下同密钥复用 nonce 会直接泄露明文异或。
    rand::rngs::OsRng.fill_bytes(&mut salt);
    rand::rngs::OsRng.fill_bytes(&mut nonce_bytes);

    let key = derive_key(password, &salt, ARGON2_M_COST_KIB, ARGON2_T_COST, ARGON2_P_COST)?;
    let cipher = Aes256Gcm::new((&key.0).into());
    let ct = cipher
        .encrypt(Nonce::from_slice(&nonce_bytes), plain)
        .map_err(|e| AppError::Crypto(format!("加密失败: {e}")))?;

    Ok(Envelope {
        v: ENVELOPE_VERSION,
        kdf: KDF_ARGON2ID.into(),
        m: ARGON2_M_COST_KIB,
        t: ARGON2_T_COST,
        p: ARGON2_P_COST,
        salt: STANDARD.encode(salt),
        nonce: STANDARD.encode(nonce_bytes),
        ct: STANDARD.encode(&ct),
    })
}

/// 用口令解开信封。
///
/// **错误必须能区分「口令错」与「文件坏」**——两者的用户动作完全不同（重输 vs 换文件），
/// 混成一句「解密失败」会让人反复试口令却毫无进展。GCM 认证失败即口令错（或密文被篡改），
/// 故那条单独给文案；格式/base64/版本问题归「文件损坏或版本不符」。
pub fn open(password: &str, env: &Envelope) -> AppResult<Vec<u8>> {
    if env.v != ENVELOPE_VERSION {
        return Err(AppError::Invalid(format!(
            "加密信封版本不支持：文件是 v{}，本程序支持 v{ENVELOPE_VERSION}。请用相同或更新版本的 SynaRoute 打开。",
            env.v
        )));
    }
    if env.kdf != KDF_ARGON2ID {
        return Err(AppError::Invalid(format!(
            "未知的口令派生算法「{}」，本程序只支持 {KDF_ARGON2ID}",
            env.kdf
        )));
    }
    let salt = STANDARD
        .decode(&env.salt)
        .map_err(|e| AppError::Invalid(format!("文件损坏（盐字段不是合法 base64）: {e}")))?;
    let nonce = STANDARD
        .decode(&env.nonce)
        .map_err(|e| AppError::Invalid(format!("文件损坏（nonce 字段不是合法 base64）: {e}")))?;
    let ct = STANDARD
        .decode(&env.ct)
        .map_err(|e| AppError::Invalid(format!("文件损坏（密文字段不是合法 base64）: {e}")))?;
    if nonce.len() != NONCE_LEN {
        return Err(AppError::Invalid(format!(
            "文件损坏（nonce 长度应为 {NONCE_LEN} 字节，实际 {}）",
            nonce.len()
        )));
    }

    // 用**信封里记录的**参数派生，而非当前常量——保证调参后老文件仍可解。
    let key = derive_key(password, &salt, env.m, env.t, env.p)?;
    let cipher = Aes256Gcm::new((&key.0).into());
    cipher
        .decrypt(Nonce::from_slice(&nonce), ct.as_ref())
        .map_err(|_| {
            // 刻意不透出底层错误细节：GCM 失败原因单一（认证不通过），且详情对用户无意义。
            AppError::Invalid(
                "口令错误，或文件已被修改/损坏。请确认口令后重试（口令区分大小写）。".into(),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试用轻量 Argon2 参数：正式参数 64MiB×3 单次约百毫秒，一个测试文件跑十几次会明显拖慢。
    /// 这里只验证信封逻辑（参数从信封读取这一点本身也被下面的用例覆盖），故用最小合法参数。
    fn seal_fast(password: &str, plain: &[u8]) -> Envelope {
        let mut salt = [0u8; SALT_LEN];
        let mut nonce_bytes = [0u8; NONCE_LEN];
        rand::rngs::OsRng.fill_bytes(&mut salt);
        rand::rngs::OsRng.fill_bytes(&mut nonce_bytes);
        // 8 MiB / 1 轮：Argon2 允许的低端，够快且仍走真实实现。
        let (m, t, p) = (8 * 1024, 1, 1);
        let key = derive_key(password, &salt, m, t, p).unwrap();
        let cipher = Aes256Gcm::new((&key.0).into());
        let ct = cipher
            .encrypt(Nonce::from_slice(&nonce_bytes), plain)
            .unwrap();
        Envelope {
            v: ENVELOPE_VERSION,
            kdf: KDF_ARGON2ID.into(),
            m,
            t,
            p,
            salt: STANDARD.encode(salt),
            nonce: STANDARD.encode(nonce_bytes),
            ct: STANDARD.encode(&ct),
        }
    }

    #[test]
    fn seal_open_roundtrip_with_real_params() {
        // 正式参数至少跑通一次——否则「常量写错导致 Params::new 报错」这类问题测不出来。
        let env = seal("正确口令-Aa1!", b"secret payload").unwrap();
        assert_eq!(env.v, ENVELOPE_VERSION);
        assert_eq!(env.kdf, KDF_ARGON2ID);
        assert_eq!(env.m, ARGON2_M_COST_KIB);
        let out = open("正确口令-Aa1!", &env).unwrap();
        assert_eq!(out, b"secret payload");
    }

    #[test]
    fn wrong_password_reports_password_error_not_corruption() {
        // 口令错与文件坏的用户动作不同（重输 vs 换文件），文案必须能区分。
        let env = seal_fast("right", b"payload");
        let err = open("wrong", &env).unwrap_err().to_string();
        assert!(err.contains("口令错误"), "应明确指向口令: {err}");
    }

    #[test]
    fn tampered_ciphertext_is_rejected() {
        // GCM 是 AEAD：密文被改一个字节就必须认证失败，而不是解出垃圾明文。
        let mut env = seal_fast("pw", b"payload");
        let mut raw = STANDARD.decode(&env.ct).unwrap();
        raw[0] ^= 0xFF;
        env.ct = STANDARD.encode(&raw);
        assert!(open("pw", &env).is_err(), "篡改的密文必须被拒");
    }

    #[test]
    fn unknown_version_and_kdf_are_rejected_with_actionable_message() {
        let mut env = seal_fast("pw", b"x");
        let good = env.clone();

        env.v = 99;
        let err = open("pw", &env).unwrap_err().to_string();
        assert!(err.contains("版本"), "要说清是版本问题: {err}");
        assert!(err.contains("更新版本的 SynaRoute"), "要给出行动指引: {err}");

        env = good.clone();
        env.kdf = "scrypt".into();
        let err = open("pw", &env).unwrap_err().to_string();
        assert!(err.contains("scrypt"), "要点出是哪个算法不认识: {err}");
    }

    #[test]
    fn corrupt_base64_fields_report_corruption_not_password_error() {
        let good = seal_fast("pw", b"x");

        for (field, mutate) in [
            ("盐", 0usize),
            ("nonce", 1),
            ("密文", 2),
        ] {
            let mut env = good.clone();
            match mutate {
                0 => env.salt = "!!!not-base64!!!".into(),
                1 => env.nonce = "!!!not-base64!!!".into(),
                _ => env.ct = "!!!not-base64!!!".into(),
            }
            let err = open("pw", &env).unwrap_err().to_string();
            assert!(
                err.contains("文件损坏"),
                "{field} 字段坏应报「文件损坏」而非口令错（否则用户白试口令）: {err}"
            );
        }
    }

    #[test]
    fn wrong_nonce_length_is_rejected_before_decrypt() {
        // 长度不对时 Nonce::from_slice 会 panic，必须在那之前挡住。
        let mut env = seal_fast("pw", b"x");
        env.nonce = STANDARD.encode([0u8; 8]); // 只有 8 字节
        let err = open("pw", &env).unwrap_err().to_string();
        assert!(err.contains("nonce 长度"), "{err}");
    }

    #[test]
    fn each_seal_uses_fresh_salt_and_nonce() {
        // 同口令同明文两次加密必须产出不同盐/nonce/密文。
        // GCM 下复用 nonce 会泄露明文异或；盐复用则让同口令派生同密钥、跨文件可比对。
        let a = seal_fast("pw", b"same payload");
        let b = seal_fast("pw", b"same payload");
        assert_ne!(a.salt, b.salt, "盐必须每次全新");
        assert_ne!(a.nonce, b.nonce, "nonce 必须每次全新");
        assert_ne!(a.ct, b.ct, "密文不应相同（否则等于确定性加密，可比对）");
    }

    #[test]
    fn empty_password_is_rejected() {
        assert!(seal("", b"x").is_err(), "空口令等于没加密，必须拒绝");
    }

    #[test]
    fn params_are_read_from_envelope_not_from_constants() {
        // 关键前向兼容保证：日后调强常量后，用旧参数加密的文件仍必须能解开。
        // 这里用与常量**不同**的参数造信封（seal_fast 就是 8MiB/1 轮），若 open 用常量派生
        // 就会得到不同密钥、认证失败。
        let env = seal_fast("pw", b"old file");
        assert_ne!(env.m, ARGON2_M_COST_KIB, "前置条件：测试参数与常量不同");
        assert_eq!(open("pw", &env).unwrap(), b"old file");
    }

    // ---- 长驻密钥模式（主口令）----

    /// 测试用轻量 KDF 头：正式参数 64MiB×3，一个文件里派生十几次会明显拖慢。
    fn fast_header() -> KdfHeader {
        let mut hdr = KdfHeader::new_random();
        hdr.m = 8 * 1024;
        hdr.t = 1;
        hdr
    }

    #[test]
    fn vault_key_seals_and_opens_many_boxes() {
        // 主口令模式的核心用法：派生一次，加解密多条。
        let hdr = fast_header();
        let key = derive_vault_key("pw", &hdr).unwrap();
        let a = seal_with_key(&key, b"sk-one").unwrap();
        let b = seal_with_key(&key, b"sk-two").unwrap();
        assert_eq!(open_with_key(&key, &a).unwrap(), b"sk-one");
        assert_eq!(open_with_key(&key, &b).unwrap(), b"sk-two");
    }

    #[test]
    fn each_sealed_box_uses_fresh_nonce() {
        // 同一 VaultKey 下复用 nonce 会泄露明文异或——每条必须独立 nonce。
        let hdr = fast_header();
        let key = derive_vault_key("pw", &hdr).unwrap();
        let a = seal_with_key(&key, b"same").unwrap();
        let b = seal_with_key(&key, b"same").unwrap();
        assert_ne!(a.nonce, b.nonce, "nonce 必须每条全新");
        assert_ne!(a.ct, b.ct, "同明文不应产出相同密文");
    }

    #[test]
    fn same_password_same_header_derives_same_key() {
        // 解锁的前提：同口令 + 同头部必须派生出同一密钥，否则重启后解不开自己的库。
        let hdr = fast_header();
        let k1 = derive_vault_key("pw", &hdr).unwrap();
        let sealed = seal_with_key(&k1, b"payload").unwrap();
        let k2 = derive_vault_key("pw", &hdr).unwrap();
        assert_eq!(open_with_key(&k2, &sealed).unwrap(), b"payload");
    }

    #[test]
    fn verifier_distinguishes_right_and_wrong_password_even_on_empty_vault() {
        // 这是校验串存在的唯一理由：密钥库为空时无从「试解一条密钥」，
        // 没有校验串就会把错口令当成解锁成功，随后新存的密钥用错密钥加密、旧的永久解不开。
        let hdr = fast_header();
        let right = derive_vault_key("correct-horse", &hdr).unwrap();
        let verifier = make_verifier(&right).unwrap();

        assert!(check_verifier(&right, &verifier), "正确口令必须通过");

        let wrong = derive_vault_key("wrong", &hdr).unwrap();
        assert!(!check_verifier(&wrong, &verifier), "错误口令必须被拒");
    }

    #[test]
    fn tampered_sealed_box_is_rejected() {
        let hdr = fast_header();
        let key = derive_vault_key("pw", &hdr).unwrap();
        let mut boxed = seal_with_key(&key, b"payload").unwrap();
        let mut raw = STANDARD.decode(&boxed.ct).unwrap();
        raw[0] ^= 0xFF;
        boxed.ct = STANDARD.encode(&raw);
        assert!(open_with_key(&key, &boxed).is_err(), "篡改必须被 GCM 认证拦下");
    }

    #[test]
    fn vault_key_rejects_empty_password_and_unknown_kdf() {
        let hdr = fast_header();
        assert!(derive_vault_key("", &hdr).is_err(), "空口令等于没加密");

        let mut bad = fast_header();
        bad.kdf = "scrypt".into();
        // 刻意不用 unwrap_err()：VaultKey 有意不实现 Debug（避免密钥被打进日志），
        // 而 unwrap_err 要求 Ok 侧可 Debug。
        let err = match derive_vault_key("pw", &bad) {
            Ok(_) => panic!("未知 KDF 必须被拒"),
            Err(e) => e.to_string(),
        };
        assert!(err.contains("scrypt"), "要点出是哪个算法不认识: {err}");
    }

    #[test]
    fn vault_header_salt_is_random_per_creation() {
        // 盐复用会让同口令在不同库派生出同一密钥，跨库可比对。
        assert_ne!(KdfHeader::new_random().salt, KdfHeader::new_random().salt);
    }

    #[test]
    fn malformed_sealed_box_reports_corruption_not_password_error() {
        // 与 open() 同样的口径：格式坏 ≠ 口令错，两者用户动作不同。
        let hdr = fast_header();
        let key = derive_vault_key("pw", &hdr).unwrap();
        let good = seal_with_key(&key, b"x").unwrap();

        let mut bad = good.clone();
        bad.nonce = "!!!".into();
        assert!(open_with_key(&key, &bad).unwrap_err().to_string().contains("损坏"));

        let mut short = good.clone();
        short.nonce = STANDARD.encode([0u8; 8]);
        let err = open_with_key(&key, &short).unwrap_err().to_string();
        assert!(err.contains("nonce 长度"), "{err}");
    }
}
