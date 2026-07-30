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
use futures_util::StreamExt;
use http_body_util::{combinators::BoxBody, BodyExt, Full, StreamBody};
use hyper::body::{Frame, Incoming};
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

/// 响应体类型：可能是完整缓冲体（Full）或流式体（StreamBody），统一装箱为 BoxBody。
/// 流式路径用于同协议 SSE 透传（stream:true），缓冲路径用于非流式 / 跨协议。
type ResBody = BoxBody<Bytes, std::io::Error>;

/// 把完整字节体装箱为 ResBody（Full 的错误类型是 Infallible，用 `match` 消解）。
fn full_body(body: Bytes) -> ResBody {
    Full::new(body).map_err(|never| match never {}).boxed()
}

/// 运行中的代理句柄
struct RunningProxy {
    port: u16,
    handle: JoinHandle<()>,
    /// 关闭信号：广播给 accept 循环与所有已建立的连接任务，实现「停止即断连」。
    shutdown: tokio::sync::watch::Sender<bool>,
}

/// 首选端口被占时，在 [preferred, preferred+RANGE] 内向上兜底寻找可用端口（与 MCP 同策略）。
const PROXY_PORT_FALLBACK_RANGE: u16 = 20;

/// 「全部 Key 均失败」后的短路窗口（毫秒）：窗口内该分类的新请求**直接返回失败**，
/// 不再逐个重打上游。
///
/// 为什么必须有：`health::select_candidates` 在「所有 Key 都已熔断」时会**忽略熔断窗口、
/// 把全部 Key 原样返回**（避免单 Key 场景无处可切就自杀）。副作用是熔断在「全坏」这个最需要
/// 它的场景下形同虚设——`record_live_failure` 攒够 3 次设了 `breaker_until`，但下一个请求照旧
/// 把所有 Key 完整重打一遍。客户端（Claude 桌面端等）收到 5xx 会自动重发，于是表现为
/// 「一直轮询」：实测单次故障窗口内 3 条并发链各走完全部候选、共 16 条事件，
/// 相邻两次「全部 Key 失败」间隔低至 0.2s，白耗上游额度且刷爆日志。
///
/// 取 5s：足够把客户端连环重发的尖峰压平（0.2~0.5s 级的重试全部被挡），又短到 Key 恢复后
/// 几乎无感。任何一次转发成功都会**立即**解除短路（见 `clear_all_failed_gate`），
/// 故不会延误恢复。
const ALL_FAILED_SHORT_CIRCUIT_MS: i64 = 5_000;

/// 各分类的「全部 Key 失败」短路截止时刻（epoch ms）。进程内状态，重启即清。
fn all_failed_gate() -> &'static Mutex<HashMap<String, i64>> {
    static GATE: std::sync::OnceLock<Mutex<HashMap<String, i64>>> = std::sync::OnceLock::new();
    GATE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 记一次「全部 Key 失败」，武装短路窗口。
fn arm_all_failed_gate(category: CategoryType) {
    let until = chrono::Utc::now().timestamp_millis() + ALL_FAILED_SHORT_CIRCUIT_MS;
    all_failed_gate()
        .lock()
        .insert(category.as_str().to_string(), until);
}

/// 短路窗口是否仍有效。有效则返回剩余毫秒（>0），否则 None（并顺手清理过期项）。
fn all_failed_gate_remaining(category: CategoryType) -> Option<i64> {
    let now = chrono::Utc::now().timestamp_millis();
    let mut gate = all_failed_gate().lock();
    match gate.get(category.as_str()).copied() {
        Some(until) if until > now => Some(until - now),
        Some(_) => {
            gate.remove(category.as_str());
            None
        }
        None => None,
    }
}

/// 转发成功 → 立即解除短路（Key 恢复后无需等窗口自然到期）。
fn clear_all_failed_gate(category: CategoryType) {
    all_failed_gate().lock().remove(category.as_str());
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
        let settings = self.store.get_settings();
        let lan = settings.lan_exposure;
        let host = if lan { [0, 0, 0, 0] } else { [127, 0, 0, 1] };

        // 粘滞固定端口：首选端口取配置里该分类的值（缺省用 default_proxy_port）。
        // 从首选端口起在 [preferred, preferred+FALLBACK_RANGE] 内逐个尝试，绑上即用；
        // 全被占才报错提示改端口。避免早期「bind 0 随机端口」导致每次重启端口漂移、
        // 客户端追不上（config 只在客户端启动时读一次）。
        let preferred = settings
            .proxy_ports
            .get(category.as_str())
            .copied()
            .unwrap_or_else(|| crate::model::default_proxy_port(category.as_str()));
        let end = preferred.saturating_add(PROXY_PORT_FALLBACK_RANGE);
        let mut listener = None;
        let mut last_err = String::new();
        for candidate in preferred..=end {
            match TcpListener::bind(SocketAddr::from((host, candidate))).await {
                Ok(l) => {
                    listener = Some(l);
                    break;
                }
                Err(e) => last_err = format!("{candidate}: {e}"),
            }
        }
        let listener = listener.ok_or_else(|| {
            AppError::Proxy(format!(
                "端口 {preferred}~{end} 全部被占用（最后错误 {last_err}）。请在设置里换一个端口。"
            ))
        })?;
        let port = listener
            .local_addr()
            .map_err(|e| AppError::Proxy(e.to_string()))?
            .port();
        // 端口粘滞：实际绑定端口若与首选不同（回退了），写回配置作为下次首选，避免每次都回退漂移。
        if settings.proxy_ports.get(category.as_str()).copied() != Some(port) {
            let _ = self.store.set_proxy_port(category.as_str(), port);
        }

        // 关闭信号通道：stop() 时 send(true)，accept 循环退出、每个连接任务 select 到即断开。
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let store = self.store.clone();
        let accept_shutdown = shutdown_rx.clone();
        let handle = tokio::spawn(async move {
            let mut loop_shutdown = accept_shutdown;
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let Ok((stream, _)) = accepted else { break };
                        let io = TokioIo::new(stream);
                        let store = store.clone();
                        let mut conn_shutdown = shutdown_rx.clone();
                        tokio::spawn(async move {
                            let svc = service_fn(move |req| handle_request(store.clone(), category, req));
                            let conn = hyper::server::conn::http1::Builder::new()
                                .serve_connection(io, svc);
                            tokio::pin!(conn);
                            tokio::select! {
                                _ = conn.as_mut() => {}
                                // 收到停止信号：drop 连接（立即断开），使「已停止」后不再有请求被转发。
                                _ = conn_shutdown.changed() => {}
                            }
                        });
                    }
                    _ = loop_shutdown.changed() => break, // 停止：退出 accept 循环，释放端口
                }
            }
        });

        // 竞态处理（并发 start 同一分类）：绑定发生在 await 期间、锁外，故拿锁后再检查一次。
        // 若别的调用已插入，则放弃我们刚建的这套（发关闭信号 + abort，释放刚绑的端口），
        // 返回已存在的端口——避免泄漏一个无法再被 stop 的监听器。
        let mut running = self.running.lock();
        if let Some(existing) = running.get(category.as_str()) {
            let _ = shutdown_tx.send(true);
            handle.abort();
            return Ok(existing.port);
        }
        running.insert(
            category.as_str().to_string(),
            RunningProxy { port, handle, shutdown: shutdown_tx },
        );
        Ok(port)
    }

    pub fn stop(&self, category: CategoryType) {
        if let Some(p) = self.running.lock().remove(category.as_str()) {
            // 先广播关闭信号（断开已建立的 keep-alive 连接），再 abort accept 循环。
            let _ = p.shutdown.send(true);
            p.handle.abort();
        }
    }
}

/// 处理一次下游工具请求：故障转移路由。
async fn handle_request(
    store: Arc<Store>,
    category: CategoryType,
    req: Request<Incoming>,
) -> Result<Response<ResBody>, hyper::Error> {
    // 保留完整路径 + query（如 /v1/messages/count_tokens?beta=true）：同协议转发时原样透传，
    // 使 count_tokens 等非补全端点不被误改写为补全端点。协议判定仍用 contains 兼容。
    let path = req
        .uri()
        .path_and_query()
        .map(|pq| pq.as_str().to_string())
        .unwrap_or_else(|| req.uri().path().to_string());

    // 模型发现端点：Claude Code（v2.1.126+）/model 选择器会 GET <base>/v1/models 拉取可选模型。
    // 只放行 model 发现的可选模型（各启用 Key 可服务模型的交集/单 Key 自身），不进故障转移补全逻辑。
    // 用 path（去掉 query）的路径部分判定，避免把补全端点误判进来。
    let path_only = req.uri().path();
    if req.method() == hyper::Method::GET
        && (path_only == "/v1/models" || path_only == "/models" || path_only.starts_with("/v1/models/"))
    {
        return Ok(handle_list_models(&store, category));
    }

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
    let client_model = req_json
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .to_string();
    // 应用内「对外模型名」覆盖（借鉴 EchoBird）：某些客户端（如 Codex）模型菜单是内置固定
    // 清单、拉不到中转的真实模型，用户在本应用内选定的模型经此覆盖客户端发来的模型名。
    // 每请求实时读 get_settings，改选即时生效、免重启客户端。空/未选时透传客户端原值。
    // 覆盖后的名字仍走下游各 Key 的 resolve_model（映射→三档→原生→兜底），故多 Key 故障转移不受影响。
    let requested_model = store
        .get_settings()
        .active_models
        .get(category.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or(client_model);

    // 下游是否要求流式（Claude Code / Codex 默认 stream:true）。
    let wants_stream = req_json
        .get("stream")
        .and_then(|s| s.as_bool())
        .unwrap_or(false);
    // 下游请求协议（按 path 判定）：/messages=Anthropic，/responses=OpenAI Responses，否则 Chat。
    let downstream = downstream_protocol(&path);

    // 空探测请求短路：客户端（实测为 Claude 桌面端）会发 `{"messages":[],"model":null}` 这类
    // **无内容**的探测请求。它转发到任何上游都必然 400（实测 "Failed to parse request body"），
    // 而 400 被 status_counts_against_breaker 判为硬错误 → record_live_failure → 攒够 3 次把
    // **完好的 Key** 刷成熔断，进而触发全熔断兜底、把所有 Key 反复重打（问题3 的更深一层根因：
    // 实测 68 条失败请求里近半是这种空探测）。
    //
    // 故在此直接回 400，且**不打上游、不计熔断、不记 error 事件**：既不白耗上游额度，也不让
    // 客户端的探测行为污染健康状态。判据要求「无内容」与「无模型」同时成立，避免误伤
    // count_tokens 等合法的无 model 子路径请求（它们带真实 messages）。
    if is_contentless_probe(&req_json, &requested_model) {
        return Ok(error_resp(
            StatusCode::BAD_REQUEST,
            "空探测请求（无消息内容且未指定模型）：已在代理侧拒绝，未转发上游",
        ));
    }

    // 候选：启用 + 未熔断，按优先级。全被熔断时（如单 Key 刚熔断）忽略熔断兜底，
    // 避免无处可切却直接 503——熔断本为多 Key 快速切换，无候选可切时不应自杀。
    let (candidates, used_breaker_fallback) =
        health::select_candidates(store.enabled_keys_sorted(category));

    if candidates.is_empty() {
        return Ok(error_resp(
            StatusCode::SERVICE_UNAVAILABLE,
            "无可用 Key（全部停用或明确不可用）",
        ));
    }
    if used_breaker_fallback {
        store.append_event(
            category,
            "failover",
            None,
            "所有 Key 均在熔断窗口内，已忽略熔断兜底重试（无处可切换）",
        );
    }

    // 「全部 Key 失败」短路：上一次已确认全部候选都打不通，且仍在短路窗口内 →
    // 直接返回失败，不再逐个重打上游。见 ALL_FAILED_SHORT_CIRCUIT_MS 的完整理由。
    if let Some(remaining_ms) = all_failed_gate_remaining(category) {
        return Ok(error_resp(
            StatusCode::BAD_GATEWAY,
            &format!(
                "全部 Key 不可用：上次尝试已确认全部候选均失败，{}s 内不再重试（任一次成功即恢复）",
                (remaining_ms as f64 / 1000.0).ceil() as i64
            ),
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
        // 与路由成功日志同一口径：用 fmt_route_model 生成动词化的模型段，避免两处不一致。
        // 这里 real_model 由调用方传入（可能是尝试写出去的模型名或 resolve_model 结果），
        // 若无法从 requested + real 反推 kind，就展示 `客户端要 X → 实际用 Y` / 同名时 `模型 X`。
        let model_part = if shown_model == real_model.as_str() {
            format!("模型 {real_model}")
        } else {
            format!("客户端要 {shown_model} → 实际用 {real_model}")
        };
        let detail = if ok {
            format!("{} · {} · {}ms", key.name, model_part, elapsed)
        } else {
            format!(
                "{} · {} · {}ms · 失败{}",
                key.name,
                model_part,
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

    // 流式可走真流式的条件：同协议直通，或跨协议但有受支持的 SSE 翻译方向。
    // 其余跨协议组合（无翻译器）才跳过，交给故障转移找同协议 Key。
    let can_stream = |k: &ProviderKey| {
        downstream == k.protocol
            || crate::upstream::sse_direction(downstream, k.protocol).is_some()
    };

    let mut last_err = String::new();
    for (i, key) in candidates.iter().enumerate() {
        let started = std::time::Instant::now();
        let next = candidates.get(i + 1);
        // 故障转移日志：写清「谁失败（客户端要什么/实际打的什么）→ 转给谁」，避免只写「尝试下一个」看不出链路。
        let log_failover = |store: &Arc<Store>,
                            failed: &ProviderKey,
                            verb: &str,
                            err: &str,
                            next: Option<&ProviderKey>| {
            let failed_model = fmt_route_model_for_key(failed, &requested_model);
            // 用 · 分隔 Key 名 / 模型段 / 动词，避免视觉黏连产生「Key 上有 X」误读。
            let detail = match next {
                Some(n) => {
                    let next_model = fmt_route_model_for_key(n, &requested_model);
                    format!(
                        "{} · {} · {} → 转移 {} · {}",
                        failed.name, failed_model, verb, n.name, next_model
                    )
                }
                None => format!(
                    "{} · {} · {}（无后续候选）：{}",
                    failed.name, failed_model, verb, err
                ),
            };
            // 有后续时附简短原因；无后续时原因已写在 detail 里，避免重复。
            let detail = if next.is_some() && !err.is_empty() {
                format!("{detail}（{err}）")
            } else {
                detail
            };
            store.append_event(category, "failover", Some(&failed.id), &detail);
        };

        // 流式直通分支：下游要 stream 且无需跨协议转换 → 真流式透传（边收边发）。
        // 先探上游状态码：非 2xx 则照常切换下一个 Key（首字节尚未发出，切换安全）；
        // 2xx 则把上游 SSE 流原样转给下游，正确设置 content-type，直接返回（不再切换）。
        if wants_stream && can_stream(key) {
            match try_stream_to_key(&store, key, &path, &req_json, &requested_model, &fwd_headers).await {
                Ok(StreamAttempt::Streaming { resp, url, real_model, request_body }) => {
                    health::record_live_success(&store, &key.id);
                    // 有 Key 能用了 → 立即解除「全部失败」短路，不等窗口自然到期。
                    clear_all_failed_gate(category);
                    let elapsed = started.elapsed().as_millis() as u64;
                    // 流式成功也记一条 request 事件（开关开启时）。请求体以「转换后发往上游」为主
                    // （含 reasoning→thinking 映射结果，排障核心），单独完整保留、放最前；
                    // 「下游原始 body」（Codex 发来、转换前）体量极大（可达十几万字符），仅在
                    // log_downstream_raw_enabled 开关开启时追加、且单独截到小额度，避免把转换后段挤没
                    // ——此前双段直接拼接后被整体 cap(20000) 截断，转换后段常被下游原始段整段吞掉。
                    let combined_req = if store.get_settings().log_downstream_raw_enabled {
                        format!(
                            "==== 转换后发往上游 ====\n{}\n\n==== 下游原始请求（转换前，Codex 发来）====\n{}",
                            cap_to(&request_body, 8000),
                            cap_to(&downstream_body, 4000)
                        )
                    } else {
                        request_body
                    };
                    log_request(
                        &store,
                        key,
                        elapsed,
                        url,
                        real_model,
                        combined_req,
                        "（流式响应：边收边发，body 不留存。如需完整响应体，请在客户端侧抓取）".to_string(),
                        Some(200),
                        true,
                    );
                    store.append_event(
                        category,
                        "route",
                        Some(&key.id),
                        &format!(
                            "{} · 流式返回 · {}",
                            key.name,
                            fmt_route_model_for_key(key, &requested_model)
                        ),
                    );
                    return Ok(resp);
                }
                // 上游非 2xx：记录并切下一个
                Ok(StreamAttempt::HttpError { status, body, url, real_model }) => {
                    let elapsed = started.elapsed().as_millis() as u64;
                    let snippet: String = body.trim().chars().take(400).collect();
                    last_err = if snippet.is_empty() {
                        format!("HTTP {status}")
                    } else {
                        format!("HTTP {status}: {snippet}")
                    };
                    log_request(&store, key, elapsed, url, real_model, downstream_body.clone(), body, Some(status), false);
                    // 429/5xx 临时错误只切、不罚（不熔断好 Key）；4xx 硬错误才计入熔断。
                    if status_counts_against_breaker(status) {
                        health::record_live_failure(&store, &key.id);
                    }
                    log_failover(
                        &store,
                        key,
                        failover_verb(status),
                        &last_err,
                        next,
                    );
                    continue;
                }
                // 连接层失败：记录并切下一个
                Err(e) => {
                    let elapsed = started.elapsed().as_millis() as u64;
                    last_err = e.to_string();
                    log_request(&store, key, elapsed, String::new(), key.resolve_model(&requested_model), downstream_body.clone(), last_err.clone(), None, false);
                    health::record_live_failure(&store, &key.id);
                    log_failover(&store, key, "失败", &last_err, next);
                    continue;
                }
            }
        }

        // 流式 + 无法翻译的跨协议组合（如两端各为 Anthropic/Responses，无中枢路径）：
        // 绝不能走缓冲路径返回 application/json——下游按 text/event-stream 解析必失败。
        // 跳过该候选，让故障转移去找可流式的 Key；若最终都不可流式，循环结束统一回 502。
        if wants_stream && !can_stream(key) {
            last_err = "流式请求不支持跨协议转换（该 Key 协议与下游不一致）".to_string();
            log_failover(&store, key, "跳过", &last_err, next);
            continue;
        }

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
                health::record_live_success(&store, &key.id);
                // 有 Key 能用了 → 立即解除「全部失败」短路，不等窗口自然到期。
                clear_all_failed_gate(category);
                store.append_event(
                    category,
                    "route",
                    Some(&key.id),
                    &format!(
                        "{} · 成功返回 · {}",
                        key.name,
                        fmt_route_model_for_key(key, &requested_model)
                    ),
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
                // 429/5xx 临时错误只切、不罚（不熔断好 Key）；4xx 硬错误才计入熔断。
                if status_counts_against_breaker(outcome.status) {
                    health::record_live_failure(&store, &key.id);
                }
                log_failover(
                    &store,
                    key,
                    failover_verb(outcome.status),
                    &last_err,
                    next,
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
                    key.resolve_model(&requested_model),
                    downstream_body.clone(),
                    last_err.clone(),
                    None,
                    false,
                );
                health::record_live_failure(&store, &key.id);
                log_failover(&store, key, "失败", &last_err, next);
            }
        }
    }

    store.append_event(category, "error", None, &format!("全部 Key 失败: {last_err}"));
    // 武装短路窗口：窗口内后续请求（含客户端自动重发）直接失败，不再重打全部上游。
    arm_all_failed_gate(category);
    Ok(error_resp(
        StatusCode::BAD_GATEWAY,
        &format!("全部 Key 不可用：{last_err}"),
    ))
}

/// 计算 `/model` 选择器应展示的可选模型集（用户约定）：
/// - 无候选 → 空
/// - 单个候选 → 该 Key 自身可服务模型集（cc-switch 式）
/// - 多个候选 → 各 Key 可服务模型集的**交集**（共有），保证选任意模型都能在所有候选上路由，
///   不会「模型不存在」。顺序以主 Key（candidates[0]，已按 priority 升序）为准。
/// - 交集为空（多 Key 对外名不统一）→ 回退主 Key 的可服务模型集。
pub(crate) fn discoverable_models(candidates: &[ProviderKey]) -> Vec<String> {
    let Some((primary, rest)) = candidates.split_first() else {
        return Vec::new();
    };
    let primary_models = primary.serviceable_models();
    if rest.is_empty() {
        return primary_models;
    }
    // 各备用 Key 的可服务集，用于取交集
    let backup_sets: Vec<std::collections::HashSet<String>> = rest
        .iter()
        .map(|k| k.serviceable_models().into_iter().collect())
        .collect();
    let intersection: Vec<String> = primary_models
        .iter()
        .filter(|m| backup_sets.iter().all(|s| s.contains(*m)))
        .cloned()
        .collect();
    if intersection.is_empty() {
        // 空交集：对外名不统一。回退主 Key，保证选择器不空且主 Key 一定能路由。
        primary_models
    } else {
        intersection
    }
}

/// 返回模型发现结果（GET /v1/models）。按分类协议输出对应形态：
/// - Claude CLI / 桌面端（Anthropic）：`{"data":[{"type":"model","id":..,"display_name":..}],"has_more":false}`
/// - Codex（OpenAI）：`{"object":"list","data":[{"id":..,"object":"model","owned_by":"synaroute"}]}`
fn handle_list_models(store: &Arc<Store>, category: CategoryType) -> Response<ResBody> {
    // 模型发现不受健康态影响：只要 Key 启用，就应在 /model 选择器里列出它能服务的模型名。
    // 健康/熔断只决定「实际路由到哪个 Key」，不该决定「能选哪些模型」——否则单 Key 被真实探测
    // 判 Down 后 /model 会空掉，用户连模型都选不了（此前用 select_candidates 过滤导致的 bug）。
    let models = discoverable_models(&store.enabled_keys_sorted(category));

    // 分类固定了下游协议形态：Codex 用 OpenAI，Claude CLI/桌面端用 Anthropic。
    // Claude Code 静默丢弃 id 不以 claude/anthropic 开头的条目 → 非合规名包成
    // `claude-synaroute-<real>`，display_name 仍显示真实名，resolve 时剥前缀。
    let body = if matches!(category, CategoryType::Codex) {
        let data: Vec<Value> = models
            .iter()
            .map(|m| serde_json::json!({"id": m, "object": "model", "owned_by": "synaroute"}))
            .collect();
        serde_json::json!({"object": "list", "data": data})
    } else {
        let data: Vec<Value> = models
            .iter()
            .map(|m| {
                let id = crate::model::to_gateway_model_id(m);
                // display_name 用真实名，选择器上用户看到 grok-4.5 而非长前缀
                serde_json::json!({"type": "model", "id": id, "display_name": m})
            })
            .collect();
        let first = models.first().map(|m| crate::model::to_gateway_model_id(m));
        let last = models.last().map(|m| crate::model::to_gateway_model_id(m));
        serde_json::json!({
            "data": data,
            "has_more": false,
            "first_id": first,
            "last_id": last,
        })
    };
    let bytes = Bytes::from(serde_json::to_vec(&body).unwrap_or_default());
    json_resp(StatusCode::OK, bytes)
}

/// 请求/响应体在日志里的最大字符数（防止超大 body 撑爆内存日志）
const REQ_LOG_CAP: usize = 20_000;

/// 截断超长文本，附省略提示。
fn cap(s: &str) -> String {
    cap_to(s, REQ_LOG_CAP)
}

/// 按指定上限截断（供双段日志分别控额度：转换后段须完整可见，下游原始段太大只留头部）。
fn cap_to(s: &str, limit: usize) -> String {
    if s.chars().count() <= limit {
        s.to_string()
    } else {
        let head: String = s.chars().take(limit).collect();
        format!("{head}\n…（已截断，共 {} 字符）", s.chars().count())
    }
}

/// 流式转发的尝试结果。
enum StreamAttempt {
    /// 上游 2xx：已构建好流式响应，直接返回给下游（不再切换 Key）。
    /// 附带诊断快照供调用模型日志：实际请求的上游 URL、映射后模型名、
    /// **转换后发往上游的请求体**（含 reasoning→thinking 等映射结果，供排障核对）。
    /// 响应体因是真流式（边收边发）无法完整留存，日志侧标注说明。
    Streaming {
        resp: Response<ResBody>,
        url: String,
        real_model: String,
        request_body: String,
    },
    /// 上游有响应但非 2xx：缓冲错误体，调用方据此切换下一个 Key。
    HttpError {
        status: u16,
        body: String,
        url: String,
        real_model: String,
    },
}

/// 同协议流式转发：把上游 SSE 响应边收边发透传给下游。
///
/// 与 forward_to_key 的差异：
/// - 不设总超时（长回答会被 30s 掐断），仅设连接超时；流本身靠客户端断开或上游结束收尾。
/// - 先 send() 探状态码：非 2xx 缓冲错误体返回 HttpError（首字节未发，切换安全）；
///   2xx 则用 bytes_stream() 逐块转发，content-type 沿用上游真实值（保 text/event-stream）。
/// - 仅在下游协议与 Key 协议一致时调用（无需跨协议转换），跨协议 SSE 翻译属已知限制。
async fn try_stream_to_key(
    store: &Arc<Store>,
    key: &ProviderKey,
    path: &str,
    req_json: &Value,
    requested_model: &str,
    fwd_headers: &[(String, String)],
) -> AppResult<StreamAttempt> {
    let secret = store
        .secrets
        .read()
        .get(&key.id)?
        .ok_or_else(|| AppError::Upstream("密钥缺失".into()))?;

    // 模型解析：映射 → 原生支持 → 默认兜底 → 第一个模型 → 透传（见 ProviderKey::resolve_model）
    let real_model = key.resolve_model(requested_model);

    // 下游协议 → 上游 Key 协议：请求体按需转换（同协议 convert_request 内部直接克隆）。
    let downstream = downstream_protocol(path);
    let mut payload = req_json.clone();
    if let Some(obj) = payload.as_object_mut() {
        obj.insert("model".into(), Value::String(real_model.clone()));
    }
    inject_default_effort(store, &mut payload, downstream, key.protocol);
    let payload = crate::upstream::convert_request(&payload, downstream, key.protocol);

    // SSE 翻译方向：同协议为 None（原样透传）；跨协议按方向重组事件流。
    let sse_dir = crate::upstream::sse_direction(downstream, key.protocol);

    // 同协议：原样保留下游路径 + query（count_tokens 等子路径正确透传）。
    // 跨协议：退回上游协议的补全端点。
    let resource_path: std::borrow::Cow<str> = if downstream == key.protocol {
        std::borrow::Cow::Borrowed(path)
    } else {
        std::borrow::Cow::Borrowed(key.protocol.completion_path())
    };
    let url = crate::upstream::join_endpoint(&key.base_url, &resource_path);
    let (auth_header, auth_val, extra) = if matches!(key.protocol, Protocol::Anthropic) {
        ("x-api-key", secret.clone(), Some(("anthropic-version", "2023-06-01")))
    } else {
        ("authorization", format!("Bearer {secret}"), None)
    };

    // 流式：用共享客户端（连接池复用，含 connect_timeout），不设总超时，避免长回答被掐断。
    let client = crate::upstream::shared_client();

    let mut rb = client.post(&url).json(&payload);
    for (h, v) in fwd_headers {
        rb = rb.header(h, v);
    }
    rb = rb.header(auth_header, auth_val);
    if let Some((h, v)) = extra {
        rb = rb.header(h, v);
    }

    let resp = rb
        .send()
        .await
        .map_err(|e| AppError::Upstream(format!("连接 {url} 失败: {e}")))?;
    let status = resp.status();

    if !status.is_success() {
        // 非 2xx：缓冲错误体供切换决策与日志。
        let body = resp
            .bytes()
            .await
            .map(|b| String::from_utf8_lossy(&b).to_string())
            .unwrap_or_default();
        return Ok(StreamAttempt::HttpError {
            status: status.as_u16(),
            body,
            url,
            real_model,
        });
    }

    // 2xx：同协议原样透传；跨协议用 SseTranslator 逐块重组事件流。
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("text/event-stream")
        .to_string();

    let body: ResBody = match sse_dir {
        // 同协议：reqwest 字节流 → hyper StreamBody，逐块原样透传。
        None => {
            let byte_stream = resp.bytes_stream().map(|chunk| {
                chunk
                    .map(Frame::data)
                    .map_err(|e| std::io::Error::other(e.to_string()))
            });
            BodyExt::boxed(StreamBody::new(byte_stream))
        }
        // 跨协议：有状态翻译器逐块把上游 SSE 重组成下游协议事件；流末尾冲刷收尾事件。
        // 用 stream::unfold 承载状态机（不引 async_stream 依赖）：累加器持有翻译器、上游流、
        // 以及「是否已冲刷收尾」标志。每步产出一个 Frame。
        Some(dir) => {
            // 从下游请求 tools 收集 namespace 名（Codex 把 MCP 工具折叠成 type:"namespace"）。
            // 响应侧回填 function_call 时据此把全名 <ns>__<sub> 拆回 {name, namespace} 两字段——
            // Codex router 用结构化 ToolName{namespace,name} 查表，不拆 name 字符串，缺 namespace
            // 字段就报 unsupported call（大脑聚合失效根因）。
            let namespaces = crate::upstream::collect_tool_namespaces(req_json);
            let custom_tools = crate::upstream::collect_custom_tools(req_json);
            // Codex 的延迟工具检索器（type:"tool_search"）：模型对它的调用必须回程成
            // tool_search_call，Codex 才会本地跑 BM25 检索并在下一轮把 MCP 工具真 schema
            // 灌进 tool_search_output —— 那是 mcp__* 工具唯一的来源。
            let search_tools = crate::upstream::collect_search_tools(req_json);
            let translator = crate::upstream::SseTranslator::with_namespaces_and_custom(
                dir,
                namespaces,
                custom_tools,
                search_tools,
            );
            let upstream = resp.bytes_stream();
            struct StreamState<S> {
                translator: crate::upstream::SseTranslator,
                upstream: S,
                finished: bool,
            }
            let init = StreamState { translator, upstream, finished: false };
            let translated = futures_util::stream::unfold(init, |mut st| async move {
                loop {
                    match st.upstream.next().await {
                        Some(Ok(bytes)) => {
                            let out = st.translator.push(&bytes);
                            if out.is_empty() {
                                continue; // 该块未凑齐完整行，继续拉取
                            }
                            let frame = Ok(Frame::data(Bytes::from(out)));
                            return Some((frame, st));
                        }
                        Some(Err(e)) => {
                            let frame = Err(std::io::Error::other(e.to_string()));
                            st.finished = true;
                            return Some((frame, st));
                        }
                        None => {
                            // 上游结束：冲刷一次收尾事件，之后终止。
                            if st.finished {
                                return None;
                            }
                            st.finished = true;
                            let tail = st.translator.finish();
                            if tail.is_empty() {
                                return None;
                            }
                            let frame = Ok(Frame::data(Bytes::from(tail)));
                            return Some((frame, st));
                        }
                    }
                }
            });
            BodyExt::boxed(StreamBody::new(translated))
        }
    };

    // 跨协议时下游 content-type 固定为 SSE（上游的可能不同）；同协议沿用上游值。
    let out_content_type = if sse_dir.is_some() {
        "text/event-stream".to_string()
    } else {
        content_type
    };

    let response = Response::builder()
        .status(StatusCode::OK)
        .header("content-type", out_content_type)
        .header("cache-control", "no-cache")
        .body(body)
        .map_err(|e| AppError::Upstream(e.to_string()))?;

    // 转换后发往上游的请求体快照（供调用模型日志核对 reasoning→thinking 等映射）。
    let request_body = serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string());
    Ok(StreamAttempt::Streaming {
        resp: response,
        url,
        real_model,
        request_body,
    })
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

/// 按下游请求 path 判定下游客户端使用的协议：
/// `/messages` → Anthropic（Claude CLI/桌面端）；`/responses` → OpenAI Responses（Codex）；
/// 其余（`/chat/completions` 等）→ OpenAI Chat。用于选择请求/响应的跨协议转换方向。
fn downstream_protocol(path: &str) -> Protocol {
    if path.contains("/messages") {
        Protocol::Anthropic
    } else if path.contains("/responses") {
        Protocol::OpenaiResponses
    } else {
        Protocol::OpenaiChat
    }
}

/// 方案 A：为 Codex 注入「默认推理强度」。
///
/// 背景：Codex Desktop 对自定义 provider **不下发** `reasoning.effort`（实测 body 里
/// 只有 `reasoning:{summary:auto}`，effort 缺失），故用户在 Codex UI 里设的强度传不到上游。
/// 本函数在转发前，按分类配置（active_efforts["codex"]）把 effort 注入进 payload 的
/// `reasoning.effort`，后续转换链（responses_to_chat 透传 → openai_to_anthropic 映射 thinking，
/// 或原生 Responses 上游直接用）即可让强度生效。
///
/// 严格的不影响边界：
/// - **仅下游为 OpenAI Responses（Codex）时注入**：其它下游（Claude Code /messages、
///   /chat/completions 客户端）根本不经过这里的判断，零影响。
/// - **上游协议不限**：Anthropic / Chat 上游经转换链映射 thinking / reasoning_effort；
///   同协议 Responses 上游直通、也补默认 effort——经 SynaRoute 转发的一律是自定义 provider，
///   Codex 对自定义 provider 不下发 effort，故此路径同样需要补，否则接 Responses 协议第三方
///   中转商时配置的推理强度不生效（这正是方案 A 的前提，不区分上游协议）。
/// - **已有 effort 则不覆盖**：只在缺失时补默认，将来 Codex 真发了 effort 也不破坏；
///   若确实接了 OpenAI 官方 Responses 端点，官方客户端会自带 effort，has_effort 命中即跳过。
/// - 配置为空 / 未设 → 完全不注入，保持现状。
fn inject_default_effort(
    store: &Arc<Store>,
    payload: &mut Value,
    downstream: Protocol,
    upstream: Protocol,
) {
    // 只作用于 Codex（下游 Responses）。含同协议 Responses 上游：经 SynaRoute 转发的一律是
    // 自定义 provider，Codex 对自定义 provider 不下发 effort（方案 A 的前提），故同协议直通场景
    // 也需补默认强度，否则接 Responses 协议第三方中转商时配置的推理强度不生效。
    // （upstream 参数保留以备将来按上游细分策略，当前不再据此跳过。）
    let _ = upstream;
    if downstream != Protocol::OpenaiResponses {
        return;
    }
    let category = downstream_category_for_effort();
    let effort = match store
        .get_settings()
        .active_efforts
        .get(category)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        Some(e) => e,
        None => return, // 未配默认强度：保持现状，不注入
    };
    let Some(obj) = payload.as_object_mut() else { return };
    // 已带 effort（无论 Codex 何时开始下发）→ 尊重下游，不覆盖。
    let has_effort = obj
        .get("reasoning")
        .and_then(|r| r.get("effort"))
        .map(|e| !e.is_null())
        .unwrap_or(false);
    if has_effort {
        return;
    }
    // 注入到 reasoning.effort（保留已有的 reasoning.summary 等字段）。
    match obj.get_mut("reasoning").and_then(|r| r.as_object_mut()) {
        Some(r) => {
            r.insert("effort".into(), Value::String(effort));
        }
        None => {
            obj.insert("reasoning".into(), serde_json::json!({ "effort": effort }));
        }
    }
}

/// 推理强度注入目前只服务 Codex 分类（其模型/推理菜单对自定义 provider 不下发 effort）。
fn downstream_category_for_effort() -> &'static str {
    CategoryType::Codex.as_str()
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

    // 模型解析：映射 → 原生支持 → 默认兜底 → 第一个模型 → 透传（FR-006，见 resolve_model）
    let real_model = key.resolve_model(requested_model);

    // 判定下游请求协议（按 path），与目标 Key 协议做适配
    let downstream = downstream_protocol(path);
    let mut payload = req_json.clone();
    if let Some(obj) = payload.as_object_mut() {
        obj.insert("model".into(), Value::String(real_model.clone()));
    }
    inject_default_effort(store, &mut payload, downstream, key.protocol);

    // 跨协议转换（下游协议 → 上游 Key 协议；同协议时 convert_request 内部直接返回克隆）
    let payload = crate::upstream::convert_request(&payload, downstream, key.protocol);

    // 发往上游的请求体快照（pretty，方便页面阅读；密钥不在 body 里，安全）
    let request_body = serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string());

    // 目标 URL + 鉴权。
    // 同协议：原样保留下游路径 + query（count_tokens 等子路径正确透传）。
    // 跨协议：只能映射主补全端点（其它子路径无跨协议等价物，退回该上游协议的补全端点）。
    let same_protocol = downstream == key.protocol;
    let resource_path: std::borrow::Cow<str> = if same_protocol {
        std::borrow::Cow::Borrowed(path)
    } else {
        std::borrow::Cow::Borrowed(key.protocol.completion_path())
    };
    let url = crate::upstream::join_endpoint(&key.base_url, &resource_path);
    let (auth_header, auth_val, extra) = if matches!(key.protocol, Protocol::Anthropic) {
        ("x-api-key", secret.clone(), Some(("anthropic-version", "2023-06-01")))
    } else {
        ("authorization", format!("Bearer {secret}"), None)
    };

    // 用共享客户端（连接池复用），总超时按 Key 配置逐请求指定。
    let client = crate::upstream::shared_client();

    // 先透传下游客户端头（User-Agent / anthropic-beta / x-app / x-stainless-* 等），
    // 让中转商识别为真实 Claude Code 客户端；再用本 Key 的鉴权头覆盖，确保鉴权用对密钥。
    let mut rb = client
        .post(&url)
        .json(&payload)
        .timeout(std::time::Duration::from_millis(key.params.timeout_ms.unwrap_or(30_000)));
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

    // 跨协议响应翻译：上游 2xx 时，把响应体从上游协议翻译回下游客户端期望的协议格式。
    // 请求已翻译（上面的 payload 转换），响应也必须翻译，否则下游客户端收到无法解析的异协议体。
    // 非 2xx 不翻译：错误体原样透传，便于日志显示上游真实错误。同协议时 convert_response 内部直接返回克隆。
    let bytes = if status.is_success() && !same_protocol {
        match serde_json::from_slice::<Value>(&bytes) {
            Ok(v) => {
                // 传入两个工具集合：Chat→Responses 时把 apply_patch 等的回程 item type 改写为
                // custom_tool_call、tool_search 的改写为 tool_search_call（否则 Codex router
                // 认不出：前者工具执行失败，后者检索发不起来→MCP 工具拿不到 schema）。
                // 其他协议对不涉及此逻辑。
                let custom_tools = crate::upstream::collect_custom_tools(req_json);
                let search_tools = crate::upstream::collect_search_tools(req_json);
                let translated = crate::upstream::convert_response_ext(
                    &v,
                    key.protocol,
                    downstream,
                    &custom_tools,
                    &search_tools,
                );
                Bytes::from(serde_json::to_vec(&translated).unwrap_or_else(|_| bytes.to_vec()))
            }
            Err(_) => bytes, // 解析失败（非 JSON）：原样透传
        }
    } else {
        bytes
    };

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

/// 上游返回某状态码时，是否该「计入熔断」（惩罚该 Key）。
///
/// 429（限流）与 5xx（网关/后端临时故障）都是**临时性**、Key 本身没坏：
/// - 429 requests-per-minute：这一分钟请求满了，下一分钟自动恢复，不该把好 Key 熔断 60s。
/// - 502/503/504：中转商网关抖动，同样短暂。
///
/// 这类只做「切下一个 Key 应急」，但**不累加 fail_count、不熔断**——下个请求仍优先用它。
///
/// 4xx（除 429）才是确定性故障：401/403 鉴权失败、400 参数错、404 模型不存在——
/// 这种重试/保留都没意义，计入熔断，连续几次后把它熔断掉，避免每个请求都白试。
fn status_counts_against_breaker(status: u16) -> bool {
    !matches!(status, 429 | 500 | 502 | 503 | 504)
}

/// 是否为「无内容探测请求」：消息载荷为空**且**未解析出任何模型名。
///
/// 两条件必须同时成立：
/// - 只看「空内容」会误伤 —— 上游偶尔要求 model 由代理注入，但请求带真实 messages，属正常请求。
/// - 只看「无模型」会误伤 `count_tokens` 等合法子路径（不带 model 但带真实 messages）。
///
/// 覆盖三种协议的载荷字段：Anthropic/Chat 用 `messages`，OpenAI Responses 用 `input`
/// （字符串形态的 `input` 只要非空即算有内容）。非对象 body（如 `Value::Null`，解析失败）
/// 不算探测——那是坏请求，交由既有路径处理，避免把「body 解析失败」误报成探测。
fn is_contentless_probe(req_json: &Value, requested_model: &str) -> bool {
    let Some(obj) = req_json.as_object() else {
        return false;
    };
    if !requested_model.trim().is_empty() {
        return false;
    }
    // 有任一非空载荷字段 → 不是空探测。
    let has_content = ["messages", "input", "prompt", "contents"].iter().any(|k| {
        match obj.get(*k) {
            Some(Value::Array(a)) => !a.is_empty(),
            Some(Value::String(s)) => !s.trim().is_empty(),
            Some(Value::Object(o)) => !o.is_empty(),
            _ => false,
        }
    });
    if has_content {
        return false;
    }
    // 载荷字段全空/缺失 + 无模型：要求至少出现过一个载荷键，避免把结构完全无关的
    // 请求（如未来新增端点）误判为探测。
    ["messages", "input", "prompt", "contents"]
        .iter()
        .any(|k| obj.contains_key(*k))
}

/// 故障转移日志里的动词：临时错误（429/5xx）用「限流/繁忙，暂避」，
/// 确定性错误用「失败」。让用户一眼区分「Key 坏了」和「Key 只是这一下满了」。
fn failover_verb(status: u16) -> &'static str {
    if status_counts_against_breaker(status) {
        "失败"
    } else {
        "限流/繁忙，暂避"
    }
}

/// 路由成功日志里的模型展示，必须一眼看清「客户端要什么 / 实际上游用什么 / 为什么改」。
///
/// 旧写法 `百倍 流式返回 请求=grok-4.5 实际=claude-opus-4-7（默认兜底）`：
/// 「百倍」与「grok-4.5」相邻，读起来像「百倍这个 Key 上有 grok-4.5」，产生歧义。
///
/// 新写法用动词把「客户端 → Key」的关系显式化，并对「Key 不支持该模型」明说：
/// - 原生:          `模型 glm-5.2`
/// - 透传:          `模型 grok-4.5（未配模型清单，原样透传）`
/// - 映射:          `客户端要 claude-opus-4-8 → 映射为 glm-5.2`
/// - 三档:          `客户端要 claude-sonnet-4-5 → 三档命中 glm-4.6`
/// - 默认兜底:      `客户端要 grok-4.5（此 Key 不支持）→ 兜底改写为 claude-opus-4-7`
/// - 列表首个:      `客户端要 grok-4.5（此 Key 不支持）→ 取列表首个 claude-opus-4-7`
fn fmt_route_model(requested: &str, real: &str, kind: crate::model::ModelResolveKind) -> String {
    use crate::model::ModelResolveKind;
    // 请求名为空（极少见，正常请求都会带 model 字段）：只报实际用哪个
    if requested.is_empty() {
        return format!("模型 {real}（{}）", kind.label_zh());
    }
    match kind {
        // 原生同名：请求什么就用什么，不啰嗦
        ModelResolveKind::Native => format!("模型 {real}"),
        // 透传：Key 未配模型清单，原样发出（可能上游不支持）
        ModelResolveKind::Passthrough => format!("模型 {real}（未配模型清单，原样透传）"),
        // 精确映射：用户显式配的规则
        ModelResolveKind::Mapping => format!("客户端要 {requested} → 映射为 {real}"),
        // 三档匹配：haiku/sonnet/opus 家族级
        ModelResolveKind::Tier => format!("客户端要 {requested} → 三档命中 {real}"),
        // 兜底路径：明说「此 Key 不支持」消除歧义
        ModelResolveKind::Default => {
            format!("客户端要 {requested}（此 Key 不支持）→ 兜底改写为 {real}")
        }
        ModelResolveKind::First => {
            format!("客户端要 {requested}（此 Key 不支持）→ 取列表首个 {real}")
        }
    }
}

/// 按 Key 解析并格式化路由日志模型段（成功/流式成功共用）。
fn fmt_route_model_for_key(key: &ProviderKey, requested: &str) -> String {
    let (real, kind) = key.resolve_model_detail(requested);
    fmt_route_model(requested, &real, kind)
}

fn json_resp(status: StatusCode, body: Bytes) -> Response<ResBody> {
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(full_body(body))
        .unwrap()
}

fn error_resp(status: StatusCode, msg: &str) -> Response<ResBody> {
    let body = serde_json::json!({ "error": { "message": msg, "source": "synaroute" } });
    json_resp(status, Bytes::from(serde_json::to_vec(&body).unwrap()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CategoryType, HealthState, KeyParams, Protocol, ProviderKey};
    use crate::store::Store;
    use serde_json::json;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir()
            .join(format!("synaroute_proxy_test_{}_{}_{}", tag, std::process::id(), seq));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn breaker_spares_transient_status_penalizes_hard_errors() {
        // 429 限流 + 5xx 网关抖动：Key 没坏，只切不罚（不熔断）。
        for s in [429u16, 500, 502, 503, 504] {
            assert!(!status_counts_against_breaker(s), "HTTP {s} 不应计入熔断");
        }
        // 4xx（除 429）鉴权/参数硬错误：计入熔断。
        for s in [400u16, 401, 403, 404, 422] {
            assert!(status_counts_against_breaker(s), "HTTP {s} 应计入熔断");
        }
        // 日志动词：临时错误说「暂避」，硬错误说「失败」。
        assert_eq!(failover_verb(429), "限流/繁忙，暂避");
        assert_eq!(failover_verb(401), "失败");
    }

    #[test]
    fn all_failed_gate_arms_blocks_then_clears_on_success() {
        // 回归护栏（用户实测「一直轮询」）：全部候选失败后必须短路，窗口内不再重打上游。
        // 实测现场：单次故障窗口 3 条并发链各走完全部候选、16 条事件，相邻两次「全部 Key 失败」
        // 间隔低至 0.2s —— 因为 select_candidates 在「全熔断」时忽略熔断窗口把所有 Key 原样返回，
        // 熔断在最需要它的场景形同虚设，客户端 5xx 自动重发于是无限放大。
        //
        // 用独立分类避免与其它并发测试共享 gate 状态。
        let cat = CategoryType::Codex;
        clear_all_failed_gate(cat);
        assert!(
            all_failed_gate_remaining(cat).is_none(),
            "初始不应处于短路"
        );

        arm_all_failed_gate(cat);
        let remaining = all_failed_gate_remaining(cat).expect("武装后应处于短路窗口");
        assert!(
            remaining > 0 && remaining <= ALL_FAILED_SHORT_CIRCUIT_MS,
            "剩余窗口应在 (0, {ALL_FAILED_SHORT_CIRCUIT_MS}] 内，实际 {remaining}"
        );

        // 任一次转发成功 → 立即解除，不等窗口自然到期（Key 恢复不被延误）。
        clear_all_failed_gate(cat);
        assert!(
            all_failed_gate_remaining(cat).is_none(),
            "成功后必须立即解除短路，否则 Key 恢复了却仍被拒"
        );
    }

    #[test]
    fn all_failed_gate_is_per_category() {
        // 分类隔离：Claude 桌面端全挂不能连带把 Codex 的请求也短路掉。
        let a = CategoryType::ClaudeDesktop;
        let b = CategoryType::ClaudeCli;
        clear_all_failed_gate(a);
        clear_all_failed_gate(b);

        arm_all_failed_gate(a);
        assert!(all_failed_gate_remaining(a).is_some(), "A 分类应短路");
        assert!(
            all_failed_gate_remaining(b).is_none(),
            "B 分类不应被 A 的失败牵连"
        );

        clear_all_failed_gate(a);
    }

    #[test]
    fn contentless_probe_detected_and_real_requests_spared() {
        // 回归护栏（用户日志实证：68 条失败请求近半是这种空探测）：
        // 桌面端发 {"messages":[],"model":null} → 转发上游必然 400 → 400 被判硬错误计入熔断
        // → 把完好的 Key 刷成熔断 → 触发全熔断兜底反复重打。必须在代理侧直接拒绝。

        // 1) 用户真实抓到的形态：空 messages + model:null（requested_model 解析为空串）。
        let probe = json!({"max_tokens":4096,"messages":[],"model":null});
        assert!(
            is_contentless_probe(&probe, ""),
            "用户实测的空探测形态必须被识别"
        );

        // 2) Responses 协议的空 input 同样是探测。
        assert!(is_contentless_probe(&json!({"input":[]}), ""));

        // ---- 以下都不得误伤 ----

        // 3) 有真实 messages 但无 model：count_tokens 等合法子路径，必须放行。
        let count_tokens = json!({"messages":[{"role":"user","content":"hi"}]});
        assert!(
            !is_contentless_probe(&count_tokens, ""),
            "带真实消息的无 model 请求（count_tokens）不得被拒"
        );

        // 4) 空 messages 但指定了模型：不是探测（交由上游判定），放行。
        assert!(
            !is_contentless_probe(&json!({"messages":[]}), "claude-opus-4-8"),
            "已指定模型的请求不得被判为探测"
        );

        // 5) 字符串形态的非空 input。
        assert!(!is_contentless_probe(&json!({"input":"hello"}), ""));

        // 6) body 解析失败（Value::Null）：不是探测，交既有路径处理，避免误报。
        assert!(
            !is_contentless_probe(&Value::Null, ""),
            "非对象 body 不应被判为空探测"
        );

        // 7) 完全无载荷键的请求（未来新端点）：不判探测，避免误伤。
        assert!(
            !is_contentless_probe(&json!({"foo":1}), ""),
            "无任何载荷键的请求不应被判为空探测"
        );
    }

    fn key(id: &str, priority: i32, base_url: &str) -> ProviderKey {
        ProviderKey {
            id: id.into(),
            category_id: CategoryType::ClaudeCli,
            name: format!("k-{id}"),
            vendor: "test".into(),
            base_url: base_url.into(),
            protocol: Protocol::Anthropic,
            has_secret: true,
            enabled: true,
            priority,
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

    /// 起一个返回固定 status + body 的 mock 上游，返回其 base_url。
    async fn spawn_mock(status: u16, body: &'static str) -> String {
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else { break };
                let io = TokioIo::new(stream);
                tokio::spawn(async move {
                    let svc = service_fn(move |_req: Request<Incoming>| async move {
                        let resp = Response::builder()
                            .status(status)
                            .header("content-type", "application/json")
                            .body(full_body(Bytes::from(body)))
                            .unwrap();
                        Ok::<_, hyper::Error>(resp)
                    });
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(io, svc)
                        .await;
                });
            }
        });
        format!("http://{addr}")
    }

    /// 端到端：高优先 Key 返 401 → 自动故障转移到低优先 Key 返 200，响应来自后者。
    #[tokio::test]
    async fn proxy_fails_over_bad_to_good() {
        let bad = spawn_mock(401, r#"{"error":{"message":"unauthorized"}}"#).await;
        let good = spawn_mock(
            200,
            r#"{"id":"msg_1","type":"message","role":"assistant","content":[{"type":"text","text":"ok"}]}"#,
        )
        .await;

        let dir = temp_dir("failover");
        let store = std::sync::Arc::new(
            Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap(),
        );
        store.upsert_key(key("k1", 0, &bad)).unwrap();
        store.upsert_key(key("k2", 1, &good)).unwrap();
        store.secrets.write().set("k1", "x").unwrap();
        store.secrets.write().set("k2", "y").unwrap();

        let pm = ProxyManager::new(store.clone());
        let port = pm.start(CategoryType::ClaudeCli).await.unwrap();

        let resp = reqwest::Client::new()
            .post(format!("http://127.0.0.1:{port}/v1/messages"))
            .json(&json!({"model":"m","max_tokens":10,"messages":[{"role":"user","content":"hi"}]}))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 200, "应故障转移到好 Key 返回 200");
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["content"][0]["text"], "ok", "响应应来自 good 上游");

        pm.stop(CategoryType::ClaudeCli);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 端到端：全部候选失败 → 502。
    #[tokio::test]
    async fn proxy_all_fail_returns_502() {
        let bad1 = spawn_mock(401, "no").await;
        let bad2 = spawn_mock(500, "err").await;
        let dir = temp_dir("allfail");
        let store = std::sync::Arc::new(
            Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap(),
        );
        store.upsert_key(key("k1", 0, &bad1)).unwrap();
        store.upsert_key(key("k2", 1, &bad2)).unwrap();
        store.secrets.write().set("k1", "x").unwrap();
        store.secrets.write().set("k2", "y").unwrap();
        let pm = ProxyManager::new(store.clone());
        let port = pm.start(CategoryType::ClaudeCli).await.unwrap();
        let resp = reqwest::Client::new()
            .post(format!("http://127.0.0.1:{port}/v1/messages"))
            .json(&json!({"model":"m","max_tokens":10,"messages":[{"role":"user","content":"hi"}]}))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 502, "全部失败应回 502");
        pm.stop(CategoryType::ClaudeCli);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 起一个**捕获请求体**的 mock 上游：把收到的 body 存进共享 Vec，返回固定 200。
    /// 用于端到端验证「发往上游的 body 里到底有没有 thinking」。
    async fn spawn_capture_mock(captured: std::sync::Arc<parking_lot::Mutex<Vec<String>>>) -> String {
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else { break };
                let cap = captured.clone();
                let io = TokioIo::new(stream);
                tokio::spawn(async move {
                    let svc = service_fn(move |req: Request<Incoming>| {
                        let cap = cap.clone();
                        async move {
                            let bytes = req.into_body().collect().await.unwrap().to_bytes();
                            cap.lock().push(String::from_utf8_lossy(&bytes).to_string());
                            let resp = Response::builder()
                                .status(200)
                                .header("content-type", "application/json")
                                .body(full_body(Bytes::from_static(
                                    br#"{"id":"m","type":"message","role":"assistant","content":[{"type":"text","text":"ok"}]}"#,
                                )))
                                .unwrap();
                            Ok::<_, hyper::Error>(resp)
                        }
                    });
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(io, svc)
                        .await;
                });
            }
        });
        format!("http://{addr}")
    }

    /// 端到端复现：Codex（下游 /v1/responses，body 无 effort）→ Anthropic 上游，配了 codex:xhigh，
    /// 断言上游实际收到的 body 里带 thinking.budget_tokens。这条覆盖真实链路的 path→downstream 判定
    /// + inject_default_effort + convert_request，是定位「推理强度没生效」的决定性测试。
    #[tokio::test]
    async fn codex_effort_injected_end_to_end() {
        let captured = std::sync::Arc::new(parking_lot::Mutex::new(Vec::<String>::new()));
        let upstream = spawn_capture_mock(captured.clone()).await;

        let dir = temp_dir("effort_e2e");
        let store = std::sync::Arc::new(
            Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap(),
        );
        // Codex 分类的 Anthropic 上游 Key（跨协议：下游 Responses → 上游 Anthropic）。
        let mut k = key("k1", 0, &upstream);
        k.category_id = CategoryType::Codex;
        k.protocol = Protocol::Anthropic;
        store.upsert_key(k).unwrap();
        store.secrets.write().set("k1", "x").unwrap();
        // 用户在 Codex 分类设了 xhigh。
        store.set_active_effort("codex", "xhigh").unwrap();

        let pm = ProxyManager::new(store.clone());
        let port = pm.start(CategoryType::Codex).await.unwrap();

        // Codex 真实形态：POST /v1/responses，input 数组 + reasoning:{summary:auto}（无 effort），非流式。
        let _ = reqwest::Client::new()
            .post(format!("http://127.0.0.1:{port}/v1/responses"))
            .json(&json!({
                "model": "claude-opus-4-7",
                "input": [{"type":"message","role":"user","content":[{"type":"input_text","text":"hi"}]}],
                "reasoning": {"summary": "auto"},
                "max_output_tokens": 8192,
                "stream": false
            }))
            .send()
            .await
            .unwrap();

        pm.stop(CategoryType::Codex);
        let bodies = captured.lock().clone();
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(bodies.len(), 1, "上游应收到 1 个请求");
        let up: serde_json::Value = serde_json::from_str(&bodies[0]).unwrap();
        assert!(
            up.get("thinking").and_then(|t| t.get("budget_tokens")).is_some(),
            "上游 body 应含 thinking.budget_tokens（xhigh 注入 + 映射），实际收到:\n{}",
            serde_json::to_string_pretty(&up).unwrap()
        );
    }

    /// 端到端（真实链路，非单元）：Codex direct 模式请求 → Anthropic 上游（opus），
    /// 断言上游**实际收到的 body** 里同时带上了延迟工具检索器与 MCP 工具的真 schema。
    ///
    /// 这是「opus 作 Codex 主 Key 能否用 MCP」的决定性测试。覆盖的真实成因（2026-07-30 抓包）：
    /// - `tool_search` 声明无 `name` 字段，请求侧曾一律 continue 跳过 → 模型不知道有检索器；
    /// - MCP 工具（`mcp__*`）**从不出现在顶层 tools**，只在 `tool_search_output.tools[]` 回灌，
    ///   未提升该处 → 模型永远看不到 `synaroute_ai`（表现为「MCP 握手正常但模型说没这工具」）。
    ///
    /// 走 `stream:false` 以复用捕获型 mock（流式路径的改写另有 upstream 侧单测覆盖）。
    #[tokio::test]
    async fn codex_tool_search_and_mcp_tools_reach_upstream() {
        let captured = std::sync::Arc::new(parking_lot::Mutex::new(Vec::<String>::new()));
        let upstream = spawn_capture_mock(captured.clone()).await;

        let dir = temp_dir("tool_search_e2e");
        let store = std::sync::Arc::new(
            Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap(),
        );
        let mut k = key("k1", 0, &upstream);
        k.category_id = CategoryType::Codex;
        k.protocol = Protocol::Anthropic;
        store.upsert_key(k).unwrap();
        store.secrets.write().set("k1", "x").unwrap();

        let pm = ProxyManager::new(store.clone());
        let port = pm.start(CategoryType::Codex).await.unwrap();

        // Codex direct 模式真实骨架：顶层 tools 含无 name 的 tool_search；
        // MCP 工具只在历史的 tool_search_output 里。
        let _ = reqwest::Client::new()
            .post(format!("http://127.0.0.1:{port}/v1/responses"))
            .json(&json!({
                "model": "gpt-5.5",
                "stream": false,
                "max_output_tokens": 1024,
                "tools": [
                    { "type": "function", "name": "shell_command",
                      "parameters": {"type":"object","properties":{"cmd":{"type":"string"}}} },
                    { "type": "tool_search", "execution": "client",
                      "description": "Tool discovery via BM25.",
                      "parameters": {"type":"object","properties":{"query":{"type":"string"}},"required":["query"]} }
                ],
                "input": [
                    { "type": "message", "role": "user",
                      "content": [{"type":"input_text","text":"用 synaroute_ai 审查"}] },
                    { "type": "tool_search_call", "id": "tsc_1", "call_id": "cs1",
                      "execution": "client", "arguments": {"query":"synaroute_ai"} },
                    { "type": "tool_search_output", "id": "tso_1", "call_id": "cs1",
                      "execution": "client", "tools": [{
                          "type": "namespace", "name": "mcp__synaroute",
                          "tools": [{ "type": "function", "name": "synaroute_ai",
                                      "description": "多模型会诊",
                                      "defer_loading": true,
                                      "parameters": {"type":"object","properties":{"prompt":{"type":"string"}},"required":["prompt"]} }]
                      }] }
                ]
            }))
            .send()
            .await
            .unwrap();

        pm.stop(CategoryType::Codex);
        let bodies = captured.lock().clone();
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(bodies.len(), 1, "上游应收到 1 个请求");
        let up: serde_json::Value = serde_json::from_str(&bodies[0]).unwrap();
        let tools = up["tools"].as_array().unwrap_or_else(|| {
            panic!(
                "上游 body 必须带 tools，实际收到:\n{}",
                serde_json::to_string_pretty(&up).unwrap()
            )
        });
        let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
        assert!(
            names.contains(&"tool_search"),
            "上游应收到延迟工具检索器（无 name 的声明需按 type 命名），实际: {names:?}"
        );
        assert!(
            names.contains(&"mcp__synaroute__synaroute_ai"),
            "上游应收到从 tool_search_output 提升的 MCP 工具全名，实际: {names:?}"
        );
        let syna = tools
            .iter()
            .find(|t| t["name"] == "mcp__synaroute__synaroute_ai")
            .unwrap();
        assert_eq!(
            syna["input_schema"]["properties"]["prompt"]["type"], "string",
            "MCP 工具的真 schema 要完整带到上游，实际: {syna}"
        );
        // 历史里的检索调用与结果不得退化成空消息（模型需看到自己检索过什么）。
        let msgs = serde_json::to_string(&up["messages"]).unwrap();
        assert!(
            msgs.contains("mcp__synaroute__synaroute_ai"),
            "检索结果回执应告知命中的工具，实际 messages:\n{msgs}"
        );
    }

    /// 同协议链路：Codex（下游 /v1/responses，无 effort）→ **OpenAI Responses 协议上游**（第三方
    /// 中转商），配了 codex:high。convert_request 对同协议直通不动 body，故若注入被 `downstream==upstream`
    /// 跳过则 effort 丢失。断言上游实际收到 reasoning.effort=high——锁住「同协议 Responses 上游也注入」修复。
    #[tokio::test]
    async fn codex_effort_injected_same_protocol_responses() {
        let captured = std::sync::Arc::new(parking_lot::Mutex::new(Vec::<String>::new()));
        let upstream = spawn_capture_mock(captured.clone()).await;

        let dir = temp_dir("effort_e2e_resp");
        let store = std::sync::Arc::new(
            Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap(),
        );
        // Codex 分类的 Responses 上游 Key（同协议：下游 Responses → 上游 Responses，convert_request 直通）。
        let mut k = key("k1", 0, &upstream);
        k.category_id = CategoryType::Codex;
        k.protocol = Protocol::OpenaiResponses;
        store.upsert_key(k).unwrap();
        store.secrets.write().set("k1", "x").unwrap();
        store.set_active_effort("codex", "high").unwrap();

        let pm = ProxyManager::new(store.clone());
        let port = pm.start(CategoryType::Codex).await.unwrap();

        let _ = reqwest::Client::new()
            .post(format!("http://127.0.0.1:{port}/v1/responses"))
            .json(&json!({
                "model": "gpt-5",
                "input": [{"type":"message","role":"user","content":[{"type":"input_text","text":"hi"}]}],
                "reasoning": {"summary": "auto"},
                "max_output_tokens": 8192,
                "stream": false
            }))
            .send()
            .await
            .unwrap();

        pm.stop(CategoryType::Codex);
        let bodies = captured.lock().clone();
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(bodies.len(), 1, "上游应收到 1 个请求");
        let up: serde_json::Value = serde_json::from_str(&bodies[0]).unwrap();
        assert_eq!(
            up.get("reasoning").and_then(|r| r.get("effort")).and_then(|e| e.as_str()),
            Some("high"),
            "同协议 Responses 上游 body 应含注入的 reasoning.effort=high，实际收到:\n{}",
            serde_json::to_string_pretty(&up).unwrap()
        );
    }

    // ---- /v1/models 模型发现 ----

    use crate::model::{ModelInfo, ModelMapping};

    fn model_info(name: &str) -> ModelInfo {
        ModelInfo { real_name: name.into(), source: "manual".into(), fetched_at: None, context_window: None }
    }
    fn mapping(expected: &str, real: &str) -> ModelMapping {
        ModelMapping { id: format!("{expected}->{real}"), expected_name: expected.into(), real_name: real.into() }
    }

    #[test]
    fn discover_single_key_uses_its_own_models() {
        let mut k = key("k1", 0, "http://x");
        k.mappings = vec![mapping("opus-4-8", "glm-5.2")];
        k.models = vec![model_info("glm-5.2"), model_info("glm-5.1")];
        // 有映射：只暴露对外名，不并入真实名
        assert_eq!(discoverable_models(&[k]), vec!["opus-4-8"]);
    }

    #[test]
    fn discover_multi_key_uses_intersection() {
        let mut a = key("a", 0, "http://x");
        a.mappings = vec![mapping("opus-4-8", "glm-5.2"), mapping("opus-4-7", "glm-5.1")];
        let mut b = key("b", 1, "http://y");
        b.mappings = vec![mapping("opus-4-8", "ds-v4")]; // 只共有 opus-4-8
        assert_eq!(discoverable_models(&[a, b]), vec!["opus-4-8"]);
    }

    #[test]
    fn discover_empty_intersection_falls_back_to_primary() {
        // 对外名不统一：a=claude-opus-4-7，b=opus-4-7 → 交集空 → 回退主 Key(a)
        let mut a = key("a", 0, "http://x");
        a.mappings = vec![mapping("claude-opus-4-7", "glm-5.1")];
        let mut b = key("b", 1, "http://y");
        b.mappings = vec![mapping("opus-4-7", "ds-v4")];
        assert_eq!(discoverable_models(&[a, b]), vec!["claude-opus-4-7"]);
    }

    #[test]
    fn discover_empty_when_no_candidates() {
        assert!(discoverable_models(&[]).is_empty());
    }

    #[test]
    fn fmt_route_model_shows_request_actual_and_reason() {
        use crate::model::ModelResolveKind;
        // 默认兜底改写：明说「此 Key 不支持」，消除「Key 上有 grok」的歧义。
        // 与 Key 名拼接后是 `百倍 流式返回 客户端要 grok-4.5（此 Key 不支持）→ 兜底改写为 claude-opus-4-7`
        assert_eq!(
            fmt_route_model("grok-4.5", "claude-opus-4-7", ModelResolveKind::Default),
            "客户端要 grok-4.5（此 Key 不支持）→ 兜底改写为 claude-opus-4-7"
        );
        // 映射改写：动词显式，不再用「请求=/实际=」
        assert_eq!(
            fmt_route_model("claude-opus-4-8", "glm-5.2", ModelResolveKind::Mapping),
            "客户端要 claude-opus-4-8 → 映射为 glm-5.2"
        );
        // 三档命中
        assert_eq!(
            fmt_route_model("claude-sonnet-4-5", "glm-4.6", ModelResolveKind::Tier),
            "客户端要 claude-sonnet-4-5 → 三档命中 glm-4.6"
        );
        // 原生同名：简洁
        assert_eq!(
            fmt_route_model("glm-5.2", "glm-5.2", ModelResolveKind::Native),
            "模型 glm-5.2"
        );
        // 透传：明说未配清单
        assert_eq!(
            fmt_route_model("grok-4.5", "grok-4.5", ModelResolveKind::Passthrough),
            "模型 grok-4.5（未配模型清单，原样透传）"
        );
        // 列表首个兜底
        assert_eq!(
            fmt_route_model("grok-4.5", "claude-opus-4-7", ModelResolveKind::First),
            "客户端要 grok-4.5（此 Key 不支持）→ 取列表首个 claude-opus-4-7"
        );
        // 请求名为空（罕见）
        assert_eq!(
            fmt_route_model("", "glm-5.2", ModelResolveKind::First),
            "模型 glm-5.2（列表首个）"
        );
    }

    #[tokio::test]
    async fn proxy_get_v1_models_returns_intersection() {
        let dir = temp_dir("listmodels");
        let store = std::sync::Arc::new(
            Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap(),
        );
        let mut a = key("a", 0, "http://x");
        a.mappings = vec![mapping("opus-4-8", "glm-5.2"), mapping("opus-4-7", "glm-5.1")];
        let mut b = key("b", 1, "http://y");
        b.mappings = vec![mapping("opus-4-8", "ds-v4")];
        store.upsert_key(a).unwrap();
        store.upsert_key(b).unwrap();
        store.secrets.write().set("a", "x").unwrap();
        store.secrets.write().set("b", "y").unwrap();
        let pm = ProxyManager::new(store.clone());
        let port = pm.start(CategoryType::ClaudeCli).await.unwrap();
        let resp = reqwest::Client::new()
            .get(format!("http://127.0.0.1:{port}/v1/models"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        let ids: Vec<String> = body["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["id"].as_str().unwrap().to_string())
            .collect();
        // 非 claude/anthropic 前缀的 id 会被包成 claude-synaroute-* 供 CLI 展示
        assert_eq!(ids, vec!["claude-synaroute-opus-4-8"], "应只返回共有的 opus-4-8（已包装）");
        assert_eq!(body["data"][0]["type"], "model", "Anthropic 形态");
        assert_eq!(body["data"][0]["display_name"], "opus-4-8", "展示名保持真实名");
        pm.stop(CategoryType::ClaudeCli);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn proxy_v1_models_wraps_non_claude_ids_for_cli() {
        // 回归：单 Key 只有 grok-4.5 时，CLI /model 必须能看见 From gateway 条目。
        // 原样返回 grok-4.5 会被 CLI 静默过滤；必须包成 claude-synaroute-grok-4.5。
        let dir = temp_dir("listmodels-wrap");
        let store = std::sync::Arc::new(
            Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap(),
        );
        let mut k = key("k1", 0, "http://x");
        k.models = vec![model_info("grok-4.5")];
        k.default_model = Some("grok-4.5".into());
        store.upsert_key(k).unwrap();
        store.secrets.write().set("k1", "x").unwrap();
        let pm = ProxyManager::new(store.clone());
        let port = pm.start(CategoryType::ClaudeCli).await.unwrap();
        let resp = reqwest::Client::new()
            .get(format!("http://127.0.0.1:{port}/v1/models"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["data"][0]["id"], "claude-synaroute-grok-4.5");
        assert_eq!(body["data"][0]["display_name"], "grok-4.5");
        pm.stop(CategoryType::ClaudeCli);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn proxy_v1_models_keeps_claude_ids_unwrapped() {
        // 已合规名不二次包装
        let dir = temp_dir("listmodels-claude");
        let store = std::sync::Arc::new(
            Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap(),
        );
        let mut k = key("k1", 0, "http://x");
        k.models = vec![model_info("claude-opus-4-5")];
        store.upsert_key(k).unwrap();
        store.secrets.write().set("k1", "x").unwrap();
        let pm = ProxyManager::new(store.clone());
        let port = pm.start(CategoryType::ClaudeCli).await.unwrap();
        let resp = reqwest::Client::new()
            .get(format!("http://127.0.0.1:{port}/v1/models"))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["data"][0]["id"], "claude-opus-4-5");
        assert_eq!(body["data"][0]["display_name"], "claude-opus-4-5");
        pm.stop(CategoryType::ClaudeCli);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn proxy_v1_models_lists_even_when_key_is_down() {
        // 回归：真实探测把单 Key 判 Down 后，/model 仍应列出它能服务的模型名。
        // 模型发现不受健康态影响——健康只决定路由，不决定「能选哪些模型」。
        let dir = temp_dir("listmodels-down");
        let store = std::sync::Arc::new(
            Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap(),
        );
        let mut k = key("k1", 0, "http://x");
        k.mappings = vec![mapping("opus-4-8", "glm-5.2")];
        k.models = vec![model_info("glm-5.2")];
        // 关键：把该 Key 置为 Down（模拟真实探测失败判死）。
        k.health = HealthState {
            status: crate::model::HealthStatus::Down,
            fail_count: 3,
            ..Default::default()
        };
        store.upsert_key(k).unwrap();
        store.secrets.write().set("k1", "x").unwrap();
        let pm = ProxyManager::new(store.clone());
        let port = pm.start(CategoryType::ClaudeCli).await.unwrap();
        let resp = reqwest::Client::new()
            .get(format!("http://127.0.0.1:{port}/v1/models"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        let ids: Vec<String> = body["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["id"].as_str().unwrap().to_string())
            .collect();
        // 有映射：只暴露对外名；非 claude 前缀包成 claude-synaroute-*
        assert_eq!(
            ids,
            vec!["claude-synaroute-opus-4-8"],
            "Down 的 Key 也应列出其映射对外名（已包装）"
        );
        pm.stop(CategoryType::ClaudeCli);
        std::fs::remove_dir_all(&dir).ok();
    }
}
