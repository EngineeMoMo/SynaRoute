//! SynaRoute Tauri 后端入口。
//! IPC 命令名与前端 src/lib/bridge.ts 严格对齐。

mod agent_tools;
mod aggregate;
mod ccswitch;
mod codegraph;
mod crypto;
mod error;
mod events;
mod health;
mod mcp;
mod model;
mod portable;
mod proc;
mod proxy;
mod retrieval;
mod secret;
mod store;
mod tools;
mod upstream;
/// upstream 对外契约守卫。**必须放在 upstream 外面**才能真正验证可见性，
/// 详见该文件的模块注释。
#[cfg(test)]
mod upstream_api_surface;
mod service;
mod workdirs;

use error::AppResult;
use mcp::McpManager;
use model::*;
use proxy::ProxyManager;
use std::sync::Arc;
use store::Store;
use tauri::Manager;

/// 全局应用状态
pub struct AppState {
    store: Arc<Store>,
    proxy: Arc<ProxyManager>,
    mcp: Arc<McpManager>,
}

// ============ 配置管理命令 ============

#[tauri::command]
fn list_keys(state: tauri::State<AppState>, category_id: CategoryType) -> Vec<ProviderKey> {
    state.store.list_keys(category_id)
}

#[tauri::command]
fn upsert_key(state: tauri::State<AppState>, key: ProviderKey) -> AppResult<ProviderKey> {
    service::save_key(&state.store, key)
}


/// 桌面端对外模型名**即时**体检（UX#4），供 KeyEditor 边打字边提示。
///
/// 刻意设计成**纯函数命令**：不收 `tauri::State`、不碰 store、全程不取任何 parking_lot 锁。
/// 它会随用户打字高频触发（防抖 250ms），若在这里取锁就会与转发热路径抢同一把锁。
///
/// 判据不在前端复刻：那是 50+ 条厂商名子串加一套词边界匹配，两份规则必然漂移，
/// 而漂移的两个方向都很糟——「界面说没问题、保存被拒」，或更糟的
/// 「界面放行、桌面端静默过滤掉」（后者表现为模型选择器为空，极难排查）。
#[tauri::command]
fn check_desktop_model_names(key: ProviderKey) -> crate::model::DesktopModelNameReport {
    crate::model::desktop_model_name_report(&key)
}

#[tauri::command]
fn delete_key(state: tauri::State<AppState>, key_id: String) -> AppResult<()> {
    state.store.delete_key(&key_id)
}

/// 按需揭示已存明文密钥（供编辑器"眼睛"查看/续用）。
/// 注意：这会把明文返回给前端 WebView，仅用于本地单机自管密钥场景。
#[tauri::command]
fn reveal_secret(state: tauri::State<AppState>, key_id: String) -> AppResult<Option<String>> {
    // 这里必须解出 Zeroizing：返回值要过 IPC 序列化给前端。
    // 到这一步「明文进 WebView」已是该命令的既定语义（供编辑器眼睛查看），
    // Zeroizing 保护的是**后端内部**那份副本的驻留时长。
    Ok(state.store.secrets.read().get(&key_id)?.map(|s| s.to_string()))
}

#[tauri::command]
fn save_secret(state: tauri::State<AppState>, key_id: String, secret: String) -> AppResult<()> {
    service::save_secret(&state.store, &key_id, &secret)
}

/// 启用/停用某条 Key。
///
/// 编排（含「启用时为何要补一次探测」的完整理由）在 [`service::toggle_key`]。
/// 这里只负责它无法做的那件事：用 `AppHandle` 取 store 的 `Arc` 送进 `spawn`
/// （不能把 `tauri::State` 跨 await 送进去），并让探测**异步跑、不阻塞返回**
/// —— 它最长可达 8s，同步等待会让用户点一下开关就看到界面明显卡顿。
#[tauri::command]
fn toggle_key(
    app: tauri::AppHandle,
    state: tauri::State<AppState>,
    key_id: String,
    enabled: bool,
) -> AppResult<()> {
    if service::toggle_key(&state.store, &key_id, enabled)? {
        let store = app.state::<AppState>().store.clone();
        tauri::async_runtime::spawn(async move {
            health::check_one(&store, &key_id).await;
        });
    }
    Ok(())
}

// ============ 主口令增强模式（FR-018 可选增强）============
//
// 默认走 Windows DPAPI（免口令、绑当前账户）。用户可改为「自己记口令」——由 Argon2id
// 从口令派生密钥加密整个密钥库（见 crate::crypto 与 crate::secret 的模块注释）。
//
// **模式的事实来源是密钥库文件本身**（有 master 头部即口令模式），不是 settings。
// settings.master_password_enabled 只是 UI 镜像，由下面几个命令与启动对账维护一致。

/// 主口令状态（是否启用 / 是否锁着）。前端每次进设置页与启动时读。
#[tauri::command]
fn get_master_password_state(state: tauri::State<AppState>) -> secret::MasterPasswordState {
    state.store.secrets.read().master_state()
}

/// 用主口令解锁本次进程。解锁前取不到任何密钥（转发会报「需解锁」）。
#[tauri::command]
fn unlock_master_password(state: tauri::State<AppState>, password: String) -> AppResult<()> {
    // 写锁在语句结束即释放，emit 在其后（纪律见 events::emit 文档）。
    state.store.secrets.write().unlock(&password)?;
    tracing::info!("密钥库已用主口令解锁");
    events::emit(events::Topic::Vault, None);
    Ok(())
}

/// 立即上锁（清掉进程内常驻的库密钥）。用于离开电脑前手动锁定。
#[tauri::command]
fn lock_master_password(state: tauri::State<AppState>) -> AppResult<()> {
    state.store.secrets.write().lock();
    events::emit(events::Topic::Vault, None);
    Ok(())
}

/// 启用主口令。编排（含「settings 镜像为何必须后写」）在 [`service::enable_master_password`]。
#[tauri::command]
fn enable_master_password(state: tauri::State<AppState>, password: String) -> AppResult<usize> {
    service::enable_master_password(&state.store, &password)
}

/// 关闭主口令：改回 DPAPI。需要输入当前主口令确认。
#[tauri::command]
fn disable_master_password(state: tauri::State<AppState>, password: String) -> AppResult<usize> {
    service::disable_master_password(&state.store, &password)
}

/// 修改主口令（旧口令验证 + 全库用新口令重新封装，含新盐与新校验串）。
#[tauri::command]
fn change_master_password(
    state: tauri::State<AppState>,
    old_password: String,
    new_password: String,
) -> AppResult<usize> {
    let n = state
        .store
        .secrets
        .write()
        .change_master_password(&old_password, &new_password)?;
    tracing::info!("主口令已修改，重新封装 {n} 条密钥");
    Ok(n)
}


/// 把某 Key 设为该分类的主 Key（优先级 0）。
///
/// 编排在 [`service::set_primary_key`]（重排规则、日志、幂等语义都在那里）。
/// 这里只补它做不到的那件事：托盘的「主 Key」子菜单要跟着更新勾选
/// —— 无论这次是从界面还是从托盘触发的。
#[tauri::command]
fn set_primary_key(
    app: tauri::AppHandle,
    state: tauri::State<AppState>,
    category_id: CategoryType,
    key_id: String,
) -> AppResult<bool> {
    let changed = service::set_primary_key(
        &state.store,
        category_id,
        &key_id,
        service::PrimarySource::Ui,
    )?;
    if changed {
        let _ = rebuild_tray(&app);
    }
    Ok(changed)
}

/// 把 Max Tokens 一次应用到该分类下全部 Key（FR-005 批量设置）。
/// 编排（含「为何返回实际改动条数」）在 [`service::apply_max_tokens_to_category`]。
#[tauri::command]
fn apply_max_tokens_to_category(
    state: tauri::State<AppState>,
    category_id: CategoryType,
    max_tokens: u32,
) -> AppResult<usize> {
    service::apply_max_tokens_to_category(&state.store, category_id, max_tokens)
}

// ============ 从 cc-switch 导入历史 Key ============
//
// 只读 cc-switch 的 SQLite 库（先复制到临时文件再打开），把它的供应商档映射成
// SynaRoute 的 Key + 加密密钥。**导入不接入**：不写任何客户端配置、不改接入状态。

/// 扫描 cc-switch 库，返回可导入候选（含掩码密钥、重复标记、不可导入原因）。
/// 明文密钥不出后端——前端只拿到掩码。
#[tauri::command]
fn scan_ccswitch(state: tauri::State<AppState>) -> AppResult<ccswitch::ScanResult> {
    ccswitch::scan(&state.store)
}

/// 按 sourceIds 导入选中的档。逐条独立处理，返回每条结局。
#[tauri::command]
fn import_from_ccswitch(
    state: tauri::State<AppState>,
    source_ids: Vec<String>,
) -> AppResult<ccswitch::ImportReport> {
    ccswitch::import(&state.store, &source_ids)
}

#[tauri::command]
async fn fetch_models(
    state: tauri::State<'_, AppState>,
    key_id: String,
) -> AppResult<Vec<ModelInfo>> {
    service::fetch_models_for_key(&state.store, &key_id).await
}

/// 用编辑器里正在填写的 Key 草稿（可能尚未保存）直接探测模型列表，不落盘。
/// 编排在 [`service::fetch_models_for_draft`]。
#[tauri::command]
async fn fetch_models_draft(
    state: tauri::State<'_, AppState>,
    key: ProviderKey,
    secret: Option<String>,
) -> AppResult<Vec<ModelInfo>> {
    service::fetch_models_for_draft(&state.store, &key, secret).await
}

#[tauri::command]
async fn check_health(state: tauri::State<'_, AppState>, key_id: String) -> AppResult<()> {
    health::check_one(&state.store, &key_id).await;
    Ok(())
}

// ============ 大脑聚合命令 ============

#[tauri::command]
fn get_brain_config(state: tauri::State<AppState>, category_id: CategoryType) -> BrainConfig {
    state.store.get_brain(category_id)
}

#[tauri::command]
fn save_brain_config(state: tauri::State<AppState>, config: BrainConfig) -> AppResult<()> {
    state.store.save_brain(config)
}

// ============ 代理生命周期命令 ============

#[tauri::command]
fn get_proxy_state(state: tauri::State<AppState>, category_id: CategoryType) -> ProxyState {
    let port = state.proxy.port_of(category_id);
    let running = state.proxy.is_running(category_id);
    ProxyState {
        category_id,
        port,
        status: if running { "running".into() } else { "stopped".into() },
        message: None,
    }
}

#[tauri::command]
async fn start_proxy(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    category_id: CategoryType,
) -> AppResult<ProxyState> {
    let port = state.proxy.start(category_id).await?;
    // 托盘图标要反映「是否有代理在跑」（FR-022），启停都得刷一次。
    let _ = rebuild_tray(&app);
    Ok(ProxyState {
        category_id,
        port: Some(port),
        status: "running".into(),
        message: None,
    })
}

#[tauri::command]
fn stop_proxy(
    app: tauri::AppHandle,
    state: tauri::State<AppState>,
    category_id: CategoryType,
) -> AppResult<ProxyState> {
    state.proxy.stop(category_id);
    let _ = rebuild_tray(&app);
    Ok(ProxyState {
        category_id,
        port: None,
        status: "stopped".into(),
        message: None,
    })
}

/// 设置某分类代理的首选监听端口（粘滞固定端口）。
/// 持久化新端口 → 停当前代理 → 用新端口重启 → 重写该分类客户端 config（指向新端口）。
/// 端口是启动时绑定的，故改端口必须重启代理才生效；重写 config 让客户端下次读到新端口。
/// 客户端（Codex/Claude）需重启才会重读 config —— 但因端口从此固定，仅此一次。
#[tauri::command]
async fn set_proxy_port(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    category_id: CategoryType,
    port: u16,
) -> AppResult<ProxyState> {
    // 先落盘新首选端口，再重启代理使其按新端口绑定。
    state.store.set_proxy_port(category_id, port)?;
    let was_running = state.proxy.is_running(category_id);
    state.proxy.stop(category_id);
    let bound = state.proxy.start(category_id).await?;
    // 重写该分类客户端 config，指向实际绑定端口（可能因占用回退，用真实值）。
    let endpoint = format!("http://127.0.0.1:{bound}");
    // 模型列表必须与 apply_tool_config、GET /v1/models 完全同源——多 Key 取交集
    // （proxy::discoverable_models），而非主 Key 的 serviceable_models() 超集。否则改端口会把
    // 桌面端 gateway 档的 inferenceModels 从接入时的安全交集重写回超集，故障转移到备用 Key 后
    // 选中备用 Key 无法服务的模型必然 404（与 apply_tool_config 的修复自相矛盾）。
    let keys = state.store.enabled_keys_sorted(category_id);
    let models = crate::proxy::discoverable_models(&keys);
    if let Err(e) = tools::apply(category_id, &endpoint, &models, &keys) {
        state.store.append_event(
            category_id,
            "error",
            None,
            &format!("改端口后重写客户端配置失败: {e}"),
        );
    } else {
        state.store.append_event(
            category_id,
            "config",
            None,
            &format!("代理端口已改为 {bound}，已重写客户端配置（客户端需重启读取新端口）"),
        );
    }
    let _ = was_running;
    // 改端口会 stop→start，托盘菜单里那条「代理 · <端口>」标签要跟着更新。
    let _ = rebuild_tray(&app);
    Ok(ProxyState {
        category_id,
        port: Some(bound),
        status: "running".into(),
        message: None,
    })
}

/// 生成代理端点并写入目标工具配置（会先备份，dev-hard-rules）
#[tauri::command]
async fn apply_tool_config(
    state: tauri::State<'_, AppState>,
    category_id: CategoryType,
) -> AppResult<String> {
    apply_tool_config_for(&state.store, &state.proxy, category_id).await
}

/// 接入写盘的实际逻辑，供 IPC 命令与**托盘的代理开关**共用。
///
/// 为什么要抽出来：托盘启停代理必须与界面按钮语义完全一致——起代理即写工具配置、
/// 停代理即还原。若托盘只 `proxy.start()` 不写 config，客户端读到的仍是官方端点，
/// 用户会看到「托盘显示已启动，但 Claude/Codex 根本没走代理」。
///
/// 收 `Arc` 而非 `tauri::State`：托盘的菜单事件回调是同步闭包，要在 `async_runtime::spawn`
/// 里跨 await 使用，`State` 借用 `AppHandle` 无法跨过去。
async fn apply_tool_config_for(
    store: &Arc<Store>,
    proxy: &Arc<ProxyManager>,
    category_id: CategoryType,
) -> AppResult<String> {
    // 确保代理已启动
    let port = proxy.start(category_id).await?;
    let endpoint = format!("http://127.0.0.1:{port}");
    // 可服务对外名列表（三端语义不同，禁止混写）：
    // - Claude CLI：取首个写 env.ANTHROPIC_MODEL + 顶层 model（对外名；策略 A 不写 DEFAULT_*）
    // - Codex：取首个写 config.toml 的 model（OpenAI 形态，无 ANTHROPIC_*）
    // - 桌面端：整份写进 gateway 档的 inferenceModels（3p 部署模式，见 tools::apply_claude_desktop）
    //
    // 口径必须与 GET /v1/models 完全一致 —— 用 proxy::discoverable_models（多 Key 取交集），
    // 而非主 Key 的 serviceable_models()：否则桌面端选择器会列出备用 Key 无法服务的对外名，
    // 故障转移到备用 Key 后该模型必然 404。
    let keys = store.enabled_keys_sorted(category_id);
    let models = crate::proxy::discoverable_models(&keys);
    let msg = tools::apply(category_id, &endpoint, &models, &keys)?;
    store.append_event(category_id, "config", None, &format!("写入工具配置: {endpoint}"));
    // 桌面端不接受的对外名另记一条 error 事件：接入弹窗一关就没了，而这个问题的症状
    // （模型选择器为空 / ModelsNotDiscoveredError）往下要排查很久，得在运行日志里留痕。
    // 用 error 而非新增 warn kind：前端 LogsPage 的分组映射是穷举的，且这条本质是「配置不可用」。
    if category_id == CategoryType::ClaudeDesktop {
        let bad = service::desktop_unacceptable_models(&models);
        if !bad.is_empty() {
            store.append_event(
                category_id,
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
    }
    Ok(msg)
}

/// 只读预览：当前分类对应工具的配置路径 + 磁盘原文（token 脱敏）。
/// Claude CLI / 桌面 / Codex 路径与格式完全不同，前端按 category 展示，禁止混用字段名。
#[tauri::command]
fn get_tool_config_preview(category_id: CategoryType) -> AppResult<tools::ToolConfigPreview> {
    tools::preview(category_id)
}

#[tauri::command]
fn restore_tool_config(category_id: CategoryType) -> AppResult<String> {
    tools::restore(category_id)
}

// ============ 日志 & 设置命令 ============

#[tauri::command]
fn list_events(state: tauri::State<AppState>, category_id: CategoryType) -> Vec<EventLogEntry> {
    state.store.list_events(category_id)
}

#[tauri::command]
fn list_all_events(state: tauri::State<AppState>) -> Vec<EventLogEntry> {
    state.store.list_all_events()
}

/// 某分类「最近一次失败」（error/failover），供分类页顶部常驻提示条用（UX#11）。
///
/// 为什么单开一个命令而不让前端复用 `list_all_events` 自己筛：那个接口返回全部 500 条，
/// 而分类页 5s 轮询一次 —— 为了显示一行摘要搬 500 条是把 P1-6 刚省下的开销又花回去。
/// 这里只回一条（或 None），`fresh_ms` 由调用方指定「多新才算最近」。
///
/// 单位统一用**毫秒**，与 `Store::recent_failure` 及 `EventLogEntry.ts` 一致。
/// （此处原先叫 `fresh_secs` 却把值原样传给按毫秒解释的 store 方法 —— 前端传 300 秒
/// 会被当成 300 毫秒，任何超过 0.3 秒的失败都被判为「不够新」而永不显示。
/// 这类单位错配不会报错、只会让功能静默失效，故两侧命名必须一致。）
#[tauri::command]
fn recent_failure(
    state: tauri::State<AppState>,
    category_id: CategoryType,
    fresh_ms: i64,
) -> Option<EventLogEntry> {
    state.store.recent_failure(category_id, fresh_ms)
}

/// 按事件 id 取链路快照（用户展开某行日志时才调）。
///
/// 列表接口刻意剥掉了 trace 正文：日志页每 2s 全量轮询，而单条 trace 的请求/响应体各上限
/// 20000 字符，500 条满载约 19 MB —— 每 2s 搬一次会把界面拖卡，而这些正文只在用户展开
/// 某一行时才被看。故改为列表只带 `hasTrace` 布尔位、展开时按 id 单取。
///
/// 返回 None = 该条已被内存日志上限（MAX_EVENTS）挤出，前端提示「已滚出保留窗口」。
#[tauri::command]
fn get_event_trace(
    state: tauri::State<AppState>,
    event_id: String,
) -> Option<crate::model::RequestTrace> {
    state.store.event_trace(&event_id)
}

#[tauri::command]
fn get_settings(state: tauri::State<AppState>) -> AppSettings {
    state.store.get_settings()
}

/// 首启向导该不该显示（UX#1）。
///
/// `should_show` 的判据是「标记未完成 **且** 一条 Key 都没有」。加后一个条件是纵深防御：
/// 万一标记因为某种原因是 false 而用户其实已经配好了，也不该拿一个空向导挡住他。
#[tauri::command]
fn get_onboarding_state(state: tauri::State<AppState>) -> OnboardingState {
    // 两条独立语句：各自的锁随语句结束即释放，不会跨调用持有。
    let done = state.store.get_settings().onboarding_done.unwrap_or(false);
    let total_keys = state.store.total_key_count();
    OnboardingState {
        should_show: !done && total_keys == 0,
        done,
        total_keys,
        ccswitch_available: ccswitch::db_available(),
    }
}

#[tauri::command]
fn set_onboarding_done(state: tauri::State<AppState>, done: bool) -> AppResult<()> {
    state.store.set_onboarding_done(done)
}

/// 自某时刻起该分类有没有真的收到过转发请求（首启向导第④步的正反馈）。
#[tauri::command]
fn first_request_since(
    state: tauri::State<AppState>,
    category_id: CategoryType,
    since_ms: i64,
) -> FirstRequestProbe {
    state.store.first_request_since(category_id, since_ms)
}

/// 保存用户偏好。入参是白名单类型 `UserPrefs` —— 后端自管字段（粘滞端口、MCP 注册记录、
/// 已选模型、密钥库模式镜像、开机自启动…）在类型上就不存在，前端连表达「我要改它」都做不到。
///
/// 此前这里是黑名单：`Store::save_settings` 内部逐个字段保留后端值。那出过 P0 ——
/// `auto_start` 不在名单里，切主题/切语言会把用户刚关掉的开机自启动重新装回系统。
///
/// **开机自启动已移出本命令**，改走专用的 `set_auto_start`：它伴随系统副作用（写注册表），
/// 而批量保存路径的入参是前端挂载时的旧快照 —— 凡「落盘之外还要动系统」的开关都不该待在
/// 那条路径上。
#[tauri::command]
fn save_settings(state: tauri::State<AppState>, settings: UserPrefs) -> AppResult<()> {
    state.store.save_settings(settings)
}

/// 开机自启动开关（FR-025）。**专用命令**，不走批量保存。
///
/// 必须与系统状态同步落地，不能只存字段：此前只把 auto_start 写进 config.json、
/// 从无任何代码去注册自启动项，开关看起来生效了、重启后什么也不发生 ——
/// 一个静默失效的开关比「明确标注未完成」更坑。
///
/// 顺序：先动系统（可能失败），成功后才落盘；**落盘失败则把系统改回原状**。
/// 三种顺序都考虑过：
/// - 先落盘后动系统：系统失败时留下「配置说开、系统没开」，而用户看设置页是开着的，无从察觉。
/// - 先动系统后落盘、失败不回滚：留下「系统已注册、配置说关」，下次启动时 setup 的状态对账
///   会按配置把它关掉 —— 用户点开了、重启后又没了，同样无从察觉。
/// - 先动系统后落盘、失败回滚（当前）：两边始终一致。最坏是「回滚也失败」，那时只能记日志
///   并如实上报，但那已是两次系统调用都失败的极端情况。
///
/// **不用「配置值是否变化」决定要不要动系统**：若用户手改过注册表（或用清理工具删了启动项），
/// 配置说 true、系统实际 false，此时点「关」再点「开」——第二次点开时配置值已经是 true，
/// 按「没变」跳过就会两边都不动，开关**点不动**。插件本身幂等，故这里无条件同步。
#[tauri::command]
async fn set_auto_start(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    enabled: bool,
) -> AppResult<()> {
    let prev = state.store.get_settings().auto_start;
    sync_autostart(&app, enabled)?;
    let saved = state.store.set_auto_start_flag(enabled);
    if saved.is_err() {
        // 落盘失败：把系统改回**落盘前的配置值**（不是 !enabled ——
        // 幂等场景下那会把系统改成与配置相反）。
        // 回滚本身失败只记日志：原始错误（落盘失败）更重要，不该被回滚错误盖掉。
        if let Err(re) = sync_autostart(&app, prev) {
            tracing::error!(
                "自启动开关落盘失败后回滚系统状态也失败（系统侧现为 {enabled}、配置侧为 {prev}）: {re}"
            );
        }
    }
    saved
}

/// 注册自启动项时附加的启动参数：让被系统拉起的实例可自我识别。
///
/// 需求 FR-025 要求「开机自启动并**最小化到托盘**」。判据必须是这个参数，
/// **不能**用「`auto_start` 配置为真」——后者在用户手动双击时同样成立，那时把窗口藏起来
/// 会让人以为程序没启动。
const AUTOSTART_FLAG: &str = "--autostart";

/// 本次进程是否由「开机自启动」拉起（据启动参数判定）。
fn launched_by_autostart<I, S>(args: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    args.into_iter().any(|a| a.as_ref() == AUTOSTART_FLAG)
}

/// 把「开机自启动」开关同步到系统（FR-025）。
///
/// 走 `tauri-plugin-autostart`：Windows 下写/删注册表 `Run` 键，跨平台由插件适配。
/// 幂等——插件内部对「已启用再 enable」不报错。
///
/// 失败必须上抛（而非只记日志）：注册自启动项可能因权限、注册表被策略锁定而失败，
/// 静默吞掉会让用户以为开成了。上抛后前端会弹错误、开关不落盘，状态保持一致。
fn sync_autostart(app: &tauri::AppHandle, enable: bool) -> AppResult<()> {
    use tauri_plugin_autostart::ManagerExt;
    let mgr = app.autolaunch();
    let r = if enable { mgr.enable() } else { mgr.disable() };
    r.map_err(|e| {
        error::AppError::Other(format!(
            "{}开机自启动失败：{e}。可能是注册表被组策略锁定或权限不足。",
            if enable { "启用" } else { "关闭" }
        ))
    })
}

/// 设置某分类当前选定的「对外模型名」（应用内模型下拉专用）。
/// 走后端专用写入而非 save_settings —— 后者会被前端携带的陈旧 settings 快照顶回
/// （与 mcp_* 同一保全策略）。空串清除该分类选择，回到「透传客户端发来的模型名」。
/// 每请求实时读取，改选即时生效、免重启客户端。
#[tauri::command]
async fn set_active_model(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    category_id: CategoryType,
    model: String,
) -> AppResult<()> {
    state.store.set_active_model(category_id, &model)?;
    // 主窗口下拉改选后，同步刷新托盘子菜单的勾选态（托盘菜单静态构建，需主动重建）。
    let _ = rebuild_tray(&app);
    Ok(())
}

/// 设置某分类的「默认推理强度」（方案 A，Codex 专用）。
/// Codex 对自定义 provider 不发 reasoning.effort，故由此配置在转发时补进请求体。
/// 取值 minimal/low/medium/high/xhigh；空串或 "off" 清除（回到不注入、保持现状）。
/// 后端自管字段，走专用命令直写，不随 save_settings 的陈旧快照覆盖。
#[tauri::command]
async fn set_active_effort(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    category_id: CategoryType,
    effort: String,
) -> AppResult<()> {
    state.store.set_active_effort(category_id, &effort)?;
    // 主窗口改推理强度后，同步刷新托盘子菜单勾选态（与 set_active_model 对称，托盘菜单静态构建需主动重建）。
    let _ = rebuild_tray(&app);
    Ok(())
}

/// 重建托盘菜单：主窗口里改了 Key 模型列表 / 切了 active_model / 改了托盘开关后，
/// 前端调此命令让托盘子菜单候选与勾选态跟最新数据一致（托盘菜单不自动跟数据变）。
#[tauri::command]
async fn rebuild_tray_menu(app: tauri::AppHandle) -> AppResult<()> {
    let _ = rebuild_tray(&app);
    Ok(())
}

// ============ MCP 服务器 ============

/// MCP 服务器运行状态（供设置页展示连接指示灯）
#[tauri::command]
fn mcp_status(state: tauri::State<AppState>) -> McpStatus {
    McpStatus {
        running: state.mcp.is_running(),
        port: state.mcp.running_port(),
        last_error: state.mcp.last_error(),
    }
}



/// 把 synaroute MCP 注册进指定分类对应工具的客户端配置，并把该分类记进
/// settings.mcp_registered_categories（去重）。写盘/跳过都记一条事件日志，方便用户排查。
fn register_and_record(state: &AppState, category: CategoryType, port: u16) {
    let url = service::mcp_url_for(port);
    let timeout_ms = service::mcp_client_timeout_ms(&state.store);
    match tools::register_mcp_client(category, &url, timeout_ms) {
        Ok((msg, _wrote)) => {
            state.store.append_event(category, "config", None, &msg);
            // 走后端专用写入（不能用 get_settings→push→save_settings，会被 save_settings
            // 的 mem::take 保留逻辑吞掉，导致该集合永远为空）。
            let _ = state.store.add_registered_category(category);
        }
        Err(e) => {
            state.store.append_event(
                category,
                "error",
                None,
                &format!("MCP 自动注册到客户端失败: {e}"),
            );
        }
    }
}

/// 端口漂移后，用新端口重写所有已注册分类的客户端配置（url 里的端口跟着变）。
fn rewrite_registered_clients(state: &AppState, port: u16) {
    let url = service::mcp_url_for(port);
    let timeout_ms = service::mcp_client_timeout_ms(&state.store);
    let cats = state.store.get_settings().mcp_registered_categories;
    for category in cats {
        {
            match tools::register_mcp_client(category, &url, timeout_ms) {
                Ok((msg, wrote)) => {
                    if wrote {
                        state.store.append_event(category, "config", None, &msg);
                    }
                }
                Err(e) => state.store.append_event(
                    category,
                    "error",
                    None,
                    &format!("MCP 端口变化后重写客户端失败: {e}"),
                ),
            }
        }
    }
}

/// 单分类接入 MCP 大脑聚合：只给指定分类写入客户端 MCP 配置（Claude CLI=~/.claude.json，
/// Codex=config.toml，桌面端=claude_desktop_config），不影响其它分类。
/// 与 set_mcp_enabled 的区别：后者是全局开关且只认「当前活跃分类」，做不到多端同时接入；
/// 本命令 per-category 独立，可让 CLI 与 Codex 各自接入、互不干扰。
/// 前置：MCP 服务必须在运行（未运行则先按首选端口启动），否则客户端写了地址也连不上。
#[tauri::command]
async fn register_mcp_for_category(
    state: tauri::State<'_, AppState>,
    category_id: CategoryType,
) -> AppResult<McpStatus> {
    // 确保服务在跑：未运行则以首选端口启动（复用粘滞端口逻辑）。
    let bound = match state.mcp.running_port() {
        Some(p) => p,
        None => {
            let port = state.store.get_settings().mcp_port;
            match state.mcp.start(port).await {
                Ok(b) => {
                    // 启动即视为 MCP 开启，持久化 enabled=true 并粘住实际端口。
                    let _ = state.store.set_mcp_enabled_flag(true);
                    if b != port {
                        rewrite_registered_clients(&state, b);
                        let _ = state.store.set_mcp_port(b);
                    }
                    b
                }
                Err(e) => {
                    state.store.append_event(
                        category_id,
                        "error",
                        None,
                        &format!("MCP 启动失败，无法接入 {}: {e}", category_id.as_str()),
                    );
                    return Err(error::AppError::Proxy(e));
                }
            }
        }
    };
    // 只给这一个分类注册 + 记入已注册集合（供端口漂移时重写、关闭时注销）。
    register_and_record(&state, category_id, bound);
    Ok(McpStatus {
        running: state.mcp.is_running(),
        port: state.mcp.running_port(),
        last_error: state.mcp.last_error(),
    })
}

/// 单分类断开 MCP 大脑聚合：只从指定分类的客户端配置移除 synaroute，并从已注册集合剔除。
/// 不停 MCP 服务、不动其它分类——与 register_mcp_for_category 对称。
#[tauri::command]
async fn unregister_mcp_for_category(
    state: tauri::State<'_, AppState>,
    category_id: CategoryType,
) -> AppResult<McpStatus> {
    match tools::unregister_mcp_client(category_id) {
        Ok((msg, wrote)) => {
            if wrote {
                state.store.append_event(category_id, "config", None, &msg);
            }
        }
        Err(e) => {
            state.store.append_event(
                category_id,
                "error",
                None,
                &format!("MCP 断开失败: {e}"),
            );
            return Err(e);
        }
    }
    let _ = state.store.remove_registered_category(category_id);
    Ok(McpStatus {
        running: state.mcp.is_running(),
        port: state.mcp.running_port(),
        last_error: state.mcp.last_error(),
    })
}

/// 启用/停用 MCP 并自动注册到「当前活跃分类」对应的工具。
/// 前端 MCP 开关走这里（携带 activeCategory），而非通用 save_settings。
#[tauri::command]
async fn set_mcp_enabled(
    state: tauri::State<'_, AppState>,
    category_id: CategoryType,
    enabled: bool,
    port: u16,
) -> AppResult<McpStatus> {
    // 端口候选先落盘（用户在设置里改端口需要保留候选值，即便本次启动失败）；
    // enabled 落盘时序按启动结果决定：先写 true 再启动会导致「服务没起来但配置说开着」
    // 的状态错乱，前端下次冷启会以为服务已在跑却 mcp_status 显示 stopped。
    state.store.set_mcp_port(port)?;
    if enabled {
        match state.mcp.start(port).await {
            Ok(bound) => {
                // 启动成功后才把开关置 true，UI/持久化状态与实际服务一致。
                state.store.set_mcp_enabled_flag(true)?;
                // 实际绑定端口可能因占用而回退；用它注册，保证客户端地址与真实端口一致。
                register_and_record(&state, category_id, bound);
                // 若端口漂移，其它已注册分类也要跟着改，并把 bound 粘为下次首选端口
                // （否则每次启动都从被占的旧端口重新回退、重写配置——治标不治本）。
                if bound != port {
                    rewrite_registered_clients(&state, bound);
                    let _ = state.store.set_mcp_port(bound);
                }
            }
            Err(e) => {
                // 启动失败：回滚 enabled=false，让 UI/持久化如实反映当前无服务。
                let _ = state.store.set_mcp_enabled_flag(false);
                state.store.append_event(
                    category_id,
                    "error",
                    None,
                    &format!("MCP 启动失败（enabled 已回滚为 false）: {e}"),
                );
                tracing::warn!("MCP 服务器启动失败: {e}");
            }
        }
    } else {
        state.mcp.stop();
        // 先把开关落成 false（服务已停，配置立刻反映）。
        state.store.set_mcp_enabled_flag(false)?;
        // 关闭开关：从已注册分类移除 synaroute，并清空记录。
        let cats = state.store.get_settings().mcp_registered_categories.clone();
        // mcp_registered_categories 现在直接是 Vec<CategoryType>（P2-8）——
        // 不再需要「字符串解析 + 解析失败静默跳过」那一层，漏配一个分类从此是编译错误。
        for &category in &cats {
            {
                match tools::unregister_mcp_client(category) {
                    Ok((msg, wrote)) => {
                        if wrote {
                            state.store.append_event(category, "config", None, &msg);
                        }
                    }
                    Err(e) => state.store.append_event(
                        category,
                        "error",
                        None,
                        &format!("MCP 注销失败: {e}"),
                    ),
                }
            }
        }
        // 已注册分类记录走后端专用清空（save_settings 不再触碰该字段）。
        state.store.clear_registered_categories()?;
    }
    Ok(McpStatus {
        running: state.mcp.is_running(),
        port: state.mcp.running_port(),
        last_error: state.mcp.last_error(),
    })
}

/// 手动重启 MCP 服务：先停后起（强制重新绑定端口），并重新注入客户端配置。
/// 用途：改了端口后立即重绑、端口冲突排障、或客户端连不上时强制重连。
/// 注意：大脑聚合参数（超时/Token/成员/决策者）是每次调用实时从配置读的，
/// 改了保存即生效，无需重启——本命令只影响 MCP 服务本身的监听与客户端 url 同步。
#[tauri::command]
async fn restart_mcp(state: tauri::State<'_, AppState>) -> AppResult<McpStatus> {
    let settings = state.store.get_settings();
    let port = settings.mcp_port;
    // 1) 先停旧监听（abort accept 循环）
    state.mcp.stop();
    state.store.append_event(
        CategoryType::ClaudeCli,
        "config",
        None,
        &format!("MCP 重启：已停止旧服务，准备绑定端口 {port}"),
    );
    // 2) 再起新监听
    match state.mcp.start(port).await {
        Ok(bound) => {
            // 端口回退时粘住
            if bound != port {
                let _ = state.store.set_mcp_port(bound);
            }
            // 3) 无论端口是否变化，都重新注入已注册分类的客户端配置（url + timeout），
            //    保证 ~/.claude.json / config.toml 与真实绑定端口一致。
            rewrite_registered_clients(&state, bound);
            // 若还没有任何已注册分类，但开关是开的：按默认分类注入一次，避免「服务在跑但客户端没配置」。
            if state
                .store
                .get_settings()
                .mcp_registered_categories
                .is_empty()
            {
                register_and_record(&state, CategoryType::ClaudeCli, bound);
            }
            state.store.append_event(
                CategoryType::ClaudeCli,
                "config",
                None,
                &format!("MCP 重启完成：http://127.0.0.1:{bound}/mcp（已重写客户端配置）"),
            );
        }
        Err(e) => {
            tracing::warn!("MCP 重启失败: {e}");
            state.store.append_event(
                CategoryType::ClaudeCli,
                "error",
                None,
                &format!("MCP 重启失败: {e}"),
            );
        }
    }
    Ok(McpStatus {
        running: state.mcp.is_running(),
        port: state.mcp.running_port(),
        last_error: state.mcp.last_error(),
    })
}

// ============ 版本与更新 ============

#[tauri::command]
fn get_app_version(app: tauri::AppHandle) -> String {
    app.package_info().version.to_string()
}



#[tauri::command]
async fn check_for_updates(app: tauri::AppHandle) -> AppResult<service::UpdateCheckResult> {
    use tauri_plugin_updater::UpdaterExt;
    let current_version = app.package_info().version.to_string();
    let updater = match app.updater() {
        Ok(u) => u,
        Err(e) => {
            return Ok(service::UpdateCheckResult {
                status: "error".into(),
                current_version,
                version: None,
                notes: None,
                error: Some(service::friendly_updater_error(&e.to_string())),
            });
        }
    };
    match updater.check().await {
        Ok(Some(u)) => Ok(service::UpdateCheckResult {
            status: "available".into(),
            current_version,
            version: Some(u.version.clone()),
            notes: u.body.clone(),
            error: None,
        }),
        Ok(None) => Ok(service::UpdateCheckResult {
            status: "up_to_date".into(),
            current_version,
            version: None,
            notes: None,
            error: None,
        }),
        Err(e) => Ok(service::UpdateCheckResult {
            // 不抛 Err：前端永远能渲染结果，避免仅靠 catch 字符串猜原因
            status: "error".into(),
            current_version,
            version: None,
            notes: None,
            error: Some(service::friendly_updater_error(&e.to_string())),
        }),
    }
}

/// 下载并安装已检测到的更新（需先 check 到 available）。安装后由插件触发重启。
#[tauri::command]
async fn install_update(app: tauri::AppHandle) -> AppResult<String> {
    use tauri_plugin_updater::UpdaterExt;
    let updater = app
        .updater()
        .map_err(|e| error::AppError::Other(format!("{e}")))?;
    let update = updater
        .check()
        .await
        .map_err(|e| error::AppError::Other(service::friendly_updater_error(&e.to_string())))?
        .ok_or_else(|| error::AppError::Other("当前已是最新版本，无需安装".into()))?;
    let ver = update.version.clone();
    update
        .download_and_install(|_chunk, _total| {}, || {})
        .await
        .map_err(|e| error::AppError::Other(format!("下载/安装更新失败: {e}")))?;
    Ok(format!("已安装 v{ver}，请重启应用完成更新"))
}

// ============ 文件目录选择 ============

#[tauri::command]
async fn pick_directory(app: tauri::AppHandle) -> Option<String> {
    use tauri_plugin_dialog::DialogExt;
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog().file().pick_folder(move |path| {
        let _ = tx.send(path.map(|p| p.to_string()));
    });
    rx.await.ok().flatten()
}

#[tauri::command]
fn get_default_log_dir() -> String {
    store::default_log_dir().to_string_lossy().into_owned()
}

/// 当前**实际生效**的日志目录（用户配了就用用户的，否则默认目录）。
///
/// 与 `get_default_log_dir` 的区别很重要：后者只给默认值，而用户可能改过。
/// 「打开日志目录」按钮必须打开真正在写的那个，否则用户对着空目录找不到日志。
#[tauri::command]
fn get_effective_log_dir(state: tauri::State<AppState>) -> String {
    state.store.effective_log_dir().to_string_lossy().into_owned()
}

/// 导出诊断报告（UX#12）：把排障要用的东西汇成**一个纯文本文件**，供用户报障时附上。
///
/// **为什么是纯文本而不是 zip**（docs/15 原建议 zip）：
/// 1. 用户能在发出前**亲眼看清里面没有密钥**——这直接决定他敢不敢发。zip 里的东西看不见，
///    要额外解释「我保证脱敏了」，信任成本高得多；
/// 2. 不引入 `zip` 直接依赖；
/// 3. 报障场景下贴一段文本比传附件更顺手。
///
/// **绝不包含**：任何密钥明文（config 走 `redact_config_secrets` 脱敏）、
/// trace 正文（调用模型日志的请求/响应体，可达数万字符且含完整对话）。
/// 头部显式列出「包含什么、不含什么」，让用户不必逐行审也能判断。
#[tauri::command]
async fn export_diagnostics(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> AppResult<Option<String>> {
    use tauri_plugin_dialog::DialogExt;
    use std::fmt::Write as _;

    let store = &state.store;
    let mut r = String::with_capacity(16 * 1024);

    let _ = writeln!(r, "# SynaRoute 诊断报告");
    let _ = writeln!(r, "生成时间：{}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S %:z"));
    let _ = writeln!(r);
    let _ = writeln!(r, "## 本文件包含什么");
    let _ = writeln!(r, "- 版本、运行环境、各路径（供核对 MSIX 虚拟化导致的「平行宇宙」问题）");
    let _ = writeln!(r, "- 配置（**已脱敏**：所有密钥字段替换为 ***）");
    let _ = writeln!(r, "- 各 Key 的健康状态与代理运行状态");
    let _ = writeln!(r, "- 最近的事件日志摘要");
    let _ = writeln!(r);
    let _ = writeln!(r, "## 本文件**不**包含");
    let _ = writeln!(r, "- 任何 API 密钥明文");
    let _ = writeln!(r, "- 对话正文（「调用模型日志」的请求体/响应体一律不含）");
    let _ = writeln!(r);

    // ---- 环境与路径 ----
    let _ = writeln!(r, "## 环境");
    let _ = writeln!(r, "- 应用版本：{}", app.package_info().version);
    let _ = writeln!(r, "- 操作系统：{} {}", std::env::consts::OS, std::env::consts::ARCH);
    let _ = writeln!(
        r,
        "- 当前 exe：{}",
        std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "?".into())
    );
    // 路径是 MSIX 虚拟化问题的关键证据：用户双击启动与被包内进程启动看到的是不同副本。
    let _ = writeln!(r, "- 配置文件：{}", store.config_path_display());
    let _ = writeln!(r, "- 日志目录：{}", store.effective_log_dir().display());
    let _ = writeln!(r, "- 丢弃日志条数（队列满/磁盘慢）：{}", store.log_dropped_count());
    // 状态推送也可能丢（队列满）。丢了不影响正确性——前端 30s 兜底轮询会追上——
    // 但「界面偶尔慢半拍」的排障线索就在这个数字里，必须能被问到。
    let _ = writeln!(r, "- 丢弃状态推送数（队列满）：{}", events::dropped_count());
    let _ = writeln!(r);

    // ---- 代理与 Key 状态 ----
    let _ = writeln!(r, "## 代理状态");
    for cat in CategoryType::ALL {
        let _ = writeln!(
            r,
            "- {}: {} 端口={:?}",
            cat.as_str(),
            if state.proxy.is_running(cat) { "running" } else { "stopped" },
            state.proxy.port_of(cat)
        );
    }
    let _ = writeln!(r);

    let _ = writeln!(r, "## Key 健康状态（不含密钥）");
    for cat in CategoryType::ALL {
        let keys = store.list_keys(cat);
        if keys.is_empty() {
            continue;
        }
        let _ = writeln!(r, "### {}", cat.as_str());
        for k in keys {
            let _ = writeln!(
                r,
                "- [{}] {} | 协议={:?} | 优先级={} | 启用={} | 有密钥={} | 状态={:?} 失败计数={} 熔断至={:?} 延迟={:?}ms | 模型数={} 映射数={}",
                k.id,
                k.name,
                k.protocol,
                k.priority,
                k.enabled,
                k.has_secret,
                k.health.status,
                k.health.fail_count,
                k.health.breaker_until,
                k.health.latency_ms,
                k.models.len(),
                k.mappings.len()
            );
            // base_url 单列一行：它常是问题根源（协议选错、路径写错），但不含密钥，可以给。
            let _ = writeln!(r, "  base_url: {}", k.base_url);
        }
    }
    let _ = writeln!(r);

    // ---- 脱敏后的配置 ----
    let _ = writeln!(r, "## 配置（已脱敏）");
    let _ = writeln!(r, "```json");
    match store.redacted_config_json() {
        Ok(s) => {
            let _ = writeln!(r, "{s}");
        }
        Err(e) => {
            let _ = writeln!(r, "（读取配置失败：{e}）");
        }
    }
    let _ = writeln!(r, "```");
    let _ = writeln!(r);

    // ---- 最近事件（不含 trace 正文）----
    const MAX_EVENTS_IN_REPORT: usize = 200;
    let events = store.list_all_events();
    let total = events.len();
    let _ = writeln!(
        r,
        "## 最近事件（共 {total} 条，取最后 {}；**不含**调用模型日志的请求/响应正文）",
        MAX_EVENTS_IN_REPORT.min(total)
    );
    for e in events.iter().rev().take(MAX_EVENTS_IN_REPORT).rev() {
        let ts = chrono::DateTime::from_timestamp_millis(e.ts)
            .map(|d| d.with_timezone(&chrono::Local).format("%m-%d %H:%M:%S").to_string())
            .unwrap_or_else(|| e.ts.to_string());
        let _ = writeln!(
            r,
            "[{ts}] {} {} {}{}",
            e.category_id.as_str(),
            e.kind,
            e.detail,
            if e.repeat > 1 { format!(" (×{})", e.repeat) } else { String::new() }
        );
    }

    let default_name = format!(
        "synaroute-diagnostics-{}.txt",
        chrono::Local::now().format("%Y%m%d-%H%M%S")
    );
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_title("保存 SynaRoute 诊断报告")
        .set_file_name(&default_name)
        .add_filter("文本文件", &["txt"])
        .save_file(move |p| {
            let _ = tx.send(p);
        });
    let Some(path) = rx.await.ok().flatten() else {
        return Ok(None); // 用户取消，不算错误
    };
    let path = path.to_string();
    std::fs::write(&path, r.as_bytes())
        .map_err(|e| error::AppError::Other(format!("写入诊断报告失败（{path}）: {e}")))?;
    Ok(Some(path))
}

/// 准备「打开日志目录」（UX#13）：确保目录存在并返回其绝对路径，由**前端**调 shell 插件打开。
///
/// 为什么后端不直接开：项目既有做法是前端 `@tauri-apps/plugin-shell` 的 `open`
/// （见 AboutPage 打开外链那处的注释），且后端的 `shell().open` 已被标记废弃。
/// 保持单一做法，避免两套外部打开路径。
///
/// 目录先创建：一条日志都没写过时它还不存在，直接交给资源管理器会报「找不到路径」。
///
/// ⚠️ **MSIX 虚拟化注意**：返回的是**当前进程视角**的路径。若 SynaRoute 由有包身份的进程
/// （如 Claude Code）启动，`%APPDATA%` 会被重定向到包内私有副本，用户看到的将是那份虚拟副本
/// 而非双击启动时的真实目录。故 UI 必须**同时显示这个绝对路径全文**（见设置页），
/// 让用户能核对自己看的是哪一份——这是 CLAUDE.md 里「平行宇宙」惨案的复发防线。
#[tauri::command]
fn prepare_log_dir(state: tauri::State<AppState>) -> AppResult<String> {
    let dir = state.store.effective_log_dir();
    std::fs::create_dir_all(&dir).map_err(|e| {
        error::AppError::Other(format!("创建日志目录失败（{}）: {e}", dir.display()))
    })?;
    Ok(dir.to_string_lossy().into_owned())
}

// ============ 配置导入 / 导出（FR-021）============
//
// 导出体不含 DPAPI 密文——那玩意儿绑当前 Windows 账户、换机解不出。含密钥导出时改用
// 用户口令重新加密（Argon2id + AES-GCM，见 crate::crypto）。详见 crate::portable 模块注释。

/// 导出结果。
///
/// **必须带上 `undecryptable`**：解不出的 Key 是被跳过而非导出失败（见
/// [`portable::build_export`]）。若只回一个路径，用户会拿到「声称含密钥、实际少几条」的文件，
/// 到新机器导入后才发现——那时已经离开源机器、无从补救。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ExportOutcome {
    path: String,
    /// 密钥解不出、已跳过的 Key 数（0 表示全部带上了）
    undecryptable: usize,
}

/// 导出配置到用户选定的文件。`password` 非空则包含密钥段（口令加密）。
/// 返回实际写入的路径 + 跳过条数；用户取消选择时返回 None。
#[tauri::command]
async fn export_config(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    password: Option<String>,
) -> AppResult<Option<ExportOutcome>> {
    use tauri_plugin_dialog::DialogExt;
    // 空串视为「不含密钥」：前端未勾选时可能传空串而非 null，两者语义应当一致。
    let pw = password.as_deref().filter(|s| !s.is_empty());
    // 先构建再弹框：构建可能失败（如密钥库损坏），失败时不该已经让用户挑好了文件。
    let (file, undecryptable) = portable::build_export(
        &state.store,
        app.package_info().version.to_string().as_str(),
        pw,
    )?;
    let data = serde_json::to_vec_pretty(&file)?;

    let default_name = format!(
        "synaroute-config-{}.json",
        chrono::Local::now().format("%Y%m%d-%H%M%S")
    );
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_title("导出 SynaRoute 配置")
        .set_file_name(&default_name)
        .add_filter("SynaRoute 配置", &["json"])
        .save_file(move |p| {
            let _ = tx.send(p);
        });
    let Some(path) = rx.await.ok().flatten() else {
        return Ok(None); // 用户取消，不算错误
    };
    let path = path
        .into_path()
        .map_err(|e| error::AppError::Other(format!("解析保存路径失败: {e}")))?;
    // 走 atomic_write：与配置落盘同一套（含跨设备 rename 回退），避免半写文件。
    secret::atomic_write(&path, &data)?;
    state.store.append_event(
        CategoryType::ClaudeCli,
        "config",
        None,
        &format!(
            "已导出配置到 {}（{}密钥{}）",
            path.display(),
            if pw.is_some() { "含" } else { "不含" },
            if undecryptable > 0 {
                format!("，{undecryptable} 条密钥解不出已跳过")
            } else {
                String::new()
            }
        ),
    );
    Ok(Some(ExportOutcome {
        path: path.display().to_string(),
        undecryptable,
    }))
}

/// 让用户选一个导出文件并**只做校验与预检**（不改任何配置）。
/// 返回 (文件路径, 预检信息)；用户取消时返回 None。
#[tauri::command]
async fn pick_and_preview_import(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> AppResult<Option<(String, portable::ImportPreview)>> {
    use tauri_plugin_dialog::DialogExt;
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_title("选择要导入的 SynaRoute 配置")
        .add_filter("SynaRoute 配置", &["json"])
        .pick_file(move |p| {
            let _ = tx.send(p);
        });
    let Some(path) = rx.await.ok().flatten() else {
        return Ok(None);
    };
    let path = path
        .into_path()
        .map_err(|e| error::AppError::Other(format!("解析文件路径失败: {e}")))?;
    let raw = std::fs::read(&path)?;
    // 校验（版本 + sha256）放在这里：让用户在**点确认之前**就知道文件是不是好的。
    let file = portable::parse_and_verify(&raw)?;
    let preview = portable::preview_import(&state.store, &file);
    Ok(Some((path.display().to_string(), preview)))
}

/// 执行导入。`path` 来自 `pick_and_preview_import`；`mode` 由用户当场选。
///
/// 刻意重新读盘 + 重新校验，而不是缓存预检时的解析结果：预检与确认之间用户可能改动了文件，
/// 且缓存跨 IPC 调用要么塞进全局状态（多了一处可变共享态）要么把整份配置回传前端再传回来
/// （白绕一圈、还多一个被篡改的机会）。重新读一次几毫秒，换来「校验的就是即将导入的字节」。
#[tauri::command]
async fn apply_import_config(
    state: tauri::State<'_, AppState>,
    path: String,
    mode: portable::ImportMode,
    password: Option<String>,
) -> AppResult<portable::ImportReport> {
    let raw = std::fs::read(&path)?;
    let file = portable::parse_and_verify(&raw)?;
    let pw = password.as_deref().filter(|s| !s.is_empty());
    let report = portable::apply_import(&state.store, &file, mode, pw)?;
    state.store.append_event(
        CategoryType::ClaudeCli,
        "config",
        None,
        &format!(
            "已导入配置（{:?}）：新增 {} / 覆盖 {} / 删除 {} 个 Key，密钥 {} 条{}",
            report.mode,
            report.keys_added,
            report.keys_overwritten,
            report.keys_removed,
            report.secrets_imported,
            // 清理掉的旧密钥要写进事件日志：密钥是敏感材料，删了几条应当留痕。
            if report.secrets_pruned > 0 {
                format!("，清理随 Key 一并移除的旧密钥 {} 条", report.secrets_pruned)
            } else {
                String::new()
            }
        ),
    );
    Ok(report)
}

/// 统计孤儿密钥条数（P2-3）：密钥库里有、但配置里已无对应 Key 的残留。
///
/// 只读命令。用于在设置页告知用户「检测到 N 条可清理的旧密钥」，由用户点确认再执行
/// [`prune_orphan_secrets`] —— 刻意不做启动时静默清理：删密钥不可逆，
/// 而残留孤儿本身是无害的（只占空间），不值得用「自动删」去换那点整洁。
#[tauri::command]
fn count_orphan_secrets(state: tauri::State<AppState>) -> usize {
    state.store.count_orphan_secrets()
}

/// 清理孤儿密钥（P2-3）。**破坏性操作**：编排（先备份再删、备份失败即放弃）在
/// [`service::prune_orphan_secrets`]。
#[tauri::command]
fn prune_orphan_secrets(state: tauri::State<AppState>) -> AppResult<usize> {
    service::prune_orphan_secrets(&state.store)
}

// ============ 厂商预设命令 ============

#[tauri::command]
fn list_vendors(state: tauri::State<AppState>) -> Vec<Vendor> {
    state.store.list_vendors()
}

#[tauri::command]
fn upsert_vendor(state: tauri::State<AppState>, vendor: Vendor) -> AppResult<Vendor> {
    state.store.upsert_vendor(vendor)
}

#[tauri::command]
fn delete_vendor(state: tauri::State<AppState>, vendor_id: String) -> AppResult<()> {
    state.store.delete_vendor(&vendor_id)
}

// ============ 大脑聚合 V2 ============

/// Phase1: 文件检索 + 参与者思考 + 决策者输出计划
#[tauri::command]
async fn aggregate_plan(
    state: tauri::State<'_, AppState>,
    category_id: CategoryType,
    prompt: String,
) -> AppResult<model::AggregateResult> {
    aggregate::run_plan(&state.store, category_id, &prompt).await
}

/// Phase2: 用户确认计划后，决策者执行修改。
/// work_dir 由 Phase1 的返回结果回传，锁定工作目录避免 auto-follow 漂移。
#[tauri::command]
async fn aggregate_execute(
    state: tauri::State<'_, AppState>,
    category_id: CategoryType,
    prompt: String,
    confirmed_plan: String,
    work_dir: Option<String>,
) -> AppResult<model::AggregateResult> {
    aggregate::run_apply(&state.store, category_id, &prompt, &confirmed_plan, work_dir).await
}

/// 检索文件（供前端预览用）
#[tauri::command]
async fn retrieve_files(
    work_dir: String,
    query: String,
    max_tokens: Option<u32>,
) -> AppResult<Vec<retrieval::RetrievedFile>> {
    retrieval::retrieve(&work_dir, &query, max_tokens.unwrap_or(50_000)).await
}

/// 检测其他 AI 工具中最近使用的项目目录（Claude CLI + Codex），按最近使用时间排序。
#[tauri::command]
fn detect_recent_workdirs() -> AppResult<Vec<workdirs::RecentWorkdir>> {
    workdirs::scan()
}

/// 检测 codegraph 可用状态（未安装 / 孤岛 / 未索引 / 就绪）。
/// `work_dir` 为空则只判可执行是否就绪，不判项目索引。
#[tauri::command]
async fn detect_codegraph(work_dir: Option<String>) -> codegraph::CodegraphState {
    let dir = work_dir
        .as_deref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(std::path::PathBuf::from);
    codegraph::detect(dir.as_deref()).await
}

/// 为指定项目建立 codegraph 索引（`codegraph init <path>`）。
/// 会在项目根创建 `.codegraph/`（SQLite 索引，纯本地、不出网）。
#[tauri::command]
async fn codegraph_init(work_dir: String) -> AppResult<String> {
    codegraph::init_project(std::path::Path::new(&work_dir))
        .await
        .map_err(error::AppError::Invalid)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Codex 大脑聚合走 stdio MCP：Codex 以子进程拉起 `synaroute.exe --mcp-stdio`，
    // 用 stdin/stdout 传 JSON-RPC。此模式不启动 Tauri UI、不开窗口、不监听端口，
    // 读到 stdin EOF（Codex 结束子进程）即退出。必须最先判定，早于任何 UI 初始化。
    if std::env::args().any(|a| a == "--mcp-stdio") {
        // 转发模式：不初始化 Store（避免读到 MSIX 虚拟化的错误配置宇宙——被 Codex 桌面端等
        // 打包应用拉起时，子进程读的是包容器里的空/旧配置）。tools/call 转发到运行中主应用的
        // HTTP MCP 端口，由持有真实配置的主应用执行聚合。TCP 端口不受 MSIX 虚拟化影响。
        let rt = tokio::runtime::Runtime::new().expect("tokio rt");
        rt.block_on(mcp::run_stdio());
        return;
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let store = Arc::new(Store::init().expect("初始化配置失败"));
    // 永久启动自检：每个实例自报「实际配置路径 / keys 数 / 用户 / exe 路径」到共享日志。
    // 背景：曾出现同一 exe 在不同启动方式下读到不同配置世代（UI 4 条 vs 磁盘 6 条），
    // 此日志让任意实例可肉眼对比运行环境差异，杜绝该类问题再变成无头案。
    {
        let (cfg_path, key_count) = store.config_fingerprint();
        store.append_event(
            CategoryType::ClaudeCli,
            "system",
            None,
            &format!(
                "启动自检 · 配置={cfg_path} · keys={key_count} · 用户={} · exe={}",
                std::env::var("USERNAME").unwrap_or_else(|_| "?".into()),
                std::env::current_exe()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|_| "?".into()),
            ),
        );
    }
    let proxy = Arc::new(ProxyManager::new(store.clone()));
    let mcp = Arc::new(McpManager::new(store.clone()));

    // 注意：MCP 自启动移到下方 setup() 里、跑在 Tauri 托管的异步运行时上。
    // 早期版本在此处用 std::thread + 临时 Runtime + block_on(mcp.start())：start() 只 spawn
    // accept 循环便返回，block_on 随即结束、临时 Runtime 被 drop → accept 任务连同监听器一起
    // 被取消，端口不再监听，但状态却显示 running（自启动的 MCP 形同虚设）。改用 Tauri 运行时
    // （生命周期与应用一致）后 accept 循环得以长存。

    // 后台定时健康检查（arch-decisions §6）。间隔由用户配置（AppSettings.health_check_interval_secs，
    // 默认 60s），每轮结束后重新读取最新配置，改设置即时生效、无需重启。设 10s 下限防误配把上游打爆。
    {
        let store_bg = store.clone();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().expect("tokio rt");
            rt.block_on(async move {
                const MIN_INTERVAL_SECS: u64 = 10;
                // 间隔为 0 = 用户关闭定时健康检查（有的用户不需要后台探测）。此时不探测，
                // 只按此固定节奏轮询配置，用户在设置里改回非 0 档后最多这么久即恢复自动探测。
                const DISABLED_POLL_SECS: u64 = 30;
                loop {
                    // 健康态合并落盘（P1-3 后半）：转发热路径的 record_live_* 只标脏，
                    // 真正的 20KB 序列化 + atomic_write 挪到这个后台线程上做。
                    //
                    // 放在这里（而非另起一个线程）是因为本循环已经是「周期性、可容忍延迟、
                    // 不在请求路径上」的现成载体；另起线程只是多一个要管生命周期的东西。
                    // 注意它在 `interval == 0`（用户关闭定时探测）时也必须继续执行——
                    // 熔断态由**真实流量**驱动，与探测开关无关，否则关掉探测就永不落盘了。
                    store_bg.flush_health_if_dirty();

                    let interval = store_bg.get_settings().health_check_interval_secs;
                    if interval == 0 {
                        // 关闭：跳过本轮探测（保留各 Key 现有健康态，不改动），仅轮询配置等待重新开启。
                        tokio::time::sleep(std::time::Duration::from_secs(DISABLED_POLL_SECS)).await;
                        continue;
                    }
                    let period =
                        std::time::Duration::from_secs(interval.max(MIN_INTERVAL_SECS));
                    // 一轮扫全部分类（P2-4）：三分类的 Key 拉平成一个任务流做有界并发，
                    // 而非「分类串行 + 分类内逐 Key 串行」那样两层串行叠加。
                    //
                    // 整轮套 timeout(period)：保证**轮次不重叠**。旧实现下 6 条不可达的 Key
                    // 一轮要 180s，长于默认 60s 间隔 → 轮次首尾相接、后台永不空闲。
                    // 超时即放弃本轮剩余探测（下一轮会重来），不留悬挂任务。
                    if tokio::time::timeout(period, health::check_all_categories(&store_bg))
                        .await
                        .is_err()
                    {
                        tracing::warn!(
                            "健康探测一轮未在 {:?} 内完成，已跳过剩余项（下一轮重试）",
                            period
                        );
                    }
                    // 每轮结束重读最新配置，改设置即时生效；设 10s 下限防误配把上游打爆。
                    tokio::time::sleep(period).await;
                }
            });
        });
    }

    tauri::Builder::default()
        // 单实例：再次启动时聚焦已有窗口，避免开多个进程（必须最先注册）
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main_window(app);
        }))
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        // 开机自启动（FR-025）。`--autostart` 参数让被系统拉起的实例可自我识别，
        // 据此最小化到托盘（见 setup 与 [`launched_by_autostart`]）。
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec![AUTOSTART_FLAG]),
        ))
        .manage(AppState { store, proxy, mcp })
        .setup(|app| {
            build_tray(app.handle())?;
            // 状态推送（UX#5）。必须在这里装 —— Store/ProxyManager/后台探测线程都在
            // tauri::Builder 之前就构造好了，那时还没有 AppHandle（详见 events.rs 模块注释）。
            // 装好之前的 emit 是空操作，不会 panic。
            events::init(app.handle());
            let state = app.state::<AppState>();
            let settings = state.store.get_settings();

            // 主口令开关的启动对账（与 FR-025 自启动同思路，但**方向相反**）：
            // 真实模式的事实来源是密钥库文件里的 master 头部，settings 只是 UI 镜像。
            // 故这里以**库**为准去修配置，而不是以配置为准去改库——后者会把用户真实的
            // 加密状态改掉。背离的成因：导入了另一台机器的配置（config 可搬、secrets.enc
            // 不可搬）、手动改过 config.json、或旧版本残留的字段值。
            {
                let real = state.store.secrets.read().is_master_mode();
                if settings.master_password_enabled != real {
                    match state.store.set_master_password_flag(real) {
                        Ok(()) => tracing::info!(
                            "主口令开关已按密钥库真实状态对账为 {real}（配置此前为 {}）",
                            settings.master_password_enabled
                        ),
                        Err(e) => tracing::warn!("主口令开关对账失败: {e}"),
                    }
                }
                if real {
                    tracing::info!("密钥库处于主口令模式，需解锁后才能转发（等待用户输入主口令）");
                }
            }

            // 首启向导标记的启动对账（UX#1）。
            //
            // 老用户升级上来配置里没有 onboarding_done 字段（反序列化成 None）。若不对账，
            // 他们哪天把 Key 全删了（换厂商、清理重配）就会突然被首启向导拦住 ——
            // 一个用了半年的软件毫无征兆地弹「欢迎使用」。这里一次性据当前 Key 数定下来。
            match state.store.reconcile_onboarding_flag() {
                Ok(Some(v)) => tracing::info!("首启向导标记已对账为 {v}（据当前 Key 数判定）"),
                Ok(None) => {}
                Err(e) => tracing::warn!("首启向导标记对账失败: {e}"),
            }

            // 开机自启动（FR-025）的两件事：
            //
            // 1) **状态对账**。config.json 里的 auto_start 与系统实际注册项可能背离——用户手动
            //    改过注册表 Run 键、用清理工具删过启动项、或从旧版本升级（旧版只存字段、从不
            //    注册）。以配置为准把系统拉回一致，让开关「所见即所得」。
            // 2) **随系统启动时最小化到托盘**（需求原文要求）。判据是启动参数里有
            //    `--autostart`（注册自启动项时带上的），而非「auto_start 为真」——后者在用户
            //    手动双击时也成立，那时不该把窗口藏起来。
            {
                let want = settings.auto_start;
                let mgr = {
                    use tauri_plugin_autostart::ManagerExt;
                    app.autolaunch()
                };
                match mgr.is_enabled() {
                    Ok(actual) if actual != want => {
                        let r = if want { mgr.enable() } else { mgr.disable() };
                        match r {
                            Ok(()) => tracing::info!("开机自启动已对账为 {want}（此前系统侧为 {actual}）"),
                            // 对账失败不阻断启动：功能降级，但应用照常可用。
                            Err(e) => tracing::warn!("开机自启动对账失败（配置={want} 系统={actual}）: {e}"),
                        }
                    }
                    Ok(_) => {}
                    Err(e) => tracing::warn!("读取开机自启动状态失败: {e}"),
                }
            }
            if launched_by_autostart(std::env::args()) {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.hide();
                }
                tracing::info!("随系统启动，已最小化到托盘（{AUTOSTART_FLAG}）");
            }

            // 内置 MCP 服务器：若用户已启用，则随应用启动（Q8），端口取自设置（默认 9527，Q7）。
            // 跑在 Tauri 托管异步运行时上，生命周期与应用一致（避免临时 Runtime drop 杀掉 accept 循环）。
            if settings.mcp_enabled {
                let mcp_bg = state.mcp.clone();
                let store_bg = state.store.clone();
                let port = settings.mcp_port;
                tauri::async_runtime::spawn(async move {
                    match mcp_bg.start(port).await {
                        Ok(bound) => {
                            // 端口“粘住”：某些机器上首选端口（如 9527/9528）被系统服务
                            // （WUDFHost / Goodix 指纹服务）永久占用，每次启动都被迫回退。
                            // 把实际绑定端口写回设置作为下次首选，避免每次开机重复
                            // “绑定失败→向上探测→回退→重写客户端”的漂移过程。
                            if bound != port {
                                let _ = store_bg.set_mcp_port(bound);
                            }
                            // 启动时端口可能因占用回退到别的值：用实际绑定端口重写所有已注册分类的
                            // 客户端配置，使 ~/.claude.json / config.toml 里的 url 端口跟真实端口一致，
                            // 用户重启客户端即可用，无需手动重配（幂等：端口没变则不写盘）。
                            let url = format!("http://127.0.0.1:{bound}/mcp");
                            let timeout_ms = service::mcp_client_timeout_ms(&store_bg);
                            let cats = store_bg.get_settings().mcp_registered_categories;
                            for category in cats {
                                {
                                    match tools::register_mcp_client(category, &url, timeout_ms) {
                                        Ok((msg, wrote)) => {
                                            if wrote {
                                                store_bg.append_event(category, "config", None, &msg);
                                            }
                                        }
                                        Err(e) => store_bg.append_event(
                                            category,
                                            "error",
                                            None,
                                            &format!("MCP 启动后重写客户端失败: {e}"),
                                        ),
                                    }
                                }
                            }
                        }
                        Err(e) => tracing::warn!("MCP 服务器启动失败: {e}"),
                    }
                });
            }
            Ok(())
        })
        // 关闭窗口 = 隐藏到托盘（后台代理继续运行），而非退出进程。
        // 真正退出走托盘菜单「退出」→ app.exit(0)（不触发本事件）。
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            list_keys,
            upsert_key,
            check_desktop_model_names,
            delete_key,
            save_secret,
            reveal_secret,
            toggle_key,
            scan_ccswitch,
            import_from_ccswitch,
            fetch_models,
            fetch_models_draft,
            check_health,
            get_brain_config,
            save_brain_config,
            get_proxy_state,
            start_proxy,
            stop_proxy,
            set_proxy_port,
            apply_tool_config,
            get_tool_config_preview,
            restore_tool_config,
            list_events,
            list_all_events,
            recent_failure,
            get_event_trace,
            get_settings,
            save_settings,
            set_auto_start,
            get_onboarding_state,
            set_onboarding_done,
            first_request_since,
            set_active_model,
            set_active_effort,
            rebuild_tray_menu,
            mcp_status,
            set_mcp_enabled,
            register_mcp_for_category,
            unregister_mcp_for_category,
            restart_mcp,
            get_app_version,
            check_for_updates,
            install_update,
            pick_directory,
            get_default_log_dir,
            get_effective_log_dir,
            prepare_log_dir,
            export_diagnostics,
            export_config,
            pick_and_preview_import,
            apply_import_config,
            count_orphan_secrets,
            prune_orphan_secrets,
            list_vendors,
            upsert_vendor,
            delete_vendor,
            aggregate_plan,
            aggregate_execute,
            retrieve_files,
            detect_recent_workdirs,
            detect_codegraph,
            codegraph_init,
            set_primary_key,
            apply_max_tokens_to_category,
            get_master_password_state,
            unlock_master_password,
            lock_master_password,
            enable_master_password,
            disable_master_password,
            change_master_password,
        ])
        .build(tauri::generate_context!())
        .expect("构建 SynaRoute 失败")
        .run(|app, event| {
            // 退出前把日志写线程的队列排空（P1-3）。
            //
            // 必须做：日志落盘改成单写者异步后，队列里可能还有尚未写盘的条目，而排障最需要的
            // 恰恰是退出/崩溃前那几条。`flush_logs` 内部带 3s 上限，磁盘僵死时也不会把退出挂住。
            if let tauri::RunEvent::Exit = event {
                let state = app.state::<AppState>();
                // 健康态若还是脏的（后台合并轮次未到就退出了），在这里补一次落盘。
                // 不做的话，本次运行攒下的熔断计数会丢，下次启动会立刻重打已知坏掉的 Key。
                state.store.flush_health_if_dirty();
                state.store.flush_logs();
                let dropped = state.store.log_dropped_count();
                if dropped > 0 {
                    // 丢弃必须留痕：静默丢日志是本项目最忌讳的失效形态。
                    tracing::warn!("本次运行共丢弃 {dropped} 条日志（队列满 / 磁盘写入过慢）");
                }
            }
        });
}

/// 构建系统托盘（FR-022）
/// 托盘菜单的 Codex 模型项 id 前缀：`model::<对外模型名>`，空名（`model::`）= 跟随客户端透传。
const TRAY_MODEL_PREFIX: &str = "model::";

/// 托盘菜单的 Codex 推理强度项 id 前缀：`effort::<档位>`，空档（`effort::`）= 关（不注入、保持现状）。
/// 取值与应用内下拉同源：low/medium/high/xhigh，空串=关。
const TRAY_EFFORT_PREFIX: &str = "effort::";

/// Codex 推理强度托盘候选：(id 档位, 中文显示名)。空档=关，与 CategoryPage 下拉一致。
const TRAY_EFFORT_OPTIONS: &[(&str, &str)] = &[
    ("", "关"),
    ("low", "低"),
    ("medium", "中"),
    ("high", "高"),
    ("xhigh", "极高"),
];

/// 托盘「代理」子菜单项 id 前缀：`proxy::<分类>`。点击 = 切换该分类代理的启停。
/// FR-022 验收标准要求「从托盘可完成代理开关」。
const TRAY_PROXY_PREFIX: &str = "proxy::";

/// 托盘「主 Key」子菜单项 id 前缀：`primary::<分类>::<keyId>`。
/// 分类与 keyId 都要带上：keyId 全局唯一，但带分类可让处理端校验一致性
/// （`Store::set_primary_key` 对「id 存在但分类不符」会拒绝，见其测试）。
const TRAY_PRIMARY_PREFIX: &str = "primary::";

/// 构建托盘菜单：显示主窗口 +（可选）Codex 模型快切子菜单 + 退出。
/// 候选与 /v1/models、应用内下拉同源（discoverable_models 交集口径），当前选中项打勾。
/// 借鉴 cc-switch 托盘切换范式：右键托盘即可切 Codex 当前对外模型，免打开主窗口。
fn build_tray_menu(app: &tauri::AppHandle) -> tauri::Result<tauri::menu::Menu<tauri::Wry>> {
    use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};

    let show = MenuItem::with_id(app, "show", "显示主窗口", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;

    let state = app.state::<AppState>();
    let settings = state.store.get_settings();

    let menu = Menu::new(app)?;
    menu.append(&show)?;

    // ---- 代理启停（FR-022 验收标准）----
    // 三分类各一项，勾选 = 正在运行。点击即切换，语义与界面按钮完全一致
    // （起→顺带写工具配置、停→顺带还原），实现见 TRAY_PROXY_PREFIX 的事件分支。
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    let proxy_menu = Submenu::new(app, "代理", true)?;
    for c in CategoryType::ALL {
        let running = state.proxy.is_running(c);
        // 标签带端口：用户一眼看到「在哪个端口上跑」，省去打开主窗口核对。
        let label = match state.proxy.port_of(c) {
            Some(p) if running => format!("{} · {p}", c.display_name()),
            _ => c.display_name().to_string(),
        };
        let item = CheckMenuItem::with_id(
            app,
            format!("{TRAY_PROXY_PREFIX}{}", c.as_str()),
            label,
            true,
            running,
            None::<&str>,
        )?;
        proxy_menu.append(&item)?;
    }
    menu.append(&proxy_menu)?;

    // ---- 主 Key 快切（FR-022 验收标准）----
    // 每个分类一个子菜单，列出该分类**启用**的 Key（按优先级），勾选当前主（优先级最小那条）。
    // 只列启用的：禁用 Key 设为主毫无意义（它不进候选池），列出来只会让人以为切了却不生效。
    let primary_menu = Submenu::new(app, "主 Key", true)?;
    let mut any_key = false;
    for c in CategoryType::ALL {
        let keys = state.store.enabled_keys_sorted(c);
        if keys.is_empty() {
            continue;
        }
        any_key = true;
        let sub = Submenu::new(app, c.display_name(), true)?;
        for (i, k) in keys.iter().enumerate() {
            let item = CheckMenuItem::with_id(
                app,
                format!("{TRAY_PRIMARY_PREFIX}{}::{}", c.as_str(), k.id),
                &k.name,
                true,
                i == 0, // enabled_keys_sorted 已按优先级升序，首个即当前主
                None::<&str>,
            )?;
            sub.append(&item)?;
        }
        primary_menu.append(&sub)?;
    }
    if !any_key {
        let empty = MenuItem::with_id(app, "noop", "（还没有启用的 Key）", false, None::<&str>)?;
        primary_menu.append(&empty)?;
    }
    menu.append(&primary_menu)?;

    // Codex 模型快切子菜单（开关开启时）：列出 Codex 启用 Key 可服务模型的交集，当前项打勾，
    // 末尾附「跟随客户端（透传）」。关闭开关则托盘只留显示/退出，不构建此段。
    if settings.tray_model_switch_enabled {
        let candidates = state.store.enabled_keys_sorted(CategoryType::Codex);
        let models = proxy::discoverable_models(&candidates);
        let active = settings
            .active_models
            .get(&CategoryType::Codex)
            .cloned()
            .unwrap_or_default();

        menu.append(&PredefinedMenuItem::separator(app)?)?;
        let submenu = Submenu::new(app, "Codex 模型", true)?;
        if models.is_empty() {
            // 无候选：给一个禁用提示项，避免空子菜单让用户以为坏了。
            let empty = MenuItem::with_id(app, "noop", "（无可用模型，先加并启用 Key）", false, None::<&str>)?;
            submenu.append(&empty)?;
        } else {
            for m in &models {
                let item = CheckMenuItem::with_id(
                    app,
                    format!("{TRAY_MODEL_PREFIX}{m}"),
                    m,
                    true,
                    &active == m,
                    None::<&str>,
                )?;
                submenu.append(&item)?;
            }
            submenu.append(&PredefinedMenuItem::separator(app)?)?;
            // 跟随客户端（透传）：空名，选中即清除 active override，回到透传客户端原模型名。
            let follow = CheckMenuItem::with_id(
                app,
                TRAY_MODEL_PREFIX,
                "跟随客户端（透传）",
                true,
                active.is_empty(),
                None::<&str>,
            )?;
            submenu.append(&follow)?;
        }
        menu.append(&submenu)?;

        // Codex 推理强度快切子菜单：与模型快切同一开关，右键即切。
        // Codex 对自定义 provider 不下发 reasoning.effort，故此处配默认强度、转发时注入。
        // 当前项打勾（active_efforts["codex"]，空=关）。切换复用 set_active_effort，每请求实时重读、免重启。
        let active_effort = settings
            .active_efforts
            .get(&CategoryType::Codex)
            .cloned()
            .unwrap_or_default();
        let effort_submenu = Submenu::new(app, "Codex 推理强度", true)?;
        for (id, label) in TRAY_EFFORT_OPTIONS {
            let item = CheckMenuItem::with_id(
                app,
                format!("{TRAY_EFFORT_PREFIX}{id}"),
                label,
                true,
                active_effort.as_str() == *id,
                None::<&str>,
            )?;
            effort_submenu.append(&item)?;
        }
        menu.append(&effort_submenu)?;
    }

    menu.append(&PredefinedMenuItem::separator(app)?)?;
    menu.append(&quit)?;
    Ok(menu)
}

/// 托盘 tooltip：附带当前 Codex 选定模型，让用户悬停即知当前用哪个（无需展开菜单）。
fn tray_tooltip(app: &tauri::AppHandle) -> String {
    let state = app.state::<AppState>();
    let settings = state.store.get_settings();
    match settings.active_models.get(&CategoryType::Codex) {
        Some(m) if !m.trim().is_empty() => format!("SynaRoute · Codex: {m}"),
        _ => "SynaRoute".to_string(),
    }
}

/// 重建托盘菜单 + 刷新 tooltip（Tauri 托盘菜单静态构建，数据变动后须显式重建）。
/// 把 RGBA8 像素就地灰度化并降不透明度，用于派生「已停止」态托盘图标。
///
/// 返回是否成功：长度不是 4 的整数倍即判失败并**不做任何修改**（宁可两态同图标，
/// 也不要因错位画出花屏）。
///
/// 灰度用感知亮度权重（0.299/0.587/0.114）而非三通道均值——均值会让偏蓝的图标看起来
/// 比实际更亮。alpha 乘 0.55 让它明显「淡下去」，即使用户系统主题下灰度对比不明显，
/// 也能靠透明度分辨。
fn desaturate_rgba_in_place(rgba: &mut [u8]) -> bool {
    if rgba.is_empty() || rgba.len() % 4 != 0 {
        return false;
    }
    for px in rgba.chunks_exact_mut(4) {
        let lum = (0.299 * px[0] as f32 + 0.587 * px[1] as f32 + 0.114 * px[2] as f32)
            .round()
            .clamp(0.0, 255.0) as u8;
        px[0] = lum;
        px[1] = lum;
        px[2] = lum;
        px[3] = (px[3] as f32 * 0.55).round().clamp(0.0, 255.0) as u8;
    }
    true
}

/// 「已停止」状态的托盘图标：由打包图标灰度化 + 降不透明度派生。
///
/// **为什么派生而不是加一份 png 资源**：图标是 `tauri.conf.json` 里配的、打包时嵌入的，
/// 日后换图标只会换那一份。若另存一张「灰色版」，换图标时必然忘记同步，托盘就会出现
/// 「运行时是新图标、停止时是旧图标」的割裂。从运行时拿到的 RGBA 现场派生，永远自动对齐。
///
/// 结果缓存（`OnceLock`）：托盘每次重建都会取一次，而源图标在进程内恒定。
fn stopped_tray_icon(app: &tauri::AppHandle) -> Option<tauri::image::Image<'static>> {
    static CACHED: std::sync::OnceLock<Option<(Vec<u8>, u32, u32)>> = std::sync::OnceLock::new();
    let cached = CACHED.get_or_init(|| {
        let src = app.default_window_icon()?;
        let (w, h) = (src.width(), src.height());
        let mut rgba = src.rgba().to_vec();
        if !desaturate_rgba_in_place(&mut rgba) {
            return None;
        }
        Some((rgba, w, h))
    });
    cached
        .as_ref()
        .map(|(rgba, w, h)| tauri::image::Image::new_owned(rgba.clone(), *w, *h))
}

/// 按「是否有任一分类的代理在运行」给托盘换图标（FR-022：托盘图标反映代理运行状态）。
///
/// 判据是**任一**在跑而非全部：托盘只有一个图标，而三个分类各自独立启停。用户关心的是
/// 「SynaRoute 现在有没有在转发」，细分状态由菜单里各分类的勾选表达。
fn apply_tray_icon(app: &tauri::AppHandle, tray: &tauri::tray::TrayIcon) {
    let state = app.state::<AppState>();
    let any_running = CategoryType::ALL.iter().any(|c| state.proxy.is_running(*c));
    let icon = if any_running {
        app.default_window_icon().cloned()
    } else {
        stopped_tray_icon(app).or_else(|| app.default_window_icon().cloned())
    };
    if let Some(icon) = icon {
        let _ = tray.set_icon(Some(icon));
    }
}

/// 触发时机：托盘内切换模型后、主窗口改动 Key 模型列表后（前端调 rebuild_tray_menu 命令）。
fn rebuild_tray(app: &tauri::AppHandle) -> tauri::Result<()> {
    if let Some(tray) = app.tray_by_id("main") {
        let menu = build_tray_menu(app)?;
        tray.set_menu(Some(menu))?;
        let _ = tray.set_tooltip(Some(tray_tooltip(app)));
        apply_tray_icon(app, &tray);
    }
    Ok(())
}

fn build_tray(app: &tauri::AppHandle) -> tauri::Result<()> {
    use tauri::tray::{TrayIconBuilder, TrayIconEvent};

    let menu = build_tray_menu(app)?;

    // 初始图标先用打包图标占位，`build` 之后立刻按当前代理状态改写（见下面的 apply_tray_icon）。
    // TrayIconBuilder 这一步无法先读状态再选：`stopped_tray_icon` 要 AppHandle，而拿到
    // TrayIcon 才好统一走同一条设置路径，避免「初始图标」与「重建后图标」两套判断逻辑分叉。
    let initial_icon = app
        .default_window_icon()
        .cloned()
        .expect("缺少默认窗口图标");

    let tray = TrayIconBuilder::with_id("main")
        .icon(initial_icon)
        .menu(&menu)
        .tooltip(tray_tooltip(app))
        .on_menu_event(|app, event| {
            let id = event.id.as_ref();
            match id {
                "show" => show_main_window(app),
                "quit" => app.exit(0),
                _ if id.starts_with(TRAY_PROXY_PREFIX) => {
                    handle_tray_proxy_toggle(app, id);
                }
                _ if id.starts_with(TRAY_PRIMARY_PREFIX) => {
                    handle_tray_set_primary(app, id);
                }
                _ if id.starts_with(TRAY_MODEL_PREFIX) => {
                    // model::<名> → 切 Codex 当前对外模型（空名=跟随客户端透传）。
                    // 复用 set_active_model：每请求实时重读，切换即时生效、免重启 Codex。
                    let model = id.strip_prefix(TRAY_MODEL_PREFIX).unwrap_or("");
                    let state = app.state::<AppState>();
                    if let Err(e) = state.store.set_active_model(CategoryType::Codex, model) {
                        state.store.append_event(
                            CategoryType::Codex,
                            "error",
                            None,
                            &format!("托盘切换模型失败: {e}"),
                        );
                        return;
                    }
                    let shown = if model.is_empty() { "跟随客户端（透传）" } else { model };
                    state.store.append_event(
                        CategoryType::Codex,
                        "config",
                        None,
                        &format!("托盘切换 Codex 模型 → {shown}（即时生效）"),
                    );
                    // 重建菜单以刷新勾选与 tooltip。
                    let _ = rebuild_tray(app);
                }
                _ if id.starts_with(TRAY_EFFORT_PREFIX) => {
                    // effort::<档位> → 切 Codex 默认推理强度（空档=关，不注入）。
                    // 复用 set_active_effort：每请求实时重读，切换即时生效、免重启 Codex。
                    let effort = id.strip_prefix(TRAY_EFFORT_PREFIX).unwrap_or("");
                    let state = app.state::<AppState>();
                    if let Err(e) = state.store.set_active_effort(CategoryType::Codex, effort) {
                        state.store.append_event(
                            CategoryType::Codex,
                            "error",
                            None,
                            &format!("托盘切换推理强度失败: {e}"),
                        );
                        return;
                    }
                    let shown = TRAY_EFFORT_OPTIONS
                        .iter()
                        .find(|(v, _)| *v == effort)
                        .map(|(_, l)| *l)
                        .unwrap_or(effort);
                    state.store.append_event(
                        CategoryType::Codex,
                        "config",
                        None,
                        &format!("托盘切换 Codex 推理强度 → {shown}（即时生效）"),
                    );
                    let _ = rebuild_tray(app);
                }
                _ => {}
            }
        })
        // 左键单击托盘图标 = 显示/聚焦主窗口（Windows 常见交互）
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: tauri::tray::MouseButton::Left,
                button_state: tauri::tray::MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        })
        .build(app)?;
    // 启动时通常没有代理在跑 → 这一步把图标改成灰度态。
    // 少了它，应用刚起来图标是彩色的（像在跑），要等用户第一次操作触发 rebuild_tray 才纠正。
    apply_tray_icon(app, &tray);
    Ok(())
}

/// 显示并聚焦主窗口（从托盘恢复）
fn show_main_window(app: &tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

/// 解析 `proxy::<分类>` / `primary::<分类>::<keyId>` 里的分类段。
/// 认不出的分类返回 None——菜单 id 由我们自己生成，认不出说明代码不一致，宁可无动作。
fn parse_tray_category(rest: &str) -> Option<CategoryType> {
    CategoryType::from_str(rest)
}

/// 托盘「代理」项被点击：切换该分类代理的启停。
///
/// **语义必须与界面按钮一致**，否则会造出「托盘说在跑、客户端却没走代理」的错觉：
/// - 启动 = `apply_tool_config_for`（起代理 **并** 写客户端配置），等价于前端 `startProxy`
/// - 停止 = `proxy.stop` + `tools::restore`（还原客户端配置），等价于前端 `stopProxy`
///
/// 全程 best-effort 记事件：托盘操作没有可回显的 UI，出错只能落运行日志，
/// 否则用户点了没反应又不知道为什么。
fn handle_tray_proxy_toggle(app: &tauri::AppHandle, id: &str) {
    let Some(rest) = id.strip_prefix(TRAY_PROXY_PREFIX) else { return };
    let Some(category) = parse_tray_category(rest) else { return };

    let state = app.state::<AppState>();
    let running = state.proxy.is_running(category);
    let store = state.store.clone();
    let proxy = state.proxy.clone();
    let app = app.clone();

    if running {
        proxy.stop(category);
        store.append_event(category, "config", None, "托盘停止代理");
        // 停止即退出接入态（与前端 stopProxy 一致）：还原客户端配置，
        // Codex 连同 auth.json 一起复原，用户官方登录立即恢复。
        match tools::restore(category) {
            Ok(msg) => store.append_event(category, "config", None, &msg),
            Err(e) => store.append_event(
                category,
                "error",
                None,
                &format!("托盘停止后还原客户端配置失败: {e}"),
            ),
        }
        let _ = rebuild_tray(&app);
        return;
    }

    // 启动：菜单事件回调是同步的，而起代理是 async，故丢到 Tauri 托管运行时上跑。
    tauri::async_runtime::spawn(async move {
        match apply_tool_config_for(&store, &proxy, category).await {
            Ok(_) => store.append_event(category, "config", None, "托盘启动代理并写入工具配置"),
            Err(e) => store.append_event(category, "error", None, &format!("托盘启动代理失败: {e}")),
        }
        // 无论成败都刷托盘：失败时状态没变，刷一次能让勾选与真实状态对齐（不留假勾）。
        let _ = rebuild_tray(&app);
    });
}

/// 托盘「主 Key」项被点击：把该 Key 设为所属分类的主（优先级 0）。
///
/// id 形如 `primary::<分类>::<keyId>`。keyId 本身可能含 `::`（UUID 不会，但不做无据假设），
/// 故按**首个** `::` 切分，右侧整体当 keyId。
fn handle_tray_set_primary(app: &tauri::AppHandle, id: &str) {
    let Some(rest) = id.strip_prefix(TRAY_PRIMARY_PREFIX) else { return };
    let Some((cat_str, key_id)) = rest.split_once("::") else { return };
    let Some(category) = parse_tray_category(cat_str) else { return };

    let state = app.state::<AppState>();
    match service::set_primary_key(&state.store, category, key_id, service::PrimarySource::Tray) {
        // 改成功：刷托盘让勾选跟上。
        Ok(true) => {
            let _ = rebuild_tray(app);
        }
        // 已经是主：无动作、不记日志（用户点了当前项，属正常操作，不该刷日志）。
        Ok(false) => {}
        Err(e) => state.store.append_event(
            category,
            "error",
            None,
            &format!("托盘设为主 Key 失败: {e}"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 「随系统启动」的判据必须是启动参数，**不能**是 `auto_start` 配置。
    ///
    /// 需求 FR-025 要求自启动时最小化到托盘。若用配置判定，用户手动双击时也会命中，
    /// 窗口被藏起来 → 用户以为程序没启动、反复双击。
    #[test]
    fn autostart_launch_is_detected_by_flag_not_by_config() {
        assert!(launched_by_autostart(["synaroute.exe", AUTOSTART_FLAG]));
        assert!(
            launched_by_autostart(["synaroute.exe", "--other", AUTOSTART_FLAG]),
            "参数位置不固定，需遍历全部"
        );
        // 手动双击：无该参数 → 必须正常显示窗口。
        assert!(!launched_by_autostart(["synaroute.exe"]));
        // 相近但不相等的参数不得误命中。
        assert!(!launched_by_autostart(["synaroute.exe", "--autostart=1"]));
        assert!(!launched_by_autostart(["synaroute.exe", "autostart"]));
        // 与 MCP stdio 模式的参数互不干扰（那条路径根本不进 Tauri setup）。
        assert!(!launched_by_autostart(["synaroute.exe", "--mcp-stdio"]));
    }

    /// 托盘菜单 id 的往返：生成端与解析端必须完全对称，否则点了没反应（走 `_ => {}` 静默丢弃）。
    #[test]
    fn tray_proxy_menu_ids_round_trip_for_every_category() {
        for c in CategoryType::ALL {
            let id = format!("{TRAY_PROXY_PREFIX}{}", c.as_str());
            assert!(id.starts_with(TRAY_PROXY_PREFIX), "生成的 id 必须带前缀: {id}");
            let rest = id.strip_prefix(TRAY_PROXY_PREFIX).unwrap();
            assert_eq!(parse_tray_category(rest), Some(c), "解析必须还原出同一分类: {id}");
        }
        // 认不出的分类必须返回 None（宁可无动作，也不要默认成某个分类去动错代理）。
        assert_eq!(parse_tray_category("gemini"), None);
        assert_eq!(parse_tray_category(""), None);
    }

    /// 主 Key 菜单 id 形如 `primary::<分类>::<keyId>`，按**首个** `::` 切分。
    /// 若按最后一个切分，含 `::` 的 keyId 会被截断成错误的 id。
    #[test]
    fn tray_primary_menu_ids_split_on_first_separator() {
        for c in CategoryType::ALL {
            let id = format!("{TRAY_PRIMARY_PREFIX}{}::key-abc", c.as_str());
            let rest = id.strip_prefix(TRAY_PRIMARY_PREFIX).unwrap();
            let (cat, key_id) = rest.split_once("::").expect("必须能切出两段");
            assert_eq!(parse_tray_category(cat), Some(c));
            assert_eq!(key_id, "key-abc");
        }
        // keyId 里含 `::` 时，右侧必须整体保留（不能只取最后一段）。
        let id = format!("{TRAY_PRIMARY_PREFIX}codex::odd::key::id");
        let rest = id.strip_prefix(TRAY_PRIMARY_PREFIX).unwrap();
        let (cat, key_id) = rest.split_once("::").unwrap();
        assert_eq!(parse_tray_category(cat), Some(CategoryType::Codex));
        assert_eq!(key_id, "odd::key::id", "keyId 内部的分隔符必须原样保留");
    }

    /// 三类托盘前缀不得互相成为前缀，否则事件分支的 `starts_with` 匹配顺序会决定行为
    /// （例如 `proxy::` 若是 `primary::` 的前缀，点主 Key 会被当成切代理）。
    #[test]
    fn tray_prefixes_are_mutually_non_overlapping() {
        let all = [
            TRAY_MODEL_PREFIX,
            TRAY_EFFORT_PREFIX,
            TRAY_PROXY_PREFIX,
            TRAY_PRIMARY_PREFIX,
        ];
        for (i, a) in all.iter().enumerate() {
            for (j, b) in all.iter().enumerate() {
                if i == j {
                    continue;
                }
                assert!(
                    !a.starts_with(b),
                    "前缀 {a:?} 不能以 {b:?} 开头（会导致事件分支串台）"
                );
            }
        }
    }

    /// 「已停止」图标的派生：必须真的变灰、真的变淡，且不改尺寸。
    /// 直接测像素变换本身（不依赖 AppHandle，单测里拿不到真实图标）。
    #[test]
    fn stopped_icon_derivation_greys_out_and_fades() {
        // 一个 2×1 的图：纯红不透明 + 纯蓝半透明。
        let mut rgba = vec![255u8, 0, 0, 255, 0, 0, 255, 128];
        assert!(desaturate_rgba_in_place(&mut rgba));

        // 像素 1：红 → 亮度 0.299*255 ≈ 76，三通道相等
        assert_eq!(&rgba[0..3], &[76, 76, 76], "必须灰度化为三通道相等");
        assert_eq!(rgba[3], 140, "alpha 应乘 0.55（255*0.55≈140）");
        // 像素 2：蓝 → 亮度 0.114*255 ≈ 29
        assert_eq!(&rgba[4..7], &[29, 29, 29]);
        assert_eq!(rgba[7], 70, "半透明像素也按同比例变淡（128*0.55≈70）");
    }

    /// 像素数据长度非法时必须原样返回、不做部分修改——宁可两态同图标，也不要画出花屏。
    #[test]
    fn stopped_icon_derivation_rejects_malformed_pixel_data() {
        let mut odd = vec![1u8, 2, 3]; // 不是 4 的整数倍
        assert!(!desaturate_rgba_in_place(&mut odd));
        assert_eq!(odd, vec![1, 2, 3], "失败时不得留下半改状态");

        let mut empty: Vec<u8> = vec![];
        assert!(!desaturate_rgba_in_place(&mut empty));
    }
}
