//! 配置导入 / 导出（FR-021）。
//!
//! ## 为什么不是「把 config.json 拷出来」这么简单
//!
//! 三个必须处理的问题，任一漏掉都会让导出文件在新机器上「导成了但不可用」：
//!
//! 1. **密钥不能照搬**。`secrets.enc` 走 Windows DPAPI，密文绑**当前 Windows 账户**，
//!    换机/换账户一律解不出。所以「含密钥导出」必须先用 DPAPI 解出明文、再用**用户口令**
//!    重新加密（[`crate::crypto`]）。这也是 FR-021「导出时密钥需保持加密或提供是否包含密钥的
//!    选项」的唯一可行解法。
//! 2. **本机绑定字段不能带走**。端口、日志目录、MCP 已注册分类这些是「这台机器的运行状态」，
//!    带到新机器上会造成端口冲突、日志写到不存在的盘、或声称已注册实则没写过客户端配置。
//!    见 [`strip_machine_local`]。
//! 3. **完整性要能校验**。FR-021 验收要求「导入时校验版本与完整性」。故导出体带
//!    `formatVersion` + 载荷的 `sha256`；导入时两者都验，损坏文件在**改动任何东西之前**就被拒。
//!
//! ## 导入的两种模式
//!
//! 用户当场选（`ImportMode`）：
//! - `Merge`：同 id 覆盖、新 id 新增、本机多出的保留。不会删任何东西，适合「把另一台机器的
//!   Key 并过来」。
//! - `Replace`：先清空同类实体再导入，语义是「还原到导出那一刻」。**会删掉导出后新建的 Key**，
//!   故执行前强制备份现有 config（`.pre-import-<时间戳>.json`）。

use crate::crypto;
use crate::error::{AppError, AppResult};
use crate::model::{AppSettings, BrainConfig, ProviderKey, Vendor};
use crate::store::Store;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// 导出文件格式版本。**改动载荷结构时必须 +1**，并在 [`check_format_version`] 里明确拒绝
/// 或做迁移——静默按新结构解析旧文件会得到半份配置。
const FORMAT_VERSION: u32 = 1;

/// 导出文件的顶层结构。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportFile {
    /// 格式版本（见 [`FORMAT_VERSION`]）
    pub format_version: u32,
    /// 产出该文件的 SynaRoute 版本（仅供人看与排障，不参与校验——同格式版本跨应用版本应可互导）
    pub app_version: String,
    /// 导出时间（ISO8601）
    pub exported_at: String,
    /// 载荷的 sha256（hex）。校验的是 `serde_json::to_vec(&payload)` 的字节。
    pub payload_sha256: String,
    /// 配置本体（不含密钥）
    pub payload: ExportPayload,
    /// 密钥段：口令加密的信封。不含密钥导出时为 None。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secrets: Option<crypto::Envelope>,
}

/// 导出的配置本体。**刻意不直接复用 `AppConfig`**：那样日后给 AppConfig 加字段会静默改变
/// 导出格式（且旧版本导入时按 `#[serde(default)]` 吞掉），这里显式列出反而让格式变化可见。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportPayload {
    pub keys: Vec<ProviderKey>,
    pub brain: Vec<BrainConfig>,
    pub vendors: Vec<Vendor>,
    pub settings: AppSettings,
}

/// 密钥段解密后的明文结构：keyId → 明文密钥。
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SecretsPlain {
    entries: std::collections::HashMap<String, String>,
}

/// 导入模式（用户当场选，见模块注释）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ImportMode {
    /// 同 id 覆盖、新 id 新增、本机多出的保留（不删任何东西）
    Merge,
    /// 清空后导入，还原到导出那一刻（会删掉导出后新建的条目）
    Replace,
}

/// 导入前的只读预检结果：让用户在**真正写盘之前**看到会发生什么。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportPreview {
    pub format_version: u32,
    pub app_version: String,
    pub exported_at: String,
    /// 文件里有多少条 Key
    pub key_count: usize,
    /// 其中与本机 id 重复的（Merge 会覆盖它们）
    pub conflicting_keys: usize,
    /// **可疑冲突**：id 相同但 name 与 base_url 都不一样的条目（P3-5）。
    ///
    /// 为什么要单列出来：历史上 Key id 是 `k_<毫秒时间戳>`，跨机会碰撞
    /// （「两台机器照同一份教程配置」是真实场景）。撞号时导入会把一条**完全无关**的本机
    /// Key 静默覆盖成对方的 base_url / 协议 / 映射，而它在 `conflicting_keys` 里只是一个
    /// 计数，与「同一条 Key 的正常更新」看不出区别。新建 Key 已改用 uuid v4，但**历史 id
    /// 不迁移**（它们是 secrets.enc 的键名），故这条防线要长期留着。
    ///
    /// 逐条给出「本机那条 → 文件那条」的名字，让用户在确认前就能看见要被换成什么。
    pub suspicious_conflicts: Vec<SuspiciousConflict>,
    /// 本机独有、Replace 模式会被删掉的 Key 数
    pub local_only_keys: usize,
    pub vendor_count: usize,
    pub brain_count: usize,
    /// 文件是否含密钥段（含则导入时需要口令）
    pub has_secrets: bool,
}

/// 一处「id 相同但看起来根本不是同一条 Key」的冲突。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SuspiciousConflict {
    pub id: String,
    /// 本机这条的名字与 base_url（将被覆盖掉的一方）
    pub local_name: String,
    pub local_base_url: String,
    /// 文件里这条的名字与 base_url（覆盖方）
    pub incoming_name: String,
    pub incoming_base_url: String,
}

/// 导入结果报告。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    pub mode: ImportMode,
    pub keys_added: usize,
    pub keys_overwritten: usize,
    pub keys_removed: usize,
    pub vendors_imported: usize,
    pub brain_imported: usize,
    pub secrets_imported: usize,
    /// 随被移除 Key 一并清理掉的旧密钥条数（P2-3，仅 Replace 模式非零）。
    ///
    /// 要报告给用户：密钥是敏感材料，「删掉了几条」应当明说，而不是无声发生。
    #[serde(default)]
    pub secrets_pruned: usize,
    /// Replace 模式下导入前备份的 config 路径（Merge 模式为 None）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backup_path: Option<String>,
    /// Replace 模式下、真的要删旧密钥时备份的 secrets.enc 路径。
    ///
    /// 必须与 `backup_path` 并列给到用户：只回滚 config.json 会得到「Key 都回来了、
    /// 密钥没了」的半截状态，两个文件要一起还原才是真正的回滚。
    /// 没有 Key 被移除、或库文件本就不存在时为 None。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secrets_backup_path: Option<String>,
    /// 非致命提示（如「某 Key 有密钥标记但文件里没有对应密钥」）
    pub warnings: Vec<String>,
}

/// 计算载荷的 sha256（hex 小写）。
fn payload_digest(payload: &ExportPayload) -> AppResult<String> {
    let bytes = serde_json::to_vec(payload)?;
    let mut h = Sha256::new();
    h.update(&bytes);
    Ok(format!("{:x}", h.finalize()))
}

/// 剔除「只对本机有意义」的设置字段。
///
/// 为什么必须剔：这些字段带到新机器上不是「没用」而是**有害**——
/// - `proxy_ports` / `mcp_port`：新机器上那些端口可能被别的软件占着，导入后代理起不来，
///   而用户以为是配置问题。留空让新机器走默认端口 + 占用回退逻辑。
/// - `log_dir`：指向源机器的绝对路径（可能是不存在的盘符），日志会写失败或落到怪地方。
/// - `mcp_registered_categories`：这是「已往客户端配置文件里写过 MCP」的记录。新机器上没写过，
///   带过去会让端口漂移时的重写逻辑去改不存在的注册、且 UI 误显示已接入。
/// - `mcp_enabled`：MCP 服务器要不要起是本机运行决策；带过去可能在新机器上抢占端口。
/// - `auto_start`：自启动项注册在源机器的注册表里，配置带过去但系统侧没有，
///   会出现「开关开着但不生效」——正是我们刚修掉的那类不一致。故导入后由用户重新开。
fn strip_machine_local(mut s: AppSettings) -> AppSettings {
    s.proxy_ports.clear();
    s.mcp_registered_categories.clear();
    s.mcp_port = crate::model::AppSettings::default().mcp_port;
    s.mcp_enabled = false;
    s.log_dir = None;
    s.auto_start = false;
    s
}

/// 构造导出文件。`password` 为 Some 时导出密钥段（口令加密）。
///
/// 密钥段的明文只在本函数内存活：从当前保护模式解出 → 立即用口令重新封装 → 明文随作用域结束丢弃。
///
/// 返回值第二项是**解不出来的 Key 数**：调用方要把它告知用户。解不出的条目会被跳过
/// （见下方理由），若不上报，用户会拿到一个「声称含密钥、实际少了几条」的文件，
/// 到新机器导入后才发现 —— 那时已经离开源机器、无从补救。
pub fn build_export(
    store: &Store,
    app_version: &str,
    password: Option<&str>,
) -> AppResult<(ExportFile, usize)> {
    let cfg = store.snapshot_config();
    let payload = ExportPayload {
        keys: cfg.keys.clone(),
        brain: cfg.brain.clone(),
        vendors: cfg.vendors.clone(),
        settings: strip_machine_local(cfg.settings.clone()),
    };
    let payload_sha256 = payload_digest(&payload)?;

    let mut undecryptable = 0usize;
    let secrets = match password {
        Some(pw) => {
            // 主口令模式未解锁时一条也解不出来，导出的会是「含密钥」但实际为空的文件——
            // 用户在新机器导入后每个 Key 都报缺密钥，且完全看不出是导出时就没带上。
            // 故直接拒绝，让用户先解锁。
            if store.secrets.read().is_locked() {
                return Err(AppError::Invalid(
                    "密钥库已用主口令加密但尚未解锁，无法导出密钥。请先解锁，或选择「不含密钥」导出。"
                        .into(),
                ));
            }
            // 逐个 Key 从当前保护模式解出明文。解不出的**跳过而非中断**：某个 Key 的密文可能因
            // 换过 Windows 账户而失效，不该因此让整次导出失败（导出元数据仍有价值）。
            // 但**必须计数并上报**——见函数文档。
            let mut entries = std::collections::HashMap::new();
            let guard = store.secrets.read();
            for k in &cfg.keys {
                match guard.get(&k.id) {
                    Ok(Some(plain)) => {
                        // 解出 Zeroizing 存进待序列化的 map：这份内容马上要被 crypto::seal
                        // 加密进导出文件，必须是普通 String 才能过 serde。
                        entries.insert(k.id.clone(), plain.to_string());
                    }
                    // `Ok(None)` = 这个 Key 本来就没配密钥，不算「解不出」。
                    Ok(None) => {}
                    Err(e) => {
                        undecryptable += 1;
                        tracing::warn!("导出时 Key {} 的密钥解密失败，已跳过: {e}", k.id);
                    }
                }
            }
            drop(guard);
            let plain = serde_json::to_vec(&SecretsPlain { entries })?;
            Some(crypto::seal(pw, &plain)?)
        }
        None => None,
    };

    Ok((
        ExportFile {
            format_version: FORMAT_VERSION,
            app_version: app_version.to_string(),
            exported_at: chrono::Utc::now().to_rfc3339(),
            payload_sha256,
            payload,
            secrets,
        },
        undecryptable,
    ))
}

/// 校验格式版本。**不认识的版本一律拒绝**，不尝试「尽力解析」——半份配置比明确报错更难排查。
fn check_format_version(v: u32) -> AppResult<()> {
    if v == FORMAT_VERSION {
        return Ok(());
    }
    Err(AppError::Invalid(if v > FORMAT_VERSION {
        format!(
            "该导出文件来自更新版本的 SynaRoute（格式 v{v}，本程序支持 v{FORMAT_VERSION}）。\
             请升级 SynaRoute 后再导入。"
        )
    } else {
        format!(
            "该导出文件格式过旧（v{v}，本程序支持 v{FORMAT_VERSION}），无法导入。"
        )
    }))
}

/// 解析 + 校验导出文件（版本 + 完整性）。**任何写盘动作之前**都要先过这一关。
pub fn parse_and_verify(raw: &[u8]) -> AppResult<ExportFile> {
    let file: ExportFile = serde_json::from_slice(raw)
        .map_err(|e| AppError::Invalid(format!("不是有效的 SynaRoute 导出文件: {e}")))?;
    check_format_version(file.format_version)?;
    let actual = payload_digest(&file.payload)?;
    if actual != file.payload_sha256 {
        return Err(AppError::Invalid(
            "导出文件校验和不匹配：文件已损坏或被修改，已拒绝导入（未改动任何本机配置）。".into(),
        ));
    }
    Ok(file)
}

/// 导入前预检：只读，不改任何东西。
pub fn preview_import(store: &Store, file: &ExportFile) -> ImportPreview {
    let local = store.snapshot_config();
    let local_ids: std::collections::HashSet<&str> =
        local.keys.iter().map(|k| k.id.as_str()).collect();
    let file_ids: std::collections::HashSet<&str> =
        file.payload.keys.iter().map(|k| k.id.as_str()).collect();

    // 可疑冲突（P3-5）：id 撞了，但 name 与 base_url **都**不同 → 极可能是时间戳 id 碰撞，
    // 而不是同一条 Key 的正常更新。判据要求两者都不同：只改名字（同一上游换个称呼）或
    // 只改 base_url（中转商换域名）都是常见的正常更新，单独一项不同不该报警。
    let suspicious_conflicts: Vec<SuspiciousConflict> = file
        .payload
        .keys
        .iter()
        .filter_map(|inc| {
            let loc = local.keys.iter().find(|k| k.id == inc.id)?;
            let name_differs = loc.name.trim() != inc.name.trim();
            let url_differs = loc.base_url.trim_end_matches('/') != inc.base_url.trim_end_matches('/');
            (name_differs && url_differs).then(|| SuspiciousConflict {
                id: inc.id.clone(),
                local_name: loc.name.clone(),
                local_base_url: loc.base_url.clone(),
                incoming_name: inc.name.clone(),
                incoming_base_url: inc.base_url.clone(),
            })
        })
        .collect();

    ImportPreview {
        format_version: file.format_version,
        app_version: file.app_version.clone(),
        exported_at: file.exported_at.clone(),
        key_count: file.payload.keys.len(),
        conflicting_keys: file_ids.iter().filter(|id| local_ids.contains(**id)).count(),
        suspicious_conflicts,
        local_only_keys: local_ids.iter().filter(|id| !file_ids.contains(**id)).count(),
        vendor_count: file.payload.vendors.len(),
        brain_count: file.payload.brain.len(),
        has_secrets: file.secrets.is_some(),
    }
}

/// 执行导入。
///
/// `password` 仅在文件含密钥段时需要；给了口令但文件无密钥段时**报错而非静默忽略**——
/// 用户显然以为在导密钥，静默成功会让他以为密钥已导入。
pub fn apply_import(
    store: &Store,
    file: &ExportFile,
    mode: ImportMode,
    password: Option<&str>,
) -> AppResult<ImportReport> {
    let mut warnings = Vec::new();

    // 主口令模式未解锁时，密钥一条也写不进去（`SecretStore::set` 会拒绝）。若照常往下走，
    // 会得到「配置全导入了、密钥全失败」的半成品，而 Replace 模式已经把本机原有条目删了。
    // 故在**动任何东西之前**拒绝。
    if file.secrets.is_some() && store.secrets.read().is_locked() {
        return Err(AppError::Invalid(
            "密钥库已用主口令加密但尚未解锁，无法导入密钥。请先解锁后重试。".into(),
        ));
    }

    // 密钥段先解开（在动配置之前）：口令错就该在什么都没改的时候失败。
    let secrets: Option<SecretsPlain> = match (&file.secrets, password) {
        (Some(env), Some(pw)) => {
            let plain = crypto::open(pw, env)?;
            Some(serde_json::from_slice(&plain).map_err(|e| {
                AppError::Invalid(format!("密钥段解密成功但内容无法解析（文件损坏）: {e}"))
            })?)
        }
        (Some(_), None) => {
            return Err(AppError::Invalid(
                "该文件包含加密的密钥段，请提供导出时设置的口令。".into(),
            ))
        }
        (None, Some(_)) => {
            return Err(AppError::Invalid(
                "该文件不包含密钥段，无需口令。导入后需为各 Key 重新录入密钥。".into(),
            ))
        }
        (None, None) => {
            warnings.push(
                "文件不含密钥：导入后各 Key 需重新录入密钥才能转发（或用「从 cc-switch 导入」补）。"
                    .into(),
            );
            None
        }
    };

    // Replace 模式先备份现有 config —— 它会删掉导出后新建的条目，必须留退路。
    let backup_path = if mode == ImportMode::Replace {
        Some(store.backup_config_before_import()?)
    } else {
        None
    };

    let local_before = store.snapshot_config();
    let local_ids: std::collections::HashSet<String> =
        local_before.keys.iter().map(|k| k.id.clone()).collect();
    let file_ids: std::collections::HashSet<String> =
        file.payload.keys.iter().map(|k| k.id.clone()).collect();

    let keys_overwritten = file_ids.iter().filter(|id| local_ids.contains(*id)).count();
    let keys_added = file.payload.keys.len() - keys_overwritten;
    let keys_removed = if mode == ImportMode::Replace {
        local_ids.iter().filter(|id| !file_ids.contains(*id)).count()
    } else {
        0
    };

    // 配置整体落盘（走 store 的带回滚写路径）。返回 Replace 模式下被移除的 Key id。
    let mut removed_ids = store.apply_imported_config(&file.payload, mode)?;
    // 密钥库备份路径（只有真的要删旧密钥时才产生），随报告一并交给用户。
    let mut secrets_backup: Option<String> = None;

    // 清理被移除 Key 的密钥（P2-3）。**必须在配置落盘成功之后**——反之若配置落盘失败，
    // Key 会在下次启动时复活而密钥已没了（`has_secret=true` 却取不到的孤儿）。
    // 这与 `Store::delete_key` 的时序理由完全相同。
    //
    // 且必须在下面写入新密钥的循环**之前**完成：removed_ids 已排除「载荷里也有的 id」，
    // 故不会误删本次马上要写入的同 id 密钥。
    //
    // 不清理的后果（历史行为）：Replace 导入后那些 Key 的可解密钥材料仍完整留在
    // secrets.enc 里，UI 无入口可见更无法删除；日后导入「含同 id Key 但不含密钥段」的文件时，
    // 对账会读到孤儿密文把 has_secret 刷成 true，得到「新配置 + 旧密钥」的嵌合体 → 莫名 401。
    let mut secrets_pruned = 0usize;
    if !removed_ids.is_empty() {
        let mut guard = store.secrets.write();
        // **删密钥前必须先备份整个 secrets.enc**，且备份不成就一条都不删。
        //
        // 为什么：上面 :372 的 `backup_config_before_import` 只备份了 config.json。
        // 若在此直接 `remove`，`SecretStore::remove` 会立刻整份重写 secrets.enc ——
        // 用户拿着报告里的 backup_path 把 config.json 还原回去，那些 Key 全部复活，
        // 密钥密文却已被覆盖掉，只能去各中转商后台重新取一遍。**配置可回滚而密钥不可回滚，
        // 等于没有回滚**（选错文件做 Replace 导入是很容易发生的误操作）。
        //
        // 备份失败时**跳过清理而不是中止导入**：此刻配置已经落盘、回不去了，中止没有意义；
        // 而残留几条孤儿密钥是无害的（UI 有 `prune_orphan_secrets` 兜底，且它自己也会先备份）。
        // 「没有安全网就不做这个不可逆动作」比「删了再说」正确。
        match guard.backup_before_rewrite("import-replace") {
            Ok(Some(bak)) => secrets_backup = Some(bak.to_string_lossy().into_owned()),
            Ok(None) => {} // 库文件还不存在（本机从没存过密钥），没什么可删也没什么可备份
            Err(e) => {
                warnings.push(format!(
                    "备份密钥库失败，已跳过清理 {} 条被移除 Key 的旧密钥（它们仍留在库中，可稍后用设置页的「清理孤儿密钥」处理）: {e}",
                    removed_ids.len()
                ));
                removed_ids.clear();
            }
        }
        for id in &removed_ids {
            match guard.remove(id) {
                Ok(()) => secrets_pruned += 1,
                // 残留一条孤儿是无害的（可被 prune_orphan_secrets 兜住），不中止导入。
                Err(e) => warnings.push(format!("清理 Key {id} 的旧密钥失败: {e}")),
            }
        }
        drop(guard);
    }

    // 密钥逐条写入（在配置落盘成功之后——否则可能出现「有密钥、没 Key」的孤儿）。
    //
    // 单条失败**只记警告不中止**：此刻配置已经落盘（Replace 模式甚至已删掉本机原有条目），
    // 中止也回不去；把剩下能写的写完、再让对账把 has_secret 修准，比半途而废信息量更大。
    // 用户拿到的报告里会逐条列出哪个 Key 写失败。
    let mut secrets_imported = 0usize;
    if let Some(s) = secrets {
        // 逐条写：`SecretStore::set` 每次都会 persist 一遍 secrets.enc。这里刻意不做批量写——
        // 批量的唯一好处是少几次落盘，代价是「中途失败则整批不知道写进去几条」，
        // 而导入是低频一次性操作，逐条落盘换来的确定性更值。
        let mut guard = store.secrets.write();
        for (key_id, plain) in &s.entries {
            // 只导入「文件里确实有这个 Key」的密钥，避免留下无主密钥。
            if !file_ids.contains(key_id) {
                continue;
            }
            match guard.set(key_id, plain) {
                Ok(()) => secrets_imported += 1,
                Err(e) => warnings.push(format!("Key {key_id} 的密钥写入失败: {e}")),
            }
        }
        drop(guard);
    }

    // has_secret 标记与实际密钥库对账 —— **两条路径都要做**（有无密钥段都要）。
    //
    // 不对账的后果：文件里 Key 带着导出机器上的 `has_secret: true`，而本机密钥库里没有对应密钥
    // → UI 显示「已配置密钥」、转发却报「密钥缺失」，用户完全看不出问题在哪
    // （与 store.rs 反复防的那类「配置与实际不一致且无从察觉」同源）。
    //
    // 注意这是**第二次** `mutate_and_persist`（第一次是上面的 `apply_imported_config`）。
    // 两次之间若有并发写（后台健康检查线程 update_health），对账这次走的是磁盘对账回滚，
    // 不会抹掉并发方已提交的变更；而它只改 `has_secret` 一个字段，与健康态字段无交集。
    let missing = store.reconcile_has_secret_flags()?;
    if missing > 0 {
        warnings.push(format!(
            "{missing} 个 Key 标记了有密钥但库里实际没有，已把标记置回未配置——请重新录入密钥。"
        ));
    }

    Ok(ImportReport {
        mode,
        keys_added,
        keys_overwritten,
        keys_removed,
        vendors_imported: file.payload.vendors.len(),
        brain_imported: file.payload.brain.len(),
        secrets_imported,
        secrets_pruned,
        backup_path,
        secrets_backup_path: secrets_backup,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::UserPrefs;
    use crate::model::{
        AggregateMode, CategoryType, HealthState, KeyParams, ModelInfo, Protocol,
    };

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "synaroute_portable_test_{}_{}_{}",
            tag,
            std::process::id(),
            seq
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn store_at(dir: &std::path::Path) -> Store {
        Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap()
    }

    fn key(id: &str, name: &str) -> ProviderKey {
        ProviderKey {
            id: id.into(),
            category_id: CategoryType::ClaudeCli,
            name: name.into(),
            vendor: "test".into(),
            base_url: "https://api.example.com".into(),
            protocol: Protocol::Anthropic,
            has_secret: false,
            enabled: true,
            priority: 0,
            headers_json: None,
            params: KeyParams::default(),
            models: vec![ModelInfo {
                real_name: "claude-opus-4-8".into(),
                source: "manual".into(),
                fetched_at: None,
                context_window: None,
            }],
            mappings: vec![],
            default_model: None,
            tier_haiku: None,
            tier_sonnet: None,
            tier_opus: None,
            health: HealthState::default(),
        }
    }

    fn vendor(id: &str, builtin: bool) -> Vendor {
        Vendor {
            id: id.into(),
            name: format!("厂商-{id}"),
            default_base_url: "https://api.example.com".into(),
            default_protocol: Protocol::Anthropic,
            builtin,
            icon: None,
            preset_models: vec![],
        }
    }

    /// 端到端：导出（含密钥）→ 新库导入 → Key、厂商、大脑配置与**明文密钥**都回来。
    ///
    /// 这条覆盖 FR-021 的核心验收标准「导出文件可在同版本重新导入并还原配置」，
    /// 且证明密钥走的是口令信封而非 DPAPI 密文（后者换库/换机必然解不出）。
    #[test]
    fn export_with_secrets_roundtrips_into_a_fresh_store() {
        let src_dir = temp_dir("rt_src");
        let dst_dir = temp_dir("rt_dst");
        let src = store_at(&src_dir);

        src.upsert_key(key("k1", "主 Key")).unwrap();
        src.upsert_key(key("k2", "备用 Key")).unwrap();
        src.secrets.write().set("k1", "sk-secret-one").unwrap();
        src.secrets.write().set("k2", "sk-secret-two").unwrap();
        // has_secret 标记与库对账（模拟真实 save_secret 路径的效果）
        src.reconcile_has_secret_flags().unwrap();
        let mut brain = src.get_brain(CategoryType::ClaudeCli);
        brain.enabled = true;
        brain.aggregate_mode = AggregateMode::Full;
        brain.total_timeout_ms = 123_000;
        src.save_brain(brain).unwrap();

        let file = build_export(&src, "9.9.9", Some("导出口令-Aa1!")).unwrap().0;
        // 序列化 → 反序列化，走真实的落盘/读盘路径（而非直接传结构体）
        let raw = serde_json::to_vec_pretty(&file).unwrap();
        let parsed = parse_and_verify(&raw).unwrap();

        let dst = store_at(&dst_dir);
        let report = apply_import(&dst, &parsed, ImportMode::Merge, Some("导出口令-Aa1!")).unwrap();
        assert_eq!(report.keys_added, 2);
        assert_eq!(report.keys_overwritten, 0);
        assert_eq!(report.secrets_imported, 2);

        // 配置回来了
        let keys = dst.list_keys(CategoryType::ClaudeCli);
        assert_eq!(keys.len(), 2);
        assert!(keys.iter().any(|k| k.name == "主 Key"));
        assert_eq!(dst.get_brain(CategoryType::ClaudeCli).total_timeout_ms, 123_000);
        assert!(dst.get_brain(CategoryType::ClaudeCli).enabled);

        // **明文密钥**回来了（这是「能真正迁移」的判据）
        assert_eq!(
            dst.secrets.read().get("k1").unwrap().as_deref().map(String::as_str),
            Some("sk-secret-one")
        );
        assert_eq!(
            dst.secrets.read().get("k2").unwrap().as_deref().map(String::as_str),
            Some("sk-secret-two")
        );
        // has_secret 与库一致，不会出现「UI 说有、转发说没有」
        assert!(
            dst.list_keys(CategoryType::ClaudeCli).iter().all(|k| k.has_secret),
            "has_secret 必须与密钥库一致，否则 UI 说有、转发说没有"
        );

        std::fs::remove_dir_all(&src_dir).ok();
        std::fs::remove_dir_all(&dst_dir).ok();
    }

    /// 导入必须**同时**做「配置落盘」与「has_secret 对账」两步。
    ///
    /// 这条测的是审查时发现的一个易漏点：对账原先只在「有密钥段」的分支里做，
    /// 而**恰恰是无密钥段的场景最需要它** —— 文件里的 Key 带着导出机器上的 `has_secret: true`，
    /// 本机却没有对应密钥，不对账就会「UI 说有、转发说没有」。
    ///
    /// 与上面那条 `export_without_secrets_warns_and_clears_has_secret_flags` 的区别：
    /// 那条走的是「导出侧就没有密钥」，这条构造的是「本机原有 Key 带 has_secret=true 且
    /// 密钥确实在库里，导入文件覆盖它但不带密钥段」—— 覆盖后标记应被修准，而不是沿用文件里的值。
    #[test]
    fn import_without_secrets_reconciles_flags_for_overwritten_keys() {
        let src_dir = temp_dir("recon_src");
        let dst_dir = temp_dir("recon_dst");

        // 源：Key 带密钥、has_secret=true，但导出时**不含**密钥段。
        let src = store_at(&src_dir);
        let mut k = key("shared", "源机器版本");
        k.has_secret = true;
        src.upsert_key(k).unwrap();
        src.secrets.write().set("shared", "sk-src").unwrap();
        let file = build_export(&src, "1.0.0", None).unwrap().0;
        assert!(
            file.payload.keys[0].has_secret,
            "前置条件：导出文件里该 Key 标着有密钥"
        );

        // 目标机：同 id 的 Key 存在但**没有**密钥。
        let dst = store_at(&dst_dir);
        dst.upsert_key(key("shared", "本机版本")).unwrap();
        assert!(dst.secrets.read().get("shared").unwrap().is_none());

        let report = apply_import(&dst, &file, ImportMode::Merge, None).unwrap();

        // 覆盖后名字来自文件，但 has_secret 必须按**本机实际**修成 false。
        let now = dst.get_key("shared").unwrap();
        assert_eq!(now.name, "源机器版本", "配置应被文件覆盖");
        assert!(
            !now.has_secret,
            "标记必须按本机实际对账成 false，否则 UI 说有密钥、转发报缺失"
        );
        assert!(
            report.warnings.iter().any(|w| w.contains("置回未配置")),
            "要明确告知标记被改了、需重新录入: {:?}",
            report.warnings
        );

        std::fs::remove_dir_all(&src_dir).ok();
        std::fs::remove_dir_all(&dst_dir).ok();
    }

    /// 主口令锁定态下，含密钥段的导入必须在**动任何东西之前**拒绝。
    ///
    /// 若放行：`SecretStore::set` 会逐条拒绝写入 → 得到「配置全导入了、密钥一条没进」的半成品，
    /// 而 Replace 模式此时已经把本机原有 Key 删掉了 —— 用户既没拿到新密钥、也丢了旧配置。
    #[test]
    fn locked_vault_refuses_secret_import_before_touching_config() {
        let src_dir = temp_dir("lock_src");
        let dst_dir = temp_dir("lock_dst");
        let src = store_at(&src_dir);
        src.upsert_key(key("imported", "文件里的")).unwrap();
        src.secrets.write().set("imported", "sk-x").unwrap();
        let file = build_export(&src, "1.0.0", Some("pw")).unwrap().0;

        let dst = store_at(&dst_dir);
        dst.upsert_key(key("local", "本机原有")).unwrap();
        dst.secrets.write().enable_master_password("master").unwrap();
        dst.secrets.write().lock();
        assert!(dst.secrets.read().is_locked(), "前置条件：锁定态");

        let err = apply_import(&dst, &file, ImportMode::Replace, Some("pw")).unwrap_err();
        assert!(err.to_string().contains("解锁"), "要指引去解锁: {err}");

        // 关键：本机配置一点没动（Replace 模式尤其危险）。
        let keys = dst.list_keys(CategoryType::ClaudeCli);
        assert_eq!(keys.len(), 1, "锁定态拒绝导入时不得改动本机配置");
        assert_eq!(keys[0].id, "local");

        std::fs::remove_dir_all(&src_dir).ok();
        std::fs::remove_dir_all(&dst_dir).ok();
    }

    /// 不含密钥导出：能导入配置，但必须**明确告知**需重新录入密钥，且 `has_secret` 被对账成 false。
    ///
    /// 若不对账，UI 会显示「已配置密钥」而转发时报「密钥缺失」——用户完全看不出问题在哪。
    #[test]
    fn export_without_secrets_warns_and_clears_has_secret_flags() {
        let src_dir = temp_dir("nosec_src");
        let dst_dir = temp_dir("nosec_dst");
        let src = store_at(&src_dir);
        src.upsert_key(key("k1", "带密钥的 Key")).unwrap();
        src.secrets.write().set("k1", "sk-x").unwrap();
        src.reconcile_has_secret_flags().unwrap();
        assert!(src.get_key("k1").unwrap().has_secret, "前置条件");

        let file = build_export(&src, "1.0.0", None).unwrap().0;
        assert!(file.secrets.is_none(), "未给口令则不应产出密钥段");

        let dst = store_at(&dst_dir);
        let report = apply_import(&dst, &file, ImportMode::Merge, None).unwrap();
        assert_eq!(report.secrets_imported, 0);
        assert!(
            report.warnings.iter().any(|w| w.contains("重新录入")),
            "必须提示需重新录入密钥: {:?}",
            report.warnings
        );
        assert!(
            !dst.get_key("k1").unwrap().has_secret,
            "标记必须被对账成 false，否则 UI 说有密钥、转发报缺失"
        );

        std::fs::remove_dir_all(&src_dir).ok();
        std::fs::remove_dir_all(&dst_dir).ok();
    }

    /// 口令错误必须在**改动任何配置之前**失败。
    #[test]
    fn wrong_password_aborts_before_touching_local_config() {
        let src_dir = temp_dir("pw_src");
        let dst_dir = temp_dir("pw_dst");
        let src = store_at(&src_dir);
        src.upsert_key(key("imported", "来自导出文件")).unwrap();
        src.secrets.write().set("imported", "sk-x").unwrap();
        let file = build_export(&src, "1.0.0", Some("right-pw")).unwrap().0;

        let dst = store_at(&dst_dir);
        dst.upsert_key(key("local", "本机原有")).unwrap();

        let err = apply_import(&dst, &file, ImportMode::Replace, Some("wrong-pw")).unwrap_err();
        assert!(err.to_string().contains("口令错误"), "{err}");
        // 关键：本机配置一点没动（Replace 模式尤其危险，口令错时绝不能已经清过库）
        let keys = dst.list_keys(CategoryType::ClaudeCli);
        assert_eq!(keys.len(), 1, "口令错时不得改动本机配置");
        assert_eq!(keys[0].id, "local");

        std::fs::remove_dir_all(&src_dir).ok();
        std::fs::remove_dir_all(&dst_dir).ok();
    }

    /// 含密钥段却不给口令 / 不含密钥段却给口令：两者都要**明确报错**而非静默处理。
    ///
    /// 后者尤其重要：用户以为在导密钥，静默成功会让他以为已经导好了。
    #[test]
    fn password_presence_must_match_file() {
        let dir = temp_dir("pw_match");
        let src = store_at(&dir);
        src.upsert_key(key("k1", "k")).unwrap();
        src.secrets.write().set("k1", "sk").unwrap();

        let with_sec = build_export(&src, "1.0.0", Some("pw")).unwrap().0;
        let no_sec = build_export(&src, "1.0.0", None).unwrap().0;

        let e1 = apply_import(&src, &with_sec, ImportMode::Merge, None).unwrap_err();
        assert!(e1.to_string().contains("请提供"), "{e1}");
        let e2 = apply_import(&src, &no_sec, ImportMode::Merge, Some("pw")).unwrap_err();
        assert!(e2.to_string().contains("不包含密钥段"), "{e2}");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Merge 保留本机独有条目、覆盖同 id；Replace 删掉本机独有并先备份。
    #[test]
    fn merge_keeps_local_only_while_replace_removes_them_after_backup() {
        let src_dir = temp_dir("mode_src");
        let src = store_at(&src_dir);
        src.upsert_key(key("shared", "文件里的版本")).unwrap();
        let file = build_export(&src, "1.0.0", None).unwrap().0;

        // --- Merge ---
        let m_dir = temp_dir("mode_merge");
        let m = store_at(&m_dir);
        m.upsert_key(key("shared", "本机旧版本")).unwrap();
        m.upsert_key(key("local-only", "只有本机有")).unwrap();
        let r = apply_import(&m, &file, ImportMode::Merge, None).unwrap();
        assert_eq!(r.keys_overwritten, 1);
        assert_eq!(r.keys_added, 0);
        assert_eq!(r.keys_removed, 0);
        assert!(r.backup_path.is_none(), "Merge 不做破坏性替换，无需备份");
        let keys = m.list_keys(CategoryType::ClaudeCli);
        assert_eq!(keys.len(), 2, "本机独有条目必须保留");
        assert_eq!(
            keys.iter().find(|k| k.id == "shared").unwrap().name,
            "文件里的版本",
            "同 id 应被文件版本覆盖"
        );

        // --- Replace ---
        let r_dir = temp_dir("mode_replace");
        let rp = store_at(&r_dir);
        rp.upsert_key(key("shared", "本机旧版本")).unwrap();
        rp.upsert_key(key("local-only", "只有本机有")).unwrap();
        let rr = apply_import(&rp, &file, ImportMode::Replace, None).unwrap();
        assert_eq!(rr.keys_removed, 1, "本机独有条目应被删除");
        let backup = rr.backup_path.expect("Replace 必须先备份，留回滚退路");
        assert!(
            std::path::Path::new(&backup).exists(),
            "备份文件应真实存在: {backup}"
        );
        let keys = rp.list_keys(CategoryType::ClaudeCli);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].id, "shared");

        std::fs::remove_dir_all(&src_dir).ok();
        std::fs::remove_dir_all(&m_dir).ok();
        std::fs::remove_dir_all(&r_dir).ok();
    }

    /// 本机绑定字段不得被带走 / 带入：端口、日志目录、MCP 注册记录、自启动。
    ///
    /// 这些字段带到新机器上不是「没用」而是**有害**：端口冲突让代理起不来、日志写到不存在的盘、
    /// 声称已注册 MCP 实则没写过客户端配置、自启动开关开着但系统侧没有项。
    #[test]
    fn machine_local_settings_are_stripped_on_export_and_preserved_on_import() {
        let src_dir = temp_dir("ml_src");
        let dst_dir = temp_dir("ml_dst");
        let src = store_at(&src_dir);
        src.set_proxy_port(CategoryType::ClaudeCli, 47199).unwrap();
        src.set_mcp_port(9599).unwrap();
        src.add_registered_category(CategoryType::ClaudeCli).unwrap();
        {
            let mut s = src.get_settings();
            s.log_dir = Some("E:\\源机器专属日志目录".into());
            s.auto_start = true;
            s.theme = "dark".into(); // 非本机绑定字段，应当被带走
            src.save_settings(UserPrefs::from(&s)).unwrap();
        }

        let file = build_export(&src, "1.0.0", None).unwrap().0;
        // 导出侧已剔除
        assert!(file.payload.settings.proxy_ports.is_empty(), "端口不得带走");
        assert!(
            file.payload.settings.mcp_registered_categories.is_empty(),
            "MCP 注册记录不得带走"
        );
        assert!(file.payload.settings.log_dir.is_none(), "日志目录不得带走");
        assert!(!file.payload.settings.auto_start, "自启动状态不得带走");
        assert!(!file.payload.settings.mcp_enabled, "MCP 开关不得带走");
        assert_eq!(file.payload.settings.theme, "dark", "普通设置应当带走");

        // 导入侧：本机现有值必须保住（哪怕有人手改导出文件塞了别的机器的端口）
        let dst = store_at(&dst_dir);
        dst.set_proxy_port(CategoryType::ClaudeCli, 47155).unwrap();
        dst.set_mcp_port(9527).unwrap();
        dst.add_registered_category(CategoryType::Codex).unwrap();
        let mut tampered = file.clone();
        tampered.payload.settings.proxy_ports.insert(CategoryType::ClaudeCli, 1);
        tampered.payload.settings.mcp_port = 2;
        tampered.payload.settings.mcp_registered_categories = vec![CategoryType::ClaudeDesktop];
        tampered.payload.settings.auto_start = true;
        // 手改载荷后校验和会不匹配——这里直接调 apply_import（跳过 parse_and_verify），
        // 正是为了验证「即使绕过校验和，导入逻辑本身也不会覆盖本机运行态」这道纵深防线。
        apply_import(&dst, &tampered, ImportMode::Merge, None).unwrap();

        let now = dst.get_settings();
        assert_eq!(
            now.proxy_ports.get(&CategoryType::ClaudeCli).copied(),
            Some(47155),
            "本机粘滞端口不得被导入值顶掉"
        );
        assert_eq!(now.mcp_port, 9527, "本机 MCP 端口不得被顶掉");
        assert_eq!(
            now.mcp_registered_categories,
            vec![CategoryType::Codex],
            "本机 MCP 注册记录不得被顶掉"
        );
        assert!(!now.auto_start, "自启动状态由本机系统实际情况决定，不得被导入值顶开");
        assert_eq!(now.theme, "dark", "普通设置应当被导入");

        std::fs::remove_dir_all(&src_dir).ok();
        std::fs::remove_dir_all(&dst_dir).ok();
    }

    /// 完整性校验：载荷被改一个字节就必须拒绝，且**在改动任何本机配置之前**。
    #[test]
    fn tampered_payload_is_rejected_by_checksum() {
        let dir = temp_dir("checksum");
        let src = store_at(&dir);
        src.upsert_key(key("k1", "原始名字")).unwrap();
        let file = build_export(&src, "1.0.0", None).unwrap().0;

        let mut raw: serde_json::Value =
            serde_json::from_slice(&serde_json::to_vec(&file).unwrap()).unwrap();
        raw["payload"]["keys"][0]["name"] = serde_json::json!("被篡改的名字");
        let bytes = serde_json::to_vec(&raw).unwrap();

        let err = parse_and_verify(&bytes).unwrap_err().to_string();
        assert!(err.contains("校验和不匹配"), "{err}");
        assert!(err.contains("未改动任何本机配置"), "要让用户放心: {err}");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 格式版本不认识时必须明确拒绝并给出行动指引（升级 / 文件过旧），不做「尽力解析」。
    #[test]
    fn unknown_format_version_is_rejected_with_actionable_message() {
        let dir = temp_dir("fmtver");
        let src = store_at(&dir);
        src.upsert_key(key("k1", "k")).unwrap();
        let file = build_export(&src, "1.0.0", None).unwrap().0;

        let mut newer: serde_json::Value =
            serde_json::from_slice(&serde_json::to_vec(&file).unwrap()).unwrap();
        newer["formatVersion"] = serde_json::json!(FORMAT_VERSION + 1);
        let err = parse_and_verify(&serde_json::to_vec(&newer).unwrap())
            .unwrap_err()
            .to_string();
        assert!(err.contains("更新版本"), "{err}");
        assert!(err.contains("请升级"), "要给行动指引: {err}");

        let mut older: serde_json::Value =
            serde_json::from_slice(&serde_json::to_vec(&file).unwrap()).unwrap();
        older["formatVersion"] = serde_json::json!(0);
        let err = parse_and_verify(&serde_json::to_vec(&older).unwrap())
            .unwrap_err()
            .to_string();
        assert!(err.contains("过旧"), "{err}");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// P2-3：Replace 导入必须清理「被移除 Key」的密钥，否则留下不可见、不可删的孤儿。
    ///
    /// 两层后果（不修的话）：
    /// 1. 用户以为那些 Key 已经没了，但可解密钥材料仍完整留在 secrets.enc 里，UI 无入口可见；
    /// 2. 更隐蔽：日后导入「含同 id Key 但不含密钥段」的文件时，has_secret 对账会读到孤儿密文
    ///    把标记刷成 true → 得到「新配置 + 旧密钥」的嵌合体，转发莫名 401 而界面一切正常。
    #[test]
    fn replace_import_prunes_secrets_of_removed_keys() {
        let src_dir = temp_dir("prune_src");
        let dst_dir = temp_dir("prune_dst");

        // 文件里只有 keep 这一条
        let src = store_at(&src_dir);
        src.upsert_key(key("keep", "留下的")).unwrap();
        let file = build_export(&src, "1.0.0", None).unwrap().0;

        // 本机有 keep + gone 两条，且两条都有密钥
        let dst = store_at(&dst_dir);
        dst.upsert_key(key("keep", "本机的")).unwrap();
        dst.upsert_key(key("gone", "将被移除")).unwrap();
        dst.secrets.write().set("keep", "sk-keep").unwrap();
        dst.secrets.write().set("gone", "sk-gone").unwrap();
        assert_eq!(dst.count_orphan_secrets(), 0, "前置条件：此时无孤儿");

        let report = apply_import(&dst, &file, ImportMode::Replace, None).unwrap();

        // gone 的密钥必须已被清理
        assert_eq!(report.secrets_pruned, 1, "应清理 1 条随 Key 移除的密钥");
        assert_eq!(
            dst.count_orphan_secrets(),
            0,
            "Replace 后不应残留孤儿密钥，实得 {} 条",
            dst.count_orphan_secrets()
        );
        assert!(
            dst.secrets.read().get("gone").unwrap().is_none(),
            "被移除 Key 的密钥必须已删除"
        );
        // keep 仍在载荷里 → 它的密钥**不能**被误删
        assert_eq!(
            dst.secrets.read().get("keep").unwrap().as_deref().map(String::as_str),
            Some("sk-keep"),
            "载荷里仍有的 Key，其密钥不得被清理"
        );

        std::fs::remove_dir_all(&src_dir).ok();
        std::fs::remove_dir_all(&dst_dir).ok();
    }

    /// Replace 导入删旧密钥**之前必须先备份 secrets.enc**，且备份路径要随报告交给用户。
    ///
    /// 为什么这条一定要有测试：`backup_config_before_import` 只备份 config.json，看起来
    /// 「导入是可回滚的」；但删密钥走的是 `SecretStore::remove` → 立即整份重写 secrets.enc。
    /// 少了这道备份，用户拿 `backup_path` 还原配置后 Key 全回来了、密钥却永久没了，
    /// 只能去各中转商后台重取——而这一切在报告里看不出任何异常（`secrets_pruned` 还显示成功）。
    /// 故障注入验证：把 `backup_before_rewrite("import-replace")` 那几行删掉，本测试必须变红。
    #[test]
    fn replace_import_backs_up_secret_vault_before_pruning() {
        let src_dir = temp_dir("secbak_src");
        let dst_dir = temp_dir("secbak_dst");

        let src = store_at(&src_dir);
        src.upsert_key(key("keep", "留下的")).unwrap();
        let file = build_export(&src, "1.0.0", None).unwrap().0;

        let dst = store_at(&dst_dir);
        dst.upsert_key(key("keep", "本机的")).unwrap();
        dst.upsert_key(key("gone", "将被移除")).unwrap();
        dst.secrets.write().set("keep", "sk-keep").unwrap();
        dst.secrets.write().set("gone", "sk-gone").unwrap();

        let report = apply_import(&dst, &file, ImportMode::Replace, None).unwrap();
        assert_eq!(report.secrets_pruned, 1, "前置条件：确实删了一条密钥");

        let bak = report
            .secrets_backup_path
            .as_deref()
            .expect("删密钥前必须备份密钥库，并把路径交给用户");
        let bak = std::path::Path::new(bak);
        assert!(bak.exists(), "报告给出的密钥库备份路径必须真实存在: {bak:?}");

        // 备份必须是**删除之前**的那一份：拿它替换回去，被删的密钥要能重新读出来。
        // 只断言「文件存在」是不够的——备份成一个空文件同样能过。
        std::fs::copy(bak, dst_dir.join("secrets.enc")).unwrap();
        let restored = crate::secret::SecretStore::load(dst_dir.join("secrets.enc")).unwrap();
        assert_eq!(
            restored.get("gone").unwrap().as_deref().map(String::as_str),
            Some("sk-gone"),
            "备份必须早于删除，否则回滚拿不回被删的密钥"
        );

        std::fs::remove_dir_all(&src_dir).ok();
        std::fs::remove_dir_all(&dst_dir).ok();
    }
    #[test]
    fn prune_orphan_secrets_cleans_history_and_skips_when_locked() {
        let dir = temp_dir("prune_history");
        let store = store_at(&dir);
        store.upsert_key(key("live", "在用")).unwrap();
        store.secrets.write().set("live", "sk-live").unwrap();
        // 手工制造历史遗留孤儿：直接往密钥库写一条配置里没有的
        store.secrets.write().set("ghost", "sk-ghost").unwrap();

        assert_eq!(store.count_orphan_secrets(), 1, "应检出 1 条孤儿");
        assert_eq!(store.prune_orphan_secrets(), 1, "应清理 1 条");
        assert_eq!(store.count_orphan_secrets(), 0);
        assert_eq!(
            store.secrets.read().get("live").unwrap().as_deref().map(String::as_str),
            Some("sk-live"),
            "在用 Key 的密钥不得被误删"
        );

        // 锁定态：读不到内容，必须**跳过**而非把所有密钥都当成孤儿删掉。
        //
        // 注意 `is_locked()` = 「主口令模式 **且** 未解锁」——DPAPI 模式下没有可锁的东西，
        // 直接调 lock() 不会进入锁定态。故必须先启用主口令，再锁。
        store.secrets.write().set("ghost2", "sk-ghost2").unwrap();
        assert_eq!(store.count_orphan_secrets(), 1);
        store.secrets.write().enable_master_password("pw-123456").unwrap();
        store.secrets.write().lock();
        assert!(store.secrets.read().is_locked(), "前置条件：必须真的进入锁定态");
        assert_eq!(
            store.count_orphan_secrets(),
            0,
            "锁定态应返回 0（不是「没有孤儿」，而是「此时无法判断」）"
        );
        assert_eq!(
            store.prune_orphan_secrets(),
            0,
            "锁定态必须跳过清理：把「暂时读不到」当成「确实没有」会误删真实密钥"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// P3-5：id 撞号但「名字与地址都不同」必须被列为**可疑冲突**逐条警告，
    /// 而不是混进 `conflicting_keys` 那个计数里（后者与正常更新无法区分）。
    ///
    /// 真实场景：早期 Key id 是 `k_<毫秒>`，两台机器照同一份教程配置、落在同一毫秒即撞号。
    /// 跨机导入会把一条**完全无关**的本机 Key 静默换成对方的 base_url / 协议 / 映射。
    #[test]
    fn suspicious_conflicts_are_listed_not_just_counted() {
        let src_dir = temp_dir("susp_src");
        let dst_dir = temp_dir("susp_dst");
        let src = store_at(&src_dir);
        // 三条都与本机同 id，但性质不同
        let mut collide = key("k_1700000000000", "对方的 GLM");
        collide.base_url = "https://glm.example.com".into();
        src.upsert_key(collide).unwrap();
        src.upsert_key(key("same-both", "同名同地址")).unwrap(); // 正常更新
        let mut renamed = key("only-name-differs", "改了个名字");
        renamed.base_url = "https://api.example.com".into(); // 地址不变
        src.upsert_key(renamed).unwrap();
        let file = build_export(&src, "1.0.0", None).unwrap().0;

        let dst = store_at(&dst_dir);
        let mut local_collide = key("k_1700000000000", "我的 Kimi");
        local_collide.base_url = "https://kimi.example.com".into();
        dst.upsert_key(local_collide).unwrap();
        dst.upsert_key(key("same-both", "同名同地址")).unwrap();
        dst.upsert_key(key("only-name-differs", "原来的名字")).unwrap();

        let pv = preview_import(&dst, &file);
        assert_eq!(pv.conflicting_keys, 3, "三条都是 id 重复");
        assert_eq!(
            pv.suspicious_conflicts.len(),
            1,
            "只有「名字与地址都不同」那条算可疑，实得 {:?}",
            pv.suspicious_conflicts
        );
        let s = &pv.suspicious_conflicts[0];
        assert_eq!(s.id, "k_1700000000000");
        assert_eq!(s.local_name, "我的 Kimi", "要指出本机哪条会被换掉");
        assert_eq!(s.incoming_name, "对方的 GLM", "要指出会被换成什么");
        assert!(s.local_base_url.contains("kimi"));
        assert!(s.incoming_base_url.contains("glm"));

        std::fs::remove_dir_all(&src_dir).ok();
        std::fs::remove_dir_all(&dst_dir).ok();
    }

    /// P3-5：新建 Key 的 id 由后端生成 uuid v4，且**必须随返回值回传**。
    ///
    /// 回传是硬要求：前端拿返回值去 saveSecret / checkHealth，若拿到空 id
    /// 会把密钥写成孤儿、探测打到不存在的 Key 上，而界面一切正常——静默失效。
    #[test]
    fn upsert_key_assigns_uuid_when_id_empty() {
        let dir = temp_dir("uuid_id");
        let store = store_at(&dir);

        let mut fresh = key("", "新建的");
        fresh.id = String::new();
        let saved = store.upsert_key(fresh).unwrap();
        assert!(!saved.id.is_empty(), "后端必须补 id 并回传");
        // uuid v4 形状：8-4-4-4-12
        let parts: Vec<&str> = saved.id.split('-').collect();
        assert_eq!(parts.len(), 5, "应为 uuid v4 形状，实得 {}", saved.id);
        assert_eq!(parts[0].len(), 8);
        assert_eq!(parts[4].len(), 12);
        // 真的进库了（用回传的 id 能取到）
        assert!(store.get_key(&saved.id).is_some(), "回传的 id 必须能取到该 Key");

        // 两次新建不撞号（时间戳 id 在同毫秒会撞，uuid 不会）
        let mut a = key("", "A");
        a.id = String::new();
        let mut b = key("", "B");
        b.id = String::new();
        let ida = store.upsert_key(a).unwrap().id;
        let idb = store.upsert_key(b).unwrap().id;
        assert_ne!(ida, idb, "连续新建必须得到不同 id");

        // 已有 id 时不得被改写（编辑保存不能变成新增一条）
        let existing = key("keep-me", "原有");
        store.upsert_key(existing.clone()).unwrap();
        let again = store.upsert_key(existing).unwrap();
        assert_eq!(again.id, "keep-me", "已有 id 必须原样保留");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 非导出文件（随便一个 JSON）要给「不是有效的 SynaRoute 导出文件」而非晦涩的 serde 报错。
    #[test]
    fn random_json_is_rejected_with_readable_message() {
        let err = parse_and_verify(br#"{"hello":"world"}"#)
            .unwrap_err()
            .to_string();
        assert!(err.contains("不是有效的 SynaRoute 导出文件"), "{err}");
    }

    /// 预检只读：不得改动任何本机配置，且统计数要对。
    #[test]
    fn preview_is_read_only_and_counts_are_correct() {
        let src_dir = temp_dir("pv_src");
        let dst_dir = temp_dir("pv_dst");
        let src = store_at(&src_dir);
        src.upsert_key(key("shared", "文件版")).unwrap();
        src.upsert_key(key("file-only", "只在文件里")).unwrap();
        src.secrets.write().set("shared", "sk").unwrap();
        let file = build_export(&src, "7.7.7", Some("pw")).unwrap().0;

        let dst = store_at(&dst_dir);
        dst.upsert_key(key("shared", "本机版")).unwrap();
        dst.upsert_key(key("local-only", "只在本机")).unwrap();

        let pv = preview_import(&dst, &file);
        assert_eq!(pv.key_count, 2);
        assert_eq!(pv.conflicting_keys, 1, "shared 会被覆盖");
        assert_eq!(pv.local_only_keys, 1, "local-only 在 Replace 下会被删");
        assert!(pv.has_secrets, "该文件含密钥段");
        assert_eq!(pv.app_version, "7.7.7");

        // 只读：本机配置一点没动
        let keys = dst.list_keys(CategoryType::ClaudeCli);
        assert_eq!(keys.len(), 2);
        assert_eq!(keys.iter().find(|k| k.id == "shared").unwrap().name, "本机版");

        std::fs::remove_dir_all(&src_dir).ok();
        std::fs::remove_dir_all(&dst_dir).ok();
    }

    /// Replace 不得清掉内置厂商种子——那是程序自带的，清掉会让「一键导入预设模型」失效；
    /// 但**自定义**厂商属于用户数据，Replace 语义下该被清掉再按文件重建。
    ///
    /// 构造方式说明：`upsert_vendor` 会强制 `builtin=false`（防伪造），测试用的
    /// `Store::new_at` 也不注入种子（只有 `init()` 注入）。故先用一次「文件里含内置厂商」的
    /// Merge 导入把种子塞进去，模拟真实机器上 init 注入后的状态，再验证 Replace 的取舍。
    #[test]
    fn replace_keeps_builtin_vendors_but_clears_custom_ones() {
        let dst_dir = temp_dir("bv_dst");
        let dst = store_at(&dst_dir);

        // 第一步：造出「有内置种子 + 有自定义厂商」的本机状态。
        let seed_payload = ExportPayload {
            keys: vec![],
            brain: vec![],
            vendors: vec![vendor("builtin-1", true)],
            settings: dst.get_settings(),
        };
        dst.apply_imported_config(&seed_payload, ImportMode::Merge)
            .unwrap();
        dst.upsert_vendor(vendor("custom-1", false)).unwrap();
        assert!(
            dst.list_vendors().iter().any(|v| v.id == "builtin-1" && v.builtin),
            "前置条件：内置厂商已就位"
        );

        // 第二步：用一个**不含厂商**的导出文件做 Replace。
        let src_dir = temp_dir("bv_src");
        let src = store_at(&src_dir);
        let file = build_export(&src, "1.0.0", None).unwrap().0;
        assert!(file.payload.vendors.is_empty(), "前置条件：导出文件里没有厂商");
        apply_import(&dst, &file, ImportMode::Replace, None).unwrap();

        let after = dst.list_vendors();
        assert!(
            after.iter().any(|v| v.id == "builtin-1"),
            "内置厂商种子不得被 Replace 清掉（否则「一键导入预设模型」失效）"
        );
        assert!(
            !after.iter().any(|v| v.id == "custom-1"),
            "自定义厂商属用户数据，Replace 语义下应被清掉"
        );

        std::fs::remove_dir_all(&src_dir).ok();
        std::fs::remove_dir_all(&dst_dir).ok();
    }

    /// 文件里没有的 Key 对应的密钥不得被导入（避免留下无主密钥）。
    #[test]
    fn orphan_secrets_are_not_imported() {
        let dir = temp_dir("orphan");
        let dst_dir = temp_dir("orphan_dst");
        let src = store_at(&dir);
        src.upsert_key(key("k1", "k")).unwrap();
        src.secrets.write().set("k1", "sk-1").unwrap();
        let mut file = build_export(&src, "1.0.0", Some("pw")).unwrap().0;

        // 手工往密钥段里塞一个「配置里不存在的 Key」的密钥，模拟被改过的文件。
        let plain = crypto::open("pw", file.secrets.as_ref().unwrap()).unwrap();
        let mut sp: SecretsPlain = serde_json::from_slice(&plain).unwrap();
        sp.entries.insert("ghost".into(), "sk-ghost".into());
        file.secrets = Some(crypto::seal("pw", &serde_json::to_vec(&sp).unwrap()).unwrap());

        let dst = store_at(&dst_dir);
        let report = apply_import(&dst, &file, ImportMode::Merge, Some("pw")).unwrap();
        assert_eq!(report.secrets_imported, 1, "只应导入配置里存在的那一条");
        assert!(
            dst.secrets.read().get("ghost").unwrap().is_none(),
            "无主密钥不得进库"
        );

        std::fs::remove_dir_all(&dir).ok();
        std::fs::remove_dir_all(&dst_dir).ok();
    }
}
