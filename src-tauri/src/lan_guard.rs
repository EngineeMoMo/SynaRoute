//! 局域网入站鉴权。**开启「局域网暴露」后唯一挡住白嫖的东西。**
//!
//! # 修的是什么
//!
//! `lan_exposure` 一开，`ProxyManager::start` 就把监听地址从 `127.0.0.1` 换成 `0.0.0.0`
//! （`proxy.rs`）。而转发路径会把下游带来的 `authorization` / `x-api-key`
//! **原样剥掉**（`is_stripped_header`）再换上用户自己的真实 Key —— 那是对的，
//! 客户端本来就不该知道上游密钥。
//!
//! 但这两件事凑在一起就是个洞：**剥掉之前从不校验**。于是同网段任何人往
//! `http://<你的内网 IP>:47100/v1/messages` 发一个请求，代理就会拿用户的付费 Key
//! 去上游跑一趟。不需要密码、不需要知道任何配置。
//!
//! # 判据：只对**非 loopback** 的对端强制令牌
//!
//! 本机请求（`127.0.0.1` / `::1`）一律放行。理由不是省事，是代价不对称：
//! - 本机已有操作系统的用户隔离；能在你机器上跑进程的人有更直接的路子。
//! - 要求本机也带令牌，就得同步改 **三份客户端配置**（Claude CLI / 桌面端 / Codex），
//!   而那三份是我们自己写的 —— 任何一份漏改都表现为「接入完成但一直 401」，
//!   而用户完全无从判断是配置问题还是代理坏了。
//!
//! 故：loopback → 放行；非 loopback → 必须带对的令牌，否则 401。
//!
//! # 令牌存在 `secrets.enc` 而不是 `config.json`
//!
//! 它是凭据，不是偏好。两条具体理由：
//! - `AppSettings` 里的字段会被前端 `saveSettings` 批量提交覆盖 —— 白名单漏一个键就被
//!   `#[serde(default)]` 补成空串。那类缺陷本仓已经吃过两次（见 `model.rs` 里
//!   `UserPrefs` 上方那段注释），而在**这里**失效的方向是「鉴权静默失效」。
//! - 主口令锁定时 `SecretStore::get` 返 `Err`（刻意，见 CLAUDE.md），本模块把 Err 当拒绝,
//!   于是锁定态下局域网自动 **fail closed**，不需要额外写一行逻辑。

use crate::route_meta;
use crate::store::Store;
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use std::net::SocketAddr;
use std::sync::Arc;

/// 令牌在 `SecretStore` 里的 id。
///
/// 前缀 `__` 与真实 Key 的 UUID 形态区分开：`ProviderKey.id` 一律是 uuid，
/// 故这个 id 不可能与任何一条 Key 撞上，也不会被「孤儿密钥清理」当成孤儿删掉
/// （那条逻辑按 `keys` 列表比对，而它本就不在 `keys` 里 —— 见下方 `token_id_is_frozen`
/// 那条测试对这个不变量的说明）。
pub(crate) const TOKEN_ID: &str = "__lan_access_token";

/// 鉴权结论。用枚举而不是 `bool`：调用方要能区分「放行」与「为什么拒」，
/// 而拒绝原因决定了要不要落事件（扫描器打过来的和用户配错的，价值完全不同）。
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// 本机来源，或令牌正确。
    Allow,
    /// 非本机且令牌不对 / 没带。
    Deny,
}

/// 纯判定：给定对端地址、下游出示的凭据、期望的令牌 → 放行或拒绝。
///
/// 抽成纯函数是为了能穷举测（并发安全、无 IO）。真正读密钥库的是 [`guarded`]。
pub(crate) fn verdict(peer: &SocketAddr, presented: Option<&str>, expected: Option<&str>) -> Verdict {
    if peer.ip().is_loopback() {
        return Verdict::Allow;
    }
    // 没有令牌可比 → 一律拒。**不能**退化成「没设令牌就放行」——
    // 那正是本缺陷的形态，而且它的失效方向是静默的（功能照常工作、防线没了）。
    let Some(expected) = expected.filter(|t| !t.is_empty()) else {
        return Verdict::Deny;
    };
    match presented {
        Some(p) if constant_time_eq(p, expected) => Verdict::Allow,
        _ => Verdict::Deny,
    }
}

/// 定长时间比较。避免用 `==` 逐字节短路，让攻击者靠响应时间差逐位试出令牌。
///
/// 长度不同也要走完循环 —— 提前 `return false` 会把长度泄露出去。
fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut diff = (a.len() ^ b.len()) as u8;
    for i in 0..a.len().max(b.len()) {
        diff |= a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(0);
    }
    diff == 0
}

/// 从下游请求里取出它出示的凭据。
///
/// 两种形态都认：`authorization: Bearer <token>` 与 `x-api-key: <token>`。
/// 不同客户端习惯不同（Anthropic 系用后者、OpenAI 系用前者），只认一种会让另一半用户
/// 卡在 401 上而看不出原因。
fn presented_token(req: &Request<Incoming>) -> Option<String> {
    let h = req.headers();
    if let Some(v) = h.get("x-api-key").and_then(|v| v.to_str().ok()) {
        let v = v.trim();
        if !v.is_empty() {
            return Some(v.to_string());
        }
    }
    let auth = h.get(hyper::header::AUTHORIZATION)?.to_str().ok()?.trim();
    let bare = auth.strip_prefix("Bearer ").or_else(|| auth.strip_prefix("bearer "));
    let v = bare.unwrap_or(auth).trim();
    (!v.is_empty()).then(|| v.to_string())
}

/// 开启局域网时预先备好令牌。由 `ProxyManager::start` 在 `lan_exposure` 为真时调用。
///
/// 不这么做的话，令牌要等**第一个局域网请求被拒**时才生成 —— 而在那之前用户完全
/// 不知道要往客户端里填什么，只能先撞一次 401。返回值刻意忽略：
/// 备不出来（如主口令锁定）不该阻止代理启动，本机转发仍应可用，
/// 而局域网那侧会在 `guarded` 里 fail closed。
pub(crate) fn ensure_token(store: &Arc<Store>) {
    let _ = token_or_create(store);
}

/// 取当前令牌；没有就**就地生成并落盘**，同时落一条带令牌的事件。
///
/// 惰性生成而不是在 `set_lan_exposure` 里生成：那样「开关已开但令牌没生成」
/// （比如老用户升级上来、配置里 `lan_exposure` 本就是 true）会变成一个静默的空洞，
/// 而这里的 `None → Deny` 虽然安全，用户却只看到 401 而不知道令牌是什么。
///
/// 事件里必须带明文令牌。设置页现在也能看/复制（B5，见 `read_lan_token_from`），
/// 但事件仍是**跨时间**的那份留存 —— 用户关掉窗口、或事后才想起要配另一台机器时靠它。
/// 那条事件写进请求日志里 —— 与「日志里不许出现上游密钥」不冲突：这不是上游密钥，
/// 它只能用来访问本机代理，且用户必须能抄到它才能把局域网客户端配起来。
fn token_or_create(store: &Arc<Store>) -> Option<String> {
    match store.secrets.read().get(TOKEN_ID) {
        // 锁定态返 Err → 直接拒（fail closed），不要在这里生成新令牌：
        // 那会往一个锁着的库里写，且会把用户原有的令牌顶掉。
        Err(_) => return None,
        Ok(Some(t)) if !t.is_empty() => return Some(t.to_string()),
        Ok(_) => {}
    }
    let token = new_token();
    if store.secrets.write().set(TOKEN_ID, &token).is_err() {
        return None;
    }
    store.append_event(
        crate::model::CategoryType::ClaudeCli,
        "config",
        None,
        &format!(
            "已为「局域网暴露」生成接入令牌：{token} —— \
             局域网客户端必须把它填进 API Key（或 Authorization: Bearer）才能使用；\
             本机客户端不受影响。"
        ),
    );
    Some(token)
}

/// 读当前令牌，供设置页显示 / 复制（B5）。
///
/// 🔴 **只读，绝不生成** —— 与 [`token_or_create`] 的分工是本函数存在的全部理由。
/// 若读一下就生成，那么「打开设置页」这个纯查看动作会给一个**没开局域网**的用户
/// 凭空造出一条密钥库条目；更糟的是它会走到「已有令牌」分支之外去，
/// 把 `ensure_token` 的单一生成点变成两个。
///
/// 🔴 **锁定态返 `Err` 而不是 `Ok(None)`** —— 这两者在界面上必须长得不一样：
/// `Ok(None)` = 「还没有令牌」→ UI 引导用户去生成；而主口令锁着时其实**可能已有令牌**，
/// 把它显示成「还没有」会让用户点下「重新生成」，于是**所有已配好的局域网客户端立刻 401**。
/// 同 `SecretStore::get` 在锁定态返 Err 的那条刻意行为（CLAUDE.md 有记）。
/// ⚠️ **各分支里只许返回值，不许再取锁**：`match` 的 scrutinee 是
/// `store.secrets.read()` 的临时 guard，它在**整个 match 期间存活** ——
/// 在分支里取 `.write()` 会 RwLock 自锁、当场挂死（写这个功能的注入验证时踩过，
/// 一条测试挂了十分钟才发现不是「注入无效」而是「进程挂死」）。
pub(crate) fn read_lan_token_from(store: &Arc<Store>) -> Result<Option<String>, String> {
    match store.secrets.read().get(TOKEN_ID) {
        Ok(Some(t)) if !t.is_empty() => Ok(Some(t.to_string())),
        Ok(_) => Ok(None),
        Err(_) => Err("密钥库当前不可读（主口令已锁定？解锁后再查看令牌）".into()),
    }
}

/// 重新生成令牌（B5）。**破坏性**：旧令牌立即失效，已配好的局域网客户端会开始 401。
///
/// 落一条带新令牌的事件：与 `token_or_create` 同一个理由 —— 那是令牌在界面之外
/// **唯一**的留存处。这里还多一层价值：用户若关掉对话框忘了抄，日志里还能找回来。
/// 事件文案里必须点明「旧的已失效」，否则用户不知道要去更新客户端。
pub(crate) fn regenerate_lan_token_in(store: &Arc<Store>) -> Result<String, String> {
    // 先确认库可写再动手：锁定态下直接返错，别落一条「已生成」的事件却没真写进去。
    read_lan_token_from(store)?;
    let token = new_token();
    store
        .secrets
        .write()
        .set(TOKEN_ID, &token)
        .map_err(|e| format!("写入密钥库失败：{e}"))?;
    store.append_event(
        crate::model::CategoryType::ClaudeCli,
        "config",
        None,
        &format!(
            "已**重新生成**「局域网暴露」接入令牌：{token} —— \
             旧令牌立即失效，请更新所有局域网客户端的 API Key；本机客户端不受影响。"
        ),
    );
    Ok(token)
}

/// 生成令牌。两个 uuid v4 拼接（去掉连字符）= 64 个十六进制字符、约 244 位熵。
///
/// 刻意复用 `uuid`（已在依赖树里、`proxy.rs` 就在用）而不是引入 `rand`/`getrandom`：
/// 为一个令牌新增依赖不值得，而 v4 本就是 CSPRNG 来源。
fn new_token() -> String {
    let a = uuid::Uuid::new_v4().simple().to_string();
    let b = uuid::Uuid::new_v4().simple().to_string();
    format!("{a}{b}")
}

/// 401 响应。**不回显期望的令牌**，也不说「你带的是什么」——那等于给试探者反馈。
fn deny_response(store: &Arc<Store>, peer: &SocketAddr) -> Response<crate::proxy::ResBody> {
    log_denied_once(store, peer);
    let body = serde_json::json!({
        "error": {
            "type": "unauthorized",
            "message": "局域网访问需要接入令牌。请把 SynaRoute 的「局域网接入令牌」\
                        填进客户端的 API Key（或 Authorization: Bearer）。\
                        令牌在应用的日志页里（搜「接入令牌」）。本机访问无需令牌。"
        }
    });
    let mut resp = Response::new(crate::proxy::full_body(bytes::Bytes::from(body.to_string())));
    *resp.status_mut() = StatusCode::UNAUTHORIZED;
    resp.headers_mut()
        .insert("content-type", hyper::header::HeaderValue::from_static("application/json"));
    // 也挂诊断头，保持「所有出口都带头」这条性质（见 route_meta 模块注释）。
    route_meta::attach(&mut resp, &route_meta::RouteMeta::default());
    resp
}

/// 被拒的来源**每个 IP 每次运行只记一条**。
///
/// 🔴 不节流就会被端口扫描器刷满 —— `MAX_EVENTS` 是个环，噪音会把真正有用的事件挤出去。
/// 本仓已经吃过两次同样的教训（短路窗口每次重发记一条、MCP 分类回落每次记一条）。
fn log_denied_once(store: &Arc<Store>, peer: &SocketAddr) {
    use std::collections::HashSet;
    use std::sync::Mutex;
    use std::sync::OnceLock;
    static SEEN: OnceLock<Mutex<HashSet<std::net::IpAddr>>> = OnceLock::new();
    let seen = SEEN.get_or_init(|| Mutex::new(HashSet::new()));
    let Ok(mut set) = seen.lock() else { return };
    if !set.insert(peer.ip()) {
        return;
    }
    store.append_event(
        crate::model::CategoryType::ClaudeCli,
        "system",
        None,
        &format!(
            "已拒绝来自 {} 的局域网请求（未带正确的接入令牌）。\
             本次运行内该来源不再重复记录。",
            peer.ip()
        ),
    );
}

/// 包住 [`crate::proxy::handle_request`] 的鉴权层。
///
/// 返回值直接交给 hyper 的 `serve_connection`。把整层收在这里（而不是在 `proxy.rs`
/// 的 accept 循环里展开）有两个好处：`proxy.rs` 那侧只剩一行调用、生产段行数反而下降；
/// 以及鉴权与转发的边界在类型上就分开了 —— 想绕过鉴权就得改这里，不会「顺手」发生。
pub(crate) fn guarded(
    store: Arc<Store>,
    category: crate::model::CategoryType,
    gate_key: String,
    peer: SocketAddr,
) -> impl hyper::service::Service<
    Request<Incoming>,
    Response = Response<crate::proxy::ResBody>,
    Error = hyper::Error,
    Future = impl std::future::Future<Output = Result<Response<crate::proxy::ResBody>, hyper::Error>>,
> {
    service_fn(move |req: Request<Incoming>| {
        let store = store.clone();
        let gate_key = gate_key.clone();
        async move {
            // loopback 快路径：不读密钥库、不解密，本机转发的开销与改动前一致。
            if !peer.ip().is_loopback() {
                let expected = token_or_create(&store);
                let presented = presented_token(&req);
                if verdict(&peer, presented.as_deref(), expected.as_deref()) == Verdict::Deny {
                    return Ok(deny_response(&store, &peer));
                }
            }
            crate::proxy::handle_request(store, category, gate_key, req).await
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    fn lan() -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 77)), 51234)
    }
    fn local_v4() -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 51234)
    }
    fn local_v6() -> SocketAddr {
        SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 51234)
    }

    /// 🔴 缺陷本体：局域网来源不带令牌必须被拒。去掉鉴权这条就红。
    #[test]
    fn lan_without_token_is_denied() {
        assert_eq!(verdict(&lan(), None, Some("tok")), Verdict::Deny);
        assert_eq!(verdict(&lan(), Some(""), Some("tok")), Verdict::Deny);
        assert_eq!(verdict(&lan(), Some("wrong"), Some("tok")), Verdict::Deny);
    }

    #[test]
    fn lan_with_correct_token_is_allowed() {
        assert_eq!(verdict(&lan(), Some("tok"), Some("tok")), Verdict::Allow);
    }

    /// 本机一律放行 —— 这条撑着「三份客户端配置不用改」那个决定。
    /// 它变红意味着 Claude CLI / 桌面端 / Codex 会集体 401。
    #[test]
    fn loopback_is_always_allowed_even_with_no_token_configured() {
        for peer in [local_v4(), local_v6()] {
            assert_eq!(verdict(&peer, None, None), Verdict::Allow, "{peer} 应放行");
            assert_eq!(verdict(&peer, Some("whatever"), Some("tok")), Verdict::Allow);
        }
    }

    /// 🔴 **没有令牌时必须拒，不能退化成放行。**
    /// 「没设令牌就放行」正是本缺陷的形态，且失效方向是静默的
    /// —— 功能照常工作、防线没了，没有任何报错。
    #[test]
    fn missing_or_blank_expected_token_denies_lan_rather_than_opening_up() {
        for expected in [None, Some(""), Some("   ")] {
            let v = verdict(&lan(), Some("anything"), expected);
            assert_eq!(v, Verdict::Deny, "expected={expected:?} 时应拒绝局域网");
        }
    }

    /// 定长比较：长度不同也要判不等，且不得因短路而提前返回。
    /// 这条测不了时序，只钉住**结果正确性**；时序性质由 `constant_time_eq` 的实现保证
    /// （注释里写了为什么不能提前 return）。
    #[test]
    fn token_comparison_handles_length_mismatch() {
        assert!(!constant_time_eq("tok", "token"));
        assert!(!constant_time_eq("token", "tok"));
        assert!(!constant_time_eq("", "tok"));
        assert!(constant_time_eq("tok", "tok"));
        assert!(constant_time_eq("", ""));
    }

    /// 令牌 id 不能与真实 Key 的 id 空间相撞。
    ///
    /// `ProviderKey.id` 一律是 uuid（含连字符、无 `__` 前缀），故这个字面量不可能撞上，
    /// 也不会被孤儿密钥清理当成孤儿删掉。改这个常量会让老用户的令牌「消失」
    /// （库里那条还在，但按新 id 取不到 → 局域网客户端集体 401 且无从排查）。
    #[test]
    fn token_id_is_frozen() {
        assert_eq!(TOKEN_ID, "__lan_access_token");
        assert!(TOKEN_ID.starts_with("__"), "必须与 uuid 形态的 Key id 区分开");
        assert!(uuid::Uuid::parse_str(TOKEN_ID).is_err(), "不能长得像 uuid");
    }

    /// 生成的令牌要够长、且两次不同。
    #[test]
    fn generated_tokens_are_long_and_unique() {
        let (a, b) = (new_token(), new_token());
        assert_eq!(a.len(), 64, "两个 uuid simple 拼接应为 64 个十六进制字符");
        assert_ne!(a, b);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    /// 🔴 **走真实 HTTP 的判据。** 上面那些是纯函数，全绿也不能证明 `guarded` 这一层
    /// 真的把请求挡在了 `handle_request` 之前 —— 而「判定函数对、接线漏」的表现是**静默的**
    /// （同 CLAUDE.md 里 `handle_http_derives_the_caller_from_the_request_path` 那条的来由）。
    ///
    /// 手法：真 bind 端口 0、直接挂 `guarded`，并**传入一个假的 LAN peer**。
    /// 连接本身来自 loopback，但 guard 按我传的 peer 判定 → 必须 401，
    /// 且必须**没有**打到上游（store 里没有任何 Key，真走转发会是别的错误码）。
    #[tokio::test]
    async fn guarded_rejects_lan_peer_over_real_http_before_reaching_the_forwarder() {
        let (store, dir) = crate::service::tests::temp_store("lan_guard_http");
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let store_srv = store.clone();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            // 刻意不用真实对端地址：要验的是「guard 按传入的 peer 判定」这条接线。
            let svc = guarded(store_srv, crate::model::CategoryType::ClaudeCli, String::new(), lan());
            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(hyper_util::rt::TokioIo::new(stream), svc)
                .await;
        });

        let resp = reqwest::Client::new()
            .post(format!("http://127.0.0.1:{port}/v1/messages"))
            .header("content-type", "application/json")
            .body(r#"{"model":"x","messages":[]}"#)
            .send()
            .await
            .expect("请求应当拿到响应");
        assert_eq!(resp.status().as_u16(), 401, "LAN peer 未带令牌必须 401");
        // 诊断头也要在（保持「所有出口都带头」那条性质）
        assert!(
            resp.headers().contains_key("x-synaroute-version"),
            "401 出口也该带诊断头"
        );
        let body = resp.text().await.unwrap_or_default();
        assert!(body.contains("接入令牌"), "响应体应说明缺什么: {body}");
        assert!(!body.contains(TOKEN_ID), "不得回显令牌的存储 id");

        let _ = std::fs::remove_dir_all(dir);
    }

    /// 🔴 `ensure_token` 必须生成令牌**并把明文写进一条事件**。
    ///
    /// 那条事件是令牌的**跨时间留存**（设置页也能看，但关掉就没了）。
    /// 只生成不落事件的话，表现是「局域网怎么配都 401，而日志里什么都没有」——
    /// 静默且无从排查。故这条同时钉住「生成」与「可见」。
    #[test]
    fn ensure_token_creates_one_and_makes_it_visible_in_an_event() {
        let (store, dir) = crate::service::tests::temp_store("lan_guard_ensure");
        assert!(
            store.secrets.read().get(TOKEN_ID).unwrap().is_none(),
            "夹具应当是干净的"
        );

        ensure_token(&store);

        let token = store
            .secrets
            .read()
            .get(TOKEN_ID)
            .expect("读令牌")
            .expect("应已生成")
            .to_string();
        assert_eq!(token.len(), 64);

        let events = store.list_all_events();
        let hit = events
            .iter()
            .find(|e| e.detail.contains("接入令牌"))
            .expect("应落一条含「接入令牌」的事件");
        assert!(
            hit.detail.contains(&token),
            "事件必须带明文令牌，否则用户无从获取: {}",
            hit.detail
        );

        // 幂等：再调一次不该换掉已有令牌（否则已配好的客户端会突然全部 401）
        ensure_token(&store);
        let again = store.secrets.read().get(TOKEN_ID).unwrap().unwrap().to_string();
        assert_eq!(again, token, "ensure_token 不得重新生成");

        let _ = std::fs::remove_dir_all(dir);
    }

    /// 🔴 设置页读令牌**绝不能顺手生成**（B5）。
    ///
    /// 生成点只有一个（开局域网时的 `ensure_token`）。若读也生成，那么「打开设置页」
    /// 这个纯查看动作会给一个**没开局域网**的用户凭空造出密钥库条目，
    /// 且把单一生成点变成两个 —— 而两个生成点意味着「谁先跑」决定用户看到哪个值。
    #[test]
    fn read_lan_token_from_never_creates_one() {
        let (store, dir) = crate::service::tests::temp_store("lan_read_pure");

        assert_eq!(read_lan_token_from(&store), Ok(None), "干净库应返 None");
        assert!(
            store.secrets.read().get(TOKEN_ID).unwrap().is_none(),
            "读了一次之后库里仍不该有令牌"
        );
        // 再读一次：若第一次偷偷生成了，这次就会返 Some
        assert_eq!(read_lan_token_from(&store), Ok(None), "第二次读仍应是 None");

        ensure_token(&store);
        let created = store.secrets.read().get(TOKEN_ID).unwrap().unwrap().to_string();
        assert_eq!(
            read_lan_token_from(&store),
            Ok(Some(created)),
            "生成之后读回的必须是同一个值"
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    /// 🔴 **锁定态必须返 `Err`，绝不能退化成 `Ok(None)`。**
    ///
    /// 这是本功能最危险的失效方向：`Ok(None)` 在界面上是「还没有令牌」，
    /// 于是用户点「重新生成」—— 而库里其实**已经有**令牌，
    /// 结果**所有已配好的局域网客户端立刻 401**，且用户完全不知道自己刚做了什么。
    ///
    /// ⚠️ 这条是补上来的：原先只有 `read_lan_token_from_never_creates_one` 一条，
    /// 它用的是**未锁定**的库，于是把 `Err(_) => Err(..)` 改成 `Err(_) => Ok(None)`
    /// **照样全绿** —— 锁定那条分支压根没被跑到。
    /// 同 CLAUDE.md 那条教训：注入不变红时先怀疑用例没压到那个分支。
    #[test]
    fn a_locked_vault_reports_an_error_not_an_absent_token() {
        let (store, dir) = crate::service::tests::temp_store("lan_locked");
        ensure_token(&store);
        let existing = store.secrets.read().get(TOKEN_ID).unwrap().unwrap().to_string();

        store.secrets.write().enable_master_password("TestPass123").unwrap();
        store.secrets.write().lock();
        assert!(store.secrets.read().is_locked(), "夹具应处于锁定态");

        let got = read_lan_token_from(&store);
        assert!(
            got.is_err(),
            "锁定态必须返 Err；返 Ok(None) 会被界面显示成「还没有令牌」→ 用户重生成 → 已配客户端全部 401。实得 {got:?}"
        );
        // 且必须**没有**把已有令牌抹掉（读是只读的）
        store.secrets.write().unlock("TestPass123").unwrap();
        assert_eq!(
            read_lan_token_from(&store),
            Ok(Some(existing)),
            "解锁后原令牌应还在"
        );

        // 重生成在锁定态也必须失败，而不是写出一个读不回来的值
        store.secrets.write().lock();
        assert!(
            regenerate_lan_token_in(&store).is_err(),
            "锁定态不该能重生成 —— 那会落一条「已生成」的事件却没真写进去"
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    /// 重新生成必须**换掉**令牌、并落一条说明「旧的已失效」的事件。
    ///
    /// 返回旧值（看着像成功、其实没换）是最坏的失败形态：用户以为轮换过了，
    /// 而泄露的那个令牌仍然有效。
    #[test]
    fn regenerating_replaces_the_token_and_leaves_an_audit_event() {
        let (store, dir) = crate::service::tests::temp_store("lan_regen");
        ensure_token(&store);
        let old = store.secrets.read().get(TOKEN_ID).unwrap().unwrap().to_string();

        let new = regenerate_lan_token_in(&store).expect("应成功");

        assert_ne!(new, old, "必须真的换掉，返回旧值等于轮换没生效");
        assert_eq!(new.len(), 64);
        assert_eq!(
            read_lan_token_from(&store),
            Ok(Some(new.clone())),
            "落盘的必须是新值"
        );

        let events = store.list_all_events();
        let hit = events
            .iter()
            .find(|e| e.detail.contains("重新生成") && e.detail.contains(&new))
            .expect("应落一条带新令牌的事件 —— 用户关掉对话框后还能从日志找回");
        assert!(
            hit.detail.contains("失效"),
            "必须点明旧令牌已失效，否则用户不知道要更新客户端: {}",
            hit.detail
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    /// 🔴 **接线判据：两个命令必须在 `generate_handler!` 里注册。**
    ///
    /// 上面两条测试直接调 `read_lan_token_from` / `regenerate_lan_token_in`，
    /// 于是**忘了注册命令它们照样全绿** —— 而那时前端一点就报
    /// `Command get_lan_token not found`，设置页整块功能不可用。
    /// 策略门 `invoke-command-must-exist` 查的是「前端调的命令存在」这个方向，
    /// 而这里漏的是同一条缝的另一头（Rust 侧写了函数但没进注册表）。
    #[test]
    fn lan_token_commands_must_be_registered() {
        let prod = crate::proxy::custom_headers::production_slice(include_str!("lib.rs"));
        let handler = &prod[prod
            .find("tauri::generate_handler![")
            .expect("注册宏还在吧")..];
        for cmd in ["get_lan_token", "regenerate_lan_token"] {
            assert!(
                handler.contains(cmd),
                "{cmd} 没进 generate_handler! —— 前端一点就 command not found"
            );
        }
    }

    /// 🔴 **源码级判据：`accept` 拿到的对端地址必须真的传进 `guarded`。**
    ///
    /// 这是上面所有测试都盖不到的那道缝：把 `proxy.rs` 里的 `peer` 换回 `_`、
    /// 再给 `guarded` 传一个硬编码的 loopback 地址，**全部 9 条测试照样绿**，
    /// 而那就是本缺陷本身（所有来源都被当成本机 → 一律放行）。
    /// 同 CLAUDE.md 里 `route_meta` 那条「记得在每个出口调一次是必然会漏的纪律」。
    ///
    /// 判据只看两件事，不锁具体写法（否则重构必然假红）：
    /// accept 处没把对端丢给 `_`；且 `guarded(` 的实参里出现 `peer`。
    #[test]
    fn accept_must_pass_the_real_peer_into_the_guard() {
        // 用共用判据取生产段：朴素的 split 会被中间的 #[cfg(test)] 单项截断，
        // 而下面那条是**否定**断言 —— 截断即空洞通过（判据变绿、缺陷仍在）。
        let prod = crate::proxy::custom_headers::production_slice(include_str!("proxy.rs"));
        assert!(
            !prod.contains("let Ok((stream, _)) = accepted"),
            "accept 又把对端地址丢成 `_` 了 —— 那样 guard 只能看到一个假 peer"
        );
        let wired = prod
            .lines()
            .any(|l| l.contains("lan_guard::guarded(") && l.contains("peer"));
        assert!(wired, "guarded(...) 的实参里必须带上 accept 得到的 peer");
    }

    /// 同一条链路，带上正确令牌就**不该**在 guard 层被拦。
    ///
    /// 判据是「不是 401」而不是「200」：store 里没有 Key，转发一定失败，
    /// 但那是转发层的错（502/529 之类）。只要不再是 401，就证明 guard 放行了。
    /// 🔴 这条防的是「令牌校验写成恒拒」—— 那样上面那条测试照样绿，而功能完全不可用。
    #[tokio::test]
    async fn guarded_lets_a_correct_token_through_to_the_forwarder() {
        let (store, dir) = crate::service::tests::temp_store("lan_guard_ok");
        let token = "t".repeat(64);
        store.secrets.write().set(TOKEN_ID, &token).expect("写令牌");

        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let store_srv = store.clone();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let svc = guarded(store_srv, crate::model::CategoryType::ClaudeCli, String::new(), lan());
            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(hyper_util::rt::TokioIo::new(stream), svc)
                .await;
        });

        let resp = reqwest::Client::new()
            .post(format!("http://127.0.0.1:{port}/v1/messages"))
            .header("content-type", "application/json")
            .header("x-api-key", &token)
            .body(r#"{"model":"x","messages":[]}"#)
            .send()
            .await
            .expect("请求应当拿到响应");
        assert_ne!(
            resp.status().as_u16(),
            401,
            "带对令牌不该被 guard 拦住（实际 {}）",
            resp.status()
        );

        let _ = std::fs::remove_dir_all(dir);
    }
}
