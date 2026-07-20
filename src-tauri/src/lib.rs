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
    // 更新 has_secret 标记
    if let Some(mut k) = state.store.get_key(&key_id) {
        k.has_secret = true;
        state.store.upsert_key(k)?;
    }
    state.store.secrets.write().set(&key_id, &secret)
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

/// 生成代理端点并写入目标工具配置（会先备份，dev-hard-rules）
#[tauri::command]
async fn apply_tool_config(
    state: tauri::State<'_, AppState>,
    category_id: CategoryType,
) -> AppResult<String> {
    // 确保代理已启动
    let port = state.proxy.start(category_id).await?;
    let endpoint = format!("http://127.0.0.1:{port}");
    let msg = tools::apply(category_id, &endpoint)?;
    state
        .store
        .append_event(category_id, "config", None, &format!("写入工具配置: {endpoint}"));
    Ok(msg)
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
    let mcp_enabled = settings.mcp_enabled;
    let mcp_port = settings.mcp_port;
    state.store.save_settings(settings)?;
    // MCP 开关/端口变化即时生效：开启→启动（幂等，端口变则重启）；关闭→停止。
    if mcp_enabled {
        if let Err(e) = state.mcp.start(mcp_port).await {
            tracing::warn!("MCP 服务器启动失败: {e}");
        }
    } else {
        state.mcp.stop();
    }
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

/// 把 synaroute MCP 注册进指定分类对应工具的客户端配置，并把该分类记进
/// settings.mcp_registered_categories（去重）。写盘/跳过都记一条事件日志，方便用户排查。
fn register_and_record(state: &AppState, category: CategoryType, port: u16) {
    let url = mcp_url_for(port);
    match tools::register_mcp_client(category, &url) {
        Ok((msg, _wrote)) => {
            state.store.append_event(category, "config", None, &msg);
            let mut s = state.store.get_settings();
            let cat = category.as_str().to_string();
            if !s.mcp_registered_categories.contains(&cat) {
                s.mcp_registered_categories.push(cat);
                let _ = state.store.save_settings(s);
            }
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
    let cats = state.store.get_settings().mcp_registered_categories;
    for c in cats {
        if let Some(category) = CategoryType::from_str(&c) {
            match tools::register_mcp_client(category, &url) {
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

/// 启用/停用 MCP 并自动注册到「当前活跃分类」对应的工具。
/// 前端 MCP 开关走这里（携带 activeCategory），而非通用 save_settings。
#[tauri::command]
async fn set_mcp_enabled(
    state: tauri::State<'_, AppState>,
    category_id: CategoryType,
    enabled: bool,
    port: u16,
) -> AppResult<McpStatus> {
    {
        let mut s = state.store.get_settings();
        s.mcp_enabled = enabled;
        s.mcp_port = port;
        state.store.save_settings(s)?;
    }
    if enabled {
        match state.mcp.start(port).await {
            Ok(bound) => {
                // 实际绑定端口可能因占用而回退；用它注册，保证客户端地址与真实端口一致。
                register_and_record(&state, category_id, bound);
                // 若端口漂移，其它已注册分类也要跟着改。
                if bound != port {
                    rewrite_registered_clients(&state, bound);
                }
            }
            Err(e) => {
                tracing::warn!("MCP 服务器启动失败: {e}");
            }
        }
    } else {
        state.mcp.stop();
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
        let mut s = state.store.get_settings();
        s.mcp_registered_categories.clear();
        state.store.save_settings(s)?;
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
async fn check_for_updates(app: tauri::AppHandle) -> AppResult<Option<String>> {
    use tauri_plugin_updater::UpdaterExt;
    let update = app
        .updater()
        .map_err(|e| error::AppError::Other(format!("{e}")))?
        .check()
        .await
        .map_err(|e| error::AppError::Other(format!("检查更新失败: {e}")))?;
    Ok(update.map(|u| u.version))
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
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let store = Arc::new(Store::init().expect("初始化配置失败"));
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
                loop {
                    for cat in [
                        CategoryType::ClaudeCli,
                        CategoryType::ClaudeDesktop,
                        CategoryType::Codex,
                    ] {
                        health::check_category(&store_bg, cat).await;
                    }
                    let interval = store_bg
                        .get_settings()
                        .health_check_interval_secs
                        .max(MIN_INTERVAL_SECS);
                    tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
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
                            // 启动时端口可能因占用回退到别的值：用实际绑定端口重写所有已注册分类的
                            // 客户端配置，使 ~/.claude.json / config.toml 里的 url 端口跟真实端口一致，
                            // 用户重启客户端即可用，无需手动重配（幂等：端口没变则不写盘）。
                            let url = format!("http://127.0.0.1:{bound}/mcp");
                            let cats = store_bg.get_settings().mcp_registered_categories;
                            for c in cats {
                                if let Some(category) = CategoryType::from_str(&c) {
                                    match tools::register_mcp_client(category, &url) {
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
            apply_tool_config,
            restore_tool_config,
            list_events,
            get_settings,
            save_settings,
            mcp_status,
            set_mcp_enabled,
            get_app_version,
            check_for_updates,
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
fn build_tray(app: &tauri::AppHandle) -> tauri::Result<()> {
    use tauri::menu::{Menu, MenuItem};
    use tauri::tray::{TrayIconBuilder, TrayIconEvent};

    let show = MenuItem::with_id(app, "show", "显示主窗口", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &quit])?;

    // 使用应用打包时的默认窗口图标，避免出现空白托盘图标
    let icon = app
        .default_window_icon()
        .cloned()
        .expect("缺少默认窗口图标");

    let _tray = TrayIconBuilder::with_id("main")
        .icon(icon)
        .menu(&menu)
        .tooltip("SynaRoute")
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => show_main_window(app),
            "quit" => app.exit(0),
            _ => {}
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
