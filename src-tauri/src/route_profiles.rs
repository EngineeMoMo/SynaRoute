//! Passive observations of proxy attempts. No prompts, endpoints, probes or credentials.
//! Streaming success means response established, not successful stream completion.
use crate::model::{CategoryType, ProviderKey};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const DAY_MS: i64 = 86_400_000;
const MAX_ROWS: usize = 10_000;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileRow {
    pub day: i64,
    pub category_id: CategoryType,
    pub key_id: String,
    pub requested_model: String,
    pub real_model: String,
    pub streaming: bool,
    pub attempts: u64,
    pub successes: u64,
    pub rate_limits: u64,
    pub success_latency_ms: u64,
    pub last_seen: i64,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileSnapshot {
    pub version: u32,
    pub since: i64,
    pub rows: Vec<ProfileRow>,
    pub dropped: u64,
    #[serde(skip_deserializing)]
    pub read_only: bool,
}

struct State {
    snapshot: ProfileSnapshot,
    dirty: bool,
}

pub struct ProfileStore {
    path: PathBuf,
    state: Mutex<State>,
    flush_lock: Mutex<()>,
}

impl ProfileStore {
    pub fn load(path: PathBuf) -> Self {
        let fresh = || ProfileSnapshot {
            version: 1,
            since: chrono::Utc::now().timestamp_millis(),
            rows: vec![],
            dropped: 0,
            read_only: false,
        };
        let snapshot = match std::fs::read(&path) {
            Ok(raw) => match serde_json::from_slice::<ProfileSnapshot>(&raw) {
                Ok(s) if s.version == 1 && s.rows.len() <= MAX_ROWS => s,
                _ => {
                    tracing::warn!("线路画像文件无法识别，本次只读以保留原文件");
                    ProfileSnapshot {
                        read_only: true,
                        ..fresh()
                    }
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => fresh(),
            Err(e) => {
                tracing::warn!("线路画像读取失败，本次只读: {e}");
                ProfileSnapshot {
                    read_only: true,
                    ..fresh()
                }
            }
        };
        Self {
            path,
            state: Mutex::new(State {
                snapshot,
                dirty: false,
            }),
            flush_lock: Mutex::new(()),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record(
        &self,
        key: &ProviderKey,
        requested: &str,
        real: &str,
        streaming: bool,
        ok: bool,
        status: Option<u16>,
        latency: u64,
    ) {
        self.record_at(
            key,
            requested,
            real,
            streaming,
            ok,
            status,
            latency,
            chrono::Utc::now().timestamp_millis(),
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn record_at(
        &self,
        key: &ProviderKey,
        requested: &str,
        real: &str,
        streaming: bool,
        ok: bool,
        status: Option<u16>,
        latency: u64,
        now: i64,
    ) {
        let mut state = self.state.lock();
        let s = &mut state.snapshot;
        let day = now.div_euclid(DAY_MS);
        s.rows.retain(|r| r.day >= day - 29);
        // Bound cardinality and attacker-controlled model strings on the hot path.
        if requested.len() > 256 || real.len() > 256 {
            s.dropped = s.dropped.saturating_add(1);
            state.dirty = true;
            return;
        }
        let pos = s.rows.iter().position(|r| {
            r.day == day
                && r.category_id == key.category_id
                && r.key_id == key.id
                && r.requested_model == requested
                && r.real_model == real
                && r.streaming == streaming
        });
        let row = if let Some(i) = pos {
            &mut s.rows[i]
        } else {
            if s.rows.len() >= MAX_ROWS {
                s.dropped = s.dropped.saturating_add(1);
                state.dirty = true;
                return;
            }
            s.rows.push(ProfileRow {
                day,
                category_id: key.category_id,
                key_id: key.id.clone(),
                requested_model: requested.into(),
                real_model: real.into(),
                streaming,
                attempts: 0,
                successes: 0,
                rate_limits: 0,
                success_latency_ms: 0,
                last_seen: now,
            });
            s.rows.last_mut().expect("row just inserted")
        };
        row.attempts = row.attempts.saturating_add(1);
        row.successes = row.successes.saturating_add(u64::from(ok));
        row.rate_limits = row
            .rate_limits
            .saturating_add(u64::from(status == Some(429)));
        if ok {
            row.success_latency_ms = row.success_latency_ms.saturating_add(latency);
        }
        row.last_seen = now;
        state.dirty = true;
    }

    pub fn snapshot(&self) -> ProfileSnapshot {
        let mut s = self.state.lock().snapshot.clone();
        let day = chrono::Utc::now().timestamp_millis().div_euclid(DAY_MS);
        s.rows.retain(|r| r.day >= day - 29 && r.day <= day);
        s
    }

    /// Existing usage maintenance/exit hooks call this; never write on the proxy hot path.
    pub fn flush(&self) {
        let _flush = self.flush_lock.lock();
        let snapshot = {
            let mut state = self.state.lock();
            if !state.dirty || state.snapshot.read_only {
                return;
            }
            state.dirty = false;
            state.snapshot.clone()
        };
        let result = serde_json::to_vec(&snapshot)
            .map_err(crate::error::AppError::from)
            .and_then(|bytes| crate::secret::atomic_write(&self.path, &bytes));
        if let Err(e) = result {
            self.state.lock().dirty = true;
            tracing::warn!("线路画像保存失败，将重试: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn separated_by_model_stream_day_and_restored_without_double_counting() {
        let dir = std::env::temp_dir().join(format!("sr-profile-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("profiles.json");
        let store = ProfileStore::load(path.clone());
        let key = ProviderKey {
            id: "one".into(),
            ..Default::default()
        };
        let now = chrono::Utc::now().timestamp_millis();
        store.record_at(&key, "alias", "model", true, true, Some(200), 80, now);
        store.record_at(&key, "alias", "model", true, false, Some(429), 999, now);
        store.record_at(&key, "alias", "model", false, true, Some(200), 400, now);
        store.record_at(&key, "other", "model", true, true, Some(200), 10, now);
        store.flush();
        store.flush();
        let restored = ProfileStore::load(path).snapshot();
        assert_eq!(restored.rows.len(), 3);
        let r = &restored.rows[0];
        assert_eq!(
            (r.attempts, r.successes, r.rate_limits, r.success_latency_ms),
            (2, 1, 1, 80)
        );
        store.record_at(
            &key,
            "new",
            "new",
            true,
            true,
            Some(200),
            10,
            now + 30 * DAY_MS,
        );
        assert_eq!(store.state.lock().snapshot.rows.len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unknown_file_is_not_overwritten_and_long_model_is_not_retained() {
        let path = std::env::temp_dir().join(format!("sr-profile-{}.json", uuid::Uuid::new_v4()));
        let original = br#"{"version":99,"since":0,"rows":[],"dropped":0}"#;
        std::fs::write(&path, original).unwrap();
        let s = ProfileStore::load(path.clone());
        s.record(
            &ProviderKey::default(),
            &"x".repeat(257),
            "m",
            false,
            false,
            None,
            0,
        );
        assert!(s.snapshot().read_only);
        assert_eq!(s.snapshot().dropped, 1);
        s.flush();
        assert_eq!(std::fs::read(&path).unwrap(), original);
        std::fs::remove_file(path).unwrap();
    }
}
