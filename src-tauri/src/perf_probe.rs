//! 性能与内存实测探针（`#[ignore]`，按需手动跑，不进常规 `cargo test`）。
//!
//! 背景：此前只做过「消除明显浪费」（P2-5 零拷贝、P1-3 异步日志等），**从未量过任何数字**。
//! 没有基线就无法回答「优化好了吗」——本模块提供可复现的三项测量：
//!
//! 1. [`measure_forward_latency`]：代理自身引入的转发开销分布（p50/p90/p99）。
//!    对照组是「客户端直连 mock 上游」，两者相减才是 SynaRoute 的净开销。
//! 2. [`measure_rss_under_load`]：跑 N 轮请求前后的进程 RSS，看负载后是否回落
//!    （判泄漏最直接的办法：涨上去不回落 = 有东西被一直攥着）。
//! 3. [`measure_store_growth`]：只打请求、不重启，观察 Store 内存结构（日志环、
//!    用量累计、健康态）随请求数的增长是否有界。
//!
//! 跑法：
//! ```text
//! cargo test --lib perf_probe -- --ignored --nocapture --test-threads=1
//! ```
//! `--test-threads=1` 必须：RSS 是进程级指标，并发跑会互相污染。

#![cfg(test)]

use crate::model::{CategoryType, HealthState, KeyParams, ProviderKey, Protocol};
use crate::proxy::ProxyManager;
use crate::store::Store;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// 当前进程 RSS（字节）。Windows 走 GetProcessMemoryInfo 的等价物：
/// 直接读 `/proc/self/statm` 不可用，故用 sysinfo-free 的平台调用。
/// 为免引入新依赖，这里用 PowerShell 查自己的 WorkingSet64（够精确，且只在 ignore 测试里跑）。
fn current_rss_bytes() -> Option<u64> {
    let pid = std::process::id();
    #[cfg(windows)]
    {
        let out = std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                &format!("(Get-Process -Id {pid}).WorkingSet64"),
            ])
            .output()
            .ok()?;
        String::from_utf8_lossy(&out.stdout).trim().parse::<u64>().ok()
    }
    #[cfg(not(windows))]
    {
        let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
        let pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
        Some(pages * 4096)
    }
}

fn mb(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

/// 分位数（p 取 0.0~1.0）。样本会被排序。
fn pct(sorted: &[u128], p: f64) -> u128 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[idx]
}

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("synaroute_perf_{tag}_{}", std::process::id()));
    std::fs::create_dir_all(&d).ok();
    d
}

fn bench_key(id: &str, base_url: &str) -> ProviderKey {
    ProviderKey {
        tier_fable: None,
        id: id.into(),
        category_id: CategoryType::ClaudeCli,
        name: format!("bench-{id}"),
        vendor: "bench".into(),
        base_url: base_url.into(),
        protocol: Protocol::Anthropic,
        has_secret: true,
        enabled: true,
        allow_in_aggregate: false,
        priority: 0,
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

/// 起一个极简 mock 上游（固定 200 + 小 JSON），返回 base_url。
/// 刻意不带延迟：要量的是**代理自身**开销，上游延迟会把信号淹掉。
async fn spawn_fast_mock() -> String {
    use http_body_util::Full;
    use hyper::service::service_fn;
    use hyper::{Request, Response};
    use hyper_util::rt::TokioIo;
    use tokio::net::TcpListener;

    const BODY: &[u8] =
        br#"{"id":"m","type":"message","role":"assistant","content":[{"type":"text","text":"ok"}],"usage":{"input_tokens":10,"output_tokens":5}}"#;

    let listener = TcpListener::bind(std::net::SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else { break };
            let io = TokioIo::new(stream);
            tokio::spawn(async move {
                let svc = service_fn(move |req: Request<hyper::body::Incoming>| async move {
                    // 必须把 body 读干，否则连接复用会错乱
                    use http_body_util::BodyExt;
                    let _ = req.into_body().collect().await;
                    Ok::<_, hyper::Error>(
                        Response::builder()
                            .status(200)
                            .header("content-type", "application/json")
                            .body(Full::new(bytes::Bytes::from_static(BODY)))
                            .unwrap(),
                    )
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(io, svc)
                    .await;
            });
        }
    });
    format!("http://{addr}")
}

/// 构造一个「代理已就绪」的环境，返回 (store, proxy, 代理端口, mock base_url, 临时目录)。
async fn setup(tag: &str) -> (Arc<Store>, Arc<ProxyManager>, u16, String, std::path::PathBuf) {
    let mock = spawn_fast_mock().await;
    let dir = temp_dir(tag);
    let store = Arc::new(
        Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap(),
    );
    store.upsert_key(bench_key("k1", &mock)).unwrap();
    store.secrets.write().set("k1", "sk-bench").unwrap();
    let proxy = Arc::new(ProxyManager::new(store.clone()));
    let port = proxy.start(CategoryType::ClaudeCli).await.unwrap();
    (store, proxy, port, mock, dir)
}

fn payload() -> serde_json::Value {
    serde_json::json!({
        "model": "claude-sonnet-4-5",
        "max_tokens": 64,
        "messages": [{"role": "user", "content": "hi"}]
    })
}

/// 测量 1：转发延迟分布 vs 直连对照组。
///
/// 两组都打同一个 mock：一组经代理、一组直连。差值即 SynaRoute 净开销
/// （路由决策 + JSON 改写 + 协议判定 + 日志/用量记账）。
#[tokio::test]
#[ignore = "性能实测，手动跑：cargo test --lib perf_probe -- --ignored --nocapture --test-threads=1"]
async fn measure_forward_latency() {
    const WARMUP: usize = 20;
    const N: usize = 300;

    let (_store, proxy, port, mock, dir) = setup("latency").await;
    let client = reqwest::Client::new();
    let via_proxy = format!("http://127.0.0.1:{port}/v1/messages");
    let direct = format!("{mock}/v1/messages");

    // 预热两条路径（建连接池、JIT 各种 lazy init），不计入统计
    for url in [&via_proxy, &direct] {
        for _ in 0..WARMUP {
            let _ = client.post(url).json(&payload()).send().await;
        }
    }

    let mut proxy_us = Vec::with_capacity(N);
    let mut direct_us = Vec::with_capacity(N);
    // 交替打，避免一段时间的系统噪声只落在一组上
    for _ in 0..N {
        let t = Instant::now();
        let r = client.post(&via_proxy).json(&payload()).send().await.unwrap();
        assert_eq!(r.status().as_u16(), 200);
        let _ = r.bytes().await;
        proxy_us.push(t.elapsed().as_micros());

        let t = Instant::now();
        let r = client.post(&direct).json(&payload()).send().await.unwrap();
        let _ = r.bytes().await;
        direct_us.push(t.elapsed().as_micros());
    }
    proxy_us.sort_unstable();
    direct_us.sort_unstable();

    println!("\n=== 测量 1：转发延迟（{N} 次，单位 µs）===");
    for (name, v) in [("经代理", &proxy_us), ("直连上游", &direct_us)] {
        println!(
            "  {name:8} p50={:>6}  p90={:>6}  p99={:>7}  max={:>7}",
            pct(v, 0.50),
            pct(v, 0.90),
            pct(v, 0.99),
            v.last().copied().unwrap_or(0)
        );
    }
    let overhead_p50 = pct(&proxy_us, 0.50) as i128 - pct(&direct_us, 0.50) as i128;
    let overhead_p99 = pct(&proxy_us, 0.99) as i128 - pct(&direct_us, 0.99) as i128;
    println!(
        "  → SynaRoute 净开销  p50≈{:.2} ms   p99≈{:.2} ms",
        overhead_p50 as f64 / 1000.0,
        overhead_p99 as f64 / 1000.0
    );

    proxy.stop(CategoryType::ClaudeCli);
    std::fs::remove_dir_all(&dir).ok();
}

/// 测量 2：负载前后 RSS，判是否有「涨上去不回落」。
#[tokio::test]
#[ignore = "性能实测，手动跑：cargo test --lib perf_probe -- --ignored --nocapture --test-threads=1"]
async fn measure_rss_under_load() {
    const ROUNDS: usize = 5;
    const PER_ROUND: usize = 200;

    let (store, proxy, port, _mock, dir) = setup("rss").await;
    let client = reqwest::Client::new();
    let url = format!("http://127.0.0.1:{port}/v1/messages");

    // 基线：起完代理、打过 0 个请求
    let base = current_rss_bytes();
    println!("\n=== 测量 2：RSS 随负载变化 ===");
    if let Some(b) = base {
        println!("  基线（代理已起、0 请求）      : {:.1} MB", mb(b));
    } else {
        println!("  ⚠ 拿不到 RSS，跳过该测量");
        proxy.stop(CategoryType::ClaudeCli);
        std::fs::remove_dir_all(&dir).ok();
        return;
    }

    let mut samples = Vec::new();
    for round in 1..=ROUNDS {
        for _ in 0..PER_ROUND {
            let r = client.post(&url).json(&payload()).send().await.unwrap();
            let _ = r.bytes().await;
        }
        let rss = current_rss_bytes().unwrap_or(0);
        samples.push(rss);
        println!(
            "  第 {round} 轮后（累计 {} 请求）      : {:.1} MB",
            round * PER_ROUND,
            mb(rss)
        );
    }

    // 静置：让定时落盘/清理跑一轮，看是否回落
    println!("  静置 3s（等定时任务跑一轮）...");
    tokio::time::sleep(Duration::from_secs(3)).await;
    let after_idle = current_rss_bytes().unwrap_or(0);
    println!("  静置后                        : {:.1} MB", mb(after_idle));

    let base = base.unwrap();
    let peak = samples.iter().copied().max().unwrap_or(0);
    let growth_per_1k =
        (peak as f64 - base as f64) / (ROUNDS * PER_ROUND) as f64 * 1000.0 / (1024.0 * 1024.0);
    println!(
        "  → 峰值增长 {:.1} MB（{} 请求）；折算约 {:.2} MB / 1000 请求",
        mb(peak.saturating_sub(base)),
        ROUNDS * PER_ROUND,
        growth_per_1k
    );
    println!(
        "  → 判读：日志环/用量/健康态都是**有界**结构，若折算值随轮次线性不收敛才是泄漏嫌疑"
    );
    // 顺带打印 Store 侧的可见规模，帮助归因（用量按 Key 聚合，条数应恒等于 Key 数 —— 有界）
    println!(
        "  用量聚合条数（应 == Key 数）  : {}",
        store.token_usage_by_key().len()
    );

    proxy.stop(CategoryType::ClaudeCli);
    std::fs::remove_dir_all(&dir).ok();
}

/// 测量 3：长跑观察——持续打请求，每隔一段打印 RSS 与日志条数，看曲线是否走平。
///
/// 默认 60s；用 `SYNAROUTE_PERF_SECS=600` 可拉长到 10 分钟做更可信的判泄漏。
#[tokio::test]
#[ignore = "长跑实测，手动跑：SYNAROUTE_PERF_SECS=600 cargo test --lib perf_probe -- --ignored --nocapture --test-threads=1"]
async fn measure_long_run_stability() {
    let secs: u64 = std::env::var("SYNAROUTE_PERF_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(60);

    let (store, proxy, port, _mock, dir) = setup("longrun").await;
    let client = reqwest::Client::new();
    let url = format!("http://127.0.0.1:{port}/v1/messages");

    println!("\n=== 测量 3：长跑 {secs}s 稳定性 ===");
    let start = Instant::now();
    let mut sent: u64 = 0;
    let mut next_report = Duration::from_secs(10);
    let mut curve: Vec<(u64, u64, u64)> = Vec::new(); // (秒, RSS, 已发)

    while start.elapsed() < Duration::from_secs(secs) {
        for _ in 0..50 {
            if let Ok(r) = client.post(&url).json(&payload()).send().await {
                let _ = r.bytes().await;
                sent += 1;
            }
        }
        if start.elapsed() >= next_report {
            let rss = current_rss_bytes().unwrap_or(0);
            let el = start.elapsed().as_secs();
            println!("  t={el:>4}s  已发 {sent:>6}  RSS {:.1} MB", mb(rss));
            curve.push((el, rss, sent));
            next_report += Duration::from_secs(10);
        }
    }

    println!("  静置 5s 后再看一次...");
    tokio::time::sleep(Duration::from_secs(5)).await;
    let final_rss = current_rss_bytes().unwrap_or(0);
    println!("  收尾 RSS: {:.1} MB（共发 {sent} 请求）", mb(final_rss));
    println!(
        "  用量聚合条数（应 == Key 数）: {}",
        store.token_usage_by_key().len()
    );

    if curve.len() >= 2 {
        let (_, first_rss, _) = curve[0];
        let (_, last_rss, _) = *curve.last().unwrap();
        let delta = last_rss as i64 - first_rss as i64;
        println!(
            "  → 首末采样差 {:+.1} MB。判读：请求持续注入下 RSS 走平（差值在噪声量级）= 无明显泄漏；\n     若单调上升且不回落，才需要进一步 profiling。",
            delta as f64 / (1024.0 * 1024.0)
        );
    }

    proxy.stop(CategoryType::ClaudeCli);
    std::fs::remove_dir_all(&dir).ok();
}
