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

/// 「全部 Key 均失败」时回给下游的状态码：**529 overloaded_error**，不是 502。
///
/// 判据来自 claude.exe v2.1.219 内嵌的官方 gateway 协议规范：529 是「上游过载、稍后重试」的
/// 专用码，客户端 SDK 见到它会走**退避重试**；而 502 映射成 `api_error`，SDK 的处理是不确定的
/// （可能立刻重发，可能直接报错终止）。「全部候选都打不通」本质就是「暂时无产能」，语义正是 529。
///
/// 用 u16 常量而非 `StatusCode` 关联常量：529 不在 http crate 的预定义列表里（非 IANA 注册码，
/// 属 Cloudflare/Anthropic 惯例），只能 `from_u16` 构造。
const STATUS_OVERLOADED: u16 = 529;

/// 各分类的「全部 Key 失败」短路状态。进程内状态，重启即清。
///
/// 键是 `{ProxyManager 实例 id}:{分类名}`（见 [`ProxyManager::gate_key`]）。
/// 生产环境全程只有一个 `ProxyManager`，故等价于「按分类」；而单元测试里每个用例各建一个
/// `ProxyManager`，于是天然互不干扰——此前只用分类名做键，两条都用 `ClaudeCli` 的 e2e 测试会
/// 共享同一格：一条武装的 5s 窗口把另一条的请求直接短路，表现为 `proxy_fails_over_bad_to_good`
/// 偶发变红（同一份代码三次跑出 276/1、276/1、277/0）。
///
/// `until_ms`：短路截止时刻（epoch ms）。
/// `retry_at_ms`：上游 429/503 的 `Retry-After` 换算出的「可再试时刻」（epoch ms），无则 None。
///   窗口内被短路的响应用它给下游一个**不早于上游要求**的 `Retry-After`，避免下游按 5s 退避
///   却在上游仍限流时又撞上去。
#[derive(Clone, Copy)]
struct GateEntry {
    until_ms: i64,
    retry_at_ms: Option<i64>,
}

fn all_failed_gate() -> &'static Mutex<HashMap<String, GateEntry>> {
    static GATE: std::sync::OnceLock<Mutex<HashMap<String, GateEntry>>> =
        std::sync::OnceLock::new();
    GATE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 记一次「全部失败」，武装短路窗口。`retry_after_secs` 为上游给出的退避秒数（若有）。
fn arm_all_failed_gate(gate_key: &str, retry_after_secs: Option<i64>) {
    let now = chrono::Utc::now().timestamp_millis();
    all_failed_gate().lock().insert(
        gate_key.to_string(),
        GateEntry {
            until_ms: now + ALL_FAILED_SHORT_CIRCUIT_MS,
            retry_at_ms: retry_after_secs.map(|s| now + s.saturating_mul(1000)),
        },
    );
}

/// 短路窗口是否仍有效。有效则返回 `(剩余毫秒>0, 应告知下游的 Retry-After 秒数)`，
/// 否则 None（并顺手清理过期项）。
fn all_failed_gate_remaining(gate_key: &str) -> Option<(i64, i64)> {
    let now = chrono::Utc::now().timestamp_millis();
    let mut gate = all_failed_gate().lock();
    match gate.get(gate_key).copied() {
        Some(e) if e.until_ms > now => {
            // Retry-After 取「短路剩余」与「上游要求」的较晚者。
            //
            // 这里取较晚是对的（与 `retry_after_hint` 取最小值不矛盾）：那边比较的是**不同候选**
            // 的限流窗口，取最早者才不误伤整池；这里比较的是**同一个结论**的两个下限 ——
            // 短路窗口内重试注定被本地挡回，而上游若要求更久，提前去也是白撞。
            // 两者都是「不早于」，所以取 max。
            let target = e.retry_at_ms.unwrap_or(0).max(e.until_ms);
            Some((e.until_ms - now, retry_after_secs_until(target, now)))
        }
        Some(_) => {
            gate.remove(gate_key);
            None
        }
        None => None,
    }
}

/// 转发成功 → 立即解除短路（Key 恢复后无需等窗口自然到期）。
fn clear_all_failed_gate(gate_key: &str) {
    all_failed_gate().lock().remove(gate_key);
}

/// 把「目标时刻」换算成 `Retry-After` 秒数（向上取整）。
///
/// 结果天然 ≥ 1：唯一调用方只在 `until_ms > now`（整数毫秒，故 `delta ≥ 1`）时调用，
/// `ceil(0.001) = 1`。这条下限很重要——`Retry-After: 0` 等于不退避，客户端会立刻重发，
/// 与短路的目的相反；对**上游给出**的秒数（可能真是 0）由调用方另行夹取，
/// 最终写响应头前还有一道边界守卫（见 `error_resp_with_retry_after`）。
fn retry_after_secs_until(target_ms: i64, now_ms: i64) -> i64 {
    let delta = (target_ms - now_ms).max(0);
    ((delta as f64) / 1000.0).ceil() as i64
}

/// 拼短路窗口键：`{实例命名空间}:{分类名}`。
fn gate_key_of(ns: u64, category: CategoryType) -> String {
    format!("{ns}:{}", category.as_str())
}

/// 代理管理器：管理各分类的代理生命周期
pub struct ProxyManager {
    store: Arc<Store>,
    running: Mutex<HashMap<String, RunningProxy>>,
    /// 本实例的短路窗口命名空间（见 [`all_failed_gate`]）。生产环境只有一个实例，
    /// 故与「按分类」等价；测试里每个用例各建一个实例，天然互不干扰。
    gate_ns: u64,
}

impl ProxyManager {
    pub fn new(store: Arc<Store>) -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT_NS: AtomicU64 = AtomicU64::new(0);
        Self {
            store,
            running: Mutex::new(HashMap::new()),
            gate_ns: NEXT_NS.fetch_add(1, Ordering::Relaxed),
        }
    }

    /// 该分类在本实例下的短路窗口键。
    fn gate_key(&self, category: CategoryType) -> String {
        gate_key_of(self.gate_ns, category)
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
        let gate_key = self.gate_key(category);
        let handle = tokio::spawn(async move {
            let mut loop_shutdown = accept_shutdown;
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let Ok((stream, _)) = accepted else { break };
                        let io = TokioIo::new(stream);
                        let store = store.clone();
                        let gate_key = gate_key.clone();
                        let mut conn_shutdown = shutdown_rx.clone();
                        tokio::spawn(async move {
                            let svc = service_fn(move |req| {
                                handle_request(store.clone(), category, gate_key.clone(), req)
                            });
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
        // 启动即解除该分类的「全部 Key 失败」短路窗口。
        //
        // 窗口原本只有两条解除路径——一次成功转发或自然到期（5s）。而「停止代理 → 换/修好 Key
        // → 重新启动」是用户已经介入的明确信号，此前被忽略，导致刚点启动后最长 5s 内的请求
        // 仍被挡成失败，用户视角是「刚点了启动却说全部 Key 不可用」。
        clear_all_failed_gate(&self.gate_key(category));
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
        // 停止时也清一次：代理已不在服务，残留的短路窗口对下一次启动毫无意义（语义更干净，
        // 也让「stop → 立刻 start」这条路径不依赖 start 侧的清理）。
        clear_all_failed_gate(&self.gate_key(category));
    }
}

/// 处理一次下游工具请求：故障转移路由。
///
/// `gate_key`：本次请求所属「代理实例 + 分类」的短路窗口键（见 [`all_failed_gate`]）。
async fn handle_request(
    store: Arc<Store>,
    category: CategoryType,
    gate_key: String,
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
        && (path_only == "/v1/models" || path_only == "/models")
    {
        return Ok(handle_list_models(&store, category));
    }
    // 单模型检索 `GET /v1/models/{id}`（Anthropic SDK 的 models.retrieve）：必须返回**单个模型
    // 对象**，不能返回列表形状——此前统一走列表分支，客户端拿到 `{"data":[…]}` 解不出 id。
    if req.method() == hyper::Method::GET {
        if let Some(raw_id) = path_only.strip_prefix("/v1/models/") {
            return Ok(handle_retrieve_model(&store, category, raw_id));
        }
    }
    // 官方 gateway 协议里的**非推理端点**（判据：claude.exe v2.1.219 内嵌的 llm-gateway-protocol
    // 规范）。它们必须由代理自己应答，绝不能落进故障转移逻辑被 POST 给上游 AI 中转商——
    // 那样等于把策略查询 / OTLP 遥测体当成补全请求打出去，白烧一次额度还必然报错。
    if let Some(resp) = handle_gateway_side_endpoints(req.method(), path_only) {
        return Ok(resp);
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
        .active_model_of(category.as_str())
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

    // 密钥库锁着（主口令模式、本次进程未解锁）→ 任何 Key 都取不到密钥。
    //
    // 必须在**进入故障转移循环之前**挡住：否则会逐个候选去试、每个都因「未解锁」失败，
    // 于是 record_live_failure 把整池好 Key 刷成熔断、还武装 529 短路窗口——用户解锁后
    // 反倒要等熔断窗口过去才恢复。而真实原因只是没解锁，与 Key 好坏无关。
    //
    // 状态码用 503（service_unavailable）而非 529：529 的语义是「上游过载、稍后重试」，
    // 客户端会自动退避重试；而这里需要的是**人来操作**（打开主窗口输口令），
    // 自动重试永远等不到结果。503 + 明确文案让客户端与用户都知道要做什么。
    if store.secrets.read().is_locked() {
        return Ok(error_resp(
            StatusCode::SERVICE_UNAVAILABLE,
            "密钥库已用主口令加密但尚未解锁：请打开 SynaRoute 主窗口输入主口令解锁后重试。",
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
    // 状态码用 529 overloaded_error（规范要求，客户端据此退避）并带 Retry-After。
    if let Some((remaining_ms, retry_after)) = all_failed_gate_remaining(&gate_key) {
        return Ok(error_resp_with_retry_after(
            overloaded_status(),
            &format!(
                "全部 Key 不可用：上次尝试已确认全部候选均失败，{}s 内不再重试（任一次成功即恢复）",
                (remaining_ms as f64 / 1000.0).ceil() as i64
            ),
            Some(retry_after),
        ));
    }

    // 调用模型日志开关（默认关）：开启后每次转发尝试记一条 request 事件（含完整链路快照）
    let req_log = store.request_log_enabled();

    // 下游发来的原始请求体快照（映射/转换前），供日志展示「我发出去的请求」。
    //
    // **开关关闭时不构造**：这一步会把整个请求体 pretty-print 成 String（Codex 的 body 可达
    // 十几万字符），而它唯一的用途是喂给 `log_request` —— 开关关着时那个闭包直接 return，
    // 这份字符串白构造、还要在每个候选失败分支被 `.clone()` 一次。默认关，故绝大多数请求
    // 都在白做这件事。
    let downstream_body = if !req_log {
        String::new()
    } else if req_json.is_null() {
        String::from_utf8_lossy(&body_bytes).chars().take(REQ_LOG_CAP).collect()
    } else {
        serde_json::to_string_pretty(&req_json).unwrap_or_else(|_| req_json.to_string())
    };

    // 一次成功转发只记**一条**日志（kind = route，带链路快照）。
    //
    // 此前是两条：`route`「成功返回」+ `request`「调用」。两者的 Key 名、模型段完全一样，
    // 只有延迟数字在后者里 —— 高频转发时界面上就是成对的重复行（实测 14 秒刷 12 条、
    // 其中 6 对是同一件事）。合成一条后信息一个不少：延迟并进 detail，链路快照仍挂在这条上。
    //
    // 另带 collapse_key：连续的「同 Key、同请求模型、同流式与否」成功记录会被折叠成一条
    // 带「×N」计数（见 `Store::append_event_collapsible`）。日志文件仍逐条完整写。
    let log_success = |store: &Arc<Store>,
                       key: &ProviderKey,
                       elapsed: u64,
                       url: String,
                       real_model: String,
                       request_body: String,
                       response_body: String,
                       status: u16,
                       streaming: bool,
                       usage: Option<crate::upstream::TokenUsage>| {
        let verb = if streaming { "流式返回" } else { "成功返回" };
        // token 用量直接写进 detail：这是用户判断「额度花在哪」的唯一入口，
        // 藏在链路快照里等于没有（快照默认关、且要展开才看得到）。
        let usage_part = usage
            .as_ref()
            .map(|u| format!(" · {}", u.fmt_compact()))
            .unwrap_or_default();
        let detail = format!(
            "{} · {} · {} · {}ms{}",
            key.name,
            verb,
            fmt_route_model_for_key(key, &requested_model),
            elapsed,
            usage_part
        );
        // 链路快照只在「调用模型日志」开关开启时产生（正文可达 2×20000 字符）。
        let trace = req_log.then(|| RequestTrace {
            key_name: key.name.clone(),
            vendor: key.vendor.clone(),
            protocol: key.protocol,
            url,
            requested_model: requested_model.clone(),
            real_model,
            request_body: cap(&request_body),
            response_body: cap(&response_body),
            status: Some(status),
            latency_ms: elapsed,
            ok: true,
        });
        let collapse = format!("ok:{}:{}:{}", key.id, requested_model, streaming);
        store.append_event_full(
            category,
            "route",
            Some(&key.id),
            &detail,
            trace,
            Some(collapse),
            usage,
        );
    };

    // 失败的单次尝试（开关开启时记，含链路快照供排障）。
    //
    // 与成功路径分开：失败信息（状态码、上游错误体）是排障核心，且它总伴随一条 failover
    // 事件说明「转给了谁」，两条不重复。折叠判据带上状态码 —— 同 Key 连续同码失败才合并，
    // 401 与 500 交替出现时不会被压成一条。
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
        let collapse = format!(
            "req:{}:{}:{}:{}",
            key.id,
            requested_model,
            ok,
            status.map(|s| s.to_string()).unwrap_or_else(|| "conn".into())
        );
        store.append_event_collapsible(
            category,
            "request",
            Some(&key.id),
            &detail,
            Some(trace),
            Some(collapse),
        );
    };

    // 流式可走真流式的条件：同协议直通，或跨协议但有受支持的 SSE 翻译方向。
    // 其余跨协议组合（无翻译器）才跳过，交给故障转移找同协议 Key。
    let can_stream = |k: &ProviderKey| {
        downstream == k.protocol
            || crate::upstream::sse_direction(downstream, k.protocol).is_some()
    };

    let mut last_err = String::new();
    // 候选给出的 `Retry-After`（秒）。上游 429/503 常带此头；全部候选失败后要把它透传给下游，
    // 否则客户端按自己的固定节奏重发，仍会撞在上游限流窗口里（官方 gateway 规范明确要求透传）。
    //
    // 取**最小值**，不是最大值。下游重试面对的是**整池**，只要有一个候选恢复就该放行：
    // 候选 A 撞上按天配额回 `Retry-After: 3600`（夹到 300），候选 B 只是一次瞬时连接抖动 ——
    // 取最大值会让整个分类停摆 300 秒，而 1 秒后 B 就能服务了。这正是本项目最痛的「误伤」形态：
    // 一个 Key 的限流窗口被强加到所有 Key 头上。取最小值即「各候选中最早可再试的时刻」，
    // 早到的那次重试若仍失败，短路窗口会再武装一次，代价只是一次探路请求。
    let mut retry_after_hint: Option<i64> = None;
    // 最后一次失败的上游状态码（连接层失败为 None）。用于区分「等一等可能好」与
    // 「不可自愈的配置错误」——两者该给下游的状态码完全不同，见函数尾部。
    let mut last_status: Option<u16> = None;
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
            match try_stream_to_key(&store, category, key, &path, &req_json, &requested_model, &fwd_headers)
                .await
            {
                Ok(StreamAttempt::Streaming { resp, url, real_model, request_body }) => {
                    health::record_live_success(&store, &key.id);
                    // 有 Key 能用了 → 立即解除「全部失败」短路，不等窗口自然到期。
                    clear_all_failed_gate(&gate_key);
                    let elapsed = started.elapsed().as_millis() as u64;
                    // 流式成功也记一条 request 事件（开关开启时）。请求体以「转换后发往上游」为主
                    // （含 reasoning→thinking 映射结果，排障核心），单独完整保留、放最前；
                    // 「下游原始 body」（Codex 发来、转换前）体量极大（可达十几万字符），仅在
                    // log_downstream_raw_enabled 开关开启时追加、且单独截到小额度，避免把转换后段挤没
                    // ——此前双段直接拼接后被整体 cap(20000) 截断，转换后段常被下游原始段整段吞掉。
                    //
                    // `req_log &&` 前置：开关关着时 log_request 会直接 return，没必要先做这段
                    // 字符串拼接 + 两次截断（也就不必再读一次 settings）。
                    let combined_req = if req_log && store.log_downstream_raw_enabled() {
                        format!(
                            "==== 转换后发往上游 ====\n{}\n\n==== 下游原始请求（转换前，Codex 发来）====\n{}",
                            cap_to(&request_body, 8000),
                            cap_to(&downstream_body, 4000)
                        )
                    } else {
                        request_body
                    };
                    log_success(
                        &store,
                        key,
                        elapsed,
                        url,
                        real_model,
                        combined_req,
                        "（流式响应：边收边发，body 不留存。如需完整响应体，请在客户端侧抓取）".to_string(),
                        200,
                        true,
                        // 流式直通拿不到 usage：SSE 是边收边发、不缓存全文，用量藏在
                        // 最后几个 chunk 里，要取就得把整个流缓存下来 —— 那会毁掉流式的
                        // 首字节延迟优势。故如实留空，而不是编一个数字。
                        // （聚合走的是非流式路径，那里有完整 usage，见 aggregate。）
                        None,
                    );
                    return Ok(resp);
                }
                // 上游非 2xx：记录并切下一个
                Ok(StreamAttempt::HttpError { status, body, url, real_model, retry_after }) => {
                    let elapsed = started.elapsed().as_millis() as u64;
                    let snippet: String = body.trim().chars().take(400).collect();
                    last_err = if snippet.is_empty() {
                        format!("HTTP {status}")
                    } else {
                        format!("HTTP {status}: {snippet}")
                    };
                    if let Some(s) = retry_after {
                        retry_after_hint = Some(retry_after_hint.map_or(s, |cur: i64| cur.min(s)));
                    }
                    last_status = Some(status);
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
                    last_status = None; // 连接层失败：无状态码，按临时错误对待
                    log_request(&store, key, elapsed, String::new(), key.resolve_model(&requested_model), downstream_body.clone(), last_err.clone(), None, false);
                    health::record_live_failure(&store, &key.id);
                    log_failover(&store, key, "失败", &last_err, next);
                    continue;
                }
            }
        }

        // 流式 + 无法翻译的跨协议组合（如两端各为 Anthropic/Responses，无中枢路径）：
        // 绝不能走缓冲路径返回 application/json——下游按 text/event-stream 解析必失败。
        // 跳过该候选，让故障转移去找可流式的 Key。
        if wants_stream && !can_stream(key) {
            last_err = "流式请求不支持跨协议转换（该 Key 协议与下游不一致）".to_string();
            // 501：这是**配置不匹配**，等多久都不会好转，不能包装成「过载请重试」
            // （见函数尾部的状态码分流）。用一个明确的「不支持」码让用户去改配置。
            last_status = Some(StatusCode::NOT_IMPLEMENTED.as_u16());
            log_failover(&store, key, "跳过", &last_err, next);
            continue;
        }

        let result =
            forward_to_key(&store, category, key, &path, &req_json, &requested_model, &fwd_headers)
                .await;
        let elapsed = started.elapsed().as_millis() as u64;
        match result {
            Ok(outcome) if outcome.ok => {
                let resp_text = String::from_utf8_lossy(&outcome.bytes).to_string();
                health::record_live_success(&store, &key.id);
                // 有 Key 能用了 → 立即解除「全部失败」短路，不等窗口自然到期。
                clear_all_failed_gate(&gate_key);
                // 非流式有完整响应体 → 能真取到 token 用量（两协议字段名已在
                // extract_usage 里归一）。上游没给用量时返回 None，日志如实不显示。
                let usage = serde_json::from_slice::<Value>(&outcome.bytes)
                    .ok()
                    .and_then(|v| crate::upstream::extract_usage(&v));
                log_success(
                    &store,
                    key,
                    elapsed,
                    outcome.url.clone(),
                    outcome.real_model.clone(),
                    outcome.request_body.clone(),
                    resp_text,
                    outcome.status,
                    false,
                    usage,
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
                if let Some(s) = outcome.retry_after {
                    retry_after_hint = Some(retry_after_hint.map_or(s, |cur: i64| cur.min(s)));
                }
                last_status = Some(outcome.status);
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
                last_status = None; // 连接层失败：无状态码，按临时错误对待
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

    // 按「最后一次失败的性质」分流。全部候选失败有两类完全不同的成因，给下游同一个状态码
    // 是错的：
    //
    // - **临时性**（429 限流 / 408 超时 / 409 冲突 / 5xx / 连接层失败）：等一等可能真的会好。
    //   回 529 `overloaded_error` + `Retry-After`，客户端据此退避重试。（此前用 502 →
    //   `api_error`，客户端重试行为不确定，实测表现为立刻重发、把失败放大成轮询。）
    //   并武装短路窗口，挡住窗口内的重发。
    //
    // - **硬错误**（401/403/404 等其余 4xx、协议不匹配的 501）：密钥填错、模型名不存在、
    //   下游要流式而该 Key 协议无法翻译 —— 这些等到宇宙尽头也不会好转。若也回 529，客户端会当
    //   「上游过载」持续退避重试，界面上呈现「过载」而真实根因是配置错误，方向完全相反；
    //   而且短路窗口一到期就放行一个真实请求再撞一次 401，如此循环。故**原样回该状态码**、
    //   **不带 Retry-After**、**不武装短路窗口**：让客户端第一次就拿到确定的失败，
    //   用户去改配置。
    //
    // 429/408/409 之所以划进临时性：它们本就是「稍后重试」语义，且与 Anthropic SDK 的
    // 重试判据（408/409/429 或 >= 500）一致。
    const TRANSIENT_4XX: [u16; 3] = [408, 409, 429];
    let is_hard_error = matches!(
        last_status,
        Some(s) if (s == 501 || ((400..500).contains(&s) && !TRANSIENT_4XX.contains(&s)))
    );

    if is_hard_error {
        let status = last_status
            .and_then(|s| StatusCode::from_u16(s).ok())
            .unwrap_or(StatusCode::BAD_GATEWAY);
        return Ok(error_resp(status, &format!("全部 Key 不可用：{last_err}")));
    }

    // 武装短路窗口：窗口内后续请求（含客户端自动重发）直接失败，不再重打全部上游。
    // 带上候选给出的最早 Retry-After（见 retry_after_hint 的取最小值理由）。
    arm_all_failed_gate(&gate_key, retry_after_hint);
    // 退避秒数：有上游值就用它（夹到 [1, MAX]——上游可能给 0，那等于不退避，与短路矛盾）；
    // 没有则用短路窗口本身的长度（窗口内重试注定被挡，早回来毫无意义）。
    let retry_after = retry_after_hint
        .map(|s| s.clamp(1, MAX_RETRY_AFTER_SECS))
        .unwrap_or((ALL_FAILED_SHORT_CIRCUIT_MS / 1000).max(1));
    Ok(error_resp_with_retry_after(
        overloaded_status(),
        &format!("全部 Key 不可用：{last_err}"),
        Some(retry_after),
    ))
}

/// 529 的 `StatusCode`。529 非 IANA 注册码（Cloudflare/Anthropic 惯例的「过载」码），
/// http crate 无关联常量，只能 `from_u16` 构造；它对 100~999 恒成功，故 expect 不会触发。
fn overloaded_status() -> StatusCode {
    StatusCode::from_u16(STATUS_OVERLOADED).expect("529 是合法 HTTP 状态码")
}

/// 单模型检索 `GET /v1/models/{id}`：返回**单个**模型对象（Anthropic SDK 的 models.retrieve
/// 期望的形状），查不到则按官方规范返回 404 + 标准错误信封。
fn handle_retrieve_model(
    store: &Arc<Store>,
    category: CategoryType,
    raw_id: &str,
) -> Response<ResBody> {
    // 去掉可能的 query（`?beta=true`）与尾斜杠；SDK 会做 URL 编码，这里解一层百分号。
    let id = raw_id.split('?').next().unwrap_or("").trim_end_matches('/');
    if id.is_empty() {
        return handle_list_models(store, category);
    }
    let models = discoverable_models(&store.enabled_keys_sorted(category));
    // 客户端可能用对外名，也可能用我们暴露的 gateway 别名（claude-synaroute-*），两者都接。
    let hit = models.iter().find(|m| {
        m.as_str() == id || crate::model::to_gateway_model_id(m) == id
    });
    match hit {
        Some(m) => {
            let body = if matches!(category, CategoryType::Codex) {
                serde_json::json!({ "id": m, "object": "model", "owned_by": "synaroute" })
            } else {
                serde_json::json!({
                    "type": "model",
                    "id": crate::model::to_gateway_model_id(m),
                    "display_name": m,
                })
            };
            json_resp(StatusCode::OK, Bytes::from(serde_json::to_vec(&body).unwrap_or_default()))
        }
        None => error_resp(StatusCode::NOT_FOUND, &format!("未知模型: {id}")),
    }
}

/// 官方 gateway 协议里的非推理端点，由代理本地应答。
///
/// 判据全部来自 claude.exe v2.1.219 内嵌的 llm-gateway-protocol 规范原文：
/// - `GET /managed/settings`（可选）：「Return `404` for "no managed policy"；
///   `200 {}` means "this user has an empty policy" — they're not the same」。
///   SynaRoute 不做企业策略下发，故返 404（干净的「未实现」）。
/// - `POST /v1/{metrics,logs,traces}`（可选，OTLP）：「Return `200` whether you forward or
///   discard — `404` makes the client's exporter log an error on every flush」。
///   故一律 200 丢弃，避免客户端每次 flush 刷错误日志。
///
/// 返回 `None` 表示「不是这些端点」，交由原有故障转移逻辑继续处理。
fn handle_gateway_side_endpoints(
    method: &hyper::Method,
    path_only: &str,
) -> Option<Response<ResBody>> {
    if method == hyper::Method::GET && path_only == "/managed/settings" {
        return Some(error_resp(
            StatusCode::NOT_FOUND,
            "SynaRoute 不下发企业管控策略（managed settings 未实现）",
        ));
    }
    if method == hyper::Method::POST
        && matches!(path_only, "/v1/metrics" | "/v1/logs" | "/v1/traces")
    {
        // 明确丢弃但回 200：规范要求如此，否则客户端 OTLP exporter 每次 flush 记一条错误。
        return Some(json_resp(StatusCode::OK, Bytes::from_static(b"{}")));
    }
    None
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
    /// `retry_after`：上游 `Retry-After` 头解析出的秒数（429/503 常带），无则 None。
    HttpError {
        status: u16,
        body: String,
        url: String,
        real_model: String,
        retry_after: Option<i64>,
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
    category: CategoryType,
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
    inject_default_effort(store, category, &mut payload, downstream, key.protocol);
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
    // `anthropic-beta` 单独算（1M 上下文需按落点模型追加特性），故先跳过原值再统一设置。
    let beta = effective_beta_header(fwd_headers, key, &real_model);
    for (h, v) in fwd_headers {
        if h == "anthropic-beta" {
            continue;
        }
        rb = rb.header(h, v);
    }
    if let Some(b) = &beta {
        rb = rb.header("anthropic-beta", b.as_str());
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
        // 非 2xx：缓冲错误体供切换决策与日志。Retry-After 须在读 body（消费 resp）之前取。
        let retry_after = parse_retry_after(resp.headers());
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
            retry_after,
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
    /// 上游 `Retry-After` 头解析出的秒数（429/503 常带），无则 None。
    retry_after: Option<i64>,
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
    category: CategoryType,
    payload: &mut Value,
    downstream: Protocol,
    upstream: Protocol,
) {
    // 只作用于「下游是 Responses 协议」的请求（实际就是 Codex）。含同协议 Responses 上游：
    // 经 SynaRoute 转发的一律是自定义 provider，Codex 对自定义 provider 不下发 effort
    //（方案 A 的前提），故同协议直通场景也需补默认强度，否则接 Responses 协议第三方中转商
    // 时配置的推理强度不生效。
    // （upstream 参数保留以备将来按上游细分策略，当前不再据此跳过。）
    let _ = upstream;
    if downstream != Protocol::OpenaiResponses {
        return;
    }
    // 用**本请求所属分类**取 effort，而非硬编码 Codex：分类各有独立端口与独立设置，
    // 硬编码会让「把某个 Responses 客户端接到别的分类端口」时读到 Codex 的强度（口径串台）。
    let effort = match store.active_effort_of(category.as_str()) {
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

/// 转发到单个 Key：套用模型映射 + 协议适配。
/// 返回完整 outcome（含发往上游的请求体、响应体、状态），供路由与调用模型日志共用。
/// 注意：非 2xx 不再直接返回 Err，而是照常返回 outcome（ok=false），
/// 由调用方决定是否切换——这样失败也能被完整记进调用模型日志。
async fn forward_to_key(
    store: &Arc<Store>,
    category: CategoryType,
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
    inject_default_effort(store, category, &mut payload, downstream, key.protocol);

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
    // `anthropic-beta` 单独算（1M 上下文需按落点模型追加特性），故先跳过原值再统一设置。
    let beta = effective_beta_header(fwd_headers, key, &real_model);
    for (h, v) in fwd_headers {
        if h == "anthropic-beta" {
            continue;
        }
        rb = rb.header(h, v);
    }
    if let Some(b) = &beta {
        rb = rb.header("anthropic-beta", b.as_str());
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
    // Retry-After 须在 resp.bytes() 消费响应体之前取（bytes() 拿走所有权）。
    let retry_after = parse_retry_after(resp.headers());
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
        retry_after,
    })
}

/// 判断某个下游请求头是否应被剔除、不透传给上游。
/// 剔除项：鉴权（用 Key 自己的）、路由/长度类（reqwest 按目标重算）、
/// hop-by-hop（RFC 7230）、content-type（reqwest .json() 自带）、
/// accept-encoding（避免上游返回压缩体导致响应快照乱码）。
/// 1M 上下文对应的 Anthropic beta 特性名。
///
/// **判据来源**（反查而非文档推测）：`claude.exe` v2.1.219 内嵌的 beta 特性注册表
/// （offset ≈ 186988648）里成对出现 `long_context` → `context-1m-2025-08-07`，
/// 与 `interleaved_thinking` → `interleaved-thinking-2025-05-14` 等同一张表。
const BETA_CONTEXT_1M: &str = "context-1m-2025-08-07";

/// 计算发往上游的 `anthropic-beta` 头：在下游原值基础上**按需追加** 1M 上下文特性。
///
/// ## 为什么代理要代劳
///
/// Claude Code 只对**它认识的**模型名发 `context-1m-2025-08-07`。经 SynaRoute 路由时，
/// 客户端看到的是我们的对外名（`claude-opus-4-8`、或非合规名被包成 `claude-synaroute-*`），
/// 客户端不认识 → 不会发这个头 → 即使上游模型真支持 1M，实际仍按默认窗口截断，
/// 用户配了大 `contextWindow` 却完全不生效（又一个「看起来配了、实际没用上」）。
///
/// 故判据取「**本次实际要打的真实模型**的 `contextWindow` ≥ 1M」——不是看客户端要什么，
/// 而是看请求最终落在哪个模型上。桌面端侧的 `supports1m` 是同一份数据的另一面
/// （见 `tools::build_desktop_model_entries`），两端口径因此天然一致。
///
/// ## 必须追加而非替换
///
/// 下游原值里带着 `claude-code-20250219` 等特性，那是中转商识别「真实 Claude Code 客户端」
/// 的依据（部分分组只放行 CC）。整体覆盖会让这些请求被拒。已存在同名特性时不重复追加。
fn effective_beta_header(
    fwd_headers: &[(String, String)],
    key: &ProviderKey,
    real_model: &str,
) -> Option<String> {
    let existing = fwd_headers
        .iter()
        .find(|(h, _)| h == "anthropic-beta")
        .map(|(_, v)| v.as_str());
    // beta 头是 Anthropic 协议特有的；打到 OpenAI 系上游时不加（原值仍按既有行为透传）
    let want_1m = matches!(key.protocol, Protocol::Anthropic)
        && key
            .context_window_of_real(real_model)
            .is_some_and(|w| w >= crate::model::ONE_MILLION_CONTEXT);
    if !want_1m {
        return existing.map(|s| s.to_string());
    }
    match existing.map(str::trim).filter(|s| !s.is_empty()) {
        // 客户端已经带上了 → 原样用，不重复追加
        Some(v) if v.split(',').any(|p| p.trim() == BETA_CONTEXT_1M) => Some(v.to_string()),
        Some(v) => Some(format!("{},{BETA_CONTEXT_1M}", v.trim_end_matches(','))),
        None => Some(BETA_CONTEXT_1M.to_string()),
    }
}

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
/// - 529：上游明说「我过载了，稍后再来」。我们自己对下游正是用它表达这个意思
///   （见 `STATUS_OVERLOADED`），收到时却判成「Key 坏了」显然口径矛盾。
///
/// **400 也不计入**（2026-07-31 实机复盘的结论）：400 的语义是「这个请求不合法」，
/// 而请求是下游客户端发的、或经我们的协议转换构造的——它与用哪个 Key 无关，
/// 换任何 Key 都会同样 400。此前把 400 计入熔断，导致：
/// - 客户端的空探测请求（`{"messages":[],"model":null}`）连打三次就把**完好的 Key** 熔断；
/// - 我们自己的跨协议转换 bug（上游报 "model is required" / "Failed to parse request body"）
///   会把整池好 Key 逐个刷成熔断，进而触发全熔断兜底、把所有 Key 反复重打。
///
/// 实测那天 68 条失败请求里近半是这两类。
///
/// 这类只做「切下一个 Key 应急」，但**不累加 fail_count、不熔断**——下个请求仍优先用它。
///
/// 仍然计入熔断的是**确定属于这个 Key** 的故障：
/// 401/403 鉴权失败（密钥错/被封）、404 端点或模型不存在——换 Key 才有意义，
/// 重试同一个只是白试，连续几次后熔断掉它，避免每个请求都从它开始。
fn status_counts_against_breaker(status: u16) -> bool {
    !matches!(status, 400 | 429 | 500 | 502 | 503 | 504 | 529)
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

/// 故障转移日志里的动词：**按真实状态码分类**，让用户一眼看出该去修什么。
///
/// 2026-08-02 真机实证的反例（勿退回旧写法）：旧版只分两类 ——
/// `status_counts_against_breaker` 为真说「失败」、否则一律说「限流/繁忙，暂避」。
/// 于是一次会话里 400×16 + 502×5 + 503×5 + 504×1 **全被写成「限流/繁忙」，
/// 而真实 429 一次都没有**。用户据此判断「触发了很多限流」，方向完全错了：
/// 那 16 个 400 是我们自己没填模型名（见 `resolve_model` 第 6 步），
/// 11 个 5xx 是上游中转商真的挂了/无可用账号，两者都与限流无关。
///
/// 措辞要能直接指向排查方向，不能用一个模糊词盖住三种不同的根因。
fn failover_verb(status: u16) -> &'static str {
    match status {
        // 真限流：唯一该说「限流」的码
        429 => "限流，暂避",
        // 请求本身不合法：换任何 Key 都同样失败，别让用户以为是 Key 的问题
        400 | 422 => "请求被拒（非 Key 问题）",
        // 鉴权/权限：Key 本身坏了或没权限
        401 | 403 => "鉴权失败",
        // 端点或模型不存在
        404 => "端点/模型不存在",
        // 上游协议不支持（我们的跨协议转换打到了不支持的端点）
        501 => "上游不支持该协议",
        // 网关层故障：上游中转商挂了，与限流无关
        502..=504 => "上游故障/无可用渠道",
        // 其余 5xx
        s if s >= 500 => "上游错误",
        // 其余 4xx
        s if s >= 400 => "请求失败",
        _ => "失败",
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

/// HTTP status → Anthropic 错误信封的 `error.type`。
///
/// 判据来自 Claude Code 内嵌的官方 gateway 协议规范（claude.exe v2.1.219 里的
/// llm-gateway-protocol 文档）给出的对应表。用错的 type 会让下游 SDK 走错重试策略
/// （例如把限流当参数错、不再退避重试）。
fn anthropic_error_type(status: u16) -> &'static str {
    match status {
        400 => "invalid_request_error",
        401 => "authentication_error",
        403 => "permission_error",
        404 => "not_found_error",
        413 => "request_too_large",
        429 => "rate_limit_error",
        501 => "not_supported",
        529 => "overloaded_error",
        s if s >= 500 => "api_error",
        _ => "invalid_request_error",
    }
}

/// 统一错误响应。
///
/// 形状必须是 Anthropic 官方错误信封 `{"type":"error","error":{"type","message"}}`——
/// 三个端的客户端都用 Anthropic SDK 解析错误：缺 `type` 时 SDK 拿不到错误分类，
/// 只能当未知错误处理（表现为界面上一句无意义的报错、且重试策略失效）。
/// 额外保留 `source` 便于用户一眼看出这条错误是代理产生的、不是上游返回的。
fn error_resp(status: StatusCode, msg: &str) -> Response<ResBody> {
    error_resp_with_retry_after(status, msg, None)
}

/// 带 `Retry-After` 的错误响应。
///
/// 官方 gateway 规范（claude.exe v2.1.219 内嵌的 llm-gateway-protocol 原文）要求：限流/过载类
/// 响应（429/529）必须带 `Retry-After`，否则下游客户端无法正确退避——表现为收到 5xx 立刻重发，
/// 把上游限流窗口反复撞满（这正是「一直轮询」现象的一环）。
fn error_resp_with_retry_after(
    status: StatusCode,
    msg: &str,
    retry_after_secs: Option<i64>,
) -> Response<ResBody> {
    let body = serde_json::json!({
        "type": "error",
        "error": {
            "type": anthropic_error_type(status.as_u16()),
            "message": msg,
            "source": "synaroute",
        }
    });
    let bytes = Bytes::from(serde_json::to_vec(&body).unwrap());
    let mut builder = Response::builder()
        .status(status)
        .header("content-type", "application/json");
    if let Some(secs) = retry_after_secs {
        builder = builder.header("retry-after", secs.max(1).to_string());
    }
    builder.body(full_body(bytes)).unwrap()
}

/// 从上游响应头解析 `Retry-After`（RFC 7231）：既支持「秒数」也支持 HTTP-date。
/// 解析不出（缺头/格式怪/已过期）返回 None，由调用方回退到自己的退避口径。
fn parse_retry_after(headers: &reqwest::header::HeaderMap) -> Option<i64> {
    let raw = headers.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?;
    let raw = raw.trim();
    // 形态一：delta-seconds。
    if let Ok(secs) = raw.parse::<i64>() {
        return Some(secs.clamp(0, MAX_RETRY_AFTER_SECS));
    }
    // 形态二：HTTP-date（如 `Wed, 21 Oct 2026 07:28:00 GMT`）→ 换算成相对秒数。
    let when = chrono::DateTime::parse_from_rfc2822(raw).ok()?;
    let delta = when.timestamp() - chrono::Utc::now().timestamp();
    Some(delta.clamp(0, MAX_RETRY_AFTER_SECS))
}

/// `Retry-After` 采纳上限（秒）。上游偶有返回极大值（甚至几小时）的情况；照抄会让客户端
/// 长时间彻底不再重试，而我们的 Key 可能几秒后就恢复（换 Key、改配置都会立即解除短路）。
/// 取 300s：足够表达「别急着重试」，又不至于把客户端锁死。
const MAX_RETRY_AFTER_SECS: i64 = 300;

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
        // 529 也在内：上游明说「过载，稍后再来」，与我们对下游的口径一致，不该反过来判 Key 坏了。
        for s in [429u16, 500, 502, 503, 504, 529] {
            assert!(!status_counts_against_breaker(s), "HTTP {s} 不应计入熔断");
        }
        // 400「请求不合法」与 Key 无关（换任何 Key 都同样 400）：不得因客户端空探测
        // 或我们自己的协议转换 bug 把完好的 Key 刷成熔断。
        assert!(
            !status_counts_against_breaker(400),
            "HTTP 400 是请求问题、不是 Key 问题，不应计入熔断"
        );
        // 确定属于该 Key 的故障：鉴权失败 / 端点或模型不存在 → 计入熔断。
        for s in [401u16, 403, 404, 422] {
            assert!(status_counts_against_breaker(s), "HTTP {s} 应计入熔断");
        }
        // 日志动词按**真实状态码**分类：一个模糊的「限流」会把三种不同根因盖住。
        // 真机反例：一次会话 400×16 + 5xx×11、429 零次，旧写法全标成「限流/繁忙」，
        // 用户据此以为触发了限流，而真因是我们没填模型名 + 上游中转商挂了。
        assert_eq!(failover_verb(429), "限流，暂避", "只有 429 该说限流");
        assert_eq!(failover_verb(400), "请求被拒（非 Key 问题）");
        assert_eq!(failover_verb(401), "鉴权失败");
        assert_eq!(failover_verb(403), "鉴权失败");
        assert_eq!(failover_verb(404), "端点/模型不存在");
        for s in [502u16, 503, 504] {
            assert_eq!(
                failover_verb(s),
                "上游故障/无可用渠道",
                "HTTP {s} 是上游挂了，不是限流"
            );
        }
        // 反例护栏：除 429 外，任何码都不得出现「限流」二字
        for s in [400u16, 401, 403, 404, 422, 500, 501, 502, 503, 504] {
            assert!(
                !failover_verb(s).contains("限流"),
                "HTTP {s} 不该被说成限流，实际：{}",
                failover_verb(s)
            );
        }
    }

    #[tokio::test]
    async fn error_resp_uses_official_anthropic_envelope() {
        // 三个端的客户端都用 Anthropic SDK 解错误：缺 type 会让 SDK 归类不出错误、
        // 重试策略失效。判据来自 claude.exe 内嵌的官方 gateway 规范对应表。
        async fn body_json(resp: Response<ResBody>) -> Value {
            let bytes = resp.into_body().collect().await.unwrap().to_bytes();
            serde_json::from_slice(&bytes).unwrap()
        }
        let cases = [
            (400u16, "invalid_request_error"),
            (401, "authentication_error"),
            (403, "permission_error"),
            (413, "request_too_large"),
            (429, "rate_limit_error"),
            (501, "not_supported"),
            (503, "api_error"),
            // 529 是 Anthropic 特有的过载码，规范要求客户端退避重试。
            (529, "overloaded_error"),
        ];
        for (code, want) in cases {
            let status = StatusCode::from_u16(code).unwrap();
            let resp = error_resp(status, "boom");
            assert_eq!(resp.status(), status);
            let v = body_json(resp).await;
            assert_eq!(v["type"], "error", "顶层必须是 type:error");
            assert_eq!(v["error"]["type"], want, "status {code} 的 error.type 不对");
            assert_eq!(v["error"]["message"], "boom");
            assert_eq!(v["error"]["source"], "synaroute", "保留来源标记便于分辨是代理产生的");
        }
    }

    /// 推理强度必须取**本请求所属分类**的设置，不能硬编码 Codex。
    /// 硬编码时把任一 Responses 客户端接到别的分类端口，就会读到 Codex 的强度（口径串台）。
    #[test]
    fn effort_injection_reads_requesting_category_not_hardcoded_codex() {
        let dir = temp_dir("effort_cat");
        let store = std::sync::Arc::new(
            Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap(),
        );
        // active_efforts 是后端自管字段：save_settings 会刻意剥掉入参里的它（防前端旧快照
        // 顶回用户刚选的值），故必须走专用写入方法 set_active_effort。
        store.set_active_effort(CategoryType::Codex.as_str(), "high").unwrap();
        store.set_active_effort(CategoryType::ClaudeDesktop.as_str(), "low").unwrap();
        let inject = |cat: CategoryType| -> Value {
            let mut payload = json!({ "model": "m", "input": [] });
            inject_default_effort(
                &store,
                cat,
                &mut payload,
                Protocol::OpenaiResponses,
                Protocol::OpenaiResponses,
            );
            payload
        };
        assert_eq!(inject(CategoryType::Codex)["reasoning"]["effort"], "high");
        assert_eq!(
            inject(CategoryType::ClaudeDesktop)["reasoning"]["effort"],
            "low",
            "桌面端分类必须用自己的强度，不得拿 Codex 的"
        );
        // 未配该分类 → 完全不注入（保持现状，别凭空造字段）。
        assert!(
            inject(CategoryType::ClaudeCli).get("reasoning").is_none(),
            "未配强度的分类不应注入 reasoning"
        );

        // 下游非 Responses（Claude 系走 /v1/messages）→ 一律不注入。
        let mut anth = json!({ "model": "m", "messages": [] });
        inject_default_effort(
            &store,
            CategoryType::Codex,
            &mut anth,
            Protocol::Anthropic,
            Protocol::Anthropic,
        );
        assert!(anth.get("reasoning").is_none(), "Anthropic 下游不该被注入 reasoning");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 三端链路矩阵：每个分类的规范下游路径 → 协议判定，以及「下游协议 × 上游 Key 协议」
    /// 九种组合都必须能流式（同协议直通 or 有跨协议翻译方向）。
    ///
    /// 这条锁住「流式不得退化成缓冲」：proxy 对「要 stream 却没有翻译方向」的组合会直接报错，
    /// 若某个组合漏了方向，用户侧表现是该分类一开流式就失败，而不是慢一点。
    #[test]
    fn three_category_chain_matrix_is_complete() {
        // 各端真实发出的路径（含 count_tokens 子路径与带 query 的形态）。
        let cases = [
            ("/v1/messages", Protocol::Anthropic, "Claude CLI / 桌面端 补全"),
            ("/v1/messages/count_tokens?beta=true", Protocol::Anthropic, "桌面端 token 计数"),
            ("/v1/responses", Protocol::OpenaiResponses, "Codex 补全"),
            ("/v1/chat/completions", Protocol::OpenaiChat, "OpenAI 兼容客户端"),
        ];
        for (path, want, who) in cases {
            assert_eq!(downstream_protocol(path), want, "{who} 的路径协议判定错: {path}");
        }

        use crate::upstream::sse_direction;
        let all = [Protocol::Anthropic, Protocol::OpenaiChat, Protocol::OpenaiResponses];
        for down in all {
            for up in all {
                if down == up {
                    assert!(
                        sse_direction(down, up).is_none(),
                        "同协议应直通（无需翻译方向）: {down:?}"
                    );
                } else {
                    assert!(
                        sse_direction(down, up).is_some(),
                        "跨协议缺流式翻译方向 → 该组合一开流式就失败: {down:?} ← {up:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn all_failed_gate_arms_blocks_then_clears_on_success() {
        // 回归护栏（用户实测「一直轮询」）：全部候选失败后必须短路，窗口内不再重打上游。
        // 实测现场：单次故障窗口 3 条并发链各走完全部候选、16 条事件，相邻两次「全部 Key 失败」
        // 间隔低至 0.2s —— 因为 select_candidates 在「全熔断」时忽略熔断窗口把所有 Key 原样返回，
        // 熔断在最需要它的场景形同虚设，客户端 5xx 自动重发于是无限放大。
        //
        // 用**本用例专属的键**：gate 是进程级状态，键里带实例命名空间，测试直接用一个不与任何
        // ProxyManager 冲突的合成串即可与并发跑的 e2e 用例完全隔离。
        let cat = "test:gate_arms_clears";
        clear_all_failed_gate(cat);
        assert!(
            all_failed_gate_remaining(cat).is_none(),
            "初始不应处于短路"
        );

        arm_all_failed_gate(cat, None);
        let (remaining, retry_after) =
            all_failed_gate_remaining(cat).expect("武装后应处于短路窗口");
        assert!(
            remaining > 0 && remaining <= ALL_FAILED_SHORT_CIRCUIT_MS,
            "剩余窗口应在 (0, {ALL_FAILED_SHORT_CIRCUIT_MS}] 内，实际 {remaining}"
        );
        // 无上游 Retry-After 时，退避时间取短路剩余（向上取整、至少 1s）——绝不能是 0，
        // `Retry-After: 0` 等于不退避，客户端会立刻重发，与短路目的相反。
        assert!(
            (1..=ALL_FAILED_SHORT_CIRCUIT_MS / 1000).contains(&retry_after),
            "Retry-After 应落在 [1, {}]，实际 {retry_after}",
            ALL_FAILED_SHORT_CIRCUIT_MS / 1000
        );

        // 上游给了更长的退避（如 429 带 Retry-After: 30）→ 必须采纳更晚者，不能只用 5s 窗口，
        // 否则客户端 5s 后重来仍撞在上游限流窗口里。
        clear_all_failed_gate(cat);
        arm_all_failed_gate(cat, Some(30));
        let (_, upstream_retry) = all_failed_gate_remaining(cat).expect("应处于短路窗口");
        assert!(
            upstream_retry >= 29,
            "上游要求 30s 退避时不得只回 5s，实际 {upstream_retry}"
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
        // 分类隔离：一个分类全挂不能连带把另一个分类的请求也短路掉。
        // 用真实的键构造函数（gate_key_of），确保测的就是生产用的那套键，而不是随手拼的串。
        let ns = 999_999u64;
        let a = gate_key_of(ns, CategoryType::ClaudeDesktop);
        let b = gate_key_of(ns, CategoryType::ClaudeCli);
        clear_all_failed_gate(&a);
        clear_all_failed_gate(&b);

        arm_all_failed_gate(&a, None);
        assert!(all_failed_gate_remaining(&a).is_some(), "A 分类应短路");
        assert!(
            all_failed_gate_remaining(&b).is_none(),
            "B 分类不应被 A 的失败牵连"
        );

        clear_all_failed_gate(&a);
    }

    /// 短路窗口必须按**代理实例**隔离，而不只是按分类名。
    ///
    /// 测试污染回归：gate 是进程级 `HashMap`，早先只用分类名做键，不认是哪个 `ProxyManager` /
    /// `Store` / 临时目录。两条都用 `ClaudeCli` 的 e2e 用例于是共享同一格——`proxy_all_fail_*`
    /// 武装的 5s 窗口把 `proxy_fails_over_bad_to_good` 的请求直接短路掉，表现为同一份代码三次
    /// 跑出 276/1、276/1、277/0 的偶发红。键里带实例命名空间后，两个实例天然互不干扰。
    #[test]
    fn gate_is_isolated_per_proxy_manager_instance() {
        let dir = temp_dir("gate_ns");
        let store = std::sync::Arc::new(
            Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap(),
        );
        let pm_a = ProxyManager::new(store.clone());
        let pm_b = ProxyManager::new(store.clone());
        let cat = CategoryType::ClaudeCli;

        let key_a = pm_a.gate_key(cat);
        let key_b = pm_b.gate_key(cat);
        assert_ne!(key_a, key_b, "不同实例的同一分类必须是不同的键");

        arm_all_failed_gate(&key_a, None);
        assert!(all_failed_gate_remaining(&key_a).is_some());
        assert!(
            all_failed_gate_remaining(&key_b).is_none(),
            "一个代理实例的「全部 Key 失败」不得短路另一个实例的同名分类"
        );

        clear_all_failed_gate(&key_a);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 启动代理必须解除该分类残留的短路窗口。
    ///
    /// 产品缺陷回归：窗口原本只有「一次成功转发」或「自然到期 5s」两条解除路径，
    /// 「停止代理 → 换/修好 Key → 重新启动」这个用户已介入的明确信号被忽略，
    /// 于是刚点启动后最长 5s 内的请求仍被挡成失败（用户视角：「刚启动却说全部 Key 不可用」）。
    #[tokio::test]
    async fn start_clears_leftover_short_circuit_gate() {
        let dir = temp_dir("gate_start");
        let store = std::sync::Arc::new(
            Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap(),
        );
        let cat = CategoryType::ClaudeDesktop;
        let pm = ProxyManager::new(store.clone());
        let gate_key = pm.gate_key(cat);

        arm_all_failed_gate(&gate_key, None);
        assert!(
            all_failed_gate_remaining(&gate_key).is_some(),
            "前置条件：窗口已武装"
        );

        let _port = pm.start(cat).await.unwrap();
        assert!(
            all_failed_gate_remaining(&gate_key).is_none(),
            "启动代理应解除残留短路窗口，否则刚启动就把请求挡成失败"
        );

        // stop 也清一次（语义更干净，且 stop→start 路径不依赖 start 侧清理）。
        arm_all_failed_gate(&gate_key, None);
        pm.stop(cat);
        assert!(
            all_failed_gate_remaining(&gate_key).is_none(),
            "停止代理也应清掉短路窗口"
        );

        std::fs::remove_dir_all(&dir).ok();
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

    /// 造一个带 contextWindow 的模型条目。
    fn model_ctx(name: &str, ctx: Option<u32>) -> crate::model::ModelInfo {
        crate::model::ModelInfo {
            real_name: name.into(),
            source: "manual".into(),
            fetched_at: None,
            context_window: ctx,
        }
    }

    #[test]
    fn beta_header_adds_1m_context_for_million_window_models() {
        // 需求（用户提出）：Claude Code / CLI 也要能用 1M 上下文。
        // 判据来源：claude.exe v2.1.219 的 beta 注册表 long_context → context-1m-2025-08-07。
        //
        // 为什么必须由代理代劳：客户端只对**它认识的**模型名发这个头，而经 SynaRoute 路由时
        // 它看到的是我们的对外名，认不出来 → 不发 → 用户配了 1M 窗口也完全不生效。
        let mut k = key("a", 0, "https://x");
        k.models = vec![model_ctx("claude-opus-4-8-1m", Some(1_000_000))];

        // ① 下游没带 beta 头 → 我们补上
        assert_eq!(
            effective_beta_header(&[], &k, "claude-opus-4-8-1m").as_deref(),
            Some(BETA_CONTEXT_1M)
        );

        // ② 下游带了其它特性 → **追加**，绝不覆盖
        //    （claude-code-20250219 是中转商识别真实 CC 客户端的依据，覆盖会被拒）
        let fwd = vec![(
            "anthropic-beta".to_string(),
            "claude-code-20250219,interleaved-thinking-2025-05-14".to_string(),
        )];
        let got = effective_beta_header(&fwd, &k, "claude-opus-4-8-1m").unwrap();
        assert!(got.contains("claude-code-20250219"), "原特性必须保留：{got}");
        assert!(got.contains("interleaved-thinking-2025-05-14"), "{got}");
        assert!(got.contains(BETA_CONTEXT_1M), "{got}");

        // ③ 客户端已经带了 1M → 不重复追加
        let fwd = vec![("anthropic-beta".to_string(), BETA_CONTEXT_1M.to_string())];
        let got = effective_beta_header(&fwd, &k, "claude-opus-4-8-1m").unwrap();
        assert_eq!(
            got.matches(BETA_CONTEXT_1M).count(),
            1,
            "同一特性不得出现两次：{got}"
        );
    }

    #[test]
    fn beta_header_is_not_added_without_evidence() {
        // 反例三条：无窗口数据 / 窗口不足 1M / 非 Anthropic 上游 —— 都不得断言 1M。
        // 「无依据不断言」与桌面端 supports1m 的口径一致：凭空声称 1M 会让上游直接 400。
        let mut k = key("a", 0, "https://x");

        // 无 contextWindow 数据
        k.models = vec![model_ctx("m-unknown", None)];
        assert_eq!(effective_beta_header(&[], &k, "m-unknown"), None);

        // 窗口不足 1M
        k.models = vec![model_ctx("m-200k", Some(200_000))];
        assert_eq!(effective_beta_header(&[], &k, "m-200k"), None);

        // 模型不在列表里（查不到窗口）
        assert_eq!(effective_beta_header(&[], &k, "not-listed"), None);

        // 非 Anthropic 上游：beta 头是 Anthropic 特有的，不该加
        k.protocol = Protocol::OpenaiChat;
        k.models = vec![model_ctx("gpt-1m", Some(1_000_000))];
        assert_eq!(effective_beta_header(&[], &k, "gpt-1m"), None);

        // 但下游原有的 beta 头在任何情况下都要**原样透传**（中转商靠它识别客户端）
        let fwd = vec![("anthropic-beta".to_string(), "claude-code-20250219".to_string())];
        assert_eq!(
            effective_beta_header(&fwd, &k, "gpt-1m").as_deref(),
            Some("claude-code-20250219"),
            "不加 1M 时也不能把原头吞掉"
        );
    }

    #[test]
    fn beta_header_threshold_is_exactly_one_million() {
        let mut k = key("a", 0, "https://x");
        // 边界：恰好 1M 要算支持（阈值是「≥」）
        k.models = vec![model_ctx("exact", Some(crate::model::ONE_MILLION_CONTEXT))];
        assert_eq!(
            effective_beta_header(&[], &k, "exact").as_deref(),
            Some(BETA_CONTEXT_1M),
            "恰好 1M 应视为支持"
        );
        // 差 1 token 就不算
        k.models = vec![model_ctx("almost", Some(crate::model::ONE_MILLION_CONTEXT - 1))];
        assert_eq!(effective_beta_header(&[], &k, "almost"), None);
    }

    /// 起一个返回固定 status + body 的 mock 上游，返回其 base_url。
    async fn spawn_mock(status: u16, body: &'static str) -> String {
        spawn_mock_with_headers(status, body, &[]).await
    }

    /// 同 [`spawn_mock`]，但额外附带响应头（用于验证 Retry-After 透传）。
    async fn spawn_mock_with_headers(
        status: u16,
        body: &'static str,
        headers: &[(&'static str, &'static str)],
    ) -> String {
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let headers: Vec<(&'static str, &'static str)> = headers.to_vec();
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else { break };
                let io = TokioIo::new(stream);
                let headers = headers.clone();
                tokio::spawn(async move {
                    let svc = service_fn(move |_req: Request<Incoming>| {
                        let headers = headers.clone();
                        async move {
                            let mut builder = Response::builder()
                                .status(status)
                                .header("content-type", "application/json");
                            for (h, v) in &headers {
                                builder = builder.header(*h, *v);
                            }
                            let resp = builder.body(full_body(Bytes::from(body))).unwrap();
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

    /// 主口令锁定态：必须在**进入故障转移之前**短路，且不得污染健康状态。
    ///
    /// 若不短路，会逐个候选去试、每个都因取不到密钥失败 → `record_live_failure` 把整池好 Key
    /// 刷成熔断 + 武装 529 短路窗口。用户解锁后反倒要等熔断过去才恢复，而真实原因只是没解锁。
    #[tokio::test]
    async fn locked_vault_short_circuits_before_touching_upstream_or_breaker() {
        // mock 上游返回 200：若真被打到，测试会拿到 200 而非 503，从而暴露「没短路」。
        let upstream = spawn_mock(
            200,
            r#"{"id":"m","type":"message","role":"assistant","content":[{"type":"text","text":"ok"}]}"#,
        )
        .await;
        let dir = temp_dir("locked");
        let store = std::sync::Arc::new(
            Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap(),
        );
        store.upsert_key(key("k1", 0, &upstream)).unwrap();
        store.secrets.write().set("k1", "sk-x").unwrap();
        // 启用主口令 → 已解锁态；随后显式上锁，模拟「新进程启动但用户还没输口令」。
        store.secrets.write().enable_master_password("master-pw").unwrap();
        store.secrets.write().lock();
        assert!(store.secrets.read().is_locked(), "前置条件：处于锁定态");

        let pm = ProxyManager::new(store.clone());
        let port = pm.start(CategoryType::ClaudeCli).await.unwrap();
        let resp = reqwest::Client::new()
            .post(format!("http://127.0.0.1:{port}/v1/messages"))
            .json(&json!({"model":"m","max_tokens":10,"messages":[{"role":"user","content":"hi"}]}))
            .send()
            .await
            .unwrap();

        // 503 而非 529：529 会让客户端自动退避重试，但这里需要**人来输口令**，自动重试等不到结果。
        assert_eq!(
            resp.status().as_u16(),
            503,
            "锁定态应回 503（需人工介入），而非 529（那会让客户端无意义地自动重试）"
        );
        let body: serde_json::Value = resp.json().await.unwrap();
        let msg = body["error"]["message"].as_str().unwrap_or_default();
        assert!(msg.contains("主口令"), "要点明是主口令未解锁: {msg}");
        assert!(msg.contains("解锁"), "要给出可操作指引: {msg}");

        // 关键：健康状态没被污染——解锁后应立即可用，不必等熔断窗口过去。
        let k = store.get_key("k1").unwrap();
        assert_eq!(k.health.fail_count, 0, "锁定态不得把好 Key 刷成失败");
        assert!(k.health.breaker_until.is_none(), "锁定态不得触发熔断");

        pm.stop(CategoryType::ClaudeCli);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 端到端：全部候选失败 → 529 overloaded_error + Retry-After。
    ///
    /// 状态码判据来自官方 gateway 规范（claude.exe v2.1.219 内嵌原文）：529 是「上游暂时无产能」
    /// 的专用码，客户端 SDK 见到它走退避重试。此前回 502（→ `api_error`），SDK 的处理不确定，
    /// 实测表现为立刻重发、把单次故障放大成持续轮询。
    #[tokio::test]
    async fn proxy_all_fail_returns_529_overloaded_with_retry_after() {
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
        assert_eq!(
            resp.status().as_u16(),
            529,
            "全部失败应回 529 overloaded_error（规范要求，客户端据此退避）"
        );
        let retry_after = resp
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<i64>().ok())
            .expect("529 必须带可解析的 Retry-After");
        assert!(retry_after >= 1, "Retry-After 不得为 0（等于不退避）");
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["error"]["type"], "overloaded_error");

        // 窗口内的短路响应同样必须是 529 + Retry-After（此前是 502）。
        let again = reqwest::Client::new()
            .post(format!("http://127.0.0.1:{port}/v1/messages"))
            .json(&json!({"model":"m","max_tokens":10,"messages":[{"role":"user","content":"hi"}]}))
            .send()
            .await
            .unwrap();
        assert_eq!(again.status().as_u16(), 529, "短路窗口内也应回 529");
        assert!(
            again.headers().contains_key("retry-after"),
            "短路响应也必须带 Retry-After"
        );

        pm.stop(CategoryType::ClaudeCli);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 全部候选都是**硬错误**（401 鉴权失败）时不得回 529，也不得武装短路窗口。
    ///
    /// 529 `overloaded_error` 的语义是「上游暂时无产能，退避后重试」，客户端 SDK 据此
    /// 持续退避重试。可 401 是密钥填错——等到宇宙尽头也不会好转：
    /// - 界面上呈现「过载」，与真实根因（配置错误）方向相反，用户会去查上游而不是查密钥；
    /// - 短路窗口一到期就放行一个真实请求再撞一次 401，无谓消耗；
    /// - 带 `Retry-After` 等于明示「稍后会好」，是错误承诺。
    ///
    /// 故硬错误原样回该状态码、不带 `Retry-After`、不武装短路窗口，让客户端第一次就拿到
    /// 确定的失败。与上面那条 529 用例对照着看：区别只在最后一个候选给的是 401 还是 5xx。
    #[tokio::test]
    async fn all_hard_errors_return_status_verbatim_without_retry_after_or_gate() {
        let bad1 = spawn_mock(401, "invalid api key").await;
        let bad2 = spawn_mock(401, "invalid api key").await;
        let dir = temp_dir("hardfail");
        let store = std::sync::Arc::new(
            Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap(),
        );
        // 用 ClaudeDesktop 分类：与其它用例的 gate key 天然隔开
        let mut k1 = key("k1", 0, &bad1);
        k1.category_id = CategoryType::ClaudeDesktop;
        let mut k2 = key("k2", 1, &bad2);
        k2.category_id = CategoryType::ClaudeDesktop;
        store.upsert_key(k1).unwrap();
        store.upsert_key(k2).unwrap();
        store.secrets.write().set("k1", "x").unwrap();
        store.secrets.write().set("k2", "y").unwrap();
        let pm = ProxyManager::new(store.clone());
        let port = pm.start(CategoryType::ClaudeDesktop).await.unwrap();

        let resp = reqwest::Client::new()
            .post(format!("http://127.0.0.1:{port}/v1/messages"))
            .json(&json!({"model":"m","max_tokens":10,"messages":[{"role":"user","content":"hi"}]}))
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status().as_u16(),
            401,
            "硬错误必须原样回传，不能包装成 529「过载」"
        );
        assert!(
            !resp.headers().contains_key("retry-after"),
            "硬错误不该给 Retry-After —— 那是「稍后会好」的错误承诺"
        );
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_ne!(body["error"]["type"], "overloaded_error", "错误分类不能是过载");

        // 短路窗口不该被武装：第二次请求应当再次真打上游、仍得 401（而非窗口短路的 529）。
        let again = reqwest::Client::new()
            .post(format!("http://127.0.0.1:{port}/v1/messages"))
            .json(&json!({"model":"m","max_tokens":10,"messages":[{"role":"user","content":"hi"}]}))
            .send()
            .await
            .unwrap();
        assert_eq!(
            again.status().as_u16(),
            401,
            "硬错误不武装短路窗口，第二次仍应是 401 而不是短路的 529"
        );

        pm.stop(CategoryType::ClaudeDesktop);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 上游 429 的 `Retry-After` 必须透传给下游，且取各候选中的**最短**者。
    ///
    /// 官方 gateway 规范明确要求限流响应带 `Retry-After`。丢掉它，下游客户端只能按自己的固定
    /// 节奏重发，仍会撞在上游的限流窗口里（这是「一直轮询」现象的一环）。
    ///
    /// **为什么取最短而非最长**：下游重试面对的是整池，只要有一个候选恢复就该放行。若取最长，
    /// 一个撞上按天配额的候选（`Retry-After: 3600`）会把整个分类停摆到上限 300s，而另一个
    /// 只是瞬时抖动的候选 1s 后就能服务 —— 那正是本项目最痛的「一个 Key 限流拖垮所有 Key」。
    #[tokio::test]
    async fn upstream_retry_after_is_propagated_downstream() {
        // 两个候选都限流：一个说 7s，一个说 42s。取较短者，让下游在 7s 后就能来探路。
        let limited_short = spawn_mock_with_headers(429, "slow down", &[("retry-after", "7")]).await;
        let limited_long = spawn_mock_with_headers(429, "slow down", &[("retry-after", "42")]).await;
        let dir = temp_dir("retry_after");
        let store = std::sync::Arc::new(
            Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap(),
        );
        // 分类用 Codex：本用例走 Responses 之外的 Anthropic 端点也无妨，这里只关心 Retry-After
        // 是否被采集并透传（gate 已按代理实例隔离，用哪个分类都不会污染别的用例）。
        let mut k1 = key("k1", 0, &limited_short);
        k1.category_id = CategoryType::Codex;
        let mut k2 = key("k2", 1, &limited_long);
        k2.category_id = CategoryType::Codex;
        store.upsert_key(k1).unwrap();
        store.upsert_key(k2).unwrap();
        store.secrets.write().set("k1", "x").unwrap();
        store.secrets.write().set("k2", "y").unwrap();
        let pm = ProxyManager::new(store.clone());
        let port = pm.start(CategoryType::Codex).await.unwrap();
        let resp = reqwest::Client::new()
            .post(format!("http://127.0.0.1:{port}/v1/messages"))
            .json(&json!({"model":"m","max_tokens":10,"messages":[{"role":"user","content":"hi"}]}))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 529);
        let retry_after = resp
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<i64>().ok())
            .expect("上游给了 Retry-After，下游必须收到");
        assert_eq!(
            retry_after, 7,
            "应透传候选中最短的 Retry-After：只要有一个候选先恢复就该放下游来探路，\
             取最长会让一个撞配额的 Key 把整池停摆"
        );

        pm.stop(CategoryType::Codex);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn parse_retry_after_accepts_seconds_and_http_date() {
        use reqwest::header::{HeaderMap, HeaderValue, RETRY_AFTER};
        let mut h = HeaderMap::new();

        // 形态一：delta-seconds
        h.insert(RETRY_AFTER, HeaderValue::from_static("30"));
        assert_eq!(parse_retry_after(&h), Some(30));

        // 形态二：HTTP-date（RFC 7231 允许）。取相对秒数，允许 ±2s 抖动。
        let future = chrono::Utc::now() + chrono::Duration::seconds(60);
        let date = future.format("%a, %d %b %Y %H:%M:%S GMT").to_string();
        h.insert(RETRY_AFTER, HeaderValue::from_str(&date).unwrap());
        let got = parse_retry_after(&h).expect("HTTP-date 形态应能解析");
        assert!((58..=62).contains(&got), "HTTP-date 应换算成约 60s，实际 {got}");

        // 已过期的 HTTP-date → 夹到 0（不产生负数退避）。
        let past = chrono::Utc::now() - chrono::Duration::seconds(120);
        h.insert(
            RETRY_AFTER,
            HeaderValue::from_str(&past.format("%a, %d %b %Y %H:%M:%S GMT").to_string()).unwrap(),
        );
        assert_eq!(parse_retry_after(&h), Some(0));

        // 上限夹取：上游给几小时不能照抄（我们的 Key 可能几秒后就恢复）。
        h.insert(RETRY_AFTER, HeaderValue::from_static("99999"));
        assert_eq!(parse_retry_after(&h), Some(MAX_RETRY_AFTER_SECS));

        // 无头 / 垃圾值 → None，由调用方回退自己的退避口径。
        assert_eq!(parse_retry_after(&HeaderMap::new()), None);
        h.insert(RETRY_AFTER, HeaderValue::from_static("soon-ish"));
        assert_eq!(parse_retry_after(&h), None);
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

    /// `GET /v1/models/{id}`（Anthropic SDK 的 models.retrieve）必须返回**单个模型对象**。
    /// 此前该路径落进列表分支，客户端拿到 `{"data":[…]}` 解不出 id。
    #[tokio::test]
    async fn proxy_retrieve_single_model_returns_object_not_list() {
        let dir = temp_dir("retrievemodel");
        let store = std::sync::Arc::new(
            Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap(),
        );
        let mut a = key("a", 0, "http://x");
        a.mappings = vec![mapping("opus-4-8", "glm-5.2")];
        store.upsert_key(a).unwrap();
        store.secrets.write().set("a", "x").unwrap();
        let pm = ProxyManager::new(store.clone());
        let port = pm.start(CategoryType::ClaudeCli).await.unwrap();
        let cli = reqwest::Client::new();

        // 用我们对外暴露的 gateway 别名检索（CLI 从 /v1/models 拿到的就是这个 id）。
        let resp = cli
            .get(format!("http://127.0.0.1:{port}/v1/models/claude-synaroute-opus-4-8"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert!(body.get("data").is_none(), "单模型检索不得返回列表形状: {body}");
        assert_eq!(body["id"], "claude-synaroute-opus-4-8");
        assert_eq!(body["display_name"], "opus-4-8");
        assert_eq!(body["type"], "model");

        // 用真实对外名也能检索到（两种写法都接）。
        let resp2 = cli
            .get(format!("http://127.0.0.1:{port}/v1/models/opus-4-8"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp2.status().as_u16(), 200);

        // 未知模型 → 404 + 标准 Anthropic 错误信封，而不是把请求打去上游。
        let miss = cli
            .get(format!("http://127.0.0.1:{port}/v1/models/does-not-exist"))
            .send()
            .await
            .unwrap();
        assert_eq!(miss.status().as_u16(), 404);
        let mb: serde_json::Value = miss.json().await.unwrap();
        assert_eq!(mb["type"], "error");
        assert_eq!(mb["error"]["type"], "not_found_error");

        pm.stop(CategoryType::ClaudeCli);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 官方 gateway 协议的非推理端点必须本地应答，**绝不能** POST 给上游 AI 中转商。
    /// 判据：claude.exe v2.1.219 内嵌的 llm-gateway-protocol 规范。
    #[tokio::test]
    async fn proxy_handles_managed_settings_and_otlp_locally() {
        let dir = temp_dir("sideendpoints");
        let store = std::sync::Arc::new(
            Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap(),
        );
        // 故意配一个**指向黑洞的** Key：若这些端点被误当成补全请求转发，
        // 就会去连 base_url 而后失败——用 404/200 的即时应答证明它们没走转发路径。
        store.upsert_key(key("a", 0, "http://127.0.0.1:9")).unwrap();
        store.secrets.write().set("a", "x").unwrap();
        let pm = ProxyManager::new(store.clone());
        let port = pm.start(CategoryType::ClaudeCli).await.unwrap();
        let cli = reqwest::Client::new();

        // GET /managed/settings → 404（规范：404 = 无策略，与 200 {} 语义不同）
        let ms = cli
            .get(format!("http://127.0.0.1:{port}/managed/settings"))
            .send()
            .await
            .unwrap();
        assert_eq!(ms.status().as_u16(), 404, "无策略必须是 404 而非 200 {{}}");
        let mb: serde_json::Value = ms.json().await.unwrap();
        assert_eq!(mb["error"]["type"], "not_found_error");

        // POST /v1/{metrics,logs,traces} → 200（规范：404 会让 exporter 每次 flush 记错误）
        for sig in ["metrics", "logs", "traces"] {
            let r = cli
                .post(format!("http://127.0.0.1:{port}/v1/{sig}"))
                .header("content-type", "application/x-protobuf")
                .body(vec![0u8, 1, 2, 3])
                .send()
                .await
                .unwrap();
            assert_eq!(r.status().as_u16(), 200, "/v1/{sig} 必须回 200（丢弃但不报错）");
        }

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
