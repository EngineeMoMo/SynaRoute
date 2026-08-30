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
//!
//! # 🔴 令牌明文只有一个出口：设置页
//!
//! 事件与日志文件里只许出现 [`fingerprint`]（前 8 位）。这不是洁癖 —— 明文进事件就等于
//! 同时进了**三个用户会分享出去的地方**：诊断报告（`diagnostics.rs` 取最近 200 条 detail
//! 原样入报告，而那份报告的用途就是发给别人）、`logs/*.jsonl`（exe 同级、非虚拟化、
//! 留 30 天、用户会直接 tail 并贴出来）、以及日志页截图。
//!
//! 拿到任意一份的人只要能进同一网段，就能用用户的付费 Key。详见 [`token_or_create`]。

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
    if is_loopback_peer(&peer.ip()) {
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

/// 这个对端是不是本机。**必须先归一 IPv4-mapped IPv6**，不能裸调 `is_loopback()`。
///
/// `std` 的 `Ipv6Addr::is_loopback()` 只认 `::1` —— 对 `::ffff:127.0.0.1`
/// （IPv4-mapped 形态）返回 **false**。当前生产路径绑 `0.0.0.0`（纯 IPv4 socket），
/// 对端永远是 `V4`，所以裸调**眼下是对的**；这一层是给将来准备的：
///
/// 谁为了支持 IPv6 客户端把绑定改成 `::`（Windows/Linux 默认双栈），
/// 本机客户端的连接就会以 `::ffff:127.0.0.1` 的形态到达 → 本机豁免失效 →
/// **Claude CLI / 桌面端 / Codex 三端集体 401**，而那恰恰是本豁免存在的全部理由
/// （见模块注释：要求本机也带令牌就得同步改三份配置，漏一份就是「接入完成但一直 401」）。
///
/// 失效方向是**拒绝**而不是放行，所以不是安全洞；但它会制造一个极难归因的支持案例
/// —— 用户只看到「升级后局域网功能一开，本机就全断了」。一行归一换掉这个坑，值。
///
/// 反过来**绝不能**把整个 `::ffff:0:0/96` 段当本机：那里面 `::ffff:192.168.1.5`
/// 是真实的局域网地址，当成本机就是把鉴权整个绕过去。故只归一、再判 loopback，
/// 判定权仍在 `is_loopback()` 手里。
fn is_loopback_peer(ip: &std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => v4.is_loopback(),
        std::net::IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            // 只对 v4-mapped 做归一。`to_ipv4()` 不行 —— 它还会把
            // IPv4-**compatible**（`::a.b.c.d`，已废弃）也转过来，而 `::1` 本身就落在
            // 那个形态里：`Ipv6Addr::LOCALHOST.to_ipv4()` == `Some(0.0.0.1)`，
            // 那是个**非** loopback 的 v4 地址 → `::1` 会被判成非本机，
            // 把本要修的坑反过来踩一遍（写这条时实测确认，有测试钉住）。
            Some(v4) => v4.is_loopback(),
            None => v6.is_loopback(),
        },
    }
}

/// 定长时间比较。避免用 `==` 逐字节短路，让攻击者靠响应时间差逐位试出令牌。
///
/// 长度不同也要走完循环 —— 提前 `return false` 会把长度泄露出去。
///
/// # 🔴 长度差异必须压成布尔，不能把异或值截断成 `u8`
///
/// 第一版写的是 `(a.len() ^ b.len()) as u8` —— `usize ^ usize` 再 `as u8` **只保留低 8 位**，
/// 于是长度差恰好是 256 的倍数时，长度差异被**整个丢掉**（`32 ^ 288 == 256`，截断后是 0）。
/// 此后只要逐字节比较也全 0（短的那边超出部分取 0，长的那边补 `\0`），函数就返回 true ——
/// 也就是「真令牌 + 256 个 NUL 后缀」会被判为相等。
///
/// 实际不可利用（HTTP 头值不允许 NUL，且攻击者得先知道真令牌），但这是判据在边界上
/// 静默失效，正是本仓最在意的那一类。有 `length_difference_survives_any_multiple_of_256`
/// 一条测试钉住。
fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut diff = u8::from(a.len() != b.len());
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

/// 取当前令牌；没有就**就地生成并落盘**，同时落一条**只带指纹**的事件。
///
/// 惰性生成而不是在 `set_lan_exposure` 里生成：那样「开关已开但令牌没生成」
/// （比如老用户升级上来、配置里 `lan_exposure` 本就是 true）会变成一个静默的空洞，
/// 而这里的 `None → Deny` 虽然安全，用户却只看到 401 而不知道令牌是什么。
///
/// 🔴 **事件里绝不放明文令牌**（2026-08-27 改，修的是一条真实泄露）。
///
/// 原设计刻意把明文写进事件，理由是「跨时间留存」—— 用户关掉窗口、事后想配另一台机器时
/// 靠它找回。那个理由在 B5（设置页可看/复制/重生成，见 [`read_lan_token_from`]）落地之后
/// 已经不成立，而它的代价一直存在且比当初以为的大：
///
/// - [`crate::diagnostics`] 取最近 200 条事件的 detail **原样**入报告，而出口那道
///   `redact_config_secrets` 只认**键名形态**与 `sk-` 前缀裸 token —— 中文句子里的
///   裸十六进制串一个字符都不掩。而诊断报告的全部意义就是**发给别人**，
///   报告开头还写着「本文件不包含：任何 API 密钥明文」。
/// - 更大的那一半：`append_event_full` 会先把**完整 detail 写进日志文件**
///   （`store.rs` 里 `write_log_to_file`），那是 exe 同级、非虚拟化、保留 30 天、
///   用户会直接 tail 并贴给别人的文件。
///
/// 拿到任何一份的人只要能进同一网段，就能用用户的付费 Key。故事件改为只带
/// [`fingerprint`]（前 8 位十六进制）—— 足够用户核对「客户端里配的是不是这一个」，
/// 但不足以拿去用（余下 56 个十六进制字符 = 224 位）。
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
            "已为「局域网暴露」生成接入令牌（指纹 {}…）—— \
             完整令牌请到「设置 → 局域网暴露」查看并复制，\
             局域网客户端必须把它填进 API Key（或 Authorization: Bearer）才能使用；\
             本机客户端不受影响。",
            fingerprint(&token)
        ),
    );
    Some(token)
}

/// 令牌指纹：前 8 个十六进制字符。**日志与事件里只许出现这个，不许出现完整令牌。**
///
/// 用途是让用户核对「客户端里配的是不是当前这一个」——排查局域网 401 时唯一需要的信息。
/// 8 位十六进制不足以拿去用（余下 56 个字符 = 224 位搜索空间），
/// 而完整令牌的获取入口只有一个：设置页（[`read_lan_token_from`]）。
fn fingerprint(token: &str) -> String {
    token.chars().take(8).collect()
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
/// 落一条**只带指纹**的事件（理由同 [`token_or_create`]：明文进事件 = 进日志文件 +
/// 进诊断报告，两者都是用户会分享出去的东西）。这条事件的价值不在「找回令牌」
/// —— 那是设置页的活 —— 而在**留下轮换的时间点**：客户端突然集体 401 时，
/// 日志里这一行是唯一能解释「为什么昨天还好好的」的证据。
/// 文案必须点明「旧的已失效」，否则用户不知道要去更新客户端。
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
            "已**重新生成**「局域网暴露」接入令牌（指纹 {}…）—— \
             旧令牌立即失效，请到「设置 → 局域网暴露」复制新令牌并更新所有局域网客户端的 \
             API Key；本机客户端不受影响。",
            fingerprint(&token)
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
            // 🔴 指路必须指向**设置页**，不是日志页。日志里现在只有指纹（前 8 位），
            // 照着去搜只能找到一个用不了的短串 —— 而「指错方向的提示比没有提示更糟」
            // 是本仓明确的判据（见 Codex 漂移告警那条）。有测试钉住这个方向。
            "message": "局域网访问需要接入令牌。请打开 SynaRoute 的\
                        「设置 → 局域网暴露」复制完整令牌，\
                        填进客户端的 API Key（或 Authorization: Bearer）。本机访问无需令牌。"
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

/// 被拒次数的**总计**（进程级，跨分类）。事件按 IP 去重，这个数字不去重。
///
/// 🔴 **去重与可观测性是两件事，必须各有各的出口。**
///
/// 原实现只有「每 IP 一条事件」，于是一个真正在试探的攻击者**只留下一条记录然后永远沉默**
/// —— 打 1 次和打 50 万次在界面与日志里长得一模一样。而这恰恰是本模块唯一想看见的信号：
/// 局域网里有人在反复撞令牌。
///
/// 反过来把节流去掉也不行：端口扫描器会把 `MAX_EVENTS` 环刷满，
/// 把真正有用的事件挤出去（本仓已吃过两次这个教训：短路窗口每次重发记一条、
/// MCP 分类回落每次记一条）。
///
/// 故：**事件按 IP 去重**（防刷屏）+ **计数器不去重**（保留量级），诊断报告各打一行。
static DENIED_TOTAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// 最多追踪多少个不同来源 IP（只影响「要不要再落一条事件」）。
///
/// 上限存在的理由：`SEEN` 是进程级、只增不减的 `HashSet`。局域网里 IP 数量本来有限，
/// 但 IPv6 一开，一台机器就能轮换几乎无限多个源地址 —— 那时这个集合会一直长。
/// 到上限之后停止插入（于是不再落新事件），但 [`DENIED_TOTAL`] 照常累加，
/// 「有人在撞」这个信号不丢。
const MAX_TRACKED_DENIED_IPS: usize = 1024;

/// 已被拒绝的局域网请求总数（不按 IP 去重）。供诊断报告。
pub(crate) fn denied_count() -> u64 {
    DENIED_TOTAL.load(std::sync::atomic::Ordering::Relaxed)
}

/// 被拒的来源**每个 IP 每次运行只记一条事件**，但总次数无条件计数。
///
/// 两者的分工见 [`DENIED_TOTAL`]。
fn log_denied_once(store: &Arc<Store>, peer: &SocketAddr) {
    use std::collections::HashSet;
    use std::sync::Mutex;
    use std::sync::OnceLock;
    // 先计数，且**绝不能**放在下面那个 early-return 之后 —— 那样它就又变成按 IP 去重的了，
    // 而这个计数器存在的全部意义就是不去重。有测试钉住这个顺序。
    DENIED_TOTAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    static SEEN: OnceLock<Mutex<HashSet<std::net::IpAddr>>> = OnceLock::new();
    let seen = SEEN.get_or_init(|| Mutex::new(HashSet::new()));
    let Ok(mut set) = seen.lock() else { return };
    if set.len() >= MAX_TRACKED_DENIED_IPS || !set.insert(peer.ip()) {
        return;
    }
    store.append_event(
        crate::model::CategoryType::ClaudeCli,
        "system",
        None,
        &format!(
            "已拒绝来自 {} 的局域网请求（未带正确的接入令牌）。\
             本次运行内该来源不再重复记录；总次数见诊断报告。",
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
            //
            // ⚠️ **必须与 `verdict` 用同一个判据**（`is_loopback_peer`，不是裸
            // `is_loopback()`）。两处分叉的后果是「快路径认为不是本机 → 去读密钥库；
            // verdict 认为是本机 → 放行」这类不一致：功能上还对，但本机转发白解密一次，
            // 而且下次谁改了其中一处就会真的分叉。有源码级判据钉住。
            if !is_loopback_peer(&peer.ip()) {
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

    /// IPv4-mapped IPv6 形态的本机地址必须被认成本机。
    ///
    /// 当前绑 `0.0.0.0`（纯 IPv4）故生产路径碰不到，这条是**为将来兜底**：
    /// 谁把绑定改成 `::`（双栈）以支持 IPv6 客户端，本机连接就会以
    /// `::ffff:127.0.0.1` 到达。裸 `is_loopback()` 对它返回 false →
    /// 三端客户端集体 401，而那正是本机豁免要防的事。
    #[test]
    fn ipv4_mapped_loopback_is_recognised_as_local() {
        let mapped: SocketAddr = "[::ffff:127.0.0.1]:51234".parse().unwrap();
        assert!(
            is_loopback_peer(&mapped.ip()),
            "::ffff:127.0.0.1 是本机 —— 裸 is_loopback() 对它返回 false"
        );
        assert_eq!(verdict(&mapped, None, Some("tok")), Verdict::Allow);
        // 127.x 整段都是 loopback，不只 127.0.0.1
        let mapped2: SocketAddr = "[::ffff:127.1.2.3]:1".parse().unwrap();
        assert!(is_loopback_peer(&mapped2.ip()));
    }

    /// 🔴 **归一绝不能把 `::ffff:` 整段当本机** —— 那里面全是真实的 IPv4 地址。
    ///
    /// 若写成「是 v4-mapped 就放行」，`::ffff:192.168.1.5` 就绕过了整个鉴权。
    /// 这是本条改动唯一可能引入的**安全**方向失效，故单独钉一条。
    #[test]
    fn ipv4_mapped_lan_addresses_are_not_treated_as_local() {
        for s in ["[::ffff:192.168.1.5]:1", "[::ffff:10.0.0.7]:1", "[::ffff:8.8.8.8]:1"] {
            let peer: SocketAddr = s.parse().unwrap();
            assert!(
                !is_loopback_peer(&peer.ip()),
                "{s} 是真实的局域网/公网地址，绝不能当本机"
            );
            assert_eq!(
                verdict(&peer, None, Some("tok")),
                Verdict::Deny,
                "{s} 不带令牌必须被拒"
            );
        }
    }

    /// 🔴 **`::1` 在归一之后仍必须是本机。**
    ///
    /// 这条防的是一个具体的写法坑：用 `to_ipv4()` 而不是 `to_ipv4_mapped()` 做归一时，
    /// IPv4-**compatible**（`::a.b.c.d`，已废弃形态）也会被转换，而 `::1` 正落在那个形态里
    /// —— `Ipv6Addr::LOCALHOST.to_ipv4()` == `Some(0.0.0.1)`，那是个**非** loopback 的
    /// v4 地址。于是「修 IPv4-mapped」这个改动会把 `::1` 反过来判成非本机，
    /// 制造出它本要消除的那个故障。
    #[test]
    fn plain_ipv6_loopback_survives_the_normalisation() {
        assert!(is_loopback_peer(&local_v6().ip()), "::1 必须仍是本机");
        assert_eq!(verdict(&local_v6(), None, None), Verdict::Allow);
        // 把这个坑本身也钉住：证明 to_ipv4() 确实会给出一个非 loopback 的结果，
        // 故实现里只能用 to_ipv4_mapped()。
        assert_eq!(
            Ipv6Addr::LOCALHOST.to_ipv4(),
            Some(std::net::Ipv4Addr::new(0, 0, 0, 1)),
            "若这条变了，实现里那句「不能用 to_ipv4()」的理由需要重新核实"
        );
        assert!(!std::net::Ipv4Addr::new(0, 0, 0, 1).is_loopback());
    }

    /// 🔴 **接线判据：两处 loopback 判定必须是同一个函数。**
    ///
    /// `verdict` 与 `guarded` 的快路径各判一次。任一处退回裸 `is_loopback()`
    /// 就会分叉，而上面那些用例只压 `verdict`（纯函数），**快路径退回去它们照样全绿**。
    /// 同本仓反复踩的那类盲区（`route_meta` / `lan_guard` 的 peer / `log_rotate` 写线程）。
    #[test]
    fn both_loopback_checks_go_through_one_predicate() {
        let prod = crate::proxy::custom_headers::production_slice(include_str!("lan_guard.rs"));
        assert!(
            !prod.contains("peer.ip().is_loopback()"),
            "别裸调 is_loopback() —— 走 is_loopback_peer()，否则 v4-mapped 本机会被判成外来"
        );
        // 两处调用点都要在（判定 + 快路径）
        let n = prod.matches("is_loopback_peer(").count();
        assert!(
            n >= 3,
            "应有定义 + verdict + guarded 快路径共 3 处 is_loopback_peer，实得 {n}"
        );
    }

    /// 🔴 **凡是会让 [`DENIED_TOTAL`] 变动的用例都必须先拿这把锁。**
    ///
    /// `DENIED_TOTAL` 是**进程级** static，而 cargo 默认并行跑用例。
    /// 「记下 before → 打 3 次 → 断言差值恰好是 3」这种写法会被**同进程里另一个
    /// 也会被拒的用例**（`guarded_rejects_lan_peer_over_real_http_...` 走 401 →
    /// `deny_response` → `log_denied_once`）插进来打破。
    ///
    /// ⚠️ 这不是假设，是实测：第一版没有这把锁，`cargo test --lib lan_guard` 连跑 3 次
    /// **红了 2 次**，而单独跑那条用例永远绿。同 CLAUDE.md 里
    /// 「`open_failed_line_count` 并进 `log_dropped_count` 会让
    /// `flush_logs_drains_the_queue` 当场变红」那条 —— **进程级计数器的断言必须串行化**。
    ///
    /// 也顺带说明了为什么这个计数器不做成 per-Store：它要在 `deny_response` 这条
    /// 「只有 `&Arc<Store>` 没有 `&mut`」的路径上无锁自增，而 store.rs 的棘轮余量是 0。
    /// 进程级 + 测试串行化是这里代价最小的组合。
    static DENY_COUNTER_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// 🔴 **被拒总次数不去重，事件才去重。**
    ///
    /// 原实现只有「每 IP 一条事件」，于是一个真正在试探的攻击者**只留下一条记录然后
    /// 永远沉默** —— 打 1 次和打 50 万次在界面与日志里完全一样，而「有人在反复撞令牌」
    /// 恰恰是本模块唯一想让用户看见的信号。
    ///
    /// 判据同时压住两个方向：同一个 IP 打多次 → 事件仍只有一条（防扫描器刷屏）、
    /// 计数照涨（保留量级）。**只测其中一个方向都会漏掉一半。**
    #[test]
    fn repeated_denials_from_one_ip_keep_one_event_but_keep_counting() {
        let _serial = DENY_COUNTER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (store, dir) = crate::service::tests::temp_store("lan_denied_count");
        let before = denied_count();

        // 同一个 IP 连打三次。用一个本用例专属的地址，避免与同进程其它用例的 SEEN 集合互相影响。
        let peer: SocketAddr = "203.0.113.77:5555".parse().unwrap();
        for _ in 0..3 {
            log_denied_once(&store, &peer);
        }

        assert_eq!(
            denied_count() - before,
            3,
            "总次数必须逐次累加 —— 它是「有人在撞」的唯一信号，不能按 IP 去重"
        );
        let hits = store
            .list_all_events()
            .iter()
            .filter(|e| e.detail.contains("203.0.113.77"))
            .count();
        assert_eq!(hits, 1, "同一来源只该落一条事件，否则扫描器能刷满 MAX_EVENTS 环");

        let _ = std::fs::remove_dir_all(dir);
    }

    /// 计数必须排在「按 IP 去重」那道 early-return **之前**。
    ///
    /// 顺序反了的话计数器也变成按 IP 去重的，于是它与事件携带的信息完全重复 ——
    /// 那就等于这条改动什么都没做，而且是静默的（数字仍在涨，只是涨得不对）。
    /// 源码级判据：`fetch_add` 必须出现在 `set.insert` 之前。
    #[test]
    fn the_counter_is_bumped_before_the_dedup_return() {
        let prod = crate::proxy::custom_headers::production_slice(include_str!("lan_guard.rs"));
        let f = &prod[prod.find("fn log_denied_once").expect("函数还在吧")..];
        let add = f.find("DENIED_TOTAL.fetch_add").expect("必须计数");
        let dedup = f.find("set.insert").expect("必须去重");
        assert!(
            add < dedup,
            "计数要排在去重之前，否则总次数也被按 IP 去重了"
        );
    }

    /// 被拒次数必须出现在**诊断报告**里 —— 否则这个数字没有任何出口，等于没记。
    #[test]
    fn the_denied_count_reaches_the_diagnostics_report() {
        let _serial = DENY_COUNTER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (store, dir) = crate::service::tests::temp_store("lan_denied_report");
        log_denied_once(&store, &"198.51.100.9:1".parse().unwrap());

        let env = crate::diagnostics::DiagnosticsEnv {
            app_version: "0.0.0-test".into(),
            exe_path: "x".into(),
            proxy: vec![],
        };
        let report = crate::diagnostics::build_diagnostics_report(&store, &env);
        assert!(
            report.contains("局域网请求被拒次数"),
            "诊断报告必须打出被拒次数，否则排障者只能看到一条按 IP 去重的事件"
        );
        let _ = std::fs::remove_dir_all(dir);
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

    /// 🔴 长度差恰好是 **256 的倍数**时也必须判不等。
    ///
    /// 上面那条用的长度差都是个位数，所以第一版实现
    /// `let mut diff = (a.len() ^ b.len()) as u8;` **照样全绿** —— 而 `usize ^ usize`
    /// 再 `as u8` 只保留低 8 位：`3 ^ 259 == 256`，截断后是 **0**，长度差异被整个丢掉。
    /// 此后逐字节比较也全 0（短的那边超出部分取 0，长的那边补 `\0`），函数返回 **true**，
    /// 也就是「真令牌 + 256 个 NUL 后缀」被判为相等。
    ///
    /// 实际不可利用（HTTP 头值不允许 NUL，且攻击者得先知道真令牌），但这是判据在边界上
    /// 静默失效 —— 正是本仓最在意的那一类，而它只在「长度差刚好越过 256」时暴露。
    #[test]
    fn length_difference_survives_any_multiple_of_256() {
        for pad in [256usize, 512, 768] {
            let forged = format!("tok{}", "\0".repeat(pad));
            assert_eq!(
                ("tok".len() ^ forged.len()) as u8,
                0,
                "这个 pad 必须真的让旧实现的 as u8 截断成 0，否则本用例压不到那个边界"
            );
            assert!(
                !constant_time_eq("tok", &forged),
                "长度差 {pad}（256 的倍数）时必须判不等"
            );
            assert!(!constant_time_eq(&forged, "tok"), "反向也一样");
        }
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
    /// ⚠️ `#[allow(clippy::await_holding_lock)]`：这里跨 await 持的是
    /// [`DENY_COUNTER_LOCK`]，一把**只在测试段存在**的串行化锁。
    /// 每个 `#[tokio::test]` 各自起一个 current-thread 运行时，该运行时上不存在第二个
    /// 会争这把锁的任务，故不可能自锁；争用只发生在**不同测试线程**之间，
    /// 而那正是它要做的事。换成 `tokio::sync::Mutex` 反而更糟 ——
    /// 同一把锁还要被两条**同步**用例拿，那边只能 `blocking_lock()`，在 async 上下文里会 panic。
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn guarded_rejects_lan_peer_over_real_http_before_reaching_the_forwarder() {
        // 这条会走 401 → `deny_response` → `log_denied_once`，也就是会动 `DENIED_TOTAL`。
        // 拿锁的理由见 `DENY_COUNTER_LOCK` —— 不拿的话它会把另一条用例的差值断言打破
        // （实测连跑 3 次红 2 次）。
        let _serial = DENY_COUNTER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
        // 🔴 指路必须指向设置页。日志里现在只有指纹（前 8 位），指去日志页 =
        // 让用户找到一个用不了的短串还以为自己抄对了。
        // 这条是补上来的 —— 把明文从事件里摘掉时，401 的文案**仍写着**「令牌在日志页里」，
        // 而当时 11 条用例全绿。判据从「提到令牌」升级成「指对地方」。
        assert!(
            body.contains("设置") && !body.contains("日志页"),
            "401 必须指向设置页而不是日志页（日志里只有指纹）: {body}"
        );
        assert!(!body.contains(TOKEN_ID), "不得回显令牌的存储 id");

        let _ = std::fs::remove_dir_all(dir);
    }

    /// 🔴 `ensure_token` 必须生成令牌**并落一条能指认它的事件**。
    ///
    /// 只生成不落事件的话，表现是「局域网怎么配都 401，而日志里什么都没有」——
    /// 静默且无从排查。故这条同时钉住「生成」与「可见」。
    ///
    /// ⚠️ 「可见」的判据是**指纹**，不是明文（2026-08-27 改）：明文进事件等于进日志文件
    /// 与诊断报告，两者都是用户会分享出去的东西。见 `the_token_plaintext_never_enters_an_event`。
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
            hit.detail.contains(&fingerprint(&token)),
            "事件必须带指纹，用户才能核对客户端里配的是不是这一个: {}",
            hit.detail
        );
        assert!(
            hit.detail.contains("设置"),
            "必须指明完整令牌去哪拿，否则用户拿到指纹也没用: {}",
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
            .find(|e| e.detail.contains("重新生成") && e.detail.contains(&fingerprint(&new)))
            .expect("应落一条带新令牌指纹的事件 —— 它是「为什么客户端突然全部 401」的唯一证据");
        assert!(
            hit.detail.contains("失效"),
            "必须点明旧令牌已失效，否则用户不知道要更新客户端: {}",
            hit.detail
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    /// 🔴 **令牌明文绝不能进事件** —— 这是一条真实泄露的回归测试（2026-08-27）。
    ///
    /// 原实现刻意把明文写进事件当「跨时间留存」，而那一条 detail 会同时流进
    /// **三个用户会分享出去的地方**：
    ///
    /// 1. `diagnostics.rs` 取最近 200 条事件 detail 原样入报告 —— 而那份报告的用途就是
    ///    发给别人，开头还写着「本文件不包含：任何 API 密钥明文」。出口那道
    ///    `redact_config_secrets` 只认键名形态与 `sk-` 前缀，**中文句子里的裸十六进制串
    ///    一个字符都不掩**（实测确认）。
    /// 2. `append_event_full` 先把完整 detail 写进 `logs/*.jsonl`（exe 同级、非虚拟化、
    ///    留 30 天、用户会直接 tail 并贴出来）。这一半比报告更大。
    /// 3. 日志页截图。
    ///
    /// 拿到任意一份的人只要能进同一网段，就能用用户的付费 Key。
    ///
    /// ⚠️ 判据必须同时压住**事件**与**诊断报告**两处。只测报告的话，把明文写回事件、
    /// 再在 diagnostics 里单独打个补丁也能过 —— 那就漏掉了日志文件那条更大的路径。
    #[test]
    fn the_token_plaintext_never_enters_an_event_or_the_diagnostics_report() {
        let (store, dir) = crate::service::tests::temp_store("lan_no_plaintext");
        ensure_token(&store);
        let token = store.secrets.read().get(TOKEN_ID).unwrap().unwrap().to_string();
        let rotated = regenerate_lan_token_in(&store).expect("重生成应成功");

        // ① 事件（也就是日志文件里那一行的来源）
        for e in store.list_all_events() {
            assert!(
                !e.detail.contains(&token) && !e.detail.contains(&rotated),
                "事件 detail 里出现了令牌明文 —— 它会随日志文件与诊断报告一起被分享出去: {}",
                e.detail
            );
        }

        // ② 诊断报告（整份，含它自己的脱敏出口）
        let env = crate::diagnostics::DiagnosticsEnv {
            app_version: "0.0.0-test".into(),
            exe_path: "x".into(),
            proxy: vec![],
        };
        let report = crate::diagnostics::build_diagnostics_report(&store, &env);
        for t in [&token, &rotated] {
            assert!(
                !report.contains(t),
                "诊断报告里出现了局域网接入令牌明文 —— 而报告的用途就是发给别人，\
                 且它开头声明「不包含任何 API 密钥明文」"
            );
        }
        // 指纹要留着：它是排查「客户端配的是不是当前这个」的唯一线索，脱敏不该把它也抹掉。
        assert!(
            report.contains(&fingerprint(&rotated)),
            "报告里应保留指纹，否则排障时无法核对客户端配的是哪一个"
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    /// 指纹必须**短到不能用**、且真的是令牌的前缀（否则核对不上）。
    #[test]
    fn fingerprint_is_a_short_prefix_not_the_whole_token() {
        let t = new_token();
        let fp = fingerprint(&t);
        assert_eq!(fp.len(), 8, "8 位十六进制：够核对，不够用");
        assert!(t.starts_with(&fp), "必须是前缀，否则用户核对不上");
        assert!(fp.len() < t.len() / 4, "不得接近完整长度");
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
