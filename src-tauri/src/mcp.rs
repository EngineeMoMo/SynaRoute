//! 内置 MCP（Model Context Protocol）服务器。
//!
//! 让 Codex CLI / Claude Code 等 MCP 客户端直接调用 SynaRoute 的多模型大脑聚合。
//! - 传输：Streamable HTTP（JSON-RPC 2.0 over HTTP POST），监听 127.0.0.1:{port}/mcp
//! - 工具：单个 `synaroute_ai`，参数区分意图（Q3）
//! - 只出主意不写文件（Q5）：返回分析/修改计划，由客户端自己执行修改
//! - 不鉴权（Q9，仅 127.0.0.1）；调用日志走 store 事件流 + 落盘（Q10）
//!
//! 协议实现为手写最小子集（无 SDK 依赖）：仅支持
//! initialize / notifications/initialized / tools/list / tools/call。

use crate::aggregate;
use crate::model::CategoryType;
use crate::store::Store;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

/// MCP 协议版本（与主流客户端对齐）
const PROTOCOL_VERSION: &str = "2024-11-05";

/// 运行中的 MCP 服务器句柄
struct RunningMcp {
    port: u16,
    handle: JoinHandle<()>,
}

/// stdio 子进程发现主应用 MCP 端口用的文件名。
const MCP_PORT_FILE: &str = "mcp-port";

/// 端口文件的完整路径。**读写两侧必须都走这里**。
///
/// 曾经是读、写各自 `current_exe().parent().join(...)` 算一遍 —— 两份等价代码，
/// 改平台适配时极易只改一处，症状是「写在 A、读在 B」，而 `read_mcp_port_file`
/// 读不到只会**静默退回端口扫描**（见 `run_stdio`），没有任何报错，最难查的那类。
///
/// ## 平台差异
///
/// **Windows：exe 同级**（如 `F:\SynaRoute\`）。理由是躲 MSIX AppData 虚拟化 ——
/// Claude/Codex 等包应用拉起的子进程读 `%APPDATA%` 会被重定向到包容器私有副本，
/// 与用户双击启动的主应用读到的是两份文件（本项目最大的历史惨案，见 CLAUDE.md）。
/// exe 同级目录不被虚拟化，任何身份的进程读到的都是同一份真实端口。
///
/// **macOS：`~/Library/Application Support/SynaRoute/`**。上述前提在 macOS 完全不存在
/// （无 AppData 虚拟化），而照搬 exe 同级会踩 bundle 的坑：`current_exe()` 位于
/// `SynaRoute.app/Contents/MacOS/`，写进去会被 updater 的整包替换清掉、让 codesign
/// 的 sealed resources 校验失败、在只读卷上直接写失败。且 Codex/Claude 拉起的 stdio
/// 子进程与主应用同属一个用户，读同一个 home 下的路径没有任何歧义。
fn mcp_port_file_path() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let dir = dirs::data_dir()?.join("SynaRoute");
        // 主应用与 stdio 子进程都可能先到；建不出目录就当拿不到路径（调用方各有兜底）。
        std::fs::create_dir_all(&dir).ok()?;
        Some(dir.join(MCP_PORT_FILE))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let exe = std::env::current_exe().ok()?;
        Some(exe.parent()?.join(MCP_PORT_FILE))
    }
}

/// 主应用把实际 MCP 端口写到端口文件，供 stdio 子进程发现（见 [`run_stdio`]）。
/// 写失败不致命（子进程会退回端口扫描），仅记日志。
fn write_mcp_port_file(port: u16) {
    let Some(path) = mcp_port_file_path() else {
        tracing::warn!("无法定位 MCP 端口文件路径，stdio 子进程将退回端口扫描");
        return;
    };
    if let Err(e) = std::fs::write(&path, port.to_string()) {
        tracing::warn!("写 MCP 端口文件失败({}): {e}", path.display());
    }
}

/// 读端口文件（stdio 子进程用）。读不到返回 None。
fn read_mcp_port_file() -> Option<u16> {
    let path = mcp_port_file_path()?;
    let content = std::fs::read_to_string(path).ok()?;
    content.trim().parse::<u16>().ok()
}

/// 端口占用时，向上探测的最大候选数（configured .. configured+FALLBACK_RANGE）。
/// 9527 等常用端口在部分 Windows 机器上被系统进程（WUDFHost / GoodixSessionService 等）占用，
/// 单端口硬绑定会静默失败，故自动向上找一个可用端口，并把真实端口暴露给 UI。
const FALLBACK_RANGE: u16 = 20;

/// MCP 服务器管理器：负责生命周期（启用即启动，改端口需重启）。
pub struct McpManager {
    store: Arc<Store>,
    running: Mutex<Option<RunningMcp>>,
    /// 最近一次启动失败的原因（供 UI 展示；成功时清空）。
    last_error: Mutex<Option<String>>,
}

impl McpManager {
    pub fn new(store: Arc<Store>) -> Self {
        Self {
            store,
            running: Mutex::new(None),
            last_error: Mutex::new(None),
        }
    }

    pub fn is_running(&self) -> bool {
        self.running.lock().is_some()
    }

    pub fn running_port(&self) -> Option<u16> {
        self.running.lock().as_ref().map(|r| r.port)
    }

    pub fn last_error(&self) -> Option<String> {
        self.last_error.lock().clone()
    }

    /// 启动 MCP 服务器（幂等：已在相同端口运行则跳过；端口变化则先停后起）。
    /// 配置端口被占用时，在 [port, port+FALLBACK_RANGE] 内自动向上寻找可用端口。
    /// 返回实际绑定的端口（可能与请求端口不同——UI 需按此展示接入地址）。
    pub async fn start(&self, port: u16) -> Result<u16, String> {
        if let Some(cur) = self.running_port() {
            if cur == port {
                return Ok(cur);
            }
            self.stop();
        }

        // 逐个尝试候选端口，记录最后一次错误用于诊断。
        let mut listener = None;
        let mut last_err = String::new();
        let end = port.saturating_add(FALLBACK_RANGE);
        for candidate in port..=end {
            let addr = SocketAddr::from(([127, 0, 0, 1], candidate));
            match TcpListener::bind(addr).await {
                Ok(l) => {
                    listener = Some(l);
                    break;
                }
                Err(e) => {
                    last_err = format!("{candidate}: {e}");
                }
            }
        }

        let listener = match listener {
            Some(l) => l,
            None => {
                let msg = format!(
                    "MCP 服务器启动失败：端口 {port}~{end} 全部被占用（最后错误 {last_err}）。请在设置里换一个端口。"
                );
                *self.last_error.lock() = Some(msg.clone());
                tracing::error!("{msg}");
                return Err(msg);
            }
        };
        let bound = listener.local_addr().map(|a| a.port()).unwrap_or(port);

        let store = self.store.clone();
        let handle = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                let io = TokioIo::new(stream);
                let store = store.clone();
                tokio::spawn(async move {
                    let svc = service_fn(move |req| handle_http(store.clone(), req));
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(io, svc)
                        .await;
                });
            }
        });

        *self.running.lock() = Some(RunningMcp {
            port: bound,
            handle,
        });
        *self.last_error.lock() = None;
        if bound != port {
            tracing::warn!("MCP 请求端口 {port} 被占用，已改用 {bound}");
        }
        // 把实际端口写到 exe 同级的 mcp-port 文件：stdio 子进程（--mcp-stdio）据此找到运行中的
        // 主应用并转发 tools/call。写 exe 同级（如 F:\SynaRoute\）而非 %APPDATA%——后者会被
        // MSIX 虚拟化（Claude/Codex 等包应用读到的是包容器私有副本，与真实文件是平行宇宙），
        // exe 同级目录不虚拟化，任何身份的进程读到的都是同一份真实端口。
        write_mcp_port_file(bound);
        tracing::info!("MCP 服务器已启动: http://127.0.0.1:{bound}/mcp");
        Ok(bound)
    }

    pub fn stop(&self) {
        if let Some(r) = self.running.lock().take() {
            r.handle.abort();
            tracing::info!("MCP 服务器已停止");
        }
    }
}

// ─── stdio 层（Codex 专用）─────────────────────────────────────────────────
//
// Codex 对 HTTP/streamable MCP 支持是实验性的（需 experimental_use_rmcp_client，且
// 握手挑剔、易「空壳」）。stdio 是 Codex 一等公民（codegraph/sqlcl 等均为 stdio），
// 稳定、无端口漂移、无首字节超时。故 Codex 走 stdio：由 Codex 以子进程拉起
// `synaroute.exe --mcp-stdio`，用 stdin/stdout 传 JSON-RPC（每行一条，LSP 无框架式）。
//
// 复用与 HTTP 完全相同的 dispatch（initialize / tools/list / tools/call / ping），
// 只是传输层换成标准输入输出。通知（无 id）不回响应，与 HTTP 语义一致。

/// stdio MCP 主循环：逐行读 stdin 的 JSON-RPC 请求，处理后把响应逐行写 stdout。
/// 阻塞直到 stdin 关闭（Codex 结束子进程时）。返回后进程应退出。
///
/// **关键设计（MSIX 宇宙错位的根治）**：本子进程**不读配置、不跑聚合**。
/// 早期版本让子进程自己 `dispatch`（含跑聚合），但 stdio 子进程被 Codex 桌面端
/// （MSIX 包）拉起时，继承其包身份 → 读 %APPDATA% 被虚拟化到包容器私有副本，
/// 那份配置没有用户在真实应用里配的 codex 聚合成员 → 永远「未启用大脑聚合」。
///
/// 现在改为**纯转发**：initialize/tools/list/ping 用本地静态响应（不依赖配置，
/// 且 Codex 启动即能握手拿到工具）；`tools/call` 转发到**运行中主应用**的 HTTP MCP
/// （127.0.0.1:{port}/mcp）。主应用是用户双击启动的、读真实配置，聚合在那里跑。
/// localhost TCP 端口不受 MSIX 虚拟化影响（系统全局），故转发能跨包身份连通。
pub async fn run_stdio() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin);
    let mut stdout = tokio::io::stdout();
    let mut line = String::new();

    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) => break, // EOF：Codex 关闭了管道，退出循环 → 进程结束
            Ok(_) => {}
            Err(_) => break,
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue, // 非法 JSON：忽略该行（无 id 无从回错）
        };

        let id = msg.get("id").cloned();
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
        // 通知（无 id，如 notifications/initialized）：不回响应。
        if id.is_none() {
            continue;
        }
        let id = id.unwrap();
        let params = msg.get("params").cloned().unwrap_or(Value::Null);

        let resp = match local_static_response(method, &params) {
            // 握手/列举/心跳/资源探测：本地静态响应（与 HTTP dispatch 共用同一表），
            // 不依赖主应用是否已启动，保证 Codex 启动即能完成握手、看到 synaroute_ai。
            Some(Ok(result)) => rpc_ok(id, result),
            Some(Err((code, msg))) => rpc_error(id, code, &msg),
            // tools/call：转发到运行中主应用（持有真实配置）。
            None => match forward_tool_call_to_main(&params).await {
                Ok(value) => rpc_ok(id, value),
                Err(msg) => rpc_ok(id, tool_error_content(&msg)),
            },
        };
        let mut out = serde_json::to_vec(&resp).unwrap_or_default();
        out.push(b'\n');
        if stdout.write_all(&out).await.is_err() {
            break;
        }
        let _ = stdout.flush().await;
    }
}

/// 把 stdio 收到的 `tools/call` 转发到运行中主应用的 HTTP MCP。
/// 端口发现：先读 exe 同级端口文件（主应用启动时写，不受 MSIX 虚拟化影响），
/// 读不到再扫描默认端口范围。主应用未运行时返回可读错误，提示用户启动应用。
async fn forward_tool_call_to_main(params: &Value) -> Result<Value, String> {
    let port = discover_main_mcp_port()
        .await
        .ok_or_else(|| "未找到运行中的 SynaRoute 主程序，请先启动 SynaRoute 桌面应用".to_string())?;
    let url = format!("http://127.0.0.1:{port}/mcp");
    // 组装标准 MCP tools/call 请求（JSON-RPC over HTTP），转发给主应用。
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": params,
    });
    // 聚合可能耗时较久（多模型并行 + 决策者），给足超时（与 tool_timeout_sec 对齐留余量）。
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(600))
        .build()
        .map_err(|e| format!("构造 HTTP 客户端失败: {e}"))?;
    let resp = client
        .post(&url)
        .header("content-type", "application/json")
        .header("accept", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("连接 SynaRoute 主程序失败({url}): {e}"))?;
    let v: Value = resp
        .json()
        .await
        .map_err(|e| format!("解析主程序响应失败: {e}"))?;
    // JSON-RPC 响应：优先取 result（工具结果），有 error 则透出其 message。
    if let Some(result) = v.get("result") {
        Ok(result.clone())
    } else if let Some(err) = v.get("error") {
        let m = err.get("message").and_then(|m| m.as_str()).unwrap_or("主程序返回错误");
        Err(m.to_string())
    } else {
        Err("主程序响应缺少 result/error".into())
    }
}

/// 发现运行中主应用的 MCP 端口：先读 exe 同级端口文件，再扫描默认端口范围探活。
async fn discover_main_mcp_port() -> Option<u16> {
    // 1. 端口文件（主应用启动时写，最可靠）。读到后探活确认真的是 SynaRoute MCP。
    if let Some(p) = read_mcp_port_file() {
        if probe_mcp_alive(p).await {
            return Some(p);
        }
    }
    // 2. 兜底：扫描默认端口范围（端口文件缺失/过期时）。默认起始端口 9527，与 default_mcp_port
    //    对齐；主应用端口占用回退时也在 [9527, 9527+FALLBACK_RANGE] 内，故这里同范围扫描能覆盖。
    let start = 9527u16;
    for p in start..=start.saturating_add(FALLBACK_RANGE) {
        if probe_mcp_alive(p).await {
            return Some(p);
        }
    }
    None
}

/// 探活：向候选端口发一个最小 initialize，确认对面是活着的 SynaRoute MCP。
async fn probe_mcp_alive(port: u16) -> bool {
    let url = format!("http://127.0.0.1:{port}/mcp");
    let body = json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": { "protocolVersion": PROTOCOL_VERSION }
    });
    let Ok(client) = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
    else {
        return false;
    };
    match client
        .post(&url)
        .header("content-type", "application/json")
        .header("accept", "application/json")
        .json(&body)
        .send()
        .await
    {
        Ok(resp) => {
            // 确认是 SynaRoute（serverInfo.name），避免误连其它占用同端口的服务。
            if let Ok(v) = resp.json::<Value>().await {
                return v
                    .get("result")
                    .and_then(|r| r.get("serverInfo"))
                    .and_then(|s| s.get("name"))
                    .and_then(|n| n.as_str())
                    == Some("SynaRoute");
            }
            false
        }
        Err(_) => false,
    }
}

// ─── HTTP 层 ────────────────────────────────────────────────────────────────

fn json_response(status: StatusCode, body: Value) -> Response<Full<Bytes>> {
    let bytes = serde_json::to_vec(&body).unwrap_or_default();
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .header("access-control-allow-origin", "*")
        .body(Full::new(Bytes::from(bytes)))
        .unwrap()
}

fn empty_response(status: StatusCode) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .header("access-control-allow-origin", "*")
        .header("access-control-allow-methods", "POST, GET, OPTIONS")
        .header("access-control-allow-headers", "content-type, mcp-session-id")
        .body(Full::new(Bytes::new()))
        .unwrap()
}

/// 是否允许该 `Origin` 访问本地 MCP（DNS rebinding 防护）。
///
/// MCP Streamable HTTP 规范要求本地服务器校验 `Origin`：否则用户随便打开的网页可以用
/// `fetch('http://127.0.0.1:9527/mcp')` 直接驱动本机 MCP —— 对 SynaRoute 尤其危险，
/// 因为 `synaroute_ai` 会消耗用户上游额度，且它的 `cwd` 参数会让服务端去检索该目录下的文件。
///
/// 判据：
/// - **无 Origin 头** → 放行。非浏览器客户端（Codex 的 rmcp、Claude CLI、curl）不发 Origin，
///   而浏览器发起的跨源请求一定带 Origin，故「无 Origin」不是绕过口子。
/// - 有 Origin → 仅放行 loopback 源（`http(s)://localhost|127.0.0.1|[::1]`，可带端口）
///   与 `null`（file:// 页面 / sandbox iframe，本机开发调试用）。
fn origin_allowed(origin: Option<&str>) -> bool {
    let Some(origin) = origin else { return true };
    let o = origin.trim();
    if o.is_empty() || o.eq_ignore_ascii_case("null") {
        return true;
    }
    let rest = match o.split_once("://") {
        Some((scheme, rest)) if scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https") => rest,
        // 非 http(s) 源（tauri://、app:// 等自有壳）一律放行：不是浏览器可伪造的跨源场景。
        _ => return true,
    };
    // 去掉端口后比对主机名。IPv6 形如 `[::1]:9527`。
    let host = if let Some(stripped) = rest.strip_prefix('[') {
        match stripped.split_once(']') {
            Some((h, _)) => format!("[{h}]"),
            None => return false,
        }
    } else {
        rest.split(':').next().unwrap_or("").to_string()
    };
    matches!(host.to_ascii_lowercase().as_str(), "localhost" | "127.0.0.1" | "[::1]")
}

async fn handle_http(
    store: Arc<Store>,
    req: Request<Incoming>,
) -> Result<Response<Full<Bytes>>, hyper::Error> {
    // Origin 校验放在最前：非法源连 OPTIONS 预检都不该拿到 CORS 许可，
    // 否则等于告诉网页「可以来调」。
    let origin = req
        .headers()
        .get(hyper::header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    if !origin_allowed(origin.as_deref()) {
        tracing::warn!("拒绝非本机来源的 MCP 请求: Origin={:?}", origin);
        return Ok(json_response(
            StatusCode::FORBIDDEN,
            rpc_error(Value::Null, -32600, "仅允许本机来源访问 MCP（Origin 校验未通过）"),
        ));
    }
    // CORS 预检
    if req.method() == hyper::Method::OPTIONS {
        return Ok(empty_response(StatusCode::NO_CONTENT));
    }
    // GET /mcp：部分客户端探测 SSE 通道。我们只做请求-响应，返回 405 让其回退到 POST。
    if req.method() == hyper::Method::GET {
        return Ok(empty_response(StatusCode::METHOD_NOT_ALLOWED));
    }
    if req.method() != hyper::Method::POST {
        return Ok(empty_response(StatusCode::METHOD_NOT_ALLOWED));
    }

    let body_bytes = match req.into_body().collect().await {
        Ok(c) => c.to_bytes(),
        Err(_) => {
            return Ok(json_response(
                StatusCode::BAD_REQUEST,
                rpc_error(Value::Null, -32700, "读取请求体失败"),
            ))
        }
    };

    let msg: Value = match serde_json::from_slice(&body_bytes) {
        Ok(v) => v,
        Err(_) => {
            return Ok(json_response(
                StatusCode::BAD_REQUEST,
                rpc_error(Value::Null, -32700, "JSON 解析失败"),
            ))
        }
    };

    // 通知（无 id）：如 notifications/initialized —— 返回 202，无响应体
    let id = msg.get("id").cloned();
    let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");

    if id.is_none() {
        // 通知类消息，确认收到即可
        return Ok(empty_response(StatusCode::ACCEPTED));
    }
    let id = id.unwrap();

    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let is_initialize = method == "initialize";
    let result = dispatch(&store, method, params).await;

    match result {
        Ok(value) => {
            let mut resp = json_response(StatusCode::OK, rpc_ok(id, value));
            // Streamable HTTP 规范：服务器在 initialize 响应里通过 `Mcp-Session-Id` 头下发会话 ID，
            // 客户端（rmcp / Codex）后续请求带回。缺此头时严格客户端认为握手未完成、不发 tools/list
            // ——表现为「连上了但工具是空壳」。我们无状态，会话 ID 仅作握手合规用，不校验其回传。
            if is_initialize {
                if let Ok(v) = hyper::header::HeaderValue::from_str(&new_session_id()) {
                    resp.headers_mut().insert("mcp-session-id", v);
                }
            }
            Ok(resp)
        }
        Err((code, message)) => Ok(json_response(
            StatusCode::OK,
            rpc_error(id, code, &message),
        )),
    }
}

/// 生成一个会话 ID（Streamable HTTP 的 Mcp-Session-Id）。无状态服务，仅为握手合规。
fn new_session_id() -> String {
    // 用时间戳 + 进程内自增，避免引入 uuid 依赖；唯一性对本用途足够。
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("synaroute-{ts:x}-{n:x}")
}

// ─── JSON-RPC 分发 ───────────────────────────────────────────────────────────

fn rpc_ok(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// 不依赖运行态（Store / 主应用）的静态 MCP 方法：握手、心跳、工具/资源/提示词列举。
/// stdio 与 HTTP 两条路径共用此表，保证行为一致、单一事实源。
///
/// 返回 `Some(..)` 表示已处理；`None` 表示交由调用方处理（目前仅 `tools/call`，
/// 因两条路径去向不同：stdio 转发到主应用，HTTP 本地执行）。
///
/// 关键：`resources/list`、`resources/templates/list`、`prompts/list` 即便本服务
/// 不提供，也必须返回**空列表**而非 -32601。Codex 桌面端（protocol 2025-06-18）在
/// initialize 后会探测这些方法，收到 error 会判定 server 启动失败并 cancel 整个连接，
/// 导致 synaroute_ai 进不了工具目录（模型侧表现为「没有这个工具」）。
fn local_static_response(method: &str, params: &Value) -> Option<Result<Value, (i64, String)>> {
    match method {
        // 回显客户端请求的 protocolVersion：rmcp（Codex 用的客户端）会协商版本，服务器硬编码
        // 旧版而客户端要更新版时，握手被判不完整、后续 tools/list 拿不到工具。回显即对齐；
        // 客户端未带时兜底用我们支持的默认版本。
        "initialize" => {
            let ver = params
                .get("protocolVersion")
                .and_then(|v| v.as_str())
                .unwrap_or(PROTOCOL_VERSION);
            Some(Ok(json!({
                "protocolVersion": ver,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "SynaRoute", "version": env!("CARGO_PKG_VERSION") }
            })))
        }
        "ping" => Some(Ok(json!({}))),
        "tools/list" => Some(Ok(json!({ "tools": [tool_schema()] }))),
        "resources/list" => Some(Ok(json!({ "resources": [] }))),
        "resources/templates/list" => Some(Ok(json!({ "resourceTemplates": [] }))),
        "prompts/list" => Some(Ok(json!({ "prompts": [] }))),
        "tools/call" => None,
        other => Some(Err((-32601, format!("未知方法: {other}")))),
    }
}

async fn dispatch(
    store: &Arc<Store>,
    method: &str,
    params: Value,
) -> Result<Value, (i64, String)> {
    match local_static_response(method, &params) {
        Some(res) => res,
        // tools/call：HTTP 路径本地执行（主应用持有真实配置）。
        None => handle_tool_call(store, params).await,
    }
}

/// synaroute_ai 工具的 schema（Q6：基本参数）
fn tool_schema() -> Value {
    json!({
        "name": "synaroute_ai",
        "description": "调用 SynaRoute 多模型大脑聚合：多个模型并行回答同一个问题，再由决策者综合出一份高质量答案。这是一个**通用**的多视角会诊工具，适用于任何值得多个模型交叉验证的任务——技术方案设计、代码审查、疑难排查、资料分析、决策权衡、开放式问答等，不限于编程。当你希望「集思广益、降低单模型偏差、得到更可靠的结论」时调用它。注意：本工具只返回分析与建议，不直接修改文件；若结论涉及改代码，请据此用你自己的编辑工具执行并让用户确认。",
        "inputSchema": {
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "要交给多模型会诊的问题或任务。可以是任何主题，例如「审查鉴权模块的安全性」「设计一个缓存层方案」「对比几种限流算法的取舍」「分析这段需求有哪些坑」，也可以是与代码无关的开放问题。"
                },
                "cwd": {
                    "type": "string",
                    "description": "可选：当前项目根目录的绝对路径。仅当问题与某个代码库相关、需要 SynaRoute 检索项目文件作为上下文时才传；纯问答/与代码无关的任务可省略。省略时若开启了自动跟随，则用最近活跃的项目。"
                },
                "category": {
                    "type": "string",
                    "enum": ["claude-cli", "claude-desktop", "codex"],
                    "description": "使用哪个分类下配置的 Key 池与聚合设置，省略默认 claude-cli"
                },
                "languageHint": {
                    "type": "string",
                    "description": "回答语言提示，如 zh / en，省略则跟随 prompt 语言"
                },
                "images": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "可选：要让模型一起看的图片，填**相对于 cwd 的路径**（如 docs/error.png）。用于「这个报错截图是什么问题」这类需要看图的任务。最多 4 张、单张不超过 5MB，仅支持 png / jpg / jpeg / gif / webp。传了 images 就必须同时传 cwd。注意：参与者模型需具备多模态能力，纯文本模型会返回 4xx。"
                }
            },
            "required": ["prompt"]
        }
    })
}

/// 校验并取出 `images` 参数（相对 cwd 的路径数组）。
///
/// **响亮报错、不静默丢**（FR-027）：调用方把任何一种「传了但形状不对」都变成用户可见的错误，
/// 而不是当作「没传图」继续。三种要拦的错法：
/// - `images` 存在但不是数组（`"images":"a.png"` 裸字符串是模型最容易犯的）
/// - 数组里混进非字符串项（`["a.png", 123]`）
/// - 数组里有空字符串
///
/// 内容级校验（张数 / 大小 / 格式 / 路径逃逸）在 `agent_tools::load_images` 里做，不在这里。
fn parse_image_paths(images: Option<&Value>) -> Result<Vec<String>, String> {
    let Some(v) = images else {
        return Ok(Vec::new());
    };
    // 显式 null 视同未传（JSON 客户端常把可选参数填 null）。
    if v.is_null() {
        return Ok(Vec::new());
    }
    let Some(arr) = v.as_array() else {
        return Err(
            "images 必须是字符串数组（相对 cwd 的图片路径），例如 [\"docs/error.png\"]。\
             若只有一张也要用数组包起来。"
                .into(),
        );
    };
    let mut out = Vec::with_capacity(arr.len());
    for (i, item) in arr.iter().enumerate() {
        let Some(s) = item.as_str() else {
            return Err(format!(
                "images[{i}] 不是字符串。每一项都应是相对 cwd 的图片路径。"
            ));
        };
        let s = s.trim();
        if s.is_empty() {
            return Err(format!("images[{i}] 是空字符串，请填写有效的图片路径或去掉它。"));
        }
        out.push(s.to_string());
    }
    Ok(out)
}

async fn handle_tool_call(
    store: &Arc<Store>,
    params: Value,
) -> Result<Value, (i64, String)> {    let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
    if name != "synaroute_ai" {
        return Err((-32602, format!("未知工具: {name}")));
    }
    let args = params.get("arguments").cloned().unwrap_or(Value::Null);

    let prompt = args
        .get("prompt")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if prompt.is_empty() {
        return Ok(tool_error_content("缺少必填参数 prompt"));
    }

    let cwd = args
        .get("cwd")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let category = args
        .get("category")
        .and_then(|v| v.as_str())
        .and_then(CategoryType::from_str)
        .unwrap_or(CategoryType::ClaudeCli);
    let language_hint = args
        .get("languageHint")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    // 图片路径按原样传下去，由 aggregate 用**同一个** effective_work_dir 解析并校验
    // （两处各算一次工作目录必然漂移）。
    //
    // 校验形状要**响亮报错、不静默丢**（FR-027 原则）：模型很可能把单图传成裸字符串
    // `"images":"a.png"` 而非数组，或数组里混进非字符串。旧写法 `as_array()?.filter_map(as_str)`
    // 会把这些整个吞掉 → 聚合照跑 → 用户拿到「看起来看了图、实际没看」的答案。
    let image_paths = match parse_image_paths(args.get("images")) {
        Ok(v) => v,
        Err(msg) => return Ok(tool_error_content(&msg)),
    };

    // 语言提示拼进 prompt（决策者/参与者都会看到）
    let effective_prompt = match &language_hint {
        Some(lang) if !lang.is_empty() => {
            format!("{prompt}\n\n（请用 {lang} 回答）")
        }
        _ => prompt.clone(),
    };

    let started = std::time::Instant::now();
    let outcome =
        aggregate::run_mcp(store, category, &effective_prompt, cwd.clone(), image_paths).await;
    let elapsed = started.elapsed().as_millis() as u64;

    match outcome {
        Ok(res) => {
            // 记录调用日志（事件流 + 落盘，Q10）。带完整 trace：prompt 进 requestBody、
            // 聚合分析全文进 responseBody，日志页可展开查看，解决「MCP 日志太长看不了」。
            let detail = format!(
                "synaroute_ai · {} · 参与者 成功{}/发起{}{} · {}个文件 · {}ms",
                res.work_dir.as_deref().unwrap_or("(无工作目录)"),
                res.member_labels.len(),
                res.members_attempted,
                if res.members_failed > 0 {
                    format!("(失败{})", res.members_failed)
                } else {
                    String::new()
                },
                res.file_count,
                elapsed
            );
            let members = if res.member_labels.is_empty() {
                "(无可用参与者)".to_string()
            } else {
                res.member_labels.join(" · ")
            };
            let trace = crate::model::RequestTrace {
                key_name: "synaroute_ai".into(),
                vendor: "mcp".into(),
                protocol: crate::model::Protocol::Anthropic,
                url: res.work_dir.clone().unwrap_or_default(),
                requested_model: format!("参与者: {members}"),
                real_model: format!("{}个文件", res.file_count),
                request_body: prompt.clone(),
                response_body: res.analysis.clone(),
                status: Some(200),
                latency_ms: elapsed,
                ok: true,
            };
            store.append_event_trace(category, "mcp", None, &detail, Some(trace));

            let md = format_markdown(&prompt, &res, elapsed);
            Ok(tool_text_content(&md))
        }
        Err(e) => {
            let trace = crate::model::RequestTrace {
                key_name: "synaroute_ai".into(),
                vendor: "mcp".into(),
                protocol: crate::model::Protocol::Anthropic,
                url: cwd.clone().unwrap_or_default(),
                requested_model: String::new(),
                real_model: String::new(),
                request_body: prompt.clone(),
                response_body: format!("{e}"),
                status: None,
                latency_ms: elapsed,
                ok: false,
            };
            store.append_event_trace(
                category,
                "mcp",
                None,
                &format!("synaroute_ai 失败: {e}"),
                Some(trace),
            );
            Ok(tool_error_content(&format!("聚合失败: {e}")))
        }
    }
}

/// 组装返回给客户端的 Markdown（Q4：Codex / Claude Code 都能干净渲染）
fn format_markdown(prompt: &str, res: &aggregate::McpAggregateResult, elapsed_ms: u64) -> String {
    let mut md = String::new();
    md.push_str("## 🧠 SynaRoute 多模型聚合分析\n\n");
    md.push_str(&res.analysis);
    md.push_str("\n\n---\n");
    if let Some(ref wd) = res.work_dir {
        md.push_str(&format!("**项目**：`{wd}`\n\n"));
    }
    if res.member_labels.is_empty() {
        // 全部成员失败/不可用时，说清是「发起了 N 个但全失败」还是「没有可用成员」，
        // 而非笼统一句「无可用参与者」——便于用户判断是配置问题还是上游问题。
        if res.members_attempted > 0 {
            md.push_str(&format!(
                "**参与模型**：发起 {} 个，全部失败（已由决策者独立分析）。请在 SynaRoute「运行日志 → 大脑聚合」查看每个成员的失败原因。\n\n",
                res.members_attempted
            ));
        } else {
            md.push_str("**参与模型**：（无可用参与者，已由决策者独立分析）\n\n");
        }
    } else {
        md.push_str(&format!(
            "**参与模型**（成功 {} / 发起 {}{}）：{}\n\n",
            res.member_labels.len(),
            res.members_attempted,
            if res.members_failed > 0 {
                format!(" · 失败 {}", res.members_failed)
            } else {
                String::new()
            },
            res.member_labels.join(" · ")
        ));
    }
    if res.members_skipped_disabled > 0 {
        md.push_str(&format!(
            "> 注：{} 个成员因所属 Key 已停用而跳过。\n\n",
            res.members_skipped_disabled
        ));
    }
    md.push_str(&format!(
        "**决策者**：{} · **检索文件**：{} 个 · **耗时**：{}ms\n\n",
        res.decider_ref, res.file_count, elapsed_ms
    ));
    md.push_str(&format!(
        "> 以上为分析与建议。请据此用你自己的编辑工具执行修改，并让用户确认。（原始需求：{}）",
        truncate(prompt, 80)
    ));
    md
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let t: String = s.chars().take(max).collect();
        format!("{t}…")
    }
}

/// 标准 MCP 工具成功响应
fn tool_text_content(text: &str) -> Value {
    json!({
        "content": [ { "type": "text", "text": text } ],
        "isError": false
    })
}

/// 标准 MCP 工具错误响应（isError=true，客户端会显示为工具失败）
fn tool_error_content(text: &str) -> Value {
    json!({
        "content": [ { "type": "text", "text": text } ],
        "isError": true
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // initialize 必须回显客户端请求的 protocolVersion（而非硬编码旧版）。
    // rmcp/Codex 协商更新版本时，服务器回显旧版会致握手不完整、tools/list 拿不到工具。
    #[tokio::test]
    async fn initialize_echoes_client_protocol_version() {
        let dir = std::env::temp_dir().join(format!("mcp_init_{}", std::process::id()));
        std::fs::create_dir_all(&dir).ok();
        let store = Arc::new(
            Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap(),
        );
        let params = json!({ "protocolVersion": "2025-06-18" });
        let res = dispatch(&store, "initialize", params).await.unwrap();
        assert_eq!(
            res.get("protocolVersion").and_then(|v| v.as_str()),
            Some("2025-06-18"),
            "应回显客户端请求的协议版本"
        );
        assert!(res.get("capabilities").and_then(|c| c.get("tools")).is_some());
        std::fs::remove_dir_all(&dir).ok();
    }

    // 客户端未带 protocolVersion 时回落到服务器默认版本（不 panic、不返回 null）。
    #[tokio::test]
    async fn initialize_falls_back_to_default_version() {
        let dir = std::env::temp_dir().join(format!("mcp_initf_{}", std::process::id()));
        std::fs::create_dir_all(&dir).ok();
        let store = Arc::new(
            Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap(),
        );
        let res = dispatch(&store, "initialize", Value::Null).await.unwrap();
        assert_eq!(
            res.get("protocolVersion").and_then(|v| v.as_str()),
            Some(PROTOCOL_VERSION),
            "无客户端版本时用默认版本兜底"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    // tools/list 必须返回 synaroute_ai 工具及必填参数 schema（Codex 据此才能真正挂上工具）。
    #[tokio::test]
    async fn tools_list_exposes_synaroute_ai() {
        let dir = std::env::temp_dir().join(format!("mcp_tools_{}", std::process::id()));
        std::fs::create_dir_all(&dir).ok();
        let store = Arc::new(
            Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap(),
        );
        let res = dispatch(&store, "tools/list", Value::Null).await.unwrap();
        let tools = res.get("tools").and_then(|t| t.as_array()).unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].get("name").and_then(|n| n.as_str()), Some("synaroute_ai"));
        let required = tools[0]
            .get("inputSchema")
            .and_then(|s| s.get("required"))
            .and_then(|r| r.as_array())
            .unwrap();
        assert!(required.iter().any(|v| v.as_str() == Some("prompt")), "prompt 必填");
        std::fs::remove_dir_all(&dir).ok();
    }

    // Codex 桌面端（protocol 2025-06-18）在 initialize 后会探测 resources/prompts。
    // 这些方法即便我们不提供也必须返回空列表——返回 -32601 会被客户端判定为启动失败并
    // cancel 整个连接，synaroute_ai 就进不了工具目录（曾致「工具不存在」惨案）。
    // 用 local_static_response（stdio 与 HTTP dispatch 共用同一表）锁死此行为。
    #[test]
    fn origin_gate_blocks_web_pages_but_not_native_clients() {
        // 无 Origin：Codex 的 rmcp / Claude CLI / curl 都不发，必须放行，
        // 否则等于把整个 MCP 关掉（这是最容易改错的一侧）。
        assert!(origin_allowed(None), "原生客户端不发 Origin，必须放行");
        assert!(origin_allowed(Some("")), "空 Origin 视为无");
        assert!(origin_allowed(Some("null")), "file:// 页面的 null 源放行（本机调试）");

        // 本机来源放行（含端口、含 IPv6、大小写不敏感）。
        for ok in [
            "http://localhost",
            "http://localhost:1420",
            "http://127.0.0.1:9527",
            "https://127.0.0.1",
            "http://[::1]:9527",
            "HTTP://LocalHost:5173",
            "tauri://localhost",
        ] {
            assert!(origin_allowed(Some(ok)), "本机来源应放行: {ok}");
        }

        // 外部网页一律拒绝——否则用户随手打开的页面就能驱动聚合烧额度、
        // 并借 synaroute_ai 的 cwd 参数让服务端去读任意目录。
        for bad in [
            "http://evil.example",
            "https://evil.example",
            "http://127.0.0.1.evil.example",
            "http://localhost.evil.example:80",
            "https://sub.100xlabs.space",
            "http://[2001:db8::1]:9527",
        ] {
            assert!(!origin_allowed(Some(bad)), "外部来源必须拒绝: {bad}");
        }
    }

    #[test]
    fn capability_probes_never_error() {
        for (method, key) in [
            ("resources/list", "resources"),
            ("resources/templates/list", "resourceTemplates"),
            ("prompts/list", "prompts"),
        ] {
            let res = local_static_response(method, &Value::Null)
                .unwrap_or_else(|| panic!("{method} 应由静态表处理，而非落到 tools/call"))
                .unwrap_or_else(|e| panic!("{method} 不得返回 error（会致客户端 cancel）: {e:?}"));
            let arr = res.get(key).and_then(|v| v.as_array());
            assert!(arr.is_some(), "{method} 应返回 {{{key}: []}}");
            assert!(arr.unwrap().is_empty(), "{method} 默认返回空列表");
        }
    }

    // 未知方法仍应返回 -32601（回归保护：容错只针对已知能力探测，不是无脑吞所有方法）。
    #[test]
    fn unknown_method_still_errors() {
        let res = local_static_response("does/not/exist", &Value::Null);
        match res {
            Some(Err((code, _))) => assert_eq!(code, -32601),
            other => panic!("未知方法应返回 -32601，实际: {other:?}"),
        }
    }

    // session id 唯一（每次 initialize 下发不同值，供严格客户端完成握手）。
    #[test]
    fn session_ids_are_unique() {
        let a = new_session_id();
        let b = new_session_id();
        assert_ne!(a, b);
        assert!(a.starts_with("synaroute-"));
    }

    // images 参数形状校验：不静默丢，错法都要响亮报错（FR-027）。
    #[test]
    fn parse_image_paths_accepts_valid_array() {
        let v = json!(["docs/a.png", "  b.jpg  "]);
        assert_eq!(
            parse_image_paths(Some(&v)).unwrap(),
            vec!["docs/a.png".to_string(), "b.jpg".to_string()]
        );
    }

    #[test]
    fn parse_image_paths_absent_or_null_is_empty() {
        assert!(parse_image_paths(None).unwrap().is_empty());
        assert!(parse_image_paths(Some(&Value::Null)).unwrap().is_empty());
        assert!(parse_image_paths(Some(&json!([]))).unwrap().is_empty());
    }

    #[test]
    fn parse_image_paths_rejects_bare_string_instead_of_dropping() {
        // 模型最容易犯的错：单图传成裸字符串。旧写法会静默当「没传图」，
        // 用户拿到看起来看了图实际没看的答案。必须报错。
        let e = parse_image_paths(Some(&json!("only-one.png"))).unwrap_err();
        assert!(e.contains("数组"), "{e}");
    }

    #[test]
    fn parse_image_paths_rejects_non_string_and_empty_items() {
        let e = parse_image_paths(Some(&json!(["a.png", 123]))).unwrap_err();
        assert!(e.contains("images[1]") && e.contains("不是字符串"), "{e}");
        let e = parse_image_paths(Some(&json!(["a.png", "   "]))).unwrap_err();
        assert!(e.contains("images[1]") && e.contains("空字符串"), "{e}");
    }
}
