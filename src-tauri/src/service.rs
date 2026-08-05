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
use crate::mcp::McpManager;
use crate::model::*;
use crate::proxy::ProxyManager;
use crate::store::Store;
use std::sync::Arc;

// ==== 代理与工具接入 ====

/// 写进客户端配置的**对外模型名**列表。
///
/// 口径必须与 `GET /v1/models`、应用内模型下拉**完全一致**：都用
/// [`crate::proxy::discoverable_models`]（多 Key 取**交集**），而非主 Key 的
/// `serviceable_models()`（那是超集）。
///
/// 为什么这一条值得单独抽出来：它有三个调用点（接入、改端口后重写、托盘模型子菜单），
/// 历史上其中一个用了超集口径，症状是桌面端模型选择器列出备用 Key 无法服务的名字，
/// 故障转移到备用 Key 后那个模型必然 404。三处各写一遍就迟早再漂移一次。
pub(crate) fn models_for_apply(store: &Store, category: CategoryType) -> Vec<String> {
    crate::proxy::discoverable_models(&store.enabled_keys_sorted(category))
}

/// 起代理并写入目标工具配置（接入）。返回给用户看的提示文案。
///
/// 收 `&Arc<..>` 而非 `tauri::State`：托盘的菜单事件回调是同步闭包，要在
/// `async_runtime::spawn` 里跨 await 使用，`State` 借用 `AppHandle` 过不去。
/// 而托盘启停**必须**与界面按钮语义完全一致——起代理即写工具配置、停代理即还原。
/// 若托盘只 `proxy.start()` 不写 config，客户端读到的仍是官方端点，
/// 用户会看到「托盘显示已启动，但 Claude/Codex 根本没走代理」。
pub(crate) async fn apply_tool_config(
    store: &Arc<Store>,
    proxy: &Arc<ProxyManager>,
    category: CategoryType,
) -> AppResult<String> {
    let port = proxy.start(category).await?;
    let endpoint = format!("http://127.0.0.1:{port}");
    // 三端写入字段语义不同（禁止混写）：
    // - Claude CLI：取首个写 env.ANTHROPIC_MODEL + 顶层 model（对外名；策略 A 不写 DEFAULT_*）
    // - Codex：取首个写 config.toml 的 model（OpenAI 形态，无 ANTHROPIC_*）
    // - 桌面端：整份写进 gateway 档的 inferenceModels（3p 部署模式）
    let keys = store.enabled_keys_sorted(category);
    let models = crate::proxy::discoverable_models(&keys);
    let msg = crate::tools::apply(category, &endpoint, &models, &keys)?;
    record_apply_success(store, category, &models, &format!("写入工具配置: {endpoint}"));
    Ok(msg)
}

/// 接入写盘**成功之后**的统一记账：记一条 config 事件 + 对桌面端做一次对外名体检。
///
/// 把两件事捆成一个函数、而不是让每条接入路径各写两句：现在有两条路径
/// （接入、改端口后重写），日后若加第三条，分开写时最容易漏掉的恰恰是第二句
/// —— 而它的症状（模型选择器为空 / `ModelsNotDiscoveredError`）最难排查。
/// 捆在一起后要忘也只能整条忘掉，那是编译期就看得见的缺失。
///
/// 只在**成功**时调：失败时模型压根没写进去，此时提示「这些名字桌面端不接受」是噪音。
fn record_apply_success(
    store: &Store,
    category: CategoryType,
    models: &[String],
    detail: &str,
) {
    store.append_event(category, "config", None, detail);
    warn_desktop_unacceptable_models(store, category, models);
}

/// 桌面端接入时，把「不被桌面端接受的对外模型名」另记一条 error 事件。
///
/// 为什么必须落日志而不只靠弹窗：接入弹窗一关就没了，而这个问题的症状
/// （模型选择器为空 / `ModelsNotDiscoveredError`）往下要排查很久，得在运行日志里留痕。
///
/// 用 `error` 而不新增 `warn` kind：前端 LogsPage 的分组映射是穷举的，
/// 加新 kind 会落到「未分类」里；且这条本质就是「配置不可用」。
fn warn_desktop_unacceptable_models(store: &Store, category: CategoryType, models: &[String]) {
    if category != CategoryType::ClaudeDesktop {
        return;
    }
    let bad = desktop_unacceptable_models(models);
    if bad.is_empty() {
        return;
    }
    store.append_event(
        category,
        "error",
        None,
        &format!(
            "{} 个对外模型名不被 Claude 桌面端接受（{}）：桌面端会过滤掉它们{}。\
             请在「模型映射」里改成含 claude/opus/sonnet/haiku 的对外名。",
            bad.len(),
            bad.join("、"),
            if bad.len() == models.len() {
                "，模型选择器将为空、打开会话报 ModelsNotDiscoveredError"
            } else {
                ""
            }
        ),
    );
}

/// 设置某分类代理的**首选监听端口**（粘滞固定端口）。返回实际绑定到的端口。
///
/// 顺序：落盘新端口 → 停当前代理 → 用新端口重启 → 重写该分类客户端 config。
/// 端口是启动时绑定的，故改端口必须重启代理才生效；重写 config 让客户端下次读到新端口。
/// 客户端（Codex / Claude）需重启才会重读 config —— 但因端口从此固定，仅此一次。
///
/// 重写用的模型列表走 [`models_for_apply`]，与接入同源。**这一点是必须的**：
/// 若这里退回主 Key 的超集口径，改一次端口就会把桌面端 gateway 档的 `inferenceModels`
/// 从接入时的安全交集重写回超集，与接入路径自相矛盾。
///
/// 写 config 失败**不让整个改端口失败**：端口已经落盘、代理已按新端口跑起来了，
/// 此时返回 Err 会让前端以为改端口没成功，而实际状态已变——只记一条 error 事件。
pub(crate) async fn set_proxy_port(
    store: &Arc<Store>,
    proxy: &Arc<ProxyManager>,
    category: CategoryType,
    port: u16,
) -> AppResult<u16> {
    store.set_proxy_port(category, port)?;
    proxy.stop(category);
    let bound = proxy.start(category).await?;
    // 用**真实绑定端口**而非请求端口：首选端口被占用时会回退，写错客户端就连不上。
    let endpoint = format!("http://127.0.0.1:{bound}");
    let keys = store.enabled_keys_sorted(category);
    let models = crate::proxy::discoverable_models(&keys);
    match crate::tools::apply(category, &endpoint, &models, &keys) {
        Ok(_) => record_apply_success(
            store,
            category,
            &models,
            &format!("代理端口已改为 {bound}，已重写客户端配置（客户端需重启读取新端口）"),
        ),
        Err(e) => store.append_event(
            category,
            "error",
            None,
            &format!("改端口后重写客户端配置失败: {e}"),
        ),
    }
    Ok(bound)
}

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

/// 当前 MCP 状态快照（前端指示灯）。
pub(crate) fn mcp_status(mcp: &McpManager) -> McpStatus {
    McpStatus {
        running: mcp.is_running(),
        port: mcp.running_port(),
        last_error: mcp.last_error(),
    }
}

/// 客户端配置重写的**触发场合**。只影响失败时的日志文案，不影响行为。
///
/// 为什么要区分：这三种场合的排障方向完全不同。「端口变化后」说明端口漂移了但客户端没跟上
/// （客户端仍指向死端口，症状是工具调用连不上）；「重启后」是用户刚点过按钮、正等着看结果；
/// 「启动后」发生在无人看着的时候，日志是唯一线索。混成一句话会让日志读者
/// 分不清是自动漂移、开机自愈、还是自己刚点的。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RewriteReason {
    /// 实际绑定端口与首选端口不同（被占用后向上回退）。
    PortDrift,
    /// 手动重启 MCP 服务后的重新注入。
    Restart,
    /// 应用启动时 MCP 随之启动后的重新注入。
    Startup,
}

impl RewriteReason {
    fn describe(self) -> &'static str {
        match self {
            RewriteReason::PortDrift => "MCP 端口变化后重写客户端失败",
            RewriteReason::Restart => "MCP 重启后重写客户端失败",
            RewriteReason::Startup => "MCP 启动后重写客户端失败",
        }
    }
}

/// 把 synaroute MCP 注册进某分类对应工具的客户端配置，并把该分类记入
/// `settings.mcp_registered_categories`（去重）。
///
/// 记入集合这件事必须走 `Store::add_registered_category` 这条**后端专用**写入 ——
/// 不能用「读 settings → push → save_settings」：`save_settings` 的入参是白名单类型
/// `UserPrefs`，这个字段在类型上就不存在，那样写等于该集合永远为空，
/// 而它一空，端口漂移时的批量重写就会漏掉所有分类（客户端 MCP 指向死端口，永不自愈）。
///
/// 失败只记事件、不返回 Err：注册失败时 MCP 服务本身还在跑，
/// 让整个「启用 MCP」失败反而更糟（用户以为服务没起来）。
pub(crate) fn register_and_record(store: &Store, category: CategoryType, port: u16) {
    let url = mcp_url_for(port);
    let timeout_ms = mcp_client_timeout_ms(store);
    match crate::tools::register_mcp_client(category, &url, timeout_ms) {
        Ok((msg, _wrote)) => {
            store.append_event(category, "config", None, &msg);
            let _ = store.add_registered_category(category);
        }
        Err(e) => store.append_event(
            category,
            "error",
            None,
            &format!("MCP 自动注册到客户端失败: {e}"),
        ),
    }
}

/// 用新端口重写**所有已注册分类**的客户端配置（url 里的端口跟着变）。
///
/// 只在真写了盘时记事件（`wrote`）：`register_mcp_client` 内容相同即跳过写盘，
/// 无条件记录会让每次重启都在日志里刷出三条「已重写」而其实什么都没变。
pub(crate) fn rewrite_registered_clients(store: &Store, port: u16, reason: RewriteReason) {
    let url = mcp_url_for(port);
    let timeout_ms = mcp_client_timeout_ms(store);
    for category in store.get_settings().mcp_registered_categories {
        match crate::tools::register_mcp_client(category, &url, timeout_ms) {
            Ok((msg, wrote)) => {
                if wrote {
                    store.append_event(category, "config", None, &msg);
                }
            }
            Err(e) => store.append_event(
                category,
                "error",
                None,
                &format!("{}: {e}", reason.describe()),
            ),
        }
    }
}

/// 从某分类的客户端配置移除 synaroute，并从已注册集合剔除。
fn unregister_and_forget(store: &Store, category: CategoryType) -> AppResult<()> {
    match crate::tools::unregister_mcp_client(category) {
        Ok((msg, wrote)) => {
            if wrote {
                store.append_event(category, "config", None, &msg);
            }
        }
        Err(e) => {
            store.append_event(category, "error", None, &format!("MCP 断开失败: {e}"));
            return Err(e);
        }
    }
    let _ = store.remove_registered_category(category);
    Ok(())
}

/// 确保 MCP 服务在跑，返回**实际绑定端口**。
///
/// 未运行则以首选端口启动。端口回退时（首选被占用）做两件事：重写已注册分类的客户端配置、
/// 把实际端口粘为下次首选。**粘住这一步不能省**：否则每次启动都从被占的旧端口重新回退、
/// 重新重写配置，客户端每次都要重启才能跟上——治标不治本。
async fn ensure_mcp_running(store: &Store, mcp: &McpManager) -> Result<u16, String> {
    if let Some(p) = mcp.running_port() {
        return Ok(p);
    }
    let preferred = store.get_settings().mcp_port;
    let bound = mcp.start(preferred).await?;
    // 启动即视为 MCP 开启：持久化 enabled=true，否则下次冷启前端读配置以为没开。
    let _ = store.set_mcp_enabled_flag(true);
    if bound != preferred {
        rewrite_registered_clients(store, bound, RewriteReason::PortDrift);
        let _ = store.set_mcp_port(bound);
    }
    Ok(bound)
}

/// 单分类接入 MCP 大脑聚合：只给该分类写客户端配置，不影响其它分类。
///
/// 与 [`set_mcp_enabled`] 的区别：后者是全局开关且只认「当前活跃分类」，做不到多端同时接入；
/// 本函数 per-category 独立，可让 CLI 与 Codex 各自接入、互不干扰。
///
/// 前置是**服务必须在跑**（未跑则先启动）：客户端写了地址也连不上的话，
/// 用户看到的是「显示已接入但工具用不了」。
pub(crate) async fn register_mcp_for_category(
    store: &Store,
    mcp: &McpManager,
    category: CategoryType,
) -> AppResult<McpStatus> {
    let bound = match ensure_mcp_running(store, mcp).await {
        Ok(b) => b,
        Err(e) => {
            store.append_event(
                category,
                "error",
                None,
                &format!("MCP 启动失败，无法接入 {}: {e}", category.as_str()),
            );
            return Err(AppError::Proxy(e));
        }
    };
    register_and_record(store, category, bound);
    Ok(mcp_status(mcp))
}

/// 单分类断开：只从该分类客户端配置移除 synaroute，不停服务、不动其它分类。
pub(crate) fn unregister_mcp_for_category(
    store: &Store,
    mcp: &McpManager,
    category: CategoryType,
) -> AppResult<McpStatus> {
    unregister_and_forget(store, category)?;
    Ok(mcp_status(mcp))
}

/// 启用/停用 MCP 全局开关，并自动注册到「当前活跃分类」对应的工具。
///
/// **`enabled` 的落盘时序按启动结果决定**，不能先写 true 再启动：那样会造出
/// 「服务没起来但配置说开着」的错乱状态，前端下次冷启以为服务在跑、而 `mcp_status`
/// 显示 stopped。端口候选则**先落盘**（用户在设置里改的端口要保留，即便本次启动失败）。
///
/// 启动失败时回滚 `enabled=false` 并记事件，但**不返回 Err**：端口已经保存成功了，
/// 返回 Err 会让前端把「端口没存上」和「服务没起来」混为一谈。
pub(crate) async fn set_mcp_enabled(
    store: &Store,
    mcp: &McpManager,
    category: CategoryType,
    enabled: bool,
    port: u16,
) -> AppResult<McpStatus> {
    store.set_mcp_port(port)?;
    if enabled {
        match mcp.start(port).await {
            Ok(bound) => {
                store.set_mcp_enabled_flag(true)?;
                // 用**实际绑定端口**注册，保证客户端地址与真实端口一致。
                register_and_record(store, category, bound);
                if bound != port {
                    rewrite_registered_clients(store, bound, RewriteReason::PortDrift);
                    let _ = store.set_mcp_port(bound);
                }
            }
            Err(e) => {
                let _ = store.set_mcp_enabled_flag(false);
                store.append_event(
                    category,
                    "error",
                    None,
                    &format!("MCP 启动失败（enabled 已回滚为 false）: {e}"),
                );
                tracing::warn!("MCP 服务器启动失败: {e}");
            }
        }
    } else {
        mcp.stop();
        store.set_mcp_enabled_flag(false)?;
        // 关开关 = 从所有已注册分类移除 synaroute 并清空记录。
        // 逐条 best-effort：某一端的客户端文件被占用不该让其余端留着死配置。
        for category in store.get_settings().mcp_registered_categories {
            match crate::tools::unregister_mcp_client(category) {
                Ok((msg, wrote)) => {
                    if wrote {
                        store.append_event(category, "config", None, &msg);
                    }
                }
                Err(e) => {
                    store.append_event(category, "error", None, &format!("MCP 注销失败: {e}"))
                }
            }
        }
        store.clear_registered_categories()?;
    }
    Ok(mcp_status(mcp))
}

/// 手动重启 MCP 服务：先停后起（强制重新绑定端口），并重新注入客户端配置。
///
/// 用途：改了端口后立即重绑、端口冲突排障、客户端连不上时强制重连。
/// 注意大脑聚合参数（超时/Token/成员/决策者）是每次调用实时读的，改了保存即生效、
/// **不需要**走这里——本函数只影响 MCP 服务本身的监听与客户端 url 同步。
///
/// 无论端口是否变化都重新注入：手动重启的语义就是「把客户端配置也对齐一次」，
/// 这正是排障时最想要的动作。
pub(crate) async fn restart_mcp(store: &Store, mcp: &McpManager) -> AppResult<McpStatus> {
    let port = store.get_settings().mcp_port;
    mcp.stop();
    store.append_event(
        CategoryType::ClaudeCli,
        "config",
        None,
        &format!("MCP 重启：已停止旧服务，准备绑定端口 {port}"),
    );
    match mcp.start(port).await {
        Ok(bound) => {
            if bound != port {
                let _ = store.set_mcp_port(bound);
            }
            rewrite_registered_clients(store, bound, RewriteReason::Restart);
            // 一个分类都没注册、但开关是开的：按默认分类注入一次，
            // 避免留下「服务在跑但客户端没有任何配置」这种看起来正常的空转状态。
            if store.get_settings().mcp_registered_categories.is_empty() {
                register_and_record(store, CategoryType::ClaudeCli, bound);
            }
            store.append_event(
                CategoryType::ClaudeCli,
                "config",
                None,
                &format!("MCP 重启完成：{}（已重写客户端配置）", mcp_url_for(bound)),
            );
        }
        Err(e) => {
            tracing::warn!("MCP 重启失败: {e}");
            store.append_event(
                CategoryType::ClaudeCli,
                "error",
                None,
                &format!("MCP 重启失败: {e}"),
            );
        }
    }
    Ok(mcp_status(mcp))
}

/// 应用启动时随之拉起 MCP 服务（用户已启用时）。
///
/// 与 [`ensure_mcp_running`] 的区别：这里**不写** `enabled` 标记（它本来就是 true，
/// 正是它把我们带到这条路径上的），且失败只告警不返回 —— 启动期的 MCP 拉不起来
/// 不该影响应用本身可用。
///
/// 端口「粘住」这一步是必须的：某些机器上首选端口（9527 / 9528）被系统服务
/// （WUDFHost / 指纹服务）永久占用，每次开机都被迫回退。不把实际端口写回设置，
/// 就会每次开机重复「绑定失败 → 向上探测 → 回退 → 重写客户端」，
/// 而客户端配置只在客户端启动时读一次，用户那边表现为「时不时要重启 Claude 才能用」。
pub(crate) async fn start_mcp_on_launch(store: &Store, mcp: &McpManager) {
    let preferred = store.get_settings().mcp_port;
    match mcp.start(preferred).await {
        Ok(bound) => {
            if bound != preferred {
                let _ = store.set_mcp_port(bound);
            }
            // 无条件重写（不只在端口变化时）：客户端配置里的 url 可能来自上一次
            // 不同端口的运行，幂等写入内部会比对内容、没变就不写盘。
            rewrite_registered_clients(store, bound, RewriteReason::Startup);
        }
        Err(e) => tracing::warn!("MCP 服务器启动失败: {e}"),
    }
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

    // ---- 批 3：接入写盘的模型口径 ----

    /// `models_for_apply` 必须是**交集**口径（不是主 Key 的超集）。
    ///
    /// 这是三个调用点（接入、改端口后重写、托盘模型子菜单）共用的单一事实来源。
    /// 历史上其中一处用了主 Key 的 `serviceable_models()`，症状是桌面端模型选择器
    /// 列出备用 Key 无法服务的名字，故障转移到备用 Key 后那个模型必然 404 ——
    /// 而故障转移本身是偶发的，用户看到的是「这个模型有时候能用有时候 404」。
    #[test]
    fn models_for_apply_is_the_intersection_not_the_primary_superset() {
        let (store, dir) = temp_store("models_apply");

        let mut primary = key(CategoryType::ClaudeCli);
        primary.id = "ka".into();
        primary.priority = 0;
        primary.models = vec![model("claude-opus-4-5"), model("claude-sonnet-4-5")];
        let mut backup = key(CategoryType::ClaudeCli);
        backup.id = "kb".into();
        backup.priority = 1;
        backup.models = vec![model("claude-sonnet-4-5")]; // 备用 Key 服务不了 opus
        store.upsert_key(primary).unwrap();
        store.upsert_key(backup).unwrap();

        assert_eq!(
            models_for_apply(&store, CategoryType::ClaudeCli),
            vec!["claude-sonnet-4-5".to_string()],
            "只写两边都能服务的名字，否则转移到备用 Key 后 404"
        );

        // 停用备用 Key → 交集不再受它约束，主 Key 全集恢复可见。
        store.toggle_key("kb", false).unwrap();
        assert_eq!(
            models_for_apply(&store, CategoryType::ClaudeCli).len(),
            2,
            "停用的 Key 不该继续收窄可用模型"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 桌面端接入时，不合规的对外名要在**运行日志**里留痕（不只弹窗）。
    ///
    /// 接入弹窗一关就没了，而症状（选择器为空 / `ModelsNotDiscoveredError`）
    /// 往下要排查很久。另一条判据是「全部不合规」时文案要更重——那才是死局。
    #[test]
    fn desktop_apply_records_unacceptable_names_in_the_event_log() {
        let (store, dir) = temp_store("desktop_warn");

        // 部分不合规：提示里不该出现「选择器将为空」。
        warn_desktop_unacceptable_models(
            &store,
            CategoryType::ClaudeDesktop,
            &["claude-opus-4-5".into(), "glm-4.6".into()],
        );
        let partial = store
            .list_events(CategoryType::ClaudeDesktop)
            .into_iter()
            .find(|e| e.detail.contains("不被 Claude 桌面端接受"))
            .expect("应记一条 error 事件");
        assert_eq!(partial.kind, "error", "必须是 error（LogsPage 的分组是穷举的）");
        assert!(partial.detail.contains("glm-4.6"));
        assert!(
            !partial.detail.contains("模型选择器将为空"),
            "还有合规名时不该报死局: {}",
            partial.detail
        );

        // 全部不合规：必须点明后果。
        warn_desktop_unacceptable_models(&store, CategoryType::ClaudeDesktop, &["glm-4.6".into()]);
        let fatal = store
            .list_events(CategoryType::ClaudeDesktop)
            .into_iter()
            .filter(|e| e.detail.contains("不被 Claude 桌面端接受"))
            .count();
        assert_eq!(fatal, 2, "两次调用各留一条");
        assert!(
            store
                .list_events(CategoryType::ClaudeDesktop)
                .iter()
                .any(|e| e.detail.contains("ModelsNotDiscoveredError")),
            "全部不合规时必须点明会报 ModelsNotDiscoveredError"
        );

        // 非桌面端分类：一条都不该记（CLI 有前缀兜底、Codex 无此约束）。
        warn_desktop_unacceptable_models(&store, CategoryType::ClaudeCli, &["glm-4.6".into()]);
        warn_desktop_unacceptable_models(&store, CategoryType::Codex, &["gpt-5".into()]);
        assert!(
            store.list_events(CategoryType::ClaudeCli).is_empty()
                && store.list_events(CategoryType::Codex).is_empty(),
            "只有桌面端受这条判据约束"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 接入成功后的记账必须**两件事都做**：记 config 事件 + 桌面端对外名体检。
    ///
    /// 这条守的是「日后加第三条接入路径时漏掉体检」——把两件事捆进
    /// `record_apply_success` 就是为此，而这条测试保证捆绑本身不被拆开。
    #[test]
    fn apply_success_records_both_the_config_event_and_the_desktop_check() {
        let (store, dir) = temp_store("apply_record");

        record_apply_success(
            &store,
            CategoryType::ClaudeDesktop,
            &["glm-4.6".into()],
            "写入工具配置: http://127.0.0.1:8788",
        );

        let events = store.list_events(CategoryType::ClaudeDesktop);
        assert!(
            events.iter().any(|e| e.kind == "config" && e.detail.contains("写入工具配置")),
            "接入成功要留一条 config 事件: {events:?}"
        );
        assert!(
            events
                .iter()
                .any(|e| e.kind == "error" && e.detail.contains("不被 Claude 桌面端接受")),
            "同时必须做桌面端对外名体检，否则症状要排查很久: {events:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- 批 4：MCP 控制面 ----
    //
    // 注意这一批**刻意不测** register_and_record / rewrite_registered_clients / 启用路径：
    // 它们经 crate::tools::register_mcp_client 真写开发机上的 ~/.claude.json 与
    // Codex config.toml（且在包身份下写的是虚拟副本，见 CLAUDE.md 的平行宇宙陷阱）。
    // 单测里跑会破坏本机真实客户端配置——这类编排靠真机验证，不靠 mock 文件系统。
    // 能测的是它周围那几层：状态快照、日志文案的可分辨性、以及**关开关**这条不碰文件的路径。

    /// 三种重写场合的日志文案必须**互不相同**。
    ///
    /// 它们的排障方向完全不同：端口漂移=客户端指着死端口；手动重启=用户正等结果；
    /// 开机启动=无人看着、日志是唯一线索。若两条文案撞车，日志读者就分不清
    /// 「是自动漂移还是我刚点的」，而这恰好决定下一步该查什么。
    #[test]
    fn rewrite_reason_messages_stay_distinguishable() {
        let all = [
            RewriteReason::PortDrift,
            RewriteReason::Restart,
            RewriteReason::Startup,
        ];
        let msgs: Vec<&str> = all.iter().map(|r| r.describe()).collect();
        let uniq: std::collections::HashSet<&&str> = msgs.iter().collect();
        assert_eq!(uniq.len(), all.len(), "文案撞车会让日志无法分辨来源: {msgs:?}");
        assert!(
            msgs.iter().all(|m| m.contains("重写客户端失败")),
            "都要含这个可检索的共同片段，便于一次捞出全部重写失败: {msgs:?}"
        );
    }

    /// MCP 地址格式：**必须绑定 127.0.0.1**，且带 `/mcp` 路径。
    ///
    /// 不能写成 0.0.0.0 或本机 IP：这是给本机客户端用的回环地址，写成对外地址等于
    /// 把大脑聚合入口暴露到局域网（局域网暴露是代理那一侧独立的开关，不该由这里顺带打开）。
    #[test]
    fn mcp_url_is_loopback_with_the_mcp_path() {
        assert_eq!(mcp_url_for(9527), "http://127.0.0.1:9527/mcp");
        assert!(
            !mcp_url_for(9527).contains("0.0.0.0"),
            "绝不能绑 0.0.0.0：那会把聚合入口暴露到局域网"
        );
    }

    /// 未启动时的状态快照：`running=false`、无端口、无错误。
    ///
    /// 前端设置页的指示灯直接读这三项。若 `running` 在没起服务时就为 true，
    /// 用户会以为服务在跑、却怎么都连不上。
    #[test]
    fn mcp_status_reports_stopped_before_any_start() {
        let (store, dir) = temp_store("mcp_status");
        let mcp = McpManager::new(store.clone());

        let st = mcp_status(&mcp);
        assert!(!st.running, "没起服务就不能报 running");
        assert_eq!(st.port, None);
        assert_eq!(st.last_error, None, "没试过启动就不该有错误");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 关 MCP 开关：**端口候选先落盘**，且 `enabled` 落成 false。
    ///
    /// 端口先落盘这一点有独立意义：用户在设置里改了端口又把开关关掉，
    /// 那个端口值必须留下来（下次开启时用），不能因为「这次没启动」就丢掉。
    /// 故本例刻意先把 enabled 置 true，才能真的验证它被改回 false ——
    /// 默认值本就是 false，不做这个前置的话这条断言恒真、等于没测。
    ///
    /// **「关开关要清空已注册集合」这一条无法在单测里验证**：清空前要先逐个
    /// `unregister_mcp_client`，那会真写开发机的 ~/.claude.json 与 Codex config.toml。
    /// 故本例把集合保持为空（不触碰任何文件），那条编排靠真机验证。
    /// 这里如实记下来，免得日后有人以为它已被覆盖。
    #[tokio::test]
    async fn disabling_mcp_keeps_the_port_choice_and_turns_the_flag_off() {
        let (store, dir) = temp_store("mcp_disable");
        let mcp = McpManager::new(store.clone());
        store.set_mcp_enabled_flag(true).unwrap();
        assert!(
            store.get_settings().mcp_registered_categories.is_empty(),
            "前置条件：没有已注册分类，本测试才不会去写真实客户端配置"
        );

        let st = set_mcp_enabled(&store, &mcp, CategoryType::ClaudeCli, false, 9600)
            .await
            .unwrap();

        assert!(!st.running);
        let s = store.get_settings();
        assert_eq!(s.mcp_port, 9600, "改过的端口候选必须留下，供下次开启时用");
        assert!(
            !s.mcp_enabled,
            "开关必须落成 false，否则前端下次冷启以为服务在跑"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
