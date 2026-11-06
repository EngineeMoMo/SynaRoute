use super::config::{AffinityMode, RouteGroup, SceneRoute};
use crate::model::CategoryType;
use parking_lot::{Mutex, ReentrantMutex};
use serde::Serialize;
use std::{collections::{BTreeMap, HashMap}, sync::{Arc, atomic::{AtomicBool, AtomicU64, Ordering}}, time::{Duration, Instant}};

pub const SESSION_TTL: Duration = Duration::from_secs(24 * 3600);
const MAX_SESSIONS: usize = 1024;
#[derive(Clone, Debug)]
pub struct Binding { pub key_id: String, pub real_model: String, pub ticket: u64 }
#[derive(Clone, Debug)]
pub struct TurnState {
    pub turn: String,
    pub position: usize,
    pub generation: u64,
    pub model: Option<String>,
    pub previous_model: Option<String>,
    pub binding: Option<Binding>,
    pub fingerprint: String,
    pub touched: Instant,
}
#[derive(Clone, Debug)]
pub struct Lease {
    pub session: String,
    pub turn: String,
    pub generation: u64,
    pub category: CategoryType,
    pub ticket: u64,
    pub failed: Arc<AtomicBool>,
}
#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Activity {
    pub category_id: Option<CategoryType>,
    pub observed_at: i64,
    pub identity: String,
    pub selection: String,
    pub selected_model: Option<String>,
    pub kept_key: Option<String>,
    pub privacy: String,
    pub findings: BTreeMap<String, usize>,
    pub blocked: u64,
    pub checked: u64,
}
#[derive(Clone)]
pub struct CachedDecision { pub rule: Option<String>, pub expires: Instant }

#[derive(Default)]
pub struct Runtime {
    pub edit: ReentrantMutex<()>,
    pub sessions: Mutex<HashMap<(CategoryType, String), TurnState>>,
    pub activities: Mutex<BTreeMap<CategoryType, Activity>>,
    pub decisions: Mutex<HashMap<String, CachedDecision>>,
    pub cooldowns: Mutex<HashMap<String, Instant>>,
    pub judges: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    pub previews: Mutex<HashMap<String, PendingScene>>,
    sequence: AtomicU64,
}

pub struct PendingScene {
    pub scene_id: String,
    pub fingerprint: String,
    pub routes: Vec<SceneRoute>,
    pub files: Vec<crate::tools::scene_config::PreparedCategory>,
    pub expires: Instant,
}

impl Runtime {
    pub fn next(&self) -> u64 { self.sequence.fetch_add(1, Ordering::Relaxed) + 1 }
    pub fn reset(&self, category: CategoryType) {
        self.sessions.lock().retain(|(cat, _), _| *cat != category);
        self.activities.lock().remove(&category);
    }
    pub fn begin(&self, category: CategoryType, session: &str, turn: &str, position: usize, fingerprint: &str, mode: AffinityMode) -> Option<Lease> {
        let mut sessions = self.sessions.lock();
        sessions.retain(|_, state| state.touched.elapsed() < SESSION_TTL);
        let key = (category, session.to_owned());
        if !sessions.contains_key(&key) && sessions.len() >= MAX_SESSIONS { return None; }
        let state = sessions.entry(key).or_insert_with(|| TurnState {
            turn: turn.into(), position, generation: self.next(), model: None, previous_model: None, binding: None, fingerprint: fingerprint.into(), touched: Instant::now(),
        });
        if state.fingerprint != fingerprint {
            *state = TurnState { turn: turn.into(), position, generation: self.next(), model: None, previous_model: None, binding: None, fingerprint: fingerprint.into(), touched: Instant::now() };
        }
        if position < state.position || position == state.position && state.turn != turn { return None; }
        if state.turn != turn {
            state.turn = turn.into();
            state.position = position;
            state.generation = self.next();
            state.model = None;
            if mode != AffinityMode::Session { state.binding = None; }
        }
        state.touched = Instant::now();
        Some(Lease { session: session.into(), turn: turn.into(), generation: state.generation, category, ticket: self.next(), failed: Arc::new(AtomicBool::new(false)) })
    }
    pub fn state(&self, lease: &Lease) -> Option<TurnState> {
        self.sessions.lock().get(&(lease.category, lease.session.clone()))
            .filter(|s| s.generation == lease.generation && s.turn == lease.turn).cloned()
    }
    pub fn choose(&self, lease: &Lease, model: &str) -> Option<String> {
        let mut sessions = self.sessions.lock();
        let state = sessions.get_mut(&(lease.category, lease.session.clone()))?;
        if state.generation != lease.generation || state.turn != lease.turn { return None; }
        Some(state.model.get_or_insert_with(|| model.into()).clone())
    }
    pub fn served(&self, lease: &Lease, key_id: &str, real_model: &str) {
        if lease.failed.load(Ordering::Acquire) { return; }
        if let Some(state) = self.sessions.lock().get_mut(&(lease.category, lease.session.clone())) {
            if state.generation == lease.generation && state.turn == lease.turn
                && state.binding.as_ref().map_or(true, |b| b.ticket <= lease.ticket) {
                if lease.failed.load(Ordering::Acquire) { return; }
                state.previous_model = state.model.clone();
                state.binding = Some(Binding { key_id: key_id.into(), real_model: real_model.into(), ticket: lease.ticket });
            }
        }
    }
    pub fn failed(&self, lease: &Lease) {
        lease.failed.store(true, Ordering::Release);
        if let Some(state) = self.sessions.lock().get_mut(&(lease.category, lease.session.clone())) {
            if state.generation == lease.generation && state.binding.as_ref().is_some_and(|b| b.ticket == lease.ticket) {
                state.binding = None;
            }
        }
    }
    pub fn forget_key(&self, lease: &Lease, key_id: &str) {
        if let Some(state) = self.sessions.lock().get_mut(&(lease.category, lease.session.clone())) {
            if state.generation == lease.generation && state.turn == lease.turn
                && state.binding.as_ref().is_some_and(|b| b.key_id == key_id && b.ticket <= lease.ticket) {
                state.binding = None;
            }
        }
    }
    pub fn update(&self, category: CategoryType, update: impl FnOnce(&mut Activity)) {
        let mut activities = self.activities.lock();
        let activity = activities.entry(category).or_default();
        activity.category_id = Some(category);
        activity.observed_at = chrono::Utc::now().timestamp_millis();
        update(activity);
    }
    pub fn judge_gate(&self, key: &str) -> Option<Arc<tokio::sync::Mutex<()>>> {
        let mut judges = self.judges.lock();
        judges.retain(|_, lock| Arc::strong_count(lock) > 1);
        if judges.len() >= 256 && !judges.contains_key(key) { return None; }
        Some(judges.entry(key.into()).or_default().clone())
    }
    pub fn cached(&self, key: &str) -> Option<Option<String>> {
        let mut decisions = self.decisions.lock();
        decisions.retain(|_, c| c.expires > Instant::now());
        decisions.get(key).map(|c| c.rule.clone())
    }
    pub fn cache(&self, key: String, rule: Option<String>) {
        let mut decisions = self.decisions.lock();
        decisions.retain(|_, c| c.expires > Instant::now());
        if decisions.len() >= 512 { decisions.clear(); }
        decisions.insert(key, CachedDecision { rule, expires: Instant::now() + Duration::from_secs(600) });
    }
}

pub fn group_fingerprint(group: &RouteGroup) -> String {
    super::hash_bytes(&serde_json::to_vec(group).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn routing_matrix_same_turn_late_failure_and_failed_candidate() {
        let rt = Runtime::default();
        let old = rt.begin(CategoryType::Codex, "s", "t", 1, "cfg", AffinityMode::Session).unwrap();
        rt.choose(&old, "model"); rt.served(&old, "first", "model");
        let new = rt.begin(CategoryType::Codex, "s", "t", 1, "cfg", AffinityMode::Session).unwrap();
        rt.forget_key(&new, "first");
        assert!(rt.state(&new).unwrap().binding.is_none());
        rt.served(&new, "second", "model");
        rt.failed(&old); rt.served(&old, "first", "model"); rt.forget_key(&old, "second");
        assert_eq!(rt.state(&new).unwrap().binding.unwrap().key_id, "second");
        rt.failed(&new); rt.served(&new, "second", "model");
        assert!(rt.state(&new).unwrap().binding.is_none());
    }

    #[test]
    fn routing_matrix_category_capacity_and_expiration() {
        let rt = Runtime::default();
        let a = rt.begin(CategoryType::Codex, "s", "t", 1, "cfg", AffinityMode::Session).unwrap();
        rt.served(&a, "a", "m");
        let b = rt.begin(CategoryType::ClaudeCli, "s", "t", 1, "cfg", AffinityMode::Session).unwrap();
        assert!(rt.state(&b).unwrap().binding.is_none());
        for n in 2..MAX_SESSIONS { assert!(rt.begin(CategoryType::Codex, &n.to_string(), "t", 1, "cfg", AffinityMode::Turn).is_some()); }
        assert!(rt.begin(CategoryType::Codex, "overflow", "t", 1, "cfg", AffinityMode::Turn).is_none());
        rt.sessions.lock().get_mut(&(CategoryType::Codex, "s".into())).unwrap().touched = Instant::now() - SESSION_TTL - Duration::from_secs(1);
        assert!(rt.begin(CategoryType::Codex, "overflow", "t", 1, "cfg", AffinityMode::Turn).is_some());
        assert!(rt.state(&a).is_none());
    }

    #[test]
    fn late_outcomes_cannot_replace_newer_turns_or_keys() {
        let rt = Runtime::default();
        let a = rt.begin(CategoryType::Codex, "s", "turn1", 1, "cfg", AffinityMode::Session).unwrap();
        rt.choose(&a, "a"); rt.served(&a, "key-a", "model-a");
        let b = rt.begin(CategoryType::Codex, "s", "turn2", 2, "cfg", AffinityMode::Session).unwrap();
        assert_eq!(rt.state(&b).unwrap().binding.unwrap().key_id, "key-a");
        rt.choose(&b, "b"); rt.served(&b, "key-b", "model-b"); rt.failed(&a); rt.served(&a, "key-a", "model-a");
        assert_eq!(rt.state(&b).unwrap().binding.unwrap().key_id, "key-b");
        assert!(rt.begin(CategoryType::Codex, "s", "turn1", 1, "cfg", AffinityMode::Session).is_none());
        rt.reset(CategoryType::Codex); rt.served(&b, "key-b", "model-b");
        assert!(rt.state(&b).is_none());
    }
    #[test]
    fn turn_mode_and_configuration_changes_release_bindings() {
        let rt = Runtime::default();
        let a = rt.begin(CategoryType::ClaudeCli, "s", "1", 1, "cfg", AffinityMode::Turn).unwrap();
        rt.served(&a, "a", "m");
        let b = rt.begin(CategoryType::ClaudeCli, "s", "2", 2, "cfg", AffinityMode::Turn).unwrap();
        assert!(rt.state(&b).unwrap().binding.is_none());
        rt.served(&b, "b", "m");
        let changed = rt.begin(CategoryType::ClaudeCli, "s", "2", 2, "new", AffinityMode::Session).unwrap();
        assert!(rt.state(&changed).unwrap().binding.is_none());
        rt.failed(&changed); rt.served(&changed, "x", "m");
        assert!(rt.state(&changed).unwrap().binding.is_none());
    }
}
