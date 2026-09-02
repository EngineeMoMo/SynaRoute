//! 内置 MCP（Model Context Protocol）服务器。
//!
//! 让 Codex CLI / Claude Code 等 MCP 客户端直接调用 SynaRoute 的多模型大脑聚合。
//! - 传输：Streamable HTTP（JSON-RPC 2.0 over HTTP POST），监听 127.0.0.1:{port}/mcp/{分类}
//! - 工具：单个 `synaroute_ai`，参数区分意图（Q3）
//! - 只出主意不写文件（Q5）：返回分析/修改计划，由客户端自己执行修改
//! - 不鉴权（Q9，仅 127.0.0.1）；调用日志走 store 事件流 + 落盘（Q10）
//!
//! 协议实现为手写最小子集（无 SDK 依赖）：仅支持
//! initialize / notifications/initialized / tools/list / tools/call。
//!
//! ## 🔴 分类身份来自 URL 路径段，不是工具参数
//!
//! 每个分类有自己的 Key 池、聚合成员、预算与日志页，所以「这次调用属于哪个分类」是**必须**
//! 答对的问题。早期靠 `synaroute_ai` 的 `category` 参数回答，那是错的：模型不知道自己活在哪个
//! 客户端里，只能反问用户或省略，而省略会被默认成 `claude-cli` —— 用错 Key 池、额度记在别的
//! 分类头上、Codex 的聚合日志落在 Claude CLI 页，全都是静默的。
//!
//! 现在身份由**接入那一刻**写进注册（那时分类是已知的，是我们自己在写配置）：
//! CLI 的 url 写成 `/mcp/claude-cli`；两个 stdio 端的 args 带 `--mcp-category=…`，
//! 由子进程翻成同样的路径段转发（见 [`stdio`]）。
//! 写侧 [`client_url`] 与读侧 [`caller_from_path`] 成对，有 round-trip 测试钉住。

/// stdio 端（Codex / Claude 桌面端）：客户端侧的分类身份标记 + 子进程转发。
/// 作为 `mcp` 的子模块而非平级模块 —— 它与本模块共用同一套「路径段携带身份」的方案，
/// 且要复用这里的 `local_static_response` / `rpc_ok` / 端口文件等，拆成平级只会互相 `pub`。
pub(crate) mod stdio;

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
pub(crate) const PROTOCOL_VERSION: &str = "2024-11-05";

/// MCP 端点的固定前缀。分类作为它后面的一段：`/mcp/claude-cli`。
const MCP_PATH: &str = "/mcp";

/// 旧版 stdio 注册（args 里没有分类标记）转发时用的哨兵段。
///
/// 它存在的意义是**保留信息**：服务端据此知道「这次来自某个 stdio 客户端，
/// 但它的配置是旧版」，从而能在「桌面端 / Codex」之间精确兜底（CLI 走 HTTP，到不了这里）。
/// 若让旧子进程直接打裸 `/mcp`，这个信息就丢了，只能一律当 CLI —— 那正是要修的缺陷。
///
/// `_` 开头保证永远不会撞上某个分类的 wire_id（有测试钉住）。
const STDIO_LEGACY_SEG: &str = "_stdio";

/// 一次 MCP 调用的**调用方身份**，由传输层（URL 路径段）决定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum McpCaller {
    /// 路径段是一个已知分类 —— 权威身份，不再看任何参数。
    Bound(CategoryType),
    /// 旧版 stdio 子进程（哨兵段）：必是桌面端或 Codex，但不知道是哪个。
    StdioLegacy,
    /// 裸 `/mcp` 或认不出的段：旧版 CLI 注册、docs 里的手写 curl、探活。
    Unbound,
}

/// 运行中的 MCP 服务器句柄
struct RunningMcp {
    port: u16,
    handle: JoinHandle<()>,
}

/// MCP 服务基址（不带分类段）。前端展示与重启日志用它。
pub(crate) fn base_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}{MCP_PATH}")
}

/// 写进某分类客户端配置的接入地址（**写侧**，与 [`caller_from_path`] 成对）。
///
/// 全仓唯一的取址点：注册（`service::register_and_record`）与端口漂移后的重写
/// （`service::rewrite_registered_clients`）都走它。漏挂分类段的表现是**静默**的
/// —— 客户端照样连得上，只是服务端认不出调用方、一律退回兜底分类。
pub(crate) fn client_url(port: u16, category: CategoryType) -> String {
    format!("{}/{}", base_url(port), category.as_str())
}

/// stdio 子进程的转发地址：知道自己分类就带分类段，否则走哨兵段。
pub(crate) fn forward_url(port: u16, category: Option<CategoryType>) -> String {
    match category {
        Some(c) => client_url(port, c),
        None => format!("{}/{STDIO_LEGACY_SEG}", base_url(port)),
    }
}

/// 从请求路径解析调用方身份（**读侧**）。
///
/// 🔴 认不出的段一律 [`McpCaller::Unbound`]，**绝不**静默落成 `ClaudeCli`：
/// 那样等于把「不知道」伪装成「知道」，用户会拿到一个走错 Key 池却毫无提示的结果。
/// 兜底该由 [`resolve_caller_category`] 一处集中做，并且落可见事件。
fn caller_from_path(path: &str) -> McpCaller {
    let Some(rest) = path.trim_end_matches('/').strip_prefix(MCP_PATH) else {
        return McpCaller::Unbound;
    };
    let seg = rest.trim_start_matches('/');
    if seg.is_empty() {
        return McpCaller::Unbound;
    }
    if seg == STDIO_LEGACY_SEG {
        return McpCaller::StdioLegacy;
    }
    match CategoryType::from_str(seg) {
        Some(c) => McpCaller::Bound(c),
        None => McpCaller::Unbound,
    }
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
pub(crate) fn read_mcp_port_file() -> Option<u16> {
    let path = mcp_port_file_path()?;
    let content = std::fs::read_to_string(path).ok()?;
    content.trim().parse::<u16>().ok()
}

/// 删掉端口文件。MCP 一停，这个文件就是**陈旧**的：stdio 子进程只有在主应用运行、
/// HTTP MCP 在监听时才连得上，主应用一停/一退，文件里的端口就指向一个没人监听的地址。
///
/// 留着它的坏处是**误导**：下次主应用还没起、而 Codex 又拉起 stdio 子进程时，子进程会
/// 读到这个旧端口、去连、失败 —— 本可以直接走「端口扫描兜底」少绕一圈。删掉它让
/// 「文件存在」== 「主应用此刻确实在监听那个端口」这个不变量成立。
/// 删除失败只告警：文件陈旧不致命，下次 `write_mcp_port_file` 会覆盖它。
fn clear_mcp_port_file() {
    let Some(path) = mcp_port_file_path() else { return };
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        // 本来就不存在 = 已是期望状态，不算错。
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!("清理 MCP 端口文件失败({}): {e}", path.display()),
    }
}

/// 端口占用时，向上探测的最大候选数（configured .. configured+FALLBACK_RANGE）。
/// 9527 等常用端口在部分 Windows 机器上被系统进程（WUDFHost / GoodixSessionService 等）占用，
/// 单端口硬绑定会静默失败，故自动向上找一个可用端口，并把真实端口暴露给 UI。
pub(crate) const FALLBACK_RANGE: u16 = 20;

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
        // 把实际端口写到平台对应的共享文件（见 mcp_port_file_path）：
        // Windows 放 exe 同级以躲 MSIX AppData 虚拟化；macOS 放 Application Support，
        // 避免写进 .app bundle。stdio 子进程据此找到主应用并转发 tools/call。
        write_mcp_port_file(bound);
        tracing::info!("MCP 服务器已启动: http://127.0.0.1:{bound}/mcp");
        Ok(bound)
    }

    pub fn stop(&self) {
        if let Some(r) = self.running.lock().take() {
            r.handle.abort();
            // 端口文件与「服务在监听」绑定：停了就删，别留个指向死端口的文件误导 stdio 子进程。
            clear_mcp_port_file();
            tracing::info!("MCP 服务器已停止");
        }
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
    // 调用方身份必须在 `into_body()` **之前**取：那一步会消耗掉整个 Request（含 uri）。
    let caller = caller_from_path(req.uri().path());

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
    let result = dispatch(&store, method, params, caller).await;

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

pub(crate) fn rpc_ok(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

pub(crate) fn rpc_error(id: Value, code: i64, message: &str) -> Value {
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
pub(crate) fn local_static_response(
    method: &str,
    params: &Value,
) -> Option<Result<Value, (i64, String)>> {
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
    caller: McpCaller,
) -> Result<Value, (i64, String)> {
    match local_static_response(method, &params) {
        Some(res) => res,
        // tools/call：HTTP 路径本地执行（主应用持有真实配置）。
        None => handle_tool_call(store, params, caller).await,
    }
}

/// 这次调用属于哪个分类。**唯一决策点** —— 别在任何别的地方再算一遍。
///
/// 优先级（从最可信到最兜底）：
/// 1. **传输层身份**：路径段里的分类。它是「接入」那一刻由我们自己写进客户端配置的，
///    因此权威 —— 此时**不看参数**，免得模型瞎填一个值把正确答案覆盖掉。
/// 2. `arguments.category`：只在传输层认不出时用。工具 schema 已不再暴露它
///    （见 [`tool_schema`]），保留读取纯为兼容：旧版注册尚未被重写、docs 里的手写
///    curl、用户自己写的 hook 提示词。
/// 3. **旧版 stdio 的精确兜底**：哨兵段说明「来自某个 stdio 客户端」，而 stdio 只可能是
///    桌面端或 Codex（CLI 走 HTTP）。若这两个里只有一个接入过，那就是它 —— 这是在两个里
///    排除，不是在三个里猜。
/// 4. `claude-cli`：历史默认。裸 `/mcp` 本就只可能是旧版 CLI 注册或手写 curl。
///
/// 2~4 档都会落一条可见事件（[`warn_unbound_once`]），因为它们都意味着
/// 「客户端配置是旧版、该重启一次」。
fn resolve_caller_category(store: &Store, caller: McpCaller, args: &Value) -> CategoryType {
    if let McpCaller::Bound(c) = caller {
        return c;
    }
    let resolved = pick_unbound_category(store, caller, args);
    warn_unbound_once(store, resolved);
    resolved
}

/// [`resolve_caller_category`] 的第 2~4 档（抽出来是为了能不带事件副作用地单测）。
fn pick_unbound_category(store: &Store, caller: McpCaller, args: &Value) -> CategoryType {
    args.get("category")
        .and_then(|v| v.as_str())
        .and_then(CategoryType::from_str)
        .or_else(|| match caller {
            McpCaller::StdioLegacy => sole_registered_stdio_category(store),
            _ => None,
        })
        .unwrap_or(CategoryType::ClaudeCli)
}

/// 两个 stdio 端（桌面端 / Codex）里是否**只有一个**接入过 MCP。都接入或都没接入返回 None。
fn sole_registered_stdio_category(store: &Store) -> Option<CategoryType> {
    let registered = store.get_settings().mcp_registered_categories;
    let mut it = [CategoryType::ClaudeDesktop, CategoryType::Codex]
        .into_iter()
        .filter(|c| registered.contains(c));
    let only = it.next()?;
    it.next().is_none().then_some(only)
}

/// 已就「调用方未携带分类标识」告警过的分类位图（每次应用运行、每个分类只落一条）。
///
/// **必须节流**：不节流就是每次工具调用刷一条，把有用事件挤出 `MAX_EVENTS`(500) 环 ——
/// 与已修过的「短路窗口内每次客户端重发都记一条『已忽略熔断兜底重试』」是同一个坑。
/// 一条/分类/运行 恰好对上补救动作的粒度（重启那个客户端一次）。
static UNBOUND_WARNED: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

fn warn_unbound_once(store: &Store, resolved: CategoryType) {
    use std::sync::atomic::Ordering;
    let idx = CategoryType::ALL.iter().position(|c| *c == resolved).unwrap_or(0);
    let bit = 1u8 << idx;
    if UNBOUND_WARNED.fetch_or(bit, Ordering::Relaxed) & bit != 0 {
        return;
    }
    store.append_event(
        resolved,
        "config",
        None,
        &format!(
            "MCP 调用方未携带分类标识（客户端配置为旧版），已按「{}」处理。\
             应用启动时会自动重写客户端配置，把该客户端重启一次后此提示消失。",
            resolved.display_name()
        ),
    );
}

/// synaroute_ai 工具的 schema（Q6：基本参数）
///
/// 🔴 **不得再加 `category`**：分类由传输层决定（见模块头）。把它放回 schema 会让模型
/// 反过来问用户「当前是哪个分类」—— 而模型无从得知，用户也没理由需要知道。
/// `tool_schema_does_not_advertise_category` 那条测试专盯这件事。
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
    caller: McpCaller,
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
    // 分类来自**传输层**（接入时写进客户端配置的路径段），不是模型传的参数。
    let category = resolve_caller_category(store, caller, &args);
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
                request_id: String::new(),
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
                was_truncated: None,
            };
            store.append_event_trace(category, "mcp", None, &detail, Some(trace));

            let md = format_markdown(&prompt, &res, elapsed);
            Ok(tool_text_content(&md))
        }
        Err(e) => {
            let trace = crate::model::RequestTrace {
                request_id: String::new(),
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
                was_truncated: None,
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
        // 出路必须给两条：「重新启用」会让这条 Key 回到故障转移池，把当初禁用它的那个
        // 404 带回主链路 —— 只说这一条等于把用户指去做一个已知有害的操作。
        md.push_str(&format!(
            "> 注：{} 个成员因所属 Key 已停用而跳过（在分类页重新启用，或在该 Key 的卡片上勾选「允许大脑聚合使用」）。\n\n",
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
pub(crate) fn tool_error_content(text: &str) -> Value {
    json!({
        "content": [ { "type": "text", "text": text } ],
        "isError": true
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 端口文件的写 → 读 → 清 往返，并钉住「清掉后再清不报错」（幂等）。
    ///
    /// `clear_mcp_port_file` 是优雅停机的一环：MCP 一停就删文件，让「文件存在」
    /// == 「此刻真有 HTTP MCP 在监听那个端口」这个不变量成立，不给下次启动留个
    /// 指向死端口的陈旧文件误导 stdio 子进程。
    #[test]
    fn mcp_port_file_write_read_clear_roundtrip() {
        // 用真实路径解析（Windows=exe 同级、mac=Application Support）；测试二进制自己的
        // 目录里写一个 mcp-port，不碰用户数据。若环境拿不到路径（极少见），跳过而非误判。
        let Some(path) = mcp_port_file_path() else {
            return;
        };
        // 先清干净，避免上一次遗留影响断言。
        clear_mcp_port_file();

        write_mcp_port_file(48123);
        assert_eq!(
            read_mcp_port_file(),
            Some(48123),
            "写入后应能读回同一端口"
        );

        clear_mcp_port_file();
        assert_eq!(
            read_mcp_port_file(),
            None,
            "清理后端口文件应消失（下次 stdio 子进程会退回端口扫描，而非连一个死端口）"
        );

        // 幂等：文件已不存在时再清一次不得报错/ panic。
        clear_mcp_port_file();
        assert_eq!(read_mcp_port_file(), None);

        // 顺带确认文件真在盘上没了。
        assert!(!path.exists(), "clear 之后文件不应还在: {}", path.display());
    }

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
        let res = dispatch(&store, "initialize", params, McpCaller::Unbound).await.unwrap();
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
        let res = dispatch(&store, "initialize", Value::Null, McpCaller::Unbound).await.unwrap();
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
        let res = dispatch(&store, "tools/list", Value::Null, McpCaller::Unbound).await.unwrap();
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

    /// 建一个隔离的临时 Store（测试专用；绝不碰开发机的真实配置）。
    fn tmp_store(tag: &str) -> (std::path::PathBuf, Arc<Store>) {
        // 加进程内自增序号：同一进程里多个用例并发跑，只靠 pid 会撞同一个目录，
        // 表现是彼此删掉对方的 config.json（偶发红）。同 store.rs 里 db_copy_path 那个坑。
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "mcp_{tag}_{}_{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).ok();
        let store =
            Arc::new(Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap());
        (dir, store)
    }

    /// 🔴 用户报的那条：schema 里**不得**再出现 `category`。
    ///
    /// 它在时模型会反问用户「当前是哪个工具分类」—— 而模型无从得知（它不知道自己活在
    /// 哪个客户端里），用户也没理由需要知道。分类由传输层携带（见模块头）。
    #[test]
    fn tool_schema_does_not_advertise_category() {
        let schema = tool_schema();
        let props = schema["inputSchema"]["properties"].as_object().unwrap();
        assert!(
            !props.contains_key("category"),
            "category 不得回到 schema —— 它一在，模型就会反过来问用户是哪个分类"
        );
        // 其余参数必须还在（别把这条测试变成「删光参数也绿」）。
        for k in ["prompt", "cwd", "languageHint", "images"] {
            assert!(props.contains_key(k), "{k} 应仍在 schema 里");
        }
        assert_eq!(
            schema["inputSchema"]["required"],
            json!(["prompt"]),
            "prompt 仍是唯一必填项"
        );
    }

    /// 写侧（注册时写进客户端配置的 url）与读侧（服务端解析路径）必须成对。
    /// 改了一侧忘了另一侧的表现是**静默**的：客户端照样连得上，只是身份丢了、
    /// 一律退回兜底分类。
    ///
    /// 同时钉住「写进客户端配置的地址**必须带分类段**」：做故障注入时把 `client_url`
    /// 改回返回裸基址，只验 tools.rs 那侧的用例会**照样全绿**（注入不变红 = 什么都没测到），
    /// 因为那边只验证「给它带分类段的 url 就会写下去」，不管谁决定带不带。
    #[test]
    fn client_url_round_trips_through_caller_from_path() {
        let base = base_url(9527);
        let mut seen = std::collections::HashSet::new();
        for c in CategoryType::ALL {
            let url = client_url(9527, c);
            assert!(url.ends_with(c.as_str()), "url 应以分类结尾: {url}");
            assert_ne!(url, base, "{c:?} 的接入地址不得等于裸基址（那就丢了身份）");
            assert!(seen.insert(url.clone()), "三个分类的接入地址必须互不相同");
            // 取 url 的 path 部分（去掉 scheme+host）再解析，模拟服务端拿到的东西。
            let path = url.strip_prefix("http://127.0.0.1:9527").unwrap();
            assert_eq!(
                caller_from_path(path),
                McpCaller::Bound(c),
                "{c:?} 的地址应能解析回同一分类"
            );
        }
        assert_eq!(seen.len(), CategoryType::ALL.len());
    }

    /// 端到端：**真起一个 HTTP 监听**，按 `/mcp/codex` 打一次 `tools/call`，
    /// 断言事件落在 Codex 分类下。
    ///
    /// 🔴 这条补的是一处真实盲区：`caller_from_path` 自己有单测、`dispatch` 也有，但
    /// **`handle_http` 里「取 path → 传给 dispatch」这一步接线**原先没有任何覆盖 ——
    /// 把它改成硬编码 `McpCaller::Unbound` 时全套 776 条测试**照样全绿**，
    /// 而那正是本轮要修的缺陷本身（每个客户端都静默退回 claude-cli）。
    ///
    /// 刻意**不走 `McpManager::start`**：它会写 exe 同级的端口文件，与
    /// `mcp_port_file_write_read_clear_roundtrip` 抢同一个文件 → 引入偶发红。
    /// 这里自己 bind 端口 0（由 OS 分配空闲端口，不会撞）并直接挂 `handle_http`。
    #[tokio::test]
    async fn handle_http_derives_the_caller_from_the_request_path() {
        let (dir, store) = tmp_store("httpcaller");
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let srv_store = store.clone();
        let server = tokio::spawn(async move {
            // 只需服务本用例发起的那几个连接；连接结束即退出循环。
            for _ in 0..4 {
                let Ok((stream, _)) = listener.accept().await else { break };
                let s = srv_store.clone();
                tokio::spawn(async move {
                    let svc = service_fn(move |req| handle_http(s.clone(), req));
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(stream), svc)
                        .await;
                });
            }
        });

        let client = reqwest::Client::new();
        let body = json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "synaroute_ai", "arguments": { "prompt": "问点什么" } }
        });
        // 打分类专属端点 —— 与注册时写进客户端配置的地址同形（client_url）。
        let url = client_url(port, CategoryType::Codex);
        let resp: Value = client
            .post(&url)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .expect("HTTP 请求应发得出去")
            .json()
            .await
            .expect("响应应是 JSON");
        // 未配置聚合 → 工具级错误；本条的判据不是它，而是事件落在哪个分类。
        assert_eq!(resp["result"]["isError"], json!(true), "实际响应: {resp}");

        assert!(
            store.list_events(CategoryType::Codex).iter().any(|e| e.kind == "mcp"),
            "打 /mcp/codex 后事件必须落在 Codex 分类下"
        );
        assert!(
            !store.list_events(CategoryType::ClaudeCli).iter().any(|e| e.kind == "mcp"),
            "不得落到 claude-cli（把 handle_http 的路径解析去掉就会这样）"
        );

        server.abort();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 🔴 下游断连后，在途的聚合必须**真的停下**，而不是继续烧上游额度。
    ///
    /// # 为什么这条必须有（2026-09-01 用户实报的第二半）
    ///
    /// 现场：用户等了 7 分 47 秒后按停止 → Codex 发 `notifications/cancelled` →
    /// stdio 层丢弃转发（`stdio::cancels_request`）→ **HTTP 连接断开**。
    /// 而那一刻聚合还在跑，我们 22 秒后才写完结果。
    ///
    /// 「断连之后聚合还在不在跑」决定了这次取消是**真的止损**还是只是不写响应：
    /// 后者的表现是用户按了停止、额度继续烧 20 秒（多模型并行时是几倍）。
    /// 修 stdio 那一层时只验到「不写响应」，**没验过这一步** —— 本用例补的正是它。
    ///
    /// # 判据：future 被 drop
    ///
    /// `handle_http` 里是 `dispatch(...).await`。Rust 的 async 是**惰性**的 ——
    /// 承载它的 future 一被 drop，其中所有未完成的 await 点一并停止，
    /// 不需要我们自己接 cancel token。本用例用一个「drop 时留痕」的哨兵证明这一点。
    ///
    /// **刻意不调真的聚合**：那要打真实上游。这里验的是承载机制（hyper 会不会在断连时
    /// drop 处理 future）；「聚合确实长在这个 future 上」由紧邻的源码级判据保证。
    #[tokio::test]
    async fn a_disconnected_client_must_cancel_the_in_flight_work() {
        use std::sync::atomic::{AtomicBool, Ordering};
        static DROPPED: AtomicBool = AtomicBool::new(false);
        static STARTED: AtomicBool = AtomicBool::new(false);

        struct Sentinel;
        impl Drop for Sentinel {
            fn drop(&mut self) {
                DROPPED.store(true, Ordering::SeqCst);
            }
        }

        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let Ok((stream, _)) = listener.accept().await else { return };
            let svc = service_fn(move |_req| async move {
                let _guard = Sentinel; // 与「工作」同生命周期
                STARTED.store(true, Ordering::SeqCst);
                // 模拟一次长聚合：远长于客户端的等待时间
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                Ok::<_, hyper::Error>(empty_response(StatusCode::OK))
            });
            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(stream), svc)
                .await;
        });

        // 发一个请求然后**中途断开**（超时即 drop 掉整个 reqwest future → 连接关闭）
        let client = reqwest::Client::new();
        let r = client
            .post(format!("http://127.0.0.1:{port}/mcp/codex"))
            .timeout(std::time::Duration::from_millis(600))
            .body("{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\"}")
            .send()
            .await;
        assert!(r.is_err(), "夹具前提：这次请求必须因超时而中断，而不是拿到响应");

        // 给 hyper 一点时间感知对端关闭
        for _ in 0..40 {
            if DROPPED.load(Ordering::SeqCst) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(STARTED.load(Ordering::SeqCst), "夹具前提：处理必须真的开始过");
        assert!(
            DROPPED.load(Ordering::SeqCst),
            "下游断连后在途工作没有被取消 —— 用户按了停止，聚合却还在烧额度"
        );
        server.abort();
    }

    /// 上一条的另一半：聚合必须**长在**那个会被 drop 的 future 上。
    ///
    /// 上面那条验的是 hyper 的承载机制，用的是自造的 `service_fn` —— 把真实的
    /// `dispatch` 改成 `tokio::spawn`，它**照样绿**。而 spawn 出去的任务有自己的
    /// 生命周期：连接断了它照样跑完、照样烧额度，也就是取消静默失效。
    ///
    /// # ⚠️ 这条判据**弱于**它看起来的样子，写明免得被当成护栏
    ///
    /// 注入实测（把这一行换成 `spawn` + `h.await`）的结果是 **E0597 编译不过**：
    /// `method`/`params` 借自本函数的局部量，而 `tokio::spawn` 要求 `'static`。
    /// 也就是说**真正拦住这个改法的是借用检查器，不是本判据** —— 谁要绕过去，
    /// 得先把那几个值都 clone 成 owned，那是一次显眼的改动，不会是手滑。
    ///
    /// 保留它的理由是钉住**形态**（那行必须长这样），让改动者读到上面这段说明；
    /// 而不是假装这里有一道机械防线。同本仓「把硬保证写成软保证是退步」那条 ——
    /// 这里反过来：软保证就该写明自己是软的。
    #[test]
    fn dispatch_must_be_awaited_inline_so_a_disconnect_cancels_it() {
        let src = crate::proxy::custom_headers::production_code_only(include_str!("mcp.rs"));
        // 🔴 只看生产段：本判据自己的字面量在测试段里，一起扫会「自我满足」而恒绿
        // —— 本仓已栽过五次的那个坑（写注入脚本时又撞了一次，锚点命中 2 处）。
        let prod = &src[..src.find("mod tests").unwrap_or(src.len())];
        assert!(
            prod.contains("dispatch(&store, method, params, caller).await"),
            "dispatch 必须在 handle_http 里直接 await —— 换成 spawn 会让「下游断连即止损」失效"
        );
        // 🔴 链条的最后一环，也是**唯一会静默断掉**的一环：聚合自己不许 spawn。
        //
        // 上面两条只保证「承载 future 被 drop」+「dispatch 长在它上面」。但聚合内部若
        // 把成员调用 `tokio::spawn` 出去，那些任务就有了自己的生命周期 —— 连接断了照样
        // 跑完、照样烧额度，而外层三条判据**全绿**。
        //
        // 今天的实现是 `join_all` + `Semaphore`（全内联 future），故取消能一路传到每个
        // 成员的 HTTP 请求上。谁为「并发更好控制」改成 `JoinSet`/`spawn`，这条就变红 ——
        // 那不是不能做，而是必须同时接一条取消通路，别让它静默退化。
        let agg = crate::proxy::custom_headers::production_code_only(include_str!("aggregate.rs"));
        let agg_prod = &agg[..agg.find("mod tests").unwrap_or(agg.len())];
        assert!(
            !agg_prod.contains("tokio::spawn"),
            "聚合生产段出现了 tokio::spawn：那些任务不随连接取消，用户按停止后仍会烧额度"
        );
    }

    /// 用户拿到一个走错 Key 池、却毫无提示的结果。
    #[test]
    fn bare_and_unknown_paths_are_unbound_not_claude_cli() {
        for p in [
            "/mcp",             // 旧版 CLI 注册 / docs 里的手写 curl
            "/mcp/",            // 带尾斜杠
            "/",                // 有些客户端会探测根路径
            "/mcp/claude-clii", // 手改配置时打错一个字母
            "/mcp/CLAUDE-CLI",  // 大小写不同不算同一个（wire_id 是精确值）
            "/mcpfoo",          // 前缀像但不是
            "/other/codex",     // 分类段不在 /mcp 之下
        ] {
            let got = caller_from_path(p);
            assert_eq!(got, McpCaller::Unbound, "{p} 应判为 Unbound，实际 {got:?}");
        }
    }

    /// 哨兵段必须永远不可能撞上某个分类的 wire_id —— 撞了的话那个分类的正常调用
    /// 会被当成「旧版 stdio」走兜底推断。
    #[test]
    fn stdio_legacy_segment_is_recognized_and_never_a_real_category() {
        assert_eq!(
            caller_from_path(&format!("/mcp/{STDIO_LEGACY_SEG}")),
            McpCaller::StdioLegacy
        );
        for c in CategoryType::ALL {
            assert_ne!(c.as_str(), STDIO_LEGACY_SEG, "{c:?} 的 wire_id 撞上了哨兵段");
        }
        // forward_url 的两条分支：知道分类走分类段，不知道走哨兵段。
        assert_eq!(forward_url(9527, Some(CategoryType::Codex)), "http://127.0.0.1:9527/mcp/codex");
        assert_eq!(forward_url(9527, None), "http://127.0.0.1:9527/mcp/_stdio");
    }

    /// 🔴 传输层身份**压过**参数。模型若瞎填一个 category（它无从得知正确值），
    /// 不得覆盖掉我们在接入时写死的那个答案。
    #[tokio::test]
    async fn transport_identity_beats_explicit_argument() {
        let (dir, store) = tmp_store("bound");
        let args = json!({ "category": "claude-cli" });
        assert_eq!(
            resolve_caller_category(&store, McpCaller::Bound(CategoryType::Codex), &args),
            CategoryType::Codex,
            "路径段说 codex，参数说 claude-cli —— 必须听路径段"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 参数只在传输层认不出时才生效（旧配置 / 手写 curl 的兼容路径）。
    #[test]
    fn explicit_argument_is_used_only_when_transport_is_unbound() {
        let (dir, store) = tmp_store("unbound");
        assert_eq!(
            pick_unbound_category(&store, McpCaller::Unbound, &json!({ "category": "codex" })),
            CategoryType::Codex,
            "认不出调用方时，显式参数应生效"
        );
        // 非法值不得让整次调用报错，落回历史默认。
        assert_eq!(
            pick_unbound_category(&store, McpCaller::Unbound, &json!({ "category": "nope" })),
            CategoryType::ClaudeCli
        );
        assert_eq!(
            pick_unbound_category(&store, McpCaller::Unbound, &Value::Null),
            CategoryType::ClaudeCli,
            "裸 /mcp 且无参数 → claude-cli（HTTP 本就只有 CLI 会用）"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 旧版 stdio（哨兵段）的精确兜底：stdio 只可能是桌面端或 Codex（CLI 走 HTTP），
    /// 所以这是**在两个里排除**，不是在三个里猜。两个都接入时才退回默认。
    #[test]
    fn stdio_legacy_resolves_to_the_only_registered_stdio_category() {
        let (dir, store) = tmp_store("stdiolegacy");

        // 一个都没接入 → 无从判断，退回历史默认。
        assert_eq!(
            pick_unbound_category(&store, McpCaller::StdioLegacy, &Value::Null),
            CategoryType::ClaudeCli
        );

        // 只有 Codex 接入 → 必然是它。（若这里退回 claude-cli，就是本轮要修的那个缺陷。）
        store.add_registered_category(CategoryType::Codex).unwrap();
        assert_eq!(
            pick_unbound_category(&store, McpCaller::StdioLegacy, &Value::Null),
            CategoryType::Codex,
            "两个 stdio 端里只有 Codex 接入过，就该是 Codex"
        );

        // CLI 也接入不影响：它走 HTTP，不参与 stdio 的排除。
        store.add_registered_category(CategoryType::ClaudeCli).unwrap();
        assert_eq!(
            pick_unbound_category(&store, McpCaller::StdioLegacy, &Value::Null),
            CategoryType::Codex,
            "claude-cli 走 HTTP，不该把 stdio 的判断搅浑"
        );

        // 两个 stdio 端都接入 → 真的分不出，退回默认（并由 warn_unbound_once 告知用户）。
        store.add_registered_category(CategoryType::ClaudeDesktop).unwrap();
        assert_eq!(
            pick_unbound_category(&store, McpCaller::StdioLegacy, &Value::Null),
            CategoryType::ClaudeCli,
            "两个都接入时无从判断"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 端到端：带分类身份的 tools/call，事件必须落在**那个**分类下。
    ///
    /// 这条盯的是修复前最隐蔽的后果：Codex 的聚合调用记在「Claude CLI」的运行日志里，
    /// 用户在 Codex 页怎么翻都看不到自己刚才那次调用。
    /// 聚合本身会因该分类未配置而失败，但失败事件同样带解析后的分类，故无需真上游。
    #[tokio::test]
    async fn tool_call_events_are_filed_under_the_transport_category() {
        let (dir, store) = tmp_store("evtcat");
        let params = json!({
            "name": "synaroute_ai",
            "arguments": { "prompt": "随便问点什么" }
        });
        let res = dispatch(
            &store,
            "tools/call",
            params,
            McpCaller::Bound(CategoryType::Codex),
        )
        .await
        .unwrap();
        // 未配置聚合 → isError，但这不是本条的判据。
        assert_eq!(res["isError"], json!(true));

        let codex_evts = store.list_events(CategoryType::Codex);
        let cli_evts = store.list_events(CategoryType::ClaudeCli);
        assert!(
            codex_evts.iter().any(|e| e.kind == "mcp"),
            "MCP 事件必须落在 codex 分类下，实际 codex={codex_evts:?}"
        );
        assert!(
            !cli_evts.iter().any(|e| e.kind == "mcp"),
            "不得落到 claude-cli 分类（修复前正是这样）"
        );
        // 报错文案要点出是哪个分类，否则用户不知道该去配哪一页。
        let text = res["content"][0]["text"].as_str().unwrap_or_default();
        assert!(
            text.contains(CategoryType::Codex.display_name()),
            "报错应点明分类名，实际: {text}"
        );
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
