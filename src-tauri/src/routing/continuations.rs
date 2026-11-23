use super::runtime::{Binding, Lease, Runtime, SESSION_TTL};
use crate::model::CategoryType;
use std::{sync::atomic::Ordering, time::Instant};

const MAX_RESPONSES: usize = 4096;

#[cfg(test)]
#[path = "continuation_http_tests.rs"]
mod tests;

#[derive(Clone)]
pub(super) struct ObservedResponse {
    pub position: usize,
    pub model: String,
    pub binding: Binding,
    fingerprint: String,
    observed: Instant,
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 256 && id.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"_-.".contains(&byte))
}

impl Runtime {
    pub fn observe_response(&self, lease: &Lease, response_id: &str, key_id: &str, real_model: &str) {
        if !valid_id(response_id) || lease.failed.load(Ordering::Acquire) || !self.current_upstream(lease) { return; }
        let sessions = self.sessions.lock();
        let Some(state) = sessions.get(&(lease.category, lease.session.clone()))
            .filter(|state| state.generation == lease.generation && state.turn == lease.turn && state.touched.elapsed() < SESSION_TTL) else { return; };
        let Some(model) = state.model.clone() else { return; };
        let mut responses = self.responses.lock();
        if lease.failed.load(Ordering::Acquire) { return; }
        responses.retain(|_, response| response.observed.elapsed() < SESSION_TTL);
        let key = (lease.category, lease.session.clone(), response_id.to_owned());
        if !responses.contains_key(&key) && responses.len() >= MAX_RESPONSES {
            if let Some(oldest) = responses.iter().min_by_key(|(_, response)| response.observed).map(|(key, _)| key.clone()) { responses.remove(&oldest); }
        }
        responses.entry(key).or_insert_with(|| ObservedResponse {
            position: state.position, model, fingerprint: state.fingerprint.clone(), observed: Instant::now(),
            binding: Binding { key_id: key_id.into(), real_model: real_model.into(), ticket: lease.ticket },
        });
    }

    pub(super) fn response_for(&self, category: CategoryType, session: &str, response_id: &str, fingerprint: &str) -> Option<ObservedResponse> {
        if !valid_id(response_id) { return None; }
        let mut responses = self.responses.lock();
        responses.retain(|_, response| response.observed.elapsed() < SESSION_TTL);
        responses.get(&(category, session.into(), response_id.into())).filter(|response| response.fingerprint == fingerprint).cloned()
    }
}
