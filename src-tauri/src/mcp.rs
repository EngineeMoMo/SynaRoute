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

async fn handle_http(
    store: Arc<Store>,
    req: Request<Incoming>,
) -> Result<Response<Full<Bytes>>, hyper::Error> {
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
    let result = dispatch(&store, method, params).await;

    match result {
        Ok(value) => Ok(json_response(StatusCode::OK, rpc_ok(id, value))),
        Err((code, message)) => Ok(json_response(
            StatusCode::OK,
            rpc_error(id, code, &message),
        )),
    }
}

// ─── JSON-RPC 分发 ───────────────────────────────────────────────────────────

fn rpc_ok(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

async fn dispatch(
    store: &Arc<Store>,
    method: &str,
    params: Value,
) -> Result<Value, (i64, String)> {
    match method {
        "initialize" => Ok(json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "SynaRoute", "version": env!("CARGO_PKG_VERSION") }
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": [tool_schema()] })),
        "tools/call" => handle_tool_call(store, params).await,
        other => Err((-32601, format!("未知方法: {other}"))),
    }
}

/// synaroute_ai 工具的 schema（Q6：基本参数）
fn tool_schema() -> Value {
    json!({
        "name": "synaroute_ai",
        "description": "调用 SynaRoute 多模型大脑聚合：多个模型并行分析当前项目代码，由决策者综合出建议或修改计划。适用于代码审查、方案设计、疑难排查等需要多视角的任务。注意：本工具只返回分析与建议，不修改文件——请根据返回的计划，用你自己的编辑工具执行修改并让用户确认。",
        "inputSchema": {
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "任务描述，例如「审查鉴权模块的安全性」或「设计一个缓存层方案」"
                },
                "cwd": {
                    "type": "string",
                    "description": "当前项目根目录的绝对路径。强烈建议传入，SynaRoute 会据此检索项目文件。省略时使用 SynaRoute 自动跟随的最近活跃项目。"
                },
                "category": {
                    "type": "string",
                    "enum": ["claude-cli", "claude-desktop", "codex"],
                    "description": "使用哪个分类下配置的 Key 池与聚合设置，省略默认 claude-cli"
                },
                "languageHint": {
                    "type": "string",
                    "description": "回答语言提示，如 zh / en，省略则跟随 prompt 语言"
                }
            },
            "required": ["prompt"]
        }
    })
}

async fn handle_tool_call(
    store: &Arc<Store>,
    params: Value,
) -> Result<Value, (i64, String)> {
    let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
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

    // 语言提示拼进 prompt（决策者/参与者都会看到）
    let effective_prompt = match &language_hint {
        Some(lang) if !lang.is_empty() => {
            format!("{prompt}\n\n（请用 {lang} 回答）")
        }
        _ => prompt.clone(),
    };

    let started = std::time::Instant::now();
    let outcome = aggregate::run_mcp(store, category, &effective_prompt, cwd.clone()).await;
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
