//! Shared streaming lifecycle: raw upstream counters, bounded usage windows, translation,
//! and one Drop settlement. Operator/disconnect endings never impersonate upstream health.
use super::{active_requests::{Control, End, Phase, Streaming}, codex_images, ResBody};
use crate::{model::{CategoryType, Protocol, ProviderKey}, store::Store, upstream::SseTranslator};
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use http_body_util::{BodyExt, StreamBody};
use hyper::{body::Frame, Response, StatusCode};
use std::{pin::Pin, sync::Arc};

type Upstream = Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send + Sync>>;
const WINDOW: usize = 8192;

struct State {
    upstream: Upstream, translator: Option<SseTranslator>, finished: bool,
    head: Vec<u8>, tail: Vec<u8>, errored: bool, eof: bool,
    store: Arc<Store>, category: CategoryType, key_id: String, requested_model: String, real_model: String,
    lease: Option<crate::store::routing::runtime::Lease>, active: Control,
}
impl State {
    fn observe(&mut self, bytes: &[u8]) {
        let n = (WINDOW - self.head.len()).min(bytes.len());
        self.head.extend_from_slice(&bytes[..n]);
        if bytes.len() >= WINDOW {
            self.tail.clear(); self.tail.extend_from_slice(&bytes[bytes.len()-WINDOW..]);
        } else {
            let remove = self.tail.len().saturating_add(bytes.len()).saturating_sub(WINDOW);
            self.tail.drain(..remove); self.tail.extend_from_slice(bytes);
        }
    }
    fn saw_error(&self) -> bool {
        self.errored || crate::upstream::sse_stream_errored(&String::from_utf8_lossy(&self.tail))
    }
}
impl Drop for State {
    fn drop(&mut self) {
        let errored = self.saw_error();
        if !matches!(self.active.end(), Some(End::Operator | End::Disconnected)) && (self.eof || self.errored) {
            crate::health::record_stream_end(&self.store, self.category, &self.key_id, &self.requested_model, &self.real_model, errored);
            crate::store::routing::policy::stream_end(&self.store, &self.lease, errored);
        }
        let usage = if let Some(tr) = &self.translator { tr.accumulated_usage() } else {
            let mut raw = String::from_utf8_lossy(&self.head).into_owned();
            raw.push('\n'); raw.push_str(&String::from_utf8_lossy(&self.tail));
            crate::upstream::extract_usage_from_sse(&raw)
        };
        if let Some(u) = usage {
            let collapse = format!("ok:{}:{}:true", self.key_id, self.requested_model);
            self.store.backfill_usage_for_collapsed_event(self.category, Some(&self.key_id), &collapse, u, &self.real_model);
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn response(
    resp: reqwest::Response, permit: tokio::sync::OwnedSemaphorePermit, store: &Arc<Store>,
    category: CategoryType, key: &ProviderKey, requested_model: &str, real_model: &str,
    downstream: Protocol, translator: Option<SseTranslator>,
    lease: Option<crate::store::routing::runtime::Lease>, active: Control,
) -> crate::error::AppResult<Response<ResBody>> {
    let translated = translator.is_some();
    let content_type = resp.headers().get("content-type").and_then(|h| h.to_str().ok()).unwrap_or("text/event-stream").to_owned();
    let encoding = resp.headers().get("content-encoding").and_then(|h| h.to_str().ok())
        .map(str::trim).filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("identity")).map(str::to_owned);
    active.phase(Phase::Waiting);
    // One shared inlet for both streaming exits; permit stays with this stream, not the handler.
    let counter = active.clone();
    let upstream = crate::upstream::guard_stream_idle(resp.bytes_stream().inspect(move |chunk| {
        if let Ok(bytes) = chunk { counter.received(bytes.len()); }
    }), permit);
    let state = State { upstream, translator, finished: false, head: Vec::new(), tail: Vec::new(),
        errored: false, eof: false, store: store.clone(), category, key_id: key.id.clone(),
        requested_model: requested_model.into(), real_model: real_model.into(), lease, active };
    let stream = futures_util::stream::unfold(state, |mut st| async move {
        if st.finished { return None; }
        loop {
            match st.upstream.next().await {
                Some(Ok(bytes)) => {
                    st.observe(&bytes);
                    let output = match &mut st.translator {
                        Some(tr) => Bytes::from(tr.push(&bytes)), None => bytes,
                    };
                    if !output.is_empty() { return Some((Ok(output), st)); }
                }
                Some(Err(e)) => {
                    st.errored = true; st.finished = true; st.active.finish(End::UpstreamError);
                    return Some((Err(std::io::Error::other(crate::diagnostics::http_error_message(e))), st));
                }
                None => {
                    st.eof = true; st.finished = true;
                    // Release the network stream and its permit before yielding a final frame.
                    // That frame may remain blocked by downstream backpressure indefinitely.
                    st.upstream = Box::pin(futures_util::stream::empty());
                    let error = st.saw_error();
                    st.active.finish(if error { End::UpstreamError } else { End::Complete });
                    if error { return None; } // never synthesize completed after an upstream error
                    let tail = st.translator.as_mut().map(|tr| tr.finish()).unwrap_or_default();
                    if tail.is_empty() { return None; }
                    return Some((Ok(Bytes::from(tail)), st));
                }
            }
        }
    });
    let stream: Pin<Box<dyn Stream<Item = std::io::Result<Bytes>> + Send + Sync>> = if translated {
        Box::pin(stream)
    } else {
        Box::pin(codex_images::stream(stream, category, downstream, &content_type, encoding.as_deref(), store))
    };
    let body = BodyExt::boxed(StreamBody::new(stream.map(|chunk| chunk.map(Frame::data))));
    let mut builder = Response::builder().status(StatusCode::OK).header("cache-control", "no-cache")
        .header("content-type", if translated { "text/event-stream" } else { &content_type });
    if !translated { if let Some(enc) = encoding { builder = builder.header("content-encoding", enc); } }
    let mut response = builder.body(body).map_err(|e| crate::error::AppError::upstream_msg(e.to_string()))?;
    response.extensions_mut().insert(Streaming);
    Ok(response)
}
