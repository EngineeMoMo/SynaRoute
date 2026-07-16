//! 本地路由代理服务 + 故障转移引擎（FR-007/008/009）。
//!
//! - 每个分类监听独立本地端口（arch-decisions 端点编排方案A）。
//! - 故障转移优先：按优先级尝试启用 Key，失败/超时切下一个（CON-3）。
//! - 转发时套用该 Key 的模型映射（FR-006）与参数。
//! - 跨协议时用 upstream 的转换（MVP 非流式）。
//! - 仅监听 127.0.0.1（NFR-007），除非设置开启局域网。

use crate::error::{AppError, AppResult};
use crate::health;
use crate::model::{CategoryType, Protocol, ProviderKey, RequestTrace};
use crate::store::Store;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use parking_lot::Mutex;
use serde_json::Value;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

/// 运行中的代理句柄
struct RunningProxy {
    port: u16,
    handle: JoinHandle<()>,
}

/// 代理管理器：管理各分类的代理生命周期
pub struct ProxyManager {
    store: Arc<Store>,
    running: Mutex<HashMap<String, RunningProxy>>,
}

impl ProxyManager {
    pub fn new(store: Arc<Store>) -> Self {
        Self { store, running: Mutex::new(HashMap::new()) }
    }

    pub fn port_of(&self, category: CategoryType) -> Option<u16> {
        self.running.lock().get(category.as_str()).map(|p| p.port)
    }

    pub fn is_running(&self, category: CategoryType) -> bool {
        self.running.lock().contains_key(category.as_str())
    }

    /// 启动某分类的代理，返回监听端口。
    pub async fn start(&self, category: CategoryType) -> AppResult<u16> {
        if let Some(p) = self.port_of(category) {
            return Ok(p);
        }
        let lan = self.store.get_settings().lan_exposure;
        let host = if lan { [0, 0, 0, 0] } else { [127, 0, 0, 1] };

        // 端口 0 = 由 OS 分配可用端口，避免冲突（FR-008 端口冲突自动改用）
        let listener = TcpListener::bind(SocketAddr::from((host, 0)))
            .await
            .map_err(|e| AppError::Proxy(format!("绑定端口失败: {e}")))?;
        let port = listener
            .local_addr()
            .map_err(|e| AppError::Proxy(e.to_string()))?
            .port();

        let store = self.store.clone();
        let handle = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else { break };
                let io = TokioIo::new(stream);
                let store = store.clone();
                tokio::spawn(async move {
                    let svc = service_fn(move |req| handle_request(store.clone(), category, req));
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(io, svc)
                        .await;
                });
            }
        });

        self.running
            .lock()
            .insert(category.as_str().to_string(), RunningProxy { port, handle });
        Ok(port)
    }

    pub fn stop(&self, category: CategoryType) {
        if let Some(p) = self.running.lock().remove(category.as_str()) {
            p.handle.abort();
        }
    }
}

/// 处理一次下游工具请求：故障转移路由。
async fn handle_request(
    store: Arc<Store>,
    category: CategoryType,
    req: Request<Incoming>,
) -> Result<Response<Full<Bytes>>, hyper::Error> {
    let path = req.uri().path().to_string();

    // 透传下游（Claude Code / CLI 等客户端）的原始请求头，供中转商做客户端身份校验
    // （FR：部分分组只放行 Claude Code 客户端，靠 User-Agent/anthropic-beta/x-app 等识别）。
    // 排除会与代理自身冲突的头：鉴权（用 Key 自己的）、host/content-length（reqwest 重算）、
    // hop-by-hop（connection/keep-alive 等）、以及 accept-encoding（避免上游返回压缩体导致快照乱码）。
    let fwd_headers: Vec<(String, String)> = req
        .headers()
        .iter()
        .filter_map(|(name, val)| {
            let n = name.as_str().to_ascii_lowercase();
            if is_stripped_header(&n) {
                return None;
            }
            val.to_str().ok().map(|v| (n, v.to_string()))
        })
        .collect();

    let body_bytes = match req.into_body().collect().await {
        Ok(c) => c.to_bytes(),
        Err(_) => return Ok(error_resp(StatusCode::BAD_REQUEST, "读取请求体失败")),
    };

    let req_json: Value = serde_json::from_slice(&body_bytes).unwrap_or(Value::Null);
    let requested_model = req_json
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .to_string();

    // 候选：启用 + 未熔断，按优先级
    let candidates: Vec<ProviderKey> = store
        .enabled_keys_sorted(category)
        .into_iter()
        .filter(|k| health::is_candidate(&k.health))
        .collect();

    if candidates.is_empty() {
        return Ok(error_resp(
            StatusCode::SERVICE_UNAVAILABLE,
            "无可用 Key（全部停用或熔断）",
        ));
    }

    // 调用模型日志开关（默认关）：开启后每次转发尝试记一条 request 事件（含完整链路快照）
    let req_log = store.get_settings().request_log_enabled;

    // 下游发来的原始请求体快照（映射/转换前），供日志展示「我发出去的请求」
    let downstream_body = if req_json.is_null() {
        String::from_utf8_lossy(&body_bytes).chars().take(REQ_LOG_CAP).collect()
    } else {
        serde_json::to_string_pretty(&req_json).unwrap_or_else(|_| req_json.to_string())
    };

    // 记录一条完整链路的调用模型日志（开关开启时）。
    let log_request = |store: &Arc<Store>,
                       key: &ProviderKey,
                       elapsed: u64,
                       url: String,
                       real_model: String,
                       request_body: String,
                       response_body: String,
                       status: Option<u16>,
                       ok: bool| {
        if !req_log {
            return;
        }
        let shown_model = if requested_model.is_empty() { "?" } else { &requested_model };
        let detail = if ok {
            format!("{} · {} → {} · {}ms", key.name, shown_model, real_model, elapsed)
        } else {
            format!(
                "{} · {} → {} · {}ms · 失败{}",
                key.name,
                shown_model,
                real_model,
                elapsed,
                status.map(|s| format!(" HTTP {s}")).unwrap_or_default()
            )
        };
        let trace = RequestTrace {
            key_name: key.name.clone(),
            vendor: key.vendor.clone(),
            protocol: key.protocol,
            url,
            requested_model: requested_model.clone(),
            real_model,
            request_body: cap(&request_body),
            response_body: cap(&response_body),
            status,
            latency_ms: elapsed,
            ok,
        };
        store.append_event_trace(category, "request", Some(&key.id), &detail, Some(trace));
    };

    let mut last_err = String::new();
    for key in &candidates {
        let started = std::time::Instant::now();
        let result = forward_to_key(&store, key, &path, &req_json, &requested_model, &fwd_headers).await;
        let elapsed = started.elapsed().as_millis() as u64;
        match result {
            Ok(outcome) if outcome.ok => {
                let resp_text = String::from_utf8_lossy(&outcome.bytes).to_string();
                log_request(
                    &store,
                    key,
                    elapsed,
                    outcome.url.clone(),
                    outcome.real_model.clone(),
                    outcome.request_body.clone(),
                    resp_text,
                    Some(outcome.status),
                    true,
                );
                store.append_event(
                    category,
                    "route",
                    Some(&key.id),
                    &format!("{} 成功返回 {}", key.name, requested_model),
                );
                return Ok(json_resp(StatusCode::OK, outcome.bytes));
            }
            // 上游有响应但非 2xx：记完整快照（含上游返回的真实错误体），再切下一个
            Ok(outcome) => {
                let resp_text = String::from_utf8_lossy(&outcome.bytes).to_string();
                let snippet: String = resp_text.trim().chars().take(400).collect();
                last_err = if snippet.is_empty() {
                    format!("HTTP {}", outcome.status)
                } else {
                    format!("HTTP {}: {}", outcome.status, snippet)
                };
                log_request(
                    &store,
                    key,
                    elapsed,
                    outcome.url.clone(),
                    outcome.real_model.clone(),
                    outcome.request_body.clone(),
                    resp_text,
                    Some(outcome.status),
                    false,
                );
                store.append_event(
                    category,
                    "failover",
                    Some(&key.id),
                    &format!("{} 失败（{}），尝试下一个", key.name, last_err),
                );
            }
            // 连接层失败（无响应）：也记一条日志，response_body 为错误信息
            Err(e) => {
                last_err = e.to_string();
                log_request(
                    &store,
                    key,
                    elapsed,
                    String::new(),
                    mapped_model(key, &requested_model),
                    downstream_body.clone(),
                    last_err.clone(),
                    None,
                    false,
                );
                store.append_event(
                    category,
                    "failover",
                    Some(&key.id),
                    &format!("{} 失败（{}），尝试下一个", key.name, last_err),
                );
            }
        }
    }

    store.append_event(category, "error", None, &format!("全部 Key 失败: {last_err}"));
    Ok(error_resp(
        StatusCode::BAD_GATEWAY,
        &format!("全部 Key 不可用：{last_err}"),
    ))
}

/// 请求/响应体在日志里的最大字符数（防止超大 body 撑爆内存日志）
const REQ_LOG_CAP: usize = 20_000;

/// 截断超长文本，附省略提示。
fn cap(s: &str) -> String {
    if s.chars().count() <= REQ_LOG_CAP {
        s.to_string()
    } else {
        let head: String = s.chars().take(REQ_LOG_CAP).collect();
        format!("{head}\n…（已截断，共 {} 字符）", s.chars().count())
    }
}

/// 一次转发的完整结果：既供路由决策（bytes/ok），也供调用模型日志（url/model/body 快照）。
struct ForwardOutcome {
    /// 上游返回体字节（成功时用于回给下游）
    bytes: Bytes,
    /// 实际请求的上游完整 URL
    url: String,
    /// 映射后实际发往上游的模型名
    real_model: String,
    /// 发往上游的请求体（已做模型映射+协议转换；不含鉴权头）
    request_body: String,
    /// 上游 HTTP 状态码
    status: u16,
    /// 是否 2xx
    ok: bool,
}

/// 转发到单个 Key：套用模型映射 + 协议适配。
/// 返回完整 outcome（含发往上游的请求体、响应体、状态），供路由与调用模型日志共用。
/// 注意：非 2xx 不再直接返回 Err，而是照常返回 outcome（ok=false），
/// 由调用方决定是否切换——这样失败也能被完整记进调用模型日志。
async fn forward_to_key(
    store: &Arc<Store>,
    key: &ProviderKey,
    path: &str,
    req_json: &Value,
    requested_model: &str,
    fwd_headers: &[(String, String)],
) -> AppResult<ForwardOutcome> {
    let secret = store
        .secrets
        .read()
        .get(&key.id)?
        .ok_or_else(|| AppError::Upstream("密钥缺失".into()))?;

    // 模型映射：把下游请求的期望模型翻译为该 Key 的真实模型（FR-006）
    let real_model = key
        .mappings
        .iter()
        .find(|m| m.expected_name == requested_model)
        .map(|m| m.real_name.clone())
        .unwrap_or_else(|| requested_model.to_string());

    // 判定下游请求协议（按 path），与目标 Key 协议做适配
    let downstream_is_anthropic = path.contains("/messages");
    let mut payload = req_json.clone();
    if let Some(obj) = payload.as_object_mut() {
        obj.insert("model".into(), Value::String(real_model.clone()));
    }

    // 跨协议转换（MVP 非流式）
    let payload = match (downstream_is_anthropic, key.protocol) {
        (true, Protocol::Openai) => crate::upstream::anthropic_to_openai(&payload),
        (false, Protocol::Anthropic) => crate::upstream::openai_to_anthropic(&payload),
        _ => payload,
    };

    // 发往上游的请求体快照（pretty，方便页面阅读；密钥不在 body 里，安全）
    let request_body = serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string());

    // 目标 URL + 鉴权
    let (url, auth_header, auth_val, extra) = match key.protocol {
        Protocol::Anthropic => (
            join(&key.base_url, "/v1/messages"),
            "x-api-key",
            secret.clone(),
            Some(("anthropic-version", "2023-06-01")),
        ),
        Protocol::Openai => (
            join(&key.base_url, "/v1/chat/completions"),
            "authorization",
            format!("Bearer {secret}"),
            None,
        ),
    };

    let timeout = key.params.timeout_ms.unwrap_or(30_000);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(timeout))
        .build()
        .map_err(|e| AppError::Upstream(e.to_string()))?;

    // 先透传下游客户端头（User-Agent / anthropic-beta / x-app / x-stainless-* 等），
    // 让中转商识别为真实 Claude Code 客户端；再用本 Key 的鉴权头覆盖，确保鉴权用对密钥。
    let mut rb = client.post(&url).json(&payload);
    for (h, v) in fwd_headers {
        rb = rb.header(h, v);
    }
    rb = rb.header(auth_header, auth_val);
    if let Some((h, v)) = extra {
        rb = rb.header(h, v);
    }

    // 连接层失败（DNS/超时/拒连）仍返回 Err，附带目标 URL 便于定位。
    let resp = rb
        .send()
        .await
        .map_err(|e| AppError::Upstream(format!("连接 {url} 失败: {e}")))?;
    let status = resp.status();
    let bytes = resp.bytes().await.map_err(|e| AppError::Upstream(e.to_string()))?;

    Ok(ForwardOutcome {
        request_body,
        url,
        real_model,
        status: status.as_u16(),
        ok: status.is_success(),
        bytes,
    })
}

/// 判断某个下游请求头是否应被剔除、不透传给上游。
/// 剔除项：鉴权（用 Key 自己的）、路由/长度类（reqwest 按目标重算）、
/// hop-by-hop（RFC 7230）、content-type（reqwest .json() 自带）、
/// accept-encoding（避免上游返回压缩体导致响应快照乱码）。
fn is_stripped_header(name: &str) -> bool {
    matches!(
        name,
        "authorization"
            | "x-api-key"
            | "anthropic-version" // 用 Key 协议对应的版本头
            | "host"
            | "content-length"
            | "content-type"
            | "accept-encoding"
            | "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    )
}

/// 映射后真实模型名（日志展示用，与 forward_to_key 内映射逻辑一致）。
fn mapped_model(key: &ProviderKey, requested_model: &str) -> String {
    key.mappings
        .iter()
        .find(|m| m.expected_name == requested_model)
        .map(|m| m.real_name.clone())
        .unwrap_or_else(|| requested_model.to_string())
}

fn join(base: &str, path: &str) -> String {
    let base = base.trim_end_matches('/');
    if base.ends_with("/v1") && path.starts_with("/v1/") {
        format!("{}{}", base, &path[3..])
    } else {
        format!("{}{}", base, path)
    }
}

fn json_resp(status: StatusCode, body: Bytes) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Full::new(body))
        .unwrap()
}

fn error_resp(status: StatusCode, msg: &str) -> Response<Full<Bytes>> {
    let body = serde_json::json!({ "error": { "message": msg, "source": "synaroute" } });
    json_resp(status, Bytes::from(serde_json::to_vec(&body).unwrap()))
}
