//! 密钥加密存储（FR-018 / NFR-006）
//!
//! 两种保护模式，同一时刻只有一种生效：
//!
//! | 模式 | 密钥来源 | 换机可用 | 需要输口令 |
//! |---|---|---|---|
//! | **DPAPI**（默认） | Windows `CryptProtectData`，绑当前用户账户 | ❌ 解不出 | 否 |
//! | **主口令**（FR-018 可选增强） | Argon2id 从用户口令派生（[`crate::crypto`]） | ✅ | 每次启动解锁一次 |
//!
//! 模式记录在密钥库文件自身（`master` 字段在即为主口令模式），**它是唯一事实来源**；
//! `settings.master_password_enabled` 只是给 UI 看的镜像，启动时按库对账。
//! 反过来（以 settings 为准）会出现「配置说开着、库里其实是 DPAPI 密文」这种解不开的死局。
//!
//! 两模式的密文分开存（`entries` / `boxes`），不共用一个 map：格式不同，混在一起一旦
//! 迁移中断就无法判断某条到底是哪种密文，只能靠猜。分开存则「哪个 map 里有」即答案。
//!
//! 密钥单独存于 secrets 加密文件，绝不随 ProviderKey 经 IPC 下发前端。

use crate::error::{AppError, AppResult};
use base64::{engine::general_purpose::STANDARD, Engine};
use std::collections::HashMap;
use std::path::PathBuf;
use zeroize::Zeroizing;

/// 主口令模式的库头部：KDF 参数 + 盐 + 校验串。存在即表示当前是主口令模式。
#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct MasterHeader {
    /// KDF 参数与盐（解锁时据此派生库密钥）
    kdf: crate::crypto::KdfHeader,
    /// 口令校验串：库里没有任何密钥时也能判断口令对不对（见 `crypto::make_verifier`）
    verifier: crate::crypto::SealedBox,
}

/// 加密后的密钥库文件结构（落盘为 JSON）
#[derive(serde::Serialize, serde::Deserialize, Default)]
struct SecretVault {
    /// **DPAPI 模式**的密文：keyId -> base64(DPAPI 密文)
    entries: HashMap<String, String>,
    /// 历史主口令模式标记（当前运行时忽略，仅兼容旧文件）
    #[serde(default)]
    master_mode: bool,
    /// 历史主口令盐（当前运行时忽略，仅兼容旧文件）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    salt: Option<String>,
    /// 主口令模式的库头部。**存在即为主口令模式。**
    #[serde(default, skip_serializing_if = "Option::is_none")]
    master: Option<MasterHeader>,
    /// **主口令模式**的密文：keyId -> 口令派生密钥加密的盒子
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    boxes: HashMap<String, crate::crypto::SealedBox>,
}

/// 主口令模式的运行时状态，供 UI 展示与解锁引导。
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MasterPasswordState {
    /// 是否处于主口令模式（判据是密钥库里有 `master` 头部，不看 settings）
    pub enabled: bool,
    /// 是否已锁定（主口令模式但本次进程还没解锁）。DPAPI 模式恒 `false`
    pub locked: bool,
}

pub struct SecretStore {
    path: PathBuf,
    vault: SecretVault,
    /// 加载时读/解析失败而降级为空库的标记。置位后首次落盘前必须先备份磁盘原文件——
    /// 否则「空库 + 用户新存的一条」整份覆盖会销毁其余全部密文（与 config 的 init
    /// fallback 覆盖同类风险）。备份失败则拒绝写入。
    load_failed: bool,
    /// 解锁后常驻的库密钥（仅主口令模式）。`None` = DPAPI 模式，或主口令模式但未解锁。
    /// 进程退出即消失，故每次启动都要重新解锁。
    vault_key: Option<crate::crypto::VaultKey>,
    /// DPAPI 模式下的**进程内解密缓存**（P2-6）。
    ///
    /// 为什么只给 DPAPI 模式：DPAPI 每次 `get` 都要走一次 `CryptUnprotectData` 内核态系统调用
    /// （10~50µs，杀软钩子下更高），而转发热路径每请求每候选都取一次密钥。主口令模式本就有
    /// 长驻 `vault_key`、每条只做 AES-GCM，没有这个问题，故不为它引入缓存状态。
    ///
    /// 安全权衡：明文密钥本来就要拼进 Authorization 头发出去，缓存**不新增暴露面**；
    /// 且值用 `Zeroizing` 包着，逐出/析构即清零。
    ///
    /// ⚠️ **失效点必须穷举覆盖**（漏一处就是「明明更新过密钥却仍报鉴权失败」这个历史症状）：
    /// `set` / `remove` / 三个整库迁移 / `lock` / `unlock`，共 7 处。
    /// **锁定态必须清空**——否则会破坏「锁定态 `get` 返 Err」这条刻意行为，
    /// 也让「立即锁定」名不副实（已解出的明文仍能从缓存里拿到）。
    os_cache: HashMap<String, Zeroizing<String>>,
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
        Ok(Self { path, vault, load_failed, vault_key: None, os_cache: HashMap::new() })
    }

    // ---- 主口令模式：状态与解锁 ----

    /// 当前是否为主口令模式。**判据是密钥库文件本身**（有 `master` 头部），不看 settings。
    pub fn is_master_mode(&self) -> bool {
        self.vault.master.is_some()
    }

    /// 主口令模式但本次进程尚未解锁 → 取不到任何密钥。
    pub fn is_locked(&self) -> bool {
        self.is_master_mode() && self.vault_key.is_none()
    }

    pub fn master_state(&self) -> MasterPasswordState {
        MasterPasswordState { enabled: self.is_master_mode(), locked: self.is_locked() }
    }

    /// 用口令解锁。口令错则**不改变**已解锁状态（避免「输错一次把已解锁的库锁回去」）。
    pub fn unlock(&mut self, password: &str) -> AppResult<()> {
        let Some(hdr) = self.vault.master.clone() else {
            return Err(AppError::Invalid(
                "当前不是主口令模式，无需解锁（密钥由 Windows 账户保护）".into(),
            ));
        };
        let key = crate::crypto::derive_vault_key(password, &hdr.kdf)?;
        if !crate::crypto::check_verifier(&key, &hdr.verifier) {
            return Err(AppError::Invalid(
                "主口令错误。请确认后重试（口令区分大小写）。".into(),
            ));
        }
        self.vault_key = Some(key);
        // 失效点 7/7：解锁前若残留过 DPAPI 模式的缓存（例如刚做完 disable→enable 往返），
        // 那些明文对应的已是旧模式的密文，留着只会给出过期结论。
        self.invalidate_cache();
        Ok(())
    }

    /// 主动上锁（清掉常驻密钥）。用于「立即锁定」这类显式操作。
    pub fn lock(&mut self) {
        self.vault_key = None;
        // **必须清缓存**（P2-6 失效点 6/7）：否则「立即锁定」名不副实——已解出的明文仍能从
        // 缓存里被 `get` 拿到，破坏「锁定态 get 返 Err」这条刻意行为。
        self.invalidate_cache();
    }

    /// 取当前可用的库密钥，未解锁时给出可行动的错误（而非静默返回「无密钥」）。
    ///
    /// 这条错误信息会一路冒泡到代理转发的失败原因里 —— 必须让用户看懂是「要解锁」，
    /// 而不是以为 Key 配错了去反复检查密钥。
    fn require_vault_key(&self) -> AppResult<&crate::crypto::VaultKey> {
        self.vault_key.as_ref().ok_or_else(|| {
            AppError::Invalid(
                "密钥库已用主口令加密但尚未解锁：请打开 SynaRoute 主窗口输入主口令解锁后重试。"
                    .into(),
            )
        })
    }

    /// 保存一条密钥（按当前模式加密）。
    ///
    /// 主口令模式下未解锁则**拒绝写入**——不能退回 DPAPI 加密，否则库里会同时存在两种密文，
    /// 而用户以为已经全部由口令保护。
    ///
    /// 两个分支都清掉**另一个 map 里的同 id 残留**：`get` 按当前模式单边读，若另一边留着
    /// 旧密文，切模式后会读出**过期的密钥**（用户改过密钥、切回原模式却拿到改之前那条），
    /// 表现为「明明更新过密钥却仍报鉴权失败」。
    pub fn set(&mut self, key_id: &str, secret: &str) -> AppResult<()> {
        if self.is_master_mode() {
            let boxed = {
                let key = self.require_vault_key()?;
                crate::crypto::seal_with_key(key, secret.as_bytes())?
            };
            self.vault.boxes.insert(key_id.to_string(), boxed);
            self.vault.entries.remove(key_id);
        } else {
            let cipher = os_encrypt(secret.as_bytes())?;
            self.vault.entries.insert(key_id.to_string(), STANDARD.encode(cipher));
            // 对称清理：DPAPI 模式下也要清掉 boxes 里的同 id（关闭主口令后新存的密钥，
            // 若 boxes 残留旧条目，将来再启用主口令时 all_key_ids 会读到那条过期密文）。
            self.vault.boxes.remove(key_id);
        }
        // 失效点 1/7：这一条的明文变了，缓存里那份已过期。
        // 漏掉它就是「明明更新过密钥却仍报鉴权失败」这个历史症状。
        // 只清这一条不够保险？——`invalidate_cache` 整体清空更稳：`set` 是低频用户操作，
        // 全清的代价只是后续几次请求各多一次解密，而漏清的代价是持续用错密钥。
        self.invalidate_cache();
        self.persist()
    }

    /// 读取一条明文密钥（仅在代理转发时后端内部使用，绝不返回前端）。
    ///
    /// 主口令模式下未解锁 → 返回 `Err`（而非 `Ok(None)`）。这个区分很重要：
    /// `Ok(None)` 的调用方会当成「这个 Key 没配密钥」，让用户去查配置；
    /// `Err` 才能把「需要解锁」这条可行动的信息带到 UI 与转发失败原因里。
    /// 取某条密钥的**明文**。
    ///
    /// 返回 `Zeroizing<String>`（P2-6）：析构时自动把缓冲区清零，缩短明文 API Key 在堆上的
    /// 驻留窗口。此前返回裸 `String`，明文副本会一直留在被释放的堆内存里直到被复用——
    /// 崩溃 dump / 页文件换出 / 休眠镜像都可能留存。这与项目已为 `DerivedKey` 做清零的意图
    /// 不一致：**最值钱的东西（上游 Key 明文）反而没有任何缩短窗口的措施**。
    ///
    /// 注意这不是完备防护（明文终究要拼进 Authorization 头发出去），只是把「无谓的长期驻留」
    /// 压成「用完即清」。
    pub fn get(&self, key_id: &str) -> AppResult<Option<Zeroizing<String>>> {
        if self.is_master_mode() {
            let Some(boxed) = self.vault.boxes.get(key_id) else {
                // 库里没这条：先判是否锁着——锁着时「没这条」的结论本身不可信
                // （可能只是还没解锁看不到，虽然本实现下 boxes 键名不加密，但保持语义一致）。
                self.require_vault_key()?;
                return Ok(None);
            };
            let key = self.require_vault_key()?;
            let plain = crate::crypto::open_with_key(key, boxed)?;
            return Ok(Some(Zeroizing::new(String::from_utf8_lossy(&plain).to_string())));
        }
        let Some(b64) = self.vault.entries.get(key_id) else {
            return Ok(None);
        };
        // DPAPI 模式：先查进程内缓存，命中即免掉一次 CryptUnprotectData 系统调用（P2-6）。
        // 缓存的失效由 set / remove / 三个迁移 / lock / unlock 共 7 处负责（见字段文档）。
        if let Some(hit) = self.os_cache.get(key_id) {
            return Ok(Some(hit.clone()));
        }
        let cipher = STANDARD.decode(b64).map_err(|e| AppError::Crypto(e.to_string()))?;
        let plain = os_decrypt(&cipher)?;
        Ok(Some(Zeroizing::new(String::from_utf8_lossy(&plain).to_string())))
    }

    /// 解密并**写入缓存**的 `get`（仅 DPAPI 模式有缓存效果）。
    ///
    /// 拆成独立方法而不是让 `get` 直接写缓存：`get` 只需 `&self`（转发热路径持读锁调用它），
    /// 写缓存需要 `&mut self`。转发路径按「先读锁查缓存、未命中再拿写锁填充」两段式使用，
    /// 由 `Store` 侧封装（见 `Store::secret_for`）。
    pub fn get_caching(&mut self, key_id: &str) -> AppResult<Option<Zeroizing<String>>> {
        let got = self.get(key_id)?;
        if let Some(v) = &got {
            // 只在 DPAPI 模式缓存：主口令模式有长驻 vault_key，本就不慢，
            // 不为它引入额外的明文驻留。
            if !self.is_master_mode() {
                self.os_cache.insert(key_id.to_string(), v.clone());
            }
        }
        Ok(got)
    }

    /// 该条是否已在解密缓存里（P2-6）。供 `Store::secret_for` 的「两段式」判断是否需要升级到写锁。
    pub fn is_cached(&self, key_id: &str) -> bool {
        self.os_cache.contains_key(key_id)
    }

    /// 清空解密缓存。**所有会让缓存过期的操作都必须调它**（见 `os_cache` 字段文档）。
    fn invalidate_cache(&mut self) {
        // Zeroizing 的值在这里被析构 → 自动清零，不留明文残迹。
        self.os_cache.clear();
    }

    pub fn remove(&mut self, key_id: &str) -> AppResult<()> {
        // 两个 map 都删：模式切换过程中断时同 id 可能两处都有，只删一个会留下能被
        // 反向模式读出的残留密文。
        self.vault.entries.remove(key_id);
        self.vault.boxes.remove(key_id);
        // 失效点 2/7：删了还留在缓存里，会让 `get` 对一条已不存在的 Key 返回明文。
        self.invalidate_cache();
        self.persist()
    }

    /// 库里现有的全部 keyId（不解密，仅用于迁移与对账）。
    ///
    /// **两个 map 取并集**，而不是按当前模式单边读。理由是迁移的安全性不能依赖「另一边一定是空的」
    /// 这个假设：`enable_master_password` 用它决定「要迁移哪些密钥」，若此刻 `boxes` 里还残留
    /// 上一次迁移中断留下的条目，单边读会把它们**静默漏掉**——迁移完成后那些 Key 的密钥就再也
    /// 读不出来了（`get` 只认当前模式那一边，而残留条目用的是另一套密钥）。
    ///
    /// 取并集则最坏情况是「多试解一条、失败即整体放弃」，不会丢数据。`set`/`remove` 已做双边
    /// 清理，正常路径下另一边本就是空的；这里是兜住异常路径。
    /// 改为 `pub(crate)`：`Store::prune_orphan_secrets`（P2-3）要据此找出
    /// 「密钥库里有、但配置里已无对应 Key」的孤儿。取并集这件事对孤儿清理同样重要——
    /// 只看单边会漏掉另一边的残留，那些密文将永远无法被清理。
    pub(crate) fn all_key_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.vault.boxes.keys().cloned().collect();
        for id in self.vault.entries.keys() {
            if !ids.iter().any(|x| x == id) {
                ids.push(id.clone());
            }
        }
        ids
    }

    // ---- 主口令模式：启用 / 关闭（整库迁移）----

    /// 启用主口令模式：把库里**全部** DPAPI 密文解出来，改用口令派生密钥重新封装。
    ///
    /// 三条硬要求，任一不满足都会造成密钥永久丢失：
    ///
    /// 1. **先全部解密成功再写盘**。若边解边写，中途某条 DPAPI 解密失败（密文损坏 /
    ///    账户变更）就会留下「一半口令密文、一半 DPAPI 密文」的半迁移库，而
    ///    `is_master_mode()` 已经为真 → 剩下那半永远读不出来。
    /// 2. **落盘前备份原库**。这是唯一一次「整份重写全部密文」的操作，写坏就全没了。
    /// 3. **落盘失败要回滚内存**，否则内存已是口令模式、磁盘还是 DPAPI，
    ///    此后每次 set 都按口令模式写，与磁盘上的 DPAPI 密文混成一团。
    pub fn enable_master_password(&mut self, password: &str) -> AppResult<usize> {
        if self.is_master_mode() {
            return Err(AppError::Invalid("已处于主口令模式，无需重复启用".into()));
        }
        if password.is_empty() {
            return Err(AppError::Invalid("主口令不能为空".into()));
        }
        if self.load_failed {
            // 降级态下 entries 是空的（读盘失败），此时迁移等于「把用户的密钥全丢掉再
            // 声称已加密」。必须拒绝，让用户先解决读盘问题。
            return Err(AppError::Invalid(
                "密钥库当前处于降级态（本次启动读取失败），为避免丢失既有密钥，已拒绝切换加密模式。\
                 请重启 SynaRoute 确认密钥可正常读取后再试。"
                    .into(),
            ));
        }

        // 第 1 步：全部解密到内存（任一失败即整体放弃，不动磁盘）。
        let ids = self.all_key_ids();
        let mut plain: Vec<(String, String)> = Vec::with_capacity(ids.len());
        for id in &ids {
            match self.get(id) {
                Ok(Some(s)) => plain.push((id.clone(), s.to_string())),
                // `Ok(None)` 在这里**不是**「这条没有密钥」，而是「它只存在于另一个 map 里」——
                // `all_key_ids` 取的是两个 map 的并集，而 `get` 按当前模式单边读。
                //
                // 必须当错误中止：继续走下去会在第 3 步 `std::mem::take` 时把那条密文
                // **静默丢弃**（新 vault 只装 `boxes`），用户永久失去该密钥且毫无提示。
                // 这属于「上一次迁移中断」的残留态，该让用户先处理，而不是替他做减法。
                Ok(None) => {
                    return Err(AppError::Crypto(format!(
                        "启用主口令失败：密钥「{id}」以另一种加密形态残留在库里（通常是上一次切换\
                         加密模式时中断所致），当前模式读不出它。为避免静默丢弃该密钥，已放弃切换、\
                         密钥库未被修改。\n\n\
                         处理方式：在界面上为该 Key 重新录入一次密钥（会覆盖残留密文），再试。"
                    )))
                }
                Err(e) => {
                    return Err(AppError::Crypto(format!(
                        "启用主口令失败：现有密钥「{id}」解密不出来（{e}），已放弃切换，密钥库未被修改。"
                    )))
                }
            }
        }

        // 第 2 步：备份原库（唯一一次整份重写，必须可回退）。
        self.backup_before_rewrite("master-on")?;

        // 第 3 步：构造新头部与新密文，全部成功后才替换内存态。
        let hdr = crate::crypto::KdfHeader::new_random();
        let vault_key = crate::crypto::derive_vault_key(password, &hdr)?;
        let verifier = crate::crypto::make_verifier(&vault_key)?;
        let mut boxes = HashMap::with_capacity(plain.len());
        for (id, s) in &plain {
            boxes.insert(id.clone(), crate::crypto::seal_with_key(&vault_key, s.as_bytes())?);
        }

        let prev = std::mem::take(&mut self.vault);
        // 失效点 3~5/7（三个整库迁移各一处）：整份重写后，缓存里的明文对应的都是**旧密文**，
        // 留着必然给出过期结论。放在 take 之后、写盘之前——即便随后回滚，多解密几次也无害，
        // 而漏清会让用错密钥持续下去。
        self.invalidate_cache();
        self.vault.master = Some(MasterHeader { kdf: hdr, verifier });
        self.vault.boxes = boxes;
        self.vault.entries.clear();
        self.vault_key = Some(vault_key);
        if let Err(e) = self.persist() {
            // 回滚内存，保持「内存态不领先磁盘」（与 store.rs 的 mutate_and_persist 同原则）。
            self.vault = prev;
            self.vault_key = None;
            return Err(e);
        }
        Ok(plain.len())
    }

    /// 关闭主口令模式：用口令解出全部密钥，改回 DPAPI 加密。
    ///
    /// 要求已解锁（或当场用给定口令解锁）——否则解不出任何密钥，关闭等于清空密钥库。
    pub fn disable_master_password(&mut self, password: &str) -> AppResult<usize> {
        if !self.is_master_mode() {
            return Err(AppError::Invalid("当前不是主口令模式，无需关闭".into()));
        }
        // 无论此前是否已解锁，都用本次输入的口令校验一遍：关闭是不可逆操作，
        // 必须确认操作者知道口令（防止有人趁已解锁的机器直接关掉保护）。
        self.unlock(password)?;

        let ids = self.all_key_ids();
        let mut plain: Vec<(String, String)> = Vec::with_capacity(ids.len());
        for id in &ids {
            match self.get(id) {
                Ok(Some(s)) => plain.push((id.clone(), s.to_string())),
                // 同 enable_master_password：`Ok(None)` 意味着该条只存在于另一个 map（中断残留），
                // 放行会让它在整份重写时被静默丢弃。
                Ok(None) => {
                    return Err(AppError::Crypto(format!(
                        "关闭主口令失败：密钥「{id}」以另一种加密形态残留在库里（通常是上一次切换\
                         加密模式时中断所致），当前模式读不出它。为避免静默丢弃该密钥，已放弃切换、\
                         密钥库未被修改。\n\n\
                         处理方式：在界面上为该 Key 重新录入一次密钥（会覆盖残留密文），再试。"
                    )))
                }
                Err(e) => {
                    return Err(AppError::Crypto(format!(
                        "关闭主口令失败：密钥「{id}」解密不出来（{e}），已放弃切换，密钥库未被修改。"
                    )))
                }
            }
        }

        self.backup_before_rewrite("master-off")?;

        let mut entries = HashMap::with_capacity(plain.len());
        for (id, s) in &plain {
            entries.insert(id.clone(), STANDARD.encode(os_encrypt(s.as_bytes())?));
        }

        let prev = std::mem::take(&mut self.vault);
        // 失效点 3~5/7（三个整库迁移各一处）：整份重写后，缓存里的明文对应的都是**旧密文**，
        // 留着必然给出过期结论。放在 take 之后、写盘之前——即便随后回滚，多解密几次也无害，
        // 而漏清会让用错密钥持续下去。
        self.invalidate_cache();
        let prev_key = self.vault_key.take();
        self.vault.entries = entries;
        self.vault.master = None;
        self.vault.boxes.clear();
        if let Err(e) = self.persist() {
            self.vault = prev;
            self.vault_key = prev_key;
            return Err(e);
        }
        Ok(plain.len())
    }

    /// 修改主口令：用旧口令解出全部密钥，用新口令重新封装（含新盐、新校验串）。
    pub fn change_master_password(&mut self, old: &str, new: &str) -> AppResult<usize> {
        if !self.is_master_mode() {
            return Err(AppError::Invalid("当前不是主口令模式，无法修改主口令".into()));
        }
        if new.is_empty() {
            return Err(AppError::Invalid("新主口令不能为空".into()));
        }
        self.unlock(old)?;

        let ids = self.all_key_ids();
        let mut plain: Vec<(String, String)> = Vec::with_capacity(ids.len());
        for id in &ids {
            match self.get(id)? {
                Some(s) => plain.push((id.clone(), s.to_string())),
                // 与另两个迁移函数同口径：`None` 意味着该条只存在于另一个 map（中断残留），
                // 放行会让它在整份重写时被静默丢弃。改口令同样是整份重写。
                None => {
                    return Err(AppError::Crypto(format!(
                        "修改主口令失败：密钥「{id}」以另一种加密形态残留在库里（通常是上一次切换\
                         加密模式时中断所致），当前模式读不出它。为避免静默丢弃该密钥，已放弃修改、\
                         密钥库未被修改。\n\n\
                         处理方式：在界面上为该 Key 重新录入一次密钥（会覆盖残留密文），再试。"
                    )))
                }
            }
        }

        self.backup_before_rewrite("master-change")?;

        let hdr = crate::crypto::KdfHeader::new_random();
        let vault_key = crate::crypto::derive_vault_key(new, &hdr)?;
        let verifier = crate::crypto::make_verifier(&vault_key)?;
        let mut boxes = HashMap::with_capacity(plain.len());
        for (id, s) in &plain {
            boxes.insert(id.clone(), crate::crypto::seal_with_key(&vault_key, s.as_bytes())?);
        }

        let prev = std::mem::take(&mut self.vault);
        // 失效点 3~5/7（三个整库迁移各一处）：整份重写后，缓存里的明文对应的都是**旧密文**，
        // 留着必然给出过期结论。放在 take 之后、写盘之前——即便随后回滚，多解密几次也无害，
        // 而漏清会让用错密钥持续下去。
        self.invalidate_cache();
        let prev_key = self.vault_key.take();
        self.vault.master = Some(MasterHeader { kdf: hdr, verifier });
        self.vault.boxes = boxes;
        if let Err(e) = self.persist() {
            self.vault = prev;
            self.vault_key = prev_key;
            return Err(e);
        }
        self.vault_key = Some(vault_key);
        Ok(plain.len())
    }

    /// 整份重写前的备份（带用途标签与时间戳，可回滚 —— dev-hard-rules）。
    /// 文件不存在时返回 `Ok(None)`，不算失败（首次启用、库还没落过盘）。
    ///
    /// `pub(crate)`：孤儿密钥清理（P2-3）与 Replace 导入清理旧密钥都是不可逆的删除操作，
    /// 必须先备份。**返回备份路径**，好让调用方把它交到用户手里 —— 一个用户找不到的
    /// 备份文件等于没有备份（Replace 导入那条路径尤其重要：报告里已有 config.json 的
    /// 备份路径，密钥库的必须并列给出，否则用户按报告回滚配置后才发现密钥回不来了）。
    pub(crate) fn backup_before_rewrite(&self, tag: &str) -> AppResult<Option<PathBuf>> {
        if !self.path.exists() {
            return Ok(None);
        }
        let ts = chrono::Utc::now().format("%Y%m%d-%H%M%S");
        let bak = self.path.with_extension(format!("enc.{tag}-{ts}"));
        std::fs::copy(&self.path, &bak).map_err(|e| {
            AppError::Other(format!(
                "整份重写密钥库前的备份失败（用途：{tag}），已放弃该操作（避免写坏后无从恢复）: {e}"
            ))
        })?;
        tracing::info!("密钥库整份重写前已备份到 {bak:?}（用途：{tag}）");
        Ok(Some(bak))
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

// ---- 操作系统托管的密钥保护：`os_encrypt` / `os_decrypt` ----
//
// 三份实现，按平台择一（同一时刻只有一份被编译）：
//
// | 平台 | 密钥由谁持有 | 加解密在哪做 | 换机可用 |
// |---|---|---|---|
// | Windows | 系统（DPAPI，绑用户账户） | 系统内核态 | ❌ |
// | macOS | 系统（Keychain，绑登录态） | 本进程 AES-256-GCM | ❌ |
// | 其他 Unix | **编译期常量**（仅开发） | 本进程 AES-256-GCM | ⚠️ 等同明文 |
//
// **刻意不再叫 `dpapi_*`**：非 Windows 那份从来就不是 DPAPI，而名字说是。
// 这类「名字与实现不符」正是密钥回退分支的缺陷能长期无人发现的原因之一
// （详见 `fallback_key` 的发现史）。缓存字段也从 `dpapi_cache` 一并改成 `os_cache`。
//
// 测试名里保留的 `dpapi` 字样（如 `..._falling_back_to_dpapi`）指的是**默认模式**这个概念，
// 不是特指 Win32 API —— 那些判据跨平台同样成立（mac 上默认模式是 Keychain），故不改名。

// ---- Windows：DPAPI ----

#[cfg(windows)]
fn os_encrypt(plain: &[u8]) -> AppResult<Vec<u8>> {
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
fn os_decrypt(cipher: &[u8]) -> AppResult<Vec<u8>> {
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

// ---- macOS：Keychain 托管一把库密钥 + AES-256-GCM ----
//
// 与 DPAPI 的信任模型对等：密钥由系统托管、绑当前登录用户、免口令、换机解不出。
// 差别在于 DPAPI 是「系统替你做加解密」，Keychain 是「系统替你保管密钥、加解密仍在本进程」——
// 故这里仍走 `aes_encrypt`/`aes_decrypt`，只是密钥来源换成 Keychain。
//
// **拒绝授权必须返 Err，绝不退到弱密钥**。这条纪律与主口令模式的锁定态同源
// （见 `locked_vault_refuses_writes_instead_of_falling_back_to_dpapi`）：
// 拿不到密钥时宁可让用户看到「取不到密钥」，也不能悄悄用一把更弱的把数据写下去
// —— 那会让「加密存储」这个承诺在用户不知情的情况下失效。

#[cfg(target_os = "macos")]
fn os_encrypt(plain: &[u8]) -> AppResult<Vec<u8>> {
    aes_encrypt(&keychain_vault_key()?, plain)
}

#[cfg(target_os = "macos")]
fn os_decrypt(cipher: &[u8]) -> AppResult<Vec<u8>> {
    aes_decrypt(&keychain_vault_key()?, cipher)
}

/// Keychain 里那条通用密码项的 service / account。
///
/// `service` 用 bundle identifier（与 `tauri.conf.json` 的 `identifier` 一致），
/// `account` 固定串——同一用户下只需一把库密钥。
#[cfg(target_os = "macos")]
const KEYCHAIN_SERVICE: &str = "com.synaroute.app";
#[cfg(target_os = "macos")]
const KEYCHAIN_ACCOUNT: &str = "vault-key";

/// 取（或首次生成）Keychain 里的库密钥。
///
/// ## 为什么缓存
///
/// 转发热路径每请求每候选都要取一次密钥。不缓存就是每次一趟 Keychain 系统调用
/// （且 ad-hoc 签名下每次都可能触发授权检查）。缓存后每进程一次。
///
/// **只在成功时写缓存**：用户第一次点「拒绝」不能被永久记成失败，之后授权了要能用上。
///
/// 安全权衡与主口令模式的长驻 `vault_key` 同源：明文密钥本就要拼进 Authorization 头发出去，
/// 且 `os_cache` 已经缓存了解密后的明文，缓存这把密钥不新增暴露面。
///
/// ## 双进程 / 同进程竞态
///
/// Keychain 只有 create-or-update 语义（`set_generic_password`），没有 create-if-absent，
/// 故「两个执行者同时发现无密钥、各自生成、各自写入」时后写者会覆盖前者。
///
/// **同进程必须用互斥锁串行初始化**：`SecretStore::get` 可在多个转发任务持读锁时并发调用，
/// 单实例插件只防第二个 GUI 进程、不防同进程线程。旧实现两线程都越过 `CACHED.get()` 后：
/// A 写入/读回 A 并 `CACHED.set(A)`，B 再写入/读回 B，B 的 `CACHED.set(B)` 失败却仍返回 B ——
/// 同一进程立刻同时用 A/B 两把库密钥；重启后 Keychain 只剩 B，所有用 A 加密的条目永久解不开。
/// `INIT_LOCK` + 锁内二次检查堵住这条数据丢失链。
///
/// 跨进程仍靠「写完立刻读回」收敛；主进程有 `tauri-plugin-single-instance`，
/// `--mcp-stdio` 子进程只做 JSON-RPC 转发、不开密钥库，所以那是窄窗口兜底，不是主路径。
#[cfg(target_os = "macos")]
fn keychain_vault_key() -> AppResult<[u8; 32]> {
    static CACHED: std::sync::OnceLock<[u8; 32]> = std::sync::OnceLock::new();
    if let Some(k) = CACHED.get() {
        return Ok(*k);
    }
    let key = keychain_fetch_or_create()?;
    let _ = CACHED.set(key);
    Ok(key)
}

/// 实际与 Keychain 打交道的那一半（供 [`keychain_vault_key`] 缓存调用）。
#[cfg(target_os = "macos")]
fn keychain_fetch_or_create() -> AppResult<[u8; 32]> {
    use security_framework::passwords::{generic_password, set_generic_password, PasswordOptions};
    use security_framework_sys::base::errSecItemNotFound;

    let read = || -> Result<Option<Vec<u8>>, security_framework::base::Error> {
        match generic_password(PasswordOptions::new_generic_password(
            KEYCHAIN_SERVICE,
            KEYCHAIN_ACCOUNT,
        )) {
            Ok(bytes) => Ok(Some(bytes)),
            // 「没有这一条」不是错误——首次运行必然走这里。
            Err(e) if e.code() == errSecItemNotFound => Ok(None),
            Err(e) => Err(e),
        }
    };

    // ① 已有则直接用。
    match read() {
        Ok(Some(bytes)) => return to_key32(&bytes),
        Ok(None) => {}
        // 拒绝授权 / 钥匙串锁定 / 其他失败：返 Err，**不生成新密钥**。
        // 生成新的会把 secrets.enc 里现有密文全部变成解不出的垃圾。
        Err(e) => {
            return Err(AppError::Crypto(format!(
                "读取 Keychain 库密钥失败（OSStatus {}）: {e}。\
                 若是授权弹框被拒，请在钥匙串访问里允许 SynaRoute 读取该项后重试。",
                e.code()
            )))
        }
    }

    // ② 首次运行：生成随机密钥并写入。
    let mut fresh = [0u8; 32];
    {
        use rand::RngCore;
        rand::rngs::OsRng.fill_bytes(&mut fresh);
    }
    set_generic_password(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT, &fresh).map_err(|e| {
        AppError::Crypto(format!("写入 Keychain 库密钥失败（OSStatus {}）: {e}", e.code()))
    })?;

    // ③ 读回确认，处理「同时写入」的竞态（见函数文档）。
    match read() {
        Ok(Some(bytes)) => {
            let got = to_key32(&bytes)?;
            if got != fresh {
                tracing::warn!(
                    "Keychain 库密钥被另一进程同时写入,已采用钥匙串里的那一把(避免两份密钥并存)"
                );
            }
            Ok(got)
        }
        // 刚写成功却读不到:不能拿 `fresh` 继续——下次启动读到的可能是别的值,
        // 那样这一轮加密的数据全部解不出。宁可这次报错。
        Ok(None) => Err(AppError::Crypto(
            "写入 Keychain 库密钥后读回为空,拒绝继续(避免用一把可能不会被持久化的密钥加密)".into(),
        )),
        Err(e) => Err(AppError::Crypto(format!(
            "写入 Keychain 库密钥后读回失败（OSStatus {}）: {e}",
            e.code()
        ))),
    }
}

/// 把 Keychain 里取出的字节转成 32 字节密钥。长度不符即报错——
/// 静默截断/补零会让密钥变成一把**可预测**的东西，比报错危险得多。
#[cfg(target_os = "macos")]
fn to_key32(bytes: &[u8]) -> AppResult<[u8; 32]> {
    if bytes.len() != 32 {
        return Err(AppError::Crypto(format!(
            "Keychain 里的库密钥长度异常（{} 字节,应为 32）。\
             该项可能被外部程序改写,请在钥匙串访问里删除 {KEYCHAIN_SERVICE}/{KEYCHAIN_ACCOUNT} 后重启\
             （注意:删除后现有密钥将无法解出,需重新录入）。",
            bytes.len()
        )));
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(bytes);
    Ok(key)
}

// ---- 其他 Unix（Linux 等）：仅开发用，不可分发 ----

#[cfg(all(not(windows), not(target_os = "macos")))]
fn os_encrypt(plain: &[u8]) -> AppResult<Vec<u8>> {
    aes_encrypt(&fallback_key(), plain)
}

#[cfg(all(not(windows), not(target_os = "macos")))]
fn os_decrypt(cipher: &[u8]) -> AppResult<Vec<u8>> {
    aes_decrypt(&fallback_key(), cipher)
}

/// **非 Windows 非 macOS**（Linux 等）的开发期回退密钥。
///
/// ⚠️ **不是生产可用的方案**，且刻意在运行时喊出来。这把密钥是编译进二进制的常量 ——
/// 谁拿到 `secrets.enc` 加一份程序就能全解出来。Windows 走 DPAPI、macOS 走 Keychain，
/// 两者都由系统托管密钥；只有这条分支没有系统级密钥保管可用，故仅供本地开发调试。
/// 若将来要正式支持 Linux，正解是接 Secret Service（libsecret）或强制主口令模式。
///
/// **发现史**（说明这条路径从未被走过）：原实现返回 `*b"synaroute-dev-fallback-key-32byte"`
/// —— 那个字面量是 **33 字节**（名字里写着 32，数错了），于是整个
/// `#[cfg(not(windows))]` 分支**根本编译不过**。注释声称「开发期在其他平台也能编译」，
/// 而在 macOS runner 上首次编译即 E0308。这也解释了为什么这个缺陷能长期存在：
/// Windows 上永远看不到它。
///
/// cfg 门刻意与调用方（`os_encrypt`/`os_decrypt` 的第三份实现）**完全一致**：
/// 写成 `not(windows)` 会让它在 macOS 上变成死代码 → dead_code 警告 → 撞破
/// 「clippy 零警告」基线。这不是洁癖，是让基线继续能当门禁用。
#[cfg(all(not(windows), not(target_os = "macos")))]
fn fallback_key() -> [u8; 32] {
    // 每进程只喊一次，避免转发热路径把日志刷爆；但一定要喊 ——
    // 本项目反复防的就是「看起来在加密、其实等于明文」这种静默降级。
    static WARNED: std::sync::Once = std::sync::Once::new();
    WARNED.call_once(|| {
        tracing::error!(
            "密钥库正在使用**编译期常量**回退密钥（非 Windows 平台）。\
             这等同于明文存储，仅供开发调试。生产分发前必须接 Keychain 或强制主口令模式。"
        );
    });

    // 从固定串取前 32 字节。刻意不改成「凑成 32 字节的新字面量」——
    // 那样只是把数错的字节数掩盖掉，而这把密钥的问题不在长度、在于它是常量。
    let mut key = [0u8; 32];
    key.copy_from_slice(&b"synaroute-dev-fallback-key-32byt"[..32]);
    key
}

/// AES-256-GCM，非 Windows 平台的实际加解密实现。
///
/// **macOS 上这是生产路径**（密钥来自 Keychain），其他 Unix 上是开发回退（密钥是常量）。
/// 两者共用同一套密码学实现、只有密钥来源不同 —— 故这段代码的正确性对 macOS 是有效要求，
/// 不是「反正只在开发时跑」。
///
/// 输出布局 `nonce(12) || ciphertext||tag`。nonce 每次随机（`OsRng`），
/// 故同一明文两次加密的密文不同 —— 这是 GCM 的硬要求（nonce 重用会泄露密钥流）。
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

/// 「跨设备移动」的 errno / Win32 错误码。
///
/// 抽成函数只为一件事：让这个平台差异有个可测的落点。它原先是 `atomic_write` 里一个
/// 无条件写死的 `17`，而 17 只在 Windows 上是 `ERROR_NOT_SAME_DEVICE`；Unix 上 17 是
/// `EEXIST`，跨设备是 `EXDEV`(18)。见调用点的注释。
#[inline]
fn cross_device_errno() -> i32 {
    #[cfg(windows)]
    {
        17 // ERROR_NOT_SAME_DEVICE
    }
    #[cfg(not(windows))]
    {
        libc::EXDEV
    }
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

    // 写临时文件失败时**必须清掉它自己**：`?` 直接返回会把一个可能半写入的 .tmp 留在
    // 数据目录里（本机实测存在 `config.json.4752.4.tmp` 这类 0 字节残留）。后续两条
    // 清理路径（rename 成功 / 回退原地写）都在这一行之后，覆盖不到这里。
    //
    // 残留的实际危害不在磁盘空间，而在**排障干扰**：本项目的诊断高度依赖「看数据目录的
    // 文件清单」（见 CLAUDE.md 的 MSIX 复发速查），散着几个 .tmp 会让人误判成
    // 「落盘正在进行」或「上次写坏了」。且若失败发生在部分写入之后，残留文件会含上一次
    // 配置的完整内容（base_url、映射等；密钥不在 config 里）。
    if let Err(e) = std::fs::write(&tmp, data) {
        let _ = std::fs::remove_file(&tmp);
        return Err(ctx("写临时文件", &e));
    }

    // 跨设备移动：确定性失败，重试无意义，直接回退原地写。
    //
    // **错误码必须按平台取**。原先无条件用 17 —— 那是 Windows 的 ERROR_NOT_SAME_DEVICE，
    // 而 Unix 上 17 是 `EEXIST`、跨设备是 `EXDEV`(18)。照搬同时踩两个坑：
    //   1. 真正的 EXDEV 不被识别 → 白白重试 6 次（约 375ms）才落到回退，纯浪费；
    //   2. 万一 rename 真返回 EEXIST，会被误判成「跨设备」而跳过重试 —— 判定张冠李戴。
    //
    // Unix 侧用 `libc::EXDEV` 而不是硬编码 18：本项目对「按错误码分流」一贯要求取自
    // 权威定义而非记忆中的数值（同 Cargo.toml 里 security-framework-sys 那条的理由）。
    // libc 本就在依赖树里（0.2.x，经 tokio/rusqlite 等间接引入），加显式依赖零新增 crate。
    let cross_device = cross_device_errno();
    // ERROR_ACCESS_DENIED(5) / ERROR_SHARING_VIOLATION(32)：杀软/并发瞬时锁，值得短暂重试。
    let mut delay_ms = 5u64;
    for attempt in 0..6 {
        match std::fs::rename(&tmp, path) {
            Ok(()) => return Ok(()),
            Err(e) => {
                if e.raw_os_error() == Some(cross_device) {
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

    /// 「跨设备」错误码必须按平台取，不能是一个写死的数值。
    ///
    /// 这条护栏防的是**回退式简化**：`cross_device_errno()` 看起来只是包了一个常量，
    /// 很容易被后来者「顺手」改回 `let cross_device = 17;`。而那个 17 在 Unix 上是
    /// `EEXIST`，不是跨设备 —— 两个平台的值本就不同，此处钉住这个不等式。
    ///
    /// 为什么值得单独一条测试：错的后果不报错、不 panic，只表现为落盘偶尔慢 375ms
    /// （真 EXDEV 被白白重试 6 次），或把 EEXIST 误判成跨设备而跳过本该做的重试。
    /// 这类「静默退化」正是本项目反复防的形态。
    #[test]
    fn cross_device_errno_is_platform_specific() {
        let got = cross_device_errno();

        #[cfg(windows)]
        assert_eq!(got, 17, "Windows 上应为 ERROR_NOT_SAME_DEVICE");

        #[cfg(not(windows))]
        {
            assert_eq!(got, libc::EXDEV, "Unix 上应为 EXDEV");
            assert_eq!(got, 18, "EXDEV 在 Linux/macOS 上均为 18（值变了说明平台假设需重核）");
            assert_ne!(
                got, 17,
                "17 是 Unix 的 EEXIST —— 若这里等于 17，说明有人把平台门控改回了硬编码"
            );
        }
    }

    /// P2-6：DPAPI 解密缓存的**全部失效点**。
    ///
    /// 这条测试是这项优化的全部风险所在：漏掉任一失效点，症状就是历史上出现过的
    /// 「明明更新过密钥却仍报鉴权失败」——不报错、不 panic，只是持续用错密钥。
    ///
    /// 逐个覆盖：`set`（更新同一条）/ `remove` / `lock`（**必须清**，否则「立即锁定」名不副实）
    /// / 三个整库迁移 / `unlock`。
    #[test]
    fn os_cache_invalidates_on_every_mutation() {
        let dir = temp_dir("cache_invalidate");
        let mut s = SecretStore::load(dir.join("secrets.enc")).unwrap();

        // 填充缓存
        s.set("k1", "sk-v1").unwrap();
        assert_eq!(s.get_caching("k1").unwrap().as_deref().map(String::as_str), Some("sk-v1"));
        assert!(s.is_cached("k1"), "get_caching 应已填充缓存（DPAPI 模式）");

        // ---- 失效点 1：set 更新同一条 ----
        s.set("k1", "sk-v2").unwrap();
        assert!(!s.is_cached("k1"), "set 后缓存必须失效");
        assert_eq!(
            s.get("k1").unwrap().as_deref().map(String::as_str),
            Some("sk-v2"),
            "更新后必须读到新值——漏清缓存就是「改了密钥仍鉴权失败」那个历史症状"
        );

        // ---- 失效点 2：remove ----
        s.get_caching("k1").unwrap();
        assert!(s.is_cached("k1"));
        s.remove("k1").unwrap();
        assert!(!s.is_cached("k1"), "remove 后缓存必须失效");
        assert!(
            s.get("k1").unwrap().is_none(),
            "已删除的 Key 不得还能从缓存里读出明文"
        );

        // ---- 失效点 3：lock ----
        s.set("k2", "sk-x").unwrap();
        s.get_caching("k2").unwrap();
        assert!(s.is_cached("k2"));
        s.lock();
        assert!(
            !s.is_cached("k2"),
            "lock 必须清缓存，否则「立即锁定」名不副实（已解出的明文仍能被读到）"
        );

        // ---- 失效点 4~6：三个整库迁移 ----
        s.set("k3", "sk-y").unwrap();
        s.get_caching("k3").unwrap();
        assert!(s.is_cached("k3"));
        s.enable_master_password("pw-123456").unwrap();
        assert!(!s.is_cached("k3"), "启用主口令（整库重写）必须清缓存");

        s.change_master_password("pw-123456", "pw-abcdef").unwrap();
        assert!(!s.is_cached("k3"), "改主口令（整库重写）必须清缓存");

        s.disable_master_password("pw-abcdef").unwrap();
        assert!(!s.is_cached("k3"), "关闭主口令（整库重写）必须清缓存");
        // 迁移往返后值必须完好
        assert_eq!(
            s.get("k3").unwrap().as_deref().map(String::as_str),
            Some("sk-y"),
            "三次整库迁移往返后明文必须完好"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// P2-6：主口令模式**不使用**缓存（它有长驻 vault_key，本就不慢），
    /// 且锁定态 `get` 仍返回 `Err` 而非从缓存兜底。
    #[test]
    fn master_mode_does_not_cache_and_locked_still_errs() {
        let dir = temp_dir("cache_master_mode");
        let mut s = SecretStore::load(dir.join("secrets.enc")).unwrap();
        s.set("k1", "sk-secret").unwrap();
        s.enable_master_password("pw-123456").unwrap();

        // 主口令模式：get_caching 不应写缓存
        assert_eq!(
            s.get_caching("k1").unwrap().as_deref().map(String::as_str),
            Some("sk-secret")
        );
        assert!(
            !s.is_cached("k1"),
            "主口令模式不该缓存明文（已有长驻密钥，缓存只是多一份无谓驻留）"
        );

        // 锁定后 get 必须返 Err（这条刻意行为不能被缓存绕过）
        s.lock();
        assert!(
            s.get("k1").is_err(),
            "锁定态必须返 Err 而非 Ok(None) 或从缓存兜底——调用方靠这个区分「要解锁」与「没配密钥」"
        );

        std::fs::remove_dir_all(&dir).ok();
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
        assert_eq!(store.get("k1").unwrap().as_deref().map(String::as_str), Some("sk-abc"), "DPAPI 加解密应可逆");
        // 无 corrupt 备份产生
        let has_bak = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| e.file_name().to_string_lossy().contains("corrupt"));
        assert!(!has_bak, "正常路径不应产生 corrupt 备份");

        std::fs::remove_dir_all(&dir).ok();
    }

    // ---- 主口令增强模式（FR-018 可选增强）----

    /// 启用主口令：既有 DPAPI 密钥必须全部迁移过去、仍能读出原值，且落盘后重开也能解锁读到。
    #[test]
    fn enabling_master_password_migrates_existing_secrets_and_survives_reload() {
        let dir = temp_dir("master_on");
        let path = dir.join("secrets.enc");
        let mut store = SecretStore::load(path.clone()).unwrap();
        store.set("k1", "sk-one").unwrap();
        store.set("k2", "sk-two").unwrap();
        assert!(!store.is_master_mode(), "默认必须是 DPAPI 模式");

        let migrated = store.enable_master_password("correct horse battery").unwrap();
        assert_eq!(migrated, 2, "两条既有密钥都要迁移，漏一条就是永久丢失");
        assert!(store.is_master_mode());
        assert!(!store.is_locked(), "启用后应处于已解锁态（刚输过口令）");
        assert_eq!(store.get("k1").unwrap().as_deref().map(String::as_str), Some("sk-one"));
        assert_eq!(store.get("k2").unwrap().as_deref().map(String::as_str), Some("sk-two"));

        // 落盘内容检查：不得残留 DPAPI 密文，否则关闭模式时会两处都有、读到哪条看分支顺序。
        let v: SecretVault = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(v.entries.is_empty(), "迁移后不得残留 DPAPI 密文");
        assert_eq!(v.boxes.len(), 2);
        assert!(v.master.is_some(), "master 头部是模式的事实来源，必须落盘");

        // 重开进程：应识别为主口令模式且处于锁定态，解锁后才读得到。
        let mut reopened = SecretStore::load(path.clone()).unwrap();
        assert!(reopened.is_master_mode());
        assert!(reopened.is_locked(), "新进程必须重新解锁");
        assert!(reopened.get("k1").is_err(), "锁定态取密钥必须报错而非返回 None");
        reopened.unlock("correct horse battery").unwrap();
        assert_eq!(reopened.get("k1").unwrap().as_deref().map(String::as_str), Some("sk-one"));

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 锁定态读密钥必须返回 **Err** 而不是 `Ok(None)`。
    ///
    /// 这个区分是整个模式最容易写错、后果最隐蔽的一处：`Ok(None)` 会让调用方（转发、
    /// has_secret 对账、导出）都当成「这个 Key 没配密钥」，于是提示用户去重录密钥，
    /// 而实际只是没解锁。has_secret 对账更糟——会把全部标记刷成 false 写盘。
    #[test]
    fn locked_vault_returns_error_not_absent() {
        let dir = temp_dir("master_locked");
        let path = dir.join("secrets.enc");
        let mut store = SecretStore::load(path.clone()).unwrap();
        store.set("k1", "sk-x").unwrap();
        store.enable_master_password("pw").unwrap();
        drop(store);

        let store = SecretStore::load(path).unwrap();
        let err = store.get("k1").unwrap_err().to_string();
        assert!(err.contains("解锁"), "错误必须告诉用户去解锁，而不是含糊的失败: {err}");
        // 连「库里根本没有的 id」也报错：锁定态下「没有」这个结论本身不可信。
        assert!(store.get("nonexistent").is_err());

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 锁定态写密钥必须被拒绝，不得偷偷回退成 DPAPI 加密。
    /// 否则库里同时存在两种密文，而用户以为已全部由口令保护。
    #[test]
    fn locked_vault_refuses_writes_instead_of_falling_back_to_dpapi() {
        let dir = temp_dir("master_locked_write");
        let path = dir.join("secrets.enc");
        let mut store = SecretStore::load(path.clone()).unwrap();
        store.enable_master_password("pw").unwrap();
        drop(store);

        let mut store = SecretStore::load(path.clone()).unwrap();
        assert!(store.set("k_new", "sk-new").is_err(), "锁定态不得写入");
        // 磁盘上不应出现 DPAPI 密文
        let v: SecretVault = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(v.entries.is_empty(), "绝不能回退成 DPAPI 加密（会造出两种密文并存）");
        assert!(v.boxes.is_empty());

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 错误口令解锁：必须失败，且**不得**把已解锁状态锁回去。
    #[test]
    fn wrong_password_fails_unlock_without_clobbering_unlocked_state() {
        let dir = temp_dir("master_wrong_pw");
        let path = dir.join("secrets.enc");
        let mut store = SecretStore::load(path).unwrap();
        store.set("k1", "sk-x").unwrap();
        store.enable_master_password("right-pw").unwrap();
        assert!(!store.is_locked());

        let err = store.unlock("wrong-pw").unwrap_err().to_string();
        assert!(err.contains("主口令错误"), "{err}");
        assert!(!store.is_locked(), "输错口令不该把已解锁的库锁回去");
        assert_eq!(store.get("k1").unwrap().as_deref().map(String::as_str), Some("sk-x"), "仍应可读");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 空库也能正确判定口令对错 —— 这是校验串（verifier）存在的唯一理由。
    /// 没有它就只能「试解某条密钥」，而空库无从试，会把错口令当成解锁成功。
    #[test]
    fn empty_vault_still_validates_password_via_verifier() {
        let dir = temp_dir("master_empty");
        let path = dir.join("secrets.enc");
        let mut store = SecretStore::load(path.clone()).unwrap();
        assert_eq!(store.enable_master_password("pw-empty").unwrap(), 0, "空库迁移 0 条");
        drop(store);

        let mut store = SecretStore::load(path).unwrap();
        assert!(store.unlock("not-it").is_err(), "空库也必须能识别错口令");
        assert!(store.unlock("pw-empty").is_ok());

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 关闭主口令：全部密钥改回 DPAPI，且必须输对当前口令才能关。
    #[test]
    fn disabling_master_password_requires_password_and_migrates_back() {
        let dir = temp_dir("master_off");
        let path = dir.join("secrets.enc");
        let mut store = SecretStore::load(path.clone()).unwrap();
        store.set("k1", "sk-one").unwrap();
        store.enable_master_password("pw").unwrap();

        // 错口令不得关闭（防止有人趁已解锁的机器直接撤掉保护）。
        assert!(store.disable_master_password("wrong").is_err());
        assert!(store.is_master_mode(), "关闭失败后模式不变");

        assert_eq!(store.disable_master_password("pw").unwrap(), 1);
        assert!(!store.is_master_mode());
        assert!(!store.is_locked(), "DPAPI 模式恒非锁定");
        assert_eq!(store.get("k1").unwrap().as_deref().map(String::as_str), Some("sk-one"), "密钥必须完好");

        let v: SecretVault = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(v.master.is_none() && v.boxes.is_empty(), "口令模式的痕迹要清干净");
        assert_eq!(v.entries.len(), 1);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 修改主口令：旧口令验证 + 换新盐重新封装；旧口令随即失效、新口令可用。
    #[test]
    fn changing_master_password_rotates_salt_and_invalidates_old() {
        let dir = temp_dir("master_change");
        let path = dir.join("secrets.enc");
        let mut store = SecretStore::load(path.clone()).unwrap();
        store.set("k1", "sk-one").unwrap();
        store.enable_master_password("old-pw").unwrap();
        let old_salt = {
            let v: SecretVault = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            v.master.unwrap().kdf.salt
        };

        assert!(store.change_master_password("wrong", "new-pw").is_err(), "旧口令要验");
        assert_eq!(store.change_master_password("old-pw", "new-pw").unwrap(), 1);

        let new_salt = {
            let v: SecretVault = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            v.master.unwrap().kdf.salt
        };
        assert_ne!(old_salt, new_salt, "换口令必须换盐，否则新旧密钥可比对");

        let mut reopened = SecretStore::load(path).unwrap();
        assert!(reopened.unlock("old-pw").is_err(), "旧口令必须立即失效");
        reopened.unlock("new-pw").unwrap();
        assert_eq!(reopened.get("k1").unwrap().as_deref().map(String::as_str), Some("sk-one"));

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 整份重写前必须留备份（唯一一次全量重写密文的操作，写坏就全没了）。
    #[test]
    fn mode_switch_backs_up_vault_before_rewriting() {
        let dir = temp_dir("master_backup");
        let path = dir.join("secrets.enc");
        let mut store = SecretStore::load(path.clone()).unwrap();
        store.set("k1", "sk-one").unwrap();
        let before = std::fs::read(&path).unwrap();

        store.enable_master_password("pw").unwrap();

        let bak = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .find(|e| e.file_name().to_string_lossy().contains("master-on-"))
            .expect("启用主口令前必须备份密钥库");
        assert_eq!(std::fs::read(bak.path()).unwrap(), before, "备份必须是切换前的原字节");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 降级态（本次启动读盘失败）下禁止切换模式：
    /// 那时 entries 是空的，迁移等于「把用户全部密钥丢掉，还声称已加密」。
    #[test]
    fn degraded_vault_refuses_mode_switch() {
        let dir = temp_dir("master_degraded");
        let path = dir.join("secrets.enc");
        std::fs::write(&path, b"{ corrupted but holds user ciphertexts }").unwrap();

        let mut store = SecretStore::load(path).unwrap();
        assert!(store.load_failed, "前置条件：应处于降级态");
        let err = store.enable_master_password("pw").unwrap_err().to_string();
        assert!(err.contains("降级"), "要说清原因并给出行动指引: {err}");
        assert!(!store.is_master_mode(), "拒绝后模式不变");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 重复启用要报错而不是重新生成盐 —— 后者会让旧密文全部解不开。
    #[test]
    fn enabling_twice_is_rejected() {
        let dir = temp_dir("master_twice");
        let mut store = SecretStore::load(dir.join("secrets.enc")).unwrap();
        store.enable_master_password("pw").unwrap();
        assert!(store.enable_master_password("pw2").is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 非主口令模式下调解锁/关闭/改口令都应给出明确错误，而不是静默成功。
    #[test]
    fn master_operations_rejected_in_dpapi_mode() {
        let dir = temp_dir("master_dpapi_ops");
        let mut store = SecretStore::load(dir.join("secrets.enc")).unwrap();
        assert!(store.unlock("pw").is_err());
        assert!(store.disable_master_password("pw").is_err());
        assert!(store.change_master_password("a", "b").is_err());
        assert!(store.enable_master_password("").is_err(), "空口令等于没加密");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 手动上锁后立刻不可读，再解锁恢复。用于「离开电脑前锁定」。
    #[test]
    fn manual_lock_takes_effect_immediately() {
        let dir = temp_dir("master_manual_lock");
        let mut store = SecretStore::load(dir.join("secrets.enc")).unwrap();
        store.set("k1", "sk-x").unwrap();
        store.enable_master_password("pw").unwrap();
        assert!(store.get("k1").is_ok());

        store.lock();
        assert!(store.is_locked());
        assert!(store.get("k1").is_err());

        store.unlock("pw").unwrap();
        assert_eq!(store.get("k1").unwrap().as_deref().map(String::as_str), Some("sk-x"));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 删除 Key 时两种密文都要清 —— 模式切换中断时同 id 可能两处都有，
    /// 只删一个会留下能被反向模式读出的残留。
    #[test]
    fn remove_clears_both_cipher_maps() {
        let dir = temp_dir("master_remove");
        let path = dir.join("secrets.enc");
        let mut store = SecretStore::load(path.clone()).unwrap();
        store.set("k1", "sk-x").unwrap();
        store.enable_master_password("pw").unwrap();
        // 人为造出「两处都有」的中断态
        store.vault.entries.insert("k1".into(), "stale-dpapi".into());
        store.remove("k1").unwrap();

        let v: SecretVault = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(v.entries.is_empty() && v.boxes.is_empty(), "两个 map 都要清");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// `set` 必须清掉**另一个 map** 里的同 id 残留（双向）。
    ///
    /// 不清的后果不是「多占空间」，而是**读出过期密钥**：`get` 按当前模式单边读，切模式后会
    /// 拿到残留的那条旧密文 —— 用户明明更新过密钥，转发却仍用改之前那条、继续报鉴权失败，
    /// 且从 UI 上完全看不出原因。
    #[test]
    fn set_clears_stale_cipher_in_the_other_map_both_directions() {
        let dir = temp_dir("set_cross_clear");
        let path = dir.join("secrets.enc");
        let mut store = SecretStore::load(path.clone()).unwrap();

        // 方向一：主口令模式下写入，应清掉 entries 里的 DPAPI 残留。
        store.enable_master_password("pw").unwrap();
        store.vault.entries.insert("k1".into(), "stale-dpapi".into());
        store.set("k1", "sk-new").unwrap();
        assert!(
            !store.vault.entries.contains_key("k1"),
            "主口令模式写入后不得留 DPAPI 残留"
        );
        assert_eq!(store.get("k1").unwrap().as_deref().map(String::as_str), Some("sk-new"));

        // 方向二：关回 DPAPI 模式后写入，应清掉 boxes 里的口令密文残留。
        store.disable_master_password("pw").unwrap();
        // 造残留：直接塞一条 boxes（模拟上一次迁移中断留下的条目）
        store.vault.boxes.insert(
            "k1".into(),
            crate::crypto::SealedBox { nonce: "AAAA".into(), ct: "BBBB".into() },
        );
        store.set("k1", "sk-dpapi").unwrap();
        assert!(
            !store.vault.boxes.contains_key("k1"),
            "DPAPI 模式写入后不得留口令密文残留（否则再启用主口令会迁移到过期密钥）"
        );
        assert_eq!(store.get("k1").unwrap().as_deref().map(String::as_str), Some("sk-dpapi"));

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 迁移必须覆盖**两个 map 的并集**，不能按当前模式单边读。
    ///
    /// 场景：上一次迁移中断，库里同时有 DPAPI 密文与口令密文（不同 id）。此时启用主口令，
    /// 若 `all_key_ids` 只读 `entries`，`boxes` 里那条会被静默漏掉 —— 迁移完成后它用的还是
    /// 上一把口令派生的密钥，新口令解不开、DPAPI 也解不开，**永久丢失且无任何提示**。
    ///
    /// 取并集后最坏情况是「多试解一条、失败即整体放弃」，不会丢数据。
    #[test]
    fn migration_covers_union_of_both_maps_not_just_current_mode() {
        let dir = temp_dir("union_ids");
        let path = dir.join("secrets.enc");
        let mut store = SecretStore::load(path.clone()).unwrap();

        // 先在 DPAPI 模式存一条，再人为塞一条**上一把口令**加密的 boxes 条目（中断态）。
        store.set("dpapi-one", "sk-dpapi").unwrap();
        let old_hdr = crate::crypto::KdfHeader::new_random();
        let old_key = crate::crypto::derive_vault_key("old-pw", &old_hdr).unwrap();
        store.vault.boxes.insert(
            "orphan-boxed".into(),
            crate::crypto::seal_with_key(&old_key, b"sk-orphan").unwrap(),
        );

        // all_key_ids 必须同时看到两条（这是并集的直接断言）。
        let ids = store.all_key_ids();
        assert!(ids.iter().any(|x| x == "dpapi-one"), "{ids:?}");
        assert!(
            ids.iter().any(|x| x == "orphan-boxed"),
            "另一个 map 里的条目不得被漏掉: {ids:?}"
        );

        // 启用主口令：那条孤儿用旧口令加密、当前模式（DPAPI）解不出 → 必须**整体放弃**并保持
        // 密钥库原样，而不是「跳过它、迁移剩下的」（后者等于静默丢弃）。
        let err = store.enable_master_password("new-pw").unwrap_err().to_string();
        assert!(
            err.contains("orphan-boxed") || err.contains("解密不出来"),
            "应明确指出是哪条解不出并放弃切换: {err}"
        );
        assert!(!store.is_master_mode(), "失败后不得留在主口令模式");
        assert_eq!(
            store.get("dpapi-one").unwrap().as_deref().map(String::as_str),
            Some("sk-dpapi"),
            "放弃切换后原有密钥必须仍可读"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
