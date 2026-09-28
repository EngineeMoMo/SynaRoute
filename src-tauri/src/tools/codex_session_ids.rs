use super::files;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;

const MAX_LINE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_ITEM_IDS: usize = 500_000;

#[derive(Clone)]
pub(super) struct Analysis {
    pub digest: String,
    pub mapping: HashMap<String, String>,
}

fn items(record: &mut Value) -> Vec<&mut Value> {
    match record.get("type").and_then(Value::as_str) {
        Some("response_item") => record.get_mut("payload").into_iter().collect(),
        Some("compacted") => record.get_mut("payload")
            .and_then(|payload| payload.get_mut("replacement_history"))
            .and_then(Value::as_array_mut)
            .map(|history| history.iter_mut().collect()).unwrap_or_default(),
        _ => Vec::new(),
    }
}

fn read_line(reader: &mut impl BufRead) -> Result<Option<String>, String> {
    let mut bytes = Vec::new();
    Read::take(&mut *reader, MAX_LINE_BYTES + 1).read_until(b'\n', &mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.is_empty() { return Ok(None); }
    if bytes.len() as u64 > MAX_LINE_BYTES {
        return Err("单条历史记录超过 16 MiB，无法安全自动修复".into());
    }
    String::from_utf8(bytes).map(Some).map_err(|_| "会话包含非 UTF-8 数据".into())
}

fn record(line: &str) -> Result<Value, String> {
    if line.trim().is_empty() { Ok(Value::Null) }
    else { serde_json::from_str(line).map_err(|_| "会话包含损坏或未写完的 JSON 记录".into()) }
}

fn analyze_reader(reader: &mut impl BufRead) -> Result<Analysis, String> {
    let mut digest = Sha256::new();
    let mut existing = HashSet::new();
    let mut mapping = HashMap::new();
    let mut destinations = HashSet::new();
    let mut owners = HashMap::new();
    let mut ambiguous = HashSet::new();
    while let Some(line) = read_line(reader)? {
        digest.update(line.as_bytes());
        for item in items(&mut record(&line)?) {
            let kind = item.get("type").and_then(Value::as_str).unwrap_or("");
            let Some(id) = item.get("id").and_then(Value::as_str) else { continue };
            if kind != "item_reference" {
                existing.insert(id.to_string());
                let owner = (kind.to_string(), item.get("call_id").cloned());
                if owners.insert(id.to_string(), owner.clone()).is_some_and(|previous| previous != owner) {
                    ambiguous.insert(id.to_string());
                }
                if existing.len() > MAX_ITEM_IDS { return Err("历史条目数量超过安全上限，未自动修复".into()); }
            }
            let prefix = match kind {
                "function_call" => "fc_",
                "custom_tool_call" => "ctc_",
                _ => continue,
            };
            if id.starts_with(prefix) { continue; }
            if item.get("call_id").and_then(Value::as_str).filter(|call| !call.is_empty()).is_none() {
                return Err("异常工具条目缺少 call_id，不能安全修复".into());
            }
            let suffix = id.split_once('_').map(|(_, suffix)| suffix).unwrap_or(id);
            if suffix.is_empty() { return Err("异常工具条目 ID 为空".into()); }
            let next = format!("{prefix}{suffix}");
            if let Some(previous) = mapping.insert(id.to_string(), next.clone()) {
                if previous != next { return Err("同一 ID 对应不同工具类型，不能安全修复".into()); }
            } else if !destinations.insert(next) {
                return Err("修复后的工具 ID 冲突，未修改文件".into());
            }
        }
    }
    if mapping.keys().any(|id| ambiguous.contains(id)) {
        return Err("异常 ID 被不同条目共用，不能安全修复".into());
    }
    if destinations.iter().any(|id| existing.contains(id)) {
        return Err("修复后的工具 ID 与已有条目冲突，未修改文件".into());
    }
    Ok(Analysis { digest: format!("{:x}", digest.finalize()), mapping })
}

fn rewrite_line(line: &str, mapping: &HashMap<String, String>) -> Result<String, String> {
    let mut value = record(line)?;
    let mut changed = false;
    for item in items(&mut value) {
        for field in ["id", "item_id"] {
            if let Some(next) = item.get(field).and_then(Value::as_str).and_then(|id| mapping.get(id)) {
                item[field] = Value::String(next.clone());
                changed = true;
            }
        }
    }
    if !changed { return Ok(line.to_string()); }
    Ok(serde_json::to_string(&value).map_err(|error| error.to_string())? + files::split_eol(line).1)
}

pub(super) fn analyze(path: &Path) -> Result<Analysis, String> {
    let source = File::open(path).map_err(|error| format!("无法读取会话：{error}"))?;
    analyze_reader(&mut BufReader::new(source))
}

pub(super) fn apply(path: &Path, backup: &Path, expected: &Analysis) -> Result<usize, String> {
    if expected.mapping.is_empty() { return Ok(0); }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(4);
    }
    let source = options.open(path).map_err(|error| format!("请先退出 Codex 再修复：{error}"))?;
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        if unsafe { libc::flock(source.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err("会话文件正被其他维护进程占用".into());
        }
    }
    let metadata = source.metadata().map_err(|error| error.to_string())?;
    if let Some(parent) = backup.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let temporary = files::tmp_path_for(path);
    let result = (|| -> Result<usize, String> {
        let mut saved = OpenOptions::new().write(true).create_new(true).open(backup).map_err(|error| error.to_string())?;
        let mut output = OpenOptions::new().write(true).create_new(true).open(&temporary).map_err(|error| error.to_string())?;
        let mut reader = BufReader::new(&source);
        let mut digest = Sha256::new();
        while let Some(line) = read_line(&mut reader)? {
            digest.update(line.as_bytes());
            saved.write_all(line.as_bytes()).map_err(|error| error.to_string())?;
            output.write_all(rewrite_line(&line, &expected.mapping)?.as_bytes()).map_err(|error| error.to_string())?;
        }
        saved.sync_all().map_err(|error| error.to_string())?;
        if format!("{:x}", digest.finalize()) != expected.digest {
            return Err("会话内容已变化，预览失效；未替换原文件，请重新扫描".into());
        }
        if let Ok(modified) = metadata.modified() {
            output.set_times(fs::FileTimes::new().set_modified(modified)).map_err(|error| error.to_string())?;
        }
        output.sync_all().map_err(|error| error.to_string())?;
        drop(output);
        let current = fs::metadata(path).map_err(|error| error.to_string())?;
        if current.len() != metadata.len() || current.modified().ok() != metadata.modified().ok() {
            return Err("会话在修复期间发生变化，未替换原文件".into());
        }
        fs::rename(&temporary, path).map_err(|error| error.to_string())?;
        Ok(expected.mapping.len())
    })();
    if result.is_err() { let _ = fs::remove_file(&temporary); }
    result.map_err(|error| format!("{error}；本次备份位置（失败时可能不完整）：{}", backup.display()))
}

#[cfg(test)]
fn repair_text(original: &str) -> Result<Option<(String, usize)>, String> {
    let analysis = analyze_reader(&mut std::io::Cursor::new(original))?;
    if analysis.mapping.is_empty() { return Ok(None); }
    let mut output = String::new();
    for line in original.split_inclusive('\n') { output.push_str(&rewrite_line(line, &analysis.mapping)?); }
    Ok(Some((output, analysis.mapping.len())))
}

#[cfg(test)]
fn repair_file(path: &Path, backup: &Path) -> Result<usize, String> {
    apply(path, backup, &analyze(path)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn streaming_scan_accepts_files_larger_than_the_old_whole_file_limit() {
        let path = std::env::temp_dir().join(format!("synaroute-large-history-{}.jsonl", uuid::Uuid::new_v4()));
        let mut output = File::create(&path).unwrap();
        output.write_all(line(json!({"type":"function_call","id":"call_large","call_id":"call_large"})).as_bytes()).unwrap();
        let padding = " ".repeat(1024 * 1024 - 1) + "\n";
        for _ in 0..129 { output.write_all(padding.as_bytes()).unwrap(); }
        drop(output);
        assert!(fs::metadata(&path).unwrap().len() > 128 * 1024 * 1024);
        assert_eq!(analyze(&path).unwrap().mapping["call_large"], "fc_large");
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn changes_during_apply_leave_the_original_untouched() {
        let home = std::env::temp_dir().join(format!("synaroute-stream-race-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&home).unwrap();
        let path = home.join("rollout.jsonl");
        let original = line(json!({"type":"function_call","id":"call_old","call_id":"call_old"}));
        fs::write(&path, &original).unwrap();
        let plan = analyze(&path).unwrap();
        let changed = original.replace("call_old", "call_new");
        fs::write(&path, &changed).unwrap();
        assert!(apply(&path, &home.join("backup.jsonl"), &plan).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), changed);
        fs::remove_dir_all(home).unwrap();
    }

    fn line(item: Value) -> String {
        format!("{}\r\n", json!({"type":"response_item", "payload":item}))
    }

    #[test]
    fn tool_id_repair_preserves_links_namespaces_content_and_is_idempotent() {
        let original = line(json!({"type":"function_call","id":"call_test","call_id":"call_test","namespace":"mcp__demo","name":"run","arguments":"{\"id\":\"call_test\"}"}))
            + &line(json!({"type":"function_call_output","call_id":"call_test","output":"call_test"}))
            + &line(json!({"type":"item_reference","id":"call_test"}));
        let (fixed, count) = repair_text(&original).unwrap().unwrap();
        assert_eq!(count, 1);
        let records: Vec<Value> = fixed.lines().map(|entry| serde_json::from_str(entry).unwrap()).collect();
        assert_eq!(records[0]["payload"]["id"], "fc_test");
        assert_eq!(records[0]["payload"]["call_id"], "call_test");
        assert_eq!(records[0]["payload"]["namespace"], "mcp__demo");
        assert_eq!(records[0]["payload"]["arguments"], "{\"id\":\"call_test\"}");
        assert_eq!(records[1]["payload"]["call_id"], "call_test");
        assert_eq!(records[1]["payload"]["output"], "call_test");
        assert_eq!(records[2]["payload"]["id"], "fc_test");
        assert_eq!(fixed.matches("\r\n").count(), 3);
        assert!(repair_text(&fixed).unwrap().is_none());
    }

    #[test]
    fn tool_id_repair_rejects_collisions_missing_links_and_broken_json() {
        let bad = line(json!({"type":"function_call","id":"call_test","call_id":"call_test"}));
        let existing = line(json!({"type":"function_call","id":"fc_test","call_id":"other"}));
        assert!(repair_text(&(bad.clone() + &existing)).is_err());
        assert!(repair_text(&line(json!({"type":"function_call","id":"call_test"}))).is_err());
        assert!(repair_text(&(bad + "{broken")).is_err());
    }

    #[test]
    fn tool_id_repair_refuses_ids_shared_by_different_calls() {
        let original = line(json!({"type":"function_call","id":"call_same","call_id":"first"}))
            + &line(json!({"type":"function_call","id":"call_same","call_id":"second"}));
        assert!(repair_text(&original).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn tool_id_repair_refuses_an_open_writer_without_backing_up_or_modifying() {
        let home = std::env::temp_dir().join(format!("synaroute-id-lock-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&home).unwrap();
        let path = home.join("rollout.jsonl");
        let backup = home.join("backup.jsonl");
        let original = line(json!({"type":"function_call","id":"call_busy","call_id":"call_busy"}));
        fs::write(&path, &original).unwrap();
        let writer = OpenOptions::new().write(true).open(&path).unwrap();
        assert!(repair_file(&path, &backup).is_err());
        assert!(!backup.exists());
        drop(writer);
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
        fs::remove_file(path).unwrap();
        fs::remove_dir(home).unwrap();
    }

    #[test]
    fn tool_id_repair_handles_compacted_history_without_touching_reasoning() {
        let original = json!({"type":"compacted","payload":{"replacement_history":[
            {"type":"custom_tool_call","id":"fc_patch","call_id":"call_patch","input":"patch"},
            {"type":"reasoning","id":"rs_original","encrypted_content":"opaque"}
        ]}}).to_string();
        let (fixed, _) = repair_text(&original).unwrap().unwrap();
        let record: Value = serde_json::from_str(&fixed).unwrap();
        assert_eq!(record["payload"]["replacement_history"][0]["id"], "ctc_patch");
        assert_eq!(record["payload"]["replacement_history"][1]["encrypted_content"], "opaque");
        assert!(!fixed.ends_with('\n'));
    }

    #[test]
    fn tool_id_repair_backs_up_exact_bytes_and_keeps_provider_roundtrips_independent() {
        let home = std::env::temp_dir().join(format!("synaroute-id-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&home).unwrap();
        let path = home.join("rollout.jsonl");
        let backup = home.join("backup.jsonl");
        let original = format!("{}\n", json!({"type":"session_meta","payload":{"model_provider":"other"}}))
            + &line(json!({"type":"function_call","id":"call_test","call_id":"call_test"}));
        fs::write(&path, &original).unwrap();
        let mtime = fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(repair_file(&path, &backup).unwrap(), 1);
        assert_eq!(fs::read_to_string(&backup).unwrap(), original);
        assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), mtime);
        for provider in ["openai", "other", "openai"] {
            super::super::rewrite_first_line(&path, Some(provider)).unwrap();
            assert_eq!(repair_file(&path, &backup).unwrap(), 0);
            let current = fs::read_to_string(&path).unwrap();
            let item: Value = serde_json::from_str(current.lines().nth(1).unwrap()).unwrap();
            assert_eq!(item["payload"]["id"], "fc_test");
            assert_eq!(item["payload"]["call_id"], "call_test");
        }
        fs::remove_file(path).unwrap();
        fs::remove_file(backup).unwrap();
        fs::remove_dir(home).unwrap();
    }
}
