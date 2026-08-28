//! 代理监听 socket 的绑定：**粘滞端口** + **双栈（IPv4 + IPv6）**。
//!
//! 挂在 [`crate::proxy`] 下（`#[path]`），理由同 `lan_guard.rs` / `log_rotate.rs`：
//! `proxy.rs` 的棘轮余量是 **0**，而目录化是 docs/15 P2 刻意未做的大 diff。
//! 抽出来之后 `proxy.rs` 里那段 20 行的端口扫描收成 1 行调用，本模块的新逻辑不占它的额度。
//!
//! # 修的是什么
//!
//! 原实现只绑 `0.0.0.0`（开局域网时）或 `127.0.0.1`。那是一个**纯 IPv4** socket，
//! 于是 IPv6 客户端**压根连不上** —— 表现是「同一个局域网里，有的机器能连、有的报
//! connection refused」，而用户看不出那台机器的区别在于它走的是 IPv6。
//!
//! 现在两个都绑：v4 必须成功（它是本机三个客户端配置里写的地址），v6 尽力而为。
//!
//! # 🔴 为什么 v6 socket 必须显式 `set_only_v6(true)`
//!
//! 这是本模块唯一真正微妙的地方，而且**平台默认值互相矛盾**：
//!
//! - Windows / macOS：`IPV6_V6ONLY` 默认 **1**（`::` 只收 IPv6）。两个 socket 各管一半，
//!   相安无事。
//! - Linux：默认 **0**（`::` 是双栈通吃）。此时先绑 `0.0.0.0:P` 再绑 `[::]:P` 会
//!   **`EADDRINUSE`** —— 两个通配地址打架。
//!
//! 不设这个选项的话，代码在 Windows 上跑得好好的，到 Linux 上 v6 绑定静默失败、
//! 退回 v4-only。那不是崩溃、不报错，只是「IPv6 客户端又连不上了」——
//! 也就是这个模块要修的缺陷原样复发，而且只在一个平台上。
//!
//! 显式 `only_v6(true)` 让三个平台行为一致：`::` 只管 IPv6，`0.0.0.0` 只管 IPv4，
//! 永不冲突。**别删这一行，也别改成依赖平台默认值。**
//!
//! # 与 `lan_guard` 的关系
//!
//! v6 socket 收到的对端地址在部分路径上会是 IPv4-mapped 形态（`::ffff:a.b.c.d`），
//! 而 `lan_guard::is_loopback_peer` 正是为此做了归一。两件事必须一起看：
//! 没有那层归一，本模块一开 v6 就会让**本机**客户端撞 401。

use socket2::{Domain, Protocol as SockProtocol, Socket, Type};
use std::net::SocketAddr;
use tokio::net::TcpListener;

/// 绑好的监听器。`v6` 为 `None` 表示这台机器上 IPv6 不可用（已停用协议栈 / 容器里没配），
/// 那不是错误 —— v4 照常服务，只是 IPv6 客户端连不上。
#[derive(Debug)]
pub(crate) struct Bound {
    pub(crate) port: u16,
    pub(crate) v4: TcpListener,
    pub(crate) v6: Option<TcpListener>,
}

/// 在 `[preferred, preferred + range]` 内找一个 v4 能绑上的端口，并在**同一个端口**上
/// 尽力补一个 v6 监听。
///
/// 端口粘滞的理由（不用 `bind(0)` 随机端口）见调用点：客户端配置只在它自己启动时读一次，
/// 端口漂移会让它追不上。
///
/// **v4 是判据、v6 是附赠**：只有 v4 成功才算这个端口可用。反过来（v6 成功就接受）在
/// Windows 上会造成一个真实的错路由 —— 别的进程占着 `0.0.0.0:P`、我们的 `[::]:P` 却绑上了，
/// 于是客户端连 `127.0.0.1:P` 落到那个**别人的**进程里，而我们这边显示「已启动」。
pub(crate) async fn bind_sticky(
    lan: bool,
    preferred: u16,
    range: u16,
) -> Result<Bound, String> {
    let v4_host = if lan { [0, 0, 0, 0] } else { [127, 0, 0, 1] };
    let end = preferred.saturating_add(range);
    let mut last_err = String::new();
    for candidate in preferred..=end {
        match TcpListener::bind(SocketAddr::from((v4_host, candidate))).await {
            Ok(v4) => {
                let port = v4.local_addr().map_err(|e| e.to_string())?.port();
                return Ok(Bound { v6: bind_v6(lan, port), port, v4 });
            }
            Err(e) => last_err = format!("{candidate}: {e}"),
        }
    }
    Err(format!(
        "端口 {preferred}~{end} 全部被占用（最后错误 {last_err}）。请在设置里换一个端口。"
    ))
}

/// 在 `port` 上补一个 **only-v6** 的监听器。失败一律返 `None`（记一行 debug，不告警）。
///
/// 失败是**正常情况**而不是异常：容器/精简系统里 IPv6 常被整个停掉。
/// 这里发 `warn!` 会让那些用户每次启动代理都看到一条看不懂的告警，
/// 而它对他们毫无行动价值（他们的客户端本来就走 v4）。
fn bind_v6(lan: bool, port: u16) -> Option<TcpListener> {
    let host: std::net::Ipv6Addr = if lan {
        std::net::Ipv6Addr::UNSPECIFIED // ::
    } else {
        std::net::Ipv6Addr::LOCALHOST // ::1
    };
    let addr = SocketAddr::from((host, port));
    let sock = Socket::new(Domain::IPV6, Type::STREAM, Some(SockProtocol::TCP)).ok()?;
    // 见模块注释：这一行是三平台行为一致的唯一保证，别删。
    sock.set_only_v6(true).ok()?;
    sock.set_nonblocking(true).ok()?;
    sock.bind(&addr.into()).ok()?;
    // backlog 取值对齐 tokio `TcpListener::bind` 的默认（1024）。
    sock.listen(1024).ok()?;
    match TcpListener::from_std(std::net::TcpListener::from(sock)) {
        Ok(l) => {
            tracing::info!("代理已额外监听 IPv6 {addr}");
            Some(l)
        }
        Err(e) => {
            tracing::debug!("IPv6 监听不可用（{addr}）：{e}");
            None
        }
    }
}

/// 从两个监听器里取下一个连接。
///
/// `v6` 为 `None` 时退化成只等 v4 —— 不能写成 `select!` 里放一个立即就绪的空分支，
/// 那会把 accept 循环变成忙等（本模块第一版就这么写过，CPU 直接跑满一个核）。
pub(crate) async fn accept_either(
    b: &Bound,
) -> std::io::Result<(tokio::net::TcpStream, SocketAddr)> {
    match &b.v6 {
        None => b.v4.accept().await,
        Some(v6) => {
            tokio::select! {
                r = b.v4.accept() => r,
                r = v6.accept() => r,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 这台机器上 IPv6 到底可不可用 —— **独立判定，不看被测代码的结论**。
    ///
    /// 🔴 这个函数是本测试段最要紧的一件事。第一版写的是
    /// `match &b.v6 { Some(..) => assert!(..), None => eprintln!("跳过") }` ——
    /// 那个 `None` 分支是个**逃生口**：把 `bind_v6` 整个删掉（= 双栈功能没了，
    /// 也就是本模块要修的缺陷本体），所有 v6 用例都走进 `None` 分支**照样全绿**。
    /// 实测确认过：注入 `v6: None` 后 10 条用例无一变红。
    ///
    /// 故改为先自己绑一次 `::1` 探明真相：**能绑上就必须要求被测代码也绑上**。
    /// 只有在真的没有 IPv6 栈的机器上（容器、精简系统）才放过。
    /// 同本仓「注入不变红时先怀疑用例没压到那个分支」那条。
    fn ipv6_available() -> bool {
        std::net::TcpListener::bind("[::1]:0").is_ok()
    }

    /// v4 必须绑上，且返回的端口真的能连。
    #[tokio::test]
    async fn binds_v4_and_reports_the_real_port() {
        let b = bind_sticky(false, 0, 0).await.expect("绑 0 号端口应成功");
        assert_ne!(b.port, 0, "必须回报内核实际分配的端口，不能回 0");
        let addr = b.v4.local_addr().unwrap();
        assert_eq!(addr.port(), b.port);
        assert!(addr.ip().is_loopback(), "未开局域网时只许绑本机");
    }

    /// 🔴 **本机有 IPv6 时就必须绑上 v6** —— 这是本模块存在的理由。
    ///
    /// 判据用 [`ipv6_available`] 独立探明，**不接受「被测代码说没有就没有」**。
    #[tokio::test]
    async fn binds_v6_whenever_the_machine_has_ipv6() {
        if !ipv6_available() {
            eprintln!("本机无 IPv6 栈，跳过");
            return;
        }
        let b = bind_sticky(false, 0, 0).await.unwrap();
        assert!(
            b.v6.is_some(),
            "本机 IPv6 可用（测试自己刚绑成功过 ::1），双栈却没绑上 —— \
             IPv6 客户端会连不上，而那正是本模块要修的缺陷"
        );
    }

    /// 🔴 **未开局域网时绝不能绑到通配地址** —— 那等于把代理暴露给整个局域网，
    /// 而界面显示的是「局域网暴露：关闭」。这是安全方向的「界面说 A、实际 B」。
    #[tokio::test]
    async fn without_lan_neither_socket_is_on_a_wildcard_address() {
        let b = bind_sticky(false, 0, 0).await.unwrap();
        assert!(b.v4.local_addr().unwrap().ip().is_loopback());
        if ipv6_available() {
            let ip = b.v6.as_ref().expect("本机有 IPv6，必须绑上").local_addr().unwrap().ip();
            assert!(ip.is_loopback(), "v6 也必须是 ::1 而不是 ::，实得 {ip}");
        }
    }

    /// 端口粘滞：首选端口被占时应在范围内往上找，而不是直接失败或回随机端口。
    #[tokio::test]
    async fn falls_forward_within_the_range_when_the_preferred_port_is_taken() {
        // 先占住一个端口，再以它为首选去绑。
        let squatter = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let taken = squatter.local_addr().unwrap().port();

        let b = bind_sticky(false, taken, 8).await.expect("应回退到相邻端口");
        assert_ne!(b.port, taken, "被占的端口不该被当成绑成功");
        assert!(
            b.port > taken && b.port <= taken + 8,
            "应落在 [{taken}, {}] 内，实得 {}",
            taken + 8,
            b.port
        );
    }

    /// 范围内全被占时要报错，且错误信息里带端口范围（用户据此去改设置）。
    #[tokio::test]
    async fn reports_the_range_when_everything_is_taken() {
        let squatter = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let taken = squatter.local_addr().unwrap().port();
        // range = 0 → 只试 taken 这一个，必失败
        let err = bind_sticky(false, taken, 0).await.expect_err("应失败");
        assert!(err.contains(&taken.to_string()), "错误信息要带端口号: {err}");
        assert!(err.contains("设置"), "要告诉用户去哪改: {err}");
    }

    /// 🔴 **v6 与 v4 必须在同一个端口上。**
    ///
    /// 两者端口不同的话，用户在客户端里配的那一个只有一半能用，
    /// 而「IPv6 客户端连不上」正是本模块要修的缺陷 —— 只是换了个更难看出的形态。
    #[tokio::test]
    async fn the_v6_listener_shares_the_v4_port() {
        if !ipv6_available() {
            eprintln!("本机无 IPv6 栈，跳过");
            return;
        }
        let b = bind_sticky(false, 0, 0).await.unwrap();
        let v6 = b.v6.as_ref().expect("本机有 IPv6，必须绑上");
        assert_eq!(v6.local_addr().unwrap().port(), b.port, "v6 与 v4 必须同端口");
    }

    /// 🔴 **只有 v4 成功才算端口可用。**
    ///
    /// 判据反过来（v6 成功就接受）会造成一个真实的错路由：Windows 上别的进程占着
    /// `0.0.0.0:P`，我们的 `[::]:P` 却能绑上，于是客户端连 `127.0.0.1:P` 落进
    /// **别人的**进程，而我们显示「已启动」。这条用「v4 被占则整体失败」把方向钉住。
    #[tokio::test]
    async fn a_taken_v4_port_fails_even_if_v6_would_be_free() {
        let squatter = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let taken = squatter.local_addr().unwrap().port();
        assert!(
            bind_sticky(false, taken, 0).await.is_err(),
            "v4 被占就必须失败，不能因为 v6 空闲而算成功"
        );
    }

    /// v6 缺失时 `accept_either` 不得忙等 —— 它必须真的挂在 v4 上等。
    ///
    /// 判据：给一个没有 v6 的 Bound，在超时内不返回（说明在等，而不是立刻返回 Pending 转圈）。
    /// 第一版把 `None` 写成 `select!` 里一个立即就绪的分支，CPU 跑满一个核而功能看着正常。
    #[tokio::test]
    async fn accept_either_waits_instead_of_spinning_when_v6_is_absent() {
        let mut b = bind_sticky(false, 0, 0).await.unwrap();
        b.v6 = None;
        let r = tokio::time::timeout(std::time::Duration::from_millis(120), accept_either(&b)).await;
        assert!(r.is_err(), "没有连接进来时应一直等待（超时），而不是立即返回");
    }

    /// 两个栈都能收到连接（本机有 IPv6 时）。
    #[tokio::test]
    async fn accepts_from_both_stacks() {
        let has_v6 = ipv6_available();
        let b = bind_sticky(false, 0, 0).await.unwrap();
        assert_eq!(
            b.v6.is_some(),
            has_v6,
            "绑定结果必须与本机真实的 IPv6 可用性一致"
        );
        let port = b.port;
        let want = if has_v6 { 2 } else { 1 };

        let h = tokio::spawn(async move {
            let mut peers = Vec::new();
            for _ in 0..want {
                let (_s, peer) = accept_either(&b).await.unwrap();
                peers.push(peer);
            }
            peers
        });

        let _c4 = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let _c6 = if has_v6 {
            Some(tokio::net::TcpStream::connect(("::1", port)).await.unwrap())
        } else {
            None
        };

        let peers = tokio::time::timeout(std::time::Duration::from_secs(5), h)
            .await
            .expect("不该超时 —— 超时说明某个栈上的连接压根没被 accept 到")
            .unwrap();
        assert_eq!(peers.len(), want);
        assert!(peers.iter().any(|p| p.is_ipv4()), "应收到一条 v4 连接");
        if has_v6 {
            assert!(peers.iter().any(|p| p.is_ipv6()), "应收到一条 v6 连接");
        }
    }

    /// 🔴 **`set_only_v6(true)` 必须在。**
    ///
    /// 这是三平台行为一致的唯一保证（Windows/macOS 默认 only-v6，Linux 默认双栈）。
    /// 少了它，Linux 上 `[::]:P` 会与已绑的 `0.0.0.0:P` 撞 `EADDRINUSE` → v6 静默退化，
    /// 也就是本模块要修的缺陷只在一个平台上复发。本机是 Windows，运行时测不出来，
    /// 故用源码级判据钉住。
    ///
    /// ⚠️ **必须用 `production_code_only` 而不是 `production_slice`** —— 本模块的
    /// 模块注释里就写着「为什么必须显式 `set_only_v6(true)`」，用后者的话
    /// **把那行代码删掉判据照样绿**（注释替代码满足了断言）。注入实测确认过这一点：
    /// 11 条注入里它是唯一一条没变红的。详见 `production_code_only` 的文档。
    #[test]
    fn only_v6_must_be_set_explicitly() {
        let code =
            crate::proxy::custom_headers::production_code_only(include_str!("proxy_listen.rs"));
        assert!(
            code.contains("set_only_v6(true)"),
            "v6 socket 必须显式 only_v6(true)，不能依赖平台默认值"
        );
    }

    /// 🔴 **接线判据：`ProxyManager::start` 必须走本模块，且 accept 走 `accept_either`。**
    ///
    /// 上面所有用例都直接调 `bind_sticky`，于是**把 proxy.rs 改回自己绑 `0.0.0.0` 它们照样全绿**
    /// —— 而那就是「IPv6 客户端连不上」这个缺陷本身，且表现是静默的。
    /// 同本仓反复踩的那类盲区（`route_meta` / `lan_guard` 的 peer / `log_rotate` 写线程）。
    #[test]
    fn proxy_start_must_go_through_this_module() {
        let prod = crate::proxy::custom_headers::production_slice(include_str!("proxy.rs"));
        assert!(
            prod.contains("proxy_listen::bind_sticky("),
            "ProxyManager::start 必须用 bind_sticky（粘滞端口 + 双栈都在里面）"
        );
        assert!(
            prod.contains("proxy_listen::accept_either("),
            "accept 循环必须用 accept_either，否则 v6 监听器永远不会被 poll"
        );
        // 旧形态：自己拼 v4 地址去 bind。留着即说明有第二条绑定路径。
        assert!(
            !prod.contains("TcpListener::bind(SocketAddr::from((host, candidate)))"),
            "proxy.rs 里不该再有自己的端口扫描旁路 —— 绑定判据要留在 proxy_listen 一处"
        );
    }
}
