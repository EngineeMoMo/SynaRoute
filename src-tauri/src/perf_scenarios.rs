use super::{bench_key, current_rss_bytes, payload, pct};
use crate::{model::{CategoryType, Protocol}, proxy::ProxyManager, store::Store};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, Response, service::service_fn};
use hyper_util::rt::TokioIo;
use serde_json::json;
use std::{sync::{Arc, atomic::{AtomicUsize, Ordering}}, time::{Duration, Instant}};

const STREAM: &str = "data: {\"id\":\"bench\",\"object\":\"chat.completion.chunk\",\"model\":\"bench\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"ok\"},\"finish_reason\":null}]}\n\ndata: {\"id\":\"bench\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5}}\n\ndata: [DONE]\n\n";

async fn upstream(status: u16, streaming: bool, delay: Duration) -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind(std::net::SocketAddr::from(([127, 0, 0, 1], 0))).await.unwrap();
    let address = listener.local_addr().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let task = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let counter = counter.clone();
            tokio::spawn(async move {
                let service = service_fn(move |request: Request<hyper::body::Incoming>| {
                    let counter = counter.clone();
                    async move {
                        request.into_body().collect().await.unwrap();
                        counter.fetch_add(1, Ordering::Relaxed);
                        if !delay.is_zero() { tokio::time::sleep(delay).await; }
                        let body = if status != 200 { "temporary failure" } else if streaming { STREAM } else {
                            r#"{"id":"bench","type":"message","role":"assistant","content":[{"type":"text","text":"ok"}],"usage":{"input_tokens":10,"output_tokens":5}}"#
                        };
                        Ok::<_, std::convert::Infallible>(Response::builder().status(status)
                            .header("content-type", if streaming { "text/event-stream" } else { "application/json" })
                            .body(Full::new(Bytes::from_static(body.as_bytes()))).unwrap())
                    }
                });
                let _ = hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(stream), service).await;
            });
        }
    });
    (format!("http://{address}"), calls, task)
}

fn distribution(mut samples: Vec<u128>) -> serde_json::Value {
    samples.sort_unstable();
    json!({"p50":pct(&samples, 0.50), "p95":pct(&samples, 0.95), "p99":pct(&samples, 0.99)})
}

#[tokio::test]
#[ignore = "performance report: npm run perf"]
async fn measure_scenario_matrix() {
    for (name, large, failover, streaming, delay) in [
        ("buffered_single", false, false, false, 0),
        ("large_context_two_keys", true, false, false, 0),
        ("failover_two_keys", false, true, false, 0),
        ("cross_protocol_sse", false, false, true, 0),
        ("upstream_wait", false, false, false, 10),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::new_at(directory.path().join("config.json"), directory.path().join("secrets.enc")).unwrap());
        let (endpoint, calls, server) = upstream(200, streaming, Duration::from_millis(delay)).await;
        let (failed_endpoint, failures, failed_server) = upstream(503, false, Duration::ZERO).await;
        let mut primary = bench_key(&format!("{name}-primary"), if failover { &failed_endpoint } else { &endpoint });
        primary.protocol = if streaming { Protocol::OpenaiChat } else { Protocol::Anthropic };
        store.upsert_key(primary.clone()).unwrap();
        store.secrets.write().set(&primary.id, "synthetic-test-key").unwrap();
        if large || failover {
            let mut backup = bench_key(&format!("{name}-backup"), &endpoint);
            backup.priority = 1;
            store.upsert_key(backup.clone()).unwrap();
            store.secrets.write().set(&backup.id, "synthetic-test-key").unwrap();
        }
        let proxy = ProxyManager::new(store.clone());
        let port = proxy.start(CategoryType::ClaudeCli).await.unwrap();
        let target = format!("http://127.0.0.1:{port}/v1/messages");
        let direct = format!("{endpoint}/v1/messages");
        let client = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(10)).build().unwrap();
        let mut request = payload();
        if large { request["messages"][0]["content"] = json!("large-context-".repeat(80_000)); }
        if streaming { request["stream"] = json!(true); }
        let serialized = serde_json::to_vec(&request).unwrap();
        let samples = if large { 30 } else { 80 };
        let mut headers = Vec::new();
        let mut total = Vec::new();
        let mut direct_headers = Vec::new();
        let mut overhead = Vec::new();
        let rss_before = current_rss_bytes();
        for index in 0..samples + 5 {
            let started = Instant::now();
            let response = client.post(&target).header("content-type", "application/json").body(serialized.clone()).send().await.unwrap();
            assert_eq!(response.status().as_u16(), 200, "{name}");
            let header_us = started.elapsed().as_micros();
            let body = response.bytes().await.unwrap();
            if streaming { assert!(String::from_utf8_lossy(&body).contains("message_stop")); }
            let total_us = started.elapsed().as_micros();
            let direct_started = Instant::now();
            let response = client.post(&direct).header("content-type", "application/json").body(serialized.clone()).send().await.unwrap();
            assert_eq!(response.status().as_u16(), 200);
            let direct_us = direct_started.elapsed().as_micros();
            response.bytes().await.unwrap();
            if index >= 5 {
                headers.push(header_us); total.push(total_us); direct_headers.push(direct_us);
                overhead.push(header_us.saturating_sub(direct_us));
            }
        }
        assert!(calls.load(Ordering::Relaxed) >= samples);
        if failover { assert!(failures.load(Ordering::Relaxed) >= samples, "each sample must exercise failover"); }
        println!("SYNAROUTE_PERF {}", json!({"name":name, "samples":samples, "request_bytes":serialized.len(),
            "headers_us":distribution(headers), "total_us":distribution(total), "direct_headers_us":distribution(direct_headers),
            "paired_header_overhead_us":distribution(overhead), "rss_before_bytes":rss_before, "rss_after_bytes":current_rss_bytes()}));
        proxy.stop(CategoryType::ClaudeCli);
        server.abort(); failed_server.abort();
    }
}

#[tokio::test]
#[ignore = "performance report: npm run perf"]
async fn measure_key_queue() {
    let key = format!("perf-queue-{}", uuid::Uuid::new_v4());
    let mut permits = Vec::new();
    for _ in 0..crate::health::concurrency::MAX_INFLIGHT_PER_KEY {
        permits.push(crate::health::concurrency::acquire(&key).await);
    }
    let release = tokio::spawn(async move { tokio::time::sleep(Duration::from_millis(20)).await; drop(permits); });
    let started = Instant::now();
    let (permit, _) = crate::health::concurrency::acquire_with_budget(&key, Duration::from_secs(2)).await.unwrap();
    let queue_us = started.elapsed().as_micros();
    assert!(queue_us >= 15_000);
    drop(permit);
    release.await.unwrap();
    println!("SYNAROUTE_PERF {}", json!({"name":"key_queue", "samples":1, "queue_us":queue_us, "injected_hold_ms":20}));
}
