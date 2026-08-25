//! Codex 接入：把本地代理端点写进 `~/.codex/config.toml` 的自定义 provider。
//!
//! 从 `tools.rs` 抽出来的理由不只是体积（虽然那也是硬约束 —— tools.rs 的棘轮余量为 0）：
//! Codex 这条路上的判据密度远高于另两个客户端，而它们此前散在 tools.rs 的四个不相邻区段里，
//! 「占位符是不是我们写的」这一个语义就被抄成了三份（见 [`auth_carries_our_placeholder`]）。
//!
//! # 2026-08-25 实测判据矩阵（codex-cli 0.148.0-alpha.9，隔离 `CODEX_HOME` + 本地探针抓
//! `Authorization` 头）—— 本模块的每个决定都出自这张表，不是文档推测
//!
//! | config.toml | auth.json | 实际 base_url | 实际 Authorization |
//! |---|---|---|---|
//! | 我们的 provider + `experimental_bearer_token` | 有（真 OAuth） | provider 的 base_url | `Bearer <bearer>` ——**auth.json 被忽略** |
//! | 我们的 provider + bearer，`requires_openai_auth=true` | **无** | provider 的 base_url | `Bearer <bearer>`，**没有凭据门禁** |
//! | 我们的 provider + bearer，`requires_openai_auth` 省略 | **无** | provider 的 base_url | `Bearer <bearer>` |
//! | provider 表无 bearer | 有 | provider 的 base_url | `Bearer <auth.json 的 OPENAI_API_KEY>` |
//! | **顶层 `model_provider` 键整个缺失** | 有占位符 | `https://api.openai.com/v1` | `Bearer synaroute-proxy` → **401** |
//! | `model_provider="synaroute"` 但**表缺失** | 任意 | —— | `Error: Model provider \`synaroute\` not found`，一个请求都不发 |
//! | `[model_providers.openai]` 想覆盖内置 id | —— | —— | 启动即失败：`reserved built-in provider IDs` |
//!
//! 由此三条结论，逐条对应本模块的一个设计：
//!
//! 1. **`experimental_bearer_token` 优先于 auth.json，且没有凭据门禁** → 我们**不写 auth.json**。
//!    那份占位符在正常接入时从不外发，纯粹是负债：它只在漂移之后才会被发出去，而收件人是
//!    **真实的 OpenAI**。用户看到 `Incorrect API key provided: synarout***roxy` 被指向
//!    platform.openai.com 查自己的 key —— 方向完全相反。不写它，这个失败模式从根上消失，
//!    顺带也不再动用户的 ChatGPT 登录态（旧实现是**整份替换** auth.json）。
//! 2. **真正把假 key 送去官方的判据是「顶层 `model_provider` 键缺失」**，不是历史注释写的
//!    「provider 表被丢掉而选中项留着」—— 后者 Codex 直接硬报错、不发请求。告警文案按形态分支
//!    （见 [`DriftState`]），否则会把人指去查一个根本不会发生的 401。
//! 3. **内置 provider id 不可覆盖** → 「把内置 `openai` 指向本地代理来中和回落」这条路已被证伪，
//!    别再试（Codex 启动即失败）。
//!
//! # 版本前提（重要）
//!
//! 上表测的是**用户今天在跑的那个版本**。2026-08-02 的旧判据（「`requires_openai_auth=true`
//! 必须配套 auth.json，否则桌面端停在登录页」）当时是真机观测，Codex 后来改了行为。
//! 故这里的取舍是**按代价不对称**定的，不是赌某个版本：万一某个 Codex 版本仍要凭据，
//! 它报的是 `no Codex credentials were found · Run codex login`（响亮、可自助、且用户的
//! OAuth 完好无损）；而写占位符的代价是把一个假凭据发给第三方 + 一句指错方向的报错。

use crate::error::{AppError, AppResult};
use serde_json::Value;
use std::path::{Path, PathBuf};

use super::{
    backup_and_write_bytes, prerestore_path_for, read_preview_text, with_rollback,
    MCP_CLIENT_NAME, PROXY_PLACEHOLDER,
};

/// `~/.codex/config.toml`。
pub(super) fn config_path() -> AppResult<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| AppError::ToolConfig("无法定位用户目录".into()))?;
    Ok(home.join(".codex").join("config.toml"))
}

/// `~/.codex/auth.json`（与 config.toml 同目录）。
///
/// **我们不再往这里写任何东西**（见模块头）。保留这个路径只为两件事：
/// ① 清理旧版本 SynaRoute 留下的占位符；② 漂移检测时判断「假 key 是否仍武装着」。
pub(super) fn auth_path() -> AppResult<PathBuf> {
    let cfg = config_path()?;
    Ok(cfg
        .parent()
        .ok_or_else(|| AppError::ToolConfig("无法定位 .codex 目录".into()))?
        .join("auth.json"))
}

// ---------------------------------------------------------------------------
// 接入
// ---------------------------------------------------------------------------

/// 写入 Codex 接入配置。
///
/// 只动 `config.toml` 一个文件，**并顺带解除旧版本留下的假凭据**。
pub(super) fn apply(endpoint: &str, default_model: Option<&str>) -> AppResult<String> {
    let path = config_path()?;
    let auth = auth_path()?;
    // 两条路径都进 with_rollback：config.toml 是我们要写的，auth.json 是我们要**摘**的
    // （摘除同样是写操作，失败也得能回滚）。副文件（`.bak` / `.synaroute-created`）由
    // `with_rollback` 自己按主路径推导后一并纳入快照 —— 漏了它们会造成数据丢失级后果，
    // 判据见 tools.rs 里 `with_rollback` 的文档。
    with_rollback(&[path.clone(), auth.clone()], || {
        let msg = apply_at(&path, endpoint, default_model)?;

        // **写完立刻读回校验**，且校验的是「身份」不是「存在」（见 `is_intact`）。
        //
        // 为什么不可省：Codex 桌面端 App 拥有 config.toml，会在接入**之后**重写它。
        // 这道门只保证「写入那一刻是对的」，之后靠 `drift_state` 常驻检测。
        if !is_intact(&path, endpoint) {
            return Err(AppError::ToolConfig(format!(
                "写入 Codex 配置后校验未通过：{} 里 model_provider 未选中 `{}`，\
                 或 [model_providers.{}] 的 base_url 不是本机代理地址（{}）。已回滚本次改动。\n\
                 常见原因：Codex 正在运行并重写了该文件 —— 请先完全退出 Codex 再重试接入。",
                path.display(),
                MCP_CLIENT_NAME,
                MCP_CLIENT_NAME,
                expected_base_url(endpoint)
            )));
        }

        // 旧版本（≤0.1.33）接入时会把 auth.json **整份替换**成占位符。升级到本版后那份假凭据
        // 仍留在盘上，而我们已经不需要它了 —— 留着只会在下一次漂移时被发给真实 OpenAI。
        // 故每次接入都顺手解除一次（幂等，没有就什么都不做）。
        let disarmed = disarm_legacy_placeholder_auth(&auth)?;
        Ok(match disarmed {
            Some(note) => format!("{msg}；{note}"),
            None => msg,
        })
    })
}

/// provider 表里该写的 `base_url`：Codex 按 `wire_api=responses` 调 `{base_url}/responses`，
/// 而本地代理识别 `/v1/responses`，故要带上 `/v1`。
fn expected_base_url(endpoint: &str) -> String {
    format!("{}/v1", endpoint.trim_end_matches('/'))
}

/// 可测入口：写入指定 config.toml。
///
/// 写的四个字段各有判据：
/// - `model_provider = "synaroute"` 选中我们；
/// - `[model_providers.synaroute].base_url = {endpoint}/v1`；
/// - `wire_api = "responses"`（Codex 的 Responses 形态）；
/// - `experimental_bearer_token` = 占位符 —— 代理侧剥掉入站鉴权头、按路由 Key 注入真实密钥，
///   故此值只需非空。它是 `ModelProviderInfo` 的正式字段（codex.exe 符号表可见）。
///
/// **刻意不写的两个字段**：
/// - `requires_openai_auth`：它是「走 OpenAI 凭据门禁」的开关，而我们不是 OpenAI。
///   写 true 会把 Codex 推向 auth.json / 环境变量那条凭据链，正是要摆脱的那条。
/// - `env_key`：那会要求用户手设环境变量。
///
/// 幂等：序列化结果与磁盘一致时 `backup_and_write_bytes` 短路（不备份不写），
/// 故重复接入不会把已接入的 config 当成「接入前快照」拷进 `.bak`。
/// 保留 config.toml 其余表（`mcp_servers` / 用户自己的 `model_providers` 等）不动。
pub(super) fn apply_at(
    path: &Path,
    endpoint: &str,
    default_model: Option<&str>,
) -> AppResult<String> {
    let content = if path.exists() {
        std::fs::read_to_string(path)?
    } else {
        String::new()
    };

    let mut doc: toml::Value = if content.trim().is_empty() {
        toml::Value::Table(Default::default())
    } else {
        content
            .parse::<toml::Value>()
            .map_err(|e| AppError::ToolConfig(format!("解析 config.toml 失败: {e}")))?
    };

    let base_url = expected_base_url(endpoint);
    let table = doc
        .as_table_mut()
        .ok_or_else(|| AppError::ToolConfig("config.toml 顶层非表".into()))?;

    table.insert(
        "model_provider".to_string(),
        toml::Value::String(MCP_CLIENT_NAME.to_string()),
    );

    // 默认模型（借鉴 cc-switch）：写顶层 model，让 Codex 启动即有模型、无需 /model 手选。
    // 解析不出时**不动**用户已有的 model 字段（避免清空）。
    if let Some(m) = default_model.map(|s| s.trim()).filter(|s| !s.is_empty()) {
        table.insert("model".to_string(), toml::Value::String(m.to_string()));
    }

    let providers = table
        .entry("model_providers".to_string())
        .or_insert_with(|| toml::Value::Table(Default::default()));
    let providers_table = providers
        .as_table_mut()
        .ok_or_else(|| AppError::ToolConfig("model_providers 非表".into()))?;
    let mut entry = toml::value::Table::new();
    entry.insert("name".into(), toml::Value::String("SynaRoute".into()));
    entry.insert("base_url".into(), toml::Value::String(base_url.clone()));
    entry.insert("wire_api".into(), toml::Value::String("responses".into()));
    entry.insert(
        "experimental_bearer_token".into(),
        toml::Value::String(PROXY_PLACEHOLDER.to_string()),
    );
    // 旧版本写过 `requires_openai_auth = true`，升级后必须**主动删掉**：留着它，Codex 会继续
    // 走 OpenAI 凭据门禁，于是「不写 auth.json」这件事的收益就被抵消了（老用户静默保持旧行为，
    // 而这类「只对新用户生效的修复」是本仓反复踩过的坑）。
    entry.remove("requires_openai_auth");
    providers_table.insert(MCP_CLIENT_NAME.to_string(), toml::Value::Table(entry));

    let serialized =
        toml::to_string_pretty(&doc).map_err(|e| AppError::ToolConfig(e.to_string()))?;
    backup_and_write_bytes(path, serialized.as_bytes())?;
    let model_note = default_model
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|m| format!("，默认模型={m}"))
        .unwrap_or_default();
    Ok(format!(
        "已写入 Codex 配置：{}（model_provider={MCP_CLIENT_NAME}，base_url={base_url}，\
         wire_api=responses{model_note}）；鉴权走 provider 表的 bearer，**未改动 auth.json**\
         （官方 ChatGPT 登录状态原样保留）；原文件已备份",
        path.display()
    ))
}

// ---------------------------------------------------------------------------
// 假凭据的识别与解除
// ---------------------------------------------------------------------------

/// auth.json 里的 `OPENAI_API_KEY` **是不是我们写的占位符**。
///
/// 这是「假 key 是否武装着」的**唯一判据**，漂移检测与清理都走它。
///
/// # 为什么不能是「整个对象恰好只有这一个键」
///
/// 旧实现是 `obj.len() == 1 && OPENAI_API_KEY == 占位符`。那个 `len() == 1` 是从另一个问题
/// （「这份文件能不能整份删掉」）借来的代理判据，两个问题不是一回事：
/// - 「能不能删」要保守（**绝不能**删掉含 OAuth `tokens` 的真凭据）→ 见 [`auth_is_only_our_placeholder`]；
/// - 「假 key 在不在」要宽松（多一个 `auth_mode` 字段不影响那个 key 会被发出去）。
///
/// 把保守判据用在宽松问题上，失效方向是**静默放行**（fail-open）：`codex login --with-api-key`
/// 写出的是 `{"auth_mode":"apikey","OPENAI_API_KEY":"…"}` **两个字段**，命中不了 `len()==1`，
/// 于是漂移告警与清理同时哑掉，而那个 key 照样在发。
pub(super) fn auth_carries_our_placeholder(path: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(&text) else {
        return false;
    };
    matches!(obj.get("OPENAI_API_KEY"), Some(Value::String(v)) if v == PROXY_PLACEHOLDER)
}

/// 这份 auth.json 是否**纯粹**是我们凭空造出来的占位符文件（可以整份删掉）。
///
/// 比 [`auth_carries_our_placeholder`] 严格：额外要求没有 OAuth `tokens`，也没有
/// 除 `OPENAI_API_KEY` / `auth_mode` 之外的任何字段。用于「删整个文件」这种不可逆操作。
///
/// 允许 `auth_mode` 共存的理由：`codex login --with-api-key` 会补上它，而那种文件里
/// **只有**我们的占位符，删掉它不会丢任何用户凭据。
pub(super) fn auth_is_only_our_placeholder(path: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(&text) else {
        return false;
    };
    if !matches!(obj.get("OPENAI_API_KEY"), Some(Value::String(v)) if v == PROXY_PLACEHOLDER) {
        return false;
    }
    if obj.get("tokens").is_some_and(|t| !t.is_null()) {
        return false;
    }
    obj.keys()
        .all(|k| k == "OPENAI_API_KEY" || k == "auth_mode")
}

/// 解除旧版本留在 auth.json 里的占位符。返回 `Some(给用户看的说明)` 表示确实动了。
///
/// 三种情形，都要留下**可回滚的现场**（`.synaroute-prerestore`），与 `restore_one` 同一纪律：
/// - `.bak` 在（旧版接入时备份过真凭据）→ 交还真凭据，删 `.bak`；
/// - 无 `.bak` 且文件纯粹是占位符 → 删掉整个文件（回到「接入前没有 auth.json」）；
/// - 无 `.bak` 但文件里还有别的东西（用户后来自己 `codex login` 过）→ 只摘掉那个 key，
///   其余字段原样保留。**绝不整份删**。
pub(super) fn disarm_legacy_placeholder_auth(path: &Path) -> AppResult<Option<String>> {
    if !auth_carries_our_placeholder(path) {
        return Ok(None);
    }
    // 无论走哪支，先留现场：这一步失败不算致命（继续解除更重要），但要告警。
    if let Err(e) = std::fs::copy(path, prerestore_path_for(path)) {
        tracing::warn!("解除 Codex 占位凭据前保留现场失败 {}: {e}", path.display());
    }

    let backup = super::backup_path_for(path);
    if backup.exists() {
        let data = std::fs::read(&backup)?;
        crate::secret::atomic_write(path, &data)?;
        if let Err(e) = std::fs::remove_file(&backup) {
            tracing::warn!("解除后清理备份 {} 失败: {e}", backup.display());
        }
        return Ok(Some(format!(
            "已从备份交还 {}（旧版本写入的占位密钥已解除，官方登录态恢复）",
            path.display()
        )));
    }

    if auth_is_only_our_placeholder(path) {
        std::fs::remove_file(path)?;
        // 凭空新建标记若还在，一并清掉：文件已不存在，标记留着只会让日后的 restore
        // 去删一份「用户后来自己登录出来的」真凭据（无 .bak 无 prerestore，静默丢失）。
        let marker = super::created_marker_path_for(path);
        if marker.exists() {
            if let Err(e) = std::fs::remove_file(&marker) {
                tracing::warn!("清理新建标记 {} 失败: {e}", marker.display());
            }
        }
        return Ok(Some(format!(
            "已删除 {}（旧版本凭空创建的纯占位密钥文件）",
            path.display()
        )));
    }

    // 混合形态：只摘 key，保留用户自己的其余字段。
    let text = std::fs::read_to_string(path)?;
    let mut obj: serde_json::Map<String, Value> = match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(o)) => o,
        _ => return Ok(None),
    };
    obj.remove("OPENAI_API_KEY");
    // `auth_mode: "apikey"` 失去了它指向的 key，留着会让 Codex 判定为 api-key 模式却找不到 key。
    if matches!(obj.get("auth_mode"), Some(Value::String(v)) if v == "apikey") {
        obj.remove("auth_mode");
    }
    let bytes = serde_json::to_vec_pretty(&Value::Object(obj))
        .map_err(|e| AppError::ToolConfig(format!("序列化 auth.json 失败: {e}")))?;
    crate::secret::atomic_write(path, &bytes)?;
    Ok(Some(format!(
        "已从 {} 摘除旧版本写入的占位密钥（其余字段保留）",
        path.display()
    )))
}

// ---------------------------------------------------------------------------
// 完好性与漂移
// ---------------------------------------------------------------------------

/// 我们那套接入配置是否**完好** —— 判的是「身份」，不是「存在」。
///
/// 四条同时成立才算完好：
/// 1. 顶层 `model_provider` 选中 `synaroute`；
/// 2. `[model_providers.synaroute]` 表在；
/// 3. 它的 `base_url` **正是本机当前代理地址**；
/// 4. 它带着我们的 `experimental_bearer_token`。
///
/// # 为什么 3 和 4 不能省
///
/// 旧实现只查「`base_url` 非空」。于是
/// `[model_providers.synaroute] base_url = "https://relay.example/v1"`（或指着一个**已死的
/// 旧端口**）被判为完好 → 漂移告警永远不发。这不是假想形态：cc-switch 会把 SynaRoute
/// 的接入态整份存成它自己的一个 provider 档，用户在它那边切回来时写出的就是一个
/// **端口可能已经变了、且没有 bearer** 的 `[model_providers.synaroute]` ——
/// 此时占位符成了唯一凭据，而 `is_intact` 说一切正常。
///
/// 端口比对失配时的正确处置是**重新接入**（写上当前端口），不是报错；调用方据此决定。
pub(super) fn is_intact(path: &Path, endpoint: &str) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(doc) = text.parse::<toml::Value>() else {
        return false;
    };
    let selected = doc
        .get("model_provider")
        .and_then(|v| v.as_str())
        .is_some_and(|v| v == MCP_CLIENT_NAME);
    if !selected {
        return false;
    }
    let Some(t) = doc
        .get("model_providers")
        .and_then(|v| v.get(MCP_CLIENT_NAME))
        .and_then(|v| v.as_table())
    else {
        return false;
    };
    let url_ok = t
        .get("base_url")
        .and_then(|b| b.as_str())
        .is_some_and(|s| s == expected_base_url(endpoint));
    let bearer_ok = t
        .get("experimental_bearer_token")
        .and_then(|b| b.as_str())
        .is_some_and(|s| s == PROXY_PLACEHOLDER);
    url_ok && bearer_ok
}

/// 漂移形态。文案按形态分支，因为**每种形态下 Codex 的实际行为不同**，
/// 而指错方向的告警比没有告警更糟（它会让用户去查一个根本不存在的问题）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum DriftState {
    /// 接入完好。
    Intact,
    /// 我们从未接入过（或已还原）—— 此时 config 不指向我们完全正常，不该报警。
    NotApplied,
    /// 顶层 `model_provider` 键缺失或 `= "openai"` → Codex 回落内置官方地址。
    /// **只有这一支会出现那句 401 `Incorrect API key provided`**（且仅当假 key 还武装着）。
    FellBackToOfficial { placeholder_armed: bool },
    /// 选中了别人的 provider。`destination` 是那个 provider 的 base_url（可能拿不到）。
    /// `placeholder_would_be_sent` = 该表没有自带凭据 → 我们的占位符会被发往那个地址。
    SelectedElsewhere {
        provider_id: String,
        destination: Option<String>,
        placeholder_would_be_sent: bool,
    },
    /// 选中项悬空（表缺失）→ Codex **启动即硬报错**、一个请求都不发。文案不得提 401。
    SelectionDangling,
    /// 表在、选中项也是我们，但 `base_url` 指向别处或缺 bearer（端口漂移 / 被外部改写）。
    OurTablePointsElsewhere { destination: Option<String> },
    /// `$CODEX_HOME/*.config.toml` 存在 → `codex --profile <name>` 会**整条旁路**我们写的一切。
    ProfileShadowed { profiles: Vec<String> },
    /// 顶层遗留 `profile = "..."` 老写法 → 当前 Codex **整份配置加载失败**。
    LegacyProfileKeyBreaksLoad { profile: String },
}

/// 判定漂移形态。`applied` = 我们是否认为自己处于已接入态（由调用方按 `.bak`/运行态给出）。
///
/// 判据刻意**不是**「占位符在不在 auth.json 里」（旧实现如此）：本版已经不写 auth.json 了，
/// 那个门一旦成为唯一入口，整个漂移检测就会变成永不触发的死代码 —— 而检测器自己静默失效，
/// 正是本仓反复吃过的那类亏。
pub(super) fn drift_state(
    cfg_path: &Path,
    auth_path: &Path,
    endpoint: &str,
    applied: bool,
) -> DriftState {
    // 配置加载层面的问题优先报：它们让 config.toml 里写了什么都无关紧要。
    if let Some(p) = legacy_profile_key(cfg_path) {
        return DriftState::LegacyProfileKeyBreaksLoad { profile: p };
    }
    let profiles = shadowing_profiles(cfg_path);
    if !profiles.is_empty() {
        return DriftState::ProfileShadowed { profiles };
    }

    if is_intact(cfg_path, endpoint) {
        return DriftState::Intact;
    }
    if !applied {
        return DriftState::NotApplied;
    }

    let armed = auth_carries_our_placeholder(auth_path);
    let doc = std::fs::read_to_string(cfg_path)
        .ok()
        .and_then(|t| t.parse::<toml::Value>().ok());
    let selected = doc
        .as_ref()
        .and_then(|d| d.get("model_provider"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let table_of = |id: &str| -> Option<&toml::value::Table> {
        doc.as_ref()?
            .get("model_providers")?
            .get(id)?
            .as_table()
    };

    match selected.as_deref() {
        // 键缺失，或显式选了内置 openai → 回落官方
        None | Some("openai") => DriftState::FellBackToOfficial {
            placeholder_armed: armed,
        },
        Some(id) if id == MCP_CLIENT_NAME => match table_of(id) {
            None => DriftState::SelectionDangling,
            Some(t) => DriftState::OurTablePointsElsewhere {
                destination: t
                    .get("base_url")
                    .and_then(|b| b.as_str())
                    .map(str::to_string),
            },
        },
        Some(id) => match table_of(id) {
            None => DriftState::SelectionDangling,
            Some(t) => {
                let has_own_credential = t.get("experimental_bearer_token").is_some()
                    || t.get("env_key").is_some();
                DriftState::SelectedElsewhere {
                    provider_id: id.to_string(),
                    destination: t
                        .get("base_url")
                        .and_then(|b| b.as_str())
                        .map(str::to_string),
                    placeholder_would_be_sent: armed && !has_own_credential,
                }
            }
        },
    }
}

/// 顶层遗留的 `profile = "..."`。当前 Codex 报
/// `legacy profile = "x" config is no longer supported`，**整份配置加载失败** ——
/// 此时我们写得再对也没用，而 `is_intact` 只用 toml 解析、看不出这件事。
fn legacy_profile_key(cfg_path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(cfg_path).ok()?;
    let doc = text.parse::<toml::Value>().ok()?;
    doc.get("profile")
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

/// `$CODEX_HOME` 下的 `<name>.config.toml` 清单。`codex --profile <name>` 读它、
/// **完全忽略** config.toml 的 `model_provider` —— 一条我们写什么都不生效的静默旁路。
fn shadowing_profiles(cfg_path: &Path) -> Vec<String> {
    let Some(dir) = cfg_path.parent() else {
        return Vec::new();
    };
    let mut out: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.strip_suffix(".config.toml").map(str::to_string)
        })
        .collect();
    out.sort();
    out
}

/// 漂移告警文案。`None` = 无需告警。
///
/// 每支都必须说清三件事：**Codex 现在实际在做什么**、**这不是用户的密钥有问题**、
/// **怎么自助修**。不同支的第一件事不同，这正是要分支的原因。
pub(super) fn drift_warning(state: &DriftState) -> Option<String> {
    const FIX: &str = "处理：完全退出 Codex，再在本页点「停止」后重新「启动」以重写接入配置。";
    Some(match state {
        DriftState::Intact | DriftState::NotApplied => return None,

        DriftState::FellBackToOfficial { placeholder_armed } => {
            let head = "Codex 接入已失效：config.toml 里的顶层 `model_provider` 已被其它程序\
                 （Codex 自身或 cc-switch）改掉或删掉，Codex 正回落到官方 \
                 https://api.openai.com/v1，你的请求**没有走 SynaRoute**。";
            if *placeholder_armed {
                format!(
                    "{head}\n而 auth.json 里还留着旧版本 SynaRoute 写入的占位密钥，\
                     于是 Codex 会把它发给官方，报「Incorrect API key provided: synarout***roxy」\
                     —— **那不是你的 OpenAI 密钥有问题**。\n{FIX}\
                     （本版 SynaRoute 已不再写 auth.json；重新启动一次即会自动解除这份占位密钥。）"
                )
            } else {
                format!("{head}\n{FIX}")
            }
        }

        DriftState::SelectedElsewhere {
            provider_id,
            destination,
            placeholder_would_be_sent,
        } => {
            let dest = destination.as_deref().unwrap_or("（该 provider 未写 base_url）");
            let head = format!(
                "Codex 当前选中的是 provider `{provider_id}`（{dest}），\
                 不是 SynaRoute —— 你的请求没有走本地代理，故障转移/模型映射/用量统计都不生效。"
            );
            if *placeholder_would_be_sent {
                format!(
                    "{head}\n且该 provider 没有自带密钥，Codex 会把 auth.json 里旧版本 SynaRoute \
                     写入的占位密钥发往 {dest}，那边会回「密钥无效」。\n{FIX}"
                )
            } else {
                format!("{head}\n若这是你自己在 cc-switch 等工具里切换的结果，可忽略。\n{FIX}")
            }
        }

        DriftState::SelectionDangling => format!(
            "Codex 配置自相矛盾：顶层 `model_provider` 选中的 provider 在 \
             `[model_providers.*]` 里不存在。此时 Codex **启动即报错** \
             `Error: Model provider ... not found`，一个请求都发不出去 —— \
             所以你看到的不会是鉴权报错，而是压根打不开。\n{FIX}"
        ),

        DriftState::OurTablePointsElsewhere { destination } => format!(
            "Codex 里 SynaRoute 的 provider 表被改过：base_url 现在是 {} —— \
             不是本机代理的当前地址。最常见的成因是**代理端口变了而客户端配置没跟上**\
             （或被 cc-switch 写回了一份旧快照）。请求会打向那个地址，通常表现为连不上。\n{FIX}",
            destination.as_deref().unwrap_or("（空）")
        ),

        DriftState::ProfileShadowed { profiles } => format!(
            "检测到 Codex profile 配置：{}。用 `codex --profile <名字>` 启动时，Codex 读的是 \
             `<名字>.config.toml`，会**完全忽略** SynaRoute 写入的 `model_provider` —— \
             这条路上接入不生效，且不会有任何报错。若你平时用 profile 启动 Codex，\
             请把 `[model_providers.{}]` 与 `model_provider = \"{}\"` 一并写进那份 profile 文件。",
            profiles.join("、"),
            MCP_CLIENT_NAME,
            MCP_CLIENT_NAME
        ),

        DriftState::LegacyProfileKeyBreaksLoad { profile } => format!(
            "Codex 配置无法加载：config.toml 顶层还留着老写法 `profile = \"{profile}\"`，\
             当前版本的 Codex 会直接报 \
             `legacy profile config is no longer supported; use --profile {profile} with \
             {profile}.config.toml instead` 并拒绝启动。此时 SynaRoute 写什么都不生效。\n\
             处理：删掉 config.toml 里那一行 `profile = \"{profile}\"`。"
        ),
    })
}

// ---------------------------------------------------------------------------
// 预览
// ---------------------------------------------------------------------------

/// Codex 的工具配置预览（设置面板 / 分类页常驻告警都读它）。
///
/// `applied`（我们是否认为自己处于已接入态）由 [`believed_applied`] 从磁盘推导，
/// 不从前端传 —— 前端传进来的是挂载时的旧快照，而这里判的恰恰是「磁盘现在是什么样」。
pub(super) fn preview(endpoint: &str) -> AppResult<super::ToolConfigPreview> {
    let cfg = config_path()?;
    let auth = auth_path()?;
    // config.toml 必须脱敏：它可能含用户自己配的其它 provider 的 api_key / env_key，
    // 以及 MCP server 的环境变量。此前整份明文回显 —— 同一段密钥放 settings.json 会打码、
    // 放 config.toml 却不打码，口径分叉且泄露。
    let (cfg_exists, cfg_content) = read_preview_text(&cfg, true)?;
    let (auth_exists, auth_content) = read_preview_text(&auth, true)?;
    let state = drift_state(&cfg, &auth, endpoint, believed_applied(&cfg));
    Ok(super::ToolConfigPreview {
        category_id: crate::model::CategoryType::Codex,
        summary: "Codex：只写 ~/.codex/config.toml（model_provider=synaroute、\
                  [model_providers.synaroute] 含 base_url/wire_api/bearer 占位、可选顶层 model）。\
                  **不写 auth.json**，官方 ChatGPT 登录态原样保留。不写任何 ANTHROPIC_*。"
            .into(),
        mcp_registered: super::is_mcp_registered(crate::model::CategoryType::Codex),
        takeover_warning: drift_warning(&state),
        files: vec![
            super::ToolConfigFilePreview {
                path: cfg.display().to_string(),
                exists: cfg_exists,
                format: "toml".into(),
                content: cfg_content,
            },
            super::ToolConfigFilePreview {
                path: auth.display().to_string(),
                exists: auth_exists,
                format: "json".into(),
                content: auth_content,
            },
        ],
    })
}

/// 我们是否**认为**自己处于已接入态：`config.toml` 的 `.synaroute.bak` 或
/// 「凭空新建」标记存在。
///
/// 这两个副文件的语义就是「SynaRoute 动过这个文件、还没还原」（`restore_one` 成功后会删掉
/// 它们）。用它当判据而不是「代理在不在跑」：漂移的危险窗口恰恰覆盖「应用没开着」的那段
/// —— 用户几天没开 SynaRoute，某天打开 Codex 就撞上 401。
fn believed_applied(cfg_path: &Path) -> bool {
    super::backup_path_for(cfg_path).exists() || super::created_marker_path_for(cfg_path).exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试用临时文件（pid + 自增序号，避免并发用例互踩 —— 与 tools.rs 的 `temp_file` 同一手法。
    /// 刻意不用时间戳：本机实测 `timestamp_nanos` 的量化粒度只有 100ns，
    /// 并发下撞名率极高，见 CLAUDE.md 里 `db_copy_path` 那条）。
    fn temp_dir(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let d = std::env::temp_dir().join(format!(
            "synaroute_codex_test_{tag}_{}_{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    const EP: &str = "http://127.0.0.1:47101";

    fn write(p: &Path, s: &str) {
        std::fs::write(p, s).unwrap();
    }

    // ---- 接入写出的内容 ----

    /// 接入**不得**创建 auth.json，且 provider 表里不得有 `requires_openai_auth`。
    ///
    /// 这两条是本轮修复的核心：那份占位符在正常接入时从不外发（bearer 优先），
    /// 只在漂移后被发给**真实的 OpenAI**，换回一句指错方向的 401。
    #[test]
    fn apply_writes_bearer_and_never_creates_auth_json() {
        let d = temp_dir("apply_no_auth");
        let cfg = d.join("config.toml");
        let auth = d.join("auth.json");

        let msg = apply_at(&cfg, EP, Some("gpt-5.6-sol")).unwrap();
        assert!(!auth.exists(), "接入不得创建 auth.json");

        let doc = std::fs::read_to_string(&cfg).unwrap().parse::<toml::Value>().unwrap();
        assert_eq!(doc["model_provider"].as_str(), Some("synaroute"));
        assert_eq!(doc["model"].as_str(), Some("gpt-5.6-sol"));
        let p = &doc["model_providers"]["synaroute"];
        assert_eq!(p["base_url"].as_str(), Some("http://127.0.0.1:47101/v1"));
        assert_eq!(p["wire_api"].as_str(), Some("responses"));
        assert_eq!(p["experimental_bearer_token"].as_str(), Some(PROXY_PLACEHOLDER));
        assert!(
            p.get("requires_openai_auth").is_none(),
            "requires_openai_auth 会把 Codex 推回 OpenAI 凭据门禁，正是要摆脱的那条"
        );

        // 成功文案不得自相矛盾：旧实现把「未改动 auth.json」与「已写入 …auth.json（鉴权占位）」
        // 用 format! 拼进同一句给用户看 —— 同一句话既说没动又说写了。
        assert!(msg.contains("未改动 auth.json"));
        assert!(
            !msg.contains("鉴权占位）"),
            "不得声称写了 auth.json 占位：{msg}"
        );

        std::fs::remove_dir_all(&d).ok();
    }

    /// 老用户升级：provider 表里遗留的 `requires_openai_auth = true` 必须被**主动删掉**。
    /// 不删的话老用户静默保持旧行为 —— 「只对新用户生效的修复」是本仓反复踩过的坑。
    #[test]
    fn apply_strips_legacy_requires_openai_auth() {
        let d = temp_dir("strip_roa");
        let cfg = d.join("config.toml");
        write(
            &cfg,
            "model_provider = \"synaroute\"\n\
             [model_providers.synaroute]\n\
             name = \"SynaRoute\"\n\
             base_url = \"http://127.0.0.1:1/v1\"\n\
             wire_api = \"responses\"\n\
             requires_openai_auth = true\n",
        );
        apply_at(&cfg, EP, None).unwrap();
        let doc = std::fs::read_to_string(&cfg).unwrap().parse::<toml::Value>().unwrap();
        assert!(
            doc["model_providers"]["synaroute"].get("requires_openai_auth").is_none(),
            "遗留的 requires_openai_auth 必须被删掉"
        );
        std::fs::remove_dir_all(&d).ok();
    }

    /// 接入保留 config.toml 里其它一切（用户自己的 provider、MCP 注册、features 段）。
    #[test]
    fn apply_preserves_unrelated_tables() {
        let d = temp_dir("preserve");
        let cfg = d.join("config.toml");
        write(
            &cfg,
            "disable_response_storage = true\n\
             [features]\njs_repl = false\n\
             [model_providers.custom]\nname = \"别人\"\nbase_url = \"https://x.example/v1\"\n\
             [mcp_servers.synaroute]\ncommand = \"x.exe\"\n",
        );
        apply_at(&cfg, EP, None).unwrap();
        let doc = std::fs::read_to_string(&cfg).unwrap().parse::<toml::Value>().unwrap();
        assert_eq!(doc["disable_response_storage"].as_bool(), Some(true));
        assert_eq!(doc["features"]["js_repl"].as_bool(), Some(false));
        assert_eq!(doc["model_providers"]["custom"]["base_url"].as_str(), Some("https://x.example/v1"));
        assert!(doc["mcp_servers"]["synaroute"].get("command").is_some());
        std::fs::remove_dir_all(&d).ok();
    }

    // ---- is_intact 判的是身份，不是存在 ----

    /// 旧实现只查「base_url 非空」。指向第三方、或指着一个**已死的旧端口**都会被判完好，
    /// 于是漂移告警永远发不出来。cc-switch 存的 SynaRoute 档正是这个形状（无 bearer）。
    #[test]
    fn intact_requires_our_endpoint_and_our_bearer() {
        let d = temp_dir("intact");
        let cfg = d.join("config.toml");

        apply_at(&cfg, EP, None).unwrap();
        assert!(is_intact(&cfg, EP), "刚写完必须完好");

        // 端口漂移：非空但不是我们
        write(
            &cfg,
            "model_provider = \"synaroute\"\n\
             [model_providers.synaroute]\nbase_url = \"http://127.0.0.1:9999/v1\"\n\
             experimental_bearer_token = \"synaroute-proxy\"\n",
        );
        assert!(!is_intact(&cfg, EP), "base_url 不是本机当前端点 → 不完好");

        // 指向第三方且没有 bearer（cc-switch 写回的形状）
        write(
            &cfg,
            "model_provider = \"synaroute\"\n\
             [model_providers.synaroute]\nbase_url = \"https://relay.example/v1\"\n",
        );
        assert!(!is_intact(&cfg, EP), "指向第三方 + 无 bearer → 绝不能判完好");

        // 缺 bearer（占位符会成为唯一凭据）
        write(
            &cfg,
            &format!(
                "model_provider = \"synaroute\"\n\
                 [model_providers.synaroute]\nbase_url = \"{}/v1\"\n",
                EP
            ),
        );
        assert!(!is_intact(&cfg, EP), "缺 bearer → 不完好");

        // 选中别人
        write(&cfg, "model_provider = \"custom\"\n[model_providers.custom]\nbase_url = \"https://x/v1\"\n");
        assert!(!is_intact(&cfg, EP));

        std::fs::remove_dir_all(&d).ok();
    }

    // ---- 占位符判据：宽松 vs 严格 ----

    /// `codex login --with-api-key` 写出的是**两个字段**。旧判据 `obj.len() == 1` 判否，
    /// 失效方向是**静默放行**：假 key 照样在发，而告警与清理同时哑掉。
    #[test]
    fn two_field_auth_from_codex_login_is_still_recognized_as_ours() {
        let d = temp_dir("twofield");
        let auth = d.join("auth.json");

        write(&auth, r#"{"auth_mode":"apikey","OPENAI_API_KEY":"synaroute-proxy"}"#);
        assert!(auth_carries_our_placeholder(&auth), "两字段形态必须被认出来");
        assert!(auth_is_only_our_placeholder(&auth), "只有我们的 key + auth_mode → 可整份删");

        // 单字段（旧版 SynaRoute 自己写的）同样认得
        write(&auth, r#"{"OPENAI_API_KEY":"synaroute-proxy"}"#);
        assert!(auth_carries_our_placeholder(&auth));
        assert!(auth_is_only_our_placeholder(&auth));

        std::fs::remove_dir_all(&d).ok();
    }

    /// 真实凭据在任何形态下都不能被当成我们的（这是「删整个文件」那条路的护栏）。
    #[test]
    fn real_credentials_are_never_treated_as_ours() {
        let d = temp_dir("realcred");
        let auth = d.join("auth.json");

        // ChatGPT OAuth
        write(
            &auth,
            r#"{"OPENAI_API_KEY":null,"auth_mode":"chatgpt","tokens":{"access_token":"x"}}"#,
        );
        assert!(!auth_carries_our_placeholder(&auth));
        assert!(!auth_is_only_our_placeholder(&auth));

        // 用户自己的真实 api key
        write(&auth, r#"{"OPENAI_API_KEY":"sk-realkey","auth_mode":"apikey"}"#);
        assert!(!auth_carries_our_placeholder(&auth));
        assert!(!auth_is_only_our_placeholder(&auth));

        // 我们的占位符 + OAuth tokens 并存（混合态）：认得出是我们的 key（要摘），
        // 但**不可**整份删（会连 tokens 一起丢）。
        write(
            &auth,
            r#"{"OPENAI_API_KEY":"synaroute-proxy","tokens":{"access_token":"x"}}"#,
        );
        assert!(auth_carries_our_placeholder(&auth), "key 是我们的 → 要摘");
        assert!(
            !auth_is_only_our_placeholder(&auth),
            "含 OAuth tokens → 绝不能整份删"
        );

        std::fs::remove_dir_all(&d).ok();
    }

    // ---- 解除旧占位符 ----

    /// 有 `.bak`（旧版接入时备份过真凭据）→ 交还真凭据并删 .bak。
    #[test]
    fn disarm_restores_real_credentials_from_backup() {
        let d = temp_dir("disarm_bak");
        let auth = d.join("auth.json");
        let bak = super::super::backup_path_for(&auth);
        write(&auth, r#"{"OPENAI_API_KEY":"synaroute-proxy"}"#);
        write(&bak, r#"{"auth_mode":"chatgpt","tokens":{"access_token":"real"}}"#);

        let note = disarm_legacy_placeholder_auth(&auth).unwrap();
        assert!(note.is_some(), "确实动了就要有说明");
        let after = std::fs::read_to_string(&auth).unwrap();
        assert!(after.contains("real"), "真凭据必须交还：{after}");
        assert!(!bak.exists(), "交还后删掉 .bak");
        assert!(
            prerestore_path_for(&auth).exists(),
            "改写前必须留现场（可回滚）"
        );
        std::fs::remove_dir_all(&d).ok();
    }

    /// 无 `.bak` 且文件纯粹是占位符 → 删掉整个文件，并清掉「凭空新建」标记。
    ///
    /// 标记不清的后果是数据丢失级：日后 restore 会走 marker 支路 `remove_file`，
    /// 删掉用户**后来自己 `codex login` 出来的**真凭据，而那条支路不留 prerestore。
    #[test]
    fn disarm_deletes_pure_placeholder_and_clears_created_marker() {
        let d = temp_dir("disarm_pure");
        let auth = d.join("auth.json");
        let marker = super::super::created_marker_path_for(&auth);
        write(&auth, r#"{"OPENAI_API_KEY":"synaroute-proxy"}"#);
        write(&marker, "");

        assert!(disarm_legacy_placeholder_auth(&auth).unwrap().is_some());
        assert!(!auth.exists(), "纯占位符文件应整份删除");
        assert!(!marker.exists(), "凭空新建标记必须一并清掉，否则日后会误删真凭据");
        std::fs::remove_dir_all(&d).ok();
    }

    /// 混合形态（用户后来自己 login 过）→ 只摘我们的 key，其余字段保留。
    #[test]
    fn disarm_only_removes_our_key_from_a_mixed_auth_file() {
        let d = temp_dir("disarm_mixed");
        let auth = d.join("auth.json");
        write(
            &auth,
            r#"{"OPENAI_API_KEY":"synaroute-proxy","auth_mode":"apikey","tokens":{"access_token":"real"}}"#,
        );

        assert!(disarm_legacy_placeholder_auth(&auth).unwrap().is_some());
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&auth).unwrap()).unwrap();
        assert!(v.get("OPENAI_API_KEY").is_none(), "我们的 key 必须摘掉");
        assert!(v["tokens"]["access_token"].as_str() == Some("real"), "用户的 tokens 必须保留");
        assert!(
            v.get("auth_mode").is_none(),
            "auth_mode=apikey 失去了它指向的 key，留着会让 Codex 判成 api-key 模式却找不到 key"
        );
        std::fs::remove_dir_all(&d).ok();
    }

    /// 幂等：没有我们的占位符时什么都不做（也不留现场、不误删）。
    #[test]
    fn disarm_is_a_noop_when_there_is_nothing_of_ours() {
        let d = temp_dir("disarm_noop");
        let auth = d.join("auth.json");
        write(&auth, r#"{"auth_mode":"chatgpt","tokens":{"access_token":"real"}}"#);
        assert!(disarm_legacy_placeholder_auth(&auth).unwrap().is_none());
        assert!(auth.exists());
        assert!(!prerestore_path_for(&auth).exists(), "没动就不该留现场");

        // 文件不存在也不能报错
        let missing = d.join("nope.json");
        assert!(disarm_legacy_placeholder_auth(&missing).unwrap().is_none());
        std::fs::remove_dir_all(&d).ok();
    }

    // ---- 漂移分支 ----

    /// 每种漂移形态的文案必须点名**那个形态下 Codex 实际的行为**。
    /// 指错方向的告警比没有告警更糟：它会让用户去查一个根本不会发生的问题。
    #[test]
    fn drift_message_matches_what_codex_actually_does_in_each_shape() {
        let d = temp_dir("drift");
        let cfg = d.join("config.toml");
        let auth = d.join("auth.json");

        // ① 完好 → 不报警
        apply_at(&cfg, EP, None).unwrap();
        assert_eq!(drift_state(&cfg, &auth, EP, true), DriftState::Intact);
        assert!(drift_warning(&DriftState::Intact).is_none());

        // ② 未接入 + config 不指向我们 → 不报警（噪音）
        write(&cfg, "model = \"x\"\n");
        assert_eq!(drift_state(&cfg, &auth, EP, false), DriftState::NotApplied);

        // ③ model_provider 键缺失 + 占位符武装 → 回落官方，**这一支才有那句 401**
        write(&auth, r#"{"OPENAI_API_KEY":"synaroute-proxy"}"#);
        let s = drift_state(&cfg, &auth, EP, true);
        assert_eq!(s, DriftState::FellBackToOfficial { placeholder_armed: true });
        let w = drift_warning(&s).unwrap();
        assert!(w.contains("api.openai.com"), "要点名假 key 会发去哪：{w}");
        assert!(w.contains("不是你的 OpenAI 密钥有问题"), "必须纠正错误方向：{w}");

        // ③b 同形态但占位符已解除 → 仍要报「没走 SynaRoute」，但不得再提 401
        std::fs::remove_file(&auth).unwrap();
        let s = drift_state(&cfg, &auth, EP, true);
        assert_eq!(s, DriftState::FellBackToOfficial { placeholder_armed: false });
        let w = drift_warning(&s).unwrap();
        assert!(!w.contains("Incorrect API key"), "占位符已解除就不该提那句 401：{w}");

        // ④ 选中第三方、该表自带 bearer → 占位符不外发，文案不得说「假密钥正被发往…」
        write(
            &cfg,
            "model_provider = \"custom\"\n[model_providers.custom]\n\
             base_url = \"https://relay.example/v1\"\nexperimental_bearer_token = \"sk-theirs\"\n",
        );
        write(&auth, r#"{"OPENAI_API_KEY":"synaroute-proxy"}"#);
        let s = drift_state(&cfg, &auth, EP, true);
        assert_eq!(
            s,
            DriftState::SelectedElsewhere {
                provider_id: "custom".into(),
                destination: Some("https://relay.example/v1".into()),
                placeholder_would_be_sent: false,
            }
        );
        let w = drift_warning(&s).unwrap();
        assert!(w.contains("relay.example"), "要点名真实去处：{w}");
        assert!(!w.contains("api.openai.com"), "不得指向 openai（会指错方向）：{w}");

        // ④b 该表没有自带凭据 → 占位符真的会被发过去
        write(
            &cfg,
            "model_provider = \"custom\"\n[model_providers.custom]\nbase_url = \"https://relay.example/v1\"\n",
        );
        let s = drift_state(&cfg, &auth, EP, true);
        assert!(matches!(
            s,
            DriftState::SelectedElsewhere { placeholder_would_be_sent: true, .. }
        ));
        assert!(drift_warning(&s).unwrap().contains("占位密钥发往"));

        // ⑤ 选中项悬空 → Codex 硬报错、不发请求，文案**不得**提 401
        write(&cfg, "model_provider = \"synaroute\"\n");
        let s = drift_state(&cfg, &auth, EP, true);
        assert_eq!(s, DriftState::SelectionDangling);
        let w = drift_warning(&s).unwrap();
        assert!(w.contains("not found"), "要引用 Codex 的原话：{w}");
        assert!(!w.contains("401"), "这一支压根不会出现 401：{w}");

        // ⑥ 我们的表指向别处（端口漂移）
        write(
            &cfg,
            "model_provider = \"synaroute\"\n[model_providers.synaroute]\n\
             base_url = \"http://127.0.0.1:9999/v1\"\nexperimental_bearer_token = \"synaroute-proxy\"\n",
        );
        let s = drift_state(&cfg, &auth, EP, true);
        assert_eq!(
            s,
            DriftState::OurTablePointsElsewhere {
                destination: Some("http://127.0.0.1:9999/v1".into())
            }
        );
        assert!(drift_warning(&s).unwrap().contains("9999"));

        std::fs::remove_dir_all(&d).ok();
    }

    /// profile 旁路：两种都让「我们写的字段在位」变得毫无意义，而 toml 解析看不出来。
    #[test]
    fn profile_shapes_bypass_everything_we_write() {
        let d = temp_dir("profile");
        let cfg = d.join("config.toml");
        let auth = d.join("auth.json");

        // `<name>.config.toml` 存在 → `codex --profile name` 完全忽略 config.toml
        apply_at(&cfg, EP, None).unwrap();
        write(&d.join("mine.config.toml"), "model = \"x\"\n");
        let s = drift_state(&cfg, &auth, EP, true);
        assert_eq!(s, DriftState::ProfileShadowed { profiles: vec!["mine".into()] });
        let w = drift_warning(&s).unwrap();
        assert!(w.contains("mine"), "要点名那份 profile：{w}");
        assert!(w.contains("--profile"));
        std::fs::remove_file(d.join("mine.config.toml")).unwrap();

        // 顶层遗留 `profile = "..."` → 当前 Codex 整份配置加载失败
        let mut t = std::fs::read_to_string(&cfg).unwrap();
        t.insert_str(0, "profile = \"old\"\n");
        write(&cfg, &t);
        let s = drift_state(&cfg, &auth, EP, true);
        assert_eq!(s, DriftState::LegacyProfileKeyBreaksLoad { profile: "old".into() });
        assert!(drift_warning(&s).unwrap().contains("no longer supported"));

        std::fs::remove_dir_all(&d).ok();
    }

    /// 完好态下即使有 profile 文件也要报 —— 那条路上接入是**静默**不生效的，
    /// 而「静默」正是本仓最忌的形态。
    #[test]
    fn profile_shadowing_is_reported_even_when_our_config_is_intact() {
        let d = temp_dir("profile_intact");
        let cfg = d.join("config.toml");
        let auth = d.join("auth.json");
        apply_at(&cfg, EP, None).unwrap();
        assert_eq!(drift_state(&cfg, &auth, EP, true), DriftState::Intact);
        write(&d.join("work.config.toml"), "model = \"x\"\n");
        assert!(
            matches!(drift_state(&cfg, &auth, EP, true), DriftState::ProfileShadowed { .. }),
            "接入完好也要报 profile 旁路"
        );
        std::fs::remove_dir_all(&d).ok();
    }

    /// 接入**逐字节不动**已存在的 auth.json。
    ///
    /// 这条替代了旧的 `codex_auth_apply_backs_up_oauth_and_restore_brings_it_back_verbatim`：
    /// 那条测的是「整份替换掉 OAuth 之后还能不能还原回来」，而现在的性质更强 ——
    /// 压根不动它，于是「还原不回来」这个风险面整个消失。
    #[test]
    fn apply_never_touches_an_existing_oauth_auth_json() {
        let d = temp_dir("keep_oauth");
        let cfg = d.join("config.toml");
        let auth = d.join("auth.json");
        let oauth = r#"{"OPENAI_API_KEY":null,"auth_mode":"chatgpt","tokens":{"access_token":"real","refresh_token":"rt"},"last_refresh":"2026-06-25T13:01:56Z"}"#;
        write(&auth, oauth);

        apply_at(&cfg, EP, None).unwrap();

        assert_eq!(
            std::fs::read_to_string(&auth).unwrap(),
            oauth,
            "接入不得改动用户的 ChatGPT 登录态"
        );
        assert!(
            !super::super::backup_path_for(&auth).exists(),
            "既然不动它，就不该为它建 .bak（建了反而是把真凭据复制了一份）"
        );
        std::fs::remove_dir_all(&d).ok();
    }

    /// 占位符只有一份事实来源：Codex 的 bearer、桌面端 gateway、Claude CLI 的
    /// `ANTHROPIC_AUTH_TOKEN` 必须全部引用同一个常量。
    ///
    /// 抄成三份的后果不是「不一致」这么轻 —— 任何「这个 token 是不是我们写的」的判断都会
    /// 答「不是我们的」，清理与告警一起静默失效（fail-open）。编译器管不到字面量这条缝。
    ///
    /// ⚠️ **判据必须放过常量自己的定义行**，否则这个门会永远报自己 —— 与
    /// CLAUDE.md 里「密钥扫描门不再报自己」是同一个坑（第一版就踩了）。
    #[test]
    fn placeholder_has_a_single_source_of_truth() {
        // 本仓的 .rs 是 CRLF（见 .gitattributes）。`include_str!` 给的是原始字节，
        // 不先归一行尾，下面那个 split 找不到锚点 → `prod` 变成整份文件、把测试段里的
        // 字面量也算成违规。**判据自己失效的方向必须是响亮的**：所以下面还有一条反向断言。
        let src = include_str!("../tools.rs").replace("\r\n", "\n");
        // 只看生产段（尾部 `#[cfg(test)] mod tests` 之前），与棘轮脚本同一口径。
        // 用 rfind 取**最后**一处：文件中间还有若干 `#[cfg(test)]` 单项（如 REAL_CLEANUP_CALLS）。
        let cut = src
            .rfind("\n#[cfg(test)]\nmod tests")
            .expect("tools.rs 尾部应有 `#[cfg(test)] mod tests`");
        let prod = &src[..cut];
        let offenders: Vec<&str> = prod
            .lines()
            .filter(|l| {
                let t = l.trim_start();
                !t.starts_with("//") && t.contains("\"synaroute-proxy\"")
            })
            // 放过唯一合法的那一行：常量自己的定义。
            .filter(|l| !l.contains("const PROXY_PLACEHOLDER"))
            .collect();
        assert!(
            offenders.is_empty(),
            "tools.rs 生产段里仍有裸的 \"synaroute-proxy\" 字面量，必须改用 PROXY_PLACEHOLDER：{offenders:?}"
        );
        // 反向判据：门不能因为「压根没找到那个字符串」而恒绿（同 `invoke-command-must-exist`
        // 那条教训 —— 解析到 0 个候选时要主动判失败）。
        assert!(
            prod.contains("const PROXY_PLACEHOLDER"),
            "常量定义都找不到，说明这条判据在空转"
        );
        assert_eq!(PROXY_PLACEHOLDER, "synaroute-proxy");
    }
}
