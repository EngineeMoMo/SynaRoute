//! 目标工具接入模块 —— 把本地代理端点写入三个工具的真实配置文件。
//!
//! 硬规则（dev-hard-rules，用户强制要求）：
//! 1. 改写任何配置文件前，先备份为 *.synaroute.bak
//! 2. 原子写（临时文件 → 重命名替换）
//! 3. 路径全部动态解析（dirs / env），禁止硬编码本机路径
//!
//! 接入机制（三端严格分离，禁止混写）：
//! - **Claude CLI**：`~/.claude/settings.json`
//!   - 写：env.ANTHROPIC_BASE_URL / ANTHROPIC_AUTH_TOKEN(占位) / GATEWAY_MODEL_DISCOVERY
//!   - 写：env.ANTHROPIC_MODEL + 顶层 `model`（主 Key 首个可服务**对外名**；策略 A）
//!   - **不写** ANTHROPIC_DEFAULT_{HAIKU,SONNET,OPUS}_MODEL（避免 /model 三个 Custom 同名）
//!   - 应用时**删除** env 里残留的三档 DEFAULT_*（清 cc-switch/旧版写入）
//! - **Codex**：`~/.codex/config.toml` + `auth.json`（OpenAI 形态，无 ANTHROPIC_*）
//!   - 写：model_provider=synaroute、[model_providers.synaroute]、可选顶层 model、OPENAI_API_KEY 占位
//! - **Claude 桌面端**：切「第三方部署模式（deploymentMode=3p）」+ 预置 gateway 配置档
//!   （对齐 cc-switch）：写 `<Claude|Claude-3p>/claude_desktop_config.json` 的 deploymentMode、
//!   `<Claude-3p>/configLibrary/{ID}.json`（gateway 端点/占位 key/模型）与 `_meta.json`。
//!   凭据预填齐 → 桌面端跳过 get-started 登录。不写 CLI settings、不写 ANTHROPIC_*。

use crate::error::{AppError, AppResult};
use crate::model::{CategoryType, ProviderKey};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// 备份文件后缀
const BACKUP_SUFFIX: &str = "synaroute.bak";

/// Codex 专属接入逻辑（判据密度最高的一端，单独成模块）。
///
/// 子模块能访问 `tools` 的私有项（Rust 的可见性是「定义模块及其后代」），
/// 故 `read_preview_text` 等**无需放宽可见性**。
pub(crate) mod codex;

/// 写入 / 备份 / 回滚 / 还原这一层。抽出来的判据见该模块头。
mod fsops;

use fsops::{
    backup_and_write_bytes, backup_and_write_json, backup_path_for, created_marker_path_for,
    prerestore_path_for, read_json_or_empty, restore_one, with_rollback,
    write_json_without_locking_snapshot, write_without_locking_snapshot,
};

/// 写进客户端配置的**鉴权占位值**。代理剥掉入站鉴权头、按路由 Key 注入真实密钥，
/// 故此值只需非空以让客户端走 bearer 鉴权流程，代理侧不校验它。
///
/// # 为什么必须是**一个**常量
///
/// 它此前在仓库里有三份：`CODEX_AUTH_PLACEHOLDER`、`DESKTOP_GATEWAY_PLACEHOLDER`，
/// 以及 Claude CLI 写 `ANTHROPIC_AUTH_TOKEN` 时的裸字面量。改了常量而漏掉字面量，
/// 后果不是「不一致」这么轻 —— 任何「这个 token 是不是我们写的」的判断都会答「不是我们的」，
/// 于是清理/告警一起静默失效（与 Codex 那个 `obj.len() == 1` 同型的 fail-open）。
/// `placeholder_has_a_single_source_of_truth` 那条测试把这件事钉住。
pub(crate) const PROXY_PLACEHOLDER: &str = "synaroute-proxy";


/// SynaRoute 在 Claude 桌面端 `configLibrary` 里的专属配置档 ID。
/// 刻意区别于 cc-switch 的 `00000000-0000-4000-8000-000000157210`：两者可**共存**于
/// `_meta.entries`，`appliedId` 指向当前接入者。还原时只删本档、绝不动 cc-switch 的档。
/// 末段 `000053796e61` 是 "Syna"（S=0x53 y=0x79 n=0x6e a=0x61）的 hex，便于辨识。
const DESKTOP_PROFILE_ID: &str = "00000000-0000-4000-8000-000053796e61";
/// 本档在 `_meta.entries` 里显示的名称。
const DESKTOP_PROFILE_NAME: &str = "SynaRoute";
/// 桌面端 gateway 档的 `inferenceGatewayApiKey` 占位值。见 [`PROXY_PLACEHOLDER`]。
const DESKTOP_GATEWAY_PLACEHOLDER: &str = PROXY_PLACEHOLDER;
/// 两个部署目录里的部署配置文件名。
const DESKTOP_CONFIG_FILE: &str = "claude_desktop_config.json";
/// 3p 目录下存放配置档的子目录名。
const DESKTOP_CONFIG_LIBRARY: &str = "configLibrary";
/// configLibrary 里的元数据文件名（登记各档 id/name 与当前 appliedId）。
const DESKTOP_META_FILE: &str = "_meta.json";

/// 将某分类的代理端点写入对应目标工具配置。返回人类可读的结果说明。
///
/// `models`：`discoverable_models` 口径 —— 多 Key 取**并集**（备用 Key 独有的也在）。有序。
/// `keys`：按优先级排序的启用 Key。**桌面端与 Codex 都用**：前者推导能力断言，后者推导
/// `supported_reasoning_levels` 与 `context_window` —— 传空切片会让 Codex 的档位选择器消失。
/// - **Claude CLI only**：取首个写 env.ANTHROPIC_MODEL + 顶层 `model`；并清除三档 DEFAULT_* 残留。
/// - **Codex only**：整份写进 `model_catalog_json` 模型目录；顶层 `model` **仅在缺失/不可服务时**写。
/// - **桌面端**：整份列表写进 gateway 档的 `inferenceModels`（切 3p 部署模式，见 apply_claude_desktop）。
pub fn apply(
    category: CategoryType,
    endpoint: &str,
    models: &[String],
    keys: &[ProviderKey],
) -> AppResult<String> {
    let first = models.first().map(String::as_str);
    match category {
        CategoryType::ClaudeCli => apply_claude_cli(endpoint, first),
        CategoryType::Codex => codex::apply(endpoint, models, keys),
        CategoryType::ClaudeDesktop => apply_claude_desktop(endpoint, models, keys),
    }
}

// ---- Claude CLI ----

fn claude_cli_settings_path() -> AppResult<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| AppError::ToolConfig("无法定位用户目录".into()))?;
    Ok(home.join(".claude").join("settings.json"))
}

fn apply_claude_cli(endpoint: &str, default_model: Option<&str>) -> AppResult<String> {
    let path = claude_cli_settings_path()?;
    apply_claude_cli_at(&path, endpoint, default_model)
}

/// 可测入口：写入指定 settings.json（生产走 `claude_cli_settings_path`）。
fn apply_claude_cli_at(
    path: &Path,
    endpoint: &str,
    default_model: Option<&str>,
) -> AppResult<String> {
    let mut root = read_json_or_empty(path)?;

    // 确保 env 对象存在
    let env = root
        .as_object_mut()
        .ok_or_else(|| AppError::ToolConfig("settings.json 顶层非对象".into()))?
        .entry("env")
        .or_insert_with(|| Value::Object(Default::default()));
    let env_obj = env
        .as_object_mut()
        .ok_or_else(|| AppError::ToolConfig("env 非对象".into()))?;

    env_obj.insert("ANTHROPIC_BASE_URL".into(), Value::String(endpoint.to_string()));
    // 代理侧不校验 token，但工具要求存在，写占位。**用常量，不写字面量** —— 见 PROXY_PLACEHOLDER。
    env_obj
        .entry("ANTHROPIC_AUTH_TOKEN".to_string())
        .or_insert_with(|| Value::String(PROXY_PLACEHOLDER.into()));
    // 开启网关模型发现：Claude Code 默认不调用 <base>/v1/models，必须显式置 1 才会拉取代理
    // 暴露的可选模型填充 /model 选择器（需 CLI ≥ v2.1.129）。强制写 1，这正是 SynaRoute
    // 多 Key 路由要生效的前提。
    // CLI 只接受 id 以 "claude"/"anthropic" 开头的模型，其它静默过滤。代理侧对非合规名
    // （如 grok-4.5）自动包成 `claude-synaroute-<name>` 暴露，resolve 时再剥前缀；映射
    // 对外名若想出现在选择器里仍可直接写成 claude-*（无需包装）。
    env_obj.insert(
        "CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY".into(),
        Value::String("1".into()),
    );

    // 策略 A（用户拍板）：只写 ANTHROPIC_MODEL + 顶层 model（对外名），
    // 不写 ANTHROPIC_DEFAULT_{HAIKU,SONNET,OPUS}_MODEL —— 内置三档靠代理 resolve_model，
    // 避免 /model 出现三个「Custom * 都是同一个 id」。
    // 同时删除 env 里残留的三档 DEFAULT_*（旧版/cc-switch 可能写入）。
    for k in [
        "ANTHROPIC_DEFAULT_HAIKU_MODEL",
        "ANTHROPIC_DEFAULT_SONNET_MODEL",
        "ANTHROPIC_DEFAULT_OPUS_MODEL",
    ] {
        env_obj.remove(k);
    }

    let model_note = if let Some(m) = default_model.map(str::trim).filter(|s| !s.is_empty()) {
        env_obj.insert("ANTHROPIC_MODEL".into(), Value::String(m.to_string()));
        // 顶层 model：/model 当前默认；覆盖 claude-synaroute-* 等 Custom 残留
        if let Some(obj) = root.as_object_mut() {
            obj.insert("model".into(), Value::String(m.to_string()));
        }
        format!("，默认模型={m}（未写三档 DEFAULT_*）")
    } else {
        String::new()
    };

    backup_and_write_json(path, &root)?;
    Ok(format!(
        "已写入 Claude CLI 配置：{}（ANTHROPIC_BASE_URL={endpoint}{model_note}），原文件已备份",
        path.display()
    ))
}

// ---- Claude 桌面端 ----
//
// 桌面端不像 CLI 那样读 ANTHROPIC_BASE_URL —— 它有自己的「部署模式（deploymentMode）」概念：
// `1p`=官方后端（走 get-started 登录），`3p`=第三方 inference gateway（预置好凭据即跳过登录）。
// 早期实现往 `%APPDATA%\Roaming\Claude\claude_desktop_config.json` 写 `{"baseUrl":...}`——位置
// 与字段皆错（桌面端在 Windows 读 %LOCALAPPDATA%，且无 baseUrl 概念、启动时会用自己的
// preferences 覆盖该文件），故从未生效、桌面端始终停在 get-started。
//
// 现对齐 cc-switch 的真实机制（本机已有其生效样本作为字段/布局权威依据）：
// - `<Claude>/claude_desktop_config.json`      → 合并写 deploymentMode="3p"
// - `<Claude-3p>/claude_desktop_config.json`   → 合并写 deploymentMode="3p"（保留 preferences 等）
// - `<Claude-3p>/configLibrary/{ID}.json`      → gateway 档（inferenceProvider/BaseUrl/ApiKey/…）
// - `<Claude-3p>/configLibrary/_meta.json`     → entries[] 登记本档 + appliedId 指向本档
// 凭据（BaseUrl+占位 ApiKey+bearer）预填齐 → 桌面端认为环境已配好 → 跳过 get-started。
// 与 cc-switch 用独立 DESKTOP_PROFILE_ID **共存**：还原只动本档，绝不误删 cc-switch 的档。

/// 定位 Claude 桌面端的两个部署基目录：`normal`（官方，如 `Claude`）与 `threep`（第三方，如
/// `Claude-3p`）。Windows 用 `%LOCALAPPDATA%`，macOS/Linux 用 `~/Library/Application Support`
/// 等 `data_dir`。找不到精确名时扫描以 `Claude` 开头的目录兜底（区分是否带 `-3p` 后缀）。
fn claude_desktop_dirs() -> AppResult<(PathBuf, PathBuf)> {
    // 桌面端数据在 Windows 落 %LOCALAPPDATA%（与 CLI 的 %APPDATA% 不同！早期 bug 正源于此）。
    // 非 Windows 走 data_dir（macOS = ~/Library/Application Support）。
    #[cfg(windows)]
    let base = dirs::data_local_dir();
    #[cfg(not(windows))]
    let base = dirs::data_dir();
    let base = base.ok_or_else(|| AppError::ToolConfig("无法定位桌面端数据目录".into()))?;

    let normal = pick_desktop_dir(&base, false).unwrap_or_else(|| base.join("Claude"));
    let threep = pick_desktop_dir(&base, true).unwrap_or_else(|| base.join("Claude-3p"));
    Ok((normal, threep))
}

/// 在 `base` 下挑选桌面端目录：`want_3p=true` 找第三方目录（名以 `Claude` 开头且含 `-3p`），
/// `false` 找官方目录（以 `Claude` 开头且不含 `-3p`）。精确名（`Claude`/`Claude-3p`）优先；
/// 否则扫描现有目录取排序首个。都没有则返回 None（调用方回退到精确名）。
fn pick_desktop_dir(base: &Path, want_3p: bool) -> Option<PathBuf> {
    let exact = base.join(if want_3p { "Claude-3p" } else { "Claude" });
    if exact.is_dir() {
        return Some(exact);
    }
    let mut matches: Vec<PathBuf> = std::fs::read_dir(base)
        .ok()?
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| e.path())
        .filter(|p| {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            name.starts_with("Claude") && (name.contains("-3p") == want_3p)
        })
        .collect();
    matches.sort();
    matches.into_iter().next()
}

/// 3p 目录下的 gateway 配置档路径 `configLibrary/{DESKTOP_PROFILE_ID}.json`。
fn desktop_profile_path(threep: &Path) -> PathBuf {
    threep
        .join(DESKTOP_CONFIG_LIBRARY)
        .join(format!("{DESKTOP_PROFILE_ID}.json"))
}

/// 3p 目录下的元数据文件路径 `configLibrary/_meta.json`。
fn desktop_meta_path(threep: &Path) -> PathBuf {
    threep.join(DESKTOP_CONFIG_LIBRARY).join(DESKTOP_META_FILE)
}

fn apply_claude_desktop(
    endpoint: &str,
    models: &[String],
    keys: &[ProviderKey],
) -> AppResult<String> {
    let (normal, threep) = claude_desktop_dirs()?;
    let normal_config = normal.join(DESKTOP_CONFIG_FILE);
    let threep_config = threep.join(DESKTOP_CONFIG_FILE);
    let profile = desktop_profile_path(&threep);
    let meta = desktop_meta_path(&threep);

    // 接入前的 appliedId（可能是 cc-switch 的档）。记下来，还原时精确交还给它，
    // 而不是乱指 entries 首个把用户静默切到另一个供应商档。须在写 _meta **之前**读。
    let prev_applied = read_desktop_applied_id(&meta).filter(|id| id != DESKTOP_PROFILE_ID);

    // 四个文件一次写完，任一步失败整体回滚（避免半配置：如 deploymentMode 已切 3p 但 gateway
    // 档没写 → 桌面端进 3p 却无凭据、反而卡死）。与 Codex 双文件接入同一套原子保证。
    let msg = with_rollback(
        &[
            normal_config.clone(),
            threep_config.clone(),
            profile.clone(),
            meta.clone(),
        ],
        || {
            apply_desktop_at(
                &normal_config,
                &threep_config,
                &profile,
                &meta,
                endpoint,
                models,
                keys,
            )
        },
    )?;

    // 接入成功才记录（失败已回滚，接入前状态未变，不该留记录）。
    record_desktop_prev_applied_id(prev_applied.as_deref());

    // 清理早期失效实现的残留（旧路径 %APPDATA%\Roaming\Claude 的 baseUrl 键及其 .bak）。
    // 刻意放在 with_rollback **之外、apply_desktop_at 之后**：它动的是真实用户目录且属
    // best-effort，不该影响接入结果；也因此不能放进 apply_desktop_at —— 否则单测调用可测入口
    // 时会对用户真实 %APPDATA%\Roaming\Claude 执行删改（测试非 hermetic）。
    cleanup_legacy_desktop_residue();

    Ok(msg)
}

/// 可测入口：把 3p 部署模式与 gateway 档写入指定的四个文件。
///
/// `keys` 供推导每条模型的能力断言（`supports1m` 等，见 [`build_desktop_model_entries`]）；
/// 传空切片时退化为「无窗口数据」，即 `supports1m` 一律不写（保守，不做无依据的断言）。
fn apply_desktop_at(
    normal_config: &Path,
    threep_config: &Path,
    profile: &Path,
    meta: &Path,
    endpoint: &str,
    models: &[String],
    keys: &[ProviderKey],
) -> AppResult<String> {
    // gateway 档读一次，用于合并写 + 缺省模型回退。
    let existing = read_json_or_empty(profile)?;

    // 有效模型：优先用入参；入参为空（如改端口时主 Key 恰无可服务模型）时回退到档内已有的
    // inferenceModels，避免把之前接入写好的模型清单擦掉、或让「仅改端点」的重写失败。
    let effective: Vec<String> = if models.is_empty() {
        existing_inference_model_names(&existing)
    } else {
        models.to_vec()
    };

    // 仍为空 → 真正的死局（首次接入且无任何可服务模型）：桌面端在 3p 模式下靠 gateway 档的
    // inferenceModels 列出可选模型（cc-switch 样本恒带 6 个）。若空，桌面端只能依赖「启动那一刻
    // 本地代理在线」的运行时发现，一旦失败就 adminList/discovered 双空 → models_not_discovered
    // → 连会话都开不起来，症状与「卡在 get-started」同样难排查。宁可明确报错，让用户先配模型。
    if effective.is_empty() {
        return Err(AppError::ToolConfig(
            "桌面端接入需要至少一个可服务模型：当前分类启用的 Key 一个模型都没有配置。\
             请先在「模型映射」里为该分类配置模型后重试。"
                .into(),
        ));
    }

    // 1) 两个部署配置：合并写 deploymentMode="3p"，保留 preferences 等既有键。
    write_deployment_mode(normal_config, "3p")?;
    write_deployment_mode(threep_config, "3p")?;

    // 2) gateway 档：预填端点 + 占位 key + bearer + 模型清单（合并写，保留档内其它既有键）。
    //    每条模型的能力断言（supports1m / anthropicFamilyTier）由 Key 数据推导，见
    //    build_desktop_model_entries——不做无依据的断言。
    let entries = build_desktop_model_entries(&effective, keys);
    let profile_json = build_gateway_profile(existing, endpoint, &entries);
    crate::secret::atomic_write(profile, &serde_json::to_vec_pretty(&profile_json)?)?; // 不写 .bak：判据见 desktop_apply_leaves_no_stale_backup_snapshots

    // 3) _meta.json：登记本档（与 cc-switch 档共存）并把 appliedId 指向本档。
    write_desktop_meta_apply(meta)?;

    let mut msg = format!(
        "已接入 Claude 桌面端（3p 部署模式）：{}，gateway 端点={endpoint}，模型={}，原文件已备份。请重启桌面端生效。",
        threep_config.display(),
        effective.join("/")
    );
    if let Some(warning) = desktop_model_names_warning(&effective) {
        msg.push_str("\n\n");
        msg.push_str(&warning);
    }
    Ok(msg)
}

/// 写进 `inferenceModels` 的名字里若有桌面端不接受的，生成警告文案（全合规则返回 None）。
///
/// **不阻断接入**（与 cc-switch 同口径）：照原样写进档里，保留「先接入、再回头调映射」的用法。
/// 保存 Key 时已有前置拦截（见 `lib.rs` 的 `reject_desktop_key_with_unusable_model_names`），
/// 这里是兜底——档内历史清单（改端口时回退用的 `existing_inference_model_names`）与更早版本
/// 存下的 Key 都可能绕过那道拦截。
///
/// 不合规名会被桌面端在加载时**过滤掉**（不是提示）；全部不合规时选择器为空、打开会话抛
/// `ModelsNotDiscoveredError`，故那种情形单独升级措辞。判据与后果详见
/// [`crate::model::is_desktop_acceptable_model_id`]。
fn desktop_model_names_warning(models: &[String]) -> Option<String> {
    let bad: Vec<&str> = models
        .iter()
        .filter(|m| !crate::model::is_desktop_acceptable_model_id(m))
        .map(String::as_str)
        .collect();
    if bad.is_empty() {
        return None;
    }
    let all_bad = bad.len() == models.len();
    let head = if all_bad {
        format!(
            "⚠ 全部 {} 个模型名都不被桌面端接受：{}\n\
             桌面端会把它们从模型列表里过滤掉 → 模型选择器为空 → 打开会话报 \
             ModelsNotDiscoveredError。**当前这份配置写进去了但用不了。**",
            bad.len(),
            bad.join("、")
        )
    } else {
        format!(
            "⚠ 其中 {} 个模型名不被桌面端接受：{}\n\
             它们会被桌面端从模型列表里过滤掉（其余 {} 个仍可用）。",
            bad.len(),
            bad.join("、"),
            models.len() - bad.len()
        )
    };
    Some(format!(
        "{head}\n\
         要求：名字须含 claude/opus/sonnet/haiku/fable/mythos/anthropic 之一，\
         且不得含 glm/gpt/grok/deepseek/qwen/kimi/llama 等厂商名。\
         注意 claude-synaroute- 前缀对桌面端无效。\n\
         修法：在「模型映射」里把对外名改成合规形式（如 claude-opus-4-8 → {}），\
         对外用合规名、上游仍打原名。",
        bad.first().copied().unwrap_or("上游真实名")
    ))
}

/// 从 gateway 档 JSON 里取出 `inferenceModels[].name`（缺失/格式不符返回空）。
/// 用于改端口等「入参模型为空」的场景回退到档内已有清单，不擦掉之前写好的模型。
fn existing_inference_model_names(profile: &Value) -> Vec<String> {
    profile
        .get("inferenceModels")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m.get("name").and_then(|n| n.as_str()))
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// 测试可观测的「真实目录清理」调用计数：仅在 `#[cfg(test)]` 下由
/// [`cleanup_legacy_desktop_residue`]（会触碰真实用户 %APPDATA% 的那个无参版本）自增。
/// 用于断言 hermetic 的可测入口 [`apply_desktop_at`] **绝不**触发它——一旦有人把清理误挪进
/// 可测核心，计数变化即让 `apply_desktop_at_does_not_trigger_real_cleanup` 变红。
#[cfg(test)]
static REAL_CLEANUP_CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// 清理早期失效实现的残留：旧代码往 `%APPDATA%\Roaming\Claude\claude_desktop_config.json`
/// 写 `{"baseUrl":...}`（位置/字段皆错、从未生效），并留下同名 `.synaroute.bak`。
///
/// **只摘键、不删文件**：旧实现是读-改-写（`read_json_or_empty` 后 insert `baseUrl`），故该文件
/// 极可能是「桌面端自己的 preferences / 用户手配的 mcpServers + baseUrl」混合体——而这个路径正是
/// Claude 桌面端官方 `mcpServers` 配置位置。整文件删除会连带抹掉用户配置且不可恢复，故此处仅
/// 移除 `baseUrl` 这一个键，其余原样保留；仅当摘键后对象变空（`{}`，确为旧实现凭空造的纯残留）
/// 才删文件。
///
/// `.bak` 亦不盲删：它可能是接入前用户原始 config 的唯一副本。仅当其内容恰为「只含 baseUrl 的
/// 单键对象」（确为旧实现产物）才删。
///
/// 全程 best-effort：任何一步失败只告警、不影响接入（本函数在 with_rollback 集合之外调用）。
fn cleanup_legacy_desktop_residue() {
    // 测试观测点：记录「触碰真实用户目录的清理」被调用（用于 hermetic 断言，见 REAL_CLEANUP_CALLS）。
    #[cfg(test)]
    REAL_CLEANUP_CALLS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);

    // 旧实现用 config_dir()（Windows=%APPDATA%\Roaming）→ Claude\claude_desktop_config.json。
    let Some(base) = dirs::config_dir() else { return };
    let legacy = base.join("Claude").join(DESKTOP_CONFIG_FILE);

    // macOS/Linux 上 config_dir() 与 data_dir() 可能是同一目录（macOS 皆为
    // ~/Library/Application Support），此时 legacy 与本次刚写入的 normal_config 是**同一个文件**。
    // 必须跳过，否则会把刚写好的 3p 配置连同其备份一起清掉（自毁）。
    let protected: Vec<PathBuf> = match claude_desktop_dirs() {
        Ok((normal, threep)) => vec![
            normal.join(DESKTOP_CONFIG_FILE),
            threep.join(DESKTOP_CONFIG_FILE),
        ],
        Err(_) => Vec::new(),
    };
    if protected.iter().any(|p| p == &legacy) {
        return;
    }

    cleanup_legacy_desktop_residue_at(&legacy);
}

/// 可测入口：对指定的旧 config 路径执行「摘 baseUrl 键」式清理（见
/// [`cleanup_legacy_desktop_residue`] 的语义说明）。不解析真实目录，便于单测注入临时路径。
fn cleanup_legacy_desktop_residue_at(legacy: &Path) {
    let legacy_bak = backup_path_for(legacy);

    // 1) config：只摘 baseUrl 键，保留其它键（preferences / mcpServers 等用户配置）。
    if legacy.exists() {
        match std::fs::read_to_string(legacy)
            .ok()
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        {
            Some(Value::Object(mut map)) if map.contains_key("baseUrl") => {
                map.remove("baseUrl");
                if map.is_empty() {
                    // 摘完变空对象：确为旧实现凭空造的纯残留，删掉不留空壳。
                    if let Err(e) = std::fs::remove_file(legacy) {
                        tracing::warn!("清理旧桌面端残留 {} 失败: {e}", legacy.display());
                    }
                } else {
                    // 仍有用户配置：原子写回摘键后的内容。刻意**不走** backup_and_write_json——
                    // 它会把摘键前的内容拷进 .synaroute.bak，反而销毁接入前的原始副本。
                    match serde_json::to_vec_pretty(&Value::Object(map))
                        .map_err(AppError::from)
                        .and_then(|data| crate::secret::atomic_write(legacy, &data))
                    {
                        Ok(()) => {}
                        Err(e) => {
                            tracing::warn!("清理旧桌面端残留 {} 失败: {e}", legacy.display())
                        }
                    }
                }
            }
            // 不含 baseUrl（桌面端自己的合法配置）或非对象：一律不动。
            _ => {}
        }
    }

    // 2) .bak：仅当内容恰为「只含 baseUrl 的单键对象」才删。否则它可能是接入前用户原始
    //    config 的唯一副本，删掉就再无第二处可恢复。
    if legacy_bak.exists() {
        let is_pure_legacy = std::fs::read_to_string(&legacy_bak)
            .ok()
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
            .and_then(|v| match v {
                Value::Object(map) => Some(map.len() == 1 && map.contains_key("baseUrl")),
                _ => None,
            })
            .unwrap_or(false);
        if is_pure_legacy {
            if let Err(e) = std::fs::remove_file(&legacy_bak) {
                tracing::warn!("清理旧桌面端残留 {} 失败: {e}", legacy_bak.display());
            }
        }
    }
}

/// 读-改-写某 config 的 `deploymentMode`，保留其它键（preferences 等）。文件/目录不存在时创建。
fn write_deployment_mode(path: &Path, mode: &str) -> AppResult<()> {
    let mut root = read_json_or_empty(path)?;
    let obj = root
        .as_object_mut()
        .ok_or_else(|| AppError::ToolConfig(format!("{} 顶层非对象", path.display())))?;
    obj.insert("deploymentMode".into(), Value::String(mode.to_string()));
    backup_and_write_json(path, &root)
}

/// 桌面端 gateway 档里 `inferenceModels` 的一条：对外名 + 由 Key 数据推导出的能力断言。
///
/// 三个字段各有官方语义（判据：`app.asar` v1.24012.9 的 `inferenceModels` schema，
/// offset ≈ 7013300 / 消费点 ≈ 7400700）：
/// - `supports1m`：**你对自己部署做的能力断言**，只对确认支持 1M 窗口的模型设置。
///   故此处按该对外名解析到的上游模型 `contextWindow` 判定，**无数据时保守 false**——
///   一律写 true 会让桌面端给出一个上游实际不支持、必然失败的 1M 选项。
/// - `anthropic_family_tier`：桌面端遇到裸别名（`opus`/`sonnet`/…）时钉到本条；不填则裸别名无处可落。
/// - `is_family_default`：同档位多条时选谁。同档位内只给**第一条**置 true（官方对多个 true
///   会告警并取首个，我们不制造这种告警）。
#[derive(Debug, Clone, PartialEq)]
pub struct DesktopModelEntry {
    pub name: String,
    pub supports1m: bool,
    pub anthropic_family_tier: Option<&'static str>,
    pub is_family_default: bool,
}

/// 把「对外名列表 + 各分类启用 Key」组装成 gateway 档需要的模型条目。
///
/// `keys` 为该分类按优先级排序的启用 Key（与 `discoverable_models` 同源）。
///
/// **窗口只认主 Key**（`keys.first()`，即路由实际优先落点），不逐 Key 找第一个有数据的。
/// 逐 Key 找的问题：主 Key 把 `claude-opus-4-8` 映射到一个没记 `context_window` 的模型
/// （`fetch_models` 拉来的模型一律 `context_window: None`，很常见），备用 Key 恰好记了 1M，
/// 于是写出 `supports1m: true` —— 而请求实际落在主 Key 的 200k 上，桌面端给出一个必然被截断
/// 的选项。查不到就保守写 false：少一个 1M 选项只是少个可选项，多一个假的会让请求直接失败。
fn build_desktop_model_entries(
    models: &[String],
    keys: &[ProviderKey],
) -> Vec<DesktopModelEntry> {
    let mut tier_seen: std::collections::HashSet<&'static str> = std::collections::HashSet::new();
    let primary = keys.first();
    models
        .iter()
        .map(|name| {
            let ctx = primary.and_then(|k| k.context_window_for_outward(name));
            let tier = crate::model::desktop_family_tier_of(name);
            // 同档位只让第一条当默认（官方对多个 isFamilyDefault 会告警并取首个）。
            let is_default = tier.is_some_and(|t| tier_seen.insert(t));
            DesktopModelEntry {
                name: name.clone(),
                supports1m: ctx.is_some_and(|c| c >= crate::model::ONE_MILLION_CONTEXT),
                anthropic_family_tier: tier,
                is_family_default: is_default,
            }
        })
        .collect()
}

/// 构造 gateway 配置档 JSON（对齐 cc-switch `build_gateway_profile`）。
///
/// **合并写**：在 `existing`（档内既有内容）之上覆盖本函数负责的 7 个键，其余键原样保留——
/// 用户若在桌面端 Setup 面板里给本档加过字段，不会因改端口/重新接入而被静默抹掉。
/// `existing` 非对象（空档/损坏）时按空对象起算。
///
/// `inferenceGatewayApiKey` 用占位（代理剥入站鉴权头、按路由 Key 注入真实密钥）；
/// `inferenceGatewayBaseUrl` 指向本地代理源（桌面端按 Anthropic 风格发 /v1/messages，代理已识别）。
///
/// `disableDeploymentModeChooser` 恒 `true`：**对齐 cc-switch 实测样本**（用户拍板保持一致）。
/// 注意它并非 3p 生效的必需条件——判据 `pd(e) = hasInference(e) && (disableClaudeAiSignIn(e)
/// || persistedMode() !== "1p")`（`app.asar` offset ≈ 7100100）是「或」关系，而我们本就会写
/// `deploymentMode=3p`。代价是接入后桌面端里看不到官方登录入口，须从 SynaRoute 点还原才能回官方。
///
/// `entries` 恒非空（调用方已挡空列表）；每条的能力断言见 [`DesktopModelEntry`]。
fn build_gateway_profile(
    existing: Value,
    endpoint: &str,
    entries: &[DesktopModelEntry],
) -> Value {
    let mut obj = match existing {
        Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    obj.insert(
        "coworkEgressAllowedHosts".into(),
        Value::Array(vec![Value::String("*".into())]),
    );
    obj.insert("disableDeploymentModeChooser".into(), Value::Bool(true));
    obj.insert(
        "inferenceGatewayApiKey".into(),
        Value::String(DESKTOP_GATEWAY_PLACEHOLDER.into()),
    );
    obj.insert(
        "inferenceGatewayAuthScheme".into(),
        Value::String("bearer".into()),
    );
    obj.insert(
        "inferenceGatewayBaseUrl".into(),
        Value::String(endpoint.to_string()),
    );
    obj.insert("inferenceProvider".into(), Value::String("gateway".into()));
    // 恒写 inferenceModels（调用方已挡空列表）：合并写下若沿用旧值会与当前可服务集脱节。
    let arr: Vec<Value> = entries
        .iter()
        .map(|e| {
            let mut o = serde_json::Map::new();
            o.insert("name".into(), Value::String(e.name.clone()));
            // supports1m 只在确有依据时写：官方语义是能力断言，无依据即不断言。
            if e.supports1m {
                o.insert("supports1m".into(), Value::Bool(true));
            }
            if let Some(tier) = e.anthropic_family_tier {
                o.insert("anthropicFamilyTier".into(), Value::String(tier.into()));
                // isFamilyDefault 只在有 tier 时才有意义（官方对「无 tier 却设该标记」会告警并忽略）。
                if e.is_family_default {
                    o.insert("isFamilyDefault".into(), Value::Bool(true));
                }
            }
            Value::Object(o)
        })
        .collect();
    obj.insert("inferenceModels".into(), Value::Array(arr));
    Value::Object(obj)
}

/// 接入时更新 `_meta.json`：确保 entries 里有本档（去重，与 cc-switch 档共存），appliedId 指向本档。
fn write_desktop_meta_apply(path: &Path) -> AppResult<()> {
    let mut root = read_json_or_empty(path)?;
    let obj = root
        .as_object_mut()
        .ok_or_else(|| AppError::ToolConfig("_meta.json 顶层非对象".into()))?;

    let entries = obj
        .entry("entries")
        .or_insert_with(|| Value::Array(Vec::new()));
    let arr = entries
        .as_array_mut()
        .ok_or_else(|| AppError::ToolConfig("_meta.entries 非数组".into()))?;
    // 去重：移除已存在的本档 entry，再重新追加（幂等）。不动其它档（cc-switch 的等）。
    arr.retain(|e| e.get("id").and_then(|v| v.as_str()) != Some(DESKTOP_PROFILE_ID));
    arr.push(serde_json::json!({
        "id": DESKTOP_PROFILE_ID,
        "name": DESKTOP_PROFILE_NAME,
    }));

    obj.insert(
        "appliedId".into(),
        Value::String(DESKTOP_PROFILE_ID.into()),
    );
    crate::secret::atomic_write(path, &serde_json::to_vec_pretty(&root)?) // 不写 .bak：判据见 desktop_apply_leaves_no_stale_backup_snapshots
}

// ---- MCP 客户端自动注册 ----
//
// 让「启用 MCP 开关」后无需用户手动 `claude mcp add`：后端直接把 synaroute 这台 HTTP MCP
// 服务器写进目标工具的客户端配置，用户重启客户端即可用。格式严格按官方：
// - Claude CLI：~/.claude.json 的 mcpServers.synaroute = { type:"http", url }
// - Codex：~/.codex/config.toml 的 [mcp_servers.synaroute] url = "..."
// 幂等：已存在同 url 则跳过写盘（不产生无谓备份 / 事件噪音）。

/// MCP 服务器在客户端配置里的固定名称（不随端口变化，故端口变了只需改 url）。
///
/// 两个 stdio 端（Codex / 桌面端）的 args 见 [`crate::mcp::stdio::args`]
/// （`--mcp-stdio` + 分类标记）—— 那里是**唯一事实来源**，JSON 与 TOML 两个注册点都从它取。
const MCP_CLIENT_NAME: &str = "synaroute";

/// Codex stdio MCP 的每工具调用超时（秒），写进 `tool_timeout_sec`。默认 60s 不够大脑聚合
/// 跑完（多模型并行 + 决策者综合常 30s+，偶尔超 60s → `user cancelled MCP tool call`），放大到 600s。
const MCP_TOOL_TIMEOUT_SEC: i64 = 600;

/// MCP 单次工具调用超时（毫秒）的**兜底下限**，写进客户端配置的 `timeout` 字段。
///
/// SynaRoute 的多模型聚合天然慢（多模型并行 + 决策者二次调用）。Claude Code 对 HTTP MCP
/// 有两层超时：单次工具调用总时长、以及「首字节」per-request 计时器（默认 60s，请求超时
/// 不重试）。官方文档：把 server 的 `timeout` 设为 ≥60s 会**同时**抬高首字节计时器到该值。
///
/// 注意：此值仅作**下限兜底**。实际写入的客户端超时由 lib.rs 的 `mcp_client_timeout_ms`
/// 按「各分类整轮预算 total_timeout_ms 的最大值 + 余量」动态算出，并对本常量取 max——
/// 保证客户端超时始终 ≥ 服务端整轮预算 + 余量（服务端总在客户端杀连接前优雅降级返回），
/// 且不会比历史值（10 分钟）更短。
pub(crate) const MCP_TOOL_TIMEOUT_MS: u64 = 600_000;

/// Claude 全局配置 ~/.claude.json（Claude CLI 的 mcpServers 存放处）。
fn claude_json_path() -> AppResult<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| AppError::ToolConfig("无法定位用户目录".into()))?;
    Ok(home.join(".claude.json"))
}

/// Claude 桌面端的 MCP 配置文件：**两个部署目录下的 `claude_desktop_config.json` 都要写**。
///
/// 桌面端 MCP **必须与 CLI 分离**：CLI 读 `~/.claude.json`，桌面端读它自己部署目录里的
/// `claude_desktop_config.json`（`mcpServers` 段，形态同 CLI）。二者若共用一份文件同一个
/// `synaroute` 项，两分类端口不同就会互相覆盖——接入桌面端会把 CLI 的 MCP 指到桌面端端口，
/// 反之亦然。
///
/// ## 为什么是两个文件，而不是只写 3p 那个
///
/// 桌面端读哪个 `claude_desktop_config.json` 取决于**当前部署模式**：3p 模式读
/// `Claude-3p/` 下那份（本机实测：该文件 `preferences` 里塞满活跃会话状态、随手每次操作
/// 都在写），1p（官方）模式读 `Claude/` 下那份。而部署模式会在我们背后变：
///
/// - 用户还没点「接入」就先开了大脑聚合开关 —— 此刻是 1p；
/// - **「切回官方」会把 deploymentMode 复位成 1p，却刻意保留 MCP 注册**
///   （`restore_client_config_keeping_mcp`，事件文案写的是「已保留 MCP 注册（大脑聚合继续
///   可用）」）。只写 3p 那份时，这句话是**假的**：复位后桌面端读的是 1p 配置，那里没有
///   synaroute 项，聚合工具凭空消失；而 `is_mcp_registered` 读的还是 3p 那份 →
///   界面照旧显示「已接入」。典型的「配置说接了、客户端没有」。
///
/// 两个都写即与模式解耦，和 `apply_claude_desktop` 把 `deploymentMode` 同时写进两个 config
/// 的理由完全一致（同一份「说不准它读哪个」的不确定性）。非当前模式的那份是惰性的，无副作用。
fn desktop_mcp_config_paths() -> AppResult<Vec<PathBuf>> {
    let (normal, threep) = claude_desktop_dirs()?;
    // 3p 优先：它是接入后（也是绝大多数时间）真正生效的那份，日志里先出现更贴合用户预期。
    Ok(vec![threep.join(DESKTOP_CONFIG_FILE), normal.join(DESKTOP_CONFIG_FILE)])
}

/// 某 Claude 系分类的 MCP 客户端配置文件路径的**唯一决策点**：
/// - CLI → `~/.claude.json`
/// - 桌面端 → 两个部署目录下的 `claude_desktop_config.json`（与 CLI 分离，
///   且两个都写以与部署模式解耦，见 [`desktop_mcp_config_paths`]）
///
/// register / unregister / is_registered 三处都经此路由，保证「CLI vs 桌面端写哪个文件」的判定
/// 永远一致、且可被单测直接覆盖：一旦有人把桌面端臂改回 `claude_json_path()`（两端共用一份文件、
/// 端口互相覆盖的原始 bug），针对本函数的测试立即失败。Codex 不走 Claude 系 MCP，显式报错而非
/// 静默落到某文件。
fn claude_mcp_config_paths(category: CategoryType) -> AppResult<Vec<PathBuf>> {
    match category {
        CategoryType::ClaudeCli => Ok(vec![claude_json_path()?]),
        CategoryType::ClaudeDesktop => desktop_mcp_config_paths(),
        CategoryType::Codex => {
            Err(AppError::ToolConfig("Codex 不使用 Claude 系 MCP 配置路径".into()))
        }
    }
}

/// 把 SynaRoute MCP 服务器注册进某分类对应工具的客户端配置。
/// `timeout_ms`：写入客户端的单次工具调用超时（由调用方按整轮预算联动算出，见
/// lib.rs `mcp_client_timeout_ms`）。返回 (人类可读结果, 是否实际写盘)。已是最新时不写盘、返回 false。
///
/// **三端 transport 形态各异，禁止混写**：
/// - Claude CLI → JSON `mcpServers.synaroute` = `{type:"http", url, timeout}`（CLI 支持 HTTP）
/// - Claude 桌面端 → JSON `mcpServers.synaroute` = `{command, args:["--mcp-stdio"]}`
///   （桌面端**只认 stdio**；写 HTTP 形态会被判 "not valid MCP server configurations" 并跳过）
/// - Codex → TOML `mcp_servers.synaroute` = stdio（command + args + type + 超时）
pub fn register_mcp_client(category: CategoryType, mcp_url: &str, timeout_ms: u64) -> AppResult<(String, bool)> {
    match category {
        CategoryType::ClaudeCli => {
            register_mcp_claude_at(&single_claude_mcp_path(category)?, mcp_url, timeout_ms)
        }
        // 桌面端走 stdio：它不支持 HTTP transport（见 register_mcp_claude_desktop_at）。
        CategoryType::ClaudeDesktop => register_mcp_claude_desktop(),
        CategoryType::Codex => register_mcp_codex(mcp_url, timeout_ms),
    }
}

/// CLI 臂的便捷取用：它只有一个路径，取 `claude_mcp_config_paths` 的唯一项。
///
/// 刻意不写成「取第 0 项」的通用助手：桌面端有两个路径，静默只用第一个正是本轮修掉的缺陷。
fn single_claude_mcp_path(category: CategoryType) -> AppResult<PathBuf> {
    let mut paths = claude_mcp_config_paths(category)?;
    if paths.len() != 1 {
        return Err(AppError::ToolConfig(format!(
            "{} 有 {} 个 MCP 配置路径，不能按单文件处理",
            category.as_str(),
            paths.len()
        )));
    }
    Ok(paths.remove(0))
}

/// 从某分类对应工具的客户端配置移除 synaroute MCP 项（关闭开关时）。
///
/// 桌面端有两份配置（见 [`desktop_mcp_config_paths`]），**两份都要摘**：只摘生效的那份，
/// 另一份会留下一个指向已停服务的死项，用户下次切模式时它就复活了。
/// 逐份 best-effort：一份被占用不该让另一份留着死配置。
pub fn unregister_mcp_client(category: CategoryType) -> AppResult<(String, bool)> {
    match category {
        CategoryType::ClaudeCli | CategoryType::ClaudeDesktop => {
            let mut msgs = Vec::new();
            let mut wrote_any = false;
            let mut last_err = None;
            for path in claude_mcp_config_paths(category)? {
                match unregister_mcp_claude_at(&path) {
                    Ok((msg, wrote)) => {
                        wrote_any |= wrote;
                        if wrote {
                            msgs.push(msg);
                        }
                    }
                    Err(e) => last_err = Some(e),
                }
            }
            if msgs.is_empty() {
                // 一份都没摘到：若有过错误就如实上报，否则本来就没注册（幂等成功）。
                return match last_err {
                    Some(e) => Err(e),
                    None => Ok(("Claude 未注册 synaroute，无需移除".into(), false)),
                };
            }
            Ok((msgs.join("；"), wrote_any))
        }
        CategoryType::Codex => unregister_mcp_codex(),
    }
}

/// 检测某分类对应工具的客户端配置里是否已注册 synaroute MCP（供配置预览显示接入状态）。
/// 读各端真实 MCP 客户端文件（CLI=~/.claude.json、桌面端=两个部署目录下的
/// claude_desktop_config.json 的 mcpServers；Codex=config.toml 的 mcp_servers），
/// 只判存在性、不改盘。文件不存在或解析失败均视为未注册。
///
/// 桌面端用 **`all`（而非 `any`）**：注册路径两份都写，故「两份都在」才算真接上。
/// 用 `any` 会让「只剩一份」也报「已接入」—— 而缺的那份恰恰可能是当前部署模式在读的，
/// 界面说接了、桌面端里没有，正是本轮要消灭的形态。两份都没有时自然为 false。
pub fn is_mcp_registered(category: CategoryType) -> bool {
    match category {
        CategoryType::ClaudeCli | CategoryType::ClaudeDesktop => claude_mcp_config_paths(category)
            .ok()
            .map(|paths| all_json_have_mcp_server(&paths))
            .unwrap_or(false),
        CategoryType::Codex => {
            let Ok(path) = codex::config_path() else { return false };
            if !path.exists() {
                return false;
            }
            let Ok(raw) = std::fs::read_to_string(&path) else { return false };
            let Ok(v) = raw.parse::<toml::Value>() else { return false };
            v.get("mcp_servers")
                .and_then(|s| s.get(MCP_CLIENT_NAME))
                .is_some()
        }
    }
}

/// 某 JSON 配置文件的 `mcpServers` 里是否含 synaroute 项（CLI/桌面端同形态）。
/// 一组配置文件是否**全部**已注册 synaroute MCP。
///
/// 抽出来是为了让 `all`（而非 `any`）这条判据可被单测直接钉住 —— `is_mcp_registered`
/// 只吃 `CategoryType`、走真实系统目录，测不到。
///
/// 为什么必须是 `all`：桌面端注册路径两份都写（1p/3p 各一），故「两份都在」才算真接上。
/// `any` 会让「只剩一份」也报「已接入」，而缺的那份恰恰可能正是当前部署模式在读的 ——
/// 界面说接了、桌面端里没有。空列表返回 false（没有任何文件可证明已接入）。
fn all_json_have_mcp_server(paths: &[PathBuf]) -> bool {
    !paths.is_empty() && paths.iter().all(|p| json_has_mcp_server(p))
}

fn json_has_mcp_server(path: &Path) -> bool {
    if !path.exists() {
        return false;
    }
    let Ok(raw) = std::fs::read_to_string(path) else { return false };
    let Ok(v) = serde_json::from_str::<Value>(&raw) else { return false };
    v.get("mcpServers")
        .and_then(|s| s.get(MCP_CLIENT_NAME))
        .is_some()
}

fn register_mcp_claude_at(path: &Path, mcp_url: &str, timeout_ms: u64) -> AppResult<(String, bool)> {
    let mut root = read_json_or_empty(path)?;
    let obj = root
        .as_object_mut()
        .ok_or_else(|| AppError::ToolConfig("~/.claude.json 顶层非对象".into()))?;

    let servers = obj
        .entry("mcpServers")
        .or_insert_with(|| Value::Object(Default::default()));
    let servers_obj = servers
        .as_object_mut()
        .ok_or_else(|| AppError::ToolConfig("mcpServers 非对象".into()))?;

    // 幂等：已存在且 url / type / timeout 全一致 → 不写盘。
    // 必须比对 timeout：否则老配置（无 timeout 或旧值）会因 url/type 匹配被判「已是最新」
    // 而永远升不上目标值，超时修复形同虚设。
    if let Some(existing) = servers_obj.get(MCP_CLIENT_NAME) {
        if existing.get("url").and_then(|u| u.as_str()) == Some(mcp_url)
            && existing.get("type").and_then(|t| t.as_str()) == Some("http")
            && existing.get("timeout").and_then(|t| t.as_u64()) == Some(timeout_ms)
        {
            return Ok((format!("Claude MCP 已是最新（{mcp_url}），跳过"), false));
        }
    }

    servers_obj.insert(
        MCP_CLIENT_NAME.to_string(),
        json_http_mcp(mcp_url, timeout_ms),
    );
    write_json_without_locking_snapshot(path, &root)?;
    Ok((
        format!("已注册 MCP 到 Claude：{}（{mcp_url}），重启客户端生效", path.display()),
        true,
    ))
}

/// Claude 的 HTTP MCP 项：{ "type":"http", "url":"...", "timeout":<ms> }。
/// **仅 CLI 用**——桌面端不支持 HTTP transport，见 [`json_stdio_mcp`]。
/// timeout（毫秒）：Claude Code 对 HTTP MCP 有两层超时——单次工具调用总时长、以及
/// 「首字节」per-request timer（默认 60s）。把 timeout 设为 ≥60s 会同时抬高首字节 timer 到该值
/// （见 code.claude.com/docs/en/mcp）。此值由调用方按整轮预算联动算出（见
/// lib.rs `mcp_client_timeout_ms`），保证 ≥ 服务端整轮预算 + 余量。
fn json_http_mcp(mcp_url: &str, timeout_ms: u64) -> Value {
    let mut m = serde_json::Map::new();
    m.insert("type".into(), Value::String("http".into()));
    m.insert("url".into(), Value::String(mcp_url.to_string()));
    m.insert("timeout".into(), Value::Number(timeout_ms.into()));
    Value::Object(m)
}

/// Claude 桌面端的 stdio MCP 项：`{ command, args: ["--mcp-stdio", "--mcp-category=…"] }`。
///
/// 桌面端的 `claude_desktop_config.json` **只接受 stdio transport**：写 CLI 那套
/// `{type:"http", url, timeout}` 会被判「not valid MCP server configurations」整项跳过
/// （用户可见弹窗：`The following entries ... were skipped: synaroute`）。
/// 形态与 Codex stdio 注册同源，差别是这里是 JSON、且不写 `type`/超时字段（桌面端 schema
/// 只需 command+args；无 HTTP 首字节计时器，故 timeout 无处可写——聚合慢由服务端自身优雅降级保证）。
///
/// **args 必须带分类**：桌面端与 Codex 的 command 都是同一个 exe，args 里若只有
/// `--mcp-stdio` 两端就一字不差，服务端无从分辨调用方（那正是「还要问用户是哪个分类」的根因）。
fn json_stdio_mcp(exe_path: &str, category: CategoryType) -> Value {
    let mut m = serde_json::Map::new();
    m.insert("command".into(), Value::String(exe_path.to_string()));
    m.insert("args".into(), crate::mcp::stdio::args_json(category));
    Value::Object(m)
}

/// 把 SynaRoute 注册为 Claude 桌面端的 **stdio** MCP（桌面端不支持 HTTP，见 [`json_stdio_mcp`]）。
/// command 指向当前运行的 synaroute.exe，故升级换目录后重新接入会自动改写为现役路径。
///
/// **两个部署目录下的 config 都写**（见 [`desktop_mcp_config_paths`]）：桌面端读哪份取决于
/// 当前 `deploymentMode`，而「切回官方」会把它复位成 1p 却刻意保留 MCP 注册 ——
/// 只写 3p 那份时那句「已保留 MCP 注册」就是假的。
///
/// 一份失败不放弃另一份：另一份写成了，用户在对应模式下仍然可用；全失败才报错。
fn register_mcp_claude_desktop() -> AppResult<(String, bool)> {
    let exe = std::env::current_exe()
        .map_err(|e| AppError::ToolConfig(format!("无法定位 synaroute 可执行文件: {e}")))?;
    let exe = exe.display().to_string();
    let mut msgs = Vec::new();
    let mut wrote_any = false;
    let mut last_err = None;
    let mut ok_count = 0usize;
    for path in claude_mcp_config_paths(CategoryType::ClaudeDesktop)? {
        match register_mcp_claude_desktop_at(&path, &exe) {
            Ok((msg, wrote)) => {
                ok_count += 1;
                wrote_any |= wrote;
                if wrote {
                    msgs.push(msg);
                }
            }
            Err(e) => last_err = Some(e),
        }
    }
    if ok_count == 0 {
        return Err(last_err.unwrap_or_else(|| {
            AppError::ToolConfig("找不到 Claude 桌面端配置目录".into())
        }));
    }
    if msgs.is_empty() {
        return Ok(("Claude 桌面端 MCP（stdio）已是最新，跳过".to_string(), false));
    }
    Ok((msgs.join("；"), wrote_any))
}

/// 可测入口：把 stdio 形态的 synaroute MCP 写进指定的桌面端 config。
///
/// 幂等：已是同 command + args 时不写盘。此外**主动清除**旧 HTTP 形态残留（老版本写的
/// `type:"http"`/`url`/`timeout` 键）——桌面端见到这些非法键就整项跳过，必须整项替换而非合并。
fn register_mcp_claude_desktop_at(path: &Path, exe_path: &str) -> AppResult<(String, bool)> {
    let mut root = read_json_or_empty(path)?;
    let obj = root.as_object_mut().ok_or_else(|| {
        AppError::ToolConfig(format!("{} 顶层非对象", path.display()))
    })?;

    let servers = obj
        .entry("mcpServers")
        .or_insert_with(|| Value::Object(Default::default()));
    let servers_obj = servers
        .as_object_mut()
        .ok_or_else(|| AppError::ToolConfig("mcpServers 非对象".into()))?;

    let desired = json_stdio_mcp(exe_path, CategoryType::ClaudeDesktop);
    // 幂等：整项完全相等才跳过。用整项比对（而非逐字段）顺带保证旧 HTTP 残留键必被判为不同 →
    // 触发重写，把非法项换成合法 stdio 项。
    if servers_obj.get(MCP_CLIENT_NAME) == Some(&desired) {
        return Ok((
            "Claude 桌面端 MCP（stdio）已是最新，跳过".to_string(),
            false,
        ));
    }

    servers_obj.insert(MCP_CLIENT_NAME.to_string(), desired);
    write_json_without_locking_snapshot(path, &root)?;
    Ok((
        format!(
            "已注册 MCP 到 Claude 桌面端（stdio）：{}，重启桌面端生效",
            path.display()
        ),
        true,
    ))
}

fn unregister_mcp_claude_at(path: &Path) -> AppResult<(String, bool)> {
    if !path.exists() {
        return Ok(("Claude 配置不存在，无需移除".into(), false));
    }
    let mut root = read_json_or_empty(path)?;
    let Some(obj) = root.as_object_mut() else {
        return Ok(("~/.claude.json 顶层非对象，跳过".into(), false));
    };
    let removed = obj
        .get_mut("mcpServers")
        .and_then(|s| s.as_object_mut())
        .map(|servers| servers.remove(MCP_CLIENT_NAME).is_some())
        .unwrap_or(false);
    if !removed {
        return Ok(("Claude 未注册 synaroute，无需移除".into(), false));
    }
    write_json_without_locking_snapshot(path, &root)?;
    Ok((format!("已从 Claude 移除 MCP：{}", path.display()), true))
}

/// Codex 走 **stdio** MCP（而非 HTTP）：Codex 对 HTTP/streamable MCP 仅实验性支持
/// （需 experimental_use_rmcp_client、握手挑剔、易「空壳」），而 stdio 是其一等公民
/// （codegraph/sqlcl 等均为 stdio），稳定、无端口漂移、无首字节超时。故写成:
///   [mcp_servers.synaroute]
///   command = "<synaroute.exe 绝对路径>"
///   args = ["--mcp-stdio"]
/// 由 Codex 以子进程拉起 synaroute.exe --mcp-stdio，用 stdin/stdout 传 JSON-RPC。
/// args 里还带 `--mcp-category=codex`（见 [`crate::mcp::stdio::args`]）——
/// 没有它，Codex 与桌面端的注册一字不差，服务端分辨不出这次调用属于哪个分类。
/// `_timeout_ms` 于 stdio 不需要（无 HTTP 首字节超时），保留形参与 Claude 端签名一致。
fn register_mcp_codex(_mcp_url: &str, _timeout_ms: u64) -> AppResult<(String, bool)> {
    let exe = std::env::current_exe()
        .map_err(|e| AppError::ToolConfig(format!("无法定位 synaroute 可执行文件: {e}")))?;
    register_mcp_codex_at(&codex::config_path()?, &exe.display().to_string())
}

fn register_mcp_codex_at(path: &Path, exe_path: &str) -> AppResult<(String, bool)> {
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
    let table = doc
        .as_table_mut()
        .ok_or_else(|| AppError::ToolConfig("config.toml 顶层非表".into()))?;

    // 幂等：mcp_servers.synaroute 已是 stdio 形态（command 指向当前 exe、args 完全一致）
    // → 跳过写盘。exe 路径变化（升级换目录）时会重写，保证 command 始终指向现役 exe。
    //
    // 🔴 args 必须**整体**比对（而非只看第一项/长度）：旧版注册的 args 只有 `--mcp-stdio`、
    // 没有分类标记，若比对放宽到「首项是 --mcp-stdio 就算最新」，旧配置会被永久判为
    // 「已是最新」而跳过重写 —— 自愈路径失效，用户的 Codex 永远走不到分类身份那条路，
    // 且失效是静默的。
    let already = table
        .get("mcp_servers")
        .and_then(|v| v.as_table())
        .and_then(|s| s.get(MCP_CLIENT_NAME))
        .and_then(|v| v.as_table())
        .map(|e| {
            e.get("command").and_then(|c| c.as_str()) == Some(exe_path)
                && e.get("args") == Some(&crate::mcp::stdio::args_toml(CategoryType::Codex))
                // type="stdio" 必须纳入幂等：Codex 桌面端靠此字段识别 stdio MCP，缺了就不加载
                // 该 MCP（CLI 宽松、不需要也能识别，但桌面端严格）。老配置缺 type 时须重写补上，
                // 不能因 command/args 一致就判「已最新」跳过。
                && e.get("type").and_then(|t| t.as_str()) == Some("stdio")
                // tool_timeout_sec 同样纳入幂等：默认 60s 不够聚合跑（多模型并行+决策者常需 30s+，
                // 偶尔超 60s → `user cancelled MCP tool call`）。老配置缺此字段时须重写补上。
                && e.get("tool_timeout_sec").and_then(|t| t.as_integer()) == Some(MCP_TOOL_TIMEOUT_SEC)
        })
        .unwrap_or(false);
    if already {
        return Ok(("Codex MCP（stdio）已是最新，跳过".into(), false));
    }

    let servers = table
        .entry("mcp_servers".to_string())
        .or_insert_with(|| toml::Value::Table(Default::default()));
    let servers_table = servers
        .as_table_mut()
        .ok_or_else(|| AppError::ToolConfig("mcp_servers 非表".into()))?;

    // stdio transport：command + args + type。无 url、无 timeout、无 experimental 开关。
    // type="stdio" 不可省：Codex 桌面端靠它识别 stdio MCP（CLI 宽松、缺了也识别，但桌面端
    // 缺了就不加载该 MCP，表现为「对话里根本没有该工具」——与能用的 codegraph/sqlcl 的唯一差异）。
    let mut entry = toml::value::Table::new();
    entry.insert("command".to_string(), toml::Value::String(exe_path.to_string()));
    entry.insert(
        "args".to_string(),
        crate::mcp::stdio::args_toml(CategoryType::Codex),
    );
    entry.insert("type".to_string(), toml::Value::String("stdio".to_string()));
    // tool_timeout_sec：大脑聚合要跑多模型并行 + 决策者综合，常 30s+，偶尔更久。Codex 默认
    // 每工具调用超时仅 60s，不够 → 表现为「synaroute_ai started 后 user cancelled」。放大到 600s。
    // startup_timeout_sec：子进程启动握手超时，默认 10s，给足 30s 稳妥。
    entry.insert("tool_timeout_sec".to_string(), toml::Value::Integer(MCP_TOOL_TIMEOUT_SEC));
    entry.insert("startup_timeout_sec".to_string(), toml::Value::Integer(30));
    servers_table.insert(MCP_CLIENT_NAME.to_string(), toml::Value::Table(entry));

    let serialized =
        toml::to_string_pretty(&doc).map_err(|e| AppError::ToolConfig(e.to_string()))?;
    write_without_locking_snapshot(path, serialized.as_bytes())?;
    Ok((
        format!("已接入大脑聚合到 Codex（stdio）：{}，重启 Codex 生效", path.display()),
        true,
    ))
}

fn unregister_mcp_codex() -> AppResult<(String, bool)> {
    unregister_mcp_codex_at(&codex::config_path()?)
}

fn unregister_mcp_codex_at(path: &Path) -> AppResult<(String, bool)> {
    if !path.exists() {
        return Ok(("Codex 配置不存在，无需移除".into(), false));
    }
    let content = std::fs::read_to_string(path)?;
    if content.trim().is_empty() {
        return Ok(("Codex 配置为空，无需移除".into(), false));
    }
    let mut doc: toml::Value = content
        .parse::<toml::Value>()
        .map_err(|e| AppError::ToolConfig(format!("解析 config.toml 失败: {e}")))?;
    let removed = doc
        .as_table_mut()
        .and_then(|t| t.get_mut("mcp_servers"))
        .and_then(|s| s.as_table_mut())
        .map(|servers| servers.remove(MCP_CLIENT_NAME).is_some())
        .unwrap_or(false);
    if !removed {
        return Ok(("Codex 未注册 synaroute，无需移除".into(), false));
    }
    let serialized =
        toml::to_string_pretty(&doc).map_err(|e| AppError::ToolConfig(e.to_string()))?;
    write_without_locking_snapshot(path, serialized.as_bytes())?;
    Ok((format!("已从 Codex 移除 MCP：{}", path.display()), true))
}


/// 还原某工具配置（从 .synaroute.bak 恢复）。
///
/// Codex 是双文件语义，但**两个文件的角色不对称**（本版起）：
/// - `config.toml` 是我们写的 → 从 `.bak` 还原；
/// - `auth.json` 我们**不再写**，但旧版本（≤0.1.33）写过占位符 → 这里负责把它解除。
///
/// **顺序刻意是「先解除假凭据、再还原 config」**，因为两种半途残留的代价不对称：
/// - 「config 还原了、假 key 还在」= 静默把一个假凭据发给真实 OpenAI，换回一句
///   指向 platform.openai.com 的 401（用户被引向完全错误的方向）；
/// - 「假 key 解除了、config 还指着我们」= 客户端连不上，**响亮失败**，用户立刻知道要重试。
///
/// 先做危险的那一步，它失败时另一步还没发生。
///
/// 「无备份」不视为错误：还原由「停止代理」自动触发，从未接入过的分类本就没有 .bak，
/// 此时已处于接入前状态，返回成功（无需还原），避免每次停止都弹误报错。
pub fn restore(category: CategoryType) -> AppResult<String> {
    // 桌面端不是「从 .bak 还原单文件」那套：接入切了 deploymentMode=3p 并写了 gateway 档，
    // 还原须把两个 config 复位 1p、删本档 profile、从 _meta 摘掉本档并改 appliedId（镜像
    // cc-switch 的 restore）。故单独分派。
    if category == CategoryType::ClaudeDesktop {
        return restore_claude_desktop();
    }
    let path = match category {
        CategoryType::ClaudeCli => claude_cli_settings_path()?,
        CategoryType::Codex => codex::config_path()?,
        CategoryType::ClaudeDesktop => unreachable!("桌面端已在上方分派"),
    };
    let mut restored = Vec::new();

    // 先解除 Codex 的假凭据（见上面的顺序判据）。
    // 失败**不早退**：config 那一步仍要尝试，错误留到最后一起上报 —— 早退会让用户
    // 停在「假 key 摘不掉、config 也没还原」这个两头皆输的状态。
    let mut deferred: Option<AppError> = None;
    if category == CategoryType::Codex {
        match codex::codex_catalog::restore_side_files() {
            Ok(Some(note)) => restored.push(note),
            Ok(None) => {}
            Err(e) => deferred = Some(e),
        }
    }

    match restore_one(&path) {
        Ok(true) => restored.push(path.display().to_string()),
        Ok(false) => {}
        Err(e) => {
            if let Some(prev) = deferred {
                return Err(AppError::ToolConfig(format!(
                    "还原 Codex 配置失败：副文件处理 {prev}；还原 {} 失败 {e}",
                    path.display()
                )));
            }
            return Err(e);
        }
    }

    if let Some(e) = deferred {
        return Err(AppError::ToolConfig(format!(
            "已还原 {}，但副文件处理失败：{e}（错误自带路径 —— 可能是 auth.json 的占位凭据\
             没解除，也可能是模型目录没删掉；auth.json 里那个占位符是 `{PROXY_PLACEHOLDER}`）。",
            path.display()
        )));
    }

    if restored.is_empty() {
        Ok("无备份，无需还原（未接入或已还原）".into())
    } else {
        Ok(format!("已从备份还原：{}", restored.join("、")))
    }
}

/// 断开 Claude 桌面端接入：删除本档 gateway 文件、从 `_meta.json` 摘掉本档并把 appliedId 交还
/// 给接入前那一档（记不得则交给剩余首个），**仅在确实无档可交时**才把 deploymentMode 复位 `1p`。
/// 镜像 cc-switch 的 restore。
///
/// 只动 SynaRoute 自己的档（DESKTOP_PROFILE_ID）——cc-switch 的档（若共存）原样保留，
/// 避免误删用户另一套接入。
///
/// 「未接入」（无本档 profile、appliedId 也不是本档）不视为错误：还原由「停止代理」自动触发，
/// 返回成功、不弹误报。
///
/// **复位 1p 的判据必须同时满足两条**（缺一即把用户踢回 get-started 登录页）：
/// 1. appliedId 当前确为本档（否则用户正用别人的档，不能动其部署模式）；
/// 2. 摘掉本档后 entries 里**再无其它档**——若还剩 cc-switch 的档，appliedId 会被交还给它，
///    那是个 3p 网关档，此时复位 1p 与该交还自相矛盾，结果是把正在用 cc-switch 的用户
///    在「接入 SynaRoute → 停止代理」一个来回后踢回官方登录页。
fn restore_claude_desktop() -> AppResult<String> {
    let (normal, threep) = claude_desktop_dirs()?;
    let normal_config = normal.join(DESKTOP_CONFIG_FILE);
    let threep_config = threep.join(DESKTOP_CONFIG_FILE);
    let profile = desktop_profile_path(&threep);
    let meta = desktop_meta_path(&threep);

    // 接入时记下的「接入前 appliedId」，用于精确交还。
    let prefer = read_desktop_prev_applied_id().filter(|id| id != DESKTOP_PROFILE_ID);
    let msg = restore_desktop_at(
        &normal_config,
        &threep_config,
        &profile,
        &meta,
        prefer.as_deref(),
    )?;

    // 交还完成，记录已无用，清掉（best-effort，失败不影响还原结果）。
    clear_desktop_prev_applied_id();
    Ok(msg)
}

/// 可测入口：对指定的四个文件执行桌面端还原（不解析真实目录、不读写 SynaRoute 侧记录，
/// 便于单测注入临时路径并断言真实逻辑，而非在测试里重抄一遍步骤）。
///
/// **整体原子**：还原要动 4 个文件（两个 config 的 deploymentMode、本档 profile、`_meta`），
/// 任一步失败就把 4 个文件全部恢复到还原前的状态。否则会留下「半还原」态——最典型的是
/// profile 已删、`_meta` 却仍把 `appliedId` 指着它，桌面端启动时拿不到那份 gateway 档而卡死。
/// 与接入侧（`apply_claude_desktop`）用同一套 `with_rollback` 保证。
///
/// 回滚集里还必须包含**两个 `.synaroute-created` 标记**（共 6 个文件）。
/// 少了它们会漏掉一种**不可自愈**的半还原态：`restore_created_or_write_mode` 处理
/// normal_config 时删掉了它和它的标记，随后处理 threep_config 时失败（比如文件被正在运行的
/// 桌面端独占）→ 回滚把两个 config 都还原了，但 normal 的标记**已经没了**。
/// 下次再还原时那条分支看不到标记，就会往一个「本是 SynaRoute 凭空新建」的文件里写
/// `{"deploymentMode":"1p"}` —— 留下一个冒充「用户官方配置」的文件，而标记无从重建。
/// 标记本身就是判据的载体，它必须和被它描述的文件同生共死。
///
/// `prefer`：appliedId 指向本档时优先交还给它（须仍在 entries 里），否则退化为剩余首个。
fn restore_desktop_at(
    normal_config: &Path,
    threep_config: &Path,
    profile: &Path,
    meta: &Path,
    prefer: Option<&str>,
) -> AppResult<String> {
    with_rollback(
        &[
            normal_config.to_path_buf(),
            threep_config.to_path_buf(),
            profile.to_path_buf(),
            meta.to_path_buf(),
            created_marker_path_for(normal_config),
            created_marker_path_for(threep_config),
        ],
        || restore_desktop_steps(normal_config, threep_config, profile, meta, prefer),
    )
}

/// 桌面端还原的实际步骤（由 [`restore_desktop_at`] 套上整体回滚后调用）。
fn restore_desktop_steps(
    normal_config: &Path,
    threep_config: &Path,
    profile: &Path,
    meta: &Path,
    prefer: Option<&str>,
) -> AppResult<String> {
    let mut done = Vec::new();

    let applied_is_ours = read_desktop_applied_id(meta).as_deref() == Some(DESKTOP_PROFILE_ID);
    // 摘掉本档后是否还剩别的档（如 cc-switch 的）。必须在清 _meta **之前**算。
    let others_remain = desktop_other_entry_ids(meta).next().is_some();

    // 删本档 gateway 文件（存在才删）。
    if profile.exists() {
        std::fs::remove_file(profile)?;
        done.push(format!("删除 {}", profile.display()));
    }

    // 从 _meta 摘掉本档；appliedId 若指向本档，优先交还给接入前记录的那一档。
    if write_desktop_meta_clear_preferring(meta, prefer)? {
        done.push("_meta 清除本档".to_string());
    }

    // 仅在「本档确为当前生效档」且「已无其它档接手」时才复位 1p。
    //
    // 两个 config 各自可能是**接入时凭空新建的**（用户从未装过/从未打开过桌面端时该文件不存在）。
    // 那种情况下「还原」的正解是**删掉整个文件**，而不是往里写 `deploymentMode: "1p"` ——
    // 后者会留下一个 SynaRoute 自己造出来的文件冒充「用户的官方配置」。
    // `restore_created_or_write_mode` 按 `.synaroute-created` 标记区分这两种情形。
    if applied_is_ours && !others_remain {
        let n = restore_created_or_write_mode(normal_config, "1p")?;
        let t = restore_created_or_write_mode(threep_config, "1p")?;
        done.push(match (n, t) {
            // 两个都是我们建的 → 都已删除
            (true, true) => "删除接入时新建的两个 claude_desktop_config.json".to_string(),
            // 混合/都存在 → 如实描述
            _ => "deploymentMode→1p".to_string(),
        });
    }

    if done.is_empty() {
        Ok("桌面端未接入 SynaRoute，无需还原".into())
    } else {
        Ok(format!("已断开 Claude 桌面端接入：{}。请重启桌面端生效。", done.join("、")))
    }
}

/// 桌面端 config 的还原：**接入时凭空新建的就删掉，原本存在的才写回 `mode`**。
///
/// 返回 `true` 表示「该文件是我们新建的，已整份删除」。
///
/// 为什么不能一律 `write_deployment_mode(path, "1p")`：那会在用户**从未装过桌面端**
/// （或从未打开过、配置文件尚不存在）的机器上，留下一个 SynaRoute 自己造出来的
/// `claude_desktop_config.json`，内容是 `{"deploymentMode":"1p"}`。它冒充「用户的官方配置」，
/// 而用户从来没有过这个文件——与 CLI/Codex 那条「凭空新建须整份删除」是同一条语义
/// （见 [`restore_one`] 的 created 标记分支）。
fn restore_created_or_write_mode(path: &Path, mode: &str) -> AppResult<bool> {
    let marker = created_marker_path_for(path);
    if marker.exists() {
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        if let Err(e) = std::fs::remove_file(&marker) {
            tracing::warn!("还原后清理新建标记 {} 失败: {e}", marker.display());
        }
        return Ok(true);
    }
    write_deployment_mode(path, mode)?;
    Ok(false)
}

/// 读 `_meta.json` 的 `appliedId`（不存在/解析失败均返回 None）。
fn read_desktop_applied_id(meta: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(meta).ok()?;
    let v: Value = serde_json::from_str(&raw).ok()?;
    v.get("appliedId")
        .and_then(|a| a.as_str())
        .map(|s| s.to_string())
}

/// 列出 `_meta.entries` 里**除本档以外**的档 id（用于判断「还有别的档接手吗」）。
/// 文件不存在/解析失败/无 entries 均返回空迭代器。
fn desktop_other_entry_ids(meta: &Path) -> impl Iterator<Item = String> {
    let ids: Vec<String> = std::fs::read_to_string(meta)
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|v| {
            v.get("entries").and_then(|e| e.as_array()).map(|arr| {
                arr.iter()
                    .filter_map(|e| e.get("id").and_then(|i| i.as_str()))
                    .filter(|id| *id != DESKTOP_PROFILE_ID)
                    .map(|s| s.to_string())
                    .collect()
            })
        })
        .unwrap_or_default();
    ids.into_iter()
}

/// SynaRoute 自己的「接入前 appliedId」记录文件（落 SynaRoute 数据目录，**不放**桌面端
/// `configLibrary/`——那里多一个 json 可能被桌面端当成配置档误扫）。
fn desktop_prev_applied_path() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("SynaRoute").join("desktop_prev_applied.json"))
}

/// 接入时记录「被本档顶替掉的那个 appliedId」，供还原时精确交还（而非乱指 entries 首个）。
/// best-effort：失败只告警，不影响接入。`prev` 为空或就是本档时不记录。
fn record_desktop_prev_applied_id(prev: Option<&str>) {
    let Some(prev) = prev.filter(|p| *p != DESKTOP_PROFILE_ID) else { return };
    let Some(path) = desktop_prev_applied_path() else { return };
    let payload = serde_json::json!({ "previousAppliedId": prev });
    let write = serde_json::to_vec_pretty(&payload)
        .map_err(AppError::from)
        .and_then(|data| {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            crate::secret::atomic_write(&path, &data)
        });
    if let Err(e) = write {
        tracing::warn!("记录桌面端接入前 appliedId 失败: {e}");
    }
}

/// 读回「接入前 appliedId」记录（无记录/解析失败均 None）。
fn read_desktop_prev_applied_id() -> Option<String> {
    let path = desktop_prev_applied_path()?;
    let raw = std::fs::read_to_string(path).ok()?;
    let v: Value = serde_json::from_str(&raw).ok()?;
    v.get("previousAppliedId")
        .and_then(|s| s.as_str())
        .map(|s| s.to_string())
}

/// 还原完成后清掉记录（best-effort）。
fn clear_desktop_prev_applied_id() {
    if let Some(path) = desktop_prev_applied_path() {
        if path.exists() {
            if let Err(e) = std::fs::remove_file(&path) {
                tracing::warn!("清除桌面端接入前 appliedId 记录失败: {e}");
            }
        }
    }
}

/// 还原时更新 `_meta.json`：移除本档 entry；若 appliedId 指向本档，则交还给 `prefer`
/// （接入前那一档，须仍在 entries 里），否则退化为剩余 entries 首个；无剩余则删除 appliedId 键。
/// 返回是否实际改动。文件不存在视为无需改动（返回 false）。
fn write_desktop_meta_clear_preferring(meta: &Path, prefer: Option<&str>) -> AppResult<bool> {
    if !meta.exists() {
        return Ok(false);
    }
    let mut root = read_json_or_empty(meta)?;
    let Some(obj) = root.as_object_mut() else {
        return Ok(false);
    };

    // 移除本档 entry。
    let mut changed = false;
    if let Some(arr) = obj.get_mut("entries").and_then(|e| e.as_array_mut()) {
        let before = arr.len();
        arr.retain(|e| e.get("id").and_then(|v| v.as_str()) != Some(DESKTOP_PROFILE_ID));
        changed |= arr.len() != before;
    }

    // appliedId 若指向本档：交还给 prefer（若仍在 entries 中），否则剩余首个，都没有则删键。
    if obj.get("appliedId").and_then(|a| a.as_str()) == Some(DESKTOP_PROFILE_ID) {
        let remaining: Vec<String> = obj
            .get("entries")
            .and_then(|e| e.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|e| e.get("id").and_then(|i| i.as_str()))
                    .map(|s| s.to_string())
                    .collect()
            })
            .unwrap_or_default();
        let next = prefer
            .filter(|p| remaining.iter().any(|r| r == *p))
            .map(|s| s.to_string())
            .or_else(|| remaining.first().cloned());
        match next {
            Some(id) => {
                obj.insert("appliedId".into(), Value::String(id));
            }
            None => {
                obj.remove("appliedId");
            }
        }
        changed = true;
    }

    if changed {
        crate::secret::atomic_write(meta, &serde_json::to_vec_pretty(&root)?)?; // 不写 .bak：判据见 desktop_apply_leaves_no_stale_backup_snapshots
    }
    Ok(changed)
}

/// [`write_desktop_meta_clear_preferring`] 的便捷形式（无「接入前那一档」偏好）。
/// 生产路径一律走 `_preferring` 版本（还原时要精确交还 appliedId），此形式仅供单测直接验证
/// 「无偏好」分支。
#[cfg(test)]
fn write_desktop_meta_clear(meta: &Path) -> AppResult<bool> {
    write_desktop_meta_clear_preferring(meta, None)
}

// ---- 只读预览（阶段 2：不编辑，只展示路径与脱敏正文）----

/// 某分类对应「目标工具」配置文件的只读快照。
/// 三端路径/格式不同：Claude CLI=settings.json；Codex=config.toml+auth.json；桌面=claude_desktop_config.json。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolConfigPreview {
    pub category_id: CategoryType,
    /// 人类可读说明：本分类写哪些文件、不写哪些
    pub summary: String,
    pub files: Vec<ToolConfigFilePreview>,
    /// 本分类是否已接入 MCP 大脑聚合（目标配置文件里已含 synaroute MCP 段）。
    /// 供前端「接入/移除大脑聚合」按钮判定当前态，与全局 mcp_enabled 开关无关。
    pub mcp_registered: bool,
    /// **仅桌面端**：接入被其他工具（如 cc-switch）接管时的说明；未被接管则为 None。
    /// 见 [`detect_desktop_takeover`]。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub takeover_warning: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolConfigFilePreview {
    pub path: String,
    pub exists: bool,
    /// json | toml | text
    pub format: String,
    /// 脱敏后的文件正文；不存在时为 None
    pub content: Option<String>,
}

/// 读取当前分类工具配置的只读预览（token 脱敏，不修改磁盘）。
///
/// `endpoint` 是该分类**当前应当生效**的代理地址（运行中取实际绑定端口，否则取配置端口）。
/// Codex 的漂移检测要拿它比对 provider 表里的 base_url —— 只查「非空」是不够的：
/// 指向第三方、或指着一个已死的旧端口，都会被旧判据当成「接入完好」而永不告警。
pub fn preview(category: CategoryType, endpoint: &str) -> AppResult<ToolConfigPreview> {
    match category {
        CategoryType::ClaudeCli => preview_claude_cli(),
        CategoryType::Codex => codex::preview(endpoint),
        CategoryType::ClaudeDesktop => preview_claude_desktop(),
    }
}

fn preview_claude_cli() -> AppResult<ToolConfigPreview> {
    let path = claude_cli_settings_path()?;
    let (exists, content) = read_preview_text(&path, true)?;
    Ok(ToolConfigPreview {
        category_id: CategoryType::ClaudeCli,
        summary: "Claude CLI：~/.claude/settings.json。写入 BASE_URL / AUTH_TOKEN(占位) / 发现开关 / ANTHROPIC_MODEL / 顶层 model；不写三档 DEFAULT_*，不写 Codex/桌面端文件。".into(),
        mcp_registered: is_mcp_registered(CategoryType::ClaudeCli),
        takeover_warning: None,
        files: vec![ToolConfigFilePreview {
            path: path.display().to_string(),
            exists,
            format: "json".into(),
            content,
        }],
    })
}

fn preview_claude_desktop() -> AppResult<ToolConfigPreview> {
    // 桌面端接入落在 %LOCALAPPDATA% 的两个部署目录（非 CLI 的 %APPDATA%）：Claude 与 Claude-3p。
    // 预览展示真正生效的四个文件：两个 config（deploymentMode）+ 3p 目录的 gateway 档 + _meta。
    let (normal, threep) = claude_desktop_dirs()?;
    let normal_config = normal.join(DESKTOP_CONFIG_FILE);
    let threep_config = threep.join(DESKTOP_CONFIG_FILE);
    let profile = desktop_profile_path(&threep);
    let meta = desktop_meta_path(&threep);

    let mut files = Vec::new();
    for p in [&normal_config, &threep_config, &profile, &meta] {
        let (exists, content) = read_preview_text(p, true)?;
        files.push(ToolConfigFilePreview {
            path: p.display().to_string(),
            exists,
            format: "json".into(),
            content,
        });
    }
    Ok(ToolConfigPreview {
        category_id: CategoryType::ClaudeDesktop,
        summary: "Claude 桌面端（3p 部署模式）：两个 claude_desktop_config.json 写 deploymentMode=3p，Claude-3p/configLibrary 里写 gateway 档（inferenceGatewayBaseUrl 指向本机代理 + 占位 key + bearer + 可选模型）并登记进 _meta。凭据预填齐即跳过 get-started。与 cc-switch 用独立档共存。不写 CLI 的 settings.json。".into(),
        mcp_registered: is_mcp_registered(CategoryType::ClaudeDesktop),
        takeover_warning: detect_desktop_takeover_at(&profile, &meta),
        files,
    })
}

/// 检测「桌面端接入已被其他工具接管」。返回 None = 没被接管（含「我们本就没接入」）。
///
/// 为什么需要：cc-switch 一被点开就会**整份重写** `configLibrary/_meta.json`，把 SynaRoute
/// 登记的 entry 连 `appliedId` 一起换成它自己的。此时 SynaRoute 的 gateway 档文件还在磁盘上，
/// UI 也仍显示「已接入」，但桌面端实际加载的是 cc-switch 那一档 —— 用户只看到「接入了但不生效」，
/// 且没有任何线索指向真正的原因。
///
/// 判据：本档 profile 文件存在（说明我们确实接入过）**且** `_meta.appliedId` 不是本档。
/// 两个条件都必要：
/// - 只看 appliedId 会把「从未接入」误报成被接管；
/// - 只看 profile 存在则漏掉「appliedId 仍是我们、只是档没了」这种情况（那属另一类残缺，
///   接入流程会重写补齐，不是接管）。
fn detect_desktop_takeover_at(profile: &Path, meta: &Path) -> Option<String> {
    if !profile.exists() {
        return None; // 没接入过，无从谈接管
    }
    let applied = read_desktop_applied_id(meta);
    match applied.as_deref() {
        Some(DESKTOP_PROFILE_ID) => None, // 仍指向本档，正常
        Some(other) => Some(format!(
            "SynaRoute 的 gateway 档仍在磁盘上，但桌面端当前生效的是另一档（appliedId={other}）\
             ——通常是 cc-switch 等工具被打开后整份重写了 _meta.json。\
             此时桌面端走的是那一档、不经 SynaRoute 代理。\
             重新点「写入工具配置」即可把 appliedId 改回本档。"
        )),
        None => Some(
            "SynaRoute 的 gateway 档仍在磁盘上，但 _meta.json 里已没有 appliedId\
             ——通常是其他工具重写了该文件。桌面端此时不会加载任何 3p 档。\
             重新点「写入工具配置」即可恢复。"
                .into(),
        ),
    }
}

fn read_preview_text(path: &Path, redact_secrets: bool) -> AppResult<(bool, Option<String>)> {
    if !path.exists() {
        return Ok((false, None));
    }
    // 读取失败不上抛：预览是只读展示，某个文件因编码/ACL/被独占锁而读不出时，应降级为
    // 「存在但无法读取」的占位，让其余文件照常显示——而非让整份预览返回 Err、前端丢掉全部
    // 路径、聚合页永久卡「加载中」。
    let raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            return Ok((true, Some(format!("/* 无法读取该文件: {e} */"))));
        }
    };
    let text = if redact_secrets {
        redact_config_secrets(&raw)
    } else {
        raw
    };
    // 预览截断：按 char 边界截，避免切在 UTF-8 多字节中间 panic
    const CAP: usize = 32_000;
    let text = if text.len() > CAP {
        let mut end = CAP;
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        format!(
            "{}…\n/* truncated {} bytes */",
            &text[..end],
            text.len() - end
        )
    } else {
        text
    };
    Ok((true, Some(text)))
}

/// 脱敏：避免预览面板泄露 token（不用 regex 依赖，按键名扫描 JSON/TOML/简单文本）。
///
/// 三层策略：
/// 1. **精确键名**：已知的固定字段（ANTHROPIC_AUTH_TOKEN 等）。
/// 2. **后缀模式**：任意含 `_TOKEN` / `_SECRET` / `_KEY` / `APIKEY` 等的键名（大小写不敏感），
///    覆盖用户手配的第三方厂商字段（如 `MOONSHOT_API_KEY`、`myVendorToken`）——此前只认固定
///    白名单，变体命名一律明文回显。
/// 3. **裸 token 兜底**：`sk-` 前缀长串。
///
/// JSON（`"key": "value"`）与 TOML（`key = "value"`）两种形态都处理。
pub(crate) fn redact_config_secrets(s: &str) -> String {
    let mut out = s.to_string();
    for key in [
        "ANTHROPIC_AUTH_TOKEN",
        "ANTHROPIC_API_KEY",
        "OPENAI_API_KEY",
        "api_key",
        "apiKey",
        // Codex 官方 ChatGPT OAuth 登录的令牌（auth.json 的 tokens.* 段）：均为 JWT，
        // 不带 sk- 前缀，故按键名单独脱敏，避免只读预览面板把访问/刷新令牌明文回传前端。
        "access_token",
        "refresh_token",
        "id_token",
        // Claude 桌面端 3p gateway 档的 API key（预览会读 configLibrary/{ID}.json）。
        "inferenceGatewayApiKey",
    ] {
        out = redact_json_string_field(&out, key);
        out = redact_toml_field(&out, key);
    }
    // 按后缀模式脱敏所有「看起来是密钥」的键名（用户手配的第三方字段用什么名都覆盖）。
    for key in secretish_keys(&out) {
        out = redact_json_string_field(&out, &key);
        out = redact_toml_field(&out, &key);
    }
    // bare sk- tokens（含 "sk-" 在内至少 12 字符）。按字符边界扫描：不能用字节索引切片，
    // 否则配置里出现多字节 UTF-8（如中文路径）时 &s[i..i+3] 会切在字符中间 panic，
    // 且 `byte as char` 会把多字节序列拆成乱码。
    let mut result = String::with_capacity(out.len());
    let mut rest = out.as_str();
    while let Some(pos) = rest.find("sk-") {
        result.push_str(&rest[..pos]);
        let after = &rest[pos + 3..];
        // 连续的 token 字符（字母数字 / _ / -）长度，按字符边界累加。
        let tok_len: usize = after
            .char_indices()
            .take_while(|(_, c)| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
            .map(|(idx, c)| idx + c.len_utf8())
            .last()
            .unwrap_or(0);
        let total = 3 + tok_len; // 含 "sk-" 的完整 token 长度
        if total >= 12 {
            result.push_str("sk-***");
        } else {
            result.push_str(&rest[pos..pos + total]);
        }
        rest = &rest[pos + total..];
    }
    result.push_str(rest);
    result
}

/// 扫出文本里所有「看起来是密钥」的键名（JSON `"k":` 与 TOML `k =` 两种形态）。
///
/// 判据：键名（大小写不敏感）含 `token` / `secret` / `password` / `passwd` / `credential`，
/// 或以 `key` 结尾/含 `apikey`（`key` 用后缀而非包含，避免误伤 `keyId` / `keychain_path`
/// 这类非密钥字段被打码后让用户看不到自己的配置）。
fn secretish_keys(s: &str) -> Vec<String> {
    fn is_secretish(name: &str) -> bool {
        let l = name.to_ascii_lowercase();
        const CONTAINS: [&str; 6] = [
            "token",
            "secret",
            "password",
            "passwd",
            "credential",
            "apikey",
        ];
        if CONTAINS.iter().any(|p| l.contains(p)) {
            return true;
        }
        // `..._key` / `...Key` 结尾（api_key、gatewayKey、AUTH_KEY 等）。
        l.ends_with("key") || l.ends_with("_key")
    }

    let mut found: Vec<String> = Vec::new();
    let push = |name: &str, found: &mut Vec<String>| {
        if is_secretish(name) && !found.iter().any(|x| x == name) {
            found.push(name.to_string());
        }
    };

    // JSON 形态：`"key"` 紧跟（可含空白）`:`。
    let bytes = s.as_char_indices_quotes();
    for (name, followed_by_colon) in bytes {
        if followed_by_colon {
            push(&name, &mut found);
        }
    }

    // TOML 形态：行首（可含缩进）裸键名 + `=`。
    for line in s.lines() {
        let t = line.trim_start();
        if t.starts_with('#') || t.starts_with('[') {
            continue;
        }
        if let Some((lhs, _)) = t.split_once('=') {
            let name = lhs.trim().trim_matches('"');
            if !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
            {
                push(name, &mut found);
            }
        }
    }
    found
}

/// 提取文本里所有双引号字符串及其后是否紧跟 `:`（用于识别 JSON 键）。
/// 独立成 trait 方法以便 [`secretish_keys`] 读起来更直白；不处理转义（密钥键名不含 `\"`）。
trait QuotedKeys {
    fn as_char_indices_quotes(&self) -> Vec<(String, bool)>;
}

impl QuotedKeys for str {
    fn as_char_indices_quotes(&self) -> Vec<(String, bool)> {
        let mut out = Vec::new();
        let mut rest = self;
        while let Some(open) = rest.find('"') {
            let after_open = &rest[open + 1..];
            let Some(close) = after_open.find('"') else { break };
            let name = &after_open[..close];
            let tail = &after_open[close + 1..];
            let followed_by_colon = tail.trim_start().starts_with(':');
            out.push((name.to_string(), followed_by_colon));
            rest = tail;
        }
        out
    }
}

/// 把 `key = "...."`（TOML）的值换成 `***`。按行处理：跳过注释与表头，
/// 只在 `=` 左侧裸键名（可带引号）恰为 `key` 时替换右侧字符串字面量。
/// TOML 的键不带引号，故 JSON 版的 `"key"` 查找匹配不到，必须单独处理。
fn redact_toml_field(s: &str, key: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for (i, line) in s.lines().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') || trimmed.starts_with('[') {
            out.push_str(line);
            continue;
        }
        let Some((lhs, rhs)) = line.split_once('=') else {
            out.push_str(line);
            continue;
        };
        if lhs.trim().trim_matches('"') != key {
            out.push_str(line);
            continue;
        }
        let rhs_trim = rhs.trim_start();
        // 只替换字符串字面量（`"..."`）；数字/布尔等非密钥值原样保留。
        if !rhs_trim.starts_with('"') {
            out.push_str(line);
            continue;
        }
        let lead_ws = &rhs[..rhs.len() - rhs_trim.len()];
        out.push_str(lhs);
        out.push('=');
        out.push_str(lead_ws);
        out.push_str("\"***\"");
    }
    // 保留原文末尾换行（lines() 会吃掉）。
    if s.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// 把 `"key": "...."` 的值换成 `***`（JSON 形态；TOML 见 [`redact_toml_field`]）。
fn redact_json_string_field(s: &str, key: &str) -> String {
    let needle = format!("\"{key}\"");
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(idx) = rest.find(&needle) {
        out.push_str(&rest[..idx]);
        out.push_str(&needle);
        let after_key = &rest[idx + needle.len()..];

        // **不匹配就一个字符都不能提前推进 out**。
        //
        // 这里原先是「边扫边推」：先把冒号前的空白推进 out，再判断下一个字符是不是 `:`，
        // 不是就 `rest = after_key` 回退 —— 而 after_key **包含刚推过的那段空白**，
        // 下一轮又推一遍。于是每跑一次脱敏，非字符串字段的空白就翻倍。
        //
        // 这条对布尔/数值字段**必然**命中：`hasSecret`（含 secret）、
        // `masterPasswordEnabled`（含 password）都在键名判据里，而它们的值是 `true`/`false`
        // 不带引号，走的正是 bail-out 分支。症状是诊断报告与配置预览里的 JSON
        // 出现莫名的长串空格（`"hasSecret":        true`），越脱敏越长。
        //
        // 故改为：先纯读地量出 `空白 → : → 空白 → "` 这个完整形状，全部对上才推 out；
        // 任何一步不符就只推 needle、把 rest 落在 after_key，让那段原文由后续循环
        // （或末尾的 push_str(rest)）**原样**带出去。
        let ws1 = after_key.len() - after_key.trim_start().len();
        let after_ws = &after_key[ws1..];
        if !after_ws.starts_with(':') {
            rest = after_key;
            continue;
        }
        let after_colon = &after_ws[1..];
        let ws2 = after_colon.len() - after_colon.trim_start().len();
        let after_ws2 = &after_colon[ws2..];
        if !after_ws2.starts_with('"') {
            rest = after_key;
            continue;
        }

        // 形状已确认：把原文的空白与冒号照原样带上，值换成掩码。
        out.push_str(&after_key[..ws1]);
        out.push(':');
        out.push_str(&after_colon[..ws2]);
        // find closing quote (no escape handling for simplicity — secrets rarely have \")
        if let Some(end) = after_ws2[1..].find('"') {
            out.push_str("\"***\"");
            rest = &after_ws2[1 + end + 1..];
        } else {
            out.push_str(after_ws2);
            rest = "";
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn temp_file(tag: &str, name: &str) -> PathBuf {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir()
            .join(format!("synaroute_tools_test_{}_{}_{}", tag, std::process::id(), seq));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    /// MCP 注册/注销**不得**锁住「接入前快照」。
    ///
    /// # 这条测试锁的是一个用户机器上实证过的缺陷
    ///
    /// `.bak` 是**首写即锁**的，语义是「SynaRoute 第一次动这个文件之前」。而 MCP 注册
    /// 原先也走 `backup_and_write_bytes`，于是那个时间点被悄悄提前到「因为**任何**理由
    /// 第一次碰这个文件之前」—— 开大脑聚合开关通常发生在点「启动」之前。
    ///
    /// 后果（用户 `~/.codex/config.toml.synaroute.bak` 实测）：`.bak` 停在 8-16，里面带着
    /// 一个早已废弃的 `[mcp_servers.synaroute] url = "http://127.0.0.1:9527/mcp"`
    /// （HTTP 形态、端口已死）；此后 SynaRoute 自己又改过 config.toml，`.bak` 却因已上锁没更新。
    /// 点一次「还原」就把这份陈旧快照**整份写回**，而界面显示「已从备份还原」= 成功。
    ///
    /// 故障注入判据：把 MCP 的两个写点改回 `backup_and_write_bytes` → 本测试必须变红。
    #[test]
    fn mcp_registration_does_not_lock_the_apply_snapshot() {
        let cfg = temp_file("mcp_no_lock", "config.toml");
        let bak = backup_path_for(&cfg);
        let marker = created_marker_path_for(&cfg);

        // 用户原有配置
        let original = "model = \"gpt-5.6-sol\"\n\n[features]\njs_repl = false\n";
        std::fs::write(&cfg, original).unwrap();

        // ① 注册 MCP（= 开大脑聚合开关）：**不得**产生 .bak / 不得落「凭空新建」标记
        register_mcp_codex_at(&cfg, "C:/nonexistent/synaroute.exe").unwrap();
        assert!(
            !bak.exists(),
            "MCP 注册不得锁住「接入前快照」—— 它的逆操作是精确的 remove(mcp_servers.synaroute)，用不着整文件快照"
        );
        assert!(!marker.exists(), "MCP 注册不得落「凭空新建」标记");
        let after_mcp = std::fs::read_to_string(&cfg).unwrap();
        assert!(after_mcp.contains("mcp_servers"), "MCP 项本身要写进去");

        // ② 接入（真正的「接入」）：此刻才该抓快照，且快照必须是**含 MCP 的当前内容**
        codex::apply_at(&cfg, "http://127.0.0.1:47101", &[], &[], &cfg.with_extension("catalog.json")).unwrap();
        assert!(bak.exists(), "接入必须抓一份接入前快照");
        assert_eq!(
            std::fs::read_to_string(&bak).unwrap(),
            after_mcp,
            "快照必须是「接入前」的真实内容（含用户已开的 MCP），不是「碰这个文件之前」"
        );

        // ③ 还原：回到「有 MCP、没有 model_providers.synaroute」这个真实的接入前状态
        let restored = restore_one(&cfg).unwrap();
        assert!(restored);
        let back = std::fs::read_to_string(&cfg).unwrap();
        assert!(back.contains("mcp_servers"), "还原不得把用户已开的 MCP 一起抹掉");
        assert!(
            !back.contains("[model_providers.synaroute]"),
            "还原要撤掉的是接入配置：{back}"
        );
        assert!(back.contains("js_repl"), "用户其余配置段必须原样保留");

        // ④ 注销 MCP 同样不锁快照（此时 .bak 已被 restore_one 删掉，若注销去锁就会重新造一份陈旧快照）
        unregister_mcp_codex_at(&cfg).unwrap();
        assert!(!bak.exists(), "MCP 注销同样不得锁快照");

        std::fs::remove_dir_all(cfg.parent().unwrap()).ok();
    }

    #[test]
    fn claude_register_writes_http_entry_and_is_idempotent() {        let path = temp_file("claude_reg", ".claude.json");
        let url = "http://127.0.0.1:9527/mcp";

        // 首次注册：写盘
        let (_, wrote) = register_mcp_claude_at(&path, url, MCP_TOOL_TIMEOUT_MS).unwrap();
        assert!(wrote, "首次注册应写盘");

        let v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let entry = &v["mcpServers"]["synaroute"];
        assert_eq!(entry["type"], "http", "必须是官方 http transport");
        assert_eq!(entry["url"], url, "url 应写入");
        assert_eq!(entry["timeout"], 600000, "必须写入 timeout 抬高客户端首字节超时，避免聚合被判超时");

        // 再次相同 url：幂等，不写盘
        let (_, wrote2) = register_mcp_claude_at(&path, url, MCP_TOOL_TIMEOUT_MS).unwrap();
        assert!(!wrote2, "相同 url 应跳过写盘");

        // 换端口：应重写
        let url2 = "http://127.0.0.1:9600/mcp";
        let (_, wrote3) = register_mcp_claude_at(&path, url2, MCP_TOOL_TIMEOUT_MS).unwrap();
        assert!(wrote3, "url 变化应写盘");
        let v2: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(v2["mcpServers"]["synaroute"]["url"], url2);

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn claude_register_writes_coupled_timeout_and_rewrites_on_change() {
        // 客户端超时联动：写入调用方算出的 timeout（= 整轮预算 + 余量），且 timeout 变化必须重写，
        // 否则用户调大整轮预算后客户端超时不跟随，聚合仍被客户端提前杀死。
        let path = temp_file("claude_timeout", ".claude.json");
        let url = "http://127.0.0.1:9527/mcp";

        // 用一个非默认的联动值（630000 = 600000 整轮 + 30000 余量）。
        let (_, wrote) = register_mcp_claude_at(&path, url, 630_000).unwrap();
        assert!(wrote);
        let v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(v["mcpServers"]["synaroute"]["timeout"], 630000, "应写入联动算出的 timeout");

        // 同 url 同 timeout：幂等。
        let (_, wrote2) = register_mcp_claude_at(&path, url, 630_000).unwrap();
        assert!(!wrote2, "url+timeout 都没变应跳过");

        // url 不变但 timeout 变大（用户调大整轮预算）：必须重写。
        let (_, wrote3) = register_mcp_claude_at(&path, url, 1_830_000).unwrap();
        assert!(wrote3, "timeout 变化应重写");
        let v2: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(v2["mcpServers"]["synaroute"]["timeout"], 1830000);

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn claude_register_preserves_existing_servers_and_keys() {
        let path = temp_file("claude_preserve", ".claude.json");
        // 预置已有 mcpServers 与其它顶层键，注册不能破坏它们。
        std::fs::write(
            &path,
            r#"{"numStartups":5,"mcpServers":{"other":{"type":"stdio","command":"x"}}}"#,
        )
        .unwrap();

        register_mcp_claude_at(&path, "http://127.0.0.1:9527/mcp", MCP_TOOL_TIMEOUT_MS).unwrap();
        let v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(v["numStartups"], 5, "其它顶层键应保留");
        assert_eq!(v["mcpServers"]["other"]["command"], "x", "已有 MCP 应保留");
        assert_eq!(v["mcpServers"]["synaroute"]["type"], "http");

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn claude_unregister_removes_only_synaroute() {
        let path = temp_file("claude_unreg", ".claude.json");
        std::fs::write(
            &path,
            r#"{"mcpServers":{"synaroute":{"type":"http","url":"u"},"other":{"type":"stdio","command":"x"}}}"#,
        )
        .unwrap();

        let (_, wrote) = unregister_mcp_claude_at(&path).unwrap();
        assert!(wrote);
        let v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(v["mcpServers"].get("synaroute").is_none(), "synaroute 应被移除");
        assert_eq!(v["mcpServers"]["other"]["command"], "x", "其它 MCP 应保留");

        // 再次移除：无操作
        let (_, wrote2) = unregister_mcp_claude_at(&path).unwrap();
        assert!(!wrote2, "已无 synaroute，应不写盘");

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn claude_mcp_path_routes_cli_and_desktop_to_distinct_files() {
        // 回归护栏（真能证伪版）：经**真正的分派点** claude_mcp_config_paths 验证两分类落到不同
        // 文件。旧版 desktop_and_cli_mcp_use_separate_files 直调 register_mcp_claude_at(手工分好
        // 的两个路径)，绕过了分派逻辑——若有人把 ClaudeDesktop 臂改回 claude_json_path，旧测试
        // 仍绿（审查 F3）。此处直接断言分派结果：
        // 1) 两分类解析出的路径必须不同；
        // 2) 桌面端路径必须是部署目录下的 claude_desktop_config.json，CLI 必须是 ~/.claude.json；
        // 3) **桌面端必须是两份**（两个部署目录各一），只写一份会在切模式后静默失联 ——
        //    见 desktop_mcp_config_paths 的说明；一旦被改回单文件，这条断言立即失败。
        let cli = claude_mcp_config_paths(CategoryType::ClaudeCli).unwrap();
        let desktop = claude_mcp_config_paths(CategoryType::ClaudeDesktop).unwrap();
        assert_eq!(cli.len(), 1, "CLI 只有一份 ~/.claude.json");
        assert_eq!(
            desktop.len(),
            2,
            "桌面端必须写两个部署目录（1p/3p）各自的 config —— 切回官方会复位 deploymentMode \
             却保留 MCP 注册，只写一份时那句「已保留 MCP 注册」是假的"
        );
        for d in &desktop {
            assert!(!cli.contains(d), "CLI 与桌面端 MCP 必须落到不同文件，否则端口互相覆盖");
            assert_eq!(
                d.file_name().and_then(|n| n.to_str()),
                Some(DESKTOP_CONFIG_FILE),
                "桌面端应写 claude_desktop_config.json"
            );
        }
        assert_ne!(desktop[0], desktop[1], "两个部署目录的 config 必须是不同文件");
        assert_eq!(
            cli[0].file_name().and_then(|n| n.to_str()),
            Some(".claude.json"),
            "CLI 应写 ~/.claude.json"
        );
        assert!(
            desktop.iter().all(|d| d.parent() != cli[0].parent()),
            "两分类不能共用同一目录"
        );
        // Codex 不走 Claude 系路径，应报错而非静默落到某文件。
        assert!(
            claude_mcp_config_paths(CategoryType::Codex).is_err(),
            "Codex 不应解析出 Claude 系 MCP 路径"
        );
    }

    /// 桌面端「已接入 MCP」必须是**两份都在**（`all`），不是任一份在（`any`）。
    ///
    /// 桌面端读哪个 `claude_desktop_config.json` 取决于当前 `deploymentMode`，故注册两份都写。
    /// 判据若用 `any`，则「3p 那份在、1p 那份缺」也报「已接入」—— 而用户一旦「切回官方」
    /// （复位 1p、刻意保留 MCP 注册），桌面端读的正是缺的那份：界面说接了、聚合工具却消失。
    ///
    /// 顺带钉住 `register`/`unregister` 的两份对称性：注册两份 → all 为真；
    /// 只摘掉一份 → all 立刻为假（这正是 `any` 会漏掉的状态）。
    #[test]
    fn desktop_mcp_registered_requires_both_deployment_configs() {
        let (dir, normal, threep, _p, _m) = desktop_layout("mcp_both");
        let paths = vec![threep.clone(), normal.clone()];

        assert!(!all_json_have_mcp_server(&paths), "两份都没有 → 未接入");

        // 只写 3p 那份：`any` 会说「已接入」，`all` 必须说没有
        register_mcp_claude_desktop_at(&threep, "C:/x/synaroute.exe").unwrap();
        assert!(json_has_mcp_server(&threep));
        assert!(
            !all_json_have_mcp_server(&paths),
            "只有 3p 那份在时不能报「已接入」——切回官方后桌面端读的是 1p 那份，那里没有"
        );

        // 两份都写：才算真接上
        register_mcp_claude_desktop_at(&normal, "C:/x/synaroute.exe").unwrap();
        assert!(all_json_have_mcp_server(&paths), "两份都在 → 已接入");

        // 摘掉任一份就不再算接入（对称性）
        unregister_mcp_claude_at(&normal).unwrap();
        assert!(!all_json_have_mcp_server(&paths), "少一份即不算接入");

        assert!(!all_json_have_mcp_server(&[]), "空列表没有任何证据，必须为 false");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn desktop_mcp_writes_stdio_not_http() {
        // 回归护栏（用户实测 bug）：桌面端只认 stdio；写 CLI 那套 type:"http"+url 会被判
        // 「not valid MCP server configurations」整项跳过（弹窗 skipped: synaroute）。
        let (dir, _normal, threep, _profile, _meta) = desktop_layout("mcp_stdio");
        let exe = r"C:\Program Files\SynaRoute\synaroute.exe";

        let (_, wrote) = register_mcp_claude_desktop_at(&threep, exe).unwrap();
        assert!(wrote, "首次注册应写盘");

        let v: Value = serde_json::from_slice(&std::fs::read(&threep).unwrap()).unwrap();
        let entry = &v["mcpServers"]["synaroute"];
        assert_eq!(entry["command"], exe, "command 应指向当前 exe");
        assert_eq!(
            entry["args"],
            crate::mcp::stdio::args_json(CategoryType::ClaudeDesktop),
            "args 应为带分类标记的 stdio args（只有 --mcp-stdio 的话与 Codex 一字不差，服务端分辨不出）"
        );
        // 关键：绝不能出现 HTTP 形态的键，否则桌面端整项跳过。
        assert!(entry.get("type").is_none(), "stdio 项不应带 type");
        assert!(entry.get("url").is_none(), "stdio 项不应带 url");
        assert!(entry.get("timeout").is_none(), "stdio 项不应带 timeout");

        // 幂等：同 exe 再注册不写盘。
        let (_, wrote2) = register_mcp_claude_desktop_at(&threep, exe).unwrap();
        assert!(!wrote2, "同 exe 重复注册应跳过");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn desktop_mcp_replaces_legacy_http_entry() {
        // 升级路径：老版本写的 HTTP 形态残留必须被整项替换成 stdio（不能合并留下非法键）。
        let (dir, _normal, threep, _profile, _meta) = desktop_layout("mcp_stdio_migrate");
        std::fs::write(
            &threep,
            r#"{"deploymentMode":"3p","mcpServers":{"synaroute":{"type":"http","url":"http://127.0.0.1:9600/mcp","timeout":600000},"other":{"command":"keep"}}}"#,
        )
        .unwrap();

        let exe = r"C:\Program Files\SynaRoute\synaroute.exe";
        let (_, wrote) = register_mcp_claude_desktop_at(&threep, exe).unwrap();
        assert!(wrote, "HTTP 残留必须触发重写");

        let v: Value = serde_json::from_slice(&std::fs::read(&threep).unwrap()).unwrap();
        let entry = &v["mcpServers"]["synaroute"];
        assert_eq!(entry["command"], exe);
        assert!(entry.get("url").is_none(), "旧 url 键必须被清除");
        assert!(entry.get("type").is_none(), "旧 type 键必须被清除");
        assert!(entry.get("timeout").is_none(), "旧 timeout 键必须被清除");
        // 邻居配置与 deploymentMode 不受影响。
        assert_eq!(v["mcpServers"]["other"]["command"], "keep", "其它 MCP 应保留");
        assert_eq!(v["deploymentMode"], "3p", "deploymentMode 应保留");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn desktop_mcp_config_shares_file_with_deployment_mode() {
        // 桌面端 MCP 与 deploymentMode 是同一个 claude_desktop_config.json 里的并列键，
        // 互不干扰：先接入（写 deploymentMode=3p），再注册 MCP，两键应共存。
        let (dir, _normal, threep, _profile, _meta) = desktop_layout("mcp_coexist_deploy");
        write_deployment_mode(&threep, "3p").unwrap();
        register_mcp_claude_desktop_at(&threep, r"C:\x\synaroute.exe").unwrap();

        let v: Value = serde_json::from_slice(&std::fs::read(&threep).unwrap()).unwrap();
        assert_eq!(v["deploymentMode"], "3p", "deploymentMode 应保留");
        assert_eq!(
            v["mcpServers"]["synaroute"]["command"], r"C:\x\synaroute.exe",
            "mcpServers 与 deploymentMode 并列共存"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn codex_register_writes_stdio_command_and_preserves_other_tables() {
        let path = temp_file("codex_reg", "config.toml");
        std::fs::write(
            &path,
            "model = \"gpt-5\"\n\n[mcp_servers.codegraph]\ncommand = \"codegraph\"\n",
        )
        .unwrap();
        let exe = "C:\\Program Files\\SynaRoute\\synaroute.exe";

        let (_, wrote) = register_mcp_codex_at(&path, exe).unwrap();
        assert!(wrote);

        let doc: toml::Value = std::fs::read_to_string(&path).unwrap().parse().unwrap();
        // stdio 形态：command 指向 exe、args=[--mcp-stdio, --mcp-category=codex]，
        // 无 url/timeout/实验开关。
        assert_eq!(
            doc["mcp_servers"]["synaroute"]["command"].as_str(),
            Some(exe),
            "应写入 stdio command 指向 synaroute.exe"
        );
        assert_eq!(
            doc["mcp_servers"]["synaroute"]["args"],
            crate::mcp::stdio::args_toml(CategoryType::Codex),
            "args 应为带分类标记的 stdio args"
        );
        assert!(
            doc["mcp_servers"]["synaroute"].get("url").is_none(),
            "stdio 形态不应有 url"
        );
        assert_eq!(
            doc["mcp_servers"]["synaroute"]["type"].as_str(),
            Some("stdio"),
            "必须写 type=stdio（Codex 桌面端靠它识别 stdio MCP，缺了就不加载）"
        );
        assert_eq!(
            doc["mcp_servers"]["synaroute"]["tool_timeout_sec"].as_integer(),
            Some(MCP_TOOL_TIMEOUT_SEC),
            "必须写 tool_timeout_sec（默认 60s 不够聚合跑，会 user cancelled）"
        );
        assert_eq!(
            doc["mcp_servers"]["codegraph"]["command"].as_str(),
            Some("codegraph"),
            "已有 MCP 应保留"
        );
        assert_eq!(doc["model"].as_str(), Some("gpt-5"), "顶层键应保留");

        // 幂等：command/args 一致 → 跳过
        let (_, wrote2) = register_mcp_codex_at(&path, exe).unwrap();
        assert!(!wrote2, "相同 stdio 配置应跳过");

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    /// 回归：老配置是旧的 HTTP 形态（url+timeout+experimental 开关）。再次接入必须迁移成
    /// stdio 形态（command+args），不能因残留 url 而误判已最新——否则 Codex 仍连不上。
    #[test]
    fn codex_register_migrates_http_to_stdio() {
        let path = temp_file("codex_migrate", "config.toml");
        let exe = "C:\\Program Files\\SynaRoute\\synaroute.exe";
        // 预置：旧 HTTP 形态。
        std::fs::write(
            &path,
            "model = \"gpt-5\"\nexperimental_use_rmcp_client = true\n\n[mcp_servers.synaroute]\nurl = \"http://127.0.0.1:9530/mcp\"\ntimeout = 600000\n",
        )
        .unwrap();

        let (_, wrote) = register_mcp_codex_at(&path, exe).unwrap();
        assert!(wrote, "旧 HTTP 形态必须被重写为 stdio");

        let doc: toml::Value = std::fs::read_to_string(&path).unwrap().parse().unwrap();
        assert_eq!(
            doc["mcp_servers"]["synaroute"]["command"].as_str(),
            Some(exe),
            "应迁移为 stdio command"
        );
        assert!(
            doc["mcp_servers"]["synaroute"].get("url").is_none(),
            "旧 url 应被替换掉（stdio 条目整体覆盖）"
        );

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    /// 🔴 迁移判据：旧版注册的 args 只有 `--mcp-stdio`（没有分类标记），再次接入**必须重写**。
    ///
    /// 这条盯的是「幂等比对放宽了就静默失去自愈能力」：若比对只看首项 / 长度，
    /// 旧配置会被永久判为「已是最新」而跳过，用户的 Codex 就永远拿不到分类身份 ——
    /// 而这件事没有任何外部表现（工具照样能调，只是一直走 claude-cli 的 Key 池）。
    ///
    /// 预置里把 type / tool_timeout_sec / startup_timeout_sec 都填成最新值，
    /// 让**只有 args 不同**：否则别的字段也不一致，测试即便在 args 判据坏掉时也会绿
    /// （那就什么都没测到 —— CLAUDE.md 里 B64_MIN_RUN 那条就是这么栽的）。
    #[test]
    fn codex_register_rewrites_legacy_args_without_category_marker() {
        let path = temp_file("codex_cat_migrate", "config.toml");
        let exe = "C:\\Program Files\\SynaRoute\\synaroute.exe";
        std::fs::write(
            &path,
            format!(
                "model = \"gpt-5\"\n\n[mcp_servers.synaroute]\ncommand = '{exe}'\n\
                 args = [\"--mcp-stdio\"]\ntype = \"stdio\"\n\
                 tool_timeout_sec = {MCP_TOOL_TIMEOUT_SEC}\nstartup_timeout_sec = 30\n"
            ),
        )
        .unwrap();

        let (_, wrote) = register_mcp_codex_at(&path, exe).unwrap();
        assert!(wrote, "旧 args（无分类标记）必须被重写，否则自愈路径静默失效");

        let doc: toml::Value = std::fs::read_to_string(&path).unwrap().parse().unwrap();
        assert_eq!(
            doc["mcp_servers"]["synaroute"]["args"],
            crate::mcp::stdio::args_toml(CategoryType::Codex),
            "重写后 args 应带 --mcp-category=codex"
        );

        // 迁移完成后必须重新变幂等：否则每次启动都写盘 + 造备份 + 刷一条事件。
        let (_, wrote2) = register_mcp_codex_at(&path, exe).unwrap();
        assert!(!wrote2, "已是新形态时不该再写盘");

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    /// 桌面端同上：旧 args 无分类标记 → 必须重写；重写后幂等。
    #[test]
    fn desktop_register_rewrites_legacy_args_without_category_marker() {
        let (dir, _normal, threep, _profile, _meta) = desktop_layout("desk_cat_migrate");
        let exe = r"C:\Program Files\SynaRoute\synaroute.exe";
        // 预置旧形态：command 已是现役 exe，**只有 args 不同**。
        std::fs::write(
            &threep,
            serde_json::to_vec(&serde_json::json!({
                "mcpServers": { "synaroute": { "command": exe, "args": ["--mcp-stdio"] } }
            }))
            .unwrap(),
        )
        .unwrap();

        let (_, wrote) = register_mcp_claude_desktop_at(&threep, exe).unwrap();
        assert!(wrote, "旧 args（无分类标记）必须被重写");

        let v: Value = serde_json::from_slice(&std::fs::read(&threep).unwrap()).unwrap();
        assert_eq!(
            v["mcpServers"]["synaroute"]["args"],
            crate::mcp::stdio::args_json(CategoryType::ClaudeDesktop),
            "重写后 args 应带 --mcp-category=claude-desktop"
        );

        let (_, wrote2) = register_mcp_claude_desktop_at(&threep, exe).unwrap();
        assert!(!wrote2, "已是新形态时不该再写盘");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// CLI 侧同理：旧注册的 url 是裸 `/mcp`（不带分类段），必须被重写成 `/mcp/claude-cli`。
    /// 不重写的话服务端只能把它当「认不出的调用方」，走兜底。
    #[test]
    fn claude_cli_register_rewrites_legacy_url_without_category_segment() {
        let path = temp_file("cli_cat_migrate", ".claude.json");
        let base = crate::mcp::base_url(9527);
        let scoped = crate::mcp::client_url(9527, CategoryType::ClaudeCli);
        // 预置旧形态：url 是裸基址，type / timeout 都已是最新 → **只有 url 不同**。
        std::fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({
                "mcpServers": {
                    "synaroute": { "type": "http", "url": base, "timeout": MCP_TOOL_TIMEOUT_MS }
                }
            }))
            .unwrap(),
        )
        .unwrap();

        let (_, wrote) = register_mcp_claude_at(&path, &scoped, MCP_TOOL_TIMEOUT_MS).unwrap();
        assert!(wrote, "裸 /mcp 的旧 url 必须被重写成带分类段的地址");

        let v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            v["mcpServers"]["synaroute"]["url"].as_str(),
            Some(scoped.as_str())
        );
        assert!(scoped.ends_with("/claude-cli"), "地址应带分类段: {scoped}");

        let (_, wrote2) = register_mcp_claude_at(&path, &scoped, MCP_TOOL_TIMEOUT_MS).unwrap();
        assert!(!wrote2, "已是新形态时不该再写盘");

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    /// 回归：老配置已是 stdio（command/args 都对）但**缺 type="stdio"**（首版接入按钮漏写）。
    /// 再次接入必须补上 type，不能因 command/args 一致就判「已最新」跳过——否则 Codex 桌面端
    /// 靠 type 识别 stdio MCP，缺了就不加载该工具（对话里根本没有 synaroute_ai）。
    #[test]    fn codex_register_backfills_missing_type_stdio() {
        let path = temp_file("codex_type_backfill", "config.toml");
        let exe = "C:\\Program Files\\SynaRoute\\synaroute.exe";
        // 预置：stdio 形态但缺 type 字段。
        std::fs::write(
            &path,
            format!(
                "model = \"gpt-5\"\n\n[mcp_servers.synaroute]\ncommand = '{}'\nargs = [\"--mcp-stdio\"]\n",
                exe
            ),
        )
        .unwrap();

        let (_, wrote) = register_mcp_codex_at(&path, exe).unwrap();
        assert!(wrote, "缺 type 时即便 command/args 一致也必须重写补上");

        let doc: toml::Value = std::fs::read_to_string(&path).unwrap().parse().unwrap();
        assert_eq!(
            doc["mcp_servers"]["synaroute"]["type"].as_str(),
            Some("stdio"),
            "type=stdio 应被补上（Codex 桌面端识别 stdio MCP 的关键字段）"
        );

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn redact_json_string_field_masks_known_secrets() {
        let raw = r#"{"ANTHROPIC_AUTH_TOKEN":"sk-abc1234567890","OPENAI_API_KEY":"secret","model":"x"}"#;
        let out = redact_config_secrets(raw);
        assert!(out.contains(r#""ANTHROPIC_AUTH_TOKEN": "***""#) || out.contains(r#""ANTHROPIC_AUTH_TOKEN":"***""#));
        assert!(out.contains("***"));
        assert!(!out.contains("sk-abc1234567890"));
        assert!(!out.contains("secret"));
        assert!(out.contains(r#""model":"x""#) || out.contains(r#""model": "x""#));
    }

    /// 键名命中判据、但**值不是字符串**的字段必须**逐字节原样保留**。
    ///
    /// 这曾是一条真缺陷：`redact_json_string_field` 原先边扫边推 —— 先把冒号前的空白
    /// 推进结果，再判断下一个字符是不是 `:`；不是就把游标回退到**含那段空白的位置**，
    /// 下一轮又推一遍。于是每跑一次脱敏，这类字段的空白就翻倍。
    ///
    /// 它对布尔字段**必然**命中：`hasSecret` 含 secret、`masterPasswordEnabled` 含 password，
    /// 而两者的值都是不带引号的 `true`/`false`。用户看到的是配置预览与诊断报告里
    /// 莫名出现长串空格（`"hasSecret":        true`），且越脱敏越长 —— 而没人会把
    /// 「JSON 排版变怪」联想到脱敏函数。
    ///
    /// 判据取**幂等 + 原文相等**两条：只测幂等的话，一个「稳定地多插一个空格」的实现
    /// 也能通过。
    #[test]
    fn redact_leaves_non_string_values_byte_identical() {
        // 覆盖三种间距：紧贴、单空格、多空格（pretty-print 对齐后的样子）。
        let raw = concat!(
            "{\n",
            "  \"hasSecret\":true,\n",
            "  \"masterPasswordEnabled\": false,\n",
            "  \"tokenBudget\":    4096,\n",
            "  \"apiKey\": \"sk-should-be-masked\"\n",
            "}"
        );
        let once = redact_config_secrets(raw);
        let twice = redact_config_secrets(&once);

        assert_eq!(once, twice, "脱敏必须幂等：预览/报告会对同一份内容跑不止一次");
        // 非字符串值的三行必须与原文逐字节相同（间距一个都不能多）。
        for line in ["  \"hasSecret\":true,", "  \"masterPasswordEnabled\": false,", "  \"tokenBudget\":    4096,"] {
            assert!(
                once.contains(line),
                "非字符串字段被改动了间距。期望原样保留:\n  {line:?}\n实际输出:\n{once}"
            );
        }
        // 而字符串值仍要被掩码（别为了修间距把脱敏本身弄丢）。
        assert!(!once.contains("sk-should-be-masked"), "字符串密钥仍必须被掩码: {once}");
        assert!(once.contains("\"apiKey\": \"***\""), "掩码形态应保持: {once}");
    }

    /// **同一个键名出现多次**时，每一处都要按自己的值类型独立处理。
    ///
    /// 这条是上面那个 bail-out 修复的直接后果：修法把不匹配时的游标落点从
    /// `after_colon` 改成了 `after_key`，也就是「让那段原文交给后续循环重新扫」。
    /// 若这个落点选错（例如落回 `rest` 开头），多次出现就会死循环或漏掉后面的；
    /// 落得太远（跳过整个值）则会漏掉紧跟其后的另一处同名键。
    ///
    /// 这里刻意把布尔与字符串**交替排列**：非字符串那次走 bail-out、字符串那次走替换，
    /// 两条路径交错走完还要保持各自正确。
    #[test]
    fn redact_handles_the_same_key_appearing_repeatedly() {
        let raw = concat!(
            "{\n",
            "  \"apiKey\": true,\n",
            "  \"nested\": { \"apiKey\": \"leak-one\" },\n",
            "  \"apiKey\": 0,\n",
            "  \"tail\": { \"apiKey\": \"leak-two\" }\n",
            "}"
        );
        let out = redact_config_secrets(raw);

        assert!(!out.contains("leak-one"), "第一个字符串值必须被掩码: {out}");
        assert!(!out.contains("leak-two"), "最后一个也必须被掩码（游标落点若跳太远会漏掉它）: {out}");
        assert!(out.contains("\"apiKey\": true,"), "布尔那次要原样保留: {out}");
        assert!(out.contains("\"apiKey\": 0,"), "数值那次要原样保留: {out}");
        assert_eq!(out.matches("\"***\"").count(), 2, "应恰好掩掉两处: {out}");
        assert_eq!(redact_config_secrets(&out), out, "多次出现的情形也必须幂等");
    }

    /// 端到端：配置只读预览读**真实形状**的 `~/.claude.json`，必须掩掉密钥、
    /// 且不弄坏其余 JSON。
    ///
    /// 上面那几条只测脱敏函数本身。这条走 `read_preview_text` —— 用户在
    /// 「配置预览」面板里看到的就是它的返回值。夹具按 `apply_claude_cli` 真实写入的字段
    /// 拼（`env.ANTHROPIC_BASE_URL` / `ANTHROPIC_AUTH_TOKEN` / 顶层 `model` / `mcpServers`），
    /// 再掺进一个布尔字段 —— 那正是空白翻倍缺陷发作的位置。
    ///
    /// 判据里包含「输出仍是合法 JSON」：脱敏是纯文本替换，切错一个引号就会让整段
    /// 预览变成不可读的乱码，而那种坏法在只断言「不含密钥」的测试下是绿的。
    #[test]
    fn preview_of_a_realistic_claude_config_masks_secrets_and_stays_valid_json() {
        let path = temp_file("preview_real", ".claude.json");
        let raw = concat!(
            "{\n",
            "  \"env\": {\n",
            "    \"ANTHROPIC_BASE_URL\": \"http://127.0.0.1:8787\",\n",
            "    \"ANTHROPIC_AUTH_TOKEN\": \"sk-ant-real-looking-token-value\",\n",
            "    \"GATEWAY_MODEL_DISCOVERY\": \"1\"\n",
            "  },\n",
            "  \"model\": \"claude-opus-4-5\",\n",
            "  \"hasSecret\": true,\n",
            "  \"mcpServers\": {\n",
            "    \"synaroute\": { \"type\": \"http\", \"url\": \"http://127.0.0.1:9527/mcp\", \"timeout\": 600000 }\n",
            "  }\n",
            "}\n"
        );
        std::fs::write(&path, raw).unwrap();

        let (exists, text) = read_preview_text(&path, true).unwrap();
        assert!(exists);
        let text = text.expect("应读到内容");

        // 密钥掩掉，但排障要用的其余字段一个不能少。
        assert!(!text.contains("sk-ant-real-looking-token-value"), "令牌必须被掩码:\n{text}");
        assert!(text.contains("http://127.0.0.1:8787"), "baseUrl 要留着（它常是问题根源）:\n{text}");
        assert!(text.contains("claude-opus-4-5"), "模型名要留着:\n{text}");
        assert!(text.contains("\"timeout\": 600000"), "MCP timeout 要留着:\n{text}");
        // 布尔字段原样（空白翻倍缺陷正是在这里发作）。
        assert!(text.contains("\"hasSecret\": true,"), "布尔字段间距被改动了:\n{text}");

        // 仍是合法 JSON —— 切错引号的坏法只有这条能抓到。
        let parsed: Value = serde_json::from_str(&text)
            .unwrap_or_else(|e| panic!("脱敏后不再是合法 JSON（{e}）:\n{text}"));
        assert_eq!(parsed["env"]["ANTHROPIC_AUTH_TOKEN"], "***");
        assert_eq!(parsed["env"]["ANTHROPIC_BASE_URL"], "http://127.0.0.1:8787");
        assert_eq!(parsed["hasSecret"], true);

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn redact_handles_non_ascii_without_panic() {
        // 配置常含中文路径/工作目录（本机 Windows 用户名即中文）。脱敏必须按字符边界扫描，
        // 不得因 sk- 扫描的字节切片切在多字节字符中间而 panic，且非 ASCII 不能被拆成乱码。
        let raw = r#"{"cwd":"C:\\Users\\莫海明\\项目","OPENAI_API_KEY":"sk-abcdefghijklmnop","note":"路径含中文🚀"}"#;
        let out = redact_config_secrets(raw);
        // 已知字段脱敏
        assert!(!out.contains("sk-abcdefghijklmnop"));
        assert!(out.contains("***"));
        // 非 ASCII 原样保留（未乱码）
        assert!(out.contains("莫海明"));
        assert!(out.contains("项目"));
        assert!(out.contains("路径含中文🚀"));
    }

    #[test]
    fn redact_bare_sk_token_only_when_long_enough() {
        // 短 sk- 串（不足 12 字符）不脱敏；达到阈值才脱敏。
        let short = redact_config_secrets("prefix sk-abc done");
        assert!(short.contains("sk-abc"), "短 token 应原样保留: {short}");
        let long = redact_config_secrets("prefix sk-abcdefghij done");
        assert!(long.contains("sk-***"));
        assert!(!long.contains("sk-abcdefghij"));
        assert!(long.contains("prefix ") && long.contains(" done"), "周边文本应保留");
    }

    #[test]
    fn preview_truncate_respects_utf8_boundary() {
        // 构造刚好跨 CAP 的多字节字符，截断不得 panic
        let mut s = "a".repeat(31_998);
        s.push('中'); // 3 bytes UTF-8
        s.push_str("tail");
        let path = temp_file("preview_utf8", "settings.json");
        std::fs::write(&path, &s).unwrap();
        let (exists, content) = read_preview_text(&path, false).unwrap();
        assert!(exists);
        let c = content.unwrap();
        assert!(c.contains("truncated"));
        // 必须是合法 UTF-8（unwrap 已保证）且不含 panic
        assert!(c.is_char_boundary(c.len()));
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn claude_cli_apply_overwrites_model_defaults_like_cc_switch() {
        // 策略 A：只写 ANTHROPIC_MODEL + 顶层 model；并清除三档 DEFAULT_* 残留。
        // 不写 DEFAULT_*，避免 /model 出现三个 Custom 同名。仅 Claude CLI 路径。
        let path = temp_file("claude_cli_model", "settings.json");
        std::fs::write(
            &path,
            r#"{
  "env": {
    "ANTHROPIC_BASE_URL": "http://old:1",
    "ANTHROPIC_MODEL": "claude-synaroute-grok-4.5",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL": "grok-4.5",
    "ANTHROPIC_DEFAULT_SONNET_MODEL": "grok-4.5",
    "ANTHROPIC_DEFAULT_OPUS_MODEL": "grok-4.5"
  },
  "model": "claude-synaroute-grok-4.5"
}"#,
        )
        .unwrap();

        let msg = apply_claude_cli_at(
            &path,
            "http://127.0.0.1:8788",
            Some("claude-opus-4-7"),
        )
        .unwrap();
        assert!(msg.contains("默认模型=claude-opus-4-7"));
        assert!(msg.contains("未写三档 DEFAULT_*"));

        let v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(v["model"], "claude-opus-4-7");
        assert_eq!(v["env"]["ANTHROPIC_BASE_URL"], "http://127.0.0.1:8788");
        assert_eq!(v["env"]["ANTHROPIC_MODEL"], "claude-opus-4-7");
        // 策略 A：必须清除三档 DEFAULT_*
        assert!(v["env"].get("ANTHROPIC_DEFAULT_HAIKU_MODEL").is_none());
        assert!(v["env"].get("ANTHROPIC_DEFAULT_SONNET_MODEL").is_none());
        assert!(v["env"].get("ANTHROPIC_DEFAULT_OPUS_MODEL").is_none());
        assert_eq!(v["env"]["CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY"], "1");
        assert_eq!(v["env"]["ANTHROPIC_AUTH_TOKEN"], "synaroute-proxy");

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }


    #[test]
    fn claude_cli_apply_skips_model_fields_when_no_default() {
        // 取不到可服务模型时，不碰用户已有 model / ANTHROPIC_MODEL；但仍清 DEFAULT_* 残留
        let path = temp_file("claude_cli_skip", "settings.json");
        std::fs::write(
            &path,
            r#"{"env":{"ANTHROPIC_MODEL":"keep-me","ANTHROPIC_DEFAULT_OPUS_MODEL":"stale"},"model":"keep-me"}"#,
        )
        .unwrap();

        apply_claude_cli_at(&path, "http://127.0.0.1:8788", None).unwrap();
        let v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(v["model"], "keep-me");
        assert_eq!(v["env"]["ANTHROPIC_MODEL"], "keep-me");
        assert_eq!(v["env"]["ANTHROPIC_BASE_URL"], "http://127.0.0.1:8788");
        assert!(
            v["env"].get("ANTHROPIC_DEFAULT_OPUS_MODEL").is_none(),
            "即无 default_model 也应清掉 DEFAULT_* 残留"
        );

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }


    #[test]
    fn codex_unregister_removes_only_synaroute() {
        let path = temp_file("codex_unreg", "config.toml");
        std::fs::write(
            &path,
            "[mcp_servers.synaroute]\nurl = \"u\"\n\n[mcp_servers.codegraph]\ncommand = \"codegraph\"\n",
        )
        .unwrap();

        let (_, wrote) = unregister_mcp_codex_at(&path).unwrap();
        assert!(wrote);
        let doc: toml::Value = std::fs::read_to_string(&path).unwrap().parse().unwrap();
        assert!(
            doc["mcp_servers"].get("synaroute").is_none(),
            "synaroute 应被移除"
        );
        assert!(
            doc["mcp_servers"].get("codegraph").is_some(),
            "其它 MCP 应保留"
        );

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn restore_one_recovers_from_backup() {
        // backup_and_write_bytes 先备份原文件、再写新内容；restore_one 应把原内容还原回来。
        let path = temp_file("restore_one", "auth.json");
        std::fs::write(&path, b"ORIGINAL_OAUTH").unwrap();

        // 模拟接入：备份原文件并写入占位内容。
        backup_and_write_bytes(&path, b"PLACEHOLDER").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"PLACEHOLDER");

        // 断开还原：应从 .synaroute.bak 拿回原内容。
        let ok = restore_one(&path).unwrap();
        assert!(ok, "备份存在应还原并返回 true");
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"ORIGINAL_OAUTH",
            "restore_one 应还原到接入前的原始内容"
        );

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    /// 还原是**整文件回滚到首写即锁的旧快照**，会抹掉用户在那之后的全部自有改动。
    /// 覆盖前必须留一份现场，否则卸载路径上这一步之后应用就没了，用户无从找回。
    ///
    /// 具体场景：用户 1 月接入，`.bak` 锁死为 1 月内容；此后数月 Claude Code 往
    /// `settings.json` 追加 permissions/hooks/statusLine。8 月卸载 → 整文件覆盖回 1 月，
    /// 且旧实现随即删掉 `.bak` —— 磁盘上一份副本都不剩。
    #[test]
    fn restore_one_keeps_prerestore_copy_before_overwriting() {
        let path = temp_file("restore_prerestore", "settings.json");
        std::fs::write(&path, b"ORIGINAL").unwrap();

        // 接入：备份原文件并写入代理配置。
        backup_and_write_bytes(&path, b"PROXY_CONFIG").unwrap();
        // 用户此后自己改了这个文件（Claude Code 追加 permissions 等）。
        std::fs::write(&path, b"PROXY_CONFIG + USER_EDITS_MONTHS_LATER").unwrap();

        assert!(restore_one(&path).unwrap());

        // 还原本身照旧：回到接入前的内容。
        assert_eq!(std::fs::read(&path).unwrap(), b"ORIGINAL");

        // 关键：被覆盖掉的那份用户内容必须还在盘上，可回滚。
        let scene = prerestore_path_for(&path);
        assert!(
            scene.exists(),
            "还原前必须保留现场副本，否则用户数月的自有配置不可恢复"
        );
        assert_eq!(
            std::fs::read(&scene).unwrap(),
            b"PROXY_CONFIG + USER_EDITS_MONTHS_LATER",
            "现场副本应是被覆盖前的实际内容"
        );

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    /// 接入前文件根本不存在（如未装 settings.json 的 Claude CLI 用户）：还原必须把它
    /// 整份删掉，而不是留在盘上指着一个已经没人监听的端口。
    ///
    /// 钉住的是一条 P1：旧实现只在 `path.exists()` 时才拍 `.bak`，凭空新建的文件从不
    /// 产生备份，`restore_one` 因 `!backup.exists()` 直接判定「无需还原」，文件永久残留。
    #[test]
    fn restore_one_deletes_file_that_was_created_from_nothing() {
        let path = temp_file("restore_created", "settings.json");
        assert!(!path.exists(), "前提：接入前该文件不存在");

        // 接入：凭空新建（无原内容可备份，应落 created 标记而非 .bak）。
        backup_and_write_bytes(&path, b"PROXY_CONFIG").unwrap();
        assert!(path.exists());
        assert!(
            !backup_path_for(&path).exists(),
            "凭空新建不应产生 .bak（没有原内容）"
        );
        assert!(
            created_marker_path_for(&path).exists(),
            "凭空新建应落 created 标记，供还原时判定「该整份删除」"
        );

        // 还原：应删除整个文件（不是覆盖成某个内容），且清掉标记。
        let ok = restore_one(&path).unwrap();
        assert!(ok, "有 created 标记应视为「有还原动作」返回 true");
        assert!(!path.exists(), "凭空新建的文件还原后应被整份删除");
        assert!(
            !created_marker_path_for(&path).exists(),
            "还原成功后应清掉标记，供下一轮接入重新判定"
        );

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    /// 二次接入（如改端口重新 apply）不得把「凭空新建」误判成「原本存在」。
    ///
    /// 钉住的是修复过程中我自己引入过的一个后续漏洞：若判断顺序错了，第二次
    /// `backup_and_write_bytes` 会看到 `path.exists() == true`（因为第一次接入已经把
    /// 文件写出来了），把 SynaRoute 自己写的内容当成「原始快照」拷进 `.bak`，导致
    /// marker 与 `.bak` 同时存在——还原时只看 `.bak`，会把文件「还原」成 SynaRoute
    /// 自己写的版本，而不是删除它，凭空新建的文件从此永远回不到「不存在」这个真实原状。
    #[test]
    fn repeated_apply_after_created_from_nothing_does_not_fabricate_a_backup() {
        let path = temp_file("restore_created_twice", "config.toml");
        assert!(!path.exists());

        // 第一次接入：凭空新建，落 created 标记。
        backup_and_write_bytes(&path, b"PROXY_CONFIG_V1").unwrap();
        // 第二次接入（如改端口）：文件此刻已存在，不能因此改判成「备份」分支。
        backup_and_write_bytes(&path, b"PROXY_CONFIG_V2").unwrap();

        assert!(
            !backup_path_for(&path).exists(),
            "不应凭空产生 .bak —— 那会让 restore_one 走错分支，把「删除」误判成「还原成 V2」"
        );
        assert!(
            created_marker_path_for(&path).exists(),
            "created 标记必须原样保留，直到真正还原时才清"
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"PROXY_CONFIG_V2");

        let ok = restore_one(&path).unwrap();
        assert!(ok);
        assert!(
            !path.exists(),
            "无论中间接入过几次，凭空新建的文件最终还原都应是删除，不是回退到某个中间版本"
        );

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn restore_one_skips_when_no_backup() {
        // 备份不存在（如用户接入前无 auth.json）：应返回 false 且不报错、不动目标文件。
        let path = temp_file("restore_none", "auth.json");
        std::fs::write(&path, b"UNTOUCHED").unwrap();

        let ok = restore_one(&path).unwrap();
        assert!(!ok, "无备份应返回 false");
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"UNTOUCHED",
            "无备份时不应改动目标文件"
        );

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn repeated_apply_does_not_clobber_backup() {
        // Q4 回归：重复接入（内容相同）不得把已接入内容再拷进 .bak，否则冲掉接入前的原始备份。
        let path = temp_file("no_clobber", "config.toml");
        std::fs::write(&path, b"OFFICIAL_CONFIG").unwrap();

        // 首次接入：原始官方内容进 .bak，写入「已接入」内容。
        backup_and_write_bytes(&path, b"SYNAROUTE_CONFIG").unwrap();
        let backup = backup_path_for(&path);
        assert_eq!(std::fs::read(&backup).unwrap(), b"OFFICIAL_CONFIG");

        // 二次接入（内容相同）：内容相等守卫应短路，.bak 必须仍是官方内容、不被覆盖。
        backup_and_write_bytes(&path, b"SYNAROUTE_CONFIG").unwrap();
        assert_eq!(
            std::fs::read(&backup).unwrap(),
            b"OFFICIAL_CONFIG",
            "重复接入不得把已接入内容覆盖进 .bak（否则官方配置备份永久丢失）"
        );

        // 还原应拿回官方内容。
        assert!(restore_one(&path).unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), b"OFFICIAL_CONFIG");

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn repeated_apply_with_changed_content_keeps_original_backup() {
        // P0 回归（数据丢失级）：重复接入且**内容变化**时，.bak 仍必须是接入前的原始内容。
        //
        // 旧实现只有「内容逐字节相同」才短路，否则无条件覆盖 .bak。而真实重复接入的内容几乎
        // 总会变——改代理端口、可服务模型列表变化、升级换目录导致 MCP 的 command 路径变化
        // ——于是第二次接入把「已接入 v1」拷进 .bak，用户点还原拿回的是已接入态，官方配置
        // 与 ChatGPT OAuth 登录永久丢失（CLI/Codex 的 restore 直接读 .bak）。
        // 旧测试 repeated_apply_does_not_clobber_backup 只覆盖「内容相同」，故一直是绿的。
        let path = temp_file("changed_clobber", "config.toml");
        std::fs::write(&path, b"OFFICIAL_CONFIG").unwrap();
        let backup = backup_path_for(&path);

        // 接入 v1（如端口 8787）
        backup_and_write_bytes(&path, b"SYNAROUTE_V1_PORT_8787").unwrap();
        assert_eq!(std::fs::read(&backup).unwrap(), b"OFFICIAL_CONFIG");

        // 接入 v2（用户改了端口 → 内容不同，旧实现在此把 v1 写进 .bak）
        backup_and_write_bytes(&path, b"SYNAROUTE_V2_PORT_9999").unwrap();
        assert_eq!(
            std::fs::read(&backup).unwrap(),
            b"OFFICIAL_CONFIG",
            ".bak 首写即锁：内容变化的重复接入也不得覆盖接入前的原始快照"
        );

        // 接入 v3，再确认一次（多次变化都不动 .bak）
        backup_and_write_bytes(&path, b"SYNAROUTE_V3_MORE_MODELS").unwrap();
        assert_eq!(std::fs::read(&backup).unwrap(), b"OFFICIAL_CONFIG");

        // 还原：必须拿回**原始**内容，而不是任何一版已接入态。
        assert!(restore_one(&path).unwrap());
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"OFFICIAL_CONFIG",
            "还原必须回到接入前的原始配置"
        );

        // 还原后 .bak 应被删除，让下一轮接入重新抓新鲜快照
        // （否则锁死的旧快照会在「还原→改配置→再接入→再还原」时把用户改动覆盖回很久以前）。
        assert!(
            !backup.exists(),
            "还原成功后应删除 .bak，使下次接入重新抓取接入前快照"
        );

        // 还原后再接入：新的 .bak 应记录「本轮接入前」的内容（即刚还原出的原始内容）。
        std::fs::write(&path, b"USER_EDITED_AFTER_RESTORE").unwrap();
        backup_and_write_bytes(&path, b"SYNAROUTE_V4").unwrap();
        assert_eq!(
            std::fs::read(&backup).unwrap(),
            b"USER_EDITED_AFTER_RESTORE",
            "新一轮接入应抓取当轮接入前的内容作为备份"
        );

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }



    #[test]
    fn redact_masks_oauth_tokens() {
        // Q5 回归：ChatGPT OAuth 令牌（JWT，非 sk- 前缀）必须按键名脱敏，不得明文出现在预览里。
        let raw = r#"{"tokens":{"access_token":"eyJhbGciOiJ.SECRET.SIG","refresh_token":"rt-abc123","id_token":"eyJ.id.tok"}}"#;
        let out = redact_config_secrets(raw);
        assert!(!out.contains("eyJhbGciOiJ.SECRET.SIG"), "access_token 明文不得残留");
        assert!(!out.contains("rt-abc123"), "refresh_token 明文不得残留");
        assert!(!out.contains("eyJ.id.tok"), "id_token 明文不得残留");
        assert!(out.contains("***"), "应替换为脱敏占位");
    }

    #[test]
    fn redact_covers_toml_and_variant_key_names() {
        // 安全回归（审计 #19 + #18）：
        // 1) Codex config.toml 此前整份明文回显（preview_codex 传 redact_secrets=false）；
        // 2) 脱敏只认固定白名单 + sk- 前缀，用户手配的变体命名密钥一律漏网。
        // TOML 形态：键不带引号，JSON 版的 `"key"` 查找匹配不到，必须走 redact_toml_field。
        let toml_raw = r#"
model = "gpt-5"

[model_providers.other]
name = "Vendor"
env_key = "MOONSHOT_API_KEY"
MOONSHOT_API_KEY = "ms-plaintext-secret-value"
myVendorToken = "tok-plaintext-1234567890"
CLIENT_SECRET = "cs-plaintext-abcdef"
base_url = "https://api.example.com"
tool_timeout_sec = 600
"#;
        let out = redact_config_secrets(toml_raw);
        assert!(
            !out.contains("ms-plaintext-secret-value"),
            "TOML 里 *_API_KEY 的值必须脱敏: {out}"
        );
        assert!(
            !out.contains("tok-plaintext-1234567890"),
            "变体命名 *Token 必须脱敏: {out}"
        );
        assert!(
            !out.contains("cs-plaintext-abcdef"),
            "*_SECRET 必须脱敏: {out}"
        );
        // 非密钥字段必须原样保留，否则用户看不到自己的配置。
        assert!(out.contains("https://api.example.com"), "base_url 应保留");
        assert!(out.contains(r#"model = "gpt-5""#), "model 应保留");
        assert!(out.contains("tool_timeout_sec = 600"), "数字值应保留");
        assert!(
            out.contains("[model_providers.other]"),
            "表头应保留"
        );
    }

    #[test]
    fn redact_spares_non_secret_key_like_names() {
        // 反向护栏：`key` 只按后缀判定，不能把 keyId / keychain_path 这类非密钥字段打码，
        // 否则用户在预览里看不到自己的配置标识。
        let raw = r#"{"keyId":"k_123456","keychain_path":"C:\\x\\y","monkey":"not-secret"}"#;
        let out = redact_config_secrets(raw);
        assert!(out.contains("k_123456"), "keyId 不应被脱敏: {out}");
        assert!(out.contains("C:\\\\x\\\\y"), "keychain_path 不应被脱敏: {out}");
    }








    #[test]
    fn with_rollback_restores_all_files_on_failure() {
        // 多文件写入中途失败：已改的文件应回滚到原内容，原本不存在的应被删除。
        let dir = std::env::temp_dir().join(format!(
            "synaroute_rollback_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let existing = dir.join("existing.json");
        let fresh = dir.join("fresh.json");
        std::fs::write(&existing, b"ORIGINAL").unwrap();
        // fresh 原本不存在

        let paths = vec![existing.clone(), fresh.clone()];
        let result: AppResult<()> = with_rollback(&paths, || {
            // 先改两个文件，再返回 Err，模拟「第二步失败」。
            crate::secret::atomic_write(&existing, b"MODIFIED")?;
            crate::secret::atomic_write(&fresh, b"NEW")?;
            Err(AppError::ToolConfig("模拟失败".into()))
        });

        assert!(result.is_err(), "闭包返回 Err 应上抛");
        // existing 回滚到原内容
        assert_eq!(
            std::fs::read(&existing).unwrap(),
            b"ORIGINAL",
            "已存在文件应回滚到原内容"
        );
        // fresh 原本不存在 → 应被删除
        assert!(!fresh.exists(), "原本不存在的文件应在回滚时被删除");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 回滚必须**连副文件一起**回滚（`.synaroute.bak` / `.synaroute-created`）。
    ///
    /// 这条锁的是一条**数据丢失级**缺陷，完整链条见 `with_rollback` 的文档。要点：
    /// `backup_and_write_bytes` 在首次写一个**原本不存在**的文件时会落下
    /// `.synaroute-created` 标记；若回滚只管主文件、把标记留在盘上，那么
    /// ① 下一次接入会因为「标记在」而跳过整块备份代码（真凭据被直接覆盖、`.bak` 从未生成），
    /// ② 之后点还原会走标记支路 `remove_file` 把用户真凭据删掉，且那条支路不留 prerestore。
    /// 全程静默、且不可自愈。
    ///
    /// 故障注入判据：把 `with_rollback` 里那两行 `tracked.push(backup_path_for/created_marker_path_for)`
    /// 去掉 → 本测试必须变红。
    #[test]
    fn with_rollback_also_rolls_back_the_bak_and_created_marker() {
        let dir = std::env::temp_dir().join(format!(
            "synaroute_rollback_side_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        // 形态 A：主文件原本**不存在** → backup_and_write_bytes 会落 `.synaroute-created`。
        let fresh = dir.join("auth.json");
        let fresh_marker = created_marker_path_for(&fresh);
        let result: AppResult<()> = with_rollback(std::slice::from_ref(&fresh), || {
            backup_and_write_bytes(&fresh, b"{\"OPENAI_API_KEY\":\"placeholder\"}")?;
            assert!(fresh_marker.exists(), "前置条件：写入时应落下新建标记");
            Err(AppError::ToolConfig("模拟读回校验失败".into()))
        });
        assert!(result.is_err());
        assert!(!fresh.exists(), "主文件应被删除");
        assert!(
            !fresh_marker.exists(),
            "新建标记必须一并回滚 —— 留着它会让下一次接入跳过备份、之后的还原删掉用户真凭据"
        );

        // 形态 B：主文件原本**存在** → backup_and_write_bytes 会落 `.synaroute.bak`。
        let existing = dir.join("config.toml");
        let existing_bak = backup_path_for(&existing);
        std::fs::write(&existing, b"ORIGINAL").unwrap();
        let result: AppResult<()> = with_rollback(std::slice::from_ref(&existing), || {
            backup_and_write_bytes(&existing, b"MODIFIED")?;
            assert!(existing_bak.exists(), "前置条件：写入时应落下 .bak");
            Err(AppError::ToolConfig("模拟读回校验失败".into()))
        });
        assert!(result.is_err());
        assert_eq!(std::fs::read(&existing).unwrap(), b"ORIGINAL");
        assert!(
            !existing_bak.exists(),
            "回滚后 .bak 也该消失：它是「首写即锁」的接入前快照，而这次接入压根没成立。\
             留着它会把「接入前快照」锁在一个从未生效的时刻上"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn with_rollback_keeps_writes_on_success() {
        // 闭包成功：所有写入应保留，不触发回滚。
        let dir = std::env::temp_dir().join(format!(
            "synaroute_rollback_ok_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("f.json");

        let paths = vec![f.clone()];
        let result: AppResult<()> = with_rollback(&paths, || {
            crate::secret::atomic_write(&f, b"WRITTEN")?;
            Ok(())
        });

        assert!(result.is_ok());
        assert_eq!(std::fs::read(&f).unwrap(), b"WRITTEN", "成功时写入应保留");

        std::fs::remove_dir_all(&dir).ok();
    }

    // ---- Claude 桌面端 3p 部署模式 ----

    /// 在临时目录里搭出 normal / 3p 两个部署目录，返回 (dir, normal_config, threep_config, profile, meta)。
    fn desktop_layout(tag: &str) -> (PathBuf, PathBuf, PathBuf, PathBuf, PathBuf) {
        let normal = temp_file(tag, DESKTOP_CONFIG_FILE);
        let dir = normal.parent().unwrap().to_path_buf();
        let threep_dir = dir.join("threep");
        std::fs::create_dir_all(&threep_dir).unwrap();
        let threep_config = threep_dir.join(DESKTOP_CONFIG_FILE);
        let profile = desktop_profile_path(&threep_dir);
        let meta = desktop_meta_path(&threep_dir);
        (dir, normal, threep_config, profile, meta)
    }

    /// 桌面端测试用模型列表。接入已挡空列表（空 → 报错，见 apply_desktop_at），
    /// 故凡不专门测「空列表」的用例都必须给非空模型。
    fn desktop_test_models() -> Vec<String> {
        vec!["claude-opus-4-8".to_string()]
    }

    /// 造一个桌面端 Key：`models` 为 (真实名, contextWindow) 对，`mappings` 为 (对外名, 真实名)。
    fn desktop_key(
        models: &[(&str, Option<u32>)],
        mappings: &[(&str, &str)],
    ) -> ProviderKey {
        use crate::model::{HealthState, KeyParams, ModelInfo, ModelMapping, Protocol};
        ProviderKey {
            id: "k1".into(),
            category_id: CategoryType::ClaudeDesktop,
            name: "k".into(),
            vendor: "test".into(),
            base_url: "https://example.com".into(),
            protocol: Protocol::Anthropic,
            has_secret: true,
            enabled: true,
            allow_in_aggregate: false,
            priority: 0,
            headers_json: None,
            params: KeyParams::default(),
            models: models
                .iter()
                .map(|(n, ctx)| ModelInfo {
                    real_name: (*n).into(),
                    source: "manual".into(),
                    fetched_at: None,
                    context_window: *ctx,
                    max_output_tokens: None,
                })
                .collect(),
            mappings: mappings
                .iter()
                .enumerate()
                .map(|(i, (out, real))| ModelMapping {
                    id: format!("m{i}"),
                    expected_name: (*out).into(),
                    real_name: (*real).into(),
                })
                .collect(),
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

    /// `supports1m` 是**能力断言**：只在该对外名解析到的上游模型 contextWindow ≥ 1M 时写。
    ///
    /// 官方文档原话是「a capability assertion you make about your deployment，只对确认支持
    /// 1M 窗口的模型设置」。此前一律写 true——上游实际不支持时，桌面端会给出一个必然失败的
    /// 1M 选项，用户选中后每次请求都报错、且看不出是配置问题。
    #[test]
    fn desktop_supports1m_follows_context_window() {
        // 对外名 → 真实名经映射；窗口大小记在真实名那条 ModelInfo 上，故需先 resolve 再查。
        let key = desktop_key(
            &[("glm-4.6", Some(1_000_000)), ("glm-4.5", Some(128_000))],
            &[("claude-opus-4-8", "glm-4.6"), ("claude-sonnet-4-5", "glm-4.5")],
        );
        let entries = build_desktop_model_entries(
            &[
                "claude-opus-4-8".to_string(),
                "claude-sonnet-4-5".to_string(),
            ],
            std::slice::from_ref(&key),
        );

        assert!(entries[0].supports1m, "1M 窗口的模型应断言 supports1m");
        assert!(
            !entries[1].supports1m,
            "128k 窗口不得断言 supports1m（否则桌面端给出必然失败的 1M 选项）"
        );
    }

    #[test]
    fn desktop_supports1m_stays_false_without_context_data() {
        // 没有 contextWindow 数据（用户手填模型名、未拉取）→ 保守不断言。
        let key = desktop_key(&[("glm-4.6", None)], &[("claude-opus-4-8", "glm-4.6")]);
        let entries =
            build_desktop_model_entries(&["claude-opus-4-8".to_string()], std::slice::from_ref(&key));
        assert!(!entries[0].supports1m, "无依据时不做能力断言");

        // 连 Key 都没有（如 preview / 改端口回退路径）→ 同样不断言，且不 panic。
        let none = build_desktop_model_entries(&["claude-opus-4-8".to_string()], &[]);
        assert!(!none[0].supports1m);
    }

    /// 窗口数据只认**主 Key**，不得从备用 Key 借。
    ///
    /// 失败场景：主 Key 把 `claude-opus-4-8` 映射到没记 `context_window` 的模型
    /// （`fetch_models` 拉来的一律为 None，很常见），备用 Key 恰好记了 1M。逐 Key 找「第一个
    /// 有数据的」会写出 `supports1m: true`，而请求实际落在主 Key 的模型上 —— 桌面端给出一个
    /// 必然被截断的 1M 选项，恰是这个断言要防的后果。
    #[test]
    fn desktop_supports1m_uses_primary_key_only_not_fallback() {
        // 主 Key：映射命中但无窗口数据
        let primary = desktop_key(&[("glm-4.6", None)], &[("claude-opus-4-8", "glm-4.6")]);
        // 备用 Key：同一个对外名，映射到一个记了 1M 的模型
        let mut backup = desktop_key(
            &[("other-1m", Some(1_000_000))],
            &[("claude-opus-4-8", "other-1m")],
        );
        backup.id = "k2".into();
        backup.priority = 1;

        let entries = build_desktop_model_entries(
            &["claude-opus-4-8".to_string()],
            &[primary, backup],
        );
        assert!(
            !entries[0].supports1m,
            "主 Key 无窗口数据时必须保守 false，不能借用备用 Key 的 1M（路由落点是主 Key）"
        );
    }

    /// 兜底解析路径不得产出窗口断言。
    ///
    /// `resolve_model` 在 Key 不认识请求名时会一路兜底到 `default_model` / `models` 首个 ——
    /// 那时返回的是「随便给你一个」，拿它的窗口去断言请求名的能力毫无依据。
    #[test]
    fn desktop_supports1m_ignores_fallback_resolution() {
        // Key 只认识 big-1m（记了 1M），没有任何映射；请求一个它不认识的对外名。
        let mut key = desktop_key(&[("big-1m", Some(1_000_000))], &[]);
        key.default_model = Some("big-1m".into());

        // 对外名 unknown-name 走不到 Mapping/Tier/Native，只能兜底到 big-1m
        let entries = build_desktop_model_entries(
            &["unknown-name".to_string()],
            std::slice::from_ref(&key),
        );
        assert!(
            !entries[0].supports1m,
            "兜底解析不是该对外名的真实能力，不得据此断言 supports1m"
        );

        // 对照：原生认识的名字（Native 命中）应当照常断言
        let native = build_desktop_model_entries(
            &["big-1m".to_string()],
            std::slice::from_ref(&key),
        );
        assert!(native[0].supports1m, "Native 命中时应当正常断言");
    }

    /// `anthropicFamilyTier` 让桌面端的裸别名（`opus`/`sonnet`/…）钉到指定条目；
    /// 同档位多条时只有第一条带 `isFamilyDefault`（官方对多个 true 会告警并取首个）。
    #[test]
    fn desktop_family_tier_is_derived_and_default_is_unique_per_tier() {
        let entries = build_desktop_model_entries(
            &[
                "claude-opus-4-8".to_string(),
                "claude-opus-5".to_string(),
                "claude-sonnet-4-5".to_string(),
                "claude-haiku-4-5".to_string(),
                // 不含任何档位子串 → 无 tier，也就不该有 isFamilyDefault
                "claude-custom".to_string(),
            ],
            &[],
        );

        assert_eq!(entries[0].anthropic_family_tier, Some("opus"));
        assert!(entries[0].is_family_default, "opus 档第一条应为默认");
        assert_eq!(entries[1].anthropic_family_tier, Some("opus"));
        assert!(!entries[1].is_family_default, "同档位第二条不得再标默认");
        assert_eq!(entries[2].anthropic_family_tier, Some("sonnet"));
        assert!(entries[2].is_family_default, "sonnet 档首条独立计算");
        assert_eq!(entries[3].anthropic_family_tier, Some("haiku"));
        assert!(entries[3].is_family_default);
        assert_eq!(entries[4].anthropic_family_tier, None, "无档位子串 → 不填 tier");
        assert!(
            !entries[4].is_family_default,
            "无 tier 时不得设 isFamilyDefault（官方会告警并忽略）"
        );
    }

    /// 无 tier 的条目在 JSON 里必须**既无** `anthropicFamilyTier` **也无** `isFamilyDefault`。
    #[test]
    fn desktop_profile_omits_tier_keys_when_not_derivable() {
        let entries = build_desktop_model_entries(&["claude-custom".to_string()], &[]);
        let p = build_gateway_profile(Value::Null, "http://127.0.0.1:1", &entries);
        let m = &p["inferenceModels"][0];
        assert_eq!(m["name"], "claude-custom");
        assert!(m["anthropicFamilyTier"].is_null());
        assert!(m["isFamilyDefault"].is_null());
        assert!(m["supports1m"].is_null(), "无窗口数据不写 supports1m");
    }

    /// cc-switch 接管检测：档还在但 `appliedId` 已被换掉 → 必须能被识别并给出可操作说明。
    ///
    /// 这是「接入了但不生效」这类无头案的直接线索来源：cc-switch 一被点开就整份重写
    /// `_meta.json`，SynaRoute 的档文件还在、UI 也仍显示已接入，但桌面端加载的是别人那一档。
    #[test]
    fn desktop_takeover_detected_when_applied_id_points_elsewhere() {
        let (dir, normal, threep, profile, meta) = desktop_layout("takeover");
        let ccswitch_id = "00000000-0000-4000-8000-000000157210";

        // 未接入（无 profile）→ 不该误报被接管。
        assert!(
            detect_desktop_takeover_at(&profile, &meta).is_none(),
            "从未接入时不得报「被接管」"
        );

        // 正常接入 → appliedId 是本档 → 无警告。
        apply_desktop_at(&normal, &threep, &profile, &meta, "http://127.0.0.1:1", &desktop_test_models(), &[]).unwrap();
        assert!(
            detect_desktop_takeover_at(&profile, &meta).is_none(),
            "appliedId 仍指向本档时不得报警"
        );

        // 模拟 cc-switch 被打开：整份重写 _meta.json，把 appliedId 与 entries 换成它自己的。
        std::fs::write(
            &meta,
            format!(
                r#"{{"appliedId":"{ccswitch_id}","entries":[{{"id":"{ccswitch_id}","name":"CC Switch"}}]}}"#
            ),
        )
        .unwrap();
        let warn = detect_desktop_takeover_at(&profile, &meta).expect("被接管必须能检测到");
        assert!(warn.contains(ccswitch_id), "要点出当前生效的是哪一档: {warn}");
        assert!(
            warn.contains("写入工具配置"),
            "要给出可操作的恢复方式: {warn}"
        );

        // appliedId 整个被删掉（其他工具重写成不含该键）→ 同样算接管。
        std::fs::write(&meta, r#"{"entries":[]}"#).unwrap();
        let warn2 = detect_desktop_takeover_at(&profile, &meta).expect("缺 appliedId 也应报警");
        assert!(warn2.contains("没有 appliedId"), "{warn2}");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// P2#6 复核护栏：三端预览对「单个文件读不出」都必须降级为占位，**不得整份返回 Err**。
    ///
    /// 结论：`read_preview_text` 是三端唯一的读取入口，且它对读失败已返回
    /// `Ok((true, Some("/* 无法读取… */")))` 而非 Err —— 所以 codex / CLI 两条路径其实早已与
    /// 桌面端同等容错（docs/14 里那条 P2 是过时的）。本测试把该行为**锁住**，防止日后有人
    /// 图省事把 `?` 加回去：一旦整份 Err，前端会丢掉全部路径、聚合页永久卡「加载中」。
    #[test]
    fn preview_degrades_unreadable_file_instead_of_failing_whole_snapshot() {
        // 用目录冒充文件：read_to_string 必然失败（Windows/Unix 均如此），且不依赖权限。
        let fake = temp_file("preview_unreadable", "settings.json");
        std::fs::create_dir_all(&fake).unwrap();
        let dir = fake.parent().unwrap().to_path_buf();

        let (exists, content) = read_preview_text(&fake, true).expect("读失败不得上抛");
        assert!(exists, "文件（这里是目录）确实存在，应报 exists=true");
        let text = content.expect("应给出占位文本而非 None");
        assert!(
            text.contains("无法读取"),
            "占位要说明原因，否则用户以为文件是空的: {text}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 未接入 / 已正常接入时都不得报「被接管」——只看 appliedId 会把「从未接入」误报。
    #[test]
    fn takeover_not_reported_without_our_profile() {
        let (dir, _normal, _threep, profile, meta) = desktop_layout("takeover_none");
        std::fs::create_dir_all(meta.parent().unwrap()).unwrap();
        // 场景：别人的档已生效，但我们从未接入（无 profile 文件）→ 不是接管，是「没接入」。
        std::fs::write(&meta, r#"{"appliedId":"00000000-0000-4000-8000-000000157210"}"#).unwrap();
        assert!(
            detect_desktop_takeover_at(&profile, &meta).is_none(),
            "从未接入时不得报「被接管」，否则每个新用户一打开预览就看到假警告"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn desktop_apply_writes_3p_mode_and_gateway_profile() {
        let (dir, normal, threep, profile, meta) = desktop_layout("desktop_apply");
        // 预置 3p config 已有 preferences，验证合并写不丢它。
        std::fs::write(
            &threep,
            r#"{"preferences":{"remoteToolsDeviceName":"win-x"}}"#,
        )
        .unwrap();

        let models = vec!["claude-opus-4-8".to_string(), "claude-opus-5".to_string()];
        let msg = apply_desktop_at(
            &normal,
            &threep,
            &profile,
            &meta,
            "http://127.0.0.1:47102",
            &models,
            &[],
        )
        .unwrap();
        assert!(msg.contains("3p 部署模式"));

        // 两个 config 都切 3p，preferences 保留。
        let n: Value = serde_json::from_slice(&std::fs::read(&normal).unwrap()).unwrap();
        assert_eq!(n["deploymentMode"], "3p");
        let t: Value = serde_json::from_slice(&std::fs::read(&threep).unwrap()).unwrap();
        assert_eq!(t["deploymentMode"], "3p");
        assert_eq!(
            t["preferences"]["remoteToolsDeviceName"], "win-x",
            "既有 preferences 必须保留"
        );

        // gateway 档字段齐全。
        let p: Value = serde_json::from_slice(&std::fs::read(&profile).unwrap()).unwrap();
        assert_eq!(p["inferenceProvider"], "gateway");
        assert_eq!(p["inferenceGatewayBaseUrl"], "http://127.0.0.1:47102");
        assert_eq!(p["inferenceGatewayAuthScheme"], "bearer");
        assert_eq!(p["inferenceGatewayApiKey"], DESKTOP_GATEWAY_PLACEHOLDER);
        assert_eq!(p["disableDeploymentModeChooser"], true);
        assert_eq!(p["coworkEgressAllowedHosts"][0], "*");
        // inferenceModels：数组、名字对。
        assert_eq!(p["inferenceModels"][0]["name"], "claude-opus-4-8");
        assert_eq!(p["inferenceModels"][1]["name"], "claude-opus-5");
        // supports1m 是**能力断言**，无 contextWindow 依据时（本例传空 keys）不得凭空写。
        assert!(
            p["inferenceModels"][0]["supports1m"].is_null(),
            "无窗口数据时不该断言 supports1m：上游不支持会让桌面端给出必然失败的选项"
        );
        // anthropicFamilyTier 按名字含哪个档位子串推导；同档位只有第一条当默认。
        assert_eq!(p["inferenceModels"][0]["anthropicFamilyTier"], "opus");
        assert_eq!(p["inferenceModels"][0]["isFamilyDefault"], true);
        assert_eq!(p["inferenceModels"][1]["anthropicFamilyTier"], "opus");
        assert!(
            p["inferenceModels"][1]["isFamilyDefault"].is_null(),
            "同档位第二条不得也标默认（官方对多个 isFamilyDefault 会告警并取首个）"
        );

        // _meta：本档登记 + appliedId 指向本档。
        let m: Value = serde_json::from_slice(&std::fs::read(&meta).unwrap()).unwrap();
        assert_eq!(m["appliedId"], DESKTOP_PROFILE_ID);
        assert_eq!(m["entries"][0]["id"], DESKTOP_PROFILE_ID);
        assert_eq!(m["entries"][0]["name"], DESKTOP_PROFILE_NAME);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 接入时对外名不合规 → **照写但强警告**（用户拍板：对齐 cc-switch，不阻断）。
    ///
    /// 保存 Key 那道拦截是主防线（见 lib.rs），这里是兜底：档内历史清单（改端口时
    /// `existing_inference_model_names` 回退用）与更早版本存下的 Key 都可能绕过它。
    #[test]
    fn desktop_apply_warns_when_all_model_names_unusable() {
        let (dir, normal, threep, profile, meta) = desktop_layout("desktop_warn_all");
        let models = vec!["glm-4.6".to_string(), "grok-4.5".to_string()];
        let msg =
            apply_desktop_at(&normal, &threep, &profile, &meta, "http://127.0.0.1:1", &models, &[])
                .unwrap();

        assert!(msg.contains("3p 部署模式"), "接入本身仍应成功: {msg}");
        assert!(msg.contains("glm-4.6") && msg.contains("grok-4.5"), "要列出具体名字: {msg}");
        assert!(
            msg.contains("ModelsNotDiscoveredError"),
            "全不合规必须说清「写进去了但用不了」的后果: {msg}"
        );
        assert!(msg.contains("模型映射"), "要给可照做的修法: {msg}");

        // 关键：不合规名必须**原样写进**档里，不能被过滤。
        // 若过滤掉，effective 会变空并撞上「空列表 → 报错」分支，把用户选的「照写」变成阻断。
        let p: Value = serde_json::from_slice(&std::fs::read(&profile).unwrap()).unwrap();
        let names: Vec<&str> = p["inferenceModels"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["glm-4.6", "grok-4.5"], "照写不过滤（警告 ≠ 阻断）");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 部分不合规：措辞降级为「其余仍可用」，且合规名照旧生效。
    #[test]
    fn desktop_apply_warns_partially_and_keeps_valid_ones() {
        let (dir, normal, threep, profile, meta) = desktop_layout("desktop_warn_part");
        let models = vec!["claude-opus-4-8".to_string(), "glm-4.6".to_string()];
        let msg =
            apply_desktop_at(&normal, &threep, &profile, &meta, "http://127.0.0.1:1", &models, &[])
                .unwrap();

        assert!(msg.contains("glm-4.6"), "要点出不合规的那个: {msg}");
        assert!(msg.contains("其余 1 个仍可用"), "部分不合规应说明其余可用: {msg}");
        assert!(
            !msg.contains("ModelsNotDiscoveredError"),
            "还有合规名时选择器不会空，不该拿死局吓用户: {msg}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 全合规 → 消息里不得出现任何警告噪音。
    #[test]
    fn desktop_apply_has_no_warning_when_all_names_compliant() {
        let (dir, normal, threep, profile, meta) = desktop_layout("desktop_warn_none");
        let models = vec!["claude-opus-4-8".to_string(), "opus".to_string()];
        let msg =
            apply_desktop_at(&normal, &threep, &profile, &meta, "http://127.0.0.1:1", &models, &[])
                .unwrap();
        assert!(!msg.contains('⚠'), "全合规不该有警告: {msg}");
        assert!(!msg.contains("不被桌面端接受"), "全合规不该有警告: {msg}");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn desktop_apply_rejects_empty_model_list() {
        // 空模型列表且档内也无既有模型 → 必须报错而非静默接入：省略 inferenceModels 会让桌面端
        // 进了 3p 却无模型可选，运行时发现一失败就 models_not_discovered、连会话都开不起来
        // （与「卡 get-started」同级的死局）。
        let (dir, normal, threep, profile, meta) = desktop_layout("desktop_nomodels");
        let err = apply_desktop_at(&normal, &threep, &profile, &meta, "http://127.0.0.1:1", &[], &[])
            .unwrap_err();
        assert!(
            err.to_string().contains("至少一个可服务模型"),
            "应给出可操作的报错: {err}"
        );
        // 关键：报错时不得留下半配置（未切 3p、未建档）。
        assert!(!profile.exists(), "报错不应写出 gateway 档");
        assert!(!normal.exists(), "报错不应改动部署配置");
        assert!(!meta.exists(), "报错不应写 _meta");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn desktop_apply_empty_models_falls_back_to_existing_profile() {
        // 改端口场景（审查 F1 附带项）：入参 models 为空，但档内已有 inferenceModels 时，
        // 应回退用档内清单、只更新端点，而不是报错让「仅改端点」失败或擦掉已有模型。
        let (dir, normal, threep, profile, meta) = desktop_layout("desktop_empty_fallback");
        // 先以非空模型接入（写入 gateway 档 + inferenceModels=[A]）。
        apply_desktop_at(
            &normal,
            &threep,
            &profile,
            &meta,
            "http://127.0.0.1:1",
            &["claude-opus-4-8".to_string()],
            &[],
        )
        .unwrap();

        // 再以空模型「改端口」重写：应成功、端点更新、模型清单保留。
        apply_desktop_at(&normal, &threep, &profile, &meta, "http://127.0.0.1:2", &[], &[]).unwrap();

        let p: Value = serde_json::from_slice(&std::fs::read(&profile).unwrap()).unwrap();
        assert_eq!(
            p["inferenceGatewayBaseUrl"], "http://127.0.0.1:2",
            "端点应更新为新端口"
        );
        let names: Vec<&str> = p["inferenceModels"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["claude-opus-4-8"], "空入参应回退档内既有模型，不擦掉");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn desktop_apply_merge_preserves_unknown_profile_keys() {
        // 合并写：用户/桌面端在本档里加过的键，重新接入（改端口）后不得被抹掉。
        let (dir, normal, threep, profile, meta) = desktop_layout("desktop_profile_merge");
        std::fs::create_dir_all(profile.parent().unwrap()).unwrap();
        std::fs::write(
            &profile,
            r#"{"userAddedKey":"keep-me","inferenceModels":[{"name":"stale","supports1m":true}]}"#,
        )
        .unwrap();

        apply_desktop_at(
            &normal,
            &threep,
            &profile,
            &meta,
            "http://127.0.0.1:2",
            &desktop_test_models(),
            &[],
        )
        .unwrap();

        let p: Value = serde_json::from_slice(&std::fs::read(&profile).unwrap()).unwrap();
        assert_eq!(p["userAddedKey"], "keep-me", "档内未知键应保留");
        assert_eq!(
            p["inferenceGatewayBaseUrl"], "http://127.0.0.1:2",
            "端点应更新"
        );
        let names: Vec<&str> = p["inferenceModels"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["claude-opus-4-8"], "模型清单应被当前可服务集覆盖，不留 stale");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn desktop_apply_coexists_with_ccswitch_profile() {
        // _meta 已有 cc-switch 档 + appliedId 指向它：接入后两档共存，appliedId 改指本档，
        // cc-switch 的 entry 原样保留（不误删用户另一套接入）。
        let (dir, normal, threep, profile, meta) = desktop_layout("desktop_coexist");
        let ccswitch_id = "00000000-0000-4000-8000-000000157210";
        // _meta.json 位于 configLibrary/ 下；desktop_layout 未建该子目录，直接写会 NotFound。
        std::fs::create_dir_all(meta.parent().unwrap()).unwrap();
        std::fs::write(
            &meta,
            format!(
                r#"{{"appliedId":"{ccswitch_id}","entries":[{{"id":"{ccswitch_id}","name":"CC Switch"}}]}}"#
            ),
        )
        .unwrap();

        apply_desktop_at(&normal, &threep, &profile, &meta, "http://127.0.0.1:1", &desktop_test_models(), &[]).unwrap();

        let m: Value = serde_json::from_slice(&std::fs::read(&meta).unwrap()).unwrap();
        assert_eq!(m["appliedId"], DESKTOP_PROFILE_ID, "appliedId 应改指本档");
        let entries = m["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 2, "两档共存");
        assert!(
            entries.iter().any(|e| e["id"] == ccswitch_id),
            "cc-switch 档必须保留（不误删）"
        );
        assert!(entries.iter().any(|e| e["id"] == DESKTOP_PROFILE_ID));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn desktop_apply_idempotent_no_duplicate_entry() {
        // 重复接入：entries 里本档不重复出现（去重）。
        let (dir, normal, threep, profile, meta) = desktop_layout("desktop_idem");
        for _ in 0..3 {
            apply_desktop_at(&normal, &threep, &profile, &meta, "http://127.0.0.1:1", &desktop_test_models(), &[]).unwrap();
        }
        let m: Value = serde_json::from_slice(&std::fs::read(&meta).unwrap()).unwrap();
        let ours: Vec<_> = m["entries"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["id"] == DESKTOP_PROFILE_ID)
            .collect();
        assert_eq!(ours.len(), 1, "重复接入本档 entry 不得重复");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn desktop_restore_resets_1p_and_removes_our_profile() {
        // 接入后还原（**调真实 restore_desktop_at**，不在测试里重抄步骤）：
        // 唯一档场景 → deploymentMode 复位 1p、本档 profile 删除、_meta 清本档且删 appliedId。
        let (dir, normal, threep, profile, meta) = desktop_layout("desktop_restore");
        // 前置：两个 config 接入前**已存在**（用户已装过桌面端）。本用例测的是
        // 「原本存在 → 还原写回 1p」这一支；「原本不存在 → 还原删文件」另有专门用例
        // （desktop_restore_deletes_configs_it_created）。
        std::fs::write(&normal, b"{}").unwrap();
        std::fs::write(&threep, b"{}").unwrap();
        apply_desktop_at(&normal, &threep, &profile, &meta, "http://127.0.0.1:1", &desktop_test_models(), &[]).unwrap();
        assert!(profile.exists());
        assert_eq!(
            read_desktop_applied_id(&meta).as_deref(),
            Some(DESKTOP_PROFILE_ID)
        );

        let msg = restore_desktop_at(&normal, &threep, &profile, &meta, None).unwrap();
        assert!(msg.contains("deploymentMode→1p"), "还原信息应含复位: {msg}");

        let n: Value = serde_json::from_slice(&std::fs::read(&normal).unwrap()).unwrap();
        assert_eq!(n["deploymentMode"], "1p");
        let t: Value = serde_json::from_slice(&std::fs::read(&threep).unwrap()).unwrap();
        assert_eq!(t["deploymentMode"], "1p");
        let m: Value = serde_json::from_slice(&std::fs::read(&meta).unwrap()).unwrap();
        assert!(
            m.get("appliedId").is_none(),
            "唯一档被清后 appliedId 应删除"
        );
        assert!(
            m["entries"].as_array().unwrap().is_empty(),
            "本档 entry 应被摘掉"
        );
        assert!(!profile.exists(), "本档 profile 应删除");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 桌面端 config 原本**不存在**（用户从未装过/从未打开过桌面端）时，还原必须
    /// 把接入时凭空建出来的那两个 `claude_desktop_config.json` **整份删掉**，
    /// 而不是往里写 `{"deploymentMode":"1p"}` 冒充「用户的官方配置」。
    ///
    /// 与 CLI/Codex 的 created 标记分支同一条语义（见 restore_one 的文档）。
    #[test]
    fn desktop_restore_deletes_configs_it_created() {
        let (dir, normal, threep, profile, meta) = desktop_layout("desktop_restore_created");
        // 前提：两个 config 都不存在。
        assert!(!normal.exists(), "前提：normal config 接入前不存在");
        assert!(!threep.exists(), "前提：threep config 接入前不存在");

        apply_desktop_at(&normal, &threep, &profile, &meta, "http://127.0.0.1:1", &desktop_test_models(), &[]).unwrap();
        assert!(normal.exists(), "接入应凭空建出 normal config");
        assert!(threep.exists(), "接入应凭空建出 threep config");

        restore_desktop_at(&normal, &threep, &profile, &meta, None).unwrap();

        assert!(
            !normal.exists(),
            "接入时凭空新建的 config 还原后应整份删除，不该留下 deploymentMode=1p 的假配置"
        );
        assert!(!threep.exists(), "同上（3p 目录那份）");
        assert!(!profile.exists(), "本档 profile 应删除");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 还原中途失败时，`.synaroute-created` 标记也必须被回滚 —— 它丢了就**不可自愈**。
    ///
    /// 标记的语义是「这个文件是接入时凭空新建的，还原应整份删掉，而不是写个
    /// `{"deploymentMode":"1p"}` 冒充用户的官方配置」。它是判据本身，无从重建。
    ///
    /// 缺陷形态：回滚集只有 4 个文件（两个 config + profile + _meta），不含两个标记。
    /// `restore_created_or_write_mode` 先处理 normal —— 删掉 config **和它的标记** ——
    /// 随后处理 threep 时失败（真机上就是文件被正在运行的桌面端独占）。回滚把两个 config
    /// 都还原了，但 normal 的标记已经没了。下次再还原时那条分支看不到标记，
    /// 就往一个本是我们凭空新建的文件里写 1p，留下一个假的「用户官方配置」。
    ///
    /// 故障注入手法：让 normal 走「凭空新建」分支（接入前不存在 → 接入后有标记），
    /// threep 走「原本存在」分支（接入前已存在 → 无标记，还原时要写 deploymentMode）。
    /// 接入后把 threep 的内容改成**非法 JSON**，它那一步的 `read_json_or_empty` 必然失败 ——
    /// 而它排在 normal 之后，normal 的「删文件 + 删标记」已经跑完，正好是那个窗口。
    ///
    /// 为什么不用「把 threep 换成目录」：`with_rollback` 先对全部路径拍快照，
    /// 对目录 `fs::read` 会失败 → 还没进 op 就返回 Err，那个窗口压根不会打开
    /// （实测：那样注入时本测试恒绿，等于没测到）。
    /// 桌面端接入**不得**为 gateway 档与 `_meta.json` 留下 `.synaroute.bak`。
    ///
    /// 这条盯的是一个真机上确实发生了的泄漏：那两处原先走 `backup_and_write_json`，
    /// 而它是**首写即锁**语义、`.bak` 此后永不刷新；桌面端的还原走的是外科手术
    /// （`remove_file(profile)` + 编辑 `_meta`），**不删 `.bak`** ——
    /// 于是盘上永久留下两份陈旧快照。实测在用户机器上捞到：
    /// `…000053796e61.json.synaroute.bak` 停在 2026-08-01、
    /// `_meta.json.synaroute.bak` 停在 2026-07-30 且 `appliedId` 还指着早已被删的本档。
    ///
    /// 两重危害：① 排障时是**假现场**（我自己就把它读成过「还原失败了」）；
    /// ② `_meta` 那份写回去正是本文件点名的死局（appliedId 指向不存在的 profile
    /// → 桌面端拿不到 gateway 档而卡死）。今天没人读它，但留着就是埋雷。
    ///
    /// 这两个文件本就不需要整文件快照：gateway 档整个是我们造的（逆操作 = 删掉），
    /// `_meta` 是与 cc-switch 共用的、只做外科手术（逆操作 = 摘掉本档那一条）。
    #[test]
    fn desktop_apply_leaves_no_stale_backup_snapshots() {
        let (dir, normal, threep, profile, meta) = desktop_layout("desktop_no_bak");
        std::fs::write(&threep, b"{}").unwrap();
        // 预置一份 cc-switch 的既有 _meta：这一份必须在还原后仍然完好，
        // 而它恰恰是「陈旧 .bak 被写回」会毁掉的东西。
        // `_meta` 落在 configLibrary 子目录下，夹具里要先把它建出来。
        std::fs::create_dir_all(meta.parent().unwrap()).unwrap();
        std::fs::write(
            &meta,
            br#"{"appliedId":"cc-switch-id","entries":[{"id":"cc-switch-id","name":"CC Switch"}]}"#,
        )
        .unwrap();

        apply_desktop_at(
            &normal,
            &threep,
            &profile,
            &meta,
            "http://127.0.0.1:1",
            &desktop_test_models(),
            &[],
        )
        .unwrap();

        assert!(
            !backup_path_for(&profile).exists(),
            "gateway 档不该有 .bak —— 它整个是我们造的，逆操作是删掉它"
        );
        assert!(
            !backup_path_for(&meta).exists(),
            "_meta.json 不该有 .bak —— 它与 cc-switch 共用，只做外科手术；\
             留一份首写即锁的快照会在日后被写回时把 appliedId 指向已删的 profile"
        );

        // 重复接入同样不该凭空长出 .bak（首写即锁那条语义的反向验证）
        apply_desktop_at(
            &normal,
            &threep,
            &profile,
            &meta,
            "http://127.0.0.1:2",
            &desktop_test_models(),
            &[],
        )
        .unwrap();
        assert!(!backup_path_for(&profile).exists());
        assert!(!backup_path_for(&meta).exists());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn desktop_restore_rolls_back_created_markers_too() {
        let (dir, normal, threep, profile, meta) = desktop_layout("desktop_restore_marker");
        // 前置：normal 不存在（→ 会有 created 标记）、threep 已存在（→ 不会有标记）
        assert!(!normal.exists(), "前置条件：normal 接入前不存在");
        std::fs::write(&threep, b"{}").unwrap();
        apply_desktop_at(
            &normal,
            &threep,
            &profile,
            &meta,
            "http://127.0.0.1:1",
            &desktop_test_models(),
            &[],
        )
        .unwrap();
        let normal_marker = created_marker_path_for(&normal);
        assert!(normal_marker.exists(), "前置条件：normal 应有 created 标记");
        assert!(
            !created_marker_path_for(&threep).exists(),
            "前置条件：threep 接入前已存在，不该有 created 标记"
        );

        // 注入：threep 变成非法 JSON → 它那一步（write_deployment_mode）必然失败
        std::fs::write(&threep, b"{ not json").unwrap();

        let r = restore_desktop_at(&normal, &threep, &profile, &meta, None);
        assert!(r.is_err(), "中途失败必须整体返回错误");

        assert!(
            normal_marker.exists(),
            "created 标记必须随文件一同回滚。丢了它下次还原就会往一个本是我们凭空新建的 \
             config 里写 deploymentMode:1p，留下一个冒充「用户官方配置」的文件，而标记无从重建"
        );
        assert!(normal.exists(), "被删的 config 本身也该回滚回来");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn desktop_restore_is_atomic_on_midway_failure() {
        // P1 回归：桌面端还原要动 4 个文件（两个 config 的 deploymentMode + 本档 profile + _meta），
        // 中途失败若不整体回滚就留「半还原」态——最典型的是 profile 已删、_meta 却仍把 appliedId
        // 指着它，桌面端启动时拿不到那份 gateway 档而卡死（用户视角：桌面端起不来，也无从修复）。
        //
        // 故障注入手法：让**最后一步**（写 deploymentMode）失败——把 normal config 写成非法 JSON，
        // read_json_or_empty 会返回解析错误。此时前两步（删 profile、清 _meta）已经生效，
        // 正是「半还原」的窗口。回滚后这两步都必须被撤销。
        let (dir, normal, threep, profile, meta) = desktop_layout("desktop_restore_atomic");
        // 前置：两个 config 在接入前**已存在**（用户已装过桌面端）。
        // 这一步决定接入走 `.bak` 分支而非「凭空新建」标记分支 —— 后者还原时直接删文件、
        // 不再解析 JSON，本用例赖以制造失败的注入点（写非法 JSON）就失去着力点。
        std::fs::write(&normal, b"{}").unwrap();
        std::fs::write(&threep, b"{}").unwrap();
        apply_desktop_at(&normal, &threep, &profile, &meta, "http://127.0.0.1:1", &desktop_test_models(), &[]).unwrap();
        assert!(profile.exists(), "前置条件：接入后 profile 存在");
        assert_eq!(
            read_desktop_applied_id(&meta).as_deref(),
            Some(DESKTOP_PROFILE_ID),
            "前置条件：appliedId 指向本档"
        );
        let profile_before = std::fs::read(&profile).unwrap();
        let meta_before = std::fs::read(&meta).unwrap();

        // 注入：normal config 变成非法 JSON → 还原的最后一步必然失败。
        std::fs::write(&normal, b"{ this is not json").unwrap();

        let err = restore_desktop_at(&normal, &threep, &profile, &meta, None);
        assert!(err.is_err(), "最后一步失败时整体还原必须返回错误");

        // 回滚断言：前两步的改动都必须被撤销，桌面端仍处于可用的「已接入」态。
        assert!(
            profile.exists(),
            "还原中途失败必须回滚，profile 不得留在已删除状态（否则 appliedId 指向空档，桌面端起不来）"
        );
        assert_eq!(
            std::fs::read(&profile).unwrap(),
            profile_before,
            "回滚后 profile 内容应与失败前一致"
        );
        assert_eq!(
            std::fs::read(&meta).unwrap(),
            meta_before,
            "回滚后 _meta 应恢复（appliedId 仍指向本档、entry 未被摘掉）"
        );
        assert_eq!(
            read_desktop_applied_id(&meta).as_deref(),
            Some(DESKTOP_PROFILE_ID),
            "回滚后 appliedId 仍应指向本档"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn desktop_restore_keeps_3p_when_ccswitch_profile_remains() {
        // 回归护栏（曾把用户踢回 get-started 的 bug）：cc-switch 档共存且 appliedId 是本档时，
        // 还原必须**保持 3p**（因为 appliedId 会交还给 cc-switch 那个 3p 网关档），
        // 绝不能复位 1p —— 否则用户「接入 SynaRoute → 停止代理」一个来回后被踢回官方登录页。
        let (dir, normal, threep, profile, meta) = desktop_layout("desktop_restore_coexist");
        let ccswitch_id = "00000000-0000-4000-8000-000000157210";
        std::fs::create_dir_all(meta.parent().unwrap()).unwrap();
        std::fs::write(
            &meta,
            format!(
                r#"{{"appliedId":"{ccswitch_id}","entries":[{{"id":"{ccswitch_id}","name":"CC Switch"}}]}}"#
            ),
        )
        .unwrap();
        // cc-switch 的 profile 文件也真实落盘，验证还原不误删它。
        let ccswitch_profile = meta.parent().unwrap().join(format!("{ccswitch_id}.json"));
        std::fs::write(&ccswitch_profile, r#"{"inferenceProvider":"gateway"}"#).unwrap();

        apply_desktop_at(&normal, &threep, &profile, &meta, "http://127.0.0.1:1", &desktop_test_models(), &[]).unwrap();
        let n: Value = serde_json::from_slice(&std::fs::read(&normal).unwrap()).unwrap();
        assert_eq!(n["deploymentMode"], "3p", "接入后应为 3p");

        let msg = restore_desktop_at(&normal, &threep, &profile, &meta, Some(ccswitch_id)).unwrap();
        assert!(
            !msg.contains("deploymentMode→1p"),
            "有其它档接手时不得复位 1p: {msg}"
        );

        let n: Value = serde_json::from_slice(&std::fs::read(&normal).unwrap()).unwrap();
        assert_eq!(n["deploymentMode"], "3p", "必须保持 3p，不能踢回官方登录");
        let t: Value = serde_json::from_slice(&std::fs::read(&threep).unwrap()).unwrap();
        assert_eq!(t["deploymentMode"], "3p");
        let m: Value = serde_json::from_slice(&std::fs::read(&meta).unwrap()).unwrap();
        assert_eq!(m["appliedId"], ccswitch_id, "appliedId 应交还 cc-switch 档");
        assert!(
            ccswitch_profile.exists(),
            "cc-switch 的 profile 文件不得被删"
        );
        assert!(!profile.exists(), "本档 profile 应删除");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn desktop_restore_prefers_recorded_previous_applied_id() {
        // appliedId 交还必须精确指向「接入前那一档」，而非 entries 首个（否则静默切供应商）。
        let (dir, normal, threep, profile, meta) = desktop_layout("desktop_restore_prefer");
        let first_id = "00000000-0000-4000-8000-0000000000aa";
        let prev_id = "00000000-0000-4000-8000-0000000000bb";
        std::fs::create_dir_all(meta.parent().unwrap()).unwrap();
        std::fs::write(
            &meta,
            format!(
                r#"{{"appliedId":"{prev_id}","entries":[{{"id":"{first_id}","name":"A"}},{{"id":"{prev_id}","name":"B"}}]}}"#
            ),
        )
        .unwrap();

        apply_desktop_at(&normal, &threep, &profile, &meta, "http://127.0.0.1:1", &desktop_test_models(), &[]).unwrap();
        restore_desktop_at(&normal, &threep, &profile, &meta, Some(prev_id)).unwrap();

        let m: Value = serde_json::from_slice(&std::fs::read(&meta).unwrap()).unwrap();
        assert_eq!(
            m["appliedId"], prev_id,
            "应交还接入前那一档（{prev_id}），而非 entries 首个（{first_id}）"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn desktop_restore_noop_when_not_applied() {
        // 未接入时还原不报错、不改盘（还原由「停止代理」自动触发，不能弹误报）。
        let (dir, normal, threep, profile, meta) = desktop_layout("desktop_restore_noop");
        let msg = restore_desktop_at(&normal, &threep, &profile, &meta, None).unwrap();
        assert!(msg.contains("未接入"), "未接入应返回无需还原: {msg}");
        assert!(!normal.exists(), "未接入不应凭空创建 config");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn desktop_meta_clear_repoints_applied_to_remaining() {
        // 两档共存、appliedId 指向本档：清本档后 appliedId 应改指剩余的 cc-switch 档，不删键。
        let meta = temp_file("desktop_meta_repoint", "_meta.json");
        let ccswitch_id = "00000000-0000-4000-8000-000000157210";
        std::fs::write(
            &meta,
            format!(
                r#"{{"appliedId":"{DESKTOP_PROFILE_ID}","entries":[{{"id":"{ccswitch_id}","name":"CC Switch"}},{{"id":"{DESKTOP_PROFILE_ID}","name":"SynaRoute"}}]}}"#
            ),
        )
        .unwrap();

        assert!(write_desktop_meta_clear(&meta).unwrap());
        let m: Value = serde_json::from_slice(&std::fs::read(&meta).unwrap()).unwrap();
        assert_eq!(
            m["appliedId"], ccswitch_id,
            "appliedId 应改指剩余的 cc-switch 档"
        );
        let entries = m["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["id"], ccswitch_id, "cc-switch 档保留");

        std::fs::remove_dir_all(meta.parent().unwrap()).ok();
    }

    #[test]
    fn desktop_meta_clear_noop_when_not_ours() {
        // appliedId 指向别的档、entries 无本档：清理应为无操作（不动用户的 cc-switch 接入）。
        let meta = temp_file("desktop_meta_noop", "_meta.json");
        let ccswitch_id = "00000000-0000-4000-8000-000000157210";
        let original = format!(
            r#"{{"appliedId":"{ccswitch_id}","entries":[{{"id":"{ccswitch_id}","name":"CC Switch"}}]}}"#
        );
        std::fs::write(&meta, &original).unwrap();

        let changed = write_desktop_meta_clear(&meta).unwrap();
        assert!(!changed, "无本档时应无操作");
        let m: Value = serde_json::from_slice(&std::fs::read(&meta).unwrap()).unwrap();
        assert_eq!(m["appliedId"], ccswitch_id, "别人的 appliedId 不得改动");

        std::fs::remove_dir_all(meta.parent().unwrap()).ok();
    }

    #[test]
    fn desktop_gateway_api_key_is_redacted_in_preview() {
        // 预览脱敏必须覆盖 inferenceGatewayApiKey（即便占位，也不应把该字段值明文回传）。
        let raw = r#"{"inferenceGatewayApiKey":"synaroute-proxy","inferenceGatewayBaseUrl":"http://127.0.0.1:1"}"#;
        let out = redact_config_secrets(raw);
        assert!(
            out.contains(r#""inferenceGatewayApiKey":"***""#)
                || out.contains(r#""inferenceGatewayApiKey": "***""#),
            "inferenceGatewayApiKey 应脱敏: {out}"
        );
        assert!(
            out.contains("http://127.0.0.1:1"),
            "非密钥字段应保留"
        );
    }

    #[test]
    fn cleanup_legacy_strips_baseurl_but_keeps_user_config() {
        // 核心不变量（曾会抹掉用户配置的 bug）：旧实现是读-改-写，legacy config 常是
        // 「用户配置 + baseUrl」混合体。清理必须**只摘 baseUrl 键**，保留 mcpServers /
        // preferences 等用户内容，绝不整文件删除。
        let legacy = temp_file("legacy_mixed", DESKTOP_CONFIG_FILE);
        std::fs::write(
            &legacy,
            r#"{"baseUrl":"http://127.0.0.1:47102","mcpServers":{"mine":{"command":"x"}},"preferences":{"a":1}}"#,
        )
        .unwrap();

        cleanup_legacy_desktop_residue_at(&legacy);

        assert!(legacy.exists(), "混合体文件不得被删除");
        let v: Value = serde_json::from_slice(&std::fs::read(&legacy).unwrap()).unwrap();
        assert!(v.get("baseUrl").is_none(), "baseUrl 键应被摘掉");
        assert_eq!(
            v["mcpServers"]["mine"]["command"], "x",
            "用户手配的 MCP 服务器必须原样保留"
        );
        assert_eq!(v["preferences"]["a"], 1, "用户偏好必须原样保留");

        std::fs::remove_dir_all(legacy.parent().unwrap()).ok();
    }

    #[test]
    fn cleanup_legacy_deletes_pure_residue_file() {
        // 纯残留（摘完 baseUrl 就是空对象）才删文件，不留空壳。
        let legacy = temp_file("legacy_pure", DESKTOP_CONFIG_FILE);
        std::fs::write(&legacy, r#"{"baseUrl":"http://127.0.0.1:47102"}"#).unwrap();
        cleanup_legacy_desktop_residue_at(&legacy);
        assert!(!legacy.exists(), "纯残留应删除");
        std::fs::remove_dir_all(legacy.parent().unwrap()).ok();
    }

    #[test]
    fn cleanup_legacy_leaves_legit_config_untouched() {
        // 不含 baseUrl 的合法桌面端配置：一字不动。
        let legit = temp_file("legit_prefs", DESKTOP_CONFIG_FILE);
        let original = r#"{"preferences":{"coworkWebSearchEnabled":true}}"#;
        std::fs::write(&legit, original).unwrap();
        cleanup_legacy_desktop_residue_at(&legit);
        assert!(legit.exists(), "合法文件不得删除");
        assert_eq!(
            std::fs::read_to_string(&legit).unwrap(),
            original,
            "合法文件内容不得改动"
        );
        std::fs::remove_dir_all(legit.parent().unwrap()).ok();
    }

    #[test]
    fn cleanup_legacy_keeps_bak_that_holds_user_config() {
        // .bak 可能是接入前用户原始 config 的唯一副本：只有「单键 baseUrl」才删，
        // 含用户内容的一律保留（否则删掉就再无第二处可恢复）。
        let legacy = temp_file("legacy_bak_guard", DESKTOP_CONFIG_FILE);
        std::fs::write(&legacy, r#"{"baseUrl":"http://x"}"#).unwrap();
        let bak = backup_path_for(&legacy);
        std::fs::write(&bak, r#"{"preferences":{"keep":true},"baseUrl":"http://x"}"#).unwrap();

        cleanup_legacy_desktop_residue_at(&legacy);

        assert!(bak.exists(), "含用户配置的 .bak 不得删除");
        let v: Value = serde_json::from_slice(&std::fs::read(&bak).unwrap()).unwrap();
        assert_eq!(v["preferences"]["keep"], true, ".bak 内容不得改动");

        std::fs::remove_dir_all(legacy.parent().unwrap()).ok();
    }

    #[test]
    fn cleanup_legacy_deletes_pure_baseurl_bak() {
        // 单键 baseUrl 的 .bak 确为旧实现产物，删。
        let legacy = temp_file("legacy_bak_pure", DESKTOP_CONFIG_FILE);
        std::fs::write(&legacy, r#"{"preferences":{}}"#).unwrap();
        let bak = backup_path_for(&legacy);
        std::fs::write(&bak, r#"{"baseUrl":"http://127.0.0.1:47102"}"#).unwrap();

        cleanup_legacy_desktop_residue_at(&legacy);

        assert!(!bak.exists(), "单键 baseUrl 的 .bak 应删除");
        std::fs::remove_dir_all(legacy.parent().unwrap()).ok();
    }

    #[test]
    fn apply_desktop_at_does_not_trigger_real_cleanup() {
        // 护栏（真能证伪版）：可测入口 apply_desktop_at 绝不能触发触碰真实 %APPDATA% 的
        // cleanup_legacy_desktop_residue()。用调用计数器观测——若有人把无参 cleanup 误挪回
        // apply_desktop_at，计数会 +1，本测试变红。
        // 旧版此测试断言的是 temp legacy 文件仍在，但那文件与真实 cleanup 操作的 config_dir()
        // 路径永不相交、恒真，无法捕获回归（审查 F2）。
        //
        // 计数器是进程全局，与其它并发测试隔离靠 cleanup_test_lock()（backing: static LOCK）串行化。
        let _guard = cleanup_test_lock();
        let before = REAL_CLEANUP_CALLS.load(std::sync::atomic::Ordering::SeqCst);

        let (dir, normal, threep, profile, meta) = desktop_layout("hermetic_apply");
        apply_desktop_at(
            &normal,
            &threep,
            &profile,
            &meta,
            "http://127.0.0.1:1",
            &desktop_test_models(),
            &[],
        )
        .unwrap();

        let after = REAL_CLEANUP_CALLS.load(std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            after, before,
            "apply_desktop_at 触发了真实目录清理（+{}）——它会删用户真实 %APPDATA%，必须只挂在 apply_claude_desktop 上",
            after - before
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    // 注：不为「apply_claude_desktop 确实调用 cleanup」写正向测试——那需要往真实
    // %LOCALAPPDATA%\Claude* 写盘，恰是本轮在修的「测试污染真实 AppData」反面。cleanup 挂在
    // apply_claude_desktop 的调用是一行显式代码（审查可见），加上上面的计数器护栏保证它不在
    // 可测核心 apply_desktop_at 内，两者已足够锁定挂载位置。

    /// 串行化会读写进程全局 REAL_CLEANUP_CALLS 的测试，避免与未来可能触发 cleanup 的并发测试
    /// 互扰导致计数误判。
    fn cleanup_test_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }
}
