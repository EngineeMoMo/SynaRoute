//! SynaRoute Tauri 后端入口。
//! IPC 命令名与前端 src/lib/bridge.ts 严格对齐。

mod aggregate;
mod error;
mod health;
mod mcp;
mod model;
mod proxy;
mod retrieval;
mod secret;
mod store;
mod tools;
mod upstream;
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
    state.store.upsert_key(key)
}

#[tauri::command]
fn delete_key(state: tauri::State<AppState>, key_id: String) -> AppResult<()> {
    state.store.delete_key(&key_id)
}

/// 按需揭示已存明文密钥（供编辑器"眼睛"查看/续用）。
/// 注意：这会把明文返回给前端 WebView，仅用于本地单机自管密钥场景。
#[tauri::command]
fn reveal_secret(state: tauri::State<AppState>, key_id: String) -> AppResult<Option<String>> {
    state.store.secrets.read().get(&key_id)
}

#[tauri::command]
fn save_secret(state: tauri::State<AppState>, key_id: String, secret: String) -> AppResult<()> {
    // 顺序很重要：先把密钥写进加密库，成功后再置 has_secret=true 落盘 config。
    // 反过来（先标记后写密钥）若写密钥失败，会留下「config 记有密钥、库里实际没有」的
    // 不一致：UI 依据 has_secret 显示已配置，但 reveal_secret/fetch_models/代理转发取不到
    // 密钥，报「未配置密钥」，用户难察觉。先写密钥则失败时直接返回 Err，标记不会被写脏。
    state.store.secrets.write().set(&key_id, &secret)?;
    if let Some(mut k) = state.store.get_key(&key_id) {
        k.has_secret = true;
        state.store.upsert_key(k)?;
    }
    Ok(())
}

#[tauri::command]
fn toggle_key(state: tauri::State<AppState>, key_id: String, enabled: bool) -> AppResult<()> {
    state.store.toggle_key(&key_id, enabled)
}

#[tauri::command]
async fn fetch_models(
    state: tauri::State<'_, AppState>,
    key_id: String,
) -> AppResult<Vec<ModelInfo>> {
    let key = state
        .store
        .get_key(&key_id)
        .ok_or_else(|| error::AppError::NotFound(key_id.clone()))?;
    let secret = state
        .store
        .secrets
        .read()
        .get(&key_id)?
        .ok_or_else(|| error::AppError::Invalid("未配置密钥".into()))?;

    let names = upstream::fetch_models(&key, &secret).await?;
    let now = chrono::Utc::now().timestamp_millis();
    let old_ctx: std::collections::HashMap<String, Option<u32>> = key
        .models
        .iter()
        .map(|m| (m.real_name.clone(), m.context_window))
        .collect();
    let models: Vec<ModelInfo> = names
        .into_iter()
        .map(|n| {
            let cw = old_ctx.get(&n).copied().flatten();
            ModelInfo { real_name: n, source: "fetched".into(), fetched_at: Some(now), context_window: cw }
        })
        .collect();
    state.store.set_models(&key_id, models.clone())?;
    Ok(models)
}

/// 用编辑器里正在填写的 Key 草稿（可能尚未保存）直接探测模型列表。
/// 新增 Key 时前端还没有真实 id（是临时 `k_new`），无法走 store 查找，
/// 因此改为直接传入 key 对象 + secret。secret 为空时（编辑已有 Key 未重填密钥）
/// 回退到 store 中已存的密钥。不落盘，模型随 save 一并持久化。
#[tauri::command]
async fn fetch_models_draft(
    state: tauri::State<'_, AppState>,
    key: ProviderKey,
    secret: Option<String>,
) -> AppResult<Vec<ModelInfo>> {
    let secret = match secret {
        Some(s) if !s.is_empty() => s,
        _ => state
            .store
            .secrets
            .read()
            .get(&key.id)?
            .ok_or_else(|| error::AppError::Invalid("未配置密钥".into()))?,
    };

    let names = upstream::fetch_models(&key, &secret).await?;
    let now = chrono::Utc::now().timestamp_millis();
    let models: Vec<ModelInfo> = names
        .into_iter()
        .map(|n| ModelInfo { real_name: n, source: "fetched".into(), fetched_at: Some(now), context_window: None })
        .collect();
    Ok(models)
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
    state: tauri::State<'_, AppState>,
    category_id: CategoryType,
) -> AppResult<ProxyState> {
    let port = state.proxy.start(category_id).await?;
    Ok(ProxyState {
        category_id,
        port: Some(port),
        status: "running".into(),
        message: None,
    })
}

#[tauri::command]
fn stop_proxy(state: tauri::State<AppState>, category_id: CategoryType) -> AppResult<ProxyState> {
    state.proxy.stop(category_id);
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
    state: tauri::State<'_, AppState>,
    category_id: CategoryType,
    port: u16,
) -> AppResult<ProxyState> {
    // 先落盘新首选端口，再重启代理使其按新端口绑定。
    state.store.set_proxy_port(category_id.as_str(), port)?;
    let was_running = state.proxy.is_running(category_id);
    state.proxy.stop(category_id);
    let bound = state.proxy.start(category_id).await?;
    // 重写该分类客户端 config，指向实际绑定端口（可能因占用回退，用真实值）。
    let endpoint = format!("http://127.0.0.1:{bound}");
    let default_model = state
        .store
        .enabled_keys_sorted(category_id)
        .first()
        .and_then(|k| k.serviceable_models().into_iter().next());
    if let Err(e) = tools::apply(category_id, &endpoint, default_model.as_deref()) {
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
    // 确保代理已启动
    let port = state.proxy.start(category_id).await?;
    let endpoint = format!("http://127.0.0.1:{port}");
    // 默认模型（三端语义不同，禁止混写）：
    // - Claude CLI：env.ANTHROPIC_MODEL + 顶层 model（对外名；策略 A 不写 DEFAULT_*）
    // - Codex：config.toml 的 model（OpenAI 形态，无 ANTHROPIC_*）
    // - 桌面端：忽略 default_model
    let default_model = state
        .store
        .enabled_keys_sorted(category_id)
        .first()
        .and_then(|k| k.serviceable_models().into_iter().next());
    let msg = tools::apply(category_id, &endpoint, default_model.as_deref())?;
    state
        .store
        .append_event(category_id, "config", None, &format!("写入工具配置: {endpoint}"));
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
fn get_settings(state: tauri::State<AppState>) -> AppSettings {
    state.store.get_settings()
}

#[tauri::command]
async fn save_settings(
    state: tauri::State<'_, AppState>,
    settings: AppSettings,
) -> AppResult<()> {
    // 只落非 MCP 控制面字段。mcp_port / mcp_enabled / mcp_registered_categories 在
    // Store::save_settings 内部保留后端值 —— 前端切主题 / 语言的入参不会顶回粘滞端口，
    // 也不会触发无谓的 mcp.start / stop。MCP 控制面走专用命令：
    //   set_mcp_enabled — 开关 + 端口 + 首次注册
    //   restart_mcp     — 停后起 + 重注客户端配置
    //   set_mcp_port    — 后端粘滞端口（不经此路径）
    state.store.save_settings(settings)
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
    state.store.set_active_model(category_id.as_str(), &model)?;
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
    state.store.set_active_effort(category_id.as_str(), &effort)?;
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

/// MCP 服务器地址（供前端展示实际绑定端口的接入地址）。
fn mcp_url_for(port: u16) -> String {
    format!("http://127.0.0.1:{port}/mcp")
}

/// MCP 客户端单次工具调用超时（毫秒）= 各分类整轮预算 total_timeout_ms 的最大值 + 余量，
/// 且不低于历史兜底下限（tools::MCP_TOOL_TIMEOUT_MS）。
///
/// 联动的意义：服务端聚合是整轮墙钟预算（见 aggregate.rs），客户端 MCP 超时必须 ≥ 该预算
/// + 余量，才能保证服务端总在客户端杀连接**之前**优雅降级返回（哪怕是部分结果）。用户在任一
/// 分类调大整轮预算，下次注册/端口漂移重写时客户端超时自动跟随。MCP 客户端一个 server 只有
/// 一个 timeout，而分类各有自己的 total——故取最大值覆盖所有分类。
fn mcp_client_timeout_ms(store: &Arc<Store>) -> u64 {
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
        .max(tools::MCP_TOOL_TIMEOUT_MS)
}

/// 把 synaroute MCP 注册进指定分类对应工具的客户端配置，并把该分类记进
/// settings.mcp_registered_categories（去重）。写盘/跳过都记一条事件日志，方便用户排查。
fn register_and_record(state: &AppState, category: CategoryType, port: u16) {
    let url = mcp_url_for(port);
    let timeout_ms = mcp_client_timeout_ms(&state.store);
    match tools::register_mcp_client(category, &url, timeout_ms) {
        Ok((msg, _wrote)) => {
            state.store.append_event(category, "config", None, &msg);
            // 走后端专用写入（不能用 get_settings→push→save_settings，会被 save_settings
            // 的 mem::take 保留逻辑吞掉，导致该集合永远为空）。
            let _ = state.store.add_registered_category(category.as_str());
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
    let url = mcp_url_for(port);
    let timeout_ms = mcp_client_timeout_ms(&state.store);
    let cats = state.store.get_settings().mcp_registered_categories;
    for c in cats {
        if let Some(category) = CategoryType::from_str(&c) {
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
    let _ = state.store.remove_registered_category(category_id.as_str());
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
        for c in &cats {
            if let Some(category) = CategoryType::from_str(c) {
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

/// 检查更新的结构化结果（前端徽章 / 设置页共用）。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct UpdateCheckResult {
    /// available | up_to_date | error
    status: String,
    current_version: String,
    /// 远端新版本号（仅 available）
    version: Option<String>,
    /// 发布说明（可选）
    notes: Option<String>,
    /// 人类可读错误（仅 error）；已对私有仓库 404 等做友好化
    error: Option<String>,
}

fn friendly_updater_error(raw: &str) -> String {
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

#[tauri::command]
async fn check_for_updates(app: tauri::AppHandle) -> AppResult<UpdateCheckResult> {
    use tauri_plugin_updater::UpdaterExt;
    let current_version = app.package_info().version.to_string();
    let updater = match app.updater() {
        Ok(u) => u,
        Err(e) => {
            return Ok(UpdateCheckResult {
                status: "error".into(),
                current_version,
                version: None,
                notes: None,
                error: Some(friendly_updater_error(&e.to_string())),
            });
        }
    };
    match updater.check().await {
        Ok(Some(u)) => Ok(UpdateCheckResult {
            status: "available".into(),
            current_version,
            version: Some(u.version.clone()),
            notes: u.body.clone(),
            error: None,
        }),
        Ok(None) => Ok(UpdateCheckResult {
            status: "up_to_date".into(),
            current_version,
            version: None,
            notes: None,
            error: None,
        }),
        Err(e) => Ok(UpdateCheckResult {
            // 不抛 Err：前端永远能渲染结果，避免仅靠 catch 字符串猜原因
            status: "error".into(),
            current_version,
            version: None,
            notes: None,
            error: Some(friendly_updater_error(&e.to_string())),
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
        .map_err(|e| error::AppError::Other(friendly_updater_error(&e.to_string())))?
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
                    let interval = store_bg.get_settings().health_check_interval_secs;
                    if interval == 0 {
                        // 关闭：跳过本轮探测（保留各 Key 现有健康态，不改动），仅轮询配置等待重新开启。
                        tokio::time::sleep(std::time::Duration::from_secs(DISABLED_POLL_SECS)).await;
                        continue;
                    }
                    for cat in [
                        CategoryType::ClaudeCli,
                        CategoryType::ClaudeDesktop,
                        CategoryType::Codex,
                    ] {
                        health::check_category(&store_bg, cat).await;
                    }
                    // 每轮结束重读最新配置，改设置即时生效；设 10s 下限防误配把上游打爆。
                    tokio::time::sleep(std::time::Duration::from_secs(interval.max(MIN_INTERVAL_SECS))).await;
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
        .manage(AppState { store, proxy, mcp })
        .setup(|app| {
            build_tray(app.handle())?;
            // 内置 MCP 服务器：若用户已启用，则随应用启动（Q8），端口取自设置（默认 9527，Q7）。
            // 跑在 Tauri 托管异步运行时上，生命周期与应用一致（避免临时 Runtime drop 杀掉 accept 循环）。
            let state = app.state::<AppState>();
            let settings = state.store.get_settings();
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
                            let timeout_ms = mcp_client_timeout_ms(&store_bg);
                            let cats = store_bg.get_settings().mcp_registered_categories;
                            for c in cats {
                                if let Some(category) = CategoryType::from_str(&c) {
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
            delete_key,
            save_secret,
            reveal_secret,
            toggle_key,
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
            get_settings,
            save_settings,
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
            list_vendors,
            upsert_vendor,
            delete_vendor,
            aggregate_plan,
            aggregate_execute,
            retrieve_files,
            detect_recent_workdirs,
        ])
        .run(tauri::generate_context!())
        .expect("运行 SynaRoute 失败");
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

    // Codex 模型快切子菜单（开关开启时）：列出 Codex 启用 Key 可服务模型的交集，当前项打勾，
    // 末尾附「跟随客户端（透传）」。关闭开关则托盘只留显示/退出，不构建此段。
    if settings.tray_model_switch_enabled {
        let candidates = state.store.enabled_keys_sorted(CategoryType::Codex);
        let models = proxy::discoverable_models(&candidates);
        let active = settings
            .active_models
            .get(CategoryType::Codex.as_str())
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
            .get(CategoryType::Codex.as_str())
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
    match settings.active_models.get(CategoryType::Codex.as_str()) {
        Some(m) if !m.trim().is_empty() => format!("SynaRoute · Codex: {m}"),
        _ => "SynaRoute".to_string(),
    }
}

/// 重建托盘菜单 + 刷新 tooltip（Tauri 托盘菜单静态构建，数据变动后须显式重建）。
/// 触发时机：托盘内切换模型后、主窗口改动 Key 模型列表后（前端调 rebuild_tray_menu 命令）。
fn rebuild_tray(app: &tauri::AppHandle) -> tauri::Result<()> {
    if let Some(tray) = app.tray_by_id("main") {
        let menu = build_tray_menu(app)?;
        tray.set_menu(Some(menu))?;
        let _ = tray.set_tooltip(Some(tray_tooltip(app)));
    }
    Ok(())
}

fn build_tray(app: &tauri::AppHandle) -> tauri::Result<()> {
    use tauri::tray::{TrayIconBuilder, TrayIconEvent};

    let menu = build_tray_menu(app)?;

    // 使用应用打包时的默认窗口图标，避免出现空白托盘图标
    let icon = app
        .default_window_icon()
        .cloned()
        .expect("缺少默认窗口图标");

    let _tray = TrayIconBuilder::with_id("main")
        .icon(icon)
        .menu(&menu)
        .tooltip(tray_tooltip(app))
        .on_menu_event(|app, event| {
            let id = event.id.as_ref();
            match id {
                "show" => show_main_window(app),
                "quit" => app.exit(0),
                _ if id.starts_with(TRAY_MODEL_PREFIX) => {
                    // model::<名> → 切 Codex 当前对外模型（空名=跟随客户端透传）。
                    // 复用 set_active_model：每请求实时重读，切换即时生效、免重启 Codex。
                    let model = id.strip_prefix(TRAY_MODEL_PREFIX).unwrap_or("");
                    let state = app.state::<AppState>();
                    if let Err(e) = state.store.set_active_model(CategoryType::Codex.as_str(), model) {
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
                    if let Err(e) = state.store.set_active_effort(CategoryType::Codex.as_str(), effort) {
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
