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

use crate::error::AppResult;
use crate::model::*;
use crate::store::Store;
use std::sync::Arc;

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

/// 把 settings 里的 UI 镜像字段对齐到密钥库的真实模式。
///
/// best-effort：写失败只告警。真实模式记在密钥库里，镜像不一致最多让开关显示错，
/// 下次启动的对账会修正——不该让「设置落盘失败」把已成功的密钥迁移回滚掉。
pub(crate) fn sync_master_flag(store: &Arc<Store>, enabled: bool) {
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
        let dir = std::env::temp_dir().join(format!("synaroute_svc_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store =
            Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap();

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
}
