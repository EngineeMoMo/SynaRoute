use super::files;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;

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

fn repair_text(original: &str) -> Result<Option<(String, usize)>, String> {
    let lines: Vec<&str> = original.split_inclusive('\n').collect();
    let mut records = lines.iter().map(|line| {
        if line.trim().is_empty() { Ok(Value::Null) }
        else { serde_json::from_str::<Value>(line).map_err(|error| error.to_string()) }
    }).collect::<Result<Vec<_>, _>>()?;
    let mut existing = HashSet::new();
    let mut mapping = HashMap::new();
    let mut destinations = HashSet::new();
    let mut owners = HashMap::new();
    let mut ambiguous = HashSet::new();
    for record in &mut records {
        for item in items(record) {
            let kind = item.get("type").and_then(Value::as_str).unwrap_or("");
            let Some(id) = item.get("id").and_then(Value::as_str) else { continue };
            if kind != "item_reference" {
                existing.insert(id.to_string());
                let owner = (kind.to_string(), item.get("call_id").cloned());
                if owners.insert(id.to_string(), owner.clone()).is_some_and(|previous| previous != owner) {
                    ambiguous.insert(id.to_string());
                }
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
    if mapping.is_empty() { return Ok(None); }
    if mapping.keys().any(|id| ambiguous.contains(id)) {
        return Err("异常 ID 被不同条目共用，不能安全修复".into());
    }
    if destinations.iter().any(|id| existing.contains(id)) {
        return Err("修复后的工具 ID 与已有条目冲突，未修改文件".into());
    }
    let mut repaired = String::with_capacity(original.len());
    for (line, record) in lines.iter().zip(records.iter_mut()) {
        let mut changed = false;
        for item in items(record) {
            for field in ["id", "item_id"] {
                if let Some(next) = item.get(field).and_then(Value::as_str).and_then(|id| mapping.get(id)) {
                    item[field] = Value::String(next.clone());
                    changed = true;
                }
            }
        }
        if changed {
            repaired.push_str(&serde_json::to_string(record).map_err(|error| error.to_string())?);
            repaired.push_str(files::split_eol(line).1);
        } else {
            repaired.push_str(line);
        }
    }
    Ok(Some((repaired, mapping.len())))
}

fn repair_file(path: &Path, backup: &Path) -> Result<usize, String> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(4);
    }
    let mut source = options.open(path).map_err(|error| format!("请先退出 Codex 再同步：{error}"))?;
    let metadata = source.metadata().map_err(|error| error.to_string())?;
    if metadata.len() > 128 * 1024 * 1024 {
        return Err("会话超过 128 MiB，未自动修复，请单独处理".into());
    }
    let mut original = String::new();
    source.read_to_string(&mut original).map_err(|error| error.to_string())?;
    let Some((repaired, count)) = repair_text(&original)? else { return Ok(0) };
    if let Some(parent) = backup.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let mut saved = OpenOptions::new().write(true).create_new(true).open(backup)
        .map_err(|error| error.to_string())?;
    saved.write_all(original.as_bytes()).and_then(|_| saved.sync_all())
        .map_err(|error| error.to_string())?;
    let temporary = files::tmp_path_for(path);
    let result = (|| -> std::io::Result<()> {
        let mut output = OpenOptions::new().write(true).create_new(true).open(&temporary)?;
        output.write_all(repaired.as_bytes())?;
        if let Ok(modified) = metadata.modified() {
            output.set_times(fs::FileTimes::new().set_modified(modified))?;
        }
        output.sync_all()?;
        drop(output);
        if source.metadata()?.len() != metadata.len() || source.metadata()?.modified()? != metadata.modified()? {
            return Err(std::io::Error::other("会话在修复期间发生变化，请退出 Codex 后重试"));
        }
        fs::rename(&temporary, path)
    })();
    if result.is_err() { let _ = fs::remove_file(&temporary); }
    result.map_err(|error| format!("{error}；原文备份：{}", backup.display()))?;
    Ok(count)
}

pub(super) fn repair_at(home: &Path, data_dir: &Path) -> Result<String, String> {
    let backup_root = data_dir.join("codex-session-id-backups").join(uuid::Uuid::new_v4().to_string());
    let child_ids = super::sqlite::collect_child_thread_ids(home);
    let mut count = 0;
    for session in super::scan_at(home).sessions {
        if super::session_is_internal(&session, &child_ids) { continue; }
        let path = files::resolve_in_home(home, &session.rel_path)
            .ok_or_else(|| "会话路径越界，未同步".to_string())?;
        count += repair_file(&path, &backup_root.join(&session.rel_path))
            .map_err(|error| format!("{}：{error}。尚未切换 provider；此前已修复 {count} 个 ID，备份目录：{}", path.display(), backup_root.display()))?;
    }
    Ok(if count == 0 { String::new() } else {
        format!("；已修复 {count} 个历史工具 ID（不改 call_id/工具命名空间）。独立原文备份：{}。请重新打开 Codex 会话；切回 provider 不撤销此修复", backup_root.display())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
