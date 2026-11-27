use super::{born_after_sync, files, original_to_record, source_value_marks_internal, Manifest, ManifestEntry, SessionRef};
use crate::error::{AppError, AppResult};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Write};
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Provider { Missing, Null, Id(String) }

impl Provider {
    fn id(&self) -> Option<&str> {
        match self { Self::Id(id) => Some(id), _ => None }
    }

    fn legacy(id: &str) -> Self {
        if id.is_empty() { Self::Missing } else { Self::Id(id.to_owned()) }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Record { identity: String, provider: Provider }

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Slot { Meta(usize), Settings(usize) }

#[derive(PartialEq, Eq)]
struct Snapshot { record: Record, owned: bool }

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(super) struct SavedSettings { records: Vec<Option<Record>>, targets: Vec<String> }

impl SavedSettings {
    fn manages(&self, provider: &Provider) -> bool {
        self.targets.iter().any(|id| provider.id() == Some(id.as_str()))
    }
}

#[derive(Default, PartialEq, Eq)]
pub(super) struct Plan {
    records: Vec<Record>,
    settings: Vec<Snapshot>,
    legacy_start: usize,
    pub(super) internal: bool,
    pub(super) has_user_event: bool,
    pub(super) cwd: Option<String>,
}

fn changed() -> io::Error {
    io::Error::other("会话在同步期间发生变化，未覆盖新内容，请重新同步")
}

fn walk(path: &Path, reader: &mut dyn BufRead, output: &mut dyn Write, changes: &BTreeMap<Slot, Provider>) -> io::Result<Plan> {
    let owner = path.file_name().and_then(|name| name.to_str()).and_then(files::thread_id_from_filename);
    let mut plan = Plan::default();
    let mut line = Vec::new();
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 { break; }
        if line.windows(b"\"user_message\"".len()).any(|window| window == b"\"user_message\"")
            || line.windows(b"\"user_input\"".len()).any(|window| window == b"\"user_input\"")
        {
            plan.has_user_event = true;
        }
        let mut parsed = std::str::from_utf8(&line).ok()
            .filter(|text| text.contains("session_meta") || text.contains("thread_settings_applied") || text.contains("\\u"))
            .and_then(|text| serde_json::from_str::<Value>(text).ok());
        if plan.records.is_empty() && parsed.as_ref().is_none_or(|row| row["type"] != "session_meta") {
            return Err(io::Error::other("首行不是 session_meta"));
        }
        if let Some(row) = parsed.as_mut() {
            if plan.cwd.is_none() && row["type"] == "session_meta" {
                plan.cwd = row["payload"]["cwd"].as_str().map(str::trim).filter(|cwd| !cwd.is_empty()).map(str::to_owned);
            }
            if let Some((slot, desired)) = plan.observe(row, owner.as_deref())?.and_then(|slot| changes.get(&slot).map(|want| (slot, want))) {
                let (object, field) = match slot {
                    Slot::Meta(_) => (&mut row["payload"], "model_provider"),
                    Slot::Settings(_) => (&mut row["payload"]["thread_settings"], "model_provider_id"),
                };
                match desired {
                    Provider::Missing => {},
                    Provider::Null => object[field] = Value::Null,
                    Provider::Id(id) => object[field] = Value::String(id.clone()),
                }
                serde_json::to_writer(&mut *output, row)?;
                let eol = if line.ends_with(b"\r\n") { &b"\r\n"[..] } else if line.ends_with(b"\n") { &b"\n"[..] } else { &[] };
                output.write_all(eol)?;
                continue;
            }
        }
        output.write_all(&line)?;
    }
    if plan.records.is_empty() { return Err(io::Error::other("会话不含 session_meta")); }
    Ok(plan)
}

impl Plan {
    fn observe(&mut self, row: &mut Value, owner: Option<&str>) -> io::Result<Option<Slot>> {
        if row["type"] == "session_meta" {
            let payload = row["payload"].as_object_mut().ok_or_else(|| io::Error::other("session_meta 缺 payload"))?;
            self.internal |= payload.get("source").is_some_and(source_value_marks_internal)
                || payload.get("thread_source").and_then(Value::as_str).is_some_and(|source| {
                    !source.trim().is_empty() && !source.trim().eq_ignore_ascii_case("user")
                });
            let provider = match payload.remove("model_provider") {
                None => Provider::Missing, Some(Value::Null) => Provider::Null,
                Some(Value::String(id)) => Provider::Id(id),
                _ => return Err(io::Error::other("session_meta 的 model_provider 不是字符串")),
            };
            let key = [&row["ordinal"], &row["timestamp"], &row["payload"]["id"],
                &row["payload"]["session_id"], &row["payload"]["timestamp"]];
            let identity = format!("{:x}", Sha256::digest(serde_json::to_vec(&key)?));
            self.records.push(Record { identity, provider });
            return Ok(Some(Slot::Meta(self.records.len() - 1)));
        }
        if row["type"] != "event_msg" || row["payload"]["type"] != "thread_settings_applied"
            || !row["payload"]["thread_settings"]["model_provider_id"].is_string() { return Ok(None); }
        let owner = owner.ok_or_else(|| io::Error::other("无法从会话文件名确认线程设置的归属"))?;
        let owned = match &row["payload"]["thread_id"] {
            Value::String(id) if id.eq_ignore_ascii_case(owner) => true,
            Value::String(_) => { self.legacy_start = self.settings.len(); return Ok(None); },
            Value::Null => false,
            _ => return Ok(None),
        };
        let provider = row["payload"]["thread_settings"].as_object_mut().and_then(|obj| obj.remove("model_provider_id"));
        let Some(Value::String(id)) = provider else { return Ok(None); };
        let identity = format!("{:x}", Sha256::digest(serde_json::to_vec(row)?));
        self.settings.push(Snapshot { record: Record { identity, provider: Provider::Id(id) }, owned });
        Ok(Some(Slot::Settings(self.settings.len() - 1)))
    }

    pub(super) fn read(path: &Path) -> io::Result<Self> {
        walk(path, &mut BufReader::new(File::open(path)?), &mut io::sink(), &BTreeMap::new())
    }

    fn slots(&self) -> impl Iterator<Item = (Slot, &Record)> {
        self.records.iter().enumerate().map(|(index, record)| (Slot::Meta(index), record))
            .chain(self.settings.iter().enumerate().map(|(index, snapshot)| (Slot::Settings(index), &snapshot.record)))
    }

    fn eligible(&self, slot: Slot) -> bool {
        match slot { Slot::Meta(_) => true, Slot::Settings(index) => self.settings[index].owned || index >= self.legacy_start }
    }

    pub(super) fn matches(&self, target: &str) -> bool {
        self.slots().all(|(slot, record)| !self.eligible(slot) || record.provider.id() == Some(target))
    }

    fn validate(&self, entry: &ManifestEntry) -> io::Result<()> {
        if entry.records.len() > self.records.len() || entry.records.iter().zip(&self.records).any(|(old, now)| old.identity != now.identity)
            || entry.settings_records.as_ref().is_some_and(|saved| saved.records.len() > self.settings.len() || saved.records.iter().zip(&self.settings)
                .any(|(old, now)| old.as_ref().is_some_and(|record| record.identity != now.record.identity))) {
            return Err(changed());
        }
        Ok(())
    }

    pub(super) fn remember(&self, manifest: &mut Manifest, session: &SessionRef, target: &str) -> AppResult<bool> {
        let existing = manifest.entries.iter().position(|entry| entry.rel_path == session.rel_path);
        let born = existing.is_none() && !manifest.fallback_provider.is_empty() && session.provider == manifest.target
            && born_after_sync(session, &manifest.synced_at);
        let index = existing.unwrap_or_else(|| {
            manifest.entries.push(ManifestEntry { rel_path: session.rel_path.clone(), thread_id: session.thread_id.clone(),
                original_provider: String::new(), records: Vec::new(), settings_records: None });
            manifest.entries.len() - 1
        });
        let entry = &mut manifest.entries[index];
        let legacy = (existing.is_some() && entry.records.is_empty()).then(|| Provider::legacy(&entry.original_provider));
        self.validate(entry)?;
        if entry.records.len() == self.records.len() && entry.settings_records.as_ref().is_some_and(|saved|
            saved.targets.iter().any(|id| id == target) && saved.records.len() == self.settings.len() &&
            saved.records.iter().enumerate().all(|(index, old)| old.is_some() || !self.eligible(Slot::Settings(index)))) { return Ok(false); }
        entry.records.extend(self.records[entry.records.len()..].iter().enumerate().map(|(offset, observed)| {
            let mut record = observed.clone();
            if let Some(first) = legacy.as_ref().filter(|_| offset == 0) {
                record.provider = first.clone();
            } else if born && record.provider.id() == Some(&manifest.target) {
                record.provider = Provider::Id(manifest.fallback_provider.clone());
            } else if record.provider.id() == Some(super::super::MCP_CLIENT_NAME) && record.provider.id() != Some(target) {
                record.provider = Provider::legacy(&original_to_record(super::super::MCP_CLIENT_NAME, &manifest.fallback_provider));
            }
            record
        }));
        let inherited = entry.settings_records.as_ref().and_then(|saved| self.inherited_id(saved, &entry.records, &manifest.fallback_provider)).map(str::to_owned);
        let saved = entry.settings_records.get_or_insert_with(SavedSettings::default);
        let previous_len = saved.records.len();
        saved.records.resize(self.settings.len(), None);
        for (index, old) in saved.records.iter_mut().enumerate() {
            if old.is_none() && self.eligible(Slot::Settings(index)) {
                let mut record = self.settings[index].record.clone();
                if let Some(id) = inherited.as_ref().filter(|_| index >= previous_len && saved.targets.iter().any(|id| record.provider.id() == Some(id.as_str()))) {
                    record.provider = Provider::Id(id.clone());
                } else if !manifest.fallback_provider.is_empty() && ((born && record.provider.id() == Some(&manifest.target)) ||
                    (record.provider.id() == Some(super::super::MCP_CLIENT_NAME) && record.provider.id() != Some(target))) {
                    record.provider = Provider::Id(manifest.fallback_provider.clone());
                }
                *old = Some(record);
            }
        }
        if !saved.targets.iter().any(|id| id == target) { saved.targets.push(target.to_owned()); }
        if existing.is_none() {
            entry.original_provider = entry.records[0].provider.id().unwrap_or_default().to_owned();
        }
        Ok(true)
    }

    fn apply(&self, path: &Path, desired: impl Fn(Slot, &Record) -> Option<Provider>) -> AppResult<Option<files::RewriteInfo>> {
        let changes: BTreeMap<_, _> = self.slots().filter_map(|(slot, record)| {
            desired(slot, record).filter(|want| *want != record.provider).map(|want| (slot, want))
        }).collect();
        if changes.is_empty() { return Ok(None); }
        files::rewrite_checked(path, |reader, output| {
            let observed = walk(path, reader, output, &changes)?;
            if observed.internal || observed != *self { return Err(changed()); }
            Ok(())
        }, &mut || Ok(())).map(Some)
    }

    pub(super) fn sync(&self, path: &Path, target: &str) -> AppResult<Option<files::RewriteInfo>> {
        self.apply(path, |slot, _| self.eligible(slot).then(|| Provider::Id(target.to_string())))
    }

    fn inherited_id<'a>(&self, saved: &'a SavedSettings, metadata: &'a [Record], fallback: &'a str) -> Option<&'a str> {
        saved.records.iter().enumerate().rev().find_map(|(index, old)| old.as_ref().filter(|_| self.eligible(Slot::Settings(index))))
            .and_then(|record| record.provider.id()).or_else(|| metadata.last().and_then(|record| record.provider.id()))
            .filter(|id| !id.is_empty()).or_else(|| (!fallback.is_empty()).then_some(fallback))
    }
}

pub(super) fn born_entry(path: &Path, session: SessionRef, target: &str, fallback: &str) -> AppResult<Option<ManifestEntry>> {
    let plan = Plan::read(path)?;
    if plan.internal || plan.records[0].provider.id() != Some(target) { return Ok(None); }
    let original = |mut record: Record| {
        if record.provider.id() == Some(target) { record.provider = Provider::legacy(fallback); }
        record
    };
    let settings = plan.settings.iter().enumerate().map(|(index, snapshot)| {
        plan.eligible(Slot::Settings(index)).then(|| original(snapshot.record.clone()))
    }).collect();
    Ok(Some(ManifestEntry { rel_path: session.rel_path, thread_id: session.thread_id, original_provider: fallback.into(),
        records: plan.records.into_iter().map(original).collect(), settings_records: Some(SavedSettings { records: settings, targets: vec![target.to_owned()] }) }))
}

pub(super) fn restore(path: &Path, entry: &ManifestEntry, target: &str, fallback: &str) -> AppResult<bool> {
    if entry.records.is_empty() {
        let want = (!entry.original_provider.is_empty()).then_some(entry.original_provider.as_str());
        return super::rewrite_first_line(path, want).map(|result| result.is_some());
    }
    let plan = Plan::read(path)?;
    if plan.internal { return Err(AppError::ToolConfig("会话来源已变为内部线程，未还原".into())); }
    plan.validate(entry)?;
    let inherited = entry.settings_records.as_ref().and_then(|saved| plan.inherited_id(saved, &entry.records, fallback));
    plan.apply(path, |slot, current| match slot {
        Slot::Meta(index) => entry.records.get(index).or_else(|| {
            (current.provider.id() == Some(target)).then(|| entry.records.last()).flatten()
        }).map(|record| record.provider.clone()),
        Slot::Settings(index) => {
            let saved = entry.settings_records.as_ref()?;
            if !saved.manages(&current.provider) { return None; }
            match saved.records.get(index) {
                Some(old) => old.as_ref().map(|record| record.provider.clone()),
                None if plan.eligible(slot) => inherited.map(|id| Provider::Id(id.to_string())),
                None => None,
            }
        }
    }).map(|result| result.is_some())
}
