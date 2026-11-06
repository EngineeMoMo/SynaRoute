//! Isolated loopback fixtures: no production credentials or client configuration.
use super::{config::{AffinityMode, IntentRule, ModelRef, PrivacyConfig, RouteGroup}, identity::Identity, intent, Store};
use crate::model::{CategoryType, ModelInfo, Protocol, ProviderKey};
use serde_json::{json, Value};
use std::{sync::{Arc, atomic::{AtomicUsize, Ordering}}, time::{Duration, Instant}};
use http_body_util::{BodyExt, Full};
use hyper::{body::Incoming, service::service_fn, Request, Response};
use hyper_util::rt::TokioIo;

struct Fixture {
    dir: std::path::PathBuf,
    store: Arc<Store>,
    key: ProviderKey,
    group: RouteGroup,
    count: Arc<AtomicUsize>,
    bodies: Arc<parking_lot::Mutex<Vec<(String, Value)>>>,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) { self.server.abort(); let _ = std::fs::remove_dir_all(&self.dir); }
}
impl Fixture {
    async fn new(protocol: Protocol, status: u16, delay: Duration, answer: &str) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let count = Arc::new(AtomicUsize::new(0));
        let bodies = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let counter = count.clone(); let captured = bodies.clone(); let answer = answer.to_owned();
        let server = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else { break; };
                let count = counter.clone(); let bodies = captured.clone(); let answer = answer.clone();
                tokio::spawn(async move {
                    let service = service_fn(move |req: Request<Incoming>| {
                        let count = count.clone(); let bodies = bodies.clone(); let answer = answer.clone();
                        async move {
                            let path = req.uri().path().to_owned();
                            let bytes = req.into_body().collect().await.unwrap().to_bytes();
                            bodies.lock().push((path, serde_json::from_slice::<Value>(&bytes).unwrap()));
                            count.fetch_add(1, Ordering::SeqCst);
                            tokio::time::sleep(delay).await;
                            let body = match protocol {
                                Protocol::Anthropic => json!({"content":[{"type":"text","text":answer}],"stop_reason":"end_turn","usage":{"input_tokens":11,"output_tokens":3}}),
                                Protocol::OpenaiChat => json!({"choices":[{"message":{"content":answer},"finish_reason":"stop"}],"usage":{"prompt_tokens":11,"completion_tokens":3}}),
                                Protocol::OpenaiResponses => json!({"status":"completed","output":[{"type":"message","content":[{"type":"output_text","text":answer}]}],"usage":{"input_tokens":11,"output_tokens":3}}),
                            };
                            Ok::<_, std::convert::Infallible>(Response::builder().status(status).header("content-type", "application/json").body(Full::new(bytes::Bytes::from(body.to_string()))).unwrap())
                        }
                    });
                    let _ = hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(stream), service).await;
                });
            }
        });
        let dir = std::env::temp_dir().join(format!("sr-routing-matrix-{}", uuid::Uuid::new_v4()));
        let store = Arc::new(Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap());
        let key = ProviderKey { id: uuid::Uuid::new_v4().to_string(), category_id: CategoryType::Codex, base_url: endpoint, protocol, enabled: true,
            models: ["judge", "target", "default"].into_iter().map(|name| ModelInfo { real_name: name.into(), source: "manual".into(), fetched_at: None, context_window: None, max_output_tokens: None }).collect(), ..Default::default() };
        store.upsert_key(key.clone()).unwrap(); store.secrets.write().set(&key.id, "dummy-test-secret").unwrap();
        let group = RouteGroup { id: uuid::Uuid::new_v4().to_string(), name: "Test".into(), enabled: true, default_model: "default".into(), judge: Some(ModelRef { key_id: key.id.clone(), model: "judge".into() }), rules: vec![IntentRule { id: "tests".into(), intent: "write unit tests".into(), model: "target".into() }] };
        Self { dir, store, key, group, count, bodies, server }
    }
    fn identity(&self) -> Identity { Identity { session: "session".into(), turn: "turn".into(), position: 1, new_turn: true, prompt: Some("write tests".into()), source: "explicit" } }
    async fn select(&self, identity: &Identity, deadline: Option<Instant>) -> (String, &'static str) {
        tokio::time::timeout(Duration::from_secs(8), async {
            let lease = self.store.routing_runtime.begin(CategoryType::Codex, &identity.session, &identity.turn, identity.position, "cfg", AffinityMode::Session).unwrap();
            intent::select(&self.store, CategoryType::Codex, &self.group, Some(identity), Some(&lease), std::slice::from_ref(&self.key), false, &PrivacyConfig::default(), deadline).await.unwrap()
        }).await.expect("judge operation must finish within its bounded budget")
    }
}

#[test]
fn routing_terms_config_write_failure_keeps_old_secret_and_restart() {
    use super::config::{CategoryPolicy, PrivacyMode};
    let dir = std::env::temp_dir().join(format!("sr-terms-rollback-{}", uuid::Uuid::new_v4()));
    let config = dir.join("config.json"); let secrets = dir.join("secrets.enc");
    let store = Store::new_at(config.clone(), secrets.clone()).unwrap();
    let mut policy = CategoryPolicy::default(); policy.privacy.mode = PrivacyMode::Block;
    super::save_policy(&store, CategoryType::Codex, policy.clone(), Some(vec!["original-private-term".into()]), 0).unwrap();
    let old = super::settings(&store).unwrap();
    let vault = std::fs::read(&secrets).unwrap();
    let saved = dir.join("saved-config.json"); std::fs::rename(&config, &saved).unwrap(); std::fs::create_dir(&config).unwrap();
    assert!(super::save_policy(&store, CategoryType::Codex, policy, Some(vec!["replacement-private-term".into()]), 1).is_err());
    let current = super::settings(&store).unwrap();
    assert_eq!(current.revision, old.revision);
    assert_eq!(current.categories[&CategoryType::Codex], old.categories[&CategoryType::Codex]);
    assert_eq!(&*super::privacy::terms(&store, &current.categories[&CategoryType::Codex].privacy).unwrap(), &["original-private-term"]);
    assert_eq!(std::fs::read(&secrets).unwrap(), vault, "failed replacement must remove its new secret");
    std::fs::remove_dir(&config).unwrap(); std::fs::rename(saved, &config).unwrap(); drop(store);
    let restarted = Store::new_at(config, secrets).unwrap();
    assert_eq!(&*super::privacy::terms(&restarted, &super::settings(&restarted).unwrap().categories[&CategoryType::Codex].privacy).unwrap(), &["original-private-term"]);
    drop(restarted); std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn routing_matrix_three_protocols_single_flight_and_usage() {
    for protocol in [Protocol::Anthropic, Protocol::OpenaiChat, Protocol::OpenaiResponses] {
        let fixture = Fixture::new(protocol, 200, Duration::from_millis(30), r#"{"intent":"tests"}"#).await;
        let identity = fixture.identity();
        let (a, b) = tokio::join!(fixture.select(&identity, None), fixture.select(&identity, None));
        assert_eq!(a.0, "target"); assert_eq!(b.0, "target");
        assert_eq!(fixture.count.load(Ordering::SeqCst), 1, "same-turn concurrent requests must share a judgment");
        assert_eq!(fixture.select(&identity, None).await.0, "target");
        assert_eq!(fixture.count.load(Ordering::SeqCst), 1);
        let bodies = fixture.bodies.lock();
        assert_eq!(bodies[0].0, protocol.completion_path());
        assert_eq!(bodies[0].1["model"], "judge");
        assert!(!bodies[0].1.to_string().contains("encrypted_content"));
        let events = fixture.store.list_all_events();
        let event = events.iter().find(|e| e.detail.contains("Intent classifier usage"));
        assert!(event.is_some(), "judge usage needs a visible attributed event");
        assert_eq!(event.unwrap().key_id.as_deref(), Some(fixture.key.id.as_str()));
        let usage = fixture.store.token_usage_by_model();
        assert_eq!(usage.len(), 1);
        assert_eq!(usage[0].key_id, fixture.key.id);
        assert_eq!(usage[0].category_id, CategoryType::Codex);
        assert_eq!(usage[0].model, "judge");
        assert_eq!((usage[0].usage.input, usage[0].usage.output), (11, 3));
        assert_eq!(bodies[0].1.to_string().matches("write tests").count(), 1, "only the latest user text is supplied");
    }
}

#[tokio::test]
async fn routing_matrix_policy_manual_auto_and_tool_continuation() {
    use super::{config::CategoryPolicy, identity::{Hints, VerifiedPeer}, policy};
    let fixture = Fixture::new(Protocol::OpenaiResponses, 200, Duration::ZERO, r#"{"intent":"tests"}"#).await;
    let configured = CategoryPolicy { groups: vec![fixture.group.clone()], affinity: AffinityMode::Session, ..Default::default() };
    super::save_policy(&fixture.store, CategoryType::Codex, configured, None, 0).unwrap();
    let mut request = Request::builder().header("thread-id", "matrix-thread").body(()).unwrap();
    request.extensions_mut().insert(VerifiedPeer("127.0.0.1".parse().unwrap()));
    let hints = Hints::from_request(&request);
    let body = json!({"input":[{"role":"user","content":"write tests"}]});
    let manual = policy::prepare(&fixture.store, CategoryType::Codex, &body, Protocol::OpenaiResponses, &hints, "default", None).await.unwrap();
    assert_eq!(manual.model, "default"); assert_eq!(fixture.count.load(Ordering::SeqCst), 0);
    let auto = fixture.group.model_id();
    let chosen = policy::prepare(&fixture.store, CategoryType::Codex, &body, Protocol::OpenaiResponses, &hints, &auto, None).await.unwrap();
    assert_eq!(chosen.model, "target"); chosen.served(&fixture.store, &fixture.key, "target");
    let continuation = json!({"input":[{"role":"user","content":"write tests"},{"type":"function_call_output","call_id":"tool","output":"private tool output"}]});
    let reused = policy::prepare(&fixture.store, CategoryType::Codex, &continuation, Protocol::OpenaiResponses, &hints, &auto, None).await.unwrap();
    assert_eq!(reused.model, "target"); assert_eq!(fixture.count.load(Ordering::SeqCst), 1);
    let mut next = continuation.clone(); next["input"].as_array_mut().unwrap().push(json!({"role":"user","content":"next task"}));
    assert_eq!(policy::prepare(&fixture.store, CategoryType::Codex, &next, Protocol::OpenaiResponses, &hints, &auto, None).await.unwrap().model, "target");
    assert_eq!(fixture.count.load(Ordering::SeqCst), 2, "new user turns need their own judgment");
    assert!(!fixture.bodies.lock().iter().any(|(_, body)| body.to_string().contains("private tool output")));
    let unknown = format!("{}{}", super::config::AUTO_PREFIX, uuid::Uuid::new_v4());
    assert_eq!(policy::prepare(&fixture.store, CategoryType::Codex, &body, Protocol::OpenaiResponses, &hints, &unknown, None).await.err().unwrap().status(), hyper::StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn routing_matrix_judge_queue_and_four_second_cap() {
    use crate::health::concurrency;
    let fixture = Fixture::new(Protocol::OpenaiChat, 200, Duration::ZERO, r#"{"intent":"tests"}"#).await;
    let mut permits = Vec::new();
    for _ in 0..concurrency::MAX_INFLIGHT_PER_KEY { permits.push(tokio::time::timeout(Duration::from_secs(1), concurrency::acquire(&fixture.key.id)).await.unwrap()); }
    let started = Instant::now();
    assert_eq!(fixture.select(&fixture.identity(), Some(started + Duration::from_millis(40))).await.0, "default");
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(fixture.count.load(Ordering::SeqCst), 0, "queue time must be included before HTTP");
    drop(permits);
    let slow = Fixture::new(Protocol::Anthropic, 200, Duration::from_secs(6), r#"{"intent":"tests"}"#).await;
    let started = Instant::now();
    assert_eq!(slow.select(&slow.identity(), None).await.0, "default");
    assert!(started.elapsed() < Duration::from_millis(5500), "four-second cap must apply even without a shorter request budget");
}
#[tokio::test]
async fn routing_matrix_failure_cooldown_and_deadline() {
    let fixture = Fixture::new(Protocol::OpenaiChat, 503, Duration::ZERO, "unused").await;
    let identity = fixture.identity();
    assert_eq!(fixture.select(&identity, None).await, ("default".into(), "judge_http"));
    let mut next = identity.clone(); next.turn = "next".into(); next.position = 2;
    assert_eq!(fixture.select(&next, None).await, ("default".into(), "judge_cooldown"));
    assert_eq!(fixture.count.load(Ordering::SeqCst), 1);
    let slow = Fixture::new(Protocol::OpenaiResponses, 200, Duration::from_secs(1), r#"{"intent":"tests"}"#).await;
    let started = Instant::now();
    let result = slow.select(&slow.identity(), Some(started + Duration::from_millis(40))).await;
    assert_eq!(result.0, "default");
    assert!(started.elapsed() < Duration::from_millis(800), "request deadline must bound judge wait");
}

#[tokio::test]
async fn routing_matrix_invalid_answer_and_cache_isolation() {
    let fixture = Fixture::new(Protocol::OpenaiChat, 200, Duration::ZERO, r#"{"intent":"outside-rules"}"#).await;
    assert_eq!(fixture.select(&fixture.identity(), None).await, ("default".into(), "invalid_answer"));
    let fixture = Fixture::new(Protocol::Anthropic, 200, Duration::ZERO, r#"{"intent":"tests"}"#).await;
    let first = fixture.identity(); fixture.select(&first, None).await;
    let mut other = first.clone(); other.session = "another-session".into(); fixture.select(&other, None).await;
    assert_eq!(fixture.count.load(Ordering::SeqCst), 2, "different sessions must not reuse one another's cache");
}
