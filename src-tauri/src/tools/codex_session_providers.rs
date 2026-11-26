use super::{files, original_to_record, source_value_marks_internal, Manifest, ManifestEntry, SessionRef};
use crate::error::{AppError, AppResult};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
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

#[derive(Default)]
pub(super) struct Plan { records: Vec<Record>, pub(super) internal: bool }

fn changed() -> io::Error {
    io::Error::other("会话在同步期间发生变化，未覆盖新内容，请重新同步")
}

fn walk(
    reader: &mut dyn BufRead, output: &mut dyn Write,
    mut visit: impl FnMut(usize, Record, bool) -> io::Result<Option<Provider>>,
) -> io::Result<usize> {
    let mut line = Vec::new();
    let mut count = 0;
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 { break; }
        let mut meta = std::str::from_utf8(&line).ok()
            .filter(|text| text.contains("session_meta") || text.contains("\\u"))
            .and_then(|text| serde_json::from_str::<Value>(text).ok())
            .filter(|row| row["type"] == "session_meta");
        let Some(row) = meta.as_mut() else {
            if count == 0 { return Err(io::Error::other("首行不是 session_meta")); }
            output.write_all(&line)?;
            continue;
        };
        let payload = row["payload"].as_object_mut().ok_or_else(|| io::Error::other("session_meta 缺 payload"))?;
        let internal = payload.get("source").is_some_and(source_value_marks_internal)
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
        if let Some(desired) = visit(count, Record { identity, provider }, internal)? {
            let payload = row["payload"].as_object_mut().expect("validated metadata object");
            match desired {
                Provider::Missing => {},
                Provider::Null => { payload.insert("model_provider".into(), Value::Null); },
                Provider::Id(id) => { payload.insert("model_provider".into(), Value::String(id)); },
            }
            serde_json::to_writer(&mut *output, row)?;
            let eol = if line.ends_with(b"\r\n") { &b"\r\n"[..] } else if line.ends_with(b"\n") { &b"\n"[..] } else { &[] };
            output.write_all(eol)?;
        } else { output.write_all(&line)?; }
        count += 1;
    }
    if count == 0 { return Err(io::Error::other("会话不含 session_meta")); }
    Ok(count)
}

impl Plan {
    pub(super) fn read(path: &Path) -> io::Result<Self> {
        let mut plan = Self::default();
        walk(&mut BufReader::new(File::open(path)?), &mut io::sink(), |_, record, internal| {
            plan.records.push(record);
            plan.internal |= internal;
            Ok(None)
        })?;
        Ok(plan)
    }

    pub(super) fn matches(&self, target: &str) -> bool {
        self.records.iter().all(|record| record.provider.id() == Some(target))
    }

    fn validate(&self, saved: &[Record]) -> io::Result<()> {
        if saved.len() > self.records.len() || saved.iter().zip(&self.records).any(|(old, now)| old.identity != now.identity) {
            return Err(changed());
        }
        Ok(())
    }

    pub(super) fn remember(&self, manifest: &mut Manifest, session: &SessionRef, target: &str) -> AppResult<bool> {
        let old = manifest.entries.iter_mut().find(|entry| entry.rel_path == session.rel_path);
        let legacy = old.as_ref().filter(|entry| entry.records.is_empty()).map(|entry| Provider::legacy(&entry.original_provider));
        let saved = old.as_ref().map_or(&[][..], |entry| entry.records.as_slice());
        self.validate(saved)?;
        if saved.len() == self.records.len() { return Ok(false); }
        let extra = self.records[saved.len()..].iter().enumerate().map(|(offset, observed)| {
            let mut record = observed.clone();
            if let Some(first) = legacy.as_ref().filter(|_| saved.is_empty() && offset == 0) {
                record.provider = first.clone();
            } else if record.provider.id() == Some(super::super::MCP_CLIENT_NAME) && record.provider.id() != Some(target) {
                record.provider = Provider::legacy(&original_to_record(super::super::MCP_CLIENT_NAME, &manifest.fallback_provider));
            }
            record
        }).collect::<Vec<_>>();
        if let Some(entry) = old { entry.records.extend(extra); } else {
            manifest.entries.push(ManifestEntry {
                rel_path: session.rel_path.clone(), thread_id: session.thread_id.clone(),
                original_provider: extra[0].provider.id().unwrap_or_default().to_string(), records: extra,
            });
        }
        Ok(true)
    }

    fn apply(&self, path: &Path, desired: impl Fn(usize) -> Option<Provider>) -> AppResult<Option<files::RewriteInfo>> {
        if !self.records.iter().enumerate().any(|(index, record)| desired(index).is_some_and(|want| want != record.provider)) { return Ok(None); }
        files::rewrite_checked(path, |reader, output| {
            let count = walk(reader, output, |index, current, internal| {
                if internal || self.records.get(index) != Some(&current) { return Err(changed()); }
                Ok(desired(index).filter(|want| *want != current.provider))
            })?;
            if count != self.records.len() { return Err(changed()); }
            Ok(())
        }, &mut || Ok(())).map(Some)
    }

    pub(super) fn sync(&self, path: &Path, target: &str) -> AppResult<Option<files::RewriteInfo>> {
        self.apply(path, |_| Some(Provider::Id(target.to_string())))
    }
}

pub(super) fn born_records(path: &Path, target: &str, fallback: &str) -> AppResult<Option<Vec<Record>>> {
    let plan = Plan::read(path)?;
    if plan.internal || plan.records[0].provider.id() != Some(target) { return Ok(None); }
    Ok(Some(plan.records.into_iter().map(|mut record| {
        if record.provider.id() == Some(target) { record.provider = Provider::legacy(fallback); }
        record
    }).collect()))
}

pub(super) fn restore(path: &Path, entry: &ManifestEntry, target: &str) -> AppResult<bool> {
    if entry.records.is_empty() {
        let want = (!entry.original_provider.is_empty()).then_some(entry.original_provider.as_str());
        return super::rewrite_first_line(path, want).map(|result| result.is_some());
    }
    let plan = Plan::read(path)?;
    if plan.internal { return Err(AppError::ToolConfig("会话来源已变为内部线程，未还原".into())); }
    plan.validate(&entry.records)?;
    plan.apply(path, |index| {
        entry.records.get(index).or_else(|| {
            (plan.records[index].provider.id() == Some(target)).then(|| entry.records.last()).flatten()
        }).map(|record| record.provider.clone())
    }).map(|result| result.is_some())
}
