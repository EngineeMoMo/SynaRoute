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
#[path = "lan_guard.rs"] pub(crate) mod lan_guard; // 入站鉴权；来由见该文件模块注释
pub(crate) type ResBody = BoxBody<Bytes, std::io::Error>;

/// 把完整字节体装箱为 ResBody（Full 的错误类型是 Infallible，用 `match` 消解）。
pub(crate) fn full_body(body: Bytes) -> ResBody {
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
/// 给上游请求装齐所有请求头：透传下游头 → `anthropic-beta` → 鉴权头 → 版本头（P2-2）。
///
/// **为什么必须抽出来**：这段逻辑原先在 `try_stream_to_key` 与 `forward_to_key` 里**逐字重复**
/// （去空行去注释后 38 行完全一致），且鉴权头另有第三份实现在 `upstream::apply_auth`。
/// 三份已经分叉：proxy 的两处带 `anthropic-version`，而 `apply_auth` 不带、改由它的三个调用点
/// 各自补。任何转发前置语义变更（新增鉴权形态、改 beta 头推导、启用 `ProviderKey.headers_json`
/// 这个自定义请求头预留位）都要同时改 2~5 处，漏一处即「非流式生效、流式不生效」这类
/// 最难复现的半残缺陷——`anthropic-version` 的现状就是该风险已发生过一次的证据。
///
/// 顺序不能变：
/// 1. 先透传下游客户端头（UA / x-app / x-stainless-*），让中转商识别为真实客户端；
/// 2. `anthropic-beta` 单独算（1M 上下文要按**落点模型**追加特性），故上一步跳过原值、这里统一设；
/// 3. **鉴权头最后设**，确保覆盖掉下游可能带来的同名头（鉴权必须用本 Key 的密钥）。
///
/// ⚠️ **超时不在这里设**：流式与非流式的超时语义刻意不同（流式只约束探头阶段、不掐已建立的
/// SSE 流），必须留在各自调用点。见 `try_stream_to_key` 与 `forward_to_key` 的相应注释。
fn apply_upstream_headers(
    mut rb: reqwest::RequestBuilder,
    key: &ProviderKey,
    secret: &str,
    fwd_headers: &[(String, String)],
    real_model: &str,
) -> reqwest::RequestBuilder {
    let beta = effective_beta_header(fwd_headers, key, real_model);
    for (h, v) in fwd_headers {
        if h == "anthropic-beta" {
            continue;
        }
        rb = rb.header(h, v);
    }
    if let Some(b) = &beta {
        rb = rb.header("anthropic-beta", b.as_str());
    }
    // 鉴权与版本头都走 Protocol 的**穷举**能力方法：加第 4 种协议时编译器会点出这里要改，
    // 而不是静默按「非 Anthropic 即 OpenAI」发错误的头。
    let scheme = key.protocol.auth_scheme();
    rb = rb.header(scheme.header_name(), scheme.header_value(secret));
    if let Some((h, v)) = key.protocol.version_header() {
        rb = rb.header(h, v);
    }
    // **显式要求不压缩**。转发路径是字节透明的：拿到上游 body 原样转给下游，既不解压
    // （shared_client 关掉了自动解压，否则会破坏 SSE 分块与 content-length 语义），
    // 也不透传上游的 content-encoding。于是一旦上游返回压缩体，下游拿到的就是一堆
    // 压缩字节且无从得知 —— 表现为「乱码」。
    //
    // 而我们又刻意剥掉了下游客户端的 accept-encoding（见 is_stripped_header），
    // 按 RFC 7231「缺 Accept-Encoding 视为任何编码都可接受」，网关**完全合法地**可以压缩。
    // 补一条 `identity` 把这个口子堵在源头：对守规范的上游即「别压」。
    // 放在最后设置，覆盖 fwd_headers 里可能漏过来的同名头。
    rb = rb.header("accept-encoding", "identity");
    rb
}

/// 故障转移预算的最小切片：剩余预算低于此值时不再开始新的候选尝试。
///
/// 为什么要有下限而不是「有多少用多少」：剩下 200ms 时去打一次上游几乎必然超时，
/// 既白烧一次额度、又把总耗时再拖长 200ms。5s 是「够一次快速失败（连接被拒/401 立即返回）」
/// 与「不至于误杀一次正常应答」之间的折中。
const MIN_ATTEMPT_SLICE: std::time::Duration = std::time::Duration::from_secs(5);

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
///
/// ## 并发安全（CAS 语义）
///
/// 只在以下情况更新窗口：
/// 1. 窗口不存在（首次武装）
/// 2. 窗口已过期（自然到期后重新武装）
/// 3. 新的 `retry_at_ms` 更晚（取更保守的退避时间，避免早到的重试仍然失败）
///
/// **不**覆盖仍有效且更严格的窗口，防止并发场景下后完成的请求用更短的退避覆盖先完成的。
fn arm_all_failed_gate(gate_key: &str, retry_after_secs: Option<i64>) {
    let now = chrono::Utc::now().timestamp_millis();
    let new_until = now + ALL_FAILED_SHORT_CIRCUIT_MS;
    let new_retry_at = retry_after_secs.map(|s| now + s.saturating_mul(1000));

    let mut gate = all_failed_gate().lock();

    // CAS 语义：只在应该更新时才更新
    let should_update = match gate.get(gate_key) {
        None => true,  // 窗口不存在，首次武装
        Some(existing) if existing.until_ms <= now => true,  // 窗口已过期
        Some(existing) => {
            // 窗口仍有效：只在新值更保守时更新（取更晚的 retry_at）
            match (existing.retry_at_ms, new_retry_at) {
                (Some(old_retry), Some(new_retry)) => new_retry > old_retry,
                (None, Some(_)) => true,   // 旧值无 retry_at，新值有 → 更新
                _ => false,  // 其他情况保留现有窗口
            }
        }
    };

    if should_update {
        gate.insert(
            gate_key.to_string(),
            GateEntry {
                until_ms: new_until,
                retry_at_ms: new_retry_at,
            },
        );
    }
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
    /// key 用 CategoryType 而非字符串（P2-8）：字符串键与枚举并存是两套真相，
    /// 拼错一个字母就是「静默启动到另一个分类」而非编译失败。
    running: Mutex<HashMap<CategoryType, RunningProxy>>,
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
        self.running.lock().get(&category).map(|p| p.port)
    }

    pub fn is_running(&self, category: CategoryType) -> bool {
        self.running.lock().contains_key(&category)
    }

    /// 启动某分类的代理，返回监听端口。
    pub async fn start(&self, category: CategoryType) -> AppResult<u16> {
        if let Some(p) = self.port_of(category) {
            return Ok(p);
        }
        let settings = self.store.get_settings();
        let lan = settings.lan_exposure;
        let host = if lan { [0, 0, 0, 0] } else { [127, 0, 0, 1] };
        if lan { lan_guard::ensure_token(&self.store); } // 先备好令牌+落事件，否则用户看不到它

        // 粘滞固定端口：首选端口取配置里该分类的值（缺省用分类表里的 default_port）。
        // 从首选端口起在 [preferred, preferred+FALLBACK_RANGE] 内逐个尝试，绑上即用；
        // 全被占才报错提示改端口。避免早期「bind 0 随机端口」导致每次重启端口漂移、
        // 客户端追不上（config 只在客户端启动时读一次）。
        let preferred = settings
            .proxy_ports
            .get(&category)
            .copied()
            .unwrap_or(category.meta().default_port);
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
        if settings.proxy_ports.get(&category).copied() != Some(port) {
            let _ = self.store.set_proxy_port(category, port);
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
                        // peer 是局域网鉴权的唯一依据，**别再丢回 `_`** —— 丢了就只能放行所有人。
                        let Ok((stream, peer)) = accepted else { break };
                        let io = TokioIo::new(stream);
                        let store = store.clone();
                        let gate_key = gate_key.clone();
                        let mut conn_shutdown = shutdown_rx.clone();
                        tokio::spawn(async move {
                            let svc = lan_guard::guarded(store, category, gate_key, peer);
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
        if let Some(existing) = running.get(&category) {
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
            category,
            RunningProxy { port, handle, shutdown: shutdown_tx },
        );
        // 显式放锁再 emit。这个 guard 是在 318 行取的、本来会活到函数末尾 ——
        // 「emit 前放光锁」的纪律见 events::emit 的文档。
        drop(running);
        crate::events::emit(crate::events::Topic::Proxy, Some(category));
        Ok(port)
    }

    pub fn stop(&self, category: CategoryType) {
        // 必须先把 guard 取出来再用：写成 `if let Some(p) = self.running.lock().remove(..)`
        // 的话，那个临时 MutexGuard 会活到整个 if-let 语句结束，emit 就落在持锁期间了。
        // 泵的设计让持锁 emit 也不至于死锁，但「emit 前放光锁」这条纪律要守住 ——
        // 它防的是将来有人把泵简化掉、换成直接 app.emit 时悄悄埋雷。
        let was_running = {
            let mut running = self.running.lock();
            match running.remove(&category) {
                Some(p) => {
                    // 先广播关闭信号（断开已建立的 keep-alive 连接），再 abort accept 循环。
                    let _ = p.shutdown.send(true);
                    p.handle.abort();
                    true
                }
                None => false,
            }
        };
        // 停止时也清一次：代理已不在服务，残留的短路窗口对下一次启动毫无意义（语义更干净，
        // 也让「stop → 立刻 start」这条路径不依赖 start 侧的清理）。
        clear_all_failed_gate(&self.gate_key(category));
        if was_running {
            crate::events::emit(crate::events::Topic::Proxy, Some(category));
        }
    }

    /// 停掉**所有**分类的代理，返回实际停掉的分类数。退出应用时用。
    ///
    /// 与逐个 `stop()` 的区别，是刻意为「进程退出」这个场景设计的：
    /// - **广播 shutdown 信号**再 abort —— 这一步是「优雅」的实质：已建立的连接会收到
    ///   watch 变更、走 `select` 的关闭臂，让 hyper 正常收尾（客户端看到的是干净的连接结束，
    ///   而非进程被杀时 socket 直接 RST 的「连接被重置」）。accept 循环随后 abort、端口释放。
    /// - **不 emit 事件**：退出时前端窗口正在拆除，事件泵的接收端可能已经没了，emit 无意义；
    ///   而逐个 `stop()` 会 emit（那是运行期用户操作，UI 还在）。
    /// - **不还原客户端配置**：那是 FR-029 的取舍 —— 退出时快照「哪些在跑」，下次启动自动恢复；
    ///   退出即还原会让下次启动多绕一圈重写，且中间有窗口期客户端指向死端口。
    ///   调用方（退出处理块）必须**先做运行态快照、再调本函数**，否则快照读到的是空集。
    pub fn stop_all(&self) -> usize {
        // 一次性取走全部句柄，锁内只做 remove、不做 I/O。
        let handles: Vec<RunningProxy> = {
            let mut running = self.running.lock();
            running.drain().map(|(_, p)| p).collect()
        };
        let n = handles.len();
        for p in &handles {
            // 先广播「关闭」，让在途连接走干净的收尾路径。
            let _ = p.shutdown.send(true);
        }
        for p in handles {
            p.handle.abort();
        }
        n
    }
}

/// 处理一次下游工具请求：故障转移路由。
///
/// 本函数只做一件事：调 [`handle_request_inner`] 拿响应，再在**唯一出口**挂上
/// `X-SynaRoute-*` 诊断头（见 [`crate::route_meta`]）。
///
/// 为什么要拆这一层，而不是在 inner 里各个 `return` 前分别挂头：inner 有 8 个出口
/// （模型发现、单模型检索、gateway 侧端点、读体失败、短路窗口、无候选、成功×2、全失败），
/// 「记得在每个出口调一次」是**必然会漏**的纪律，而漏掉的表现是静默的
/// —— 没人会因为「少了个响应头」提 bug，只会在下次排障时发现某类请求查不到路由信息。
/// 收成一个出口后，漏掉这件事在结构上做不到：inner 返回什么，都会经过这里。
///
/// `gate_key`：本次请求所属「代理实例 + 分类」的短路窗口键（见 [`all_failed_gate`]）。
pub(crate) async fn handle_request(
    store: Arc<Store>,
    category: CategoryType,
    gate_key: String,
    req: Request<Incoming>,
) -> Result<Response<ResBody>, hyper::Error> {
    // 请求 id 在这里生成（而不是 inner 里）：inner 的每个早退出口都要用它，
    // 且它必须与最终挂上的头是同一个值。
    let mut meta = crate::route_meta::RouteMeta {
        request_id: uuid::Uuid::new_v4().to_string(),
        ..Default::default()
    };
    let out = handle_request_inner(store, category, gate_key, req, &mut meta).await;
    out.map(|mut resp| {
        crate::route_meta::attach(&mut resp, &meta);
        resp
    })
}

/// 转发主体。**不要直接调它** —— 走 [`handle_request`]，否则响应上不会带诊断头。
///
/// `meta`：出参。沿路把「命中哪条 Key / 实际打的模型 / 试了几次 / 耗时 / 上游状态码」
/// 填进去，由 [`handle_request`] 在唯一出口转成响应头。填不满没关系（早退路径本来就没有
/// 这些信息），`build_headers` 会省略空字段。
async fn handle_request_inner(
    store: Arc<Store>,
    category: CategoryType,
    gate_key: String,
    req: Request<Incoming>,
    meta: &mut crate::route_meta::RouteMeta,
) -> Result<Response<ResBody>, hyper::Error> {
    // 本地副本：`log_success` / `log_request` 两个闭包要按值捕获它写进 `RequestTrace`，
    // 而 `meta` 全程要可变借用（沿路填字段）。借一次 String 比让闭包持有 `&mut meta` 简单得多。
    let request_id = meta.request_id.clone();
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
    let path_only = req.uri().path().to_string();
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
    if let Some(resp) = handle_gateway_side_endpoints(req.method(), &path_only) {
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

    // `mut`：最后一个候选会用 `mem::take` 把它移交给转发函数（P2-5 零拷贝），
    // 之前的候选只借用。取走后本地变为 `Value::Null`，而循环里此后不再读它——
    // 唯一的读取点就是构造 `body_for_attempt`，且那之后立刻 break/return。
    let mut req_json: Value = serde_json::from_slice(&body_bytes).unwrap_or(Value::Null);
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
        .active_model_of(category)
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
    if is_contentless_probe(&req_json) {
        return Ok(error_resp(
            StatusCode::BAD_REQUEST,
            "空探测请求（无消息内容且未指定模型）：已在代理侧拒绝，未转发上游",
        ));
    }

    // count_tokens 本地估算：Claude 桌面端每次对话前都会发 POST /v1/messages/count_tokens
    // 估算输入 token 数，而绝大多数中转站不实现该端点 → 大量 404 日志 + 有时触发 429 限流。
    // 使用本地 estimate_tokens 按消息内容估算，直接返回标准格式，不转发上游。
    //
    // 估算误差在 10%~20% 是可接受的——该端点的用途是让客户端决定要不要截断上下文，
    // 而不是精确计费；Anthropic 文档也说其官方实现本身有误差（使用 tiktoken-style 估算）。
    // 本地估算不消耗上游额度、不产生 404/429，用户体验大幅改善。
    if path_only == "/v1/messages/count_tokens"
        || path_only.starts_with("/v1/messages/count_tokens?")
    {
        let input_tokens = estimate_count_tokens_local(&req_json);
        let resp_body = serde_json::json!({ "input_tokens": input_tokens });
        return Ok(json_resp(
            StatusCode::OK,
            Bytes::from(serde_json::to_vec(&resp_body).unwrap_or_default()),
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
    // 走 `candidates_for`：一次读锁内筛选排序、只克隆入选者（旧路径
    // `enabled_keys_sorted` + `select_candidates` 要克隆两轮全量启用 Key）。
    let (candidates, used_breaker_fallback) = store.candidates_for(category, &requested_model);

    if candidates.is_empty() {
        return Ok(error_resp(
            StatusCode::SERVICE_UNAVAILABLE,
            "无可用 Key（全部停用或明确不可用）",
        ));
    }
    // 「全部 Key 失败」短路：上一次已确认全部候选都打不通，且仍在短路窗口内 →
    // 直接返回失败，不再逐个重打上游。见 ALL_FAILED_SHORT_CIRCUIT_MS 的完整理由。
    // 状态码用 529 overloaded_error（规范要求，客户端据此退避）并带 Retry-After。
    //
    // **必须排在下面那条「所有 Key 均在熔断窗口内」事件之前。** 反过来的话，短路窗口内的
    // 每一个请求都会先记一条「已忽略熔断兜底重试」再从这里直接返回 —— 而那句话描述的动作
    // **压根没发生**（什么都没重试，就在这里断了）。Claude Code / Codex 都会自动重发，
    // 窗口内轻易几十次，于是日志被一串「描述了一件没做的事」的行刷满，
    // 还把真正有用的事件从 MAX_EVENTS 环里挤出去 —— 正是排障最需要那几条的时候。
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

    if used_breaker_fallback {
        // 折叠：同一分类连续出现只记一条带 ×N，别让它自己变成刷屏源
        // （短路窗口刚过期时会放一个请求进来，若它又失败、窗口再武装，这条就周期性复发）。
        store.append_event_collapsible(
            category,
            "failover",
            None,
            "所有 Key 均在熔断窗口内，已忽略熔断兜底重试（无处可切换）",
            None,
            Some(format!("breaker-fallback:{}", category.as_str())),
        );
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
                       was_truncated: Option<bool>,
                       usage: Option<crate::upstream::TokenUsage>| {
        let detail = fmt_success_detail(
            &key.name,
            streaming,
            &fmt_route_model_for_key(key, &requested_model),
            elapsed,
            was_truncated,
            usage.as_ref(),
        );
        // 链路快照只在「调用模型日志」开关开启时产生（正文可达 2×20000 字符）。
        let trace = req_log.then(|| RequestTrace {
            request_id: request_id.clone(),
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
            was_truncated,
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
            request_id: request_id.clone(),
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
            was_truncated: None,
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
    // 最后一次失败是否为**本地配置错误**（`AppError::Invalid`，如缺 maxOutputTokens、
    // 输入占满窗口）。这类错误永不自愈，且**与 Key 凭据无关**：
    //   - 不能记进熔断（record_live_failure）——否则一条凭据完好的 Key 被误判坏掉，
    //     还连累它上面其它可用模型；
    //   - 不能包装成 529「过载重试」——那让客户端持续退避重试而配置永远不好，
    //     还把可行动的引导文案（「去 Key 编辑器填最大单次输出」）藏进「过载」UI；
    //   - 不能武装短路窗口——窗口内正常请求会被无辜挡住。
    // 与本仓契约 all_hard_errors_return_status_verbatim 一致：硬错误原样回、不退避、不熔断。
    let mut config_error = false;
    // **整池**是否出现过「等一等可能会好」的失败（429/408/409/5xx/连接层失败）。
    //
    // 为什么不能只看最后一个候选：`last_status` 被每个候选无条件覆盖，于是
    // 「k1 撞按天配额回 429 + Retry-After: 7；k2 是早已过期的备用 Key 回 401」这种**混合池**
    // 里，尾部只看到 401 → 判硬错误 → 原样回 401 `authentication_error`、**丢掉 k1 给的
    // Retry-After**、**不武装短路窗口**。而真实结论恰恰相反：池子里有一条 Key 只是限流，
    // 7 秒后就能服务；回 401 会让客户端认为「密钥错了」不再重试，用户被引向完全错误的排查方向。
    // 只要池里有一条是临时性的，整轮就该按临时性处置（529 + 最早 Retry-After + 短路窗口）；
    // 硬错误的具体状态码仍在 failover 事件里对用户可见，不丢信息。
    let mut saw_transient = false;
    // 整池里是否出现过「临时性失败、但上游**没给** Retry-After」的候选。
    //
    // 为什么单独记：`retry_after_hint` 的最小值只在**给了头的候选之间**比较，而没给头的
    // 候选压根不参与 —— 于是混合池 [A: 429 `Retry-After: 3600`（夹到 300）, B: 500 无头]
    // 的结论是「等 300 秒」。可 B 的 500 很可能 1 秒后就好了，取最小值那段注释自己写的
    // 判据正是「只要有一个候选恢复就该放行」。同一个误伤（一个撞配额的 Key 拖垮整池），
    // 只是从「取最大值」换成了「唯一给头的那个说话」这条更隐蔽的路径。
    //
    // 处置：把「没给头」当作一个**默认候选**（= 短路窗口长度 5s）一起参与取最小值。
    // 这样两个方向都对：3600 + 无头 → 5s（不再被拖到 300）；1 + 无头 → 1s（不被拖长）。
    // 「未知恢复时间」的正确近似不是「无穷远」也不是「立刻」，而是「按本项目自己的
    // 短路窗口再来探一次」——早到的重试若仍失败，窗口会再武装一次，代价只是一次探路请求。
    let mut saw_transient_without_hint = false;

    // 故障转移总预算（FR：见 AppSettings::failover_total_budget_ms）。
    //
    // 语义是「不再**开始**新的候选尝试」——不硬掐正在进行的请求，尤其不掐已建立的 SSE 流。
    // 没有它时最坏耗时 = 候选数 × per-Key 超时（6 Key × 30s = 180s），而客户端早已超时重发，
    // 代理侧那条僵尸链仍在逐个打上游烧额度。
    let deadline = store.failover_budget().map(|b| std::time::Instant::now() + b);

    for (i, key) in candidates.iter().enumerate() {
        // 诊断头：一进候选就记「试到第几条、是哪条」。放在循环**开头**而不是各分支里，
        // 是为了让**任何**出口（含预算耗尽 break、跨协议 skip、循环尾的全失败）都能带上
        // 最后一次实际触及的候选——分支里各写一遍必然漏掉那些 `break`/`continue` 路径。
        // `attempts` 是 1-based：1 = 首选就成了，> 1 即发生过故障转移。
        meta.attempts = (i + 1) as u32;
        meta.key_name = key.name.clone();
        meta.key_id = key.id.clone();
        // 实际模型名先按该 Key 的解析结果填（真正打出去的名字在成功/失败分支里会被
        // 上游返回的口径覆盖）。这样即使这条候选在预算门那里被跳过，头里也有信息。
        meta.real_model = key.resolve_model(&requested_model);

        // 剩余预算。第一个候选永不因预算被跳过（i == 0 时至少要试一次，否则配置了极小预算
        // 会变成「一个都不试直接 529」，那比慢更糟）。
        let remaining = match deadline {
            Some(d) => {
                let left = d.saturating_duration_since(std::time::Instant::now());
                // 夹一个下限：剩余时间太短时开始新尝试几乎必然超时，白打一次上游还拖长总耗时。
                if i > 0 && left < MIN_ATTEMPT_SLICE {
                    let skipped_by_budget = candidates.len() - i;
                    // 预算耗尽时构造明确的错误信息：若已有失败记录则附加预算信息，
                    // 若还没有失败（极端情况：配置极小预算 + 第一个候选耗时过长）则说明预算不足
                    if last_err.is_empty() {
                        last_err = format!(
                            "故障转移总预算耗尽（剩余时间 {}ms < 最小尝试片 {}ms），剩余 {skipped_by_budget} 个候选未尝试。\
                             建议增加故障转移预算（当前 {} ms）或减少单 Key 超时时间。",
                            left.as_millis(),
                            MIN_ATTEMPT_SLICE.as_millis(),
                            store.failover_budget().map(|d| d.as_millis()).unwrap_or(0)
                        );
                    } else {
                        last_err = format!(
                            "{}；故障转移总预算耗尽，剩余 {skipped_by_budget} 个候选未尝试",
                            last_err
                        );
                    }
                    store.append_event(
                        category,
                        "failover",
                        None,
                        &format!(
                            "故障转移总预算耗尽，跳过剩余 {skipped_by_budget} 个候选（避免客户端已超时后仍继续打上游烧额度）"
                        ),
                    );
                    break;
                }
                // 抬到最小可用片：预算约束的是「开启多少次新尝试」，而**已决定要开启的尝试
                // 必须拿到可用的时间片**。否则会出现「第一个候选没被跳过、但拿到 0ms 超时
                // 而瞬间失败」——那等于变相跳过，比慢更糟（用户配了很小的预算，或上一次
                // 请求刚好耗尽窗口时就会踩到）。
                //
                // 对 i > 0 而言 left 已 ≥ MIN_ATTEMPT_SLICE，这里的 max 是恒等操作；
                // 真正生效的是 i == 0 那种「预算已耗尽但仍必须试一次」的情形。
                Some(left.max(MIN_ATTEMPT_SLICE))
            }
            None => None,
        };
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

        // 分层记账：把这次失败罚到**正确的作用域**（见 [`failure_scope`]）。
        //
        // 收成一个闭包而不是在流式/非流式两处各写一遍 match：这条判据刚从「一个 bool」
        // 升级成「三个作用域」，两处各抄一份就是给下一次漂移留位置 ——
        // `TRANSIENT_4XX` 那条注释记着同类漂移的代价（一次上游抖动熔断好 Key 60s）。
        //
        // `real_model` 必须是**真正发往上游的那个名字**：模型锁的键就是它，
        // 用对外名去锁会被「同一真实模型的另一个对外名」绕过。
        let record_failure_by_scope =
            |store: &Arc<Store>, key: &ProviderKey, status: u16, real_model: &str| {
                match failure_scope(status, &path) {
                    FailureScope::Key => health::record_live_failure(store, &key.id),
                    FailureScope::Model => {
                        health::record_model_unavailable(store, &key.id, real_model)
                    }
                    FailureScope::None => {}
                }
            };

        // 本次尝试传给转发函数的 body（P2-5 第二步）。
        //
        // **只有「没有后续候选」时才交出所有权**（零拷贝移动）；只要还有下一个候选，
        // 就必须传 Borrowed 让对方自己克隆——否则本次的改写（model / effort 注入）会污染
        // 原始 body，下一个候选拿到的就是「上一个候选 resolve 后的模型名」，
        // 那是静默的错路由（不报错、不 panic，只是打错了模型）。
        //
        // ⚠️ **必须在真正要用的那一刻才构造**，不能提到 `if wants_stream` 之前：
        // 那样非流式分支走到时 req_json 已被 take 空，forward_to_key 会收到 `Null`
        // （表现为发给上游的 body 只剩 `"messages": []`）。初版就是这么写的，
        // 被 codex_effort_injected_end_to_end 等三条既有测试当场抓住。
        let take_body = |req_json: &mut Value| -> std::borrow::Cow<'static, Value> {
            std::borrow::Cow::Owned(std::mem::take(req_json))
        };

        // 流式直通分支：下游要 stream 且无需跨协议转换 → 真流式透传（边收边发）。
        // 先探上游状态码：非 2xx 则照常切换下一个 Key（首字节尚未发出，切换安全）；
        // 2xx 则把上游 SSE 流原样转给下游，正确设置 content-type，直接返回（不再切换）。
        if wants_stream && can_stream(key) {
            let body = if next.is_some() {
                std::borrow::Cow::Borrowed(&req_json)
            } else {
                take_body(&mut req_json)
            };
            match try_stream_to_key(&store, category, key, &path, body, &requested_model, &fwd_headers, req_log, remaining)
                .await
            {
                Ok(StreamAttempt::Streaming { resp, url, real_model, request_body }) => {
                    // ⚠️ **不在这里 record_live_success**。响应头 2xx 只说明「开流了」，不代表这次
                    // 转发成功：上游可能 200 后在流内发 error 事件（Anthropic 过载最常见形态）。
                    // 曾经在此处同步记成功，而「流内报错补记失败」要等 body 抽干才跑 ——
                    // 于是每次请求都先把 fail_count 清零、再加回 1，BREAKER_THRESHOLD=3 永不达到，
                    // 该 Key 永不熔断、客户端无限重试同一条坏 Key（实测复现：fail_count 恒为 1）。
                    // 成功/失败的记账统一推迟到**流末**，由 try_stream_to_key 内的流终止路径按
                    // 「有无流内 error」二选一记账（同协议走尾窗判错，跨协议走翻译器旁路的原始尾窗）。
                    //
                    // `clear_all_failed_gate` 仍留在这里：它是并发闸门，拿到 200 就说明池子里有能用的
                    // Key，不该继续短路其它在途请求；若这条流随后失败，流末的失败记账会重新累积熔断。
                    clear_all_failed_gate(&gate_key);
                    let elapsed = started.elapsed().as_millis() as u64;
                    // 诊断头：用上游实际接受的模型名覆盖循环开头填的解析结果，并记本次耗时。
                    // 必须在 `real_model` 被 move 进 `log_success` **之前**。
                    // 一次 String clone（模型名，几十字节）换掉一个「响应头里的模型名可能与
                    // 实际打出去的不一致」的坑，值。
                    meta.real_model = real_model.clone();
                    meta.latency_ms = elapsed;
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
                        None, // 流式直通时无法检测截断（边收边发，不解析完整响应）
                        // 流式直通拿不到 usage：SSE 是边收边发、不缓存全文，用量藏在
                        // 最后几个 chunk 里，要取就得把整个流缓存下来 —— 那会毁掉流式的
                        // 首字节延迟优势。故如实留空，而不是编一个数字。
                        // （聚合走的是非流式路径，那里有完整 usage，见 aggregate。）
                        None,
                    );
                    return Ok(resp);
                }
                // 上游非 2xx：记录并切下一个
                Ok(StreamAttempt::HttpError {
                    status,
                    body,
                    url,
                    real_model,
                    retry_after,
                    request_body: upstream_request_body,
                }) => {

                    let elapsed = started.elapsed().as_millis() as u64;
                    // 诊断头：本次（失败的）尝试耗时，供失败出口的响应头使用。
                    meta.latency_ms = elapsed;
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
                    // 整池口径：这一条是否属「等一等可能会好」（见 saw_transient 声明处的混合池说明）
                    saw_transient = saw_transient || !all_failed_is_hard_error(false, last_status);
                    // 临时性失败却没给 Retry-After → 它的恢复时间未知，不该由别的候选的
                    // 长退避代它发言（见 saw_transient_without_hint 声明处）。
                    if retry_after.is_none() && !all_failed_is_hard_error(false, last_status) {
                        saw_transient_without_hint = true;
                    }
                    // 上游给了状态码 → 这次失败不是本地配置错误。必须复位 config_error，
                    // 否则前一个候选的 Invalid（缺 maxOutputTokens 等）会**粘住**这个标志：
                    // 「配置错 Key 优先 + 后续 Key 撞 429/5xx」时尾部会被判成硬错误，
                    // 原样回状态码、跳过短路窗口与 Retry-After —— 正是 529 设计要防的重试风暴。
                    // 契约同尾部注释：按「最后一次失败的性质」分流（config_error 必须只反映最后一次）。
                    config_error = false;
                    // 分层记账放在 log_request **之前**：那一行会 move 掉 `real_model`，
                    // 而模型锁的键就是它。（此前顺序相反，加分层时才发现。）
                    // 429/5xx 只切不罚；补全端点 404 → 只锁这个模型；其余 4xx 硬错误 → 罚 Key。
                    record_failure_by_scope(&store, key, status, &real_model);
                    // 请求体记的是**发往上游的那份**（跨协议时与 downstream_body 完全不同）。
                    // 界面标签写着「上游请求」，此前却填 downstream_body —— 见 HttpError 变体的说明。
                    log_request(&store, key, elapsed, url, real_model, upstream_request_body, body, Some(status), false);
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
                    // 诊断头：本次（失败的）尝试耗时，供失败出口的响应头使用。
                    meta.latency_ms = elapsed;
                    // 本地配置错误（缺 maxOutputTokens 等）与连接层失败分开处理：前者永不自愈、
                    // 与 Key 无关，不该熔断、不该按临时错误 529 重试（见 config_error 声明）。
                    let is_config_err = matches!(e, AppError::Invalid(_));
                    last_err = e.to_string();
                    last_status = None; // 连接层失败：无状态码，按临时错误对待
                    // 连接层失败属临时性，但**配置错误（Invalid）走的也是这个分支**、它永不自愈，
                    // 不能让它把整轮判成临时性（否则丢掉可行动的 400、回成 529 让客户端无限退避）。
                    saw_transient = saw_transient || !is_config_err;
                    // 连接层失败永远拿不到 Retry-After（没有响应头可读）→ 恒属「恢复时间未知」。
                    saw_transient_without_hint = saw_transient_without_hint || !is_config_err;
                    config_error = is_config_err;
                    log_request(&store, key, elapsed, String::new(), key.resolve_model(&requested_model), downstream_body.clone(), last_err.clone(), None, false);
                    // 被我们自己的预算掐短的尝试不计熔断（见 budget_truncated_attempt）。
                    if !is_config_err
                        && !budget_truncated_attempt(
                            elapsed,
                            remaining,
                            crate::upstream::key_timeout(key),
                        )
                    {
                        health::record_live_failure(&store, &key.id);
                    }
                    log_failover(&store, key, "失败", &last_err, next);
                    continue;
                }
            }
        }

        // 流式 + 无法翻译的跨协议组合（如两端各为 Anthropic/Responses，无中枢路径）：
        // 绝不能走缓冲路径返回 application/json——下游按 text/event-stream 解析必失败。
        // 跳过该候选，让故障转移去找可流式的 Key。
        if wants_stream && !can_stream(key) {
            // ⚠️ 只在**还没有真实失败记录**时才写 last_err/last_status：这个候选根本没被
            // 尝试过，它的「跳过」不能覆盖前面候选的真实失败性质 —— 否则「前面的 Key 全是
            // 429/5xx 临时错误、最后一个候选恰好协议不兼容」时，循环尾部按 last_status 分流
            // 会把整轮判成 501 硬错误：不带 Retry-After、不武装短路窗口、文案只剩「协议
            // 不一致」，客户端视为永不恢复的配置错误不再重试，真实根因（限流/上游抖动）
            // 被完全掩盖。全部候选都被跳过时 last_err 仍为空，501 语义照常成立。
            if last_err.is_empty() {
                last_err = "流式请求不支持跨协议转换（该 Key 协议与下游不一致）".to_string();
                // 501：这是**配置不匹配**，等多久都不会好转，不能包装成「过载请重试」
                // （见函数尾部的状态码分流）。用一个明确的「不支持」码让用户去改配置。
                last_status = Some(StatusCode::NOT_IMPLEMENTED.as_u16());
            }
            log_failover(&store, key, "跳过", "流式请求不支持跨协议转换（该 Key 协议与下游不一致）", next);
            continue;
        }

        // 同上：只有没有后续候选时才交出所有权（零拷贝），否则借用让对方自己克隆。
        let body = if next.is_some() {
            std::borrow::Cow::Borrowed(&req_json)
        } else {
            take_body(&mut req_json)
        };
        let result =
            forward_to_key(&store, category, key, &path, body, &requested_model, &fwd_headers, req_log, remaining)
                .await;
        let elapsed = started.elapsed().as_millis() as u64;
        // 诊断头：本次尝试耗时。放在这里（三个分支之前）而不是各分支里 ——
        // 成功/上游非 2xx/连接层失败都会经过这一行，少一处漏的可能。
        meta.latency_ms = elapsed;
        match result {
            Ok(outcome) if outcome.ok => {
                // 响应体快照只在开关开启时构造：唯一去向是下面的 log_success，
                // 而它在 `req_log` 关闭时直接 return。成功响应体常态几十 KB~几百 KB
                // （流式以外的完整回答），默认关日志时这份 to_string() 是纯浪费。
                let resp_text = if req_log {
                    String::from_utf8_lossy(&outcome.bytes).to_string()
                } else {
                    String::new()
                };
                // 模型名用 `outcome.real_model`（上游实际接受的那个），不是对外名 ——
                // 模型锁的键就是它。此处 outcome 还没解构，直接借。
                health::record_live_success(&store, &key.id, Some(&outcome.real_model));
                // 有 Key 能用了 → 立即解除「全部失败」短路，不等窗口自然到期。
                clear_all_failed_gate(&gate_key);
                // 非流式有完整响应体 → 能真取到 token 用量（两协议字段名已在
                // extract_usage 里归一）。上游没给用量时返回 None，日志如实不显示。
                let parsed_body = serde_json::from_slice::<Value>(&outcome.bytes).ok();
                let usage = parsed_body.as_ref().and_then(crate::upstream::extract_usage);
                // 检测截断：finish_reason:"length"（OpenAI）/ stop_reason:"max_tokens"（Anthropic）
                let was_truncated = parsed_body.as_ref().and_then(|v| {
                    crate::upstream::is_truncated_response(v).then_some(true)
                });
                // 解构取走三个 String（url / real_model / request_body），消掉三次 clone。
                // `bytes` 是 `Bytes`（引用计数，clone 廉价）留到最后回给下游；`status` 是 Copy。
                // 这是本分支对 outcome 的最后一次使用，故可以整体移动而非借用。
                let ForwardOutcome { bytes, url, real_model, request_body, status, .. } = outcome;
                // 诊断头：用上游实际接受的模型名覆盖循环开头填的解析结果。
                // 必须在 `real_model` 被 move 进 `log_success` 之前。耗时已在三分支之前统一记过。
                meta.real_model = real_model.clone();
                log_success(
                    &store,
                    key,
                    elapsed,
                    url,
                    real_model,
                    request_body,
                    resp_text,
                    status,
                    false,
                    was_truncated,
                    usage,
                );
                return Ok(json_resp(StatusCode::OK, bytes));
            }
            // 上游有响应但非 2xx：记完整快照（含上游返回的真实错误体），再切下一个
            Ok(outcome) => {
                // snippet 必须**无条件**产生：它进 `last_err`，最终作为错误信息返回给客户端，
                // 与日志开关无关。但不必为此先把整个响应体 to_string()——直接在借用的
                // Cow 上 trim + 取前 400 **字符**（`chars()` 而非字节切片：上游错误体常含中文，
                // 按字节截会切裂多字节序列）。
                let resp_cow = String::from_utf8_lossy(&outcome.bytes);
                let snippet: String = resp_cow.trim().chars().take(400).collect();
                // 完整响应体只在开关开启时落成 String（唯一去向是下面的 log_request）。
                let resp_text = if req_log { resp_cow.into_owned() } else { String::new() };
                last_err = if snippet.is_empty() {
                    format!("HTTP {}", outcome.status)
                } else {
                    format!("HTTP {}: {}", outcome.status, snippet)
                };
                if let Some(s) = outcome.retry_after {
                    retry_after_hint = Some(retry_after_hint.map_or(s, |cur: i64| cur.min(s)));
                }
                last_status = Some(outcome.status);
                // 整池口径：这一条是否属「等一等可能会好」（见 saw_transient 声明处的混合池说明）
                saw_transient = saw_transient || !all_failed_is_hard_error(false, last_status);
                // 临时性失败却没给 Retry-After → 恢复时间未知，不该由别的候选的长退避代它发言
                // （见 saw_transient_without_hint 声明处）。
                if outcome.retry_after.is_none() && !all_failed_is_hard_error(false, last_status) {
                    saw_transient_without_hint = true;
                }
                // 复位理由同流式 HttpError 分支：上游有状态码 → 非本地配置错误，
                // config_error 必须只反映最后一次失败的性质，不能被前一候选的 Invalid 粘住。
                config_error = false;
                // 解构取走三个 String，消掉三次 clone（本分支对 outcome 的最后一次使用）。
                // 必须放在 `resp_cow` 用完之后：它借用了 `outcome.bytes`。
                let ForwardOutcome { url, real_model, request_body, status, .. } = outcome;
                // 分层记账放在 log_request **之前**（同流式分支）：那一行会 move 掉 `real_model`，
                // 而模型锁的键就是它。
                record_failure_by_scope(&store, key, status, &real_model);
                log_request(
                    &store,
                    key,
                    elapsed,
                    url,
                    real_model,
                    request_body,
                    resp_text,
                    Some(status),
                    false,
                );
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
                // 同流式分支：区分本地配置错误与连接层失败（见 config_error 声明）。
                let is_config_err = matches!(e, AppError::Invalid(_));
                last_err = e.to_string();
                last_status = None; // 连接层失败：无状态码，按临时错误对待
                // 连接层失败属临时性，但**配置错误（Invalid）走的也是这个分支**、它永不自愈，
                // 不能让它把整轮判成临时性（否则丢掉可行动的 400、回成 529 让客户端无限退避）。
                saw_transient = saw_transient || !is_config_err;
                // 连接层失败永远拿不到 Retry-After（没有响应头可读）→ 恒属「恢复时间未知」。
                saw_transient_without_hint = saw_transient_without_hint || !is_config_err;
                config_error = is_config_err;
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
                if !is_config_err
                    && !budget_truncated_attempt(
                        elapsed,
                        remaining,
                        crate::upstream::key_timeout(key),
                    )
                {
                    health::record_live_failure(&store, &key.id);
                }
                log_failover(&store, key, "失败", &last_err, next);
            }
        }
    }

    store.append_event(category, "error", None, &format!("全部 Key 失败: {last_err}"));

    // 诊断头：把**最后一次**上游状态码带给下游。
    //
    // 这是这组头在失败路径上最值钱的一个字段：下游只会看到我们合成的 529 / 或原样回传的
    // 4xx，看不出「是上游真的 429 了」还是「代理这边判成了配置错误」。有了它，用户贴一条
    // `x-synaroute-upstream-status: 401` 就直接指向密钥失效，不必先怀疑网络/额度。
    // 连接层失败无状态码（`last_status` 为 None）→ 头省略，本身也是一个信号。
    meta.upstream_status = last_status;

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
    //
    // **整池口径**：`all_failed_is_hard_error` 只看最后一个候选，而 last_status 被每个候选无条件
    // 覆盖。混合池（k1 撞配额 429+Retry-After / k2 过期 Key 回 401）下尾部只看到 401 → 原样回
    // 401、丢掉 Retry-After、不武装短路窗口，把「7 秒后就能用」讲成「密钥错了」。故只要**池里
    // 出现过**临时性失败（saw_transient），整轮就按临时性处置。硬错误码仍在 failover 事件里可见。
    let is_hard_error = !saw_transient && all_failed_is_hard_error(config_error, last_status);

    if is_hard_error {
        // config_error 无上游状态码（last_status=None），用 400 Bad Request：
        // 它确实是「请求/配置不合法」，且客户端不会对 400 做过载退避重试。
        let status = last_status
            .and_then(|s| StatusCode::from_u16(s).ok())
            .unwrap_or(if config_error {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::BAD_GATEWAY
            });
        return Ok(error_resp(status, &compose_hard_error_body(last_status, &last_err)));
    }

    // 短路窗口长度（秒），同时是「恢复时间未知」这类候选的默认退避值。
    let gate_secs = (ALL_FAILED_SHORT_CIRCUIT_MS / 1000).max(1);
    let effective_hint = effective_retry_after_hint(retry_after_hint, saw_transient_without_hint);
    // 武装短路窗口：窗口内后续请求（含客户端自动重发）直接失败，不再重打全部上游。
    // 带上候选给出的最早 Retry-After（见 retry_after_hint 的取最小值理由）。
    // **必须与下游收到的头同一个值**：窗口比头短会让客户端在窗口外白等，
    // 窗口比头长会让客户端按头重发却被窗口挡住 —— 两者都是「说一套做一套」。
    arm_all_failed_gate(&gate_key, effective_hint);
    // 退避秒数：有上游值就用它（夹到 [1, MAX]——上游可能给 0，那等于不退避，与短路矛盾）；
    // 没有则用短路窗口本身的长度（窗口内重试注定被挡，早回来毫无意义）。
    let retry_after = effective_hint.map(|s| s.clamp(1, MAX_RETRY_AFTER_SECS)).unwrap_or(gate_secs);
    Ok(error_resp_with_retry_after(
        overloaded_status(),
        &format!("全部 Key 不可用：{last_err}"),
        Some(retry_after),
    ))
}

/// 这次失败是否由**我们自己**掐短的超时造成（→ 不该算进该 Key 的熔断计数）。
///
/// 每次尝试的实际超时是 `min(Key 自身超时, 故障转移剩余预算)`。当剩余预算比 Key 的超时更小时，
/// 一条**完全健康**的 Key 也会被我们在它答完之前掐断 —— 而那个 Err 走的是连接层分支，
/// 于是 `record_live_failure` 给它记一次失败。三次之后这条好 Key 被熔断 60 秒。
///
/// 真机形态：6 条 Key、per-Key 30s、总预算 10s。前两条各耗 4s 真的坏了，第三条只剩 2s ——
/// 它 2s 内没答完（正常，它需要 5s），被判失败。用户反复请求几轮后，**池子里最好的那条 Key
/// 反而先被熔断**，因为它总是排在预算末尾。这是「越排后面越容易被误判」的系统性偏置，
/// 不是偶发。
///
/// 判据要两个条件同时成立，缺一个都会误判：
/// - `budget < key_timeout`：确实是我们把时间片削短了（预算未开启或比 Key 超时还长时不成立）；
/// - `elapsed >= budget`：这次尝试**真的用完了**被削短的时间片。少了这条，一个瞬间失败的
///   DNS 错误也会被当成「我们掐的」而免罚，等于在预算紧张时整池都不再熔断。
///
/// 留 50ms 容差：`elapsed` 是错误返回后才测的，与超时触发点之间有调度抖动，
/// 严格 `>=` 会让恰好卡在边界的那次漏判（漏判方向 = 误罚好 Key，正是要修的）。
fn budget_truncated_attempt(
    elapsed_ms: u64,
    budget_left: Option<std::time::Duration>,
    key_timeout: std::time::Duration,
) -> bool {
    let Some(budget) = budget_left else { return false };
    if budget >= key_timeout {
        return false;
    }
    elapsed_ms.saturating_add(50) >= budget.as_millis() as u64
}

/// 全 Key 失败后真正要用的 `Retry-After` 秒数（`None` = 没有任何上游给过头）。
///
/// `hint` 是**给了头的候选之间**的最小值。问题在于没给头的候选压根不参与那次比较：
/// 混合池 `[A: 429 Retry-After: 3600（解析时已夹到 300）, B: 500 无头]` 的结论会是
/// 「等 300 秒」，而 B 的 500 很可能 1 秒后就好了。这与「取最小值」那段注释自己写的判据
/// （**只要有一个候选恢复就该放行**）直接冲突 —— 同一个误伤（一个撞配额的 Key 拖垮整池），
/// 只是从「取最大值」换成了「唯一给头的那个替全池发言」这条更隐蔽的路径。
///
/// 处置：把「恢复时间未知」当作一个默认候选（= 短路窗口长度）一起参与取最小值。
/// 「未知」的正确近似既不是无穷远、也不是立刻，而是「按本项目自己的短路窗口再探一次」：
/// 早到的重试若仍失败，窗口会再武装一次，代价只是一次探路请求。
///
/// 两个方向都必须成立（故测试各钉一条）：
/// - `3600 + 无头` → 窗口长度（不被拖到 300）；
/// - `1 + 无头` → 1（不被拖长到窗口长度）。取 `min` 而非直接换成窗口长度即为此。
///
/// `None` 时不凭空造值：调用方另有「全无头就退避一个窗口长度」的兜底，
/// 在这里返回 `Some(gate)` 会让「上游从没提过退避」与「上游说了正好一个窗口」不可区分。
fn effective_retry_after_hint(hint: Option<i64>, saw_transient_without_hint: bool) -> Option<i64> {
    let gate_secs = (ALL_FAILED_SHORT_CIRCUIT_MS / 1000).max(1);
    hint.map(|s| if saw_transient_without_hint { s.min(gate_secs) } else { s })
}

/// 529 的 `StatusCode`。529 非 IANA 注册码（Cloudflare/Anthropic 惯例的「过载」码），
/// http crate 无关联常量，只能 `from_u16` 构造；它对 100~999 恒成功，故 expect 不会触发。
fn overloaded_status() -> StatusCode {
    StatusCode::from_u16(STATUS_OVERLOADED).expect("529 是合法 HTTP 状态码")
}

/// 临时性 4xx：`稍后重试` 语义，与 Anthropic SDK 的重试判据一致（408/409/429 或 >=500）。
///
/// **单一事实来源**：尾部分流（[`all_failed_is_hard_error`]）与熔断计数
/// （[`status_counts_against_breaker`]）都引用它。这两处此前各写各的名单已经漂移过一次
/// （尾部把 408/409 与非 500/502/503/504 的 5xx 判为临时，熔断却把它们算进惩罚，
/// 于是一次超时/Cloudflare 52x 就把完好的 Key 熔断 60s）。共用一个常量杜绝再漂移。
const TRANSIENT_4XX: [u16; 3] = [408, 409, 429];

/// 全 Key 失败后的「硬错误」判定（决定尾部分流）。
///
/// - **硬错误** → 原样回状态码（config_error 无码时回 400）、**不**武装短路窗口、**不**带
///   Retry-After：401/403/404 等 4xx、协议不匹配的 501、以及本地配置错误（缺 maxOutputTokens 等，
///   `config_error=true` 而 `last_status=None`）—— 都永不自愈，包装成 529 只会让客户端无限退避。
/// - **临时性**（429/408/409/5xx/连接层失败）→ 回 529 + Retry-After + 武装短路窗口。
///
/// `config_error` 契约：**只反映最后一次失败的性质**。两个上游状态码失败分支都会把它复位为
/// false，否则「配置错 Key 优先 + 后续 Key 撞 429/5xx」时它会被前一候选的 Invalid 粘住，
/// 把一整轮临时故障误判成硬错误（回裸状态码、丢掉 Retry-After 与短路窗口）。
fn all_failed_is_hard_error(config_error: bool, last_status: Option<u16>) -> bool {
    config_error
        || matches!(
            last_status,
            Some(s) if (s == 501 || ((400..500).contains(&s) && !TRANSIENT_4XX.contains(&s)))
        )
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

/// 流式响应尾部滑动窗口大小。SSE 的 usage 统计在最末几个事件里，只需留住尾巴，
/// 不缓存全文 —— 既不牺牲首字节延迟，也不让长会话的响应体常驻内存。
const TAIL_WINDOW_BYTES: usize = 8192;

/// 流式响应**头部**窗口大小。Anthropic 把 input_tokens / cache_read / cache_creation 放在流首的
/// `message_start` 事件里；只留尾窗时，任何 >8KB 的回答会把它挤掉 → input/缓存 token 记成 0
/// （日志与「额度花在哪」面板把占比常 >90% 的输入/缓存显示为 ~0）。故另留头部 8KB：一旦攒够
/// 就不再增长（message_start 是第一个事件，必在其中），流末与尾窗合并取 usage。
const HEAD_WINDOW_BYTES: usize = 8192;

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
    ///
    /// 这里**不带** token 用量：流是边收边发的，`log_success` 同步执行时流才刚开始，
    /// 那一刻上游还没吐出末尾的 usage 事件。用量由流内的尾部窗口在流结束后
    /// 异步补记（同 collapse key 合并进同一条日志），不走这个返回值。
    Streaming {
        resp: Response<ResBody>,
        url: String,
        real_model: String,
        request_body: String,
    },
    /// 上游有响应但非 2xx：缓冲错误体，调用方据此切换下一个 Key。
    /// `retry_after`：上游 `Retry-After` 头解析出的秒数（429/503 常带），无则 None。
    ///
    /// `request_body` = **转换后发往上游的**请求体快照，与 `Streaming` 那份同一口径。
    /// 此前这个变体不带它，调用方只能退而记 `downstream_body`（客户端原样发来的那份）——
    /// 而链路快照的界面标签写的是「上游请求」。跨协议 Key 上两者根本不是一回事：
    /// 排查「为什么这个 Key 回 400」时，看到的是一份 Anthropic 请求体，
    /// 而我们实际发出去的是 OpenAI Responses 格式，映射结果、`max_tokens` 补写、
    /// reasoning→thinking 转换全都看不见 —— 恰恰是 400 最常见的成因所在。
    HttpError {
        status: u16,
        body: String,
        url: String,
        real_model: String,
        retry_after: Option<i64>,
        request_body: String,
    },
}

/// 同协议流式转发：把上游 SSE 响应边收边发透传给下游。
///
/// 与 forward_to_key 的差异：
/// - 不设总超时（长回答会被 30s 掐断），仅设连接超时；流本身靠客户端断开或上游结束收尾。
/// - 先 send() 探状态码：非 2xx 缓冲错误体返回 HttpError（首字节未发，切换安全）；
///   2xx 则用 bytes_stream() 逐块转发，content-type 沿用上游真实值（保 text/event-stream）。
/// - 仅在下游协议与 Key 协议一致时调用（无需跨协议转换），跨协议 SSE 翻译属已知限制。
// 参数多但每个都是这条转发链必需的运行时上下文（store/分类写日志、key/path/body/model 定位请求、
// headers 透传客户端身份）。抽成 struct 只是换个地方传同样的东西，还要动全部调用点 ——
// 与 `Store::append_event_full`、`aggregate::run_member_turns` 同样的取舍。
#[allow(clippy::too_many_arguments)]
async fn try_stream_to_key(
    store: &Arc<Store>,
    category: CategoryType,
    key: &ProviderKey,
    path: &str,
    // 下游请求体。用 `Cow` 而非 `&Value`（P2-5 第二步）：调用方对**最后一个候选**传
    // `Cow::Owned`（零拷贝移动），之前的候选传 `Cow::Borrowed`（在函数内克隆，保证后续候选
    // 拿到的仍是未被污染的原始 body）。同协议单候选是最常见路径，此时全程零深拷贝。
    req_json: std::borrow::Cow<'_, Value>,
    requested_model: &str,
    fwd_headers: &[(String, String)],
    // 见 `forward_to_key` 同名形参：仅用于决定是否构造只喂给日志的请求体快照。
    req_log: bool,
    // 故障转移剩余预算；None = 用户关闭了整体预算。
    //
    // **只用于约束 send() 探头阶段**（等上游返回响应头/状态码）。一旦拿到 2xx 并开始
    // 转发 SSE，就完全不再受它约束——流式刻意不设总超时，掐断会把长回答截断，
    // 那是本项目明确的设计判断（见下方 send() 处的注释）。
    budget_left: Option<std::time::Duration>,
) -> AppResult<StreamAttempt> {
    // 走 secret_for：DPAPI 模式带进程内解密缓存，免掉每请求每候选一次 CryptUnprotectData
    // 内核态系统调用（P2-6）。锁定态返 Err 的刻意语义原样保留。
    //
    // 密钥缺失是**本地配置错误**，不是上游故障 → 必须用 `AppError::Invalid`。
    // 原先用 `upstream_msg`，会被尾部分流判成「临时性上游故障」：回 529 + Retry-After +
    // 武装短路窗口 + 计入熔断，客户端据此无限退避重试 —— 而这个问题**永不自愈**
    // （密钥根本没存进去，或主口令未解锁），用户还会以为是中转站过载、方向完全错。
    // Invalid 走硬错误分支：原样回 400、不熔断好 Key、不退避，把可行动原因直接摆给客户端。
    let secret = store
        .secret_for(&key.id)?
        .ok_or_else(|| {
            AppError::Invalid(format!(
                "Key「{}」没有可用的密钥：请在 Key 编辑器里填写 API 密钥并保存（若已启用主口令，请先解锁后重试）。",
                key.name
            ))
        })?;

    // 模型解析：映射 → 原生支持 → 默认兜底 → 第一个模型 → 透传（见 ProviderKey::resolve_model）
    let real_model = key.resolve_model(requested_model);

    let downstream = downstream_protocol(path);
    // SSE 翻译方向：同协议为 None（原样透传）；跨协议按方向重组事件流。
    let sse_dir = crate::upstream::sse_direction(downstream, key.protocol);

    // 响应侧翻译需要的三个工具集合**必须在消费 req_json 之前收集**（P2-5 第二步）。
    //
    // 只在跨协议（sse_dir.is_some()）时才收集：同协议是原样透传，用不到它们，
    // 而同协议恰恰是最常见的路径——不能为了这里的便利给它增加无谓工作。
    //
    // 三者都是对下游 body 的**纯读取**（读 tools 声明），而下面只改 model / effort，
    // 故「先收集再改」与「先改再收集」结果一致。
    let tool_sets = sse_dir.map(|_| {
        // 从下游请求 tools 收集 namespace 名（Codex 把 MCP 工具折叠成 type:"namespace"）。
        // 响应侧回填 function_call 时据此把全名 <ns>__<sub> 拆回 {name, namespace} 两字段——
        // Codex router 用结构化 ToolName{namespace,name} 查表，不拆 name 字符串，缺 namespace
        // 字段就报 unsupported call（大脑聚合失效根因）。
        let namespaces = crate::upstream::collect_tool_namespaces(&req_json);
        let custom_tools = crate::upstream::collect_custom_tools(&req_json);
        // Codex 的延迟工具检索器（type:"tool_search"）：模型对它的调用必须回程成
        // tool_search_call，Codex 才会本地跑 BM25 检索并在下一轮把 MCP 工具真 schema
        // 灌进 tool_search_output —— 那是 mcp__* 工具唯一的来源。
        let search_tools = crate::upstream::collect_search_tools(&req_json);
        (namespaces, custom_tools, search_tools)
    });

    // 下游协议 → 上游 Key 协议：请求体按需转换。
    //
    // `into_owned()`：调用方给的是 `Cow`——最后一个候选传 Owned（零拷贝移动），
    // 之前的候选传 Borrowed（在此克隆一份，保证后续候选拿到的仍是未被污染的原始 body）。
    let mut payload = req_json.into_owned();
    if let Some(obj) = payload.as_object_mut() {
        obj.insert("model".into(), Value::String(real_model.clone()));
    }
    inject_default_effort(store, category, &mut payload, downstream, key.protocol);
    // Key 上配的 temperature / top_p（下游没显式给才注入）；输出 token 上限绝不补写。
    // count_tokens 等非补全子路径不注入（那些端点的 schema 不含采样字段，注了会被上游 400）。
    if path_takes_sampling_params(path) {
        apply_key_params(&mut payload, &key.params);
    }
    let payload = crate::upstream::convert_request_owned(payload, downstream, key.protocol);

    // OpenAI→Anthropic 跨协议时，Anthropic 的 max_tokens 虽必填，但客户端可能没给（OpenAI
    // 允许省略）。转换器不知道目标 Key/真实模型，绝不能在里面回退 4096；这里才有完整信息，
    // 因而只在补全端点按「模型最大输出 ∩ 窗口」补。count_tokens 的 schema 不收 max_tokens，
    // 必须跳过，否则严格 Anthropic 上游 400。
    let payload = if path_takes_sampling_params(path) {
        ensure_anthropic_output_budget(payload, key, &real_model)?
    } else {
        // 非补全端点（count_tokens 等）不补 max_tokens → 不会触发 apply_pending_thinking。
        // 若转换阶段暂存过 `_pending_effort`（仅目标 Anthropic + 有 effort + 无 max_tokens 时），
        // 这里兜底剥掉，杜绝把中转哨兵字段发给上游（真实客户端不走此路径，属纵深防御）。
        let mut payload = payload;
        crate::upstream::strip_pending_effort(&mut payload);
        payload
    };
    // 跨协议：退回上游协议的补全端点。
    let resource_path: std::borrow::Cow<str> = if downstream == key.protocol {
        std::borrow::Cow::Borrowed(path)
    } else {
        std::borrow::Cow::Borrowed(key.protocol.completion_path())
    };
    let url = crate::upstream::join_endpoint(&key.base_url, &resource_path);

    // 流式：用共享客户端（连接池复用，含 connect_timeout），不设总超时，避免长回答被掐断。
    let client = crate::upstream::shared_client();

    // 请求头统一由 apply_upstream_headers 装齐（与非流式路径共用同一实现，防两条路径分叉）。
    // **不在 RequestBuilder 上调 `.timeout()`**：reqwest 的那个超时覆盖「整个请求含读完 body」，
    // 对 SSE 流等于给长回答设了硬上限，会把回答截断。流式的超时只能套在**探头阶段**，
    // 见下面的 `probe_to`。
    let rb = apply_upstream_headers(client.post(&url).json(&payload), key, &secret, fwd_headers, &real_model);

    // 探头阶段超时 = min(Key 自身超时, 故障转移剩余预算)，与非流式的 `effective_to` 同口径。
    //
    // **只约束探头阶段**（等上游返回响应头/状态码），拿到 2xx 之后一律不再设超时：
    // 一旦开始转发 SSE，掐断就等于把长回答截断——那是本项目刻意避免的行为。
    // 而探头阶段不产出任何内容、卡住只是白等，超时让故障转移及时进入下一个候选。
    //
    // 为什么必须把 `key_timeout` 也算进来（本轮修的缺口）：此前这里**只用 budget_left**，
    // 于是 ① 用户为该 Key 设的超时（如 10s）对流式完全无效，仍要等满 90s 总预算；
    // ② 更糟的是把总预算设成 0（关闭）时 `budget_left` 为 None，流式探头**没有任何超时**
    // ——上游连上却不回响应头就永久挂着，直到 TCP 自己断。而 Claude Code / Codex 默认都发
    // `stream:true`，这是主路径。用户为了「别掐断长回答」去关总预算，恰恰会踩中这个组合。
    let key_to = crate::upstream::key_timeout(key);
    let probe_to = match budget_left {
        Some(b) => key_to.min(b),
        None => key_to,
    };
    let send_fut = rb.send();
    let resp = match tokio::time::timeout(probe_to, send_fut).await {
        Ok(r) => r.map_err(|e| AppError::upstream_msg(format!("连接 {url} 失败: {e}")))?,
        Err(_) => {
            return Err(AppError::upstream_msg(format!(
                "连接 {url} 超时（{}ms 内未拿到响应头）",
                probe_to.as_millis()
            )))
        }
    };
    let status = resp.status();

    if !status.is_success() {
        // 非 2xx：缓冲错误体供切换决策与日志。Retry-After 须在读 body（消费 resp）之前取。
        let retry_after = parse_retry_after(resp.headers());
        // **读错误体也必须有超时**（与探头同一口径）。此前这里是裸 `resp.bytes().await`：
        // shared_client 只设了 connect_timeout、没有响应超时，而故障转移的 deadline 只管
        // 「不再开始新尝试」、管不到已开始的这一次。于是上游发完 429/5xx 响应头就停止发 body
        // （半开连接 / LB 中途丢弃 / chunked 不收尾）时，这个候选**永久阻塞**，后续候选一个都
        // 轮不到，下游连接一直挂着直到客户端自己超时 —— 而 stream:true 是主路径。
        // 错误体只用于日志与 last_err，读不全无所谓，宁可给个「读取超时」也不能挂住整条链。
        let body = match tokio::time::timeout(probe_to, resp.bytes()).await {
            Ok(Ok(b)) => String::from_utf8_lossy(&b).to_string(),
            Ok(Err(e)) => format!("（错误体读取失败：{e}）"),
            Err(_) => format!(
                "（错误体读取超时：{}ms 内未读完，已按失败切换下一个候选）",
                probe_to.as_millis()
            ),
        };
        return Ok(StreamAttempt::HttpError {
            status: status.as_u16(),
            body,
            url,
            real_model,
            retry_after,
            // 与成功路径同一口径：开关关闭时不构造（pretty-print 整个请求体不便宜）。
            // 失败路径**比成功路径更需要**这份快照 —— 排 400/422 靠的就是核对我们到底发了什么。
            request_body: if req_log {
                serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string())
            } else {
                String::new()
            },
        });
    }

    // 2xx：同协议原样透传；跨协议用 SseTranslator 逐块重组事件流。
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("text/event-stream")
        .to_string();
    // 上游的 content-encoding（正常应为空——我们已发 `accept-encoding: identity`）。
    // 仍取它是纵深防御：个别网关不守规范、不问自压。同协议是**字节透明**透传，
    // 此时必须把该头一并转给下游，否则下游按明文解析一堆压缩字节 → 乱码。
    // 跨协议不透传：那条路要自己解析 SSE 文本，压缩体根本翻译不了（见下方 else 分支说明）。
    let upstream_content_encoding = resp
        .headers()
        .get("content-encoding")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty() && s != "identity")
        .map(|s| s.to_string());

    let body: ResBody = match sse_dir {
        // 同协议：reqwest 字节流 → hyper StreamBody，逐块原样透传。
        // 同时用滑动窗口保留尾部字节，流结束后从尾部 SSE chunk 提取 token 用量——
        // 不缓存全文、不牺牲首字节延迟。提取失败（上游没给 usage）时静默忽略。
        None => {
            // 尾部滑动窗口 + 「流已结束」信号。
            //
            // buffer 用 `std::sync::Mutex`：写入发生在 `map` 这个**同步**闭包里，
            // 用异步锁只能 `try_lock`，一旦竞争就静默丢块。这里写者只有流本身、
            // 读者只在流结束后才动，用同步锁反而既简单又不丢数据。
            //
            // 结束信号用 `Notify` + Drop 守卫：`map` 闭包被 drop 的时刻，就是
            // 上游流被消费完（或下游提前断开）的时刻，hyper 送完 body 就会 drop 它。
            // 这是唯一能在 `.map()` 上拿到「流结束」的可靠点位 —— 用 `notify_one`
            // 而非 `notify_waiters`，因为它会留下 permit，补记任务晚到也不会漏掉。
            struct StreamEnd(std::sync::Arc<tokio::sync::Notify>);
            impl Drop for StreamEnd {
                fn drop(&mut self) {
                    self.0.notify_one();
                }
            }

            let tail_buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::with_capacity(
                TAIL_WINDOW_BYTES,
            )));
            // 头部窗口：留住流首的 message_start（input/缓存 token 所在），攒够 HEAD_WINDOW_BYTES
            // 即停增长。与尾窗分开两块，不合并成一个大缓冲——长会话下全文可达几十万字节。
            let head_buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::with_capacity(
                HEAD_WINDOW_BYTES,
            )));
            let stream_done = std::sync::Arc::new(tokio::sync::Notify::new());
            let end_guard = StreamEnd(stream_done.clone());

            let tail_buf2 = tail_buf.clone();
            let head_buf2 = head_buf.clone();
            let store2 = store.clone();
            let key_id2 = key.id.clone();
            // 流末记账要按「哪个模型」衰减第二层的模型锁，故必须带上真实模型名
            // （`req_model2` 是**对外名**，不是这个）。
            let real_model2 = real_model.clone();
            let category2 = category;
            // 补记只需要「定位到哪一行」的三要素（分类 + key + collapse_key），
            // 不再自己拼 detail —— detail 由流开始时那条同步日志负责，补记只往里补用量。
            let req_model2 = requested_model.to_string();

            let byte_stream = resp.bytes_stream().map(move |chunk| {
                // 闭包持有守卫；闭包被 drop → 守卫 Drop → 通知补记任务。
                let _ = &end_guard;
                match chunk {
                    Ok(bytes) => {
                        // 头部窗口：只在未满 HEAD_WINDOW_BYTES 前追加（message_start 是第一个事件，
                        // 必落在前 8KB 内）。满了就不再动，省得长流一直拷贝。
                        if let Ok(mut hbuf) = head_buf.lock() {
                            if hbuf.len() < HEAD_WINDOW_BYTES {
                                let room = HEAD_WINDOW_BYTES - hbuf.len();
                                let take = room.min(bytes.len());
                                hbuf.extend_from_slice(&bytes[..take]);
                            }
                        }
                        // 滑动窗口：只留尾部 8KB（output usage / 终止事件在 SSE 最末几个事件里）。
                        if let Ok(mut buf) = tail_buf.lock() {
                            buf.extend_from_slice(&bytes);
                            if buf.len() > TAIL_WINDOW_BYTES {
                                let drop_n = buf.len() - TAIL_WINDOW_BYTES;
                                buf.drain(0..drop_n);
                            }
                        }
                        Ok(Frame::data(bytes))
                    }
                    Err(e) => Err(std::io::Error::other(e.to_string())),
                }
            });

            // 补记任务：**必须**先等流结束再读窗口。
            // 曾经这里直接 `lock().await` 就读 —— 而 body 是惰性的，spawn 那一刻流还没被
            // poll 过一个字节，锁空闲、buffer 为空，于是永远提取不到 usage 且不报错。
            tokio::spawn(async move {
                stream_done.notified().await;
                let tail_snap = match tail_buf2.lock() {
                    Ok(buf) => buf.clone(),
                    Err(_) => return,
                };
                let head_snap = head_buf2.lock().map(|b| b.clone()).unwrap_or_default();
                // 头窗（message_start：input/缓存 token）+ 尾窗（message_delta：output token）合并解析。
                // 只留尾窗时，>8KB 的回答会把流首的 message_start 挤掉 → input/缓存记成 0
                // （面板把占比常 >90% 的输入/缓存显示为 ~0）。extract_usage_from_sse 按字段取最大值，
                // 头尾拼接即得完整用量；两窗重叠（短流全落头窗、也落尾窗）也不会翻倍（取 max 而非累加）。
                let tail_sse = String::from_utf8_lossy(&tail_snap);
                let mut merged = String::with_capacity(head_snap.len() + tail_snap.len() + 1);
                merged.push_str(&String::from_utf8_lossy(&head_snap));
                merged.push('\n');
                merged.push_str(&tail_sse);
                // 流末统一记账（成功/失败二选一）。**必须在这里而不是拿到 200 响应头时**：
                // 上游可能 200 后在流内发 error 事件（Anthropic 过载常见形态）。早记成功会把
                // fail_count 清零，使随后的失败补记永远只能把它从 0 加到 1、够不到熔断阈值
                // （该 Key 永不熔断 → 客户端反复重打同一条坏 Key，正是熔断要止住的风暴）。
                // 用尾窗判错：终止性 error 事件必落在流末 8KB 内。
                if crate::upstream::sse_stream_errored(&tail_sse) {
                    health::record_live_failure(&store2, &key_id2);
                } else {
                    health::record_live_success(&store2, &key_id2, Some(&real_model2));
                }
                if let Some(u) = crate::upstream::extract_usage_from_sse(&merged) {
                    // 补记进**流开始时已写下的那一行**，不新追加一条。
                    // 再 append 一条同 collapse key 的事件会被折叠逻辑当成「又发生了一次」：
                    // repeat 变 2（一次请求显示成 ×2）、detail 被覆盖（延迟数字丢失）。
                    let collapse = format!("ok:{}:{}:{}", key_id2, req_model2, true);
                    store2.backfill_usage_for_collapsed_event(
                        category2,
                        Some(&key_id2),
                        &collapse,
                        u,
                    );
                }
            });
            BodyExt::boxed(StreamBody::new(byte_stream))
        }
        // 跨协议：有状态翻译器逐块把上游 SSE 重组成下游协议事件；流末尾冲刷收尾事件。
        // 用 stream::unfold 承载状态机（不引 async_stream 依赖）：累加器持有翻译器、上游流、
        // 以及「是否已冲刷收尾」标志。每步产出一个 Frame。
        Some(dir) => {
            // 三个集合已在消费 req_json 之前收集好（见函数上方 tool_sets）。
            // sse_dir 为 Some 时 tool_sets 必然也是 Some（同一个条件），故这里可以直接取。
            let (namespaces, custom_tools, search_tools) =
                tool_sets.expect("sse_dir 为 Some 时 tool_sets 必然已收集");
            let translator = crate::upstream::SseTranslator::with_namespaces_and_custom(
                dir,
                namespaces,
                custom_tools,
                search_tools,
            );
            let upstream = resp.bytes_stream();

            // 用量补记三要素（与同协议分支同一套定位口径：分类 + key + collapse_key）。
            // 跨协议这条路**不用**尾窗 + `extract_usage_from_sse`：翻译器边收边转，尾窗里
            // 躺的是已转换成下游格式的字节，字段名与上游不一定对得上。翻译器本来就要读懂
            // 上游 usage 才能翻译，直接问它最准，也省一份 8KB 缓冲。
            let usage_store = store.clone();
            let usage_key_id = key.id.clone();
            let usage_category = category;
            let usage_req_model = requested_model.to_string();
            // 同同协议分支：流末衰减模型锁要用真实模型名，不是对外名。
            let usage_real_model = real_model.clone();

            struct StreamState<S> {
                translator: crate::upstream::SseTranslator,
                upstream: S,
                finished: bool,
                /// 只补记一次。上游结束会走两遍 `None` 分支（第一遍冲刷收尾事件、
                /// 第二遍才真终止），不设这个闩会补记两次 —— 第二次把同一行的用量
                /// 又写一遍，虽然值相同，但下次改成累加式补记时就是静默翻倍。
                usage_recorded: bool,
                /// 上游**原始**字节的尾窗（≤8KB），只用来判「流内有没有 error 事件」。
                /// 必须存原始字节而不是翻译后的输出：翻译器会把它不认识的 error 事件整个丢掉
                /// （sse.rs 六个方向函数无一读 error），翻译后的流里根本查不到错误痕迹。
                raw_tail: Vec<u8>,
                /// 健康记账只做一次（Drop 与终止分支可能都会走到）。
                health_recorded: bool,
                store: std::sync::Arc<Store>,
                category: CategoryType,
                key_id: String,
                req_model: String,
                /// 发往上游的真实模型名。只用于流末按模型衰减第二层的模型锁
                /// （`req_model` 是对外名，锁的键不是它）。
                real_model: String,
            }

            // 挂在 Drop 上而不是在几个 `return None` 分支里各调一次：unfold 的累加器在
            // **任何**终止路径上都会被 drop —— 正常收完、上游报错、以及下游中途断开
            // （客户端 Ctrl+C，此时一个分支都不会执行）。前两种手写能覆盖，第三种漏不掉才是关键：
            // 那正是长回答被打断的场合，用户更想知道这次花了多少 token。
            impl<S> Drop for StreamState<S> {
                fn drop(&mut self) {
                    self.record_usage();
                    self.record_health();
                }
            }

            impl<S> StreamState<S> {
                /// 上游原始尾窗，只保留末 8KB（终止性 error 事件必在流末）。
                fn push_raw(&mut self, bytes: &[u8]) {
                    self.raw_tail.extend_from_slice(bytes);
                    if self.raw_tail.len() > TAIL_WINDOW_BYTES {
                        let drop_n = self.raw_tail.len() - TAIL_WINDOW_BYTES;
                        self.raw_tail.drain(0..drop_n);
                    }
                }

                /// 本次流内是否出现上游 error 事件（判据与同协议分支共用同一个函数）。
                fn saw_upstream_error(&self) -> bool {
                    crate::upstream::sse_stream_errored(&String::from_utf8_lossy(&self.raw_tail))
                }

                /// 流末按「有无流内 error」二选一记账。理由同同协议分支：拿到 200 响应头只代表
                /// 开流，早记成功会清零 fail_count 让后续失败补记永远够不到熔断阈值。
                /// 此前**跨协议这条路连失败检测都没有**（翻译器丢掉 error 后照常冲刷 completed，
                /// 下游拿到一条「成功完成的空回答」，健康态也被记成功）。
                fn record_health(&mut self) {
                    if self.health_recorded {
                        return;
                    }
                    self.health_recorded = true;
                    if self.saw_upstream_error() {
                        health::record_live_failure(&self.store, &self.key_id);
                    } else {
                        health::record_live_success(
                            &self.store,
                            &self.key_id,
                            Some(&self.real_model),
                        );
                    }
                }

                /// 流终止时把翻译器累积的用量补进「流开始时已写下的那一行」。
                fn record_usage(&mut self) {
                    if self.usage_recorded {
                        return;
                    }
                    self.usage_recorded = true;
                    let Some(u) = self.translator.accumulated_usage() else {
                        return; // 上游没给用量 —— 不造假数
                    };
                    // 不新追加事件：同 collapse key 再 append 会被折叠成「又发生了一次」
                    // （repeat 变 2、detail 被覆盖）。理由同同协议分支。
                    let collapse = format!("ok:{}:{}:{}", self.key_id, self.req_model, true);
                    self.store.backfill_usage_for_collapsed_event(
                        self.category,
                        Some(&self.key_id),
                        &collapse,
                        u,
                    );
                }
            }

            let init = StreamState {
                translator,
                upstream,
                finished: false,
                usage_recorded: false,
                raw_tail: Vec::new(),
                health_recorded: false,
                store: usage_store,
                category: usage_category,
                key_id: usage_key_id,
                req_model: usage_req_model,
                real_model: usage_real_model,
            };
            let translated = futures_util::stream::unfold(init, |mut st| async move {
                loop {
                    match st.upstream.next().await {
                        Some(Ok(bytes)) => {
                            // 先留一份原始字节用于判错（翻译器会丢掉 error 事件，之后就查不到了）
                            st.push_raw(&bytes);
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
                            // **上游流内报过 error 时不冲刷收尾事件**：`finish()` 会发
                            // `response.completed` / `message_stop` / `[DONE]`，而 error 事件本身
                            // 已被翻译器丢弃 —— 两者叠加会让下游拿到一条「状态 completed、内容为空
                            // 或被截断」的**假成功**，用户看到的是「模型什么都没说」而不是上游过载。
                            // 不发终止符，下游客户端会如实报「流未正常结束」，方向至少是对的。
                            // （更好的做法是翻成下游协议的 error 事件，需在 sse.rs 六个方向各加一条，
                            //  属独立改动，见 docs 待办。）
                            if st.saw_upstream_error() {
                                return None;
                            }
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

    let mut rb = Response::builder()
        .status(StatusCode::OK)
        .header("content-type", out_content_type)
        .header("cache-control", "no-cache");
    // 同协议字节透明透传时，把上游的 content-encoding 一并带给下游，让下游自己解压。
    // 跨协议**刻意不带**：那条路输出的是翻译器重新生成的明文事件，带上压缩标记会让下游
    // 去解压明文而失败。（真遇到「上游压缩 + 跨协议」时，翻译器本身就读不懂压缩字节，
    // 属另一个问题：应在源头用 accept-encoding: identity 避免，见 apply_upstream_headers。）
    if sse_dir.is_none() {
        if let Some(enc) = &upstream_content_encoding {
            rb = rb.header("content-encoding", enc.as_str());
        }
    }
    let response = rb
        .body(body)
        .map_err(|e| AppError::upstream_msg(e.to_string()))?;

    // 转换后发往上游的请求体快照（供调用模型日志核对 reasoning→thinking 等映射）。
    // 开关关闭时不构造——理由同 `forward_to_key` 内同名变量（三处同进同退）。
    let request_body = if req_log {
        serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string())
    } else {
        String::new()
    };
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
    let effort = match store.active_effort_of(category) {
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

/// Key 上配的采样参数（temperature / top_p）是否该注入本次请求。
///
/// **只有「补全端点」该注入**。Claude CLI / 桌面端做 token 计数会发
/// `POST /v1/messages/count_tokens`，body 只含 model/messages、无采样字段，
/// 同协议直通会原样发到上游的 count_tokens 端点 —— 而该端点的 schema **不含**
/// temperature/top_p，严格上游（Anthropic 官方）会以 400「extra inputs
/// not permitted」拒绝，客户端 token 计数功能失效。而 KeyEditor 默认就带
/// temperature=1.0，几乎每个 Key 都会触发，不是边角场景。
///
/// 判据用「路径**不含** count_tokens」而非「等于补全路径」：各端补全路径形态不同
/// （/v1/messages、/chat/completions、/responses，还可能带 ?beta= 等 query），
/// 用黑名单挡掉已知的非补全子路径最稳，将来新增补全端点也不会被误挡。
fn path_takes_sampling_params(path: &str) -> bool {
    !path.contains("count_tokens")
}

/// 为跨协议到 Anthropic 的请求补**协议必填**的 `max_tokens`，但只在客户端本来没给时。
///
/// 代理的总原则仍是「不替客户端决定输出长度」：
/// - 目标不是 Anthropic → 原样返回；
/// - 客户端已经给了上限 → 原样返回（即使值不理想，也不擅自覆盖）；
/// - 客户端没给且目标是 Anthropic → 这是协议唯一不允许省略的字段，按实际目标模型的
///   最大输出能力与窗口（若已有）计算一个合法值。
///
/// 计算放这里而不是 `convert.rs`：转换器只有 JSON/协议，没有 `ProviderKey` 和已经解析过的
/// `real_model`，在那层只能瞎填 4096；那正是审计发现的跨协议静默截断。
fn ensure_anthropic_output_budget(
    mut payload: Value,
    key: &ProviderKey,
    real_model: &str,
) -> AppResult<Value> {
    if key.protocol != Protocol::Anthropic
        || payload
            .get("max_tokens")
            .is_some_and(|value| !value.is_null())
    {
        // 提前返回也要清掉可能残留的中转字段：`_pending_effort` 绝不能发给上游。
        // 命中此分支的两种情况都不需要它——目标非 Anthropic（不会有该字段），
        // 或已带 max_tokens（转换时 thinking 已算过、不会暂存）。幂等清理，兜底而已。
        crate::upstream::strip_pending_effort(&mut payload);
        return Ok(payload);
    }
    let input_tokens = crate::upstream::estimate_json_tokens_without_image_transport(&payload);
    let max_tokens = crate::upstream::anthropic_required_max_tokens(
        real_model,
        key.context_window_of_real(real_model),
        input_tokens,
        key.max_output_of_real(real_model),
    )
    .map_err(AppError::Invalid)?;
    let obj = payload
        .as_object_mut()
        .ok_or_else(|| AppError::Invalid("请求体必须是 JSON 对象".into()))?;
    obj.insert("max_tokens".into(), Value::from(max_tokens));
    // max_tokens 补齐后，把转换时暂存的 reasoning.effort 落成 thinking（Codex→Anthropic
    // 跨协议扩展思考修复）。apply_pending_thinking 内部会移除 `_pending_effort` 中转字段，
    // 无暂存时是无副作用的清理。必须在插入 max_tokens **之后**——thinking 预算依赖它。
    crate::upstream::apply_pending_thinking(&mut payload, max_tokens as u64);
    Ok(payload)
}

/// 把 Key 上配置的采样参数（temperature / top_p）注入请求体。
///
/// **输出 token 上限刻意不在此列**（产品定调，2026-08-14）：代理相对 cc-switch 的增量只有
/// 路由与自动故障转移，**不替客户端决定它没要求过的输出长度**。客户端没发 `max_tokens`
/// 时，那是「由客户端/上游自己的默认值决定」，而不是「等着代理填一个」——
/// 此前用 Key 上的值（新建 Key 默认 8192）补进去，等于悄悄给每个请求加了个上限，
/// 用户看到长回答被截断只会去查上游，永远查不到是代理加的。
/// `KeyParams.max_tokens` 现在**没有任何请求路径读它**（后续 2026-08-15 那次定调把
/// 大脑聚合也改成按协议与模型上下文窗口现算了，见 `upstream/budget.rs`），仅兼容旧配置。
///
/// 客户端**显式**发的输出上限一律原样保留：同协议直通不动，跨协议由
/// `convert.rs` 负责改名（`max_output_tokens` ↔ `max_tokens` ↔ `max_completion_tokens`）。
/// 故障转移换到别的 Key 也不改——每个候选都从同一份原始 body 生成，
/// 不会出现「同一问题落到 A Key 完整、落到 B Key 被切断」。
///
/// **口径：只在下游未显式给出时注入**，与 [`inject_default_effort`] 的「已有则不覆盖」一致。
/// 客户端显式发的值代表用户当下的意图（如 Claude Code 针对某次对话调的 temperature），
/// 优先级高于 Key 上配的默认值；Key 参数的定位是「这个 Key 的缺省」，不是「强制覆盖」。
fn apply_key_params(payload: &mut Value, params: &crate::model::KeyParams) {
    let Some(obj) = payload.as_object_mut() else { return };

    // Anthropic 扩展思考（下游 body 已带 `thinking`）与采样参数互斥：开 thinking 时
    // Anthropic 要求 temperature 固定为 1、且不可有 top_p，否则 400（判据见 convert.rs:410/421，
    // 跨协议路径已按此归一）。此处是**同协议 Anthropic 直通**，没有那道归一化 ——
    // 若把 Key 上配的非 1 temperature / top_p 注进带 thinking 的请求，上游直接 400。
    // 故 thinking 在场时不碰采样字段。
    let has_thinking = obj.get("thinking").is_some_and(|t| !t.is_null());

    // 这里**故意没有 max_tokens 分支**：客户端没给输出上限就是没给，代理不代它决定。
    // 见本函数文档顶部那段产品定调；`params.max_tokens` 现在是纯兼容字段，无人读。

    // temperature / top_p 三协议同名。thinking 在场时**跳过**（见上，避免同协议直通 400）。
    for (name, val) in [("temperature", params.temperature), ("top_p", params.top_p)] {
        let Some(v) = val else { continue };
        if has_thinking {
            continue; // 扩展思考与采样参数互斥
        }
        if obj.get(name).is_some_and(|x| !x.is_null()) {
            continue; // 下游显式给了 → 尊重下游
        }
        // NaN / Inf 会得到 Null；插 null 上游多半 400，宁可当没配过。
        let encoded = f32_to_json(v);
        if !encoded.is_null() {
            obj.insert(name.into(), encoded);
        }
    }
}

/// `f32` → JSON number，**不带 f32→f64 拓宽噪声**。
///
/// `Value::from(0.2f32)` 会得到 `0.20000000298023224` —— 因为 0.2 在 f32 里本就是个近似值，
/// 直接拓宽成 f64 会把那串二进制误差如实展开。于是发给上游的 JSON 里躺着
/// `"temperature": 0.20000000298023224`，虽然数值上等价，但它会**原样出现在日志页的请求体快照里**，
/// 用户看到自己填的 0.2 变成一串小数会怀疑参数被篡改（本项目最不该制造的那类误导）。
///
/// 借 Rust 的 `f32` Display 给出「能往返的最短十进制表示」（0.2f32 → "0.2"），再按 f64 解析。
/// 与 KeyEditor 里 token 单位换算刻意走十进制字符串 + BigInt 是同一条纪律：
/// **人填进去的十进制，出去时还得是那个十进制**。
fn f32_to_json(v: f32) -> Value {
    v.to_string()
        .parse::<f64>()
        .ok()
        .and_then(serde_json::Number::from_f64)
        .map(Value::Number)
        // NaN / Inf 落这里：这两个 JSON 表示不了，宁可不注入也不发个 null 让上游 400。
        .unwrap_or(Value::Null)
}

/// 转发到单个 Key：套用模型映射 + 协议适配。
/// 返回完整 outcome（含发往上游的请求体、响应体、状态），供路由与调用模型日志共用。
/// 注意：非 2xx 不再直接返回 Err，而是照常返回 outcome（ok=false），
/// 由调用方决定是否切换——这样失败也能被完整记进调用模型日志。
// 同 `try_stream_to_key`：参数均为必需的运行时上下文，不为消警告做无收益的重构。
#[allow(clippy::too_many_arguments)]
async fn forward_to_key(
    store: &Arc<Store>,
    category: CategoryType,
    key: &ProviderKey,
    path: &str,
    // 见 `try_stream_to_key` 同名形参：`Cow` 让最后一个候选零拷贝移动，
    // 之前的候选传 Borrowed 由本函数自行克隆（保证候选间 body 不被污染）。
    req_json: std::borrow::Cow<'_, Value>,
    requested_model: &str,
    fwd_headers: &[(String, String)],
    // 调用模型日志开关（默认关）。由调用方传入而非在此重读 settings：
    // 一次转发里同一开关要被判多次，且调用方 `handle_request` 已在 `:449` 取过一次。
    // 传进来的唯一用途是决定「要不要构造那些只喂给日志的大字符串」——见下方 request_body。
    req_log: bool,
    // 故障转移剩余预算；None = 用户关闭了整体预算。与 Key 自身超时取**小**值：
    // 谁先到谁生效，既不让单个慢 Key 吃光整池预算，也不放宽用户为该 Key 设的上限。
    budget_left: Option<std::time::Duration>,
) -> AppResult<ForwardOutcome> {
    // 同 try_stream_to_key：走带解密缓存的 secret_for（P2-6）。
    // 密钥缺失同样判 `Invalid`（本地配置错误、永不自愈），理由见流式那处的完整说明。
    let secret = store
        .secret_for(&key.id)?
        .ok_or_else(|| {
            AppError::Invalid(format!(
                "Key「{}」没有可用的密钥：请在 Key 编辑器里填写 API 密钥并保存（若已启用主口令，请先解锁后重试）。",
                key.name
            ))
        })?;

    // 模型解析：映射 → 原生支持 → 默认兜底 → 第一个模型 → 透传（FR-006，见 resolve_model）
    let real_model = key.resolve_model(requested_model);

    // 判定下游请求协议（按 path），与目标 Key 协议做适配
    let downstream = downstream_protocol(path);

    // 响应侧翻译需要的两个工具集合**必须在消费 req_json 之前收集**（P2-5 第二步）。
    // 只在跨协议时才需要（同协议响应原样透传），而同协议是最常见路径——
    // 不能为这里的便利给它增加无谓工作。两者都是对下游 body 的纯读取（读 tools 声明），
    // 而下面只改 model / effort，故先收集与后收集结果一致。
    let resp_tool_sets = (downstream != key.protocol).then(|| {
        (
            crate::upstream::collect_custom_tools(&req_json),
            crate::upstream::collect_search_tools(&req_json),
            // namespaces：跨协议→Responses 时，回程 function_call 全名要按它拆成 name+namespace
            // 两字段，否则 Codex router 认不出 MCP 工具（unsupported call）。与流式路径同源收集。
            crate::upstream::collect_tool_namespaces(&req_json),
        )
    });

    // `into_owned()`：最后一个候选传的是 Owned（零拷贝移动），之前的候选传 Borrowed
    // （在此克隆，保证后续候选拿到未被污染的原始 body）。
    let mut payload = req_json.into_owned();
    if let Some(obj) = payload.as_object_mut() {
        obj.insert("model".into(), Value::String(real_model.clone()));
    }
    inject_default_effort(store, category, &mut payload, downstream, key.protocol);
    // 与 try_stream_to_key 同进同退：Key 参数注入必须两条路径都做，
    // 否则「非流式生效、流式不生效」这种按客户端而异的分叉极难归因。
    // 同样跳过 count_tokens 等非补全子路径（见 path_takes_sampling_params）。
    // 输出 token 上限不从 Key 补写；apply_key_params 只处理 temperature / top_p。
    if path_takes_sampling_params(path) {
        apply_key_params(&mut payload, &key.params);
    }

    // 跨协议转换（下游协议 → 上游 Key 协议；同协议时 convert_request 内部直接返回克隆）
    let payload = crate::upstream::convert_request_owned(payload, downstream, key.protocol);
    // 与流式路径同进同退：补 Anthropic 必填的 max_tokens（见那里的完整说明）。
    let payload = if path_takes_sampling_params(path) {
        ensure_anthropic_output_budget(payload, key, &real_model)?
    } else {
        // 兜底剥掉可能暂存的 `_pending_effort`（理由同流式路径 else 分支）。
        let mut payload = payload;
        crate::upstream::strip_pending_effort(&mut payload);
        payload
    };

    // 发往上游的请求体快照（pretty，方便页面阅读；密钥不在 body 里，安全）。
    //
    // **开关关闭时不构造**：与 `handle_request` 里 `downstream_body`（`:457`）同一理由——
    // 这份字符串唯一去向是 `ForwardOutcome.request_body`，而它的两个消费点
    // （`:766` 成功分支、`:790` 失败分支）都被 `req_log` 门控的 `log_success` / `log_request`
    // 吃掉。开关关着时那两个闭包直接 return，这里 pretty-print 一个可达十几万字符的
    // body（Codex 常态 100~200 KB，pretty 后约 1.3~1.6 倍）纯属白做，还要在失败分支
    // 被 clone 一次。`request_log_enabled` 默认 false，即绝大多数用户的绝大多数请求
    // 都在白做这件事。
    //
    // 历史：`downstream_body` 那处早先已加守卫，但**同类的这处与流式那处当时漏了**
    // （docs/14 效率整治表已就此勘误）。三处必须同进同退。
    let request_body = if req_log {
        serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string())
    } else {
        String::new()
    };

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

    // 用共享客户端（连接池复用），总超时按 Key 配置逐请求指定。
    let client = crate::upstream::shared_client();

    // 本次请求的超时 = min(Key 自身超时, 故障转移剩余预算)。
    // 取小值：既不让一个慢 Key 吃光整池预算（那会让后续候选无机会尝试），
    // 也不放宽用户为该 Key 设的上限。预算关闭时退化为纯 Key 超时（旧行为）。
    // 与流式探头阶段共用同一口径：`key_timeout` 是 per-Key 超时的**唯一事实来源**。
    // 别在这里重新写一遍 `unwrap_or(30_000)` —— 两处各写一遍就是「改了默认值只生效一半」
    // 的经典分叉（流式改了、非流式没改，或反之）。
    let key_to = crate::upstream::key_timeout(key);
    let effective_to = match budget_left {
        Some(b) => key_to.min(b),
        None => key_to,
    };
    // 请求头统一由 apply_upstream_headers 装齐（与流式路径共用同一实现，防两条路径分叉）。
    // **超时留在这里设**：非流式要等上游完整生成，语义与流式刻意不同，故不进公共函数。
    let rb = apply_upstream_headers(
        client.post(&url).json(&payload).timeout(effective_to),
        key,
        &secret,
        fwd_headers,
        &real_model,
    );

    // 连接层失败（DNS/超时/拒连）仍返回 Err，附带目标 URL 便于定位。
    let resp = rb
        .send()
        .await
        .map_err(|e| AppError::upstream_msg(format!("连接 {url} 失败: {e}")))?;
    let status = resp.status();
    // Retry-After 须在 resp.bytes() 消费响应体之前取（bytes() 拿走所有权）。
    let retry_after = parse_retry_after(resp.headers());
    let bytes = resp.bytes().await.map_err(|e| AppError::upstream_msg(e.to_string()))?;

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
                // 已在消费 req_json 之前收集好（见函数上方 resp_tool_sets）。
                // 本分支的条件 `!same_protocol` 与那里的收集条件是同一个，故必然是 Some。
                let (custom_tools, search_tools, namespaces) = resp_tool_sets
                    .clone()
                    .expect("跨协议分支下 resp_tool_sets 必然已收集");
                let translated = crate::upstream::convert_response_ext(
                    &v,
                    key.protocol,
                    downstream,
                    &custom_tools,
                    &search_tools,
                    &namespaces,
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
    // beta 头是 Anthropic 协议特有的；打到 OpenAI 系上游时不加（原值仍按既有行为透传）。
    // 走 Protocol 的**穷举**能力方法而非 `matches!(.., Anthropic)`：加第 4 种协议时
    // 编译器会要求在 supports_1m_beta 里明确回答「它支持不支持」，而不是被静默当成不支持。
    let want_1m = key.protocol.supports_1m_beta()
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
/// 请求级 4xx：错在**请求本身**（各 Key 打同样失败），与用哪个 Key 无关。
///
/// - `400` 请求不合法（客户端发的或我们协议转换构造的）；
/// - `422` 请求实体语义无效（OpenAI 兼容/中转站常用来表达 schema 校验失败）。
///
/// 这两个都**不该计入熔断**（换 Key 白试，还会把完好的 Key 刷成熔断→全熔断兜底），
/// 也**不是** Key 级硬错误。`failover_verb`、`status_counts_against_breaker` 共用它，
/// 避免「日志说非 Key 问题、熔断却罚 Key」这类同码两处定性相反（本轮审查确认的 422 矛盾）。
fn is_request_level_4xx(status: u16) -> bool {
    status == 400 || status == 422
}

/// 重试同一个只是白试，连续几次后熔断掉它，避免每个请求都从它开始。
///
/// ⚠️ **404 要配合 [`path_is_auxiliary_endpoint`] 一起判**，别单看状态码。见
/// [`failure_counts_against_breaker`]。
fn status_counts_against_breaker(status: u16) -> bool {
    // 只有「确定属于这个 Key」的硬错误才罚：与尾部 all_failed_is_hard_error 用同一套判据
    // （见 TRANSIENT_4XX 注释）——4xx 里除请求级（400/422，见 is_request_level_4xx）与
    // 408/409/429（超时/冲突/限流，临时）之外的部分（401/403/404… 鉴权、端点确与该 Key
    // 相关）。**5xx 一律不罚**：上游服务端错误或 Cloudflare 52x 与用哪个 Key 无关，
    // 等一等可能就好。此前用反向排除表把 408/409、505-511/520-527/530 都算进熔断，
    // 一次上游抖动/CDN 52x 就熔断好 Key 60s；且漏了 422（failover_verb 已判其为非 Key 问题）。
    (400..500).contains(&status)
        && !is_request_level_4xx(status)
        && !TRANSIENT_4XX.contains(&status)
}

/// **辅助端点**（非补全）：上游不实现它是常态，不该据此判定 Key 坏了。
///
/// 目前只有 token 计数一个。它是 Anthropic 官方 API 的辅助端点，
/// 而**绝大多数中转站不实现**（实测返回 `404 Invalid URL (POST /v1/messages/count_tokens)`）。
///
/// 与 [`path_takes_sampling_params`] 的判据刻意一致（都是「路径含 count_tokens」），
/// 但语义不同、故不复用同一个函数：那个回答「该注入采样参数吗」，
/// 这个回答「失败了该罚 Key 吗」。将来若两者的名单分叉（例如新增一个
/// 「要注参数但不算辅助」的端点），共用一个函数会让改动波及到不相干的那条判据。
fn path_is_auxiliary_endpoint(path: &str) -> bool {
    path.contains("count_tokens")
}

/// 本次失败是否该计入熔断（惩罚该 Key）。**状态码与请求路径一起判。**
///
/// 为什么必须带上路径（2026-08-14 真机复盘）：客户端除了发对话，还会发
/// `POST /v1/messages/count_tokens` 做 token 计数，而中转站普遍不实现该端点 → 404。
/// 旧实现只看状态码，于是每一次 token 计数都会：
///
/// 1. 打上游 → 404 → `record_live_failure` 给该 Key 累加 fail_count；
/// 2. 切下一个候选 → 同样 404（**所有**中转站都不实现它）；
/// 3. 遍历完全池 → 报「全部 Key 失败」并武装短路窗口；
/// 4. 连续几次后**整池 Key 全部熔断** → 「所有 Key 均在熔断窗口内」。
///
/// 而这些 Key 转发真实对话完全正常 —— 真机日志里同一个 Key 在 404 前后各有一条
/// 「成功返回」。用户视角就是「Key 明明能用，界面却说熔断、说无 Key 可用」。
///
/// 判据的本质：熔断要回答的是「**这个 Key** 还能不能服务」。辅助端点的 404 回答的是
/// 「**这个端点**上游没实现」——与用哪个 Key 无关，换任何 Key 都同样 404，
/// 与 400「请求不合法」属同一类，故同样只切不罚。
/// 「这次失败该罚 Key 吗」的布尔版。
///
/// 生产路径已改走 [`failure_scope`]（三个作用域，而不是一个 bool）。本函数保留为
/// **既有 4 条判据测试的入口**，并作为「Key 级」这一档的可读定义。
///
/// 标 `#[cfg(test)]` 的理由同 `health::is_candidate`：从编译期阻止有人把生产调用点切回
/// 这条**丢掉模型级作用域**的路径 —— 那种回退不报错，只是 404 又开始熔断整条 Key。
#[cfg(test)]
fn failure_counts_against_breaker(status: u16, path: &str) -> bool {
    matches!(failure_scope(status, path), FailureScope::Key)
}

/// 一次失败该罚到**哪一层**（借鉴 OmniRoute 的三层弹性作用域划分，
/// 见 `docs/architecture/RESILIENCE_GUIDE.md`）。
///
/// 这是把「熔断要回答什么问题」这句话落成类型：
/// - [`FailureScope::Key`] —— 「**这条 Key** 还能不能服务」。凭据级硬错误（401 等）。
/// - [`FailureScope::Model`] —— 「这条 Key 能不能跑**这个模型**」。范围小一级，
///   不该让整条 Key 停摆。
/// - [`FailureScope::None`] —— 谁都不罚。临时性（429/5xx）、请求级（400/422）、辅助端点。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailureScope {
    Key,
    Model,
    None,
}

/// 状态码 + 路径 → 罚哪一层。**单一事实来源**，别在 health.rs 里复制一份
/// （`TRANSIENT_4XX` 那条注释记着两处判据漂移过一次的代价）。
///
/// ## 为什么 404 归模型级而不是 Key 级（2026-08-23 改）
///
/// 补全端点上的 404 字面意思就是「这里没有这个模型/路由」。此前它走 Key 级：三次之后整条
/// Key 熔断 60 秒，**连它本来能服务的模型一起被挡住**。而中转站「某条 Key 的某个模型没开通」
/// 是最常见的一类失败形态。
///
/// 判据与既有的 [`path_is_auxiliary_endpoint`] 完全同源 —— 那条已经在说
/// 「`count_tokens` 的 404 回答的是**这个端点**上游没实现，与用哪个 Key 无关」。
/// 这里只是把同一句话往前推一步：补全端点的 404 回答的是**这个模型**上游没有，
/// 与这条 Key 的其它模型无关。
///
/// ## 为什么 401/403 仍归 Key 级
///
/// 401 是明确的凭据失效。403 有歧义（可能是「套餐不含该模型」，也可能是「Key 被封/IP 被拦」），
/// 但没有本项目自己的取证支撑把它划到模型级，所以**不猜** —— 保持原样。
/// 真要改，先拿到用户机器上中转站的实际响应体，别按推测改。
///
/// 一条 Key 上模型锁攒到阈值时会自动升级成 Key 级熔断
/// （`health::MODEL_LOCK_ESCALATE_AT`），所以「404 一切」的坏 Key 不会赖在池子里。
fn failure_scope(status: u16, path: &str) -> FailureScope {
    // 辅助端点（count_tokens）上游普遍不实现：谁都不罚。这条必须最先判 ——
    // 否则它的 404 会掉进下面的模型级分支，把一个完好模型锁 120 秒。
    if path_is_auxiliary_endpoint(path) {
        return FailureScope::None;
    }
    if status == 404 {
        return FailureScope::Model;
    }
    if status_counts_against_breaker(status) {
        return FailureScope::Key;
    }
    FailureScope::None
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
fn is_contentless_probe(req_json: &Value) -> bool {
    let Some(obj) = req_json.as_object() else {
        return false;
    };
    // 客户端**原始**是否指定了模型：读 body 里的 `model`，**不**读被 active_model 覆盖后的
    // requested_model。覆盖发生在转发前，若用覆盖后的值判定，用户一旦在应用内选定模型，
    // `model:null` 的空探测就会被填上模型名、绕过本短路 → 照样转发上游白耗一次往返 + 400 噪声
    // （熔断侧已由「400 不计熔断」兜住，故仅是效率/日志问题，P3）。
    let client_named_model = obj
        .get("model")
        .and_then(|m| m.as_str())
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    if client_named_model {
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

/// `POST /v1/messages/count_tokens` 的本地 token 估算。
///
/// Claude 桌面端在每次对话前调用该端点估算输入 token 数，决定是否截断/压缩上下文。
/// 中转站普遍不实现 → 大量 404 日志；高频调用还会触发 429 限流。故本地估算直接应答。
///
/// **必须走全 JSON 遍历**（`estimate_json_tokens_without_image_transport`），不能只挑
/// `text` 字段：agentic 会话里 Write/Edit 工具把整个文件内容装在 `tool_use.input`、
/// 扩展思考把大段文本装在 `thinking` 块 —— 首版手写遍历漏掉这两类，工具密集会话的
/// 估算值只有真实值的几分之一，客户端据此认为上下文充裕、不触发压缩，随后的真实补全
/// 直接被上游 400（prompt too long），且 400 不计熔断不换 Key，重试恒败无法自愈。
/// 全遍历会把 JSON 键名也计进去（轻微高估），对「决定何时压缩」这个用途而言，
/// 高估安全、低估致命 —— 与 budget.rs 对输入估算「取上界」的纪律一致。
/// 图片 base64 由该函数替换为固定视觉占位，不会把传输编码当文本膨胀计入。
fn estimate_count_tokens_local(req_json: &Value) -> u32 {
    crate::upstream::estimate_json_tokens_without_image_transport(req_json)
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
/// 硬错误回给下游的正文。
///
/// 两件事：
///
/// 1. **前缀按成因分流。** 请求级 4xx（400/422）说「全部 Key 不可用」是**假现场**：
///    错在请求本身，各 Key 打同样失败（这是 `is_request_level_4xx` 与 `failover_verb`
///    早就写明的定性 —— 后者的文案就是「请求被拒（非 Key 问题）」）。而下游那句
///    「全部 Key 不可用」会把用户直接送去查密钥、查额度、查中转站状态，
///    全是与本次失败无关的方向。
/// 2. 命中已知形态时附上可行动说明（见 [`annotate_known_upstream_error`]）。
///
/// 抽成纯函数才测得到：调用点在一个 700 行的 async 转发函数尾部。
fn compose_hard_error_body(last_status: Option<u16>, last_err: &str) -> String {
    let prefix = match last_status {
        Some(s) if is_request_level_4xx(s) => "请求被上游拒绝（非 Key 问题，换 Key 也一样）：",
        _ => "全部 Key 不可用：",
    };
    let mut body = format!("{prefix}{last_err}");
    if let Some(note) = annotate_known_upstream_error(last_err) {
        body.push_str(note);
    }
    body
}

/// 把上游那些**用户看不懂但成因确定**的错误，补一句可行动的解释。
///
/// 返回 `Some(附加说明)` 表示识别出了已知形态；`None` 表示不认识，原样透传。
///
/// 为什么值得有：本项目的错误文案纪律是「必须可行动」（已有先例：余额端点返回网页时给出
/// 三条排查路径、缺密钥时指向 Key 编辑器）。而上游透传上来的原文有时是**另一套系统的内部
/// 异常**，用户既看不懂、也无从判断该改什么。
///
/// ## 目前识别一种：扩展思考签名失效
///
/// 真机原文（经中转站脱敏后）：
/// ```text
/// {"__type":"***.***.***.runtimeservice#ValidationException",
///  "message":"***.***.content.6: Invalid `signature` in `thinking` block",
///  "reason":"THINKING_SIGNATURE_INVALID"}
/// ```
///
/// 成因（这一条是本项目**故障转移的固有代价**，不是 bug）：Claude 的扩展思考块带一个
/// 由**签发它的那个上游账号**签名的 `signature`，下一轮客户端把整段历史发回来时上游要验签。
/// 而 SynaRoute 会在 Key 之间做故障转移 —— 上一轮由 Key A（中转站 A → 某后端账号）签的
/// 思考块，这一轮若落到 Key B，B 验不了 A 的签名，直接 400。
///
/// **SynaRoute 自己不碰签名**（已逐处核对：同协议直通只改顶层 `model`/采样/`max_tokens`；
/// 跨协议只把 `thinking.budget_tokens` 映射成 `reasoning.effort`；响应侧从不伪造 thinking 块，
/// 全仓生产代码里没有一处构造 `"type":"thinking"`）。所以这不是转换丢字段，
/// 而是「同一段历史换了上游」这件事本身在 Anthropic 侧不被允许。
///
/// 故给出三条真正能解决的动作，而不是让用户去猜那句 coral 异常。
fn annotate_known_upstream_error(upstream_err: &str) -> Option<&'static str> {
    // 判据用**两种**写法各自匹配：`reason` 字段是机器码（最稳），message 是人类文案
    // （部分中转站只透传 message、丢掉 reason）。任一命中即可。
    let hit = upstream_err.contains("THINKING_SIGNATURE_INVALID")
        || (upstream_err.contains("thinking") && upstream_err.contains("signature"));
    if !hit {
        return None;
    }
    Some(
        "\n\n【SynaRoute 说明】这是**扩展思考签名**校验失败，不是密钥或额度问题。\n\
         Claude 的思考块带一个由「签发它的那个上游账号」签的签名，下一轮把历史发回去时上游要验签；\n\
         而故障转移换了 Key 之后，新上游验不了旧上游签的名 —— 于是整段历史被拒。\n\
         可行动的三条：\n\
         • **开一个新会话**（最快：历史里没有旧签名就不会再撞）；\n\
         • 把这个会话**固定在一条 Key** 上（分类页里只启用一条，或把它设为主 Key 且暂时停用其余）；\n\
         • 或在客户端**关掉扩展思考**（没有思考块就没有签名要验）。\n\
         注：SynaRoute 不改写思考块，也不会伪造签名 —— 转发时它是原样透传的。",
    )
}

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

/// 拼装成功转发的日志 detail 行。
///
/// 抽成独立函数只为可测：`log_success` 是个捕获了 `category`/`req_log`/`requested_model`
/// 的闭包，测不到；而这行文本是**用户默认配置下唯一能看到的信息**，两个字段靠它才可见：
///
/// - `usage`：token 用量。判断额度花在哪的唯一入口。
/// - `was_truncated`：上游按 `max_tokens` 截断。这是最需要被告知的一类异常——
///   它不报错、不失败、不触发重试，日志是一条正常的绿色「成功返回」，
///   只有答案莫名少了后半截。此前它**只**写进链路快照，而快照默认关闭、
///   且要展开某一行才看得到，等于在默认配置下完全不可见。
fn fmt_success_detail(
    key_name: &str,
    streaming: bool,
    model_part: &str,
    elapsed_ms: u64,
    was_truncated: Option<bool>,
    usage: Option<&crate::upstream::TokenUsage>,
) -> String {
    let verb = if streaming { "流式返回" } else { "成功返回" };
    let usage_part = usage.map(|u| format!(" · {}", u.fmt_compact())).unwrap_or_default();
    // 只有确定被截断（Some(true)）才标记：None 表示无法检测（流式直通时边收边发，
    // 不解析完整响应），把它当成「没截断」展示是对的，当成「截断」会满屏假警。
    let truncated_part = if was_truncated == Some(true) {
        " · ⚠ 被上游截断（达最大输出上限）"
    } else {
        ""
    };
    format!("{key_name} · {verb} · {model_part} · {elapsed_ms}ms{usage_part}{truncated_part}")
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
    // 生产段已改走 lan_guard::guarded（那层负责鉴权），故 service_fn 只剩测试在用。
    use hyper::service::service_fn;
    use crate::model::UserPrefs;
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
        // 408/409 与「其它 5xx」（505-511 / Cloudflare 520-527 / 530）是本轮全业务审查修的回归：
        // 尾部把它们判临时(→529)，熔断却曾用反向排除表把它们算进惩罚，一次超时/CDN 52x
        // 就把完好的 Key 熔断 60s。现在与尾部共用 TRANSIENT_4XX，全部只切不罚。
        for s in [
            408u16, 409, 429, 500, 502, 503, 504, 505, 508, 509, 511, 520, 524, 527, 529, 530,
        ] {
            assert!(!status_counts_against_breaker(s), "HTTP {s} 属临时/上游侧，不应计入熔断");
        }
        // 请求级 4xx（400 不合法 / 422 实体语义无效）与 Key 无关（换任何 Key 都同样失败）：
        // 不得因客户端空探测、我们自己的协议转换 bug、或 schema 校验失败把完好的 Key 刷成熔断。
        for s in [400u16, 422] {
            assert!(
                !status_counts_against_breaker(s),
                "HTTP {s} 是请求问题、不是 Key 问题，不应计入熔断"
            );
        }
        // 确定属于该 Key 的故障：鉴权失败 / 端点或模型不存在 → 计入熔断。
        for s in [401u16, 403, 404] {
            assert!(status_counts_against_breaker(s), "HTTP {s} 应计入熔断");
        }
        // 同码定性必须一致：failover_verb 判为「非 Key 问题」的码，绝不能同时被熔断惩罚
        // （本轮审查发现的 422 矛盾：日志说非 Key 问题、熔断却罚 Key）。
        for s in [400u16, 422] {
            assert!(
                failover_verb(s).contains("非 Key 问题") && !status_counts_against_breaker(s),
                "HTTP {s} 在 failover_verb 与熔断判据里定性必须一致（都视为请求级、不罚 Key）"
            );
        }
    }

    /// 辅助端点（token 计数）的失败**绝不能**计入熔断。
    ///
    /// 真机复盘（2026-08-14）：客户端每做一次 token 计数就发
    /// `POST /v1/messages/count_tokens`，而中转站普遍不实现它 → 404。旧实现只看状态码，
    /// 于是每次计数都把**整池 Key** 逐个刷上 fail_count、遍历完报「全部 Key 失败」，
    /// 几次之后所有 Key 熔断 → 界面显示「无 Key 可用」，而同一批 Key 转发真实对话完全正常
    /// （日志里 404 前后各有一条「成功返回」）。
    ///
    /// 判据的本质：熔断回答「这个 **Key** 还能不能服务」，而辅助端点的 404 回答的是
    /// 「这个 **端点** 上游没实现」——换任何 Key 都同样 404。
    #[test]
    fn auxiliary_endpoint_404_never_penalizes_the_key() {
        // 各端真实发出的 token 计数路径（含带 query 的形态）。
        for p in ["/v1/messages/count_tokens", "/v1/messages/count_tokens?beta=true"] {
            assert!(path_is_auxiliary_endpoint(p), "{p:?} 应被识别为辅助端点");
            // 404 是这条路径的常态（上游没实现），绝不能罚 Key。
            assert!(
                !failure_counts_against_breaker(404, p),
                "{p:?} 的 404 是「上游没实现该端点」，不得计入熔断"
            );
            // 连鉴权失败也不罚：辅助端点整体不作为 Key 健康度的判据，
            // 否则「上游对该端点单独鉴权」之类的实现差异又会把好 Key 刷红。
            assert!(
                !failure_counts_against_breaker(401, p),
                "辅助端点的任何失败都不该作为 Key 健康度判据"
            );
        }

        // 补全端点的 404 归**模型级**，不再罚 Key（2026-08-23 分层，见 `failure_scope`）。
        //
        // 这条断言的方向此前是相反的（「仍应计入熔断」）。改的理由：补全端点 404 的字面
        // 意思是「这里没有这个模型」，而中转站「某条 Key 的某个模型没开通」是最常见的
        // 一类失败 —— 旧语义下三次之后整条 Key 停摆 60 秒，连它本来能服务的模型一起误伤。
        // 与上面辅助端点那条是同一句话再往前推一步：404 回答的是「这个模型/端点」有没有，
        // 不是「这条 Key」还能不能用。
        for p in ["/v1/messages", "/v1/chat/completions", "/v1/responses"] {
            assert!(!path_is_auxiliary_endpoint(p), "{p:?} 是补全端点，不是辅助端点");
            assert_eq!(
                failure_scope(404, p),
                FailureScope::Model,
                "{p:?} 的 404 应只锁这个模型，不该熔断整条 Key"
            );
            assert!(
                !failure_counts_against_breaker(404, p),
                "{p:?} 的 404 不得再计入 Key 级熔断"
            );
            // 但 401 仍是 Key 级：那是明确的凭据失效，与模型无关。
            assert_eq!(
                failure_scope(401, p),
                FailureScope::Key,
                "{p:?} 的 401 是凭据问题，必须仍罚 Key（否则坏密钥永远切不走）"
            );
        }

        // 辅助端点的 404 必须仍归 None —— **不能**掉进上面那条模型级分支，
        // 否则 count_tokens 的常态 404 会把一个完好模型锁 120 秒。
        for p in ["/v1/messages/count_tokens", "/v1/messages/count_tokens?beta=true"] {
            assert_eq!(
                failure_scope(404, p),
                FailureScope::None,
                "{p:?} 的 404 是「上游没实现该端点」，谁都不该罚"
            );
        }

        // 临时性状态码在两类路径上都不罚（原有语义不变）。
        for p in ["/v1/messages", "/v1/messages/count_tokens"] {
            for s in [429u16, 500, 502, 503, 504, 529, 400] {
                assert!(
                    !failure_counts_against_breaker(s, p),
                    "HTTP {s} 在 {p:?} 上不应计入熔断"
                );
            }
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
    /// Key 上配的 temperature / top_p 必须真的进请求体。
    ///
    /// 钉住的是一条**静默失效**：`temperature` / `top_p` 曾在整个后端零使用
    /// （只有 `timeout_ms` 被读），而 UI 能填、能存。
    /// 删掉 `apply_key_params` 的调用，本测试必须变红。
    #[test]
    fn key_params_are_injected_into_payload() {
        let params = crate::model::KeyParams {
            temperature: Some(0.2),

            top_p: Some(0.9),
            timeout_ms: Some(30_000),
        };

        for body in [
            json!({ "model": "m", "messages": [] }),
            json!({ "model": "m", "input": [] }),
        ] {
            let mut payload = body;
            apply_key_params(&mut payload, &params);
            assert_eq!(payload["temperature"], 0.2, "应注入 temperature");
            assert_eq!(payload["top_p"], 0.9, "应注入 top_p");
        }
    }

    /// 输出 token 上限**绝不由代理凭空补写**（产品定调 2026-08-14）。
    ///
    /// 钉住的是一条会被误当成上游问题的行为：客户端（Claude Code / Codex）没发输出上限时，
    /// 代理曾用 Key 上的 `max_tokens` 补一个进去 —— 而 KeyEditor 新建 Key 默认就是 8192，
    /// 于是几乎每个用户的每个请求都被悄悄加了上限，长回答被截断只会去查中转商。
    /// 相对 cc-switch，本代理的增量只有路由与故障转移，不含「替客户端决定输出长度」。
    ///
    /// 若有人把 max_tokens 注入加回 `apply_key_params`，本测试必须变红。
    #[test]
    fn key_max_tokens_is_never_injected_by_proxy() {
        let params = crate::model::KeyParams {
            temperature: Some(0.2),

            top_p: Some(0.9),
            timeout_ms: Some(30_000),
        };

        // 两种下游 body 形态（Anthropic/Chat 的 messages、Responses 的 input）都不得出现
        // 代理新增的输出上限 —— 三个可能的字段名一个都不许冒出来。
        //
        // 本函数**不再按协议分支**（这正是本次改动的一部分：没有按协议选字段名这回事了），
        // 故这里不传协议、也不必枚举三种协议 —— 覆盖两种 body 形态即穷尽了输入空间。
        for body in [
            json!({ "model": "m", "messages": [] }),
            json!({ "model": "m", "input": [] }),
        ] {
            let mut payload = body;
            apply_key_params(&mut payload, &params);
            for name in ["max_tokens", "max_output_tokens", "max_completion_tokens"] {
                assert!(
                    payload.get(name).is_none(),
                    "代理不得凭空写入 {name}（客户端没要求过输出上限）"
                );
            }
            // 但采样参数照旧注入，证明「没写 max_tokens」不是因为整个函数没跑。
            assert_eq!(payload["temperature"], 0.2);
        }
    }

    /// count_tokens 等非补全子路径**不得**注入采样参数。
    ///
    /// 钉住一条 P2：Claude CLI/桌面端做 token 计数发 /v1/messages/count_tokens，
    /// 该端点 schema 不含 temperature/top_p，而 KeyEditor 默认就带 temperature ——
    /// 若无条件注入，同协议直通会把它原样发到 count_tokens 端点，严格上游 400。
    /// 判据函数是 `path_takes_sampling_params`；转发路径据它决定调不调 apply_key_params。
    #[test]
    fn count_tokens_path_excluded_from_sampling_params() {
        assert!(
            !path_takes_sampling_params("/v1/messages/count_tokens"),
            "count_tokens 子路径不应注入采样参数（上游会 400）"
        );
        assert!(
            !path_takes_sampling_params("/v1/messages/count_tokens?beta=true"),
            "带 query 的 count_tokens 同样要挡"
        );
        // 补全端点仍应注入（否则 Key 参数又变回从不生效）。
        assert!(path_takes_sampling_params("/v1/messages"));
        assert!(path_takes_sampling_params("/v1/chat/completions"));
        assert!(path_takes_sampling_params("/v1/responses"));
    }

    /// count_tokens 本地估算**必须计入** `tool_use.input` 与 `thinking` 块。
    ///
    /// 钉住一条 P2：agentic 会话里 Write/Edit 把整个文件内容装在 tool_use.input，
    /// 首版手写遍历只挑 `text` 字段 → 工具密集会话估算值只有真实值的几分之一 →
    /// 客户端不触发压缩 → 真实补全被上游 400（prompt too long）且重试恒败。
    /// 判据：含大段 tool_use.input / thinking 的请求，估算值必须显著大于「只有 text」的版本。
    #[test]
    fn count_tokens_estimate_includes_tool_use_input_and_thinking() {
        let big = "x".repeat(40_000); // 模拟一个被 Write 进 input 的文件（≈1 万 token）
        let text_only = json!({
            "model": "claude-sonnet-4-5",
            "messages": [{ "role": "user", "content": "hi" }]
        });
        let with_tool_use = json!({
            "model": "claude-sonnet-4-5",
            "messages": [
                { "role": "user", "content": "hi" },
                { "role": "assistant", "content": [
                    { "type": "thinking", "thinking": big },
                    { "type": "tool_use", "id": "t1", "name": "write_file",
                      "input": { "path": "a.rs", "content": big } }
                ] }
            ]
        });
        let base = estimate_count_tokens_local(&text_only);
        let full = estimate_count_tokens_local(&with_tool_use);
        assert!(
            full > base + 15_000,
            "tool_use.input 与 thinking 必须计入估算（base={base} full={full}）——\
             漏计会让客户端不压缩上下文、随后真实请求被上游 400"
        );
        // 图片 base64 是传输编码不是文本，必须仍被有界占位替换（复用 budget.rs 的防线）。
        let with_image = json!({
            "model": "claude-sonnet-4-5",
            "messages": [{ "role": "user", "content": [
                { "type": "image", "source": { "type": "base64", "media_type": "image/png",
                  "data": "A".repeat(6_000_000) } }
            ] }]
        });
        assert!(
            estimate_count_tokens_local(&with_image) < 20_000,
            "图片 base64 不得按文本膨胀计入"
        );
    }

    /// 扩展思考（thinking）在场时不注入 temperature / top_p —— 同协议 Anthropic 直通
    /// 没有跨协议那道「开 thinking 归一 temperature=1、去 top_p」，注了非 1 值上游会 400。
    #[test]
    fn key_params_skip_sampling_when_thinking_present() {
        let params = crate::model::KeyParams {
            temperature: Some(0.2),

            top_p: Some(0.9),
            timeout_ms: None,
        };
        // 客户端发了 thinking 但没带 temperature/top_p —— 正是会被 Key 默认值注入的空档。
        let mut payload = json!({
            "model": "m",
            "messages": [],
            "thinking": { "type": "enabled", "budget_tokens": 2048 }
        });
        apply_key_params(&mut payload, &params);
        assert!(
            payload.get("temperature").is_none(),
            "thinking 在场时不得注入 temperature（Anthropic 会 400）"
        );
        assert!(
            payload.get("top_p").is_none(),
            "thinking 在场时不得注入 top_p"
        );
        // 输出上限也不补：thinking 请求同样由客户端自己定长度。
        assert!(
            payload.get("max_tokens").is_none(),
            "代理不得凭空写入 max_tokens"
        );
    }

    /// 下游显式给了值 → 尊重下游，不覆盖（与 inject_default_effort 同口径）。
    ///
    /// 客户端显式发的输出上限也在此钉住：三种名字都必须**原值原名**留在 body 里
    /// （同协议直通不动，跨协议由 convert.rs 改名），代理既不覆盖也不追加第二个名字。
    #[test]
    fn key_params_do_not_override_explicit_downstream_values() {
        let params = crate::model::KeyParams {
            temperature: Some(0.2),

            top_p: Some(0.9),
            timeout_ms: None,
        };
        let mut payload = json!({
            "model": "m", "messages": [],
            "max_tokens": 512, "temperature": 1.5, "top_p": 0.1
        });
        apply_key_params(&mut payload, &params);
        assert_eq!(payload["max_tokens"], 512, "客户端显式值优先");
        assert_eq!(payload["temperature"], 1.5);
        assert_eq!(payload["top_p"], 0.1);

        // 客户端用别名 / Responses 名字时，同样原封不动，且不会多冒出一个同义字段。
        for (given, other_names) in [
            ("max_completion_tokens", ["max_tokens", "max_output_tokens"]),
            ("max_output_tokens", ["max_tokens", "max_completion_tokens"]),
        ] {
            let mut payload = json!({ "model": "m", "messages": [] });
            payload[given] = json!(256);
            apply_key_params(&mut payload, &params);
            assert_eq!(payload[given], 256, "{given} 应原值保留");
            for name in other_names {
                assert!(
                    payload.get(name).is_none(),
                    "已有 {given} 时不应再出现 {name}"
                );
            }
        }

        // 未配置的字段不该凭空造出来。
        let none = crate::model::KeyParams {
            temperature: None,

            top_p: None,
            timeout_ms: None,
        };
        let mut payload = json!({ "model": "m", "messages": [] });
        apply_key_params(&mut payload, &none);
        assert!(payload.get("max_tokens").is_none());
        assert!(payload.get("temperature").is_none());
        assert!(payload.get("top_p").is_none());
    }

    /// 用户填的十进制，发出去还得是那个十进制。
    ///
    /// 回归护栏：`Value::from(0.2f32)` 会得到 `0.20000000298023224`（f32→f64 拓宽把
    /// 二进制近似误差展开了）。数值上无害，但它会**原样出现在日志页的请求体快照里** ——
    /// 用户看到自己填的 0.2 变成一长串小数，会以为参数被程序改过。
    #[test]
    fn key_params_keep_decimal_fidelity() {
        let params = crate::model::KeyParams {
            temperature: Some(0.2),

            top_p: Some(0.95),
            timeout_ms: None,
        };
        let mut payload = json!({ "model": "m", "messages": [] });
        apply_key_params(&mut payload, &params);

        // 比字符串而非比数值：0.20000000298023224 == 0.2 在 f64 比较下是 false，
        // 但真正要钉的是「序列化出去长什么样」。
        assert_eq!(payload["temperature"].to_string(), "0.2");
        assert_eq!(payload["top_p"].to_string(), "0.95");
    }

    /// NaN / Inf 不可序列化成 JSON，宁可当没配过，也不要插个 null 让上游 400。
    #[test]
    fn key_params_skip_non_finite_values() {
        let params = crate::model::KeyParams {
            temperature: Some(f32::NAN),

            top_p: Some(f32::INFINITY),
            timeout_ms: None,
        };
        let mut payload = json!({ "model": "m", "messages": [] });
        apply_key_params(&mut payload, &params);
        assert!(payload.get("temperature").is_none(), "NaN 不应注入");
        assert!(payload.get("top_p").is_none(), "Inf 不应注入");
    }

    #[test]
    fn effort_injection_reads_requesting_category_not_hardcoded_codex() {
        let dir = temp_dir("effort_cat");
        let store = std::sync::Arc::new(
            Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap(),
        );
        // active_efforts 是后端自管字段：save_settings 会刻意剥掉入参里的它（防前端旧快照
        // 顶回用户刚选的值），故必须走专用写入方法 set_active_effort。
        store.set_active_effort(CategoryType::Codex, "high").unwrap();
        store.set_active_effort(CategoryType::ClaudeDesktop, "low").unwrap();
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

    /// `stop_all` 停净所有分类并如实计数；且它读的是**当前**运行集合，不受调用顺序影响。
    ///
    /// 这是退出优雅停机的实质。配套的**顺序不变量**（先快照 `is_running` 再 `stop_all`）
    /// 在退出处理块里，靠本测试保证 `stop_all` 后 `is_running` 全为 false ——
    /// 若谁把退出块的两步调换，快照就会读到空集、FR-029 下次啥都不恢复。
    #[tokio::test]
    async fn stop_all_stops_every_running_category_and_counts() {
        let dir = temp_dir("stop_all");
        let store = std::sync::Arc::new(
            Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap(),
        );
        let pm = ProxyManager::new(store.clone());

        // 起两个分类（各自绑不同默认端口，互不冲突）。
        pm.start(CategoryType::ClaudeCli).await.unwrap();
        pm.start(CategoryType::Codex).await.unwrap();
        assert!(pm.is_running(CategoryType::ClaudeCli));
        assert!(pm.is_running(CategoryType::Codex));

        let stopped = pm.stop_all();
        assert_eq!(stopped, 2, "两个在跑的分类都应被停掉并计入");
        assert!(!pm.is_running(CategoryType::ClaudeCli), "stop_all 后不得有残留");
        assert!(!pm.is_running(CategoryType::Codex));
        assert!(!pm.is_running(CategoryType::ClaudeDesktop));

        // 幂等：全停之后再调返回 0，不 panic。
        assert_eq!(pm.stop_all(), 0, "无在跑分类时应返回 0");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn contentless_probe_detected_and_real_requests_spared() {
        // 回归护栏（用户日志实证：68 条失败请求近半是这种空探测）：
        // 桌面端发 {"messages":[],"model":null} → 转发上游必然 400 → 400 被判硬错误计入熔断
        // → 把完好的 Key 刷成熔断 → 触发全熔断兜底反复重打。必须在代理侧直接拒绝。

        // 1) 用户真实抓到的形态：空 messages + model:null（客户端未指定模型）。
        let probe = json!({"max_tokens":4096,"messages":[],"model":null});
        assert!(
            is_contentless_probe(&probe),
            "用户实测的空探测形态必须被识别"
        );

        // 2) Responses 协议的空 input 同样是探测。
        assert!(is_contentless_probe(&json!({"input":[]})));

        // ---- 以下都不得误伤 ----

        // 3) 有真实 messages 但无 model：count_tokens 等合法子路径，必须放行。
        let count_tokens = json!({"messages":[{"role":"user","content":"hi"}]});
        assert!(
            !is_contentless_probe(&count_tokens),
            "带真实消息的无 model 请求（count_tokens）不得被拒"
        );

        // 4) 空 messages 但客户端**原始 body** 指定了模型：不是探测（交由上游判定），放行。
        assert!(
            !is_contentless_probe(&json!({"messages":[],"model":"claude-opus-4-8"})),
            "已指定模型的请求不得被判为探测"
        );

        // 4b) 回归（全业务审查）：判定必须基于**客户端原始** body 的 model，而非被 active_model
        //     覆盖后的 requested_model。否则用户在应用内选定模型后，`model:null` 的空探测会被
        //     填上模型名、绕过本短路照样转发上游。故 body 里 model 为 null/缺失即算「无模型」，
        //     与代理是否配了 active_model 无关。
        assert!(
            is_contentless_probe(&json!({"messages":[],"model":null})),
            "空 messages + body model 为 null：无论代理是否覆盖了 active_model，都应判为探测"
        );

        // 5) 字符串形态的非空 input。
        assert!(!is_contentless_probe(&json!({"input":"hello"})));

        // 6) body 解析失败（Value::Null）：不是探测，交既有路径处理，避免误报。
        assert!(
            !is_contentless_probe(&Value::Null),
            "非对象 body 不应被判为空探测"
        );

        // 7) 完全无载荷键的请求（未来新端点）：不判探测，避免误伤。
        assert!(
            !is_contentless_probe(&json!({"foo":1})),
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
            balance_query: None,
            cached_balance: None,
            cost_multiplier: None,
            icon: None,
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
                    max_output_tokens: None,
        }
    }

    /// OpenAI 客户端省略上限、跨协议转到 Anthropic Key 时：代理必须补**模型能力值**，
    /// 不能让 convert.rs 凭空回退 4096。此函数是两个转发路径（流式/非流式）共用的最后防线。
    #[test]
    fn cross_protocol_anthropic_budget_uses_model_cap_not_4096() {
        let mut k = key("k", 0, "https://example.com");
        k.models = vec![model_ctx("claude-sonnet-4-5", Some(200_000))];
        let original = json!({
            "model": "claude-sonnet-4-5",
            "messages": [{ "role": "user", "content": "hi" }]
        });
        // 模拟 OpenAI→Anthropic 转换后仍缺字段的状态。
        let converted = crate::upstream::convert_request_owned(
            original,
            Protocol::OpenaiChat,
            Protocol::Anthropic,
        );
        assert!(converted.get("max_tokens").is_none(), "转换层不应猜 4096");
        let final_body = ensure_anthropic_output_budget(converted, &k, "claude-sonnet-4-5")
            .expect("代理握有 Key/模型能力数据，必须能补 Anthropic 必填字段");
        assert_eq!(final_body["max_tokens"], 64_000);
        assert_ne!(final_body["max_tokens"], 4096);
    }

    /// 客户端明确给出的上限必须优先，即使目标协议为 Anthropic 也不得被能力计算覆盖。
    #[test]
    fn cross_protocol_anthropic_budget_preserves_client_cap() {
        let mut k = key("k", 0, "https://example.com");
        k.models = vec![model_ctx("claude-sonnet-4-5", Some(200_000))];
        let payload = json!({ "model": "claude-sonnet-4-5", "max_tokens": 1234, "messages": [] });
        let final_body = ensure_anthropic_output_budget(payload, &k, "claude-sonnet-4-5").unwrap();
        assert_eq!(final_body["max_tokens"], 1234);
    }

    /// P2-2 端到端：Codex(有 effort、无 max_tokens) → Anthropic Key 的完整链路里，
    /// 扩展思考必须**真的生效**——convert 暂存 effort、ensure_anthropic_output_budget 补完
    /// max_tokens 后落成 thinking。此前 thinking 映射依赖 max_tokens 存在，而 Codex 常态
    /// 不发它，导致扩展思考被静默丢弃（本项目主场景「Codex 接 Claude 中转」直接受损）。
    ///
    /// 走两个真实函数（convert_request_owned + ensure_anthropic_output_budget），
    /// 与转发路径 proxy.rs:2026-2029 完全同序，故这条覆盖的是链路而非孤立函数。
    #[test]
    fn codex_effort_survives_full_budget_path_to_anthropic() {
        let mut k = key("k", 0, "https://example.com");
        k.models = vec![model_ctx("claude-opus-4-5", Some(200_000))];
        // Codex 常态：reasoning.effort 有，但不发 max_tokens
        let original = json!({
            "model": "claude-opus-4-5",
            "messages": [{ "role": "user", "content": "hi" }],
            "reasoning": { "effort": "high" }
        });
        let converted = crate::upstream::convert_request_owned(
            original,
            Protocol::OpenaiChat,
            Protocol::Anthropic,
        );
        // 转换后：max_tokens 还没补，thinking 也还没落，但 effort 已暂存
        assert!(converted.get("thinking").is_none(), "此刻不该有 thinking");
        assert_eq!(converted["_pending_effort"], "high", "effort 必须已暂存");

        let final_body = ensure_anthropic_output_budget(converted, &k, "claude-opus-4-5")
            .expect("应能补 max_tokens 并落 thinking");
        assert_eq!(final_body["max_tokens"], 64_000, "按模型能力补 max_tokens");
        assert_eq!(final_body["thinking"]["type"], "enabled", "扩展思考必须真的生效");
        assert!(final_body["thinking"]["budget_tokens"].as_u64().unwrap() > 0);
        assert_eq!(final_body["temperature"], 1, "开思考须归一 temperature=1");
        assert!(
            final_body.get("_pending_effort").is_none(),
            "中转字段必须已被清除，绝不能发给上游"
        );
    }

    /// 认不出的第三方模型名**不再让整轮请求失败**，而是补上兜底 `max_tokens` 照常转发。
    ///
    /// ## 契约在 2026-08-21 反了过来（用户明确要求），这里记下判据
    ///
    /// 旧行为：内置表认不出 → 返回 `AppError::Invalid` → 400 + 引导用户去
    /// 「Key 编辑器 → 模型列表」手填「最大单次输出」。初衷是「不许猜」。
    ///
    /// 实测代价过高：中转站的私有模型名千变万化（`gpt-5.6-sol`、站点自定义别名），
    /// 内置表永远追不上，于是**用户每加一个 Key 都撞一次「全部 Key 不可用」** ——
    /// 一个本该开箱可用的软件变成「先查文档填数字才能用」。
    ///
    /// 反转的安全依据是两种错法后果**不对称**（见 budget.rs 同名测试的完整论证）：
    /// 填大了是硬 400（请求发不出去），填小了只在回答超长时截断，而截断现在**可见**
    /// （`was_truncated` 进日志、`stop_reason: max_tokens` 跨协议透传）。
    ///
    /// 这条测试守的是**端到端**那一段：转换层确实把兜底值写进了 payload。
    /// budget.rs 那条守取值逻辑，两者缺一都会让「Anthropic 必填 max_tokens」漏掉。
    ///
    /// 故障注入判据：把 `anthropic_required_max_tokens` 的兜底支去掉 → 这里回 Err，断言立刻红。
    #[test]
    fn unknown_model_gets_fallback_budget_instead_of_failing_request() {
        let mut k = key("k", 0, "https://example.com");
        // 内置表认不出的第三方模型名 + 用户没填 max_output + 无 context_window
        k.models = vec![model_ctx("some-relay-private-model", None)];
        let payload = json!({
            "model": "some-relay-private-model",
            "messages": [{ "role": "user", "content": "hi" }]
        });
        let out = ensure_anthropic_output_budget(payload, &k, "some-relay-private-model")
            .expect("认不出的模型名必须兜底放行，不能把用户挡在门外");
        // Anthropic 协议下 max_tokens 必填：兜底值必须真的写进去，否则上游 400。
        assert_eq!(
            out["max_tokens"], 8_192,
            "应补上兜底 max_tokens（8192 = 所有主流模型都支持的下限，绝不触发 400）"
        );
    }

    /// 用户手填的「最大单次输出」仍然**优先于**兜底值 —— 兜底不该埋掉用户的显式设置。
    ///
    /// 与上一条配对：上一条证明「不填也能用」，这条证明「填了就照你说的」。
    /// 只有上一条时，一次把兜底逻辑写成无条件覆盖的改动不会被任何测试拦住。
    #[test]
    fn user_supplied_max_output_still_wins_over_fallback() {
        let mut k = key("k", 0, "https://example.com");
        let mut m = model_ctx("some-relay-private-model", None);
        m.max_output_tokens = Some(32_000);
        k.models = vec![m];
        let payload = json!({
            "model": "some-relay-private-model",
            "messages": [{ "role": "user", "content": "hi" }]
        });
        let out = ensure_anthropic_output_budget(payload, &k, "some-relay-private-model")
            .expect("填了最大输出必须可用");
        assert_eq!(
            out["max_tokens"], 32_000,
            "用户显式填的值必须胜过兜底 8192，否则等于静默忽略用户设置"
        );
    }

    #[test]
    fn all_failed_classification_config_error_must_not_stick_across_candidates() {
        // 回归（本轮对抗式复核发现）：config_error 曾是「只置不复位」的粘滞标志——
        // 只在两个 Err(Invalid) 分支被赋值，两个上游状态码失败分支从不触碰它。
        // 于是「配置错 Key 优先（Invalid）+ 后续 Key 撞 429」这一常见故障转移序列里，
        // config_error 残留 true，尾部把整轮判成硬错误：原样回 429、跳过短路窗口与 Retry-After，
        // 正是 529 设计要防的重试风暴。修复：状态码分支复位 config_error=false。

        // ① 纯配置错误（所有 Key 都缺 maxOutputTokens）→ 硬错误，回 400、不 529。
        assert!(
            all_failed_is_hard_error(true, None),
            "config_error 单独出现必须判硬错误（回 400 可行动原因，不能 529 让客户端无限退避）"
        );

        // ② 关键回归场景：最后一次是上游 429 临时限流 → 必须判临时性（529+Retry-After），
        //    即使循环早期某个 Key 曾因配置错误失败。修复后状态码分支已把 config_error 复位，
        //    故到这里传入的 config_error 必为 false —— 断言这种组合被判为临时性。
        assert!(
            !all_failed_is_hard_error(false, Some(429)),
            "最后一次是 429 限流 → 临时性：应 529+Retry-After+短路窗口，不能原样回 429"
        );
        for transient in [408u16, 409, 429, 500, 502, 503, 529] {
            assert!(
                !all_failed_is_hard_error(false, Some(transient)),
                "{transient} 属临时性，应回 529 让客户端退避重试"
            );
        }

        // ③ 真正的硬状态码（配置/密钥错）不受影响：原样回、不退避。
        for hard in [400u16, 401, 403, 404, 422, 501] {
            assert!(
                all_failed_is_hard_error(false, Some(hard)),
                "{hard} 是永不自愈的硬错误，应原样回、不武装短路窗口"
            );
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

    /// 慢上游 mock：收到请求后先睡 `delay_ms` 再回，用于验证超时/预算行为。
    async fn spawn_slow_mock(delay_ms: u64, status: u16, body: &'static str) -> String {
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
                        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                        let resp = Response::builder()
                            .status(status)
                            .header("content-type", "application/json")
                            .body(full_body(Bytes::from(body)))
                            .unwrap();
                        Ok::<_, std::convert::Infallible>(resp)
                    });
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(io, svc)
                        .await;
                });
            }
        });
        format!("http://{addr}")
    }

    /// P2-5 第二步的核心防线：**候选之间不得互相污染请求体**。
    ///
    /// 零拷贝优化（最后一个候选 `mem::take` 走 body）一旦写错，症状是「第 2 个候选收到的
    /// 模型名是第 1 个候选 resolve 后的值」——不报错、不 panic，只是静默打错模型。
    /// 这里让两个候选映射到**不同的真实模型名**，断言各自收到自己那个。
    ///
    /// 顺带覆盖初版真实踩到的坑：`body_for_attempt` 曾被提到 `if wants_stream` 之前构造，
    /// 于是非流式路径拿到已被 take 空的 body（发出去只剩 `"messages": []`）。
    #[tokio::test]
    async fn candidates_do_not_pollute_each_others_body() {
        let cap = std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
        // 第一个候选返 500（触发故障转移），且要能抓到它收到的 body
        let bad = spawn_capture_mock_with_status(cap.clone(), 500).await;
        let good_cap = std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
        let good = spawn_capture_mock_with_status(good_cap.clone(), 200).await;

        let dir = temp_dir("no_pollute");
        let store = std::sync::Arc::new(
            Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap(),
        );
        // 两条 Key 把同一个对外名映射到**不同**真实模型名
        let mut k1 = key("k1", 0, &bad);
        k1.mappings = vec![crate::model::ModelMapping {
            id: "m1".into(),
            expected_name: "outer".into(),
            real_name: "real-FIRST".into(),
        }];
        let mut k2 = key("k2", 1, &good);
        k2.mappings = vec![crate::model::ModelMapping {
            id: "m2".into(),
            expected_name: "outer".into(),
            real_name: "real-SECOND".into(),
        }];
        store.upsert_key(k1).unwrap();
        store.upsert_key(k2).unwrap();
        store.secrets.write().set("k1", "x").unwrap();
        store.secrets.write().set("k2", "y").unwrap();

        let pm = ProxyManager::new(store.clone());
        let port = pm.start(CategoryType::ClaudeCli).await.unwrap();
        let resp = reqwest::Client::new()
            .post(format!("http://127.0.0.1:{port}/v1/messages"))
            .json(&json!({"model":"outer","max_tokens":10,"messages":[{"role":"user","content":"hi"}]}))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 200, "应故障转移到第二个候选");

        let first = cap.lock().first().cloned().unwrap_or_default();
        let second = good_cap.lock().first().cloned().unwrap_or_default();
        assert!(
            first.contains("real-FIRST"),
            "第一个候选应收到自己的映射结果，实际: {first}"
        );
        assert!(
            second.contains("real-SECOND"),
            "第二个候选必须收到**自己**的映射结果（若被上一候选污染会是 real-FIRST），实际: {second}"
        );
        assert!(
            !second.contains("real-FIRST"),
            "第二个候选的 body 被上一个候选污染了: {second}"
        );
        // 非空校验：body 不能是被 take 空后的残壳
        assert!(
            second.contains("\"content\":\"hi\"") || second.contains("hi"),
            "第二个候选收到的 body 不应是空壳（初版 take 时机写错就是这个症状）: {second}"
        );

        pm.stop(CategoryType::ClaudeCli);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 抓 body 且可指定返回状态码的 mock（用于构造「第一个候选失败」的故障转移场景）。
    async fn spawn_capture_mock_with_status(
        captured: std::sync::Arc<parking_lot::Mutex<Vec<String>>>,
        status: u16,
    ) -> String {
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
                                .status(status)
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

    /// P1-1：故障转移总预算必须挡住「候选数 × per-Key 超时」的最坏累计等待。
    ///
    /// 没有预算时：3 个慢 Key 各让 per-Key 超时跑满，客户端要等 3 倍时间；而真实场景里
    /// 客户端（Claude Code / Codex）早已自己超时重发，代理侧那条僵尸链仍在逐个打上游烧额度。
    ///
    /// 这里把预算设到「够第一个候选跑完、不够再开第二个」的量级，断言：
    /// ① 总耗时明显小于「所有候选都跑满」；② 仍然如实返回失败（529）而非假装成功。
    #[tokio::test]
    async fn failover_budget_stops_walking_all_candidates() {
        // 每个 Key 都慢到超过它自己的 timeout_ms（下面设 900ms），必然逐个失败
        let slow = spawn_slow_mock(3_000, 200, r#"{"ok":true}"#).await;

        let dir = temp_dir("failover_budget");
        let store = std::sync::Arc::new(
            Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap(),
        );
        // 5 个候选都指向同一个慢上游；每个 Key 自身超时 900ms
        for i in 0..5 {
            let mut k = key(&format!("k{i}"), i, &slow);
            k.params.timeout_ms = Some(900);
            store.upsert_key(k).unwrap();
            store.secrets.write().set(&format!("k{i}"), "x").unwrap();
        }
        // 预算 1500ms：够第一个候选跑满 900ms，剩 600ms < MIN_ATTEMPT_SLICE(5s) → 后续全跳过
        let mut s = store.get_settings();
        s.failover_total_budget_ms = 1_500;
        store.save_settings(UserPrefs::from(&s)).unwrap();

        let pm = ProxyManager::new(store.clone());
        let port = pm.start(CategoryType::ClaudeCli).await.unwrap();

        let t0 = std::time::Instant::now();
        let resp = reqwest::Client::new()
            .post(format!("http://127.0.0.1:{port}/v1/messages"))
            .json(&json!({"model":"m","max_tokens":10,"messages":[{"role":"user","content":"hi"}]}))
            .send()
            .await
            .unwrap();
        let elapsed = t0.elapsed();

        assert_eq!(
            resp.status().as_u16(),
            529,
            "全部候选失败应返回 529（过载/稍后重试），不能假装成功"
        );
        // 5 个候选各跑满 900ms ≈ 4.5s；有预算时应在 ~1s 量级结束。留足余量取 3s 上限，
        // 既能证明「没有走完全部候选」，又不会因 CI 机器抖动而假红。
        assert!(
            elapsed < std::time::Duration::from_secs(3),
            "预算应阻止继续遍历剩余候选，实测耗时 {elapsed:?}（无预算时约 4.5s）"
        );

        pm.stop(CategoryType::ClaudeCli);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// **流式**探头阶段必须受 per-Key 超时约束 —— 即便故障转移总预算被关闭。
    ///
    /// 钉住的缺口：`try_stream_to_key` 此前只用 `budget_left` 兜探头阶段、完全不读
    /// `key.params.timeout_ms`。于是：
    /// - 用户为该 Key 设的超时（这里 800ms）对流式**完全无效**；
    /// - 把总预算设成 0（关闭）时 `budget_left` 为 None，流式探头**没有任何响应超时**，
    ///   上游连上却不回响应头就永久挂着，直到 TCP 自己断。
    ///
    /// 而 Claude Code / Codex 默认都发 `stream:true`，这是主路径；用户为了「别掐断长回答」
    /// 去关总预算，恰恰会踩中这个组合。
    ///
    /// 判据：预算关闭 + 上游卡 10s 不回响应头 + Key 超时 800ms →
    /// 必须在秒级内以失败收场（两个候选各 800ms），而不是挂满 10s。
    #[tokio::test]
    async fn streaming_probe_honors_key_timeout_even_without_budget() {
        // 上游收到请求后卡 10s 才回（模拟「连上了但不回响应头」）
        let stuck = spawn_slow_mock(10_000, 200, r#"{"ok":true}"#).await;

        let dir = temp_dir("stream_probe_to");
        let store = std::sync::Arc::new(
            Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap(),
        );
        for i in 0..2 {
            let mut k = key(&format!("k{i}"), i, &stuck);
            k.params.timeout_ms = Some(800); // 每个候选自身超时 800ms
            store.upsert_key(k).unwrap();
            store.secrets.write().set(&format!("k{i}"), "x").unwrap();
        }
        // **关闭**总预算：这正是缺口最致命的配置（旧实现此时流式无任何超时）
        let mut s = store.get_settings();
        s.failover_total_budget_ms = 0;
        store.save_settings(UserPrefs::from(&s)).unwrap();

        let pm = ProxyManager::new(store.clone());
        let port = pm.start(CategoryType::ClaudeCli).await.unwrap();

        let t0 = std::time::Instant::now();
        let resp = reqwest::Client::new()
            .post(format!("http://127.0.0.1:{port}/v1/messages"))
            // stream:true → 走 try_stream_to_key（本用例要测的那条路径）
            .json(&json!({
                "model":"m","max_tokens":10,"stream":true,
                "messages":[{"role":"user","content":"hi"}]
            }))
            .send()
            .await
            .unwrap();
        let elapsed = t0.elapsed();

        assert_eq!(
            resp.status().as_u16(),
            529,
            "两个候选都探头超时 → 应如实返回 529，不能假装成功"
        );
        // 2 个候选各 800ms ≈ 1.6s。旧实现（预算关闭时无超时）会挂到上游 10s 才回。
        // 取 5s 上限：既能证明「per-Key 超时真的生效了」，又给 CI 抖动留足余量。
        assert!(
            elapsed < std::time::Duration::from_secs(5),
            "流式探头必须受 per-Key 超时约束，实测 {elapsed:?}（旧实现约 10s）"
        );

        pm.stop(CategoryType::ClaudeCli);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// P1-1 边界：**第一个候选永不因预算被跳过**。
    ///
    /// 否则用户把预算配得很小（或上一次请求刚好耗尽窗口）时会变成「一个都不试直接 529」，
    /// 那比慢更糟——至少要给一次机会。
    #[tokio::test]
    async fn failover_budget_never_skips_first_candidate() {
        let good = spawn_mock(
            200,
            r#"{"id":"msg_1","type":"message","role":"assistant","content":[{"type":"text","text":"ok"}]}"#,
        )
        .await;

        let dir = temp_dir("failover_budget_first");
        let store = std::sync::Arc::new(
            Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap(),
        );
        store.upsert_key(key("k1", 0, &good)).unwrap();
        store.secrets.write().set("k1", "x").unwrap();
        // 预算设成 1ms：远小于 MIN_ATTEMPT_SLICE，但第一个候选仍必须被尝试
        let mut s = store.get_settings();
        s.failover_total_budget_ms = 1;
        store.save_settings(UserPrefs::from(&s)).unwrap();

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
            200,
            "预算再小也必须尝试第一个候选，否则等于「一个都不试直接失败」"
        );

        pm.stop(CategoryType::ClaudeCli);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// P1-1 回归：预算为 0（用户关闭）时行为与改动前完全一致——正常故障转移不受影响。
    #[tokio::test]
    async fn failover_budget_zero_disables_the_constraint() {
        let bad = spawn_mock(401, r#"{"error":{"message":"unauthorized"}}"#).await;
        let good = spawn_mock(
            200,
            r#"{"id":"msg_1","type":"message","role":"assistant","content":[{"type":"text","text":"ok"}]}"#,
        )
        .await;

        let dir = temp_dir("failover_budget_off");
        let store = std::sync::Arc::new(
            Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap(),
        );
        store.upsert_key(key("k1", 0, &bad)).unwrap();
        store.upsert_key(key("k2", 1, &good)).unwrap();
        store.secrets.write().set("k1", "x").unwrap();
        store.secrets.write().set("k2", "y").unwrap();
        let mut s = store.get_settings();
        s.failover_total_budget_ms = 0; // 关闭
        store.save_settings(UserPrefs::from(&s)).unwrap();

        let pm = ProxyManager::new(store.clone());
        let port = pm.start(CategoryType::ClaudeCli).await.unwrap();
        let resp = reqwest::Client::new()
            .post(format!("http://127.0.0.1:{port}/v1/messages"))
            .json(&json!({"model":"m","max_tokens":10,"messages":[{"role":"user","content":"hi"}]}))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 200, "关闭预算后故障转移应照常工作");

        pm.stop(CategoryType::ClaudeCli);
        std::fs::remove_dir_all(&dir).ok();
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
        store.secrets.write().enable_master_password("TestPass123").unwrap();
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

    /// **密钥缺失是配置错误，不是上游过载**（P2 回归）。
    ///
    /// 原先取密钥失败用 `AppError::upstream_msg("密钥缺失")`，会被尾部分流判成「临时性上游
    /// 故障」→ 回 529 + Retry-After + 武装短路窗口 + 计入熔断。而这个问题**永不自愈**
    /// （密钥根本没存进去），客户端却据 529 无限退避重试，用户看到的是「上游过载」
    /// —— 方向完全错，真因是自己没填密钥。
    ///
    /// 正确行为：400（硬错误分支）+ 可行动文案 + 不熔断（Key 本身没问题，是配置缺失）。
    ///
    /// 故障注入判据：把 `Invalid` 换回 `upstream_msg`，状态码断言立刻变红（收到 529）。
    #[tokio::test]
    async fn missing_secret_returns_actionable_400_not_529_overloaded() {
        // 上游 mock 回 200：若真被打到会拿到 200，从而暴露「没在本地拦下」。
        let upstream = spawn_mock(
            200,
            r#"{"id":"m","type":"message","role":"assistant","content":[{"type":"text","text":"ok"}]}"#,
        )
        .await;
        let dir = temp_dir("nosecret");
        let store = std::sync::Arc::new(
            Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap(),
        );
        // 建 Key 但**不存密钥**（真实场景：用户填了地址就保存、密钥没填或保存失败）
        store.upsert_key(key("k1", 0, &upstream)).unwrap();

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
            400,
            "密钥缺失是配置错误 → 400；529 会让客户端把「没填密钥」当成上游过载无限退避"
        );
        assert!(
            resp.headers().get("retry-after").is_none(),
            "永不自愈的错误不该带 Retry-After（那是在说「等等就好了」）"
        );
        let body: serde_json::Value = resp.json().await.unwrap();
        let msg = body["error"]["message"].as_str().unwrap_or_default();
        assert!(msg.contains("密钥"), "要点明缺的是密钥: {msg}");
        assert!(
            msg.contains("Key 编辑器") || msg.contains("填写"),
            "要给出可自助的修复入口: {msg}"
        );

        // Key 本身没坏（凭据压根没被用过）→ 不得计入熔断，否则填上密钥后还要等窗口过去。
        let k = store.get_key("k1").unwrap();
        assert_eq!(k.health.fail_count, 0, "配置缺失不得把 Key 刷成失败");
        assert!(k.health.breaker_until.is_none(), "配置缺失不得触发熔断");

        pm.stop(CategoryType::ClaudeCli);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 扩展思考签名失效（真机报错）必须给出可行动说明，且**不得**说成「全部 Key 不可用」。
    ///
    /// 真机原文（中转站脱敏后）：
    /// ```text
    /// status_code=502, upstream HTTP 400: {"__type":"***.***.***.runtimeservice#ValidationException",
    ///  "message":"***.***.content.6: Invalid `signature` in `thinking` block",
    ///  "reason":"THINKING_SIGNATURE_INVALID"}
    /// ```
    ///
    /// 两件事一起钉：
    /// 1. **前缀不能是「全部 Key 不可用」** —— 400 是请求级错误，各 Key 打同样失败
    ///    （`is_request_level_4xx` / `failover_verb` 早就这么定性了）。说「Key 不可用」
    ///    会把用户送去查密钥、查额度、查中转站，全是无关方向。
    /// 2. 附上三条真能解决的动作。原文那句 coral 异常用户完全看不懂 ——
    ///    而这个成因（换了上游 → 验不了旧上游签的名）不看代码根本猜不到。
    ///
    /// 另钉两条边界：非请求级错误（如 401）保持原前缀；不认识的 400 不乱加说明。
    #[test]
    fn thinking_signature_error_gets_actionable_explanation() {
        let raw = r#"HTTP 400: {"__type":"aws.bedrock.runtimeservice#ValidationException","message":"messages.content.6: Invalid `signature` in `thinking` block","reason":"THINKING_SIGNATURE_INVALID"}"#;

        let body = compose_hard_error_body(Some(400), raw);
        assert!(
            !body.contains("全部 Key 不可用"),
            "400 是请求级错误，说「全部 Key 不可用」是假现场，会把用户送去查密钥/额度：\n{body}"
        );
        assert!(body.contains("非 Key 问题"), "要点明这不是 Key 的问题：\n{body}");
        assert!(body.contains(raw), "上游原文必须原样保留（排障要看它）：\n{body}");
        // 三条可行动的动作都要在
        assert!(body.contains("新会话"), "要给「开新会话」这条最快的出路：\n{body}");
        assert!(body.contains("固定在一条 Key"), "要给「固定单 Key」这条：\n{body}");
        assert!(body.contains("扩展思考"), "要给「关掉扩展思考」这条：\n{body}");

        // 只有 message 没有 reason 的中转站（丢了机器码）也要识别
        let only_msg = "HTTP 400: Invalid `signature` in `thinking` block";
        assert!(
            annotate_known_upstream_error(only_msg).is_some(),
            "部分中转站只透传 message、丢掉 reason，两种写法都得认"
        );

        // 边界①：不认识的 400 不乱加说明（别对着一个我们没搞懂的错误编原因）
        let unknown = "HTTP 400: {\"error\":\"model not found\"}";
        assert!(annotate_known_upstream_error(unknown).is_none());
        let plain = compose_hard_error_body(Some(400), unknown);
        assert!(plain.contains("非 Key 问题"), "前缀仍按请求级分流");
        assert!(plain.contains("model not found"));

        // 边界②：401 是真的 Key 问题，前缀保持原样
        let auth = "HTTP 401: invalid api key";
        let body = compose_hard_error_body(Some(401), auth);
        assert!(
            body.starts_with("全部 Key 不可用："),
            "401 确实是 Key 的问题，不该被改口径：{body}"
        );
    }

    /// 短路窗口内的请求**不得**再记「所有 Key 均在熔断窗口内，已忽略熔断兜底重试」。
    ///
    /// 那句话描述的动作在窗口内压根没发生 —— 什么都没重试，请求就在短路那里断了。
    /// 而 Claude Code / Codex 都会自动重发，窗口内轻易几十次：日志被一串
    /// 「描述了一件没做的事」的行刷满，还把真正有用的事件从 MAX_EVENTS 环里挤出去，
    /// 正是排障最需要那几条的时候。
    ///
    /// 造这个状态要同时满足两件事，比看起来讲究：
    /// - **Key 处于熔断中**（才有 `used_breaker_fallback`）：直接写 health 造，
    ///   因为 5xx/429 **刻意不计熔断**（不罚好 Key），靠打 500 永远打不出熔断；
    /// - **短路窗口已武装**：需要一次「临时性」的整轮失败。用**连接层失败**（指向死端口）——
    ///   它既计熔断又被判临时，是唯一能同时满足两者的失败形态。
    #[tokio::test]
    async fn gated_requests_do_not_log_breaker_fallback_they_never_performed() {
        let dir = temp_dir("gate_no_spam");
        let store = std::sync::Arc::new(
            Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap(),
        );
        // 死端口：连接层失败（既计熔断、又被判临时 → 会武装短路窗口）
        let mut k = key("k1", 0, "http://127.0.0.1:1");
        k.category_id = CategoryType::Codex;
        store.upsert_key(k).unwrap();
        store.secrets.write().set("k1", "x").unwrap();
        // 直接把它置成熔断中，让 candidates_for 走「全部熔断 → 兜底」那条路
        store
            .update_health(
                "k1",
                crate::model::HealthState {
                    status: crate::model::HealthStatus::Down,
                    fail_count: 3,
                    breaker_until: Some(chrono::Utc::now().timestamp_millis() + 600_000),
                    ..Default::default()
                },
            )
            .unwrap();

        let pm = ProxyManager::new(store.clone());
        let port = pm.start(CategoryType::Codex).await.unwrap();
        let url = format!("http://127.0.0.1:{port}/v1/messages");
        let body = json!({"model":"m","max_tokens":10,"messages":[{"role":"user","content":"hi"}]});
        let client = reqwest::Client::new();
        let fallback_lines = |s: &std::sync::Arc<Store>| -> usize {
            s.list_all_events()
                .iter()
                .filter(|e| e.detail.contains("已忽略熔断兜底重试"))
                .map(|e| e.repeat.max(1) as usize)
                .sum()
        };

        // 第一次：熔断兜底放它进来 → 连接失败 → 武装短路窗口。
        // 这一条**应该**记兜底事件（它真的绕过熔断试了一次）。
        let first = client.post(&url).json(&body).send().await.unwrap();
        assert_eq!(first.status().as_u16(), 529, "连接层失败属临时性 → 529");
        let after_first = fallback_lines(&store);
        assert!(after_first >= 1, "真的绕过熔断试过一次，那一条兜底事件该记");

        // 之后连发几次：**逐条**判定，不做整体计数比较。
        //
        // 为什么不能整体比：短路窗口只有 5s，而测试套件里几十个 tokio 用例并行跑，
        // 机器一卡就可能超出窗口 —— 那时某次请求会合法地再走一遍熔断兜底并记一条，
        // 整体断言就假红了（实测十轮里红过一次）。逐条判定把「时间」这个变量摘掉：
        // 只对**确实被短路挡住**的那些请求（响应体带短路专属文案）要求「计数没涨」。
        let mut gated_seen = 0usize;
        let mut prev = after_first;
        for _ in 0..8 {
            let resp = client.post(&url).json(&body).send().await.unwrap();
            assert_eq!(resp.status().as_u16(), 529, "两条路径都回 529");
            let text = resp.text().await.unwrap_or_default();
            let now = fallback_lines(&store);
            if text.contains("上次尝试已确认全部候选均失败") {
                gated_seen += 1;
                assert_eq!(
                    now, prev,
                    "被短路挡住的请求不得新增「已忽略熔断兜底重试」——那件事没发生。\
                     把该事件排到短路判定之前，客户端每次自动重发都会刷一条，\
                     把真正有用的事件挤出 MAX_EVENTS 环"
                );
            }
            prev = now;
        }
        assert!(
            gated_seen > 0,
            "至少要有一次落在短路窗口内，否则本用例什么都没验到（窗口 {}ms）",
            ALL_FAILED_SHORT_CIRCUIT_MS
        );

        pm.stop(CategoryType::Codex);
        std::fs::remove_dir_all(&dir).ok();
    }

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
        // 前提检查，理由同 `upstream_retry_after_is_propagated_downstream`：
        // 本用例的前提是「**所有候选都返回硬 4xx**」。而并行满载时 localhost 偶发连一次不上，
        // 连接层失败按设计算「等一等可能会好」（`saw_transient`）→ 整轮转为临时性处置 →
        // 短路窗口**正确地**被武装 → 第二次拿到 529，断言 401 变红而代码没错。
        // 实测判据：`--test-threads=1` 连跑 2 轮 722 全绿；并行下这条与那条 Retry-After
        // 用例是同一类前提被打破。故按实际发生的情形分别断言，两条都是真契约。
        let events = store.list_all_events();
        let saw_conn_failure = events.iter().any(|e| e.detail.contains("连接"));
        let expect_second = if saw_conn_failure { STATUS_OVERLOADED } else { 401 };
        assert_eq!(
            again.status().as_u16(),
            expect_second,
            "硬错误不武装短路窗口，第二次仍应是 401 而不是短路的 529。\n\
             本次 saw_conn_failure={saw_conn_failure}（连接层失败属临时性，会让整轮转为\
             临时处置并武装窗口——那是设计行为）。全部事件：{:#?}",
            events.iter().map(|e| (e.kind.clone(), e.detail.clone())).collect::<Vec<_>>()
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
        // 前提检查：本用例断言的是「**两个候选都返回了带 Retry-After 的 429**」这种情形。
        //
        // 为什么必须显式检查（本轮 15 轮抓到一次假红）：`effective_retry_after_hint` 现在会把
        // 「临时失败但没给 Retry-After」的候选按短路窗口长度折进最小值 —— 而**连接层失败
        // 永远没有那个头**。套件并行满载时 localhost 偶发连一次不上，于是这个用例的前提被
        // 悄悄打破：一个候选连接失败 → 按设计折回 5s → 断言 7 变红，而代码完全正确。
        //
        // 故按实际发生的情形分别断言，两条都是真契约、都不放水：
        //   - 两个 429 都拿到 → 取最小值 7；
        //   - 有候选连接层失败 → 恢复时间未知，折回短路窗口长度。
        let events = store.list_all_events();
        let saw_conn_failure = events.iter().any(|e| e.detail.contains("连接"));
        let expected = if saw_conn_failure {
            (ALL_FAILED_SHORT_CIRCUIT_MS / 1000).max(1)
        } else {
            7
        };
        assert_eq!(
            retry_after, expected,
            "两个候选都给了 Retry-After 时应透传最短的那个（7）：只要有一个候选先恢复\
             就该放下游来探路，取最长会让一个撞配额的 Key 把整池停摆。\n\
             本次 saw_conn_failure={saw_conn_failure}（连接层失败没有 Retry-After 头，\
             按设计折回短路窗口长度）。全部事件：{:#?}",
            events.iter().map(|e| (e.kind.clone(), e.detail.clone())).collect::<Vec<_>>()
        );

        pm.stop(CategoryType::Codex);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 流式失败的链路快照里，「上游请求」必须是**转换后真正发出去的**那份。
    ///
    /// 场景：下游是 Anthropic（Claude Code 发 `/v1/messages` + `stream:true`），
    /// Key 是 OpenAI Responses 协议，上游回 400。`StreamAttempt::HttpError` 此前不带
    /// 请求体快照，调用方只能退而记 `downstream_body` —— 而界面标签写着「上游请求」。
    ///
    /// 后果正落在最需要它的场合：排「这个 Key 为什么回 400」时，看到的是一份 Anthropic
    /// 请求体，而实际发出去的是 Responses 格式。模型名映射、`max_tokens` 补写、
    /// reasoning→thinking 转换全都看不见，而 400 的成因几乎总在这些转换里。
    ///
    /// 判据用**协议形状**而不是字符串相等：Responses 体有 `input`、没有 Anthropic 的
    /// `messages`/`max_tokens`。这样即便转换细节以后变了，测试仍在钉「记的是哪一份」
    /// 这个契约本身。
    #[tokio::test]
    async fn streaming_failure_trace_records_upstream_request_not_downstream_body() {
        let bad = spawn_mock(400, r#"{"error":{"message":"bad request"}}"#).await;
        let dir = temp_dir("stream_trace_body");
        let store = std::sync::Arc::new(
            Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap(),
        );
        // 链路快照只在「调用模型日志」开关开启时产生
        let mut s = store.get_settings();
        s.request_log_enabled = true;
        store.save_settings(UserPrefs::from(&s)).unwrap();

        let mut k = key("k1", 0, &bad);
        k.protocol = Protocol::OpenaiResponses; // 跨协议：下游 Anthropic → 上游 Responses
        store.upsert_key(k).unwrap();
        store.secrets.write().set("k1", "x").unwrap();

        let pm = ProxyManager::new(store.clone());
        let port = pm.start(CategoryType::ClaudeCli).await.unwrap();
        let _ = reqwest::Client::new()
            .post(format!("http://127.0.0.1:{port}/v1/messages"))
            .json(&json!({
                "model": "m",
                "max_tokens": 10,
                "stream": true,
                "messages": [{"role": "user", "content": "hi"}]
            }))
            .send()
            .await
            .unwrap();

        // 找到那条带快照的失败请求日志
        let all: Vec<_> = store.list_all_events();
        let trace = all
            .iter()
            .filter(|e| e.kind == "request")
            .find_map(|e| store.event_trace(&e.id))
            .unwrap_or_else(|| {
                panic!(
                    "开了调用模型日志，失败的流式尝试必须留下链路快照。全部事件：{:#?}",
                    all.iter().map(|e| (&e.kind, &e.detail)).collect::<Vec<_>>()
                )
            });
        assert_eq!(
            trace.status,
            Some(400),
            "应是我们 mock 的 400。实得 trace={trace:#?}\n全部事件={:#?}",
            all.iter().map(|e| (&e.kind, &e.detail)).collect::<Vec<_>>()
        );
        assert!(
            trace.request_body.contains("\"input\""),
            "应是转换后的 Responses 请求体（含 input），实得：\n{}",
            trace.request_body
        );
        assert!(
            !trace.request_body.contains("\"max_tokens\""),
            "不得记下游原始的 Anthropic 请求体（那里才有 max_tokens）——\
             界面标签写的是「上游请求」，记错会把排障引向不存在的问题。实得：\n{}",
            trace.request_body
        );

        pm.stop(CategoryType::ClaudeCli);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 被**我们自己**的故障转移预算掐短的尝试，不得算进该 Key 的熔断计数。
    ///
    /// 每次尝试的超时是 `min(Key 自身超时, 剩余预算)`。剩余预算更小时，一条完全健康的 Key
    /// 也会在答完之前被掐断，而那个 Err 走连接层分支 → `record_live_failure` 给它记一次失败。
    /// 三次之后这条好 Key 被熔断 60 秒。且这不是偶发：排在预算末尾的候选**每一轮都**被削短，
    /// 于是池子里最后那条（常常也是最好的备用）反而先被熔断，是系统性偏置。
    ///
    /// 四个方向一起钉：
    /// 1. 预算把时间片削短、且尝试用完了那个片 → 免罚；
    /// 2. 预算比 Key 超时还长（= 没削短）→ 照罚，这是 Key 自己超时；
    /// 3. 预算未开启（`None`）→ 照罚；
    /// 4. **瞬间失败**（DNS/拒连，远未用完时间片）→ 照罚。少了这条，预算紧张时整池都不再
    ///    熔断，等于把熔断关掉。
    #[test]
    fn budget_shortened_timeout_does_not_count_against_breaker() {
        let key_to = std::time::Duration::from_secs(30);

        assert!(
            budget_truncated_attempt(2_000, Some(std::time::Duration::from_millis(2_000)), key_to),
            "预算只剩 2s、尝试也确实跑满 2s → 是我们掐的，不该罚这条 Key"
        );
        assert!(
            !budget_truncated_attempt(30_000, Some(std::time::Duration::from_secs(60)), key_to),
            "预算比 Key 超时还长 → 没被削短，这是 Key 自己超时，照罚"
        );
        assert!(
            !budget_truncated_attempt(30_000, None, key_to),
            "预算未开启 → 照罚（退化成旧行为）"
        );
        assert!(
            !budget_truncated_attempt(40, Some(std::time::Duration::from_millis(2_000)), key_to),
            "40ms 就失败（DNS/拒连）远未用完 2s 时间片 → 真失败，照罚；\
             否则预算一紧张就等于把熔断整体关掉"
        );
    }

    /// 「有临时性失败但上游没给 Retry-After」时，长退避必须被折回短路窗口长度。
    ///
    /// 这是「一个撞配额的 Key 拖垮整池」的第二条路径。第一条（取最大值）早已修掉，
    /// 但取最小值只在**给了头的候选之间**比较 —— 没给头的候选压根不参与，于是混合池
    /// `[429 Retry-After: 300, 500 无头]` 仍然会让下游等 300 秒，而那个 500 很可能
    /// 1 秒后就好了。
    ///
    /// 四个方向一起钉（少任何一条这个修复都会被后人「简化」掉）：
    /// 1. 全都给了头 → 原样取最小值，**不**被窗口长度截短（上游明确说的话要听）；
    /// 2. 长退避 + 无头 → 折回窗口长度；
    /// 3. **短**退避 + 无头 → 保持短值，不被拖长成窗口长度（故用 `min` 而非直接替换）；
    /// 4. 谁都没给头 → 仍是 `None`，不在这里凭空造值（调用方另有兜底，
    ///    在此返回 `Some(gate)` 会让「从没提过退避」与「正好说了一个窗口」不可区分）。
    #[test]
    fn missing_retry_after_folds_long_backoff_back_to_gate_window() {
        let gate_secs = (ALL_FAILED_SHORT_CIRCUIT_MS / 1000).max(1);

        assert_eq!(
            effective_retry_after_hint(Some(300), false),
            Some(300),
            "所有临时失败都给了头 → 听上游的，不得被窗口长度截短"
        );
        assert_eq!(
            effective_retry_after_hint(Some(300), true),
            Some(gate_secs),
            "有候选没给头（恢复时间未知）→ 不能让唯一给头的那个替全池发言"
        );
        assert_eq!(
            effective_retry_after_hint(Some(1), true),
            Some(1),
            "短退避不得被拖长：取 min 而不是无条件换成窗口长度"
        );
        assert_eq!(
            effective_retry_after_hint(None, true),
            None,
            "谁都没给头时保持 None，由调用方兜底 —— 否则与「上游正好说了一个窗口」不可区分"
        );
    }

    /// 端到端：混合池 `[429 Retry-After: 300, 500 无头]` 下游拿到的必须是窗口长度，不是 300。
    ///
    /// 与 `upstream_retry_after_is_propagated_downstream` 成对：那条钉「都给了头时取最小」，
    /// 这条钉「有一条没给头时不被长退避绑住」。真机影响是 60 倍的过度退避 ——
    /// 客户端整整 5 分钟不再发请求，而池里那条 500 可能下一秒就恢复了。
    #[tokio::test]
    async fn mixed_pool_without_retry_after_backs_off_by_gate_not_by_the_only_hint() {
        let quota = spawn_mock_with_headers(429, "daily quota", &[("retry-after", "300")]).await;
        let flaky = spawn_mock(500, "internal error").await; // 无 retry-after 头
        let dir = temp_dir("retry_after_mixed");
        let store = std::sync::Arc::new(
            Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap(),
        );
        let mut k1 = key("k1", 0, &quota);
        k1.category_id = CategoryType::Codex;
        let mut k2 = key("k2", 1, &flaky);
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
            .expect("整轮失败必须带 Retry-After");
        assert_eq!(
            retry_after,
            (ALL_FAILED_SHORT_CIRCUIT_MS / 1000).max(1),
            "池里有一条 500 没给头（恢复时间未知）→ 按短路窗口退避；\
             照抄那条 429 的 300 秒等于让一个撞配额的 Key 把整池停摆 5 分钟"
        );

        pm.stop(CategoryType::Codex);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// **混合失败池**（P1 回归）：只要池里有一条 Key 是「等一等会好」，整轮就必须按临时性处置。
    ///
    /// 场景：k1（高优先级）撞按天配额回 `429 + Retry-After: 7`；k2 是早已过期的备用 Key 回 401。
    /// 尾部分流原先只看 `last_status`（被每个候选无条件覆盖），于是只看到 k2 的 401 → 判硬错误
    /// → **原样回 401 `authentication_error`、丢掉 k1 给的 Retry-After、不武装短路窗口**。
    /// 客户端（Anthropic SDK）对 401 不退避重试，用户看到「密钥错误」而真实结论是
    /// 「7 秒后就能用」—— 排查方向完全相反。
    ///
    /// 故障注入判据：把尾部的 `!saw_transient &&` 去掉，本测试立即变红（收到 401 而非 529）。
    #[tokio::test]
    async fn mixed_failure_pool_is_treated_as_transient_not_hard() {
        let limited = spawn_mock_with_headers(429, "quota", &[("retry-after", "7")]).await;
        let expired = spawn_mock(401, r#"{"error":{"message":"invalid api key"}}"#).await;
        let dir = temp_dir("mixed_pool");
        let store = std::sync::Arc::new(
            Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap(),
        );
        // k1 先被尝试（priority 0）回 429，k2 后被尝试回 401 —— 401 是「最后一次失败」。
        let mut k1 = key("k1", 0, &limited);
        k1.category_id = CategoryType::Codex;
        let mut k2 = key("k2", 1, &expired);
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
        assert_eq!(
            resp.status().as_u16(),
            529,
            "池里有一条只是限流 → 整轮按临时性回 529，不能因为最后一个候选是 401 就原样回 401"
        );
        let ra = resp
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<i64>().ok())
            .expect("限流候选给的 Retry-After 必须透传，不能被后面的硬错误候选丢掉");
        assert_eq!(ra, 7, "应透传 429 候选给的 7 秒");

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

    /// mock 服务器抓到的请求头：外层每次请求一条，内层是该次请求的 (name, value) 列表。
    /// 抽成别名只为可读性 —— 测试里这个类型要重复写好几处。
    type CapturedHeaders = std::sync::Arc<parking_lot::Mutex<Vec<Vec<(String, String)>>>>;

    /// 抓请求头的 mock（返回 SSE 或 JSON 由 `sse` 决定，以便同一个 mock 服务两条转发路径）。
    async fn spawn_header_capture_mock(captured: CapturedHeaders, sse: bool) -> String {
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
                            let hs: Vec<(String, String)> = req
                                .headers()
                                .iter()
                                .map(|(k, v)| {
                                    (
                                        k.as_str().to_ascii_lowercase(),
                                        v.to_str().unwrap_or("").to_string(),
                                    )
                                })
                                .collect();
                            cap.lock().push(hs);
                            let (ct, body): (&str, &[u8]) = if sse {
                                ("text/event-stream", b"event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n")
                            } else {
                                ("application/json", br#"{"id":"m","type":"message","role":"assistant","content":[{"type":"text","text":"ok"}]}"#)
                            };
                            let resp = Response::builder()
                                .status(200)
                                .header("content-type", ct)
                                .body(full_body(Bytes::from(body)))
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

    /// P2-2：**流式与非流式两条路径发出的请求头必须一致**。
    ///
    /// 这两条路径的前置段落原先逐字重复 38 行（含鉴权头三元组），任何一处改动漏掉另一处，
    /// 就得到「非流式生效、流式不生效」这类最难复现的半残缺陷——`anthropic-version` 的
    /// 分叉现状就是该风险已发生过一次的证据。现在两者共用 `apply_upstream_headers`，
    /// 这条测试是那个共用的护栏。
    ///
    /// 故障注入判据：让任一路径绕过 `apply_upstream_headers` 自己拼头，本测试立刻变红。
    #[tokio::test]
    async fn stream_and_nonstream_send_identical_headers() {
        let cap_stream = std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
        let cap_plain = std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
        let up_stream = spawn_header_capture_mock(cap_stream.clone(), true).await;
        let up_plain = spawn_header_capture_mock(cap_plain.clone(), false).await;

        // 两个 store 各配一条同样的 Anthropic Key，只是上游地址不同
        let mk = |dir: &std::path::Path, url: &str| {
            let store = std::sync::Arc::new(
                Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap(),
            );
            store.upsert_key(key("k1", 0, url)).unwrap();
            store.secrets.write().set("k1", "sk-test").unwrap();
            store
        };
        let d1 = temp_dir("hdr_stream");
        let d2 = temp_dir("hdr_plain");
        let s1 = mk(&d1, &up_stream);
        let s2 = mk(&d2, &up_plain);

        let pm1 = ProxyManager::new(s1.clone());
        let pm2 = ProxyManager::new(s2.clone());
        let p1 = pm1.start(CategoryType::ClaudeCli).await.unwrap();
        let p2 = pm2.start(CategoryType::ClaudeCli).await.unwrap();

        let cli = reqwest::Client::new();
        // 同一份请求，只有 stream 标志不同 → 分别走两条路径
        for (port, stream) in [(p1, true), (p2, false)] {
            cli.post(format!("http://127.0.0.1:{port}/v1/messages"))
                .header("user-agent", "claude-cli/1.2.3")
                .header("x-app", "cli")
                .json(&json!({
                    "model": "m", "max_tokens": 10, "stream": stream,
                    "messages": [ { "role": "user", "content": "hi" } ]
                }))
                .send()
                .await
                .unwrap();
        }

        let pick = |v: &CapturedHeaders| {
            let g = v.lock();
            let mut hs = g.first().cloned().unwrap_or_default();
            // 剔除逐请求必然不同 / 由 reqwest 自行计算的头
            hs.retain(|(k, _)| {
                !matches!(k.as_str(), "host" | "content-length" | "accept" | "accept-encoding")
            });
            hs.sort();
            hs
        };
        let a = pick(&cap_stream);
        let b = pick(&cap_plain);
        assert!(!a.is_empty(), "流式路径应已收到请求");
        assert_eq!(a, b, "两条路径的请求头集合必须完全一致\n流式={a:?}\n非流式={b:?}");

        // `accept-encoding` 被 pick 剔掉了（它不参与「两条路径一致」的比较），但它的**取值**
        // 是一条独立的硬判据：转发路径字节透明、既不解压也（跨协议时）不透传 content-encoding，
        // 故必须显式要求上游别压。缺这条时「缺 Accept-Encoding = 任何编码都可接受」，
        // 网关合法地返回 gzip/br，下游拿到压缩字节 → 乱码。两条路径都必须带。
        for (name, cap) in [("流式", &cap_stream), ("非流式", &cap_plain)] {
            let g = cap.lock();
            let hs = g.first().cloned().unwrap_or_default();
            let ae = hs
                .iter()
                .find(|(k, _)| k == "accept-encoding")
                .map(|(_, v)| v.as_str().to_ascii_lowercase());
            assert_eq!(
                ae.as_deref(),
                Some("identity"),
                "{name}路径必须显式发 `accept-encoding: identity`（否则上游可合法返回压缩体、下游乱码）"
            );
        }

        // 顺带钉住关键头的实际取值（这些是实测判据，改了会导致鉴权失败或被判 client_restricted）
        let m: std::collections::HashMap<_, _> = a.into_iter().collect();
        assert_eq!(m.get("x-api-key").map(String::as_str), Some("sk-test"), "Anthropic 用 x-api-key");
        assert!(!m.contains_key("authorization"), "Anthropic 不该带 Bearer");
        assert_eq!(
            m.get("anthropic-version").map(String::as_str),
            Some("2023-06-01"),
            "版本头必须带（真 Anthropic API 缺它会 400）"
        );
        assert_eq!(m.get("user-agent").map(String::as_str), Some("claude-cli/1.2.3"), "下游 UA 应透传");
        assert_eq!(m.get("x-app").map(String::as_str), Some("cli"), "下游 x-app 应透传");

        pm1.stop(CategoryType::ClaudeCli);
        pm2.stop(CategoryType::ClaudeCli);
        std::fs::remove_dir_all(&d1).ok();
        std::fs::remove_dir_all(&d2).ok();
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
        store.set_active_effort(CategoryType::Codex, "xhigh").unwrap();

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
        store.set_active_effort(CategoryType::Codex, "high").unwrap();

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
        ModelInfo { real_name: name.into(), source: "manual".into(), fetched_at: None, context_window: None, max_output_tokens: None }
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

    /// 截断必须在**默认配置**下可见。
    ///
    /// 这条盯的不是格式好看，是一类特定的隐身故障：上游按 `max_tokens` 把回答砍掉后，
    /// 转发本身是成功的（HTTP 200、不重试、不触发熔断），日志是一条正常的绿色
    /// 「成功返回」。此前截断标记**只**写进 `RequestTrace`，而链路快照默认关闭、
    /// 开了还得展开某一行才看得到 —— 于是用户唯一的线索是「答案莫名少了后半截」，
    /// 无从查证。detail 行是默认配置下唯一可见的文本，标记必须落在这里。
    ///
    /// 同时钉住 `None`（流式直通无法检测）不得被展示成截断：那会满屏假警，
    /// 假警多了真警就没人看了。
    #[test]
    fn success_detail_surfaces_truncation_and_usage() {
        use crate::upstream::TokenUsage;
        let usage = TokenUsage { input: 1200, output: 340, cache_read: 0, cache_creation: 0 };

        // 被截断：detail 里必须有醒目标记
        let truncated =
            fmt_success_detail("百倍", false, "模型 glm-5.2", 820, Some(true), Some(&usage));
        assert!(
            truncated.contains("被上游截断"),
            "上游按 max_tokens 截断必须写进 detail（默认配置下唯一可见处），实得：{truncated}"
        );
        assert_eq!(truncated, "百倍 · 成功返回 · 模型 glm-5.2 · 820ms · ↑1200 ↓340 · ⚠ 被上游截断（达最大输出上限）");

        // 未截断：不得出现标记
        let ok = fmt_success_detail("百倍", false, "模型 glm-5.2", 820, Some(false), Some(&usage));
        assert!(!ok.contains("被上游截断"), "未截断不应有标记：{ok}");

        // 无法检测（流式直通）：按未截断展示，不许假警
        let unknown = fmt_success_detail("百倍", true, "模型 glm-5.2", 820, None, Some(&usage));
        assert!(
            !unknown.contains("被上游截断"),
            "was_truncated=None 表示无法检测，不能当成截断报警：{unknown}"
        );
        assert_eq!(unknown, "百倍 · 流式返回 · 模型 glm-5.2 · 820ms · ↑1200 ↓340");

        // 无 usage（上游没回 usage 字段）：只是少一段，不影响截断标记
        let no_usage = fmt_success_detail("百倍", true, "模型 glm-5.2", 820, Some(true), None);
        assert_eq!(no_usage, "百倍 · 流式返回 · 模型 glm-5.2 · 820ms · ⚠ 被上游截断（达最大输出上限）");
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

    /// 钉住流式用量采集的**同步前提**：补记任务必须等到上游流真正结束才能读尾部窗口。
    ///
    /// 这条测试存在的原因是一次真实回归：补记任务写成 `tokio::spawn` 里直接
    /// `buf.lock().await`，而 hyper 的 body 是**惰性**的 —— spawn 那一刻流一个字节都还没
    /// 被 poll，锁是空闲的，补记任务瞬间拿到锁、读到空 buffer、提取不到 usage 就退出了。
    /// 于是「流式采集用量」这个功能永远静默采不到任何东西，且不报任何错。
    ///
    /// 这里用 `Notify` 复现正确的同步语义：只有流走完并 notify 之后，补记侧读到的
    /// 才是完整尾部。若把 `notified().await` 删掉（回到旧写法），断言必须变红。

    #[tokio::test]
    async fn stream_tail_window_is_read_only_after_stream_completes() {
        use std::sync::Arc;
        use tokio::sync::{Mutex, Notify};

        let tail: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
        let done = Arc::new(Notify::new());

        let tail_w = tail.clone();
        let done_w = done.clone();
        // 模拟惰性流：spawn 之后才开始产出数据。
        let producer = tokio::spawn(async move {
            for part in ["event: a\n", "event: b\n", "data: {\"usage\":1}\n"] {
                tokio::task::yield_now().await;
                tail_w.lock().await.extend_from_slice(part.as_bytes());
            }
            done_w.notify_one();
        });

        let tail_r = tail.clone();
        let done_r = done.clone();
        let collector = tokio::spawn(async move {
            // 关键：等流结束。删掉这一行就是旧的错误写法。
            done_r.notified().await;
            let buf = tail_r.lock().await;
            String::from_utf8_lossy(&buf).to_string()
        });

        producer.await.unwrap();
        let seen = collector.await.unwrap();
        assert!(
            seen.contains("usage"),
            "补记任务必须读到流末尾的 usage 数据，实际读到: {seen:?}"
        );
    }

    /// 上游 mock：回 200 + `text/event-stream`，流内先发一个正常事件、再发终止性 error 事件。
    /// 这是 Anthropic 过载最常见的真实形态（响应头已 200，错误藏在流里）。
    async fn spawn_sse_inflight_error_mock() -> String {
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
                        let body: &[u8] = b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":10}}}\n\nevent: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n\n";
                        let resp = Response::builder()
                            .status(200)
                            .header("content-type", "text/event-stream")
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

    /// **P0 回归**：同协议流式「200 响应头 + 流内 error」必须能把 Key 熔断。
    ///
    /// 曾经的缺陷（实测复现过）：拿到 2xx 响应头就同步 `record_live_success`（fail_count 清零），
    /// 而「流内报错补记失败」要等 body 抽干后的 spawn 任务才跑 —— 于是每次请求都是
    /// 「先清零 → 再加回 1」，`BREAKER_THRESHOLD = 3` 永远达不到：该 Key 永不熔断，
    /// 客户端反复重打同一条坏 Key，正是熔断机制要止住的重试风暴。
    ///
    /// 修法：把成功/失败记账统一推迟到**流末**，按有无流内 error 二选一。
    ///
    /// 故障注入判据：把 `record_live_success` 移回 `StreamAttempt::Streaming` 分支
    /// （即恢复「响应头 200 就记成功」），本测试立即变红。
    #[tokio::test]
    async fn in_stream_error_accumulates_and_trips_breaker() {
        let up = spawn_sse_inflight_error_mock().await;
        let dir = temp_dir("sse_inflight_err");
        let store = std::sync::Arc::new(
            Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap(),
        );
        store.upsert_key(key("k1", 0, &up)).unwrap();
        store.secrets.write().set("k1", "sk-test").unwrap();

        let pm = ProxyManager::new(store.clone());
        let port = pm.start(CategoryType::ClaudeCli).await.unwrap();
        let cli = reqwest::Client::new();

        // 连打 3 次（= BREAKER_THRESHOLD）。每次都必须把 body 抽干，流末记账才会发生。
        for _ in 0..3 {
            let resp = cli
                .post(format!("http://127.0.0.1:{port}/v1/messages"))
                .json(&serde_json::json!({
                    "model": "claude-sonnet-4-5",
                    "max_tokens": 64,
                    "stream": true,
                    "messages": [{ "role": "user", "content": "hi" }]
                }))
                .send()
                .await
                .expect("请求应当拿到响应（上游确实回了 200）");
            let _ = resp.bytes().await; // 抽干 → 触发流末补记任务
            // 补记走 tokio::spawn，给它一点时间落地
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }

        let k = store
            .list_keys(CategoryType::ClaudeCli)
            .into_iter()
            .find(|k| k.id == "k1")
            .expect("Key 应仍在");
        assert!(
            k.health.fail_count >= 3 || k.health.breaker_until.is_some(),
            "连续 3 次「200 + 流内 error」必须累积到熔断，实际 fail_count={} breaker_until={:?} \
             —— 若 fail_count 恒为 1，说明每次请求又在响应头阶段把它清零了",
            k.health.fail_count,
            k.health.breaker_until
        );
    }

    // ==================== 诊断响应头（route_meta）端到端 ====================
    //
    // `route_meta` 自己的单测覆盖「值怎么构造」；这里覆盖「它到底有没有到下游」——
    // 两者缺一不可：头构造正确但没挂上，或挂上了但没填对候选，都是纯单测抓不到的。

    /// 取响应上全部 `x-synaroute-*` 头（小写名 → 值）。
    fn synaroute_headers(resp: &reqwest::Response) -> std::collections::BTreeMap<String, String> {
        resp.headers()
            .iter()
            .filter(|(n, _)| n.as_str().starts_with("x-synaroute-"))
            .map(|(n, v)| (n.as_str().to_string(), v.to_str().unwrap_or("<非法>").to_string()))
            .collect()
    }

    /// 成功转发必须带全套诊断头，且 `attempts=1`（首选就成了）。
    #[tokio::test]
    async fn success_response_carries_route_meta_headers() {
        let up = spawn_mock(200, r#"{"ok":true}"#).await;
        let dir = temp_dir("meta_success");
        let store = std::sync::Arc::new(
            Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap(),
        );
        store.upsert_key(key("k1", 0, &up)).unwrap();
        store.secrets.write().set("k1", "sk-secret-value").unwrap();

        let pm = ProxyManager::new(store.clone());
        let port = pm.start(CategoryType::ClaudeCli).await.unwrap();
        let resp = reqwest::Client::new()
            .post(format!("http://127.0.0.1:{port}/v1/messages"))
            .json(&json!({"model":"claude-x","max_tokens":10,"messages":[{"role":"user","content":"hi"}]}))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        let hs = synaroute_headers(&resp);

        assert_eq!(hs.get("x-synaroute-key").map(String::as_str), Some("k-k1"));
        assert_eq!(hs.get("x-synaroute-key-id").map(String::as_str), Some("k1"));
        assert_eq!(hs.get("x-synaroute-model").map(String::as_str), Some("claude-x"));
        assert_eq!(hs.get("x-synaroute-attempts").map(String::as_str), Some("1"));
        assert_eq!(
            hs.get("x-synaroute-version").map(String::as_str),
            Some(env!("CARGO_PKG_VERSION"))
        );
        // 成功出口不带 upstream_status（下游看得到自己的 200）
        assert_eq!(hs.get("x-synaroute-upstream-status"), None);
        // request_id 是个 uuid，且与复合头同属一次请求
        let rid = hs.get("x-synaroute-request-id").expect("必须有 request-id");
        assert_eq!(rid.len(), 36, "request-id 应为 uuid: {rid}");
        let decision = hs.get("x-synaroute-decision").expect("必须有 decision");
        assert!(
            decision.starts_with("key=k-k1; model=claude-x; attempts=1; latency_ms="),
            "复合头形状不对: {decision}"
        );

        pm.stop(CategoryType::ClaudeCli);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 故障转移后，头必须指向**最终成功的那条** Key，且 `attempts` 反映真实尝试次数。
    ///
    /// 这条是这组头存在的首要理由：客户端此前完全看不出「这次悄悄换过 Key」。
    #[tokio::test]
    async fn failover_is_visible_in_attempts_and_key_headers() {
        let bad = spawn_mock(500, r#"{"error":"boom"}"#).await;
        let good = spawn_mock(200, r#"{"ok":true}"#).await;
        let dir = temp_dir("meta_failover");
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
        assert_eq!(resp.status().as_u16(), 200, "应转移到第二条");
        let hs = synaroute_headers(&resp);
        assert_eq!(
            hs.get("x-synaroute-attempts").map(String::as_str),
            Some("2"),
            "试了两条候选，头里必须是 2（否则客户端看不出发生过故障转移）"
        );
        assert_eq!(
            hs.get("x-synaroute-key-id").map(String::as_str),
            Some("k2"),
            "头必须指向**最终成功**的那条 Key，不是首选那条"
        );

        pm.stop(CategoryType::ClaudeCli);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// **失败出口也必须带头**，且带上最后一次上游状态码。
    ///
    /// 这是失败路径上最值钱的字段：下游只看到我们回的状态码，分不清「上游真的 401」
    /// 与「代理这边判成了配置错误」。漏掉失败出口是最容易犯的错（大家只顾成功路径），
    /// 故单独一条测试盯住。
    #[tokio::test]
    async fn failure_exit_also_carries_headers_with_upstream_status() {
        let up = spawn_mock(401, r#"{"error":"bad key"}"#).await;
        let dir = temp_dir("meta_fail");
        let store = std::sync::Arc::new(
            Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap(),
        );
        store.upsert_key(key("k1", 0, &up)).unwrap();
        store.secrets.write().set("k1", "x").unwrap();

        let pm = ProxyManager::new(store.clone());
        let port = pm.start(CategoryType::ClaudeCli).await.unwrap();
        let resp = reqwest::Client::new()
            .post(format!("http://127.0.0.1:{port}/v1/messages"))
            .json(&json!({"model":"m","max_tokens":10,"messages":[{"role":"user","content":"hi"}]}))
            .send()
            .await
            .unwrap();
        assert!(!resp.status().is_success(), "上游 401，不该是成功");
        let hs = synaroute_headers(&resp);
        assert_eq!(
            hs.get("x-synaroute-upstream-status").map(String::as_str),
            Some("401"),
            "失败出口必须带最后一次上游状态码"
        );
        assert_eq!(hs.get("x-synaroute-attempts").map(String::as_str), Some("1"));
        assert_eq!(hs.get("x-synaroute-key-id").map(String::as_str), Some("k1"));
        assert!(
            hs.get("x-synaroute-decision").unwrap().contains("upstream_status=401"),
            "复合头也该带上"
        );

        pm.stop(CategoryType::ClaudeCli);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 🔴 **安全不变量**：诊断头绝不能携带 base_url / 端口 / 密钥。
    ///
    /// 理由见 `route_meta` 模块注释：部分中转站把访问令牌放在 URL 路径里
    /// （`https://host/v1/<token>/`），把 url 写进响应头等于把密钥回显给下游。
    ///
    /// 这条测试是那份禁令的**机械判据**：谁哪天往 `RouteMeta` 加了 url 字段并挂上头，
    /// 这里立刻变红。仅靠模块注释提醒是不够的——本项目的历史证明注释会被跳过。
    #[tokio::test]
    async fn route_meta_headers_never_leak_base_url_or_secret() {
        let up = spawn_mock(200, r#"{"ok":true}"#).await;
        let dir = temp_dir("meta_noleak");
        let store = std::sync::Arc::new(
            Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap(),
        );
        // 把「像令牌一样的 URL 路径」也放进 base_url，模拟真实中转站形态
        let mut k = key("k1", 0, &format!("{up}/v1/tok-abc123"));
        k.name = "Sub2API".into();
        store.upsert_key(k).unwrap();
        const SECRET: &str = "sk-ant-super-secret-0987654321";
        store.secrets.write().set("k1", SECRET).unwrap();

        let pm = ProxyManager::new(store.clone());
        let port = pm.start(CategoryType::ClaudeCli).await.unwrap();
        let resp = reqwest::Client::new()
            .post(format!("http://127.0.0.1:{port}/v1/messages"))
            .json(&json!({"model":"m","max_tokens":10,"messages":[{"role":"user","content":"hi"}]}))
            .send()
            .await
            .unwrap();
        let hs = synaroute_headers(&resp);
        assert!(!hs.is_empty(), "前置条件：确实挂上了诊断头");

        // up 形如 http://127.0.0.1:PORT —— 取其 host:port 段做判据
        let host_port = up.trim_start_matches("http://").to_string();
        for (name, value) in &hs {
            assert!(!value.contains(SECRET), "{name} 泄露了密钥: {value}");
            assert!(!value.contains("tok-abc123"), "{name} 泄露了 URL 路径里的令牌: {value}");
            assert!(!value.contains(&host_port), "{name} 泄露了上游地址: {value}");
            assert!(!value.contains("http"), "{name} 里出现了 URL: {value}");
        }

        pm.stop(CategoryType::ClaudeCli);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 中文 Key 名（本项目用户的常态）必须被 percent-encode，且**可解码回原值**。
    ///
    /// 只断言「是 ASCII」不够：那样把值整段丢掉也能过。这里真解一遍。
    #[tokio::test]
    async fn chinese_key_name_is_percent_encoded_and_decodable() {
        let up = spawn_mock(200, r#"{"ok":true}"#).await;
        let dir = temp_dir("meta_cjk");
        let store = std::sync::Arc::new(
            Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap(),
        );
        let mut k = key("k1", 0, &up);
        k.name = "「林夕」公益站".into();
        store.upsert_key(k).unwrap();
        store.secrets.write().set("k1", "x").unwrap();

        let pm = ProxyManager::new(store.clone());
        let port = pm.start(CategoryType::ClaudeCli).await.unwrap();
        let resp = reqwest::Client::new()
            .post(format!("http://127.0.0.1:{port}/v1/messages"))
            .json(&json!({"model":"m","max_tokens":10,"messages":[{"role":"user","content":"hi"}]}))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        let hs = synaroute_headers(&resp);
        let raw = hs.get("x-synaroute-key").expect("必须有 key 头");
        assert!(raw.is_ascii() && raw.contains('%'), "中文名应被编码: {raw}");

        // 逐字节 percent-decode 回 UTF-8，必须等于原名
        let decoded = {
            let b = raw.as_bytes();
            let mut out = Vec::with_capacity(b.len());
            let mut i = 0;
            while i < b.len() {
                if b[i] == b'%' && i + 2 < b.len() {
                    let hex = std::str::from_utf8(&b[i + 1..i + 3]).unwrap();
                    out.push(u8::from_str_radix(hex, 16).unwrap());
                    i += 3;
                } else {
                    out.push(b[i]);
                    i += 1;
                }
            }
            String::from_utf8(out).unwrap()
        };
        assert_eq!(
            decoded, "「林夕」公益站",
            "解码后必须还原成原始 Key 名（否则只是把值丢了）"
        );

        pm.stop(CategoryType::ClaudeCli);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// **单一 chokepoint 的结构证明**：连「压根不进转发」的出口也带头。
    ///
    /// `GET /v1/models` 在 `handle_request_inner` 里是第一个早退分支，它没有候选、没有耗时；
    /// 未配置任何 Key 时的 POST 走的是「无候选」早退。它们照样带上 version 头，说明头是在
    /// **唯一出口**挂的，而不是在各转发分支里逐个挂的——后者必然漏，且漏得静默。
    /// 谁哪天把 wrapper 拆掉、改回分支内挂头，这条会变红。
    #[tokio::test]
    async fn every_exit_goes_through_the_single_chokepoint() {
        let dir = temp_dir("meta_chokepoint");
        let store = std::sync::Arc::new(
            Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap(),
        );
        let pm = ProxyManager::new(store.clone());
        let port = pm.start(CategoryType::ClaudeCli).await.unwrap();

        // 三个不同性质的非转发出口：模型发现、单模型检索、无候选（未配置任何 Key）
        for (method_post, path) in [
            (false, "/v1/models"),
            (false, "/v1/models/claude-x"),
            (true, "/v1/messages"),
        ] {
            let c = reqwest::Client::new();
            let url = format!("http://127.0.0.1:{port}{path}");
            let resp = if method_post {
                c.post(&url)
                    .json(&json!({"model":"m","max_tokens":10,"messages":[{"role":"user","content":"hi"}]}))
                    .send()
                    .await
                    .unwrap()
            } else {
                c.get(&url).send().await.unwrap()
            };
            let hs = synaroute_headers(&resp);
            assert_eq!(
                hs.get("x-synaroute-version").map(String::as_str),
                Some(env!("CARGO_PKG_VERSION")),
                "{path} 这个出口没走 chokepoint —— 头是不是被挪回各分支里挂了？"
            );
            // 每个出口都该有 request-id：它是「响应头 ↔ 日志」对账的锚点
            assert!(
                hs.contains_key("x-synaroute-request-id"),
                "{path} 缺 request-id"
            );
        }

        pm.stop(CategoryType::ClaudeCli);
        std::fs::remove_dir_all(&dir).ok();
    }

    // ============ 弹性第二层（单模型锁定）端到端 ============
    //
    // health.rs 的单测覆盖「锁的算术」；这里覆盖「它有没有真的改变路由」。
    // 断言大量借用上一批加的诊断响应头 —— 两个改动互为判据：
    // 头证明路由确实换了 Key，锁证明头里的 attempts 变化不是偶然。

    /// 补全端点 404 → 只锁那个模型，Key 不熔断；**下一次同模型请求直接跳过这条 Key**。
    ///
    /// 第二个断言是这一层的产品价值所在：旧行为下 k1 要被连打 3 次才熔断，
    /// 期间每个请求都白花一次往返；而熔断之后 k1 上**所有**模型一起被挡 60 秒。
    #[tokio::test]
    async fn completion_404_locks_only_that_model_and_next_request_skips_the_key() {
        let bad = spawn_mock(404, r#"{"error":{"message":"model not found"}}"#).await;
        let good = spawn_mock(200, r#"{"ok":true}"#).await;
        let dir = temp_dir("mlock_e2e");
        let store = std::sync::Arc::new(
            Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap(),
        );
        store.upsert_key(key("k1", 0, &bad)).unwrap();
        store.upsert_key(key("k2", 1, &good)).unwrap();
        store.secrets.write().set("k1", "x").unwrap();
        store.secrets.write().set("k2", "y").unwrap();

        let pm = ProxyManager::new(store.clone());
        let port = pm.start(CategoryType::ClaudeCli).await.unwrap();
        let send = |model: &'static str| {
            let url = format!("http://127.0.0.1:{port}/v1/messages");
            async move {
                reqwest::Client::new()
                    .post(url)
                    .json(&json!({"model":model,"max_tokens":10,"messages":[{"role":"user","content":"hi"}]}))
                    .send()
                    .await
                    .unwrap()
            }
        };

        // 第一次：k1 回 404 → 锁 gpt-9 这个模型 → 转移到 k2 成功
        let r1 = send("gpt-9").await;
        assert_eq!(r1.status().as_u16(), 200);
        assert_eq!(
            synaroute_headers(&r1).get("x-synaroute-attempts").map(String::as_str),
            Some("2"),
            "第一次应该试了 k1（404）再转到 k2"
        );

        let h1 = store.get_key("k1").unwrap().health;
        assert_eq!(h1.fail_count, 0, "404 不该累加 Key 级计数");
        assert!(h1.breaker_until.is_none(), "404 不该熔断整条 Key");
        assert!(h1.model_locks.contains_key("gpt-9"), "应锁住 gpt-9，实际 {:?}", h1.model_locks);

        // 第二次：同一个模型 → k1 已被模型锁挡住，**一次就命中 k2**
        let r2 = send("gpt-9").await;
        assert_eq!(r2.status().as_u16(), 200);
        let hs = synaroute_headers(&r2);
        assert_eq!(
            hs.get("x-synaroute-attempts").map(String::as_str),
            Some("1"),
            "被锁的 Key 应被跳过、一次命中 —— 若仍是 2，说明模型锁没进候选筛选"
        );
        assert_eq!(hs.get("x-synaroute-key-id").map(String::as_str), Some("k2"));

        pm.stop(CategoryType::ClaudeCli);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 🔴 模型锁的键必须是**真实模型名**，不是客户端要的对外名。
    ///
    /// 同一条 Key 上把两个对外名映射到同一个真实模型是常见配置。若锁在对外名上，
    /// 换一个对外名就能绕过锁 —— 那这层形同虚设，而且失效是静默的（没人会发现
    /// 「换了个别名就又开始白打 404 了」）。
    #[tokio::test]
    async fn the_model_lock_is_keyed_by_the_upstream_name_not_the_client_facing_alias() {
        let bad = spawn_mock(404, r#"{"error":"nope"}"#).await;
        let good = spawn_mock(200, r#"{"ok":true}"#).await;
        let dir = temp_dir("mlock_alias");
        let store = std::sync::Arc::new(
            Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap(),
        );
        // k1：两个不同的对外名 → **同一个**真实模型
        let mut k1 = key("k1", 0, &bad);
        k1.mappings = vec![
            crate::model::ModelMapping {
                id: "m1".into(),
                expected_name: "alias-A".into(),
                real_name: "upstream-X".into(),
            },
            crate::model::ModelMapping {
                id: "m2".into(),
                expected_name: "alias-B".into(),
                real_name: "upstream-X".into(),
            },
        ];
        store.upsert_key(k1).unwrap();
        store.upsert_key(key("k2", 1, &good)).unwrap();
        store.secrets.write().set("k1", "x").unwrap();
        store.secrets.write().set("k2", "y").unwrap();

        let pm = ProxyManager::new(store.clone());
        let port = pm.start(CategoryType::ClaudeCli).await.unwrap();
        let send = |model: &'static str| {
            let url = format!("http://127.0.0.1:{port}/v1/messages");
            async move {
                reqwest::Client::new()
                    .post(url)
                    .json(&json!({"model":model,"max_tokens":10,"messages":[{"role":"user","content":"hi"}]}))
                    .send()
                    .await
                    .unwrap()
            }
        };

        // 用 alias-A 触发锁
        let r1 = send("alias-A").await;
        assert_eq!(r1.status().as_u16(), 200, "应转移到 k2");
        let locks = store.get_key("k1").unwrap().health.model_locks;
        assert!(
            locks.contains_key("upstream-X"),
            "锁的键应是真实模型名 upstream-X，实际 {:?}",
            locks.keys().collect::<Vec<_>>()
        );
        assert!(!locks.contains_key("alias-A"), "不该锁在对外名上");

        // 换成 alias-B（映射到同一个真实模型）→ k1 仍必须被跳过
        let r2 = send("alias-B").await;
        assert_eq!(r2.status().as_u16(), 200);
        assert_eq!(
            synaroute_headers(&r2).get("x-synaroute-attempts").map(String::as_str),
            Some("1"),
            "换个对外名不该绕过模型锁 —— 若这里是 2，说明锁键用的是对外名"
        );

        pm.stop(CategoryType::ClaudeCli);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 无处可切时，模型锁也要走兜底 —— 不能因为「唯一那条 Key 的这个模型被锁了」就直接 503。
    ///
    /// 与 Key 级熔断的兜底完全同一判据（见 `Store::candidates_for` 文档）：
    /// 锁是为「多 Key 快速切换」设计的，无处可切时不该自杀。
    #[tokio::test]
    async fn a_locked_model_still_falls_back_when_it_is_the_only_key() {
        let up = spawn_mock(404, r#"{"error":"nope"}"#).await;
        let dir = temp_dir("mlock_fallback");
        let store = std::sync::Arc::new(
            Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap(),
        );
        store.upsert_key(key("k1", 0, &up)).unwrap();
        store.secrets.write().set("k1", "x").unwrap();

        let pm = ProxyManager::new(store.clone());
        let port = pm.start(CategoryType::ClaudeCli).await.unwrap();
        let send = || {
            let url = format!("http://127.0.0.1:{port}/v1/messages");
            async move {
                reqwest::Client::new()
                    .post(url)
                    .json(&json!({"model":"m","max_tokens":10,"messages":[{"role":"user","content":"hi"}]}))
                    .send()
                    .await
                    .unwrap()
            }
        };

        let r1 = send().await;
        assert!(!r1.status().is_success());
        assert!(
            store.get_key("k1").unwrap().health.model_locks.contains_key("m"),
            "前置条件：模型已被锁"
        );

        // 第二次：唯一候选被模型锁挡住 → 必须走兜底再试它一次，而不是「无可用 Key」
        let r2 = send().await;
        let hs = synaroute_headers(&r2);
        assert_eq!(
            hs.get("x-synaroute-attempts").map(String::as_str),
            Some("1"),
            "兜底应仍然试那条唯一的 Key（attempts=0 意味着一条都没试 → 直接判死）"
        );
        assert_eq!(
            hs.get("x-synaroute-upstream-status").map(String::as_str),
            Some("404"),
            "应真的打到了上游（拿到 404），而不是本地直接拒绝"
        );

        pm.stop(CategoryType::ClaudeCli);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 401 仍然走 Key 级：坏密钥必须能被切走，不能被降级成「只是某个模型不行」。
    ///
    /// 这条是分层的**反向防线**：分层的风险是把该罚 Key 的也只罚了模型，
    /// 于是一条废密钥永远赖在候选池首位。
    #[tokio::test]
    async fn a_401_still_penalizes_the_key_not_just_the_model() {
        let bad = spawn_mock(401, r#"{"error":"bad key"}"#).await;
        let good = spawn_mock(200, r#"{"ok":true}"#).await;
        let dir = temp_dir("mlock_401");
        let store = std::sync::Arc::new(
            Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap(),
        );
        store.upsert_key(key("k1", 0, &bad)).unwrap();
        store.upsert_key(key("k2", 1, &good)).unwrap();
        store.secrets.write().set("k1", "x").unwrap();
        store.secrets.write().set("k2", "y").unwrap();

        let pm = ProxyManager::new(store.clone());
        let port = pm.start(CategoryType::ClaudeCli).await.unwrap();
        for _ in 0..3 {
            let _ = reqwest::Client::new()
                .post(format!("http://127.0.0.1:{port}/v1/messages"))
                .json(&json!({"model":"m","max_tokens":10,"messages":[{"role":"user","content":"hi"}]}))
                .send()
                .await
                .unwrap();
        }
        let h = store.get_key("k1").unwrap().health;
        assert!(
            h.breaker_until.is_some(),
            "连续 401 必须熔断整条 Key（fail_count={}）—— 若只锁了模型，废密钥就切不走了",
            h.fail_count
        );
        assert!(
            h.model_locks.is_empty(),
            "401 不该产生模型锁：它跟模型无关"
        );

        pm.stop(CategoryType::ClaudeCli);
        std::fs::remove_dir_all(&dir).ok();
    }
}
