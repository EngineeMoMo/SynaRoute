use super::Store;
use crate::model::EventLogEntry;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Cursor {
    epoch: String,
    revision: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Delta {
    cursor: Cursor,
    reset: bool,
    entries: Vec<EventLogEntry>,
    removed: Vec<String>,
}

pub(super) struct Feed {
    cursor: Cursor,
    floor: u64,
    versions: HashMap<String, u64>,
    removed: VecDeque<(u64, String)>,
}

impl Default for Feed {
    fn default() -> Self {
        Self {
            cursor: Cursor { epoch: uuid::Uuid::new_v4().to_string(), revision: 0 },
            floor: 0,
            versions: HashMap::new(),
            removed: VecDeque::new(),
        }
    }
}

impl Feed {
    pub(super) fn changed(&mut self, id: &str, removed: impl Iterator<Item = String>) {
        self.cursor.revision += 1;
        self.versions.insert(id.to_owned(), self.cursor.revision);
        for id in removed {
            self.versions.remove(&id);
            self.removed.push_back((self.cursor.revision, id));
        }
        while self.removed.len() > super::MAX_EVENTS {
            if let Some((revision, _)) = self.removed.pop_front() { self.floor = revision; }
        }
    }

    fn delta(&self, events: &[EventLogEntry], cursor: Option<&Cursor>) -> Delta {
        let previous = cursor.filter(|cursor| cursor.epoch == self.cursor.epoch
            && cursor.revision >= self.floor && cursor.revision <= self.cursor.revision);
        let reset = previous.is_none();
        let revision = previous.map_or(0, |cursor| cursor.revision);
        Delta {
            cursor: self.cursor.clone(), reset,
            entries: events.iter().filter(|entry| reset || self.versions.get(&entry.id).is_some_and(|changed| *changed > revision))
                .map(Store::strip_trace).collect(),
            removed: if reset { Vec::new() } else {
                self.removed.iter().filter(|(changed, _)| *changed > revision).map(|(_, id)| id.clone()).collect()
            },
        }
    }
}

#[tauri::command]
pub(crate) fn list_event_delta(state: tauri::State<crate::AppState>, cursor: Option<Cursor>) -> Delta {
    state.store.event_delta(cursor.as_ref())
}

impl Store {
    fn event_delta(&self, cursor: Option<&Cursor>) -> Delta {
        let events = self.events.read();
        self.event_feed.read().delta(&events, cursor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::CategoryType;

    #[test]
    fn delta_tracks_append_collapse_usage_and_eviction_without_trace() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::new_at(directory.path().join("config.json"), directory.path().join("secrets.enc")).unwrap();
        let category = CategoryType::ClaudeCli;
        store.append_event_collapsible(category, "route", Some("key"), "first", None, Some("same".into()));
        let first = store.event_delta(None);
        assert!(first.reset);
        assert_eq!(first.entries.len(), 1);
        assert!(store.event_delta(Some(&first.cursor)).entries.is_empty());
        store.append_event_collapsible(category, "route", Some("key"), "second", None, Some("same".into()));
        let folded = store.event_delta(Some(&first.cursor));
        assert!(!folded.reset);
        assert_eq!(folded.entries[0].repeat, 2);
        store.backfill_usage_for_collapsed_event(category, Some("key"), "same", crate::upstream::TokenUsage { input: 7, output: 3, ..Default::default() }, "model");
        let usage = store.event_delta(Some(&folded.cursor));
        assert_eq!(usage.entries[0].usage.unwrap().input, 7);
        assert!(usage.entries[0].trace.is_none());
        for index in 0..super::super::MAX_EVENTS {
            store.append_event(category, "route", None, &index.to_string());
        }
        let evicted = store.event_delta(Some(&usage.cursor));
        assert_eq!(evicted.removed, vec![first.entries[0].id.clone()]);
        assert_eq!(evicted.entries.len(), super::super::MAX_EVENTS);
        for index in 0..super::super::MAX_EVENTS + 1 {
            store.append_event(category, "route", None, &index.to_string());
        }
        assert!(store.event_delta(Some(&usage.cursor)).reset);
        assert!(store.event_delta(Some(&Cursor { epoch: "previous-process".into(), revision: 0 })).reset);
        assert!(store.event_feed.read().versions.len() <= super::super::MAX_EVENTS);
        assert!(store.event_feed.read().removed.len() <= super::super::MAX_EVENTS);
    }
}
