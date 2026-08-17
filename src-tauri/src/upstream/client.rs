//! HTTP 传输层：共享客户端、超时、鉴权头与客户端身份。
//!
//! 三个 UA 常量刻意**保持私有**（只有 apply_client_identity 用）：它们的值是
//! 403 client_restricted 的实测判据，收窄可见性正好防止别处随手引用后各自改。

use crate::error::{AppError, AppResult};
use crate::model::{Protocol, ProviderKey};
use std::time::Duration;

/// 上游临时错误自动重试的最大尝试次数（含首次）。
pub(super) const RETRY_MAX_ATTEMPTS: u32 = 3;
/// 重试基础退避（毫秒），按尝试次数线性递增（300 / 600 ...）。
pub(super) const RETRY_BASE_BACKOFF_MS: u64 = 300;

/// 聚合自建请求的客户端标识头。部分中转渠道靠 UA / originator 做客户端准入校验，不带会被判
/// `detected: unknown` 而 403（channel:client_restricted）。这里对齐**官方客户端真实格式**，
/// 使聚合请求也能通过准入（见 [`apply_client_identity`]）。
///
/// - OpenAI 侧对齐 Codex CLI：UA 形如 `codex_cli_rs/<ver> (<os>; <arch>) <term>`，并附带
///   独立 `originator: codex_cli_rs` 头——new-api 类中转的 client_restricted 检测通常查的就是
///   originator / UA 里的 `codex_cli_rs` 标识（源自 codex-rs default_client.rs 的 get_codex_user_agent）。
/// - Anthropic 侧对齐 Claude Code CLI 真实 UA 形态。
const ANTHROPIC_CLIENT_UA: &str = "claude-cli/1.0.0 (external, cli)";
const OPENAI_CLIENT_UA: &str =
    "codex_cli_rs/0.55.0 (Windows 11; x86_64) SynaRoute";
/// Codex 的客户端来源标识头值（default_client.rs `DEFAULT_ORIGINATOR`）。
const CODEX_ORIGINATOR: &str = "codex_cli_rs";


/// 判断上游错误是否「临时性、值得重试」：429 限流、502/503/504 网关、529 过载，以及连接层失败。
/// 其余 4xx（401/403/404/422…）是鉴权或参数问题，重试无意义。
///
/// **按结构化状态码判定，不做文本嗅探。** 旧实现是
/// `msg.contains("HTTP 502") || … || msg.contains("连接")`，而 `msg` 里**拼进了上游响应体
/// 前 400 字符**（见 `anthropic_message` / `openai_chat` 的错误构造），于是：
/// - 误判：上游返 401 而响应体含中转商常见文案「请检查网络连接后重试」→ 命中 `"连接"` →
///   白重试 3 次并线性退避。聚合场景下每个成员各白等一轮，直接吃掉整轮
///   `total_timeout_ms`，用户看到「聚合超时/部分成员无结果」，真因只是一次鉴权失败。
///   网关回显原始报文里带 `HTTP 429` 字样时同样中招。
/// - 漏判：英文 `connection reset by peer` 不含中文「连接」，本该重试的瞬时抖动被判死。
///
/// 两个方向的错误都由 `retriable_*` 测试用故障注入锁住（去掉状态码判定即变红）。
pub fn is_retriable_upstream_error(e: &AppError) -> bool {
    if !e.is_upstream() {
        return false;
    }
    match e.upstream_status() {
        // 连接层失败（没拿到 HTTP 响应）：传输抖动，值得重试。
        None => true,
        Some(s) => matches!(s, 429 | 502 | 503 | 504 | 529),
    }
}

/// 截断上游响应体用于错误展示（避免超长 body 撑爆日志/错误信息）。
pub(super) fn truncate_body(raw: &str) -> String {
    const CAP: usize = 400;
    let t = raw.trim();
    if t.chars().count() <= CAP {
        t.to_string()
    } else {
        let head: String = t.chars().take(CAP).collect();
        format!("{head}…（已截断，共 {} 字符）", t.chars().count())
    }
}

// ---- 内部工具 ----

/// 连接池 / 保活的公共基线（两个共享客户端共用，避免调参漂移）。
///
/// 取值理由见 [`shared_client`]。**不含**任何解压设置——解压是否开启由各构造点决定：
/// 转发路径要字节透明（[`shared_client`]），自建请求要能读明文（[`decoding_client`]）。
fn base_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        // ---- 连接池与保活：针对「突发 + 长空闲」的桌面代理流量特征 ----
        //
        // 为什么要显式设：reqwest 的 `pool_idle_timeout` 默认 **90 秒**，而桌面代理的
        // 真实流量是「用户问一句 → 想几分钟 → 再问一句」。默认值下每次「想一会儿再问」
        // 都超过 90s，连接已被回收，下一发要重做 TCP 三次握手 + 完整 TLS 握手；
        // 跨境到中转商 RTT 常 100~300ms、完整握手 2~3 个 RTT，即**每次白付
        // 300~900ms 首字节延迟**。这是用户能直接感知的卡顿，且极易被误读成「上游慢」
        // 或「这个 Key 不行」，从而引导错误的排查方向。
        //
        // 取值理由：
        // - `pool_idle_timeout(300s)`：覆盖典型思考间隔（几分钟），比 90s 默认宽裕。
        //   不设更长是因为中转商侧通常也有空闲上限，本地留着已被对端关掉的连接
        //   反而要多付一次失败重连。
        // - `pool_max_idle_per_host(8)`：单请求只用一条连接，8 条足够覆盖故障转移与
        //   健康探测并发；上界存在是为了避免多 Key 场景下空闲连接无限累积。
        // - `tcp_keepalive(60s)`：家用路由器 / NAT 会静默丢弃空闲映射，
        //   没有 keepalive 时表现为「连接看着还在、一写就 reset」。
        //
        // 注：**当前是 HTTP/1.1 连接池**——`Cargo.toml` 里 reqwest 是
        // `default-features = false` 且未开 `http2` feature，故不存在 h2 多路复用，
        // 也就没有 `http2_keep_alive_*` 可设（编译期即报错）。h1 下「一连接一请求」，
        // 靠上面的池上限与保活即可。是否开启 h2 需单独评估：它会改变与**所有**上游的
        // ALPN 协商结果，部分中转商网关对 h2 的行为与 h1 不同，属于需要真机验证的改动，
        // 不应与本轮的纯增益调参混在一起。
        .pool_idle_timeout(Duration::from_secs(300))
        .pool_max_idle_per_host(8)
        .tcp_keepalive(Duration::from_secs(60))
        // ⚠️ 必须显式关掉自动解压。reqwest 的 `Accepts::default()` 对**编译进的**
        // gzip/brotli/deflate feature 一律置 `true`（async_impl/client.rs），与是否调用
        // `.gzip(true)` **无关**——Cargo.toml 为余额/拉模型加了这三个 feature 后，若这里不显式
        // 关闭，转发路径的 `shared_client()` 会被默认翻成「自动解压」：tower-http 还会顺带注入
        // `Accept-Encoding: gzip,br,deflate`，正好抵消 proxy.rs 剥离 accept-encoding 的用意，
        // 且解压后删除 `content-encoding`/`content-length`、把 gzip 解码塞进 SSE 热路径——
        // 破坏「字节透明转发」这条硬约束。故基线一律关；需要解压的 `decoding_client()` 再逐项开。
        .gzip(false)
        .brotli(false)
        .deflate(false)
}

/// 全局共享 HTTP 客户端：复用连接池 / TLS 会话，避免每请求重做 TCP+TLS 握手。
/// 不设总超时（response timeout）——总超时由各调用点按 Key 用 `.timeout()` 逐请求指定；
/// 仅设连接超时（连接层握手上限）。reqwest::Client 内部是 Arc，clone 廉价、共享同一连接池。
///
/// **刻意不开自动解压**：这是**转发路径**（proxy.rs 流式/非流式）用的客户端，必须对上游
/// 响应体字节透明——代理把上游的 body 与 `content-encoding` 头原样转给下游客户端，由下游
/// 自己解压。若在这里开 gzip/brotli，reqwest 会自动解压并删掉 `content-encoding`/
/// `content-length`，导致「头说压缩、体已解压」的不一致，甚至扰乱 SSE 分块。
/// 需要读明文 body 的**自建请求**（余额/拉模型/健康探测/聚合）用 [`decoding_client`]。
pub fn shared_client() -> reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT
        .get_or_init(|| {
            base_builder()
                .build()
                .expect("构建共享 HTTP 客户端失败")
        })
        .clone()
}

/// 带**自动解压**（gzip/brotli/deflate）的共享客户端，供本应用**自己解析响应体**的自建请求
/// 使用：余额查询、拉模型、健康探测、大脑聚合成员/决策者、工具会话。
///
/// 为什么单列一个：这些请求拿到 body 后自己 `resp.text()` + `serde_json::from_str` 解析。
/// 部分中转站/CDN（Cloudflare 等）即便请求未声明 `Accept-Encoding` 也会**主动**返回
/// gzip/br 压缩体；不解压时 `text()` 得到的是压缩字节，解析必然失败并把乱码糊进错误信息
/// （实测「余额查询返回一堆乱码字节」正是此因）。reqwest 开启对应 feature + `.gzip(true)` 等
/// 后：仅在请求头未含 `Accept-Encoding` 时自动补上，并**按响应的 `Content-Encoding`** 透明
/// 解压、移除 `content-encoding`/`content-length`——故对「不问自压」的网关同样生效。
/// 连接池参数与 [`shared_client`] 共用 [`base_builder`]，不丢保活。
///
/// 转发路径**绝不用它**（见 [`shared_client`] 的字节透明约束）。
pub fn decoding_client() -> reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT
        .get_or_init(|| {
            base_builder()
                .gzip(true)
                .brotli(true)
                .deflate(true)
                .build()
                .expect("构建解压 HTTP 客户端失败")
        })
        .clone()
}

/// 某 Key 的单请求总超时（缺省 30s）。可在 Key 编辑器设置（timeoutMs），
/// 服务「非流式转发」这类要等上游完整生成的请求——慢厂商可调大。
pub fn key_timeout(key: &ProviderKey) -> Duration {
    Duration::from_millis(key.params.timeout_ms.unwrap_or(30_000))
}

/// 元数据级快请求（健康探测 / 拉模型列表）的超时：取 key_timeout 但**封顶 8s**。
///
/// 为什么封顶：Key 超时开放用户设置后，慢厂商可能设 300s+；但
/// - 健康检查即便已改为有界并发（`PROBE_CONCURRENCY`），跟随大超时仍会让一轮拖到分钟级，
///   使 UI 徽标与真实状态长时间背离——而它正是用户判断「该换哪条 Key」的依据；
/// - 拉模型按候选端点顺序试（最多 4 个），跟随大超时时「拉取模型」按钮最坏挂 4×超时。
///
/// 这些都是**秒级应答**的元数据 GET / 1-token 探测。docs/02 §6.3 的原始设计口径是 3–5s；
/// 这里取 8s 略宽于它，给跨境高 RTT 链路留余量（连接握手本身就可能 300~900ms），
/// 同时把 6 Key 全不可达的一轮从 180s（串行 30s）压到约 16s（并发 4 × 8s）。
///
/// 注意这只影响**元数据探测**，不影响真实转发——后者走 `key_timeout`，慢厂商仍可设大值。
pub fn fast_timeout(key: &ProviderKey) -> Duration {
    key_timeout(key).min(Duration::from_secs(8))
}

/// 自建请求（拉模型/健康探测/聚合成员/工具会话）用的客户端：走 [`decoding_client`]
/// 以透明解压上游可能主动返回的 gzip/br 响应体（它们都在本地解析 body，见该函数说明）。
/// 保留 `key` 参数与旧签名以最小化调用点改动；总超时改由调用点逐请求 `.timeout()` 指定。
pub(super) fn build_client(key: &ProviderKey) -> AppResult<reqwest::Client> {
    let _ = key;
    Ok(decoding_client())
}

/// 按协议注入鉴权头**与版本头**。
///
/// 版本头（`anthropic-version`）收敛进来（P2-2）：它原先由本函数的三个调用点各自补
/// （`:573` / `:1225` / `:1402`），而 proxy 侧的两条转发路径又各写一遍 —— 五处实现、
/// 已经分叉过一次。现在协议→版本头的映射是 `Protocol::version_header()` 这一处**穷举 match**，
/// 加第 4 种协议时编译器会要求明确回答「它要不要版本头」。
pub(super) fn apply_auth(
    req: reqwest::RequestBuilder,
    key: &ProviderKey,
    secret: &str,
) -> reqwest::RequestBuilder {
    let scheme = key.protocol.auth_scheme();
    let mut req = req.header(scheme.header_name(), scheme.header_value(secret));
    if let Some((h, v)) = key.protocol.version_header() {
        req = req.header(h, v);
    }
    req
}

/// 为**自建请求**注入客户端身份头（User-Agent 等）。
///
/// 为什么需要：透传路径会转发下游客户端（Claude Code / Codex）的原始 UA/x-app 等头，
/// 部分中转渠道靠这些做客户端准入校验。但自建请求若不带任何客户端标识，
/// 会被这类渠道判为 `detected: unknown` 而 403（channel:client_restricted）。这里按协议
/// 补一个与官方客户端一致的 UA（及 Anthropic 的 anthropic-beta / x-app），使自建请求也能
/// 通过客户端准入。仅注入身份头，不动鉴权与业务字段。
///
/// **凡是本应用自建的上游请求都要过这一道**，目前三类调用方：
/// 聚合成员/决策者（`completion.rs`）、工具会话（`session.rs`）、
/// 余额查询（`balance.rs`）。余额查询就是漏了它才被中转站 403 —— 同一个坑踩了第二次，
/// 故把可见性从 `pub(super)` 提到 `pub`（`balance` 在 upstream 外面），
/// 并登记进 `upstream_api_surface` 守卫，让它成为有名有姓的对外契约。
pub fn apply_client_identity(
    req: reqwest::RequestBuilder,
    protocol: Protocol,
) -> reqwest::RequestBuilder {
    if protocol.is_openai() {
        // Codex CLI 风格 UA + originator 头——中转渠道的 client_restricted 检测通常查这两者里的
        // `codex_cli_rs` 标识；缺失即被判 detected:unknown 而 403。
        req.header("user-agent", OPENAI_CLIENT_UA)
            .header("originator", CODEX_ORIGINATOR)
    } else {
        // Claude Code CLI 风格 UA + 常见随附头，匹配"仅放行 Claude Code"类渠道。
        req.header("user-agent", ANTHROPIC_CLIENT_UA)
            .header("x-app", "cli")
            .header("anthropic-beta", "claude-code-20250219")
    }
}

/// /models 探测专用鉴权：同时带 `Authorization: Bearer` 与 `x-api-key`。
/// 兼容厂商（DeepSeek/Kimi/GLM 等）把 Anthropic 协议挂在子路径、但模型列表是 OpenAI 风格
/// （需 Bearer）；而真 Anthropic 的 /v1/models 需 x-api-key。GET /models 只读，多带一个
/// 不被识别的鉴权头无害，故两个都带以最大化兼容（借鉴 cc-switch 对 /models 统一用 Bearer 的思路，
/// 并叠加 x-api-key 以不牺牲真 Anthropic）。
pub(super) fn apply_models_auth(req: reqwest::RequestBuilder, secret: &str) -> reqwest::RequestBuilder {
    req.header("authorization", format!("Bearer {secret}"))
        .header("x-api-key", secret)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 重试判定必须只看**结构化状态码**，不做文本嗅探。
    ///
    /// 故障注入判据：把 `is_retriable_upstream_error` 改回
    /// `msg.contains("HTTP 502") || … || msg.contains("连接")`，前两条断言立刻变红。
    ///
    /// 这两条是真实故障形态，不是臆造的边界：
    /// - 中转商鉴权失败时响应体常写「请检查网络连接后重试」→ 命中 `"连接"` → 401 被
    ///   当成临时错误白跑 3 次退避。聚合场景下每个成员各白等一轮，直接吃掉整轮墙钟预算，
    ///   用户看到「聚合超时」，真因只是 Key 失效。
    /// - 网关把上游原始报文回显进响应体（含 `HTTP 429` 字样）时同样中招。
    #[test]
    fn retriable_judges_by_status_not_message_text() {
        // 401 + 响应体含中文「连接」：绝不可重试（旧实现会误判为可重试）
        let auth_fail_with_connection_text = AppError::upstream_http(
            401,
            "Anthropic HTTP 401: {\"error\":\"invalid api key，请检查网络连接后重试\"}",
        );
        assert!(
            !is_retriable_upstream_error(&auth_fail_with_connection_text),
            "401 是鉴权问题，重试无意义——不能因响应体里有「连接」二字就重试"
        );

        // 401 + 响应体回显 `HTTP 429`：仍然不可重试
        let auth_fail_echoing_429 =
            AppError::upstream_http(401, "OpenAI HTTP 401: upstream returned HTTP 429 earlier");
        assert!(
            !is_retriable_upstream_error(&auth_fail_echoing_429),
            "真实状态码是 401，不能因 body 里回显了 HTTP 429 就重试"
        );

        // 连接层失败（无状态码）：值得重试。旧实现靠英文子串匹配，
        // `connection reset by peer` 不含中文「连接」会被漏判。
        assert!(
            is_retriable_upstream_error(&AppError::upstream_msg("connection reset by peer")),
            "连接层失败应重试（且不依赖消息语言）"
        );

        // 真正的临时错误：按状态码放行
        for s in [429u16, 502, 503, 504, 529] {
            assert!(
                is_retriable_upstream_error(&AppError::upstream_http(s, "x")),
                "HTTP {s} 应判为可重试"
            );
        }
        // 其余 4xx 与 2xx 解析失败：不重试
        for s in [400u16, 401, 403, 404, 422, 200] {
            assert!(
                !is_retriable_upstream_error(&AppError::upstream_http(s, "x")),
                "HTTP {s} 不应判为可重试"
            );
        }

        // 非 Upstream 变体一律不重试
        assert!(!is_retriable_upstream_error(&AppError::Invalid("bad".into())));
        assert!(!is_retriable_upstream_error(&AppError::NotFound("x".into())));
    }

    /// `Display` 格式必须逐字保持 `上游请求错误: {msg}`——前端文案与 docs 里的实测
    /// 字符串都按这个格式对过，改了会连带破坏那些判据。
    #[test]
    fn upstream_display_format_is_stable() {
        let e = AppError::upstream_http(429, "OpenAI HTTP 429: rate limited");
        assert_eq!(e.to_string(), "上游请求错误: OpenAI HTTP 429: rate limited");
        assert_eq!(e.upstream_status(), Some(429));
        let c = AppError::upstream_msg("连接 https://x 失败");
        assert_eq!(c.to_string(), "上游请求错误: 连接 https://x 失败");
        assert_eq!(c.upstream_status(), None, "连接层失败无状态码");
    }

    #[test]
    fn truncate_body_caps_long_text() {
        let short = "abc";
        assert_eq!(truncate_body(short), "abc");
        let long: String = "x".repeat(500);
        let out = truncate_body(&long);
        assert!(out.contains("已截断"));
        assert!(out.chars().count() < 500);
    }

    /// 转发用的 `shared_client()` **必须**字节透明：不主动索要压缩（不发 `Accept-Encoding`），
    /// 从而上游返回未压缩体、代理原样转发。而自建请求用的 `decoding_client()` **必须**主动
    /// 索要并透明解压（发 `Accept-Encoding`）。
    ///
    /// 这条为一个实测回归立的护栏：Cargo.toml 给 reqwest 加 gzip/brotli/deflate feature 后，
    /// reqwest 的 `Accepts::default()` 会把这些 feature 一律置 true —— 若 `base_builder()` 不
    /// 显式 `.gzip(false)…`，`shared_client()` 会被默认翻成「自动解压 + 注入 Accept-Encoding」，
    /// 悄悄破坏转发字节透明。**故障注入判据**：删掉 base_builder 里的 `.gzip(false).brotli(false)
    /// .deflate(false)`，本测试的「shared_client 不发 Accept-Encoding」断言立刻变红。
    ///
    /// 用真实 TCP 监听回读请求头（而非验代码写了哪一行）：起一个只读一个请求头块的极简 server，
    /// 分别用两个 client 发 GET，比对是否出现 `accept-encoding`。
    #[test]
    fn shared_client_is_transparent_decoding_client_requests_compression() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        // 起一个只应答一次的极简 HTTP server，返回捕获到的请求头。
        fn serve_once(listener: TcpListener) -> std::thread::JoinHandle<String> {
            std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("accept");
                let mut buf = [0u8; 2048];
                let n = stream.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}",
                );
                let _ = stream.flush();
                req
            })
        }

        let rt = tokio::runtime::Runtime::new().unwrap();

        // shared_client：不应出现 accept-encoding
        let l1 = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr1 = l1.local_addr().unwrap();
        let h1 = serve_once(l1);
        rt.block_on(async {
            let _ = shared_client()
                .get(format!("http://{addr1}/"))
                .timeout(Duration::from_secs(5))
                .send()
                .await;
        });
        let req1 = h1.join().unwrap().to_ascii_lowercase();
        assert!(
            !req1.contains("accept-encoding"),
            "shared_client 必须字节透明：不得发 Accept-Encoding，实际请求头:\n{req1}"
        );

        // decoding_client：应主动索要压缩
        let l2 = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr2 = l2.local_addr().unwrap();
        let h2 = serve_once(l2);
        rt.block_on(async {
            let _ = decoding_client()
                .get(format!("http://{addr2}/"))
                .timeout(Duration::from_secs(5))
                .send()
                .await;
        });
        let req2 = h2.join().unwrap().to_ascii_lowercase();
        assert!(
            req2.contains("accept-encoding"),
            "decoding_client 必须主动索要压缩（否则遇主动压缩的中转站又会乱码），实际请求头:\n{req2}"
        );
    }
}
