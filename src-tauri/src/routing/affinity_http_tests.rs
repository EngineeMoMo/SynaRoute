use super::{config::{AffinityMode, CategoryPolicy}, Store};
use crate::{model::{CategoryType, ModelInfo, Protocol, ProviderKey}, proxy::ProxyManager};
use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
use serde_json::json;
use http_body_util::Full;
use hyper::{Response, service::service_fn};
use hyper_util::rt::TokioIo;

struct Upstream { url: String, count: Arc<AtomicUsize>, mode: Arc<AtomicUsize>, task: tokio::task::JoinHandle<()> }
impl Drop for Upstream { fn drop(&mut self) { self.task.abort(); } }
async fn upstream(mode: usize) -> Upstream {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let count = Arc::new(AtomicUsize::new(0)); let state = Arc::new(AtomicUsize::new(mode));
    let c = count.clone(); let m = state.clone();
    let task = tokio::spawn(async move {
        loop {
            let Ok((socket, _)) = listener.accept().await else { break; };
            let c = c.clone(); let m = m.clone();
            tokio::spawn(async move {
                let svc = service_fn(move |_| {
                    c.fetch_add(1, Ordering::SeqCst);
                    let mode = m.load(Ordering::SeqCst);
                    async move {
                        let (status, mime, body) = match mode {
                            1 => (500, "application/json", r#"{"error":{"message":"temporary failure"}}"#),
                            2 => (200, "text/event-stream", "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"m\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"m\",\"content\":[],\"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n\nevent: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"failed stream\"}}\n\n"),
                            _ => (200, "application/json", r#"{"id":"m","type":"message","role":"assistant","content":[{"type":"text","text":"ok"}],"stop_reason":"end_turn"}"#),
                        };
                        Ok::<_, std::convert::Infallible>(Response::builder().status(status).header("content-type", mime).body(Full::new(bytes::Bytes::from(body))).unwrap())
                    }
                });
                let _ = hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(socket), svc).await;
            });
        }
    });
    Upstream { url, count, mode: state, task }
}

#[tokio::test]
async fn routing_http_affinity_failover_stream_failure_and_breaker() {
    let a = upstream(1).await; let b = upstream(0).await;
    let dir = std::env::temp_dir().join(format!("sr-affinity-http-{}", uuid::Uuid::new_v4()));
    let store = Arc::new(Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap());
    let make = |priority, url: &str| ProviderKey { id: uuid::Uuid::new_v4().to_string(), enabled: true, priority, base_url: url.into(), protocol: Protocol::Anthropic,
        models: vec![ModelInfo { real_name: "m".into(), source: "manual".into(), fetched_at: None, context_window: None, max_output_tokens: None }], ..Default::default() };
    let mut ka = make(0, &a.url); let mut kb = make(1, &b.url);
    for key in [&mut ka, &mut kb] {
        key.balance_query = Some(crate::model::BalanceQuery { enabled: true, ..Default::default() });
        key.budget_usd = Some(0.001);
    }
    for key in [&ka, &kb] { store.upsert_key(key.clone()).unwrap(); store.secrets.write().set(&key.id, "dummy").unwrap(); }
    super::save_policy(&store, CategoryType::ClaudeCli, CategoryPolicy { affinity: AffinityMode::Session, ..Default::default() }, None, 0).unwrap();
    store.set_proxy_port(CategoryType::ClaudeCli, 0).unwrap();
    let proxy = ProxyManager::new(store.clone()); let port = proxy.start(CategoryType::ClaudeCli).await.unwrap();
    let client = reqwest::Client::builder().no_proxy().timeout(std::time::Duration::from_secs(10)).build().unwrap();
    let send = |stream| client.post(format!("http://127.0.0.1:{port}/v1/messages")).header("x-synaroute-session-id", "affinity-test").json(&json!({"model":"m","max_tokens":20,"stream":stream,"messages":[{"role":"user","content":"hello"}]})).send();
    let first = send(false).await.unwrap(); assert_eq!(first.status(), 200); first.bytes().await.unwrap();
    assert!(a.count.load(Ordering::SeqCst) > 0); assert_eq!(b.count.load(Ordering::SeqCst), 1);
    a.mode.store(0, Ordering::SeqCst); let a_before = a.count.load(Ordering::SeqCst);
    send(false).await.unwrap().bytes().await.unwrap();
    assert_eq!(a.count.load(Ordering::SeqCst), a_before, "successful fallback must stay preferred");
    assert_eq!(b.count.load(Ordering::SeqCst), 2);
    b.mode.store(2, Ordering::SeqCst);
    let failed = send(true).await.unwrap().text().await.unwrap(); assert!(failed.contains("error"));
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop { if store.routing_runtime.sessions.lock().values().all(|s| s.binding.is_none()) { break; } tokio::task::yield_now().await; }
    }).await.expect("stream failure must clear affinity through the production callback");
    b.mode.store(0, Ordering::SeqCst);
    send(false).await.unwrap().bytes().await.unwrap();
    assert_eq!(a.count.load(Ordering::SeqCst), a_before + 1);
    let mut health = store.get_key(&ka.id).unwrap().health; health.breaker_until = Some(chrono::Utc::now().timestamp_millis() + 60_000);
    store.update_health(&ka.id, health).unwrap();
    send(false).await.unwrap().bytes().await.unwrap();
    assert_eq!(a.count.load(Ordering::SeqCst), a_before + 1, "affinity must not bypass an active breaker");
    // B is retained; mark only its balance exhausted without changing configuration identity.
    let mut health = store.get_key(&ka.id).unwrap().health; health.breaker_until = None; store.update_health(&ka.id, health).unwrap();
    let mut balance = crate::model::BalanceResult::failed("fixture"); balance.ok = true; balance.error = None; balance.remaining = Some(0.0);
    store.update_balance_cache(&kb.id, balance).unwrap();
    let b_before_balance = b.count.load(Ordering::SeqCst);
    send(false).await.unwrap().bytes().await.unwrap();
    assert_eq!(b.count.load(Ordering::SeqCst), b_before_balance, "affinity must not override exhausted balance");
    let mut balance = crate::model::BalanceResult::failed("fixture"); balance.ok = true; balance.error = None; balance.remaining = Some(100.0);
    store.update_balance_cache(&kb.id, balance).unwrap();
    // A is retained; charge it beyond its budget and require B instead.
    store.append_event_model(CategoryType::ClaudeCli, "system", Some(&ka.id), "fixture usage", None, None,
        Some(crate::upstream::TokenUsage { input: 1_000_000, output: 0, cache_read: 0, cache_creation: 0 }), "gpt-4o");
    assert!(crate::usage_cost::over_budget_ids(&store, CategoryType::ClaudeCli).contains(&ka.id));
    let a_before_budget = a.count.load(Ordering::SeqCst);
    send(false).await.unwrap().bytes().await.unwrap();
    assert_eq!(a.count.load(Ordering::SeqCst), a_before_budget, "affinity must not override budget ranking");
    // B is now retained. Quota must move the next request back to A.
    crate::health::quota_window::arm(&store, CategoryType::ClaudeCli, &kb.id, 60);
    let b_before = b.count.load(Ordering::SeqCst);
    send(false).await.unwrap().bytes().await.unwrap();
    assert_eq!(b.count.load(Ordering::SeqCst), b_before, "affinity must not bypass quota");
    proxy.stop(CategoryType::ClaudeCli); drop(proxy); drop(store); std::fs::remove_dir_all(dir).ok();
}
