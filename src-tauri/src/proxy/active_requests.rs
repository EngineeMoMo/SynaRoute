//! Per-Store, memory-only proxy requests. No bodies, secrets, URLs or client identity.
//! One guard owns registration. Handler cancellation drops its whole forwarding future;
//! streamed responses move that guard to a bounded pump, independent of downstream polls.
use super::{full_body, ResBody};
use crate::{model::{CategoryType, Protocol, ProviderKey}, store::Store};
use bytes::{Bytes, BytesMut};
use http_body_util::BodyExt;
use hyper::{body::{Body, Frame, SizeHint}, Response, StatusCode};
use parking_lot::Mutex;
use serde::Serialize;
use std::{collections::HashMap, pin::Pin, sync::Arc, task::{Context, Poll}, time::Instant};
use tokio::sync::{mpsc, watch};

const STOP_MESSAGE: &str = "已由用户停止本次代理请求；不会继续尝试其他 Key。客户端可能自行发起新请求，上游也可能仍计费。";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum End { Complete, UpstreamError, Operator, Disconnected }
#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) enum Phase { Routing, Queued, Waiting, Receiving, Stopping }

struct Progress {
    phase: Phase, model: String, real_model: Option<String>, key_id: Option<String>,
    key_name: Option<String>, attempts: u32, attempt_start: Option<Instant>,
    bytes: u64, last_read: Option<Instant>, end: Option<End>,
}
struct Entry {
    id: String, category: CategoryType, streaming: bool, start: Instant,
    progress: Mutex<Progress>, cancel: watch::Sender<bool>,
}
#[derive(Clone)]
pub(super) struct Control(Arc<Entry>);

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Snapshot {
    request_id: String, category_id: CategoryType, streaming: bool, phase: Phase,
    requested_model: String, real_model: Option<String>, key_id: Option<String>, key_name: Option<String>,
    attempts: u32, elapsed_ms: u64, attempt_ms: Option<u64>, bytes_received: u64,
    last_read_ms: Option<u64>,
}
#[derive(Default)]
pub(crate) struct Registry(Mutex<HashMap<String, Arc<Entry>>>);
#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum StopResult { Requested, AlreadyStopping, Finished }

impl Registry {
    pub(super) fn begin(self: &Arc<Self>, store: Arc<Store>, id: &str, category: CategoryType, model: &str, streaming: bool) -> Guard {
        let (cancel, _) = watch::channel(false);
        let entry = Arc::new(Entry {
            id: id.into(), category, streaming, start: Instant::now(), cancel,
            progress: Mutex::new(Progress { phase: Phase::Routing, model: model.into(), real_model: None,
                key_id: None, key_name: None, attempts: 0, attempt_start: None, bytes: 0, last_read: None, end: None }),
        });
        self.0.lock().insert(id.into(), entry.clone());
        Guard { registry: self.clone(), control: Control(entry), store }
    }
    pub(crate) fn list(&self, category: CategoryType) -> Vec<Snapshot> {
        let entries: Vec<_> = self.0.lock().values().filter(|e| e.category == category).cloned().collect();
        let mut rows = Vec::with_capacity(entries.len());
        for e in entries {
            let p = e.progress.lock();
            if p.end.is_some() && p.end != Some(End::Operator) { continue; }
            rows.push(Snapshot { request_id: e.id.clone(), category_id: e.category, streaming: e.streaming,
                phase: p.phase, requested_model: p.model.clone(), real_model: p.real_model.clone(),
                key_id: p.key_id.clone(), key_name: p.key_name.clone(), attempts: p.attempts,
                elapsed_ms: millis(e.start), attempt_ms: p.attempt_start.map(millis),
                bytes_received: p.bytes, last_read_ms: p.last_read.map(millis) });
        }
        rows.sort_by(|a,b| b.elapsed_ms.cmp(&a.elapsed_ms).then_with(|| a.request_id.cmp(&b.request_id)));
        rows
    }
    pub(crate) fn stop(&self, category: CategoryType, id: &str) -> StopResult {
        let entry = self.0.lock().get(id).filter(|e| e.category == category).cloned();
        let Some(entry) = entry else { return StopResult::Finished; };
        let c = Control(entry);
        let mut p = c.0.progress.lock();
        match p.end {
            Some(End::Operator) => StopResult::AlreadyStopping,
            Some(_) => StopResult::Finished,
            None => {
                p.end = Some(End::Operator); p.phase = Phase::Stopping;
                c.0.cancel.send_replace(true);
                StopResult::Requested
            }
        }
    }
}
fn millis(start: Instant) -> u64 { start.elapsed().as_millis().min(u64::MAX as u128) as u64 }

impl Control {
    pub(super) fn phase(&self, phase: Phase) {
        let mut p = self.0.progress.lock();
        if p.end.is_none() { p.phase = phase; }
    }
    pub(super) fn model(&self, model: &str) { self.0.progress.lock().model = model.into(); }
    pub(super) fn attempt(&self, key: &ProviderKey, model: &str, number: u32) {
        let mut p = self.0.progress.lock();
        p.key_id = Some(key.id.clone()); p.key_name = Some(key.name.clone());
        p.real_model = Some(key.resolve_model(model)); p.attempts = number; p.attempt_start = Some(Instant::now());
    }
    pub(super) fn received(&self, n: usize) {
        if n == 0 { return; }
        let mut p = self.0.progress.lock();
        p.bytes = p.bytes.saturating_add(n as u64); p.last_read = Some(Instant::now());
        if p.end.is_none() { p.phase = Phase::Receiving; }
    }
    /// Linearizes natural completion against stop. A losing completion must not update health.
    pub(super) fn finish(&self, end: End) -> End {
        let mut p = self.0.progress.lock();
        let end = *p.end.get_or_insert(end);
        if matches!(end, End::Operator | End::Disconnected) { self.0.cancel.send_replace(true); }
        end
    }
    pub(super) fn end(&self) -> Option<End> { self.0.progress.lock().end }
    pub(super) async fn cancelled(&self) {
        let mut rx = self.0.cancel.subscribe();
        if *rx.borrow_and_update() { return; }
        let _ = rx.changed().await;
    }
    pub(super) async fn read_body(&self, response: reqwest::Response) -> Result<Bytes, reqwest::Error> {
        use futures_util::StreamExt;
        let mut stream = response.bytes_stream();
        let mut bytes = BytesMut::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            self.received(chunk.len()); bytes.extend_from_slice(&chunk);
        }
        Ok(bytes.freeze())
    }
}
pub(super) struct Guard { registry: Arc<Registry>, control: Control, store: Arc<Store> }
impl Guard { pub(super) fn control(&self) -> Control { self.control.clone() } }
impl Drop for Guard {
    fn drop(&mut self) {
        let end = self.control.finish(End::Disconnected);
        self.registry.0.lock().remove(&self.control.0.id);
        if end == End::Operator {
            let e = &self.control.0;
            let p = e.progress.lock();
            self.store.append_event(e.category, "warning", p.key_id.as_deref(), &format!(
                "用户停止代理请求 {} · 已接收 {} 上游字节 · {}ms。{STOP_MESSAGE}", e.id, p.bytes, millis(e.start)));
        }
    }
}

pub(super) fn stopped_response() -> Response<ResBody> {
    // 499 is a cancellation, not an upstream overload; no Retry-After or failover gate.
    let json = serde_json::json!({"type":"error","error":{"type":"request_cancelled","message":STOP_MESSAGE}});
    Response::builder().status(StatusCode::from_u16(499).expect("valid status"))
        .header("content-type", "application/json").body(full_body(Bytes::from(json.to_string()))).expect("constant headers")
}
fn stopped_frame(protocol: Protocol, sse: bool) -> std::io::Result<Frame<Bytes>> {
    use serde_json::json;
    if !sse { return Err(std::io::Error::new(std::io::ErrorKind::Interrupted, STOP_MESSAGE)); }
    let error = json!({"type":"request_cancelled", "code":"request_cancelled", "message":STOP_MESSAGE});
    let (event, payload) = match protocol {
        Protocol::Anthropic => ("event: error\n", json!({"type":"error","error":error})),
        Protocol::OpenaiChat => ("", json!({"error":error})),
        Protocol::OpenaiResponses => ("event: response.failed\n", json!({"type":"response.failed","response":{"object":"response","status":"failed","error":error}})),
    };
    Ok(Frame::data(Bytes::from(format!("{event}data: {payload}\n\n"))))
}

/// Capacity one plus the current frame: no unbounded read-ahead. The pump selects cancellation
/// both while reading upstream and while blocked sending downstream, then drops upstream first.
pub(super) fn relay(response: Response<ResBody>, guard: Guard, protocol: Protocol) -> Response<ResBody> {
    let control = guard.control();
    let (parts, mut upstream) = response.into_parts();
    let sse = parts.headers.get("content-type").and_then(|h| h.to_str().ok()).is_some_and(|h| h.starts_with("text/event-stream"))
        && !parts.headers.contains_key("content-encoding");
    let (tx, rx) = mpsc::channel(1);
    let c = control.clone();
    tokio::spawn(async move {
        loop {
            let frame = tokio::select! { biased;
                _ = c.cancelled() => break,
                _ = tx.closed() => { c.finish(End::Disconnected); break; },
                frame = upstream.frame() => frame,
            };
            let Some(frame) = frame else { c.finish(End::Complete); break; };
            let error = frame.is_err();
            if error { c.finish(End::UpstreamError); }
            tokio::select! { biased;
                _ = c.cancelled() => break,
                sent = tx.send(frame) => if sent.is_err() { c.finish(End::Disconnected); break; },
            }
            if error { break; }
        }
        drop(upstream); // settles usage/health and releases the permit even with a stalled client
        drop(guard);
    });
    Response::from_parts(parts, PumpBody { rx, control, protocol, sse, finished: false }.boxed())
}
struct PumpBody {
    rx: mpsc::Receiver<std::io::Result<Frame<Bytes>>>, control: Control, protocol: Protocol, sse: bool, finished: bool,
}
impl Body for PumpBody {
    type Data = Bytes;
    type Error = std::io::Error;
    fn poll_frame(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        if self.finished { return Poll::Ready(None); }
        if self.control.end() == Some(End::Operator) {
            self.finished = true; self.rx.close();
            return Poll::Ready(Some(stopped_frame(self.protocol, self.sse)));
        }
        let poll = self.rx.poll_recv(cx);
        // Cancellation can close the channel between the initial check and poll_recv.
        if poll.is_ready() && self.control.end() == Some(End::Operator) {
            self.finished = true; self.rx.close();
            return Poll::Ready(Some(stopped_frame(self.protocol, self.sse)));
        }
        if matches!(poll, Poll::Ready(None)) { self.finished = true; }
        poll
    }
    fn is_end_stream(&self) -> bool { self.finished }
    fn size_hint(&self) -> SizeHint { SizeHint::default() }
}
impl Drop for PumpBody { fn drop(&mut self) { self.control.finish(End::Disconnected); } }

#[derive(Clone)]
pub(super) struct Streaming;

pub(super) async fn run<F>(guard: Guard, protocol: Protocol, future: F) -> Result<Response<ResBody>, hyper::Error>
where F: std::future::Future<Output = Result<Response<ResBody>, hyper::Error>> {
    let control = guard.control();
    let result = {
        let future = future;
        tokio::pin!(future);
        tokio::select! { biased;
            _ = control.cancelled() => Ok(stopped_response()),
            result = &mut future => result,
        }
    }; // drop the entire forwarding future (including queue/send/body waits) before unregistering
    match result {
        Ok(mut response) => {
            if response.extensions_mut().remove::<Streaming>().is_some() {
                Ok(relay(response, guard, protocol))
            } else if control.finish(End::Complete) == End::Operator {
                Ok(stopped_response())
            } else { Ok(response) }
        }
        Err(error) => { control.finish(End::Disconnected); Err(error) }
    }
}

#[tauri::command]
pub(crate) fn list_active_requests(state: tauri::State<crate::AppState>, category_id: CategoryType) -> Vec<Snapshot> {
    state.store.active_requests.list(category_id)
}
#[tauri::command]
pub(crate) fn stop_active_request(state: tauri::State<crate::AppState>, category_id: CategoryType, request_id: String) -> StopResult {
    state.store.active_requests.stop(category_id, &request_id)
}

#[cfg(test)]
#[path = "active_requests_tests.rs"]
mod tests;
