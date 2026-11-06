use super::*;
use crate::health::concurrency::{self, MAX_INFLIGHT_PER_KEY};
use futures_util::{Stream, StreamExt};
use http_body_util::StreamBody;
use hyper::{body::Incoming, service::service_fn, Request};
use hyper_util::rt::TokioIo;
use serde_json::json;
use std::{convert::Infallible, future::Future, io, sync::{atomic::{AtomicUsize, Ordering}, Weak}, time::Duration};
use tokio::{net::TcpListener, sync::Notify, task::JoinHandle};

fn store() -> Arc<Store> {
    let dir = std::env::temp_dir().join(format!("synaroute-active-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    Arc::new(Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap())
}
fn begin(store: &Arc<Store>, id: &str) -> Guard {
    store.active_requests.begin(store.clone(), id, CategoryType::ClaudeCli, "m", true)
}
async fn bounded<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(5), future).await.expect("request lifecycle hung")
}
async fn until(mut condition: impl FnMut() -> bool) {
    bounded(async { while !condition() { tokio::time::sleep(Duration::from_millis(2)).await; } }).await;
}

#[test]
fn registry_is_scoped_and_guard_drop_unregisters() {
    let first = store(); let second = store();
    let g = begin(&first, "request-one"); let c = g.control();
    c.received(42);
    let rows = first.active_requests.list(CategoryType::ClaudeCli);
    assert_eq!(rows.len(), 1); assert_eq!(rows[0].bytes_received, 42);
    assert!(rows[0].last_read_ms.is_some()); assert_eq!(rows[0].request_id, "request-one");
    assert!(second.active_requests.list(CategoryType::ClaudeCli).is_empty());
    assert_eq!(first.active_requests.stop(CategoryType::Codex, "request-one"), StopResult::Finished);
    assert_eq!(first.active_requests.stop(CategoryType::ClaudeCli, "request-one"), StopResult::Requested);
    assert_eq!(first.active_requests.stop(CategoryType::ClaudeCli, "request-one"), StopResult::AlreadyStopping);
    assert_eq!(c.finish(End::Complete), End::Operator);
    drop(g);
    assert!(first.active_requests.list(CategoryType::ClaudeCli).is_empty());
    assert_eq!(first.list_events(CategoryType::ClaudeCli).iter().filter(|e| e.detail.contains("用户停止代理请求")).count(), 1);
    assert_eq!(first.active_requests.stop(CategoryType::ClaudeCli, "request-one"), StopResult::Finished);
}

#[test]
fn completion_wins_over_late_stop_without_logging_cancellation() {
    let s = store(); let g = begin(&s, "completed");
    assert_eq!(g.control().finish(End::Complete), End::Complete);
    assert_eq!(s.active_requests.stop(CategoryType::ClaudeCli, "completed"), StopResult::Finished);
    drop(g);
    assert!(s.list_events(CategoryType::ClaudeCli).is_empty());
}

#[test]
fn concurrent_completion_and_stop_have_one_winner() {
    let s = store();
    for i in 0..64 {
        let id = format!("race-{i}"); let g = begin(&s, &id); let c = g.control();
        let registry = s.active_requests.clone(); let barrier = Arc::new(std::sync::Barrier::new(2));
        let other = barrier.clone();
        let worker = std::thread::spawn(move || { other.wait(); registry.stop(CategoryType::ClaudeCli, &id) });
        barrier.wait(); let end = c.finish(End::Complete); let stopped = worker.join().unwrap();
        assert!(matches!((end, stopped), (End::Complete, StopResult::Finished) | (End::Operator, StopResult::Requested)));
        drop(g);
    }
    assert!(s.active_requests.0.lock().is_empty());
}

struct OwnedStream {
    permit: Option<tokio::sync::OwnedSemaphorePermit>, drops: Arc<AtomicUsize>, reads: Arc<AtomicUsize>,
}
impl Stream for OwnedStream {
    type Item = io::Result<Frame<Bytes>>;
    fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        Poll::Ready(Some(Ok(Frame::data(Bytes::from_static(b"data: live\n\n")))))
    }
}
impl Drop for OwnedStream {
    fn drop(&mut self) { self.permit.take(); self.drops.fetch_add(1, Ordering::SeqCst); }
}

#[tokio::test]
async fn stop_under_backpressure_releases_resources_without_polling_downstream() {
    let s = store(); let g = begin(&s, "backpressure");
    let drops = Arc::new(AtomicUsize::new(0)); let reads = Arc::new(AtomicUsize::new(0));
    let key = uuid::Uuid::new_v4().to_string();
    let stream = OwnedStream { permit: Some(bounded(concurrency::acquire(&key)).await), drops: drops.clone(), reads: reads.clone() };
    let upstream = Response::builder().header("content-type", "text/event-stream").body(BodyExt::boxed(StreamBody::new(stream))).unwrap();
    let downstream = relay(upstream, g, Protocol::Anthropic);
    until(|| reads.load(Ordering::SeqCst) >= 2).await;
    assert_eq!(reads.load(Ordering::SeqCst), 2, "one queued + one current frame is the read-ahead bound");
    assert_eq!(concurrency::in_flight(&key), 1);
    assert_eq!(s.active_requests.stop(CategoryType::ClaudeCli, "backpressure"), StopResult::Requested);
    until(|| drops.load(Ordering::SeqCst) == 1).await;
    assert_eq!(concurrency::in_flight(&key), 0);
    assert!(s.active_requests.list(CategoryType::ClaudeCli).is_empty());
    let text = String::from_utf8(bounded(downstream.into_body().collect()).await.unwrap().to_bytes().to_vec()).unwrap();
    assert!(text.contains("request_cancelled")); assert!(!text.contains("data: live"));
}

#[tokio::test]
async fn dropping_response_body_stops_the_pump_and_unregisters() {
    let s = store(); let g = begin(&s, "disconnect");
    let drops = Arc::new(AtomicUsize::new(0)); let reads = Arc::new(AtomicUsize::new(0));
    let stream = OwnedStream { permit: None, drops: drops.clone(), reads };
    let downstream = relay(Response::new(BodyExt::boxed(StreamBody::new(stream))), g, Protocol::Anthropic);
    drop(downstream);
    until(|| drops.load(Ordering::SeqCst) == 1 && s.active_requests.0.lock().is_empty()).await;
    assert!(s.list_events(CategoryType::ClaudeCli).is_empty(), "disconnect must not claim operator action");
}

#[tokio::test]
async fn stopping_during_route_preparation_drops_the_entire_future() {
    let s = store(); let g = begin(&s, "routing");
    let owned = Arc::new(()); let weak: Weak<()> = Arc::downgrade(&owned);
    let registry = s.active_requests.clone();
    let task = tokio::spawn(run(g, Protocol::Anthropic, async move {
        let _owned = owned;
        std::future::pending::<()>().await;
        Ok(Response::new(full_body(Bytes::new())))
    }));
    assert_eq!(registry.stop(CategoryType::ClaudeCli, "routing"), StopResult::Requested);
    let response = bounded(task).await.unwrap().unwrap();
    assert_eq!(response.status().as_u16(), 499); assert!(!response.headers().contains_key("retry-after"));
    assert!(weak.upgrade().is_none()); assert!(registry.0.lock().is_empty());
}

#[test]
fn cancellation_frames_match_each_downstream_and_never_claim_completion() {
    for protocol in [Protocol::Anthropic, Protocol::OpenaiChat, Protocol::OpenaiResponses] {
        let text = String::from_utf8(stopped_frame(protocol, true).unwrap().into_data().unwrap().to_vec()).unwrap();
        assert!(text.contains("request_cancelled")); assert!(!text.contains("response.completed")); assert!(!text.contains("message_stop"));
        if protocol == Protocol::Anthropic { assert!(text.starts_with("event: error\n")); }
        if protocol == Protocol::OpenaiResponses { assert!(text.starts_with("event: response.failed\n")); }
    }
    assert_eq!(stopped_frame(Protocol::Anthropic, false).unwrap_err().kind(), io::ErrorKind::Interrupted);
}

struct ListenerTask(JoinHandle<()>);
impl Drop for ListenerTask { fn drop(&mut self) { self.0.abort(); } }
struct ResponseStream {
    first: Option<Bytes>, rest: Option<Bytes>, release: Arc<Notify>, receiver: Option<Pin<Box<dyn Future<Output=()> + Send + Sync>>>, dropped: Arc<AtomicUsize>,
}
impl Stream for ResponseStream {
    type Item = io::Result<Frame<Bytes>>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if let Some(first) = self.first.take() { return Poll::Ready(Some(Ok(Frame::data(first)))); }
        if self.receiver.is_none() { self.receiver = Some(Box::pin(self.release.clone().notified_owned())); }
        if self.receiver.as_mut().unwrap().as_mut().poll(cx).is_pending() { return Poll::Pending; }
        Poll::Ready(self.rest.take().map(|bytes| Ok(Frame::data(bytes))))
    }
}
impl Drop for ResponseStream { fn drop(&mut self) { self.dropped.fetch_add(1, Ordering::SeqCst); } }
struct UpstreamMock {
    url: String, hits: Arc<AtomicUsize>, release: Arc<Notify>, dropped: Arc<AtomicUsize>, _listener: ListenerTask,
}
async fn mock(first: Option<&'static str>, rest: Option<&'static str>, sse: bool, headers_blocked: bool) -> UpstreamMock {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let hits = Arc::new(AtomicUsize::new(0)); let release = Arc::new(Notify::new()); let dropped = Arc::new(AtomicUsize::new(0));
    let (hit, signal, drop_count) = (hits.clone(), release.clone(), dropped.clone());
    let task = tokio::spawn(async move {
        loop {
            let Ok((socket, _)) = listener.accept().await else { break; };
            let hit = hit.clone(); let signal = signal.clone(); let drop_count = drop_count.clone();
            tokio::spawn(async move {
                let service = service_fn(move |req: Request<Incoming>| {
                    let hit = hit.clone(); let signal = signal.clone(); let drop_count = drop_count.clone();
                    async move {
                        let _ = req.into_body().collect().await;
                        hit.fetch_add(1, Ordering::SeqCst);
                        if headers_blocked { signal.notified().await; }
                        let stream = ResponseStream { first: first.map(|s| Bytes::from_static(s.as_bytes())), rest: rest.map(|s| Bytes::from_static(s.as_bytes())), release: signal,
                            receiver: None, dropped: drop_count };
                        Ok::<_, Infallible>(Response::builder().status(200).header("content-type", if sse { "text/event-stream" } else { "application/json" })
                            .body(BodyExt::boxed(StreamBody::new(stream))).unwrap())
                    }
                });
                let _ = crate::http_ingress::http1().serve_connection(TokioIo::new(socket), service).await;
            });
        }
    });
    UpstreamMock { url, hits, release, dropped, _listener: ListenerTask(task) }
}

struct Proxy { url: String, _listener: ListenerTask }
async fn proxy(store: Arc<Store>) -> Proxy {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let gate_key = uuid::Uuid::new_v4().to_string();
    let task = tokio::spawn(async move {
        loop {
            let Ok((socket, _)) = listener.accept().await else { break; };
            let store = store.clone();
            let gate_key = gate_key.clone();
            tokio::spawn(async move {
                let service = service_fn(move |req: Request<Incoming>| super::super::handle_request(store.clone(), CategoryType::ClaudeCli, gate_key.clone(), req));
                let _ = crate::http_ingress::http1().serve_connection(TokioIo::new(socket), service).await;
            });
        }
    });
    Proxy { url, _listener: ListenerTask(task) }
}
fn add_key(s: &Arc<Store>, url: &str, protocol: Protocol, priority: i32) -> String {
    let id = uuid::Uuid::new_v4().to_string();
    let key = ProviderKey { id: id.clone(), name: format!("active-{priority}"), base_url: url.into(), protocol, category_id: CategoryType::ClaudeCli,
        enabled: true, has_secret: true, priority, ..Default::default() };
    s.upsert_key(key).unwrap(); s.secrets.write().set(&id, "fixture-secret").unwrap();
    id
}
fn send(url: &str, stream: bool) -> impl Future<Output=reqwest::Result<reqwest::Response>> {
    reqwest::Client::builder().no_proxy().build().unwrap().post(format!("{url}/v1/messages"))
        .json(&json!({"model":"m","max_tokens":64,"stream":stream,"messages":[{"role":"user","content":"test"}]})).send()
}
async fn active_row(s: &Arc<Store>) -> Snapshot {
    until(|| !s.active_requests.list(CategoryType::ClaudeCli).is_empty()).await;
    s.active_requests.list(CategoryType::ClaudeCli).remove(0)
}

#[tokio::test]
async fn queued_request_can_be_stopped_without_sending_or_failing_over() {
    for streaming in [false, true] {
        let s = store(); let upstream = mock(None, None, true, true).await;
        let id = add_key(&s, &upstream.url, Protocol::Anthropic, 0);
        let other = mock(None, None, true, true).await; add_key(&s, &other.url, Protocol::Anthropic, 1);
        let mut held = Vec::new();
        for _ in 0..MAX_INFLIGHT_PER_KEY { held.push(bounded(concurrency::acquire(&id)).await); }
        let p = proxy(s.clone()).await;
        let task = tokio::spawn(send(&p.url, streaming));
        until(|| s.active_requests.list(CategoryType::ClaudeCli).first().is_some_and(|r| matches!(r.phase, Phase::Queued))).await;
        let row = active_row(&s).await;
        assert_eq!(s.active_requests.stop(CategoryType::ClaudeCli, &row.request_id), StopResult::Requested);
        let response = bounded(task).await.unwrap().unwrap();
        assert_eq!(response.status().as_u16(), 499);
        assert_eq!(response.headers()["x-synaroute-request-id"], row.request_id);
        assert_eq!(upstream.hits.load(Ordering::SeqCst), 0); assert_eq!(other.hits.load(Ordering::SeqCst), 0);
        assert_eq!(s.get_key(&id).unwrap().health.fail_count, 0);
        drop(held); assert_eq!(concurrency::in_flight(&id), 0);
        assert!(s.active_requests.0.lock().is_empty());
    }
}

#[tokio::test]
async fn waiting_for_headers_can_be_stopped_without_penalizing_the_key() {
    for streaming in [false, true] {
        let s = store(); let upstream = mock(None, None, true, true).await;
        let id = add_key(&s, &upstream.url, Protocol::Anthropic, 0);
        let other = mock(None, None, true, true).await; add_key(&s, &other.url, Protocol::Anthropic, 1);
        let p = proxy(s.clone()).await; let task = tokio::spawn(send(&p.url, streaming));
        until(|| upstream.hits.load(Ordering::SeqCst) == 1).await;
        let row = active_row(&s).await;
        assert!(matches!(row.phase, Phase::Waiting));
        s.active_requests.stop(CategoryType::ClaudeCli, &row.request_id);
        assert_eq!(bounded(task).await.unwrap().unwrap().status().as_u16(), 499);
        assert_eq!(concurrency::in_flight(&id), 0); assert_eq!(other.hits.load(Ordering::SeqCst), 0);
        let h = s.get_key(&id).unwrap().health;
        assert_eq!(h.fail_count, 0); assert_eq!(h.last_live_success, None);
        assert!(!crate::health::quota_window::active(&id));
        upstream.release.notify_one();
    }
}

#[tokio::test]
async fn buffered_partial_body_can_be_stopped_and_does_not_fail_over() {
    let s = store(); let upstream = mock(Some("{\"partial\":"), None, false, false).await;
    let id = add_key(&s, &upstream.url, Protocol::Anthropic, 0);
    let other = mock(None, None, true, true).await; add_key(&s, &other.url, Protocol::Anthropic, 1);
    let p = proxy(s.clone()).await; let task = tokio::spawn(send(&p.url, false));
    until(|| s.active_requests.list(CategoryType::ClaudeCli).first().is_some_and(|r| r.bytes_received > 0)).await;
    let row = active_row(&s).await; assert!(matches!(row.phase, Phase::Receiving));
    s.active_requests.stop(CategoryType::ClaudeCli, &row.request_id);
    assert_eq!(bounded(task).await.unwrap().unwrap().status().as_u16(), 499);
    until(|| upstream.dropped.load(Ordering::SeqCst) == 1).await;
    assert_eq!(other.hits.load(Ordering::SeqCst), 0); assert_eq!(concurrency::in_flight(&id), 0);
}

const ANTHROPIC_START: &str = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_test\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"m\",\"content\":[],\"usage\":{\"input_tokens\":7,\"output_tokens\":0}}}\n\n";
const CHAT_START: &str = "data: {\"id\":\"chat_test\",\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"hello\"}}],\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":0}}\n\n";

#[tokio::test]
async fn live_stream_stop_preserves_usage_and_never_touches_health() {
    for (protocol, start) in [(Protocol::Anthropic, ANTHROPIC_START), (Protocol::OpenaiChat, CHAT_START)] {
        let s = store(); let upstream = mock(Some(start), None, true, false).await;
        let id = add_key(&s, &upstream.url, protocol, 0);
        // Preexisting failures must neither increase nor decay on an operator stop.
        crate::health::record_live_failure(&s, &id);
        let h_before = s.get_key(&id).unwrap().health;
        let p = proxy(s.clone()).await; let response = bounded(send(&p.url, true)).await.unwrap();
        let request_id = response.headers()["x-synaroute-request-id"].to_str().unwrap().to_owned();
        let mut chunks = response.bytes_stream();
        assert!(!bounded(chunks.next()).await.unwrap().unwrap().is_empty());
        assert_eq!(concurrency::in_flight(&id), 1);
        let row = active_row(&s).await;
        assert_eq!(row.request_id, request_id); assert_eq!(row.bytes_received, start.len() as u64);
        s.active_requests.stop(CategoryType::ClaudeCli, &request_id);
        let mut text = String::new();
        while let Some(chunk) = bounded(chunks.next()).await { text.push_str(&String::from_utf8_lossy(&chunk.unwrap())); }
        assert!(text.contains("request_cancelled")); assert!(!text.contains("message_stop"));
        until(|| s.active_requests.0.lock().is_empty() && concurrency::in_flight(&id) == 0).await;
        until(|| upstream.dropped.load(Ordering::SeqCst) == 1).await;
        let h_after = s.get_key(&id).unwrap().health;
        assert_eq!(h_after.fail_count, h_before.fail_count); assert_eq!(h_after.last_live_success, h_before.last_live_success);
        assert!(!crate::health::quota_window::active(&id));
        let usage: u64 = s.list_events(CategoryType::ClaudeCli).iter().filter_map(|e| e.usage).map(|u| u.input).sum();
        assert_eq!(usage, 7, "received usage must be settled exactly once on cancellation");
    }
}

#[tokio::test]
async fn normal_stream_completion_still_settles_success_and_usage() {
    let s = store(); let upstream = mock(Some(ANTHROPIC_START), Some("event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"), true, false).await;
    let id = add_key(&s, &upstream.url, Protocol::Anthropic, 0);
    let p = proxy(s.clone()).await; let response = bounded(send(&p.url, true)).await.unwrap();
    let request_id = response.headers()["x-synaroute-request-id"].to_str().unwrap().to_owned();
    upstream.release.notify_one();
    let text = bounded(response.text()).await.unwrap(); assert!(text.contains("message_stop"));
    until(|| s.active_requests.0.lock().is_empty()).await;
    assert_eq!(s.active_requests.stop(CategoryType::ClaudeCli, &request_id), StopResult::Finished);
    assert!(s.get_key(&id).unwrap().health.last_live_success.is_some());
}

/// Real HTTP through the public proxy handler, not merely serialization of error frames.
#[tokio::test]
async fn all_downstream_protocols_receive_cancellation_over_http() {
    for path in ["/v1/messages", "/v1/chat/completions", "/v1/responses"] {
        let s = store(); let upstream = mock(Some(ANTHROPIC_START), None, true, false).await;
        let id = add_key(&s, &upstream.url, Protocol::Anthropic, 0);
        let p = proxy(s.clone()).await;
        let payload = if path.ends_with("responses") {
            json!({"model":"m","stream":true,"max_output_tokens":64,"input":"test"})
        } else { json!({"model":"m","stream":true,"max_tokens":64,"messages":[{"role":"user","content":"test"}]}) };
        let response = bounded(reqwest::Client::builder().no_proxy().build().unwrap()
            .post(format!("{}{path}", p.url)).json(&payload).send()).await.unwrap();
        assert_eq!(response.status(), 200);
        let request_id = response.headers()["x-synaroute-request-id"].to_str().unwrap().to_owned();
        // Responses translation may buffer message_start without producing output. Observe raw inlet instead.
        until(|| s.active_requests.list(CategoryType::ClaudeCli).first().is_some_and(|r| r.bytes_received > 0)).await;
        assert_eq!(s.active_requests.stop(CategoryType::ClaudeCli, &request_id), StopResult::Requested);
        let text = bounded(response.text()).await.unwrap();
        let payloads: Vec<serde_json::Value> = text.lines().filter_map(|line| line.strip_prefix("data:"))
            .filter_map(|data| serde_json::from_str(data.trim()).ok()).collect();
        let error = payloads.iter().find(|v| v.to_string().contains("request_cancelled")).expect("cancellation must cross HTTP");
        if path.ends_with("responses") { assert_eq!(error["type"], "response.failed"); assert_eq!(error["response"]["status"], "failed"); }
        else { assert_eq!(error["error"]["type"], "request_cancelled"); }
        assert!(!text.contains("response.completed")); assert!(!text.contains("message_stop"));
        until(|| concurrency::in_flight(&id) == 0 && s.active_requests.0.lock().is_empty()).await;
    }
}

/// Construct a real lease and a real binding; exercise the production streaming Drop path.
/// The error event deliberately reaches the raw tail, so removing the operator guard would
/// call routing::policy::stream_end(true) and erase this binding.
#[tokio::test]
async fn operator_stop_preserves_existing_binding_even_after_an_error_event() {
    let s = store();
    let upstream = mock(Some("event: error\ndata: {\"type\":\"error\",\"error\":{\"message\":\"fixture\"}}\n\n"), None, true, false).await;
    let id = add_key(&s, &upstream.url, Protocol::Anthropic, 0);
    let key = s.get_key(&id).unwrap();
    let lease = s.routing_runtime.begin(CategoryType::ClaudeCli, "active-binding", "turn", 0, "fixture",
        crate::store::routing::config::AffinityMode::Session).unwrap();
    s.routing_runtime.choose(&lease, "m"); s.routing_runtime.served(&lease, &id, "m");
    let before = s.routing_runtime.state(&lease).unwrap().binding.unwrap();
    let guard = begin(&s, "binding-request"); let control = guard.control();
    let response = bounded(reqwest::Client::builder().no_proxy().build().unwrap().post(&upstream.url).send()).await.unwrap();
    let response = super::super::streaming::response(response, bounded(concurrency::acquire(&id)).await,
        &s, CategoryType::ClaudeCli, &key, "m", "m", Protocol::Anthropic, None, Some(lease.clone()), control).unwrap();
    let mut downstream = relay(response, guard, Protocol::Anthropic).into_body();
    assert!(bounded(downstream.frame()).await.unwrap().unwrap().is_data());
    s.active_requests.stop(CategoryType::ClaudeCli, "binding-request");
    let _ = bounded(downstream.collect()).await.unwrap();
    until(|| s.active_requests.0.lock().is_empty()).await;
    let after = s.routing_runtime.state(&lease).unwrap().binding.expect("operator stop must not erase binding");
    assert_eq!((after.key_id, after.real_model, after.ticket), (before.key_id, before.real_model, before.ticket));
    assert!(!lease.failed.load(Ordering::Acquire));
    assert_eq!(s.get_key(&id).unwrap().health.fail_count, 0);
}

#[tokio::test]
async fn completed_translation_releases_permit_before_final_frame_is_consumed() {
    let s = store(); let upstream = mock(Some(ANTHROPIC_START), None, true, false).await;
    let id = add_key(&s, &upstream.url, Protocol::Anthropic, 0);
    let key = s.get_key(&id).unwrap();
    let guard = begin(&s, "final-frame");
    let response = bounded(reqwest::Client::builder().no_proxy().build().unwrap().post(&upstream.url).send()).await.unwrap();
    let translator = crate::upstream::SseTranslator::new(crate::upstream::sse_direction(Protocol::OpenaiChat, Protocol::Anthropic).unwrap());
    let mut body = super::super::streaming::response(response, bounded(concurrency::acquire(&id)).await,
        &s, CategoryType::ClaudeCli, &key, "m", "m", Protocol::OpenaiChat, Some(translator), None, guard.control()).unwrap().into_body();
    upstream.release.notify_one();
    // Consume until the generated terminal frame; retain the body without polling its EOF.
    loop {
        let frame = bounded(body.frame()).await.expect("terminal frame").unwrap();
        if frame.data_ref().is_some_and(|b| String::from_utf8_lossy(b).contains("[DONE]")) { break; }
    }
    assert_eq!(concurrency::in_flight(&id), 0, "EOF must release permit before consumer polls again");
    drop(body); drop(guard);
}

#[tokio::test]
async fn stop_after_failed_candidate_prevents_next_upstream_send() {
    for streaming in [false, true] {
        let s = store();
        // One complete failure, followed by a second candidate held in its concurrency queue.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let first_hits = Arc::new(AtomicUsize::new(0)); let hit = first_hits.clone();
        let first = ListenerTask(tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let service = service_fn(move |req: Request<Incoming>| { let hit = hit.clone(); async move {
                let _ = req.into_body().collect().await; hit.fetch_add(1, Ordering::SeqCst);
                Ok::<_, Infallible>(Response::builder().status(500).body(full_body(Bytes::from_static(b"fixture failure"))).unwrap())
            }});
            let _ = crate::http_ingress::http1().serve_connection(TokioIo::new(socket), service).await;
        }));
        add_key(&s, &url, Protocol::Anthropic, 0);
        let second = mock(None, None, true, true).await;
        let id = add_key(&s, &second.url, Protocol::Anthropic, 1);
        let mut permits = Vec::new();
        for _ in 0..MAX_INFLIGHT_PER_KEY { permits.push(bounded(concurrency::acquire(&id)).await); }
        let p = proxy(s.clone()).await; let request = tokio::spawn(send(&p.url, streaming));
        until(|| s.active_requests.list(CategoryType::ClaudeCli).first().is_some_and(|r| r.attempts == 2 && matches!(r.phase, Phase::Queued))).await;
        let row = active_row(&s).await;
        assert_eq!(first_hits.load(Ordering::SeqCst), 1);
        s.active_requests.stop(CategoryType::ClaudeCli, &row.request_id);
        assert_eq!(bounded(request).await.unwrap().unwrap().status().as_u16(), 499);
        drop(permits);
        assert_eq!(second.hits.load(Ordering::SeqCst), 0);
        assert!(s.active_requests.0.lock().is_empty());
        drop(first);
    }
}
