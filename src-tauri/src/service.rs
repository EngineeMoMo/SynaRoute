//! **业务编排层**：把 lib.rs 里那些「不只是转发一下」的逻辑集中到这里。
//!
//! ## 它解决的问题
//!
//! lib.rs 原本兼任两件事：Tauri 的 IPC 边界（解包参数、注册命令）与业务编排
//! （校验、多步写入、失败回滚、事件记录）。后者混在命令函数里就**不可单测** ——
//! 因为签名上挂着 tauri::State，测试里造不出来。而这里面恰好有几处最该被测的：
//! 桌面端模型名校验（配错会让模型选择器直接空掉）、自启动的落盘失败回滚、诊断报告脱敏。
//!
//! ## 边界约束（本模块的核心规矩）
//!
//! 函数签名只收 &Store / &Arc<Store> / &ProxyManager / &McpManager，
//! **绝不收** tauri 的 State / AppHandle / Window，也绝不 use crate::AppState。
//!
//! 凡需要 AppHandle 的副作用（重建托盘、注册自启动项、弹文件对话框、updater、
//! 取包版本号、async_runtime::spawn）一律留在 lib.rs：本层只返回「要不要做」的
//! 判定值（bool / Option / 已拼好的字符串），由 lib.rs 去做那件事。
//!
//! ## 什么不搬（防下一轮有人「顺手补全」）
//!
//! lib.rs 里有 38 条命令是**薄转发**（一行调 store/tools/aggregate/portable，无编排）：
//! list_keys、delete_key、get_settings、start_proxy、list_events… 这些搬进来只会多一层
//! 无收益的转发，**明确不搬**。
//!
//! crate::tools::apply / register_mcp_client / restore 这条链**仍然不可单测**，也别去 mock
//! 文件系统：它们会真写用户的 ~/.claude.json / config.toml / 桌面端配置 ——
//! 单测里跑会破坏开发机上真实的客户端配置，而且在 MSIX 包身份下写的还是虚拟副本
//! （见 CLAUDE.md 的平行宇宙陷阱）。本次重构的收益体现在**抽出可测的那一层**
//! （模型名校验、超时计算、报告拼装），编排本身仍靠真机验证。

use crate::error::{AppError, AppResult};
use crate::model::*;
use crate::store::Store;

// ==== Key 与密钥 ====

/// 保存（新增/更新）一条 Key。
///
/// 唯一的编排是**先校验再落盘**：桌面端不合规的对外模型名必须在源头拦下，
/// 见 [`reject_desktop_key_with_unusable_model_names`]。
pub(crate) fn save_key(store: &Store, key: ProviderKey) -> AppResult<ProviderKey> {
    reject_desktop_key_with_unusable_model_names(&key)?;
    store.upsert_key(key)
}

/// 保存密钥：先加密落库，成功后再置 `has_secret=true` 落盘 config。
///
/// **顺序不可交换**。反过来（先标记后写库）若写库失败，会留下「config 记有密钥、
/// 库里实际没有」的不一致：UI 依据 `has_secret` 显示已配置，但 `reveal_secret` /
/// 模型探测 / 代理转发都取不到密钥，报「未配置密钥」，用户难察觉自己配过了。
/// 先写库则失败时直接返回 `Err`，标记不会被写脏。
pub(crate) fn save_secret(store: &Store, key_id: &str, secret: &str) -> AppResult<()> {
    store.secrets.write().set(key_id, secret)?;
    if let Some(mut k) = store.get_key(key_id) {
        k.has_secret = true;
        store.upsert_key(k)?;
    }
    Ok(())
}

/// 启用/停用某条 Key。返回**是否需要补一次健康探测**。
///
/// 定时健康检查只扫**启用**的 Key，故一条 Key 在停用期间的 `status` 会一直冻结在
/// 它上次被探测时的结论上。真机实测过这个坑：一条禁用 Key 的卡片显示
/// 「探测不可达 · 10 天前」，而那家上游早就恢复了、真实转发也能成功 ——
/// 用户以为「现在就是坏的」，实际只是没人去刷新过那个陈旧快照。
/// 「刚把它启用」正是最需要知道它当下可用性的时刻，故此时返回 true。
///
/// **停用时不探测**（返回 false）：停用路径本身会把 health 清成 Unknown
/// （见 `Store::toggle_key`），再花最长 8s 去探一条马上不再被路由的 Key 纯属浪费。
/// 调用方拿到 true 后应**异步**跑探测、不要阻塞返回——它最长可达 `fast_timeout`(8s)，
/// 同步等待会让用户点一下开关就看到界面明显卡顿。
pub(crate) fn toggle_key(store: &Store, key_id: &str, enabled: bool) -> AppResult<bool> {
    store.toggle_key(key_id, enabled)?;
    Ok(enabled)
}

/// 主 Key 切换的**触发来源**。只影响日志文案，不影响任何行为。
///
/// 为什么值得为一行文案立个枚举：界面与托盘（FR-022）走的是同一条重排规则
/// （`Store::set_primary_key` 是单一事实来源），而这两条入口曾经各写一份规则、结果漂移。
/// 日志里能分辨「这次是从托盘点的」是当时定位漂移的唯一线索，所以这个区分要保住；
/// 而用 `&str` 让调用方自己拼前缀，等于把它退回成可以随手写错的自由文本。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PrimarySource {
    /// 主界面的 Key 卡片。
    Ui,
    /// 托盘「主 Key」子菜单。
    Tray,
}

impl PrimarySource {
    fn log_prefix(self) -> &'static str {
        match self {
            PrimarySource::Ui => "",
            PrimarySource::Tray => "托盘",
        }
    }
}

/// 把某 Key 设为该分类的主 Key（优先级 0），并记一条事件日志。
///
/// 重排规则在 [`Store::set_primary_key`] 里（单一事实来源）——界面与托盘都调它，
/// 两处各算一份必然漂移。返回**是否真的改了**（已是主则 `false`）：
/// 调用方据此决定要不要刷新界面 / 重建托盘勾选，`false` 时不该刷日志（用户点了当前项属正常操作）。
pub(crate) fn set_primary_key(
    store: &Store,
    category: CategoryType,
    key_id: &str,
    source: PrimarySource,
) -> AppResult<bool> {
    let changed = store.set_primary_key(category, key_id)?;
    if changed {
        let name = store
            .get_key(key_id)
            .map(|k| k.name)
            .unwrap_or_else(|| key_id.to_string());
        store.append_event(
            category,
            "config",
            Some(key_id),
            &format!(
                "{}设为主 Key：{name}（优先级 0，其余顺延）",
                source.log_prefix()
            ),
        );
    }
    Ok(changed)
}

/// 把 Max Tokens 一次应用到该分类下全部 Key（FR-005 批量设置）。
///
/// 规则在 [`Store::apply_max_tokens_to_category`] 里（含已停用的 Key：否则日后重新启用
/// 又带回旧值）。返回**实际改动条数**，让前端如实提示「已应用到 N 条」——
/// 全都已是该值时返回 0，不谎报「已保存」。
pub(crate) fn apply_max_tokens_to_category(
    store: &Store,
    category: CategoryType,
    max_tokens: u32,
) -> AppResult<usize> {
    let changed = store.apply_max_tokens_to_category(category, max_tokens)?;
    if changed > 0 {
        store.append_event(
            category,
            "config",
            None,
            &format!("批量设置 Max Tokens = {max_tokens}（本分类 {changed} 条 Key 已更新）"),
        );
    }
    Ok(changed)
}

/// 清理孤儿密钥（P2-3）。**破坏性操作**：先备份密钥库，再删。返回被清理条数。
///
/// 备份失败即放弃清理（`?` 直接返回）：孤儿残留是无害的（只占空间），
/// 为了整洁去冒「删了没法恢复」的风险不值。硬规则「改配置前必备份」在这里是字面意义的。
///
/// 锁定态下 `Store::prune_orphan_secrets` 会直接返回 0（不误删），故这里无需另做判断。
pub(crate) fn prune_orphan_secrets(store: &Store) -> AppResult<usize> {
    store.secrets.read().backup_before_rewrite("prune-orphans")?;
    let n = store.prune_orphan_secrets();
    if n > 0 {
        store.append_event(
            CategoryType::ClaudeCli,
            "config",
            None,
            &format!("已清理 {n} 条孤儿密钥（配置中已无对应 Key），清理前已备份密钥库"),
        );
    }
    Ok(n)
}

// ==== 模型探测 ====

/// 用**已保存**的 Key 探测模型列表并落盘。
///
/// `context_window` 由 `merge_context_windows` 从旧列表继承——上游 `/v1/models`
/// 普遍不返回这个值，而它是用户手填的（1M 上下文徽章、聚合预算都看它）。
/// 不继承就等于「每次点一下拉取，用户填过的窗口全被清空」。
pub(crate) async fn fetch_models_for_key(
    store: &Store,
    key_id: &str,
) -> AppResult<Vec<ModelInfo>> {
    let key = store
        .get_key(key_id)
        .ok_or_else(|| AppError::NotFound(key_id.to_string()))?;
    let secret = store
        .secrets
        .read()
        .get(key_id)?
        .ok_or_else(|| AppError::Invalid("未配置密钥".into()))?;

    let names = crate::upstream::fetch_models(&key, &secret).await?;
    let models = merge_context_windows(names, &key.models);
    store.set_models(key_id, models.clone())?;
    Ok(models)
}

/// 用编辑器里**正在填写**的 Key 草稿探测模型列表（不落盘）。
///
/// 新增 Key 时前端还没有真实 id（是临时 `k_new`），无法走 store 查找，故直接传 key 对象。
/// `secret` 为空时（编辑已有 Key、未重填密钥）回退到库里已存的那条。
///
/// 两个来源都统一成 `Zeroizing`：前端传入的草稿密钥与库里已存的都是明文，
/// 没理由只保护其中一个。
pub(crate) async fn fetch_models_for_draft(
    store: &Store,
    key: &ProviderKey,
    secret: Option<String>,
) -> AppResult<Vec<ModelInfo>> {
    let secret: zeroize::Zeroizing<String> = match secret {
        Some(s) if !s.is_empty() => zeroize::Zeroizing::new(s),
        _ => store
            .secrets
            .read()
            .get(&key.id)?
            .ok_or_else(|| AppError::Invalid("未配置密钥".into()))?,
    };
    let names = crate::upstream::fetch_models(key, &secret).await?;
    // 草稿路径也走同一个继承函数。前端 KeyEditor 拉取回来后自己**也**做了一次同名继承
    // （`m.contextWindow ?? oldCtx.get(...)`），所以这里的结果对它是幂等的、不改变现有行为。
    // 之所以仍要在后端做：两条命令语义一致，日后有人只改其中一边时不会一边继承一边清空。
    Ok(merge_context_windows(names, &key.models))
}

/// 把上游返回的模型名列表包装成 `ModelInfo`，并从 `previous` 里**继承同名条目的
/// `context_window`**。
///
/// 抽成纯函数是为了可测：这条继承规则一旦失效，症状是「用户手填的上下文窗口偶尔消失」，
/// 而它只在「填过窗口 + 又点了拉取」这个组合下出现，端到端很难稳定复现。
fn merge_context_windows(names: Vec<String>, previous: &[ModelInfo]) -> Vec<ModelInfo> {
    let now = chrono::Utc::now().timestamp_millis();
    let old_ctx: std::collections::HashMap<&str, Option<u32>> = previous
        .iter()
        .map(|m| (m.real_name.as_str(), m.context_window))
        .collect();
    names
        .into_iter()
        .map(|n| {
            let context_window = old_ctx.get(n.as_str()).copied().flatten();
            ModelInfo {
                real_name: n,
                source: "fetched".into(),
                fetched_at: Some(now),
                context_window,
            }
        })
        .collect()
}

// ==== 桌面端模型名校验 ====

/// 桌面端 Key 的**对外模型名**必须是 Claude 桌面端会接受的形态，否则拒绝落盘。
///
/// 为什么在保存时就拦（而非只在接入时告警）：不合规的名字会被桌面端在加载配置时
/// **过滤掉**（不是提示）。全部被过滤 → 模型选择器为空 → 打开会话抛
/// `ModelsNotDiscoveredError`，症状与「卡在登录页」同级难排查，而用户此刻已经看不出
/// 是几天前存 Key 时埋下的。故在源头拒绝，不让不可用的配置进库。
///
/// 校验对象是 `serviceable_models()`（即真正写进 gateway 档 `inferenceModels` 的那份对外名），
/// **不是** `models`。真实上游名不受限制——配一条映射 `claude-opus-4-8` → `glm-4.6` 即可：
/// 对外用合规名，上游仍打 `glm-4.6`。
///
/// 只拦桌面端分类：CLI 只要求 `claude`/`anthropic` 前缀（且 `to_gateway_model_id` 会自动包
/// `claude-synaroute-` 前缀救回来），Codex 走 OpenAI 形态、无此约束。
///
/// **无条件校验**（不因「只改了优先级」而放行）：库里若已有不合规的历史 Key
/// （老版本存的、或 cc-switch 导入后补的模型），放行只会让它继续以不可用状态留在库里，
/// 到接入那一刻才炸。宁可在用户下一次触碰它时就要求修好。
pub(crate) fn reject_desktop_key_with_unusable_model_names(key: &ProviderKey) -> AppResult<()> {
    // 与前端即时校验（`check_desktop_model_names`）共用同一份体检，
    // 保证「界面上提示的」与「保存时拦的」永远是同一批名字、同一个建议。
    // 两边各自 filter 是这类功能最典型的漂移源：用户点了界面给的修法，保存仍被拒。
    let report = crate::model::desktop_model_name_report(key);
    if report.issues.is_empty() {
        return Ok(());
    }
    let bad: Vec<&str> = report.issues.iter().map(|i| i.name.as_str()).collect();
    Err(crate::error::AppError::Invalid(format!(
        "Claude 桌面端不接受这些对外模型名：{}\n\n\
         桌面端会在加载配置时把它们从模型列表里过滤掉。全部被过滤后模型选择器为空，\
         打开会话会报 ModelsNotDiscoveredError（症状与「卡在登录页」同级难排查）。\n\n\
         要求：名字须含 claude/opus/sonnet/haiku/fable/mythos/anthropic 之一，\
         且不得含 glm/gpt/grok/deepseek/qwen/kimi/llama 等厂商名\
         （判据取自桌面端 app.asar v1.24012.9）。\
         注意 claude-synaroute- 前缀对桌面端无效——厂商名黑名单优先。\n\n\
         修法：在「模型映射」里配一条 {} → {}，\
         对外用合规名、上游仍打原名。",
        bad.join("、"),
        report.issues[0].suggestion,
        report.issues[0].name
    )))
}

/// 一组对外模型名里，桌面端**不接受**的那些。
///
/// 抽成纯函数是为了可测：判据本身取自逆向桌面端 app.asar，而「判据对不对」只能靠
/// 逐条断言，不能靠端到端跑一次（那要真的启动桌面端、真的去点模型选择器）。
pub(crate) fn desktop_unacceptable_models(models: &[String]) -> Vec<&str> {
    models
        .iter()
        .filter(|m| !crate::model::is_desktop_acceptable_model_id(m))
        .map(String::as_str)
        .collect()
}

// ==== 主口令 UI 镜像 ====

/// 启用主口令：把库里全部 DPAPI 密文改用口令派生密钥重新封装。返回迁移条数。
///
/// **settings 镜像只在库迁移成功之后才更新**，顺序不可交换：反过来会出现
/// 「配置说开着、库里其实还是 DPAPI」的死局 —— 下次启动按配置要求解锁，
/// 而库里没有 master 头部，解不了也用不了。
///
/// 写锁在第一条语句结束即释放（`.write()` 的临时借用不跨到下一行），
/// 故随后的 `sync_master_flag` 去拿 config 写锁不会与它叠在一起。
pub(crate) fn enable_master_password(store: &Store, password: &str) -> AppResult<usize> {
    let migrated = store.secrets.write().enable_master_password(password)?;
    sync_master_flag(store, true);
    tracing::info!("已启用主口令模式，迁移 {migrated} 条密钥");
    Ok(migrated)
}

/// 关闭主口令：用口令解出全部密钥、改回 DPAPI。需要当前主口令确认。返回迁移条数。
pub(crate) fn disable_master_password(store: &Store, password: &str) -> AppResult<usize> {
    let migrated = store.secrets.write().disable_master_password(password)?;
    sync_master_flag(store, false);
    tracing::info!("已关闭主口令模式，迁移 {migrated} 条密钥回 DPAPI");
    Ok(migrated)
}

/// 把 settings 里的 UI 镜像字段对齐到密钥库的真实模式。
///
/// best-effort：写失败只告警。真实模式记在密钥库里，镜像不一致最多让开关显示错，
/// 下次启动的对账会修正——不该让「设置落盘失败」把已成功的密钥迁移回滚掉。
pub(crate) fn sync_master_flag(store: &Store, enabled: bool) {
    if let Err(e) = store.set_master_password_flag(enabled) {
        tracing::warn!("同步主口令开关到设置失败（密钥库已切换成功，显示可能暂不同步）: {e}");
    }
}

// ==== MCP 辅助 ====

/// MCP 服务器地址（供前端展示实际绑定端口的接入地址）。
pub(crate) fn mcp_url_for(port: u16) -> String {
    format!("http://127.0.0.1:{port}/mcp")
}

/// MCP 客户端单次工具调用超时（毫秒）= 各分类整轮预算 total_timeout_ms 的最大值 + 余量，
/// 且不低于历史兜底下限（crate::tools::MCP_TOOL_TIMEOUT_MS）。
///
/// 联动的意义：服务端聚合是整轮墙钟预算（见 aggregate.rs），客户端 MCP 超时必须不小于
/// 「该预算 + 余量」，才能保证服务端总在客户端杀连接**之前**优雅降级返回（哪怕是部分结果）。
/// 用户在任一分类调大整轮预算，下次注册/端口漂移重写时客户端超时自动跟随。MCP 客户端一个
/// server 只有一个 timeout，而分类各有自己的 total——故取最大值覆盖所有分类。
pub(crate) fn mcp_client_timeout_ms(store: &Store) -> u64 {
    /// 客户端超时相对服务端整轮预算的余量（毫秒）：留给降级结果序列化 + 网络回传，
    /// 保证服务端先返回、客户端后到点。
    const MARGIN_MS: u64 = 30_000;
    let max_total = CategoryType::ALL
        .iter()
        .map(|c| store.get_brain(*c).total_timeout_ms)
        .max()
        .unwrap_or(0);
    max_total
        .saturating_add(MARGIN_MS)
        .max(crate::tools::MCP_TOOL_TIMEOUT_MS)
}

// ==== 更新检查 ====

/// 检查更新的结构化结果（前端徽章 / 设置页共用）。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpdateCheckResult {
    /// available | up_to_date | error
    pub(crate) status: String,
    pub(crate) current_version: String,
    /// 远端新版本号（仅 available）
    pub(crate) version: Option<String>,
    /// 发布说明（可选）
    pub(crate) notes: Option<String>,
    /// 人类可读错误（仅 error）；已对私有仓库 404 等做友好化
    pub(crate) error: Option<String>,
}

/// 把 updater 的原始英文错误翻成可行动的中文。
pub(crate) fn friendly_updater_error(raw: &str) -> String {
    let lower = raw.to_ascii_lowercase();
    if lower.contains("could not fetch a valid release json")
        || lower.contains("404")
        || lower.contains("not found")
    {
        return format!(
            "无法拉取更新清单 latest.json（常见原因：GitHub 仓库为私有，\
             公开 URL 返回 404；或尚未上传 Release 资产）。\
             请把 Release 资产放到可匿名访问的地址，或将仓库设为公开。原始错误: {raw}"
        );
    }
    if lower.contains("signature") || lower.contains("minisign") {
        return format!("更新包签名校验失败（公钥与发版签名不匹配）。原始错误: {raw}");
    }
    format!("检查更新失败: {raw}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 唯一临时目录里的一份真实 Store（不引第三方 crate，同 store.rs 的做法）。
    ///
    /// 用真 Store 而不是造 mock：本层要验的恰恰是「多步写入的顺序与失败处理」，
    /// mock 掉落盘就等于把要验的东西抹掉了。
    fn temp_store(tag: &str) -> (std::sync::Arc<Store>, std::path::PathBuf) {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "synaroute_svc_{}_{}_{}",
            tag,
            std::process::id(),
            seq
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let store =
            Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap();
        (std::sync::Arc::new(store), dir)
    }

    fn key(category: CategoryType) -> ProviderKey {
        ProviderKey {
            id: "k1".into(),
            category_id: category,
            name: "k".into(),
            vendor: "test".into(),
            base_url: "https://example.com".into(),
            protocol: Protocol::Anthropic,
            has_secret: true,
            enabled: true,
            priority: 0,
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

    fn model(real_name: &str) -> ModelInfo {
        ModelInfo {
            real_name: real_name.into(),
            source: "manual".into(),
            context_window: None,
            fetched_at: None,
        }
    }

    /// 桌面端 Key 的对外名不合规 → 拒绝落盘，且错误信息要能直接照做。
    ///
    /// 这是数据可用性防线：不合规名会被桌面端**过滤掉**（不是提示），全被过滤后模型选择器为空、
    /// 打开会话抛 ModelsNotDiscoveredError。若放行到接入那一刻才报，用户已经看不出是存 Key 时埋的。
    #[test]
    fn desktop_key_with_unusable_outward_names_is_rejected() {
        let mut k = key(CategoryType::ClaudeDesktop);
        k.models = vec![model("glm-4.6"), model("grok-4.5")];
        let err = reject_desktop_key_with_unusable_model_names(&k)
            .expect_err("桌面端对外名不合规必须拒绝落盘");
        let msg = err.to_string();
        assert!(msg.contains("glm-4.6") && msg.contains("grok-4.5"), "要列出具体名字: {msg}");
        assert!(
            msg.contains("ModelsNotDiscoveredError"),
            "要说清后果，否则用户不知道为什么必须改: {msg}"
        );
        assert!(msg.contains("模型映射"), "要给出可照做的修法: {msg}");
        assert!(
            msg.contains("claude-synaroute-"),
            "要明说该前缀对桌面端无效，否则会有人以为包前缀能绕过: {msg}"
        );
    }

    /// 有映射时校验的是**对外名**，上游真实名不受限——这正是官方给的解法。
    #[test]
    fn desktop_key_mapping_makes_noncompliant_upstream_name_acceptable() {
        let mut k = key(CategoryType::ClaudeDesktop);
        // 上游真实名是 glm-4.6（不合规），但对外只暴露 claude-opus-4-8。
        k.models = vec![model("glm-4.6")];
        k.mappings = vec![ModelMapping {
            id: "m1".into(),
            expected_name: "claude-opus-4-8".into(),
            real_name: "glm-4.6".into(),
        }];
        assert_eq!(
            k.serviceable_models(),
            vec!["claude-opus-4-8".to_string()],
            "前置条件：有映射时只暴露对外名"
        );
        assert!(
            reject_desktop_key_with_unusable_model_names(&k).is_ok(),
            "对外名合规即可放行，上游真实名不该受限"
        );
    }

    /// 只拦桌面端：CLI 靠 to_gateway_model_id 包前缀救回，Codex 走 OpenAI 形态无此约束。
    #[test]
    fn cli_and_codex_keys_are_not_restricted_by_desktop_rule() {
        for cat in [CategoryType::ClaudeCli, CategoryType::Codex] {
            let mut k = key(cat);
            k.models = vec![model("glm-4.6"), model("gpt-5")];
            assert!(
                reject_desktop_key_with_unusable_model_names(&k).is_ok(),
                "{cat:?} 不该受桌面端模型名判据约束"
            );
        }
    }

    /// 校验是**无条件**的：即使这次只动了优先级，历史遗留的不合规 Key 也必须先修好。
    ///
    /// 取舍理由（用户拍板「该严，避免不可用」）：放行只会让不可用的 Key 继续留在库里，
    /// 到接入那一刻才炸。代价是老数据调优先级/开关会被拦，但报错已给出修法。
    #[test]
    fn desktop_rule_applies_even_when_only_priority_changed() {
        let mut k = key(CategoryType::ClaudeDesktop);
        k.models = vec![model("glm-4.6")];
        k.priority = 7; // 模拟「只改了优先级」的那次保存
        assert!(
            reject_desktop_key_with_unusable_model_names(&k).is_err(),
            "历史不合规 Key 即使只改优先级也必须拦，否则不可用状态会一直留在库里"
        );
    }

    /// 三档配了会追加 claude-*-4-5 家族名（合规），不该因此被误拦。
    #[test]
    fn desktop_key_with_only_tiers_passes() {
        let mut k = key(CategoryType::ClaudeDesktop);
        k.tier_opus = Some("glm-4.5".into()); // 档位真实名不合规不影响：它不进对外集合
        assert_eq!(k.serviceable_models(), vec!["claude-opus-4-5".to_string()]);
        assert!(reject_desktop_key_with_unusable_model_names(&k).is_ok());
    }

    /// 无模型、无映射的空 Key（如 cc-switch 刚导入）→ 对外集合为空，放行。
    /// 导入语义是「先导进来、用户再补模型」，此刻拦截无意义；补模型时走上面那条拦截。
    #[test]
    fn desktop_key_without_any_model_passes() {
        let k = key(CategoryType::ClaudeDesktop);
        assert!(k.serviceable_models().is_empty());
        assert!(reject_desktop_key_with_unusable_model_names(&k).is_ok());
    }

    /// `desktop_unacceptable_models` 是纯筛子：**保序**、只回不合规的那些。
    ///
    /// 保序是有意义的判据——错误信息里列出的名字顺序要和用户在编辑器里看到的一致，
    /// 否则用户得在一串名字里来回找。
    #[test]
    fn desktop_unacceptable_models_keeps_order_and_filters_only_bad_ones() {
        let names: Vec<String> = ["claude-opus-4-5", "glm-4.6", "claude-sonnet-4-5", "grok-4.5"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(desktop_unacceptable_models(&names), vec!["glm-4.6", "grok-4.5"]);
        assert!(desktop_unacceptable_models(&[]).is_empty());
    }

    /// `mcp_client_timeout_ms` 必须给客户端**留出余量**：它是客户端等 MCP 工具返回的上限，
    /// 若刚好等于聚合总预算，客户端会在服务端还差最后一口气时先超时断开 ——
    /// 用户看到的是「工具没反应」，而服务端日志里那次聚合其实成功了。
    ///
    /// 另一条判据是取**全部分类的最大值**：MCP 只有一个服务、一份客户端配置，
    /// 而聚合超时是每分类各存一条（见 memory `brain-timeout-per-category-and-ua-403`）。
    /// 若取当前分类的值，改另一个分类的超时就会让这份配置偏小。
    #[test]
    fn mcp_client_timeout_covers_the_slowest_category_with_headroom() {
        let (store, dir) = temp_store("mcp_timeout");

        let mut slow = store.get_brain(CategoryType::Codex);
        slow.total_timeout_ms = 600_000;
        store.save_brain(slow).unwrap();

        let t = mcp_client_timeout_ms(&store);
        assert!(
            t > 600_000,
            "必须大于最慢分类的聚合总预算，否则客户端先断: {t}"
        );
        assert!(
            t >= crate::tools::MCP_TOOL_TIMEOUT_MS,
            "不得低于工具自身下限: {t}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- 批 2：Key / 密钥 / 模型探测编排 ----

    /// `save_secret` 的**顺序**是本层最要紧的一条不变量：先写密钥库，成功后才置
    /// `has_secret`。这条测试正向验证「写成功后标记也跟上」。
    ///
    /// 反向（写库失败时标记不得被写脏）由下一条测试守。
    #[test]
    fn save_secret_marks_has_secret_only_after_vault_write() {
        let (store, dir) = temp_store("save_secret");
        let mut k = key(CategoryType::ClaudeCli);
        k.has_secret = false;
        store.upsert_key(k).unwrap();

        save_secret(&store, "k1", "sk-abc").unwrap();

        assert!(
            store.get_key("k1").unwrap().has_secret,
            "写库成功后 has_secret 必须为真，否则 UI 会显示「未配置密钥」"
        );
        assert_eq!(
            store.secrets.read().get("k1").unwrap().as_deref().map(String::as_str),
            Some("sk-abc"),
            "密钥应真的进了库"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 写密钥库失败时，**`has_secret` 不得被置真**。
    ///
    /// 用主口令锁定态制造确定性失败（`set` 在锁定态返回 Err，见 secret.rs）。
    /// 若顺序反了（先标记后写库），这里会留下「config 记有密钥、库里实际没有」的不一致：
    /// UI 依据 `has_secret` 显示已配置，而转发取不到密钥、报「未配置密钥」——
    /// 用户完全看不出自己配过了。
    #[test]
    fn save_secret_leaves_flag_untouched_when_vault_write_fails() {
        let (store, dir) = temp_store("save_secret_fail");
        let mut k = key(CategoryType::ClaudeCli);
        k.has_secret = false;
        store.upsert_key(k).unwrap();

        // 启用主口令后立即上锁：此后 set 必失败。
        store.secrets.write().enable_master_password("pw-123456").unwrap();
        store.secrets.write().lock();

        let err = save_secret(&store, "k1", "sk-abc");
        assert!(err.is_err(), "锁定态写密钥必须失败（前置条件）");
        assert!(
            !store.get_key("k1").unwrap().has_secret,
            "写库失败时绝不能留下 has_secret=true 的脏标记"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `toggle_key` 返回值的语义是「**要不要补一次健康探测**」，不是「操作是否成功」。
    ///
    /// 启用→true（陈旧的 status 需要立刻刷新）、停用→false（`Store::toggle_key` 已把
    /// health 清成 Unknown，再花最长 8s 探一条马上不被路由的 Key 纯属浪费）。
    #[test]
    fn toggle_key_asks_for_probe_only_when_enabling() {
        let (store, dir) = temp_store("toggle");
        store.upsert_key(key(CategoryType::ClaudeCli)).unwrap();

        assert!(
            toggle_key(&store, "k1", true).unwrap(),
            "启用时必须要求补探测：定时探测只扫启用的 Key，停用期间的结论已经陈旧"
        );
        assert!(
            !toggle_key(&store, "k1", false).unwrap(),
            "停用时不该探测：health 已被清成 Unknown，探它没有收益"
        );

        // 不存在的 Key 必须报错，而不是静默返回 false。
        assert!(toggle_key(&store, "nope", true).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 主 Key 切换：改动才记日志，且日志前缀能分辨来源（界面 / 托盘）。
    ///
    /// 「已经是主时不记日志」不是洁癖——托盘每次点击都会调它，若无条件记录，
    /// 用户点两下当前项就在日志里刷出两条无意义的「设为主 Key」。
    #[test]
    fn set_primary_logs_once_and_records_the_trigger_source() {
        let (store, dir) = temp_store("primary");
        let mut a = key(CategoryType::ClaudeCli);
        a.id = "ka".into();
        a.name = "甲".into();
        a.priority = 0;
        let mut b = key(CategoryType::ClaudeCli);
        b.id = "kb".into();
        b.name = "乙".into();
        b.priority = 1;
        store.upsert_key(a).unwrap();
        store.upsert_key(b).unwrap();

        assert!(
            set_primary_key(&store, CategoryType::ClaudeCli, "kb", PrimarySource::Tray).unwrap(),
            "从非主切到主必须返回 true"
        );
        let events = store.list_events(CategoryType::ClaudeCli);
        let logged: Vec<&str> = events
            .iter()
            .filter(|e| e.detail.contains("设为主 Key"))
            .map(|e| e.detail.as_str())
            .collect();
        assert_eq!(logged.len(), 1, "只该记一条: {logged:?}");
        assert!(logged[0].starts_with("托盘"), "来源前缀要保住: {}", logged[0]);
        assert!(logged[0].contains('乙'), "要记 Key 名而不是 id: {}", logged[0]);

        // 幂等：已是主 → 返回 false 且不再追加日志。
        assert!(
            !set_primary_key(&store, CategoryType::ClaudeCli, "kb", PrimarySource::Ui).unwrap()
        );
        let after = store.list_events(CategoryType::ClaudeCli);
        assert_eq!(
            after.iter().filter(|e| e.detail.contains("设为主 Key")).count(),
            1,
            "重复点当前项不该刷日志"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 界面来源**没有**前缀（历史文案），托盘来源有。两者必须能在日志里区分开。
    #[test]
    fn ui_source_has_no_prefix_so_the_two_sources_stay_distinguishable() {
        let (store, dir) = temp_store("primary_ui");
        let mut a = key(CategoryType::ClaudeCli);
        a.id = "ka".into();
        a.priority = 0;
        let mut b = key(CategoryType::ClaudeCli);
        b.id = "kb".into();
        b.priority = 1;
        store.upsert_key(a).unwrap();
        store.upsert_key(b).unwrap();

        set_primary_key(&store, CategoryType::ClaudeCli, "kb", PrimarySource::Ui).unwrap();
        let detail = store
            .list_events(CategoryType::ClaudeCli)
            .into_iter()
            .find(|e| e.detail.contains("设为主 Key"))
            .expect("应有一条日志")
            .detail;
        assert!(
            detail.starts_with("设为主 Key"),
            "界面来源不带前缀（与历史文案一致）: {detail}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 批量 Max Tokens：**返回实际改动条数**，全都已是该值时为 0 且不记日志。
    ///
    /// 这条数字会直接显示给用户（「已应用到 N 条」）。若无条件返回总条数，
    /// 就变成了在没有任何改动时也谎报「已保存」。
    #[test]
    fn apply_max_tokens_reports_real_change_count_and_is_idempotent() {
        let (store, dir) = temp_store("max_tokens");
        for (i, id) in ["ka", "kb"].iter().enumerate() {
            let mut k = key(CategoryType::ClaudeCli);
            k.id = (*id).into();
            k.priority = i as i32;
            store.upsert_key(k).unwrap();
        }

        assert_eq!(
            apply_max_tokens_to_category(&store, CategoryType::ClaudeCli, 16_384).unwrap(),
            2,
            "两条都该被改"
        );
        assert_eq!(
            apply_max_tokens_to_category(&store, CategoryType::ClaudeCli, 16_384).unwrap(),
            0,
            "已是该值 → 0，不谎报「已保存」"
        );
        assert_eq!(
            store
                .list_events(CategoryType::ClaudeCli)
                .iter()
                .filter(|e| e.detail.contains("批量设置 Max Tokens"))
                .count(),
            1,
            "第二次无改动，不该再记日志"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 清理孤儿密钥：**必须先备份**，且只删配置里已无对应 Key 的那些。
    ///
    /// 备份是唯一能挽回误删的手段（硬规则「改配置前必备份」在这里是字面意义的）。
    /// 判据用「磁盘上出现 prune-orphans 备份文件」，而不是「函数没报错」——
    /// 后者在备份被谁改成 best-effort 之后依然会绿。
    #[test]
    fn prune_orphan_secrets_backs_up_first_and_spares_live_keys() {
        let (store, dir) = temp_store("prune");
        store.upsert_key(key(CategoryType::ClaudeCli)).unwrap();
        store.secrets.write().set("k1", "sk-live").unwrap();
        store.secrets.write().set("k-ghost", "sk-orphan").unwrap();

        assert_eq!(prune_orphan_secrets(&store).unwrap(), 1, "只该清掉那条孤儿");
        assert!(
            store.secrets.read().get("k1").unwrap().is_some(),
            "在用的密钥绝不能被清掉"
        );
        assert!(store.secrets.read().get("k-ghost").unwrap().is_none());

        let backups: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.contains("prune-orphans"))
            .collect();
        assert_eq!(backups.len(), 1, "删密钥前必须留下备份: {backups:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 模型探测的 `context_window` **必须从旧列表继承**。
    ///
    /// 上游 `/v1/models` 普遍不返回这个值，而它是用户手填的（1M 上下文徽章、
    /// 聚合上下文预算都看它）。不继承就等于「每点一次拉取，用户填过的窗口全被清空」，
    /// 而这个症状只在「填过窗口 + 又点了拉取」的组合下出现，端到端很难稳定复现。
    #[test]
    fn fetched_models_inherit_context_window_from_previous_list() {
        let previous = vec![
            ModelInfo {
                real_name: "claude-opus-4-5".into(),
                source: "manual".into(),
                context_window: Some(1_000_000),
                fetched_at: None,
            },
            ModelInfo {
                real_name: "已下线的模型".into(),
                source: "manual".into(),
                context_window: Some(200_000),
                fetched_at: None,
            },
        ];
        let names = vec![
            "claude-opus-4-5".to_string(),
            "claude-sonnet-4-5".to_string(),
        ];

        let merged = merge_context_windows(names, &previous);

        assert_eq!(merged.len(), 2, "结果只含上游这次返回的名字");
        assert_eq!(
            merged[0].context_window,
            Some(1_000_000),
            "同名条目要继承用户填过的窗口"
        );
        assert_eq!(
            merged[1].context_window, None,
            "上游新出现的模型没有历史值，保持 None（由用户补）"
        );
        assert!(
            merged.iter().all(|m| m.source == "fetched" && m.fetched_at.is_some()),
            "来源与时间戳要标成本次探测"
        );
        assert!(
            !merged.iter().any(|m| m.real_name == "已下线的模型"),
            "旧列表里上游已不返回的模型不该被带回来"
        );
    }
}
