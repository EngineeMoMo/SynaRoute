use super::{files, guard, ids};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::ipc::Channel;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub phase: String,
    pub completed: usize,
    pub total: usize,
}

pub(super) fn notify(channel: &Channel<Progress>, phase: &str, completed: usize, total: usize) {
    let _ = channel.send(Progress { phase: phase.into(), completed, total });
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepairFile {
    pub rel_path: String,
    pub ids: usize,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepairIssue {
    pub rel_path: String,
    pub message: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepairPreview {
    pub token: String,
    pub scanned: usize,
    pub files: Vec<RepairFile>,
    pub issues: Vec<RepairIssue>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepairResult {
    pub changed_files: usize,
    pub changed_ids: usize,
    pub remaining_files: usize,
    pub backup_dir: Option<String>,
    pub issues: Vec<RepairIssue>,
}

struct PlannedFile {
    rel_path: String,
    analysis: ids::Analysis,
}

struct RepairPlan {
    token: String,
    created: Instant,
    home: PathBuf,
    files: Vec<PlannedFile>,
}

static PREVIEW: Mutex<Option<RepairPlan>> = Mutex::new(None);

fn preview_at(home: &Path, progress: &mut dyn FnMut(&str, usize, usize)) -> Result<(RepairPreview, RepairPlan), String> {
    progress("scanning", 0, 0);
    let scan = super::scan_at(home);
    let child_ids = super::sqlite::collect_child_thread_ids(home);
    let sessions: Vec<_> = scan.sessions.into_iter().filter(|session| !super::session_is_internal(session, &child_ids)).collect();
    let mut preview = RepairPreview { token: uuid::Uuid::new_v4().to_string(), scanned: sessions.len(), files: Vec::new(), issues: Vec::new() };
    if scan.unreadable > 0 || scan.path_rejected > 0 {
        preview.issues.push(RepairIssue { rel_path: String::new(), message: format!("另有 {} 个不可读或不安全的会话路径未扫描", scan.unreadable + scan.path_rejected) });
    }
    let mut planned = Vec::new();
    for (index, session) in sessions.iter().enumerate() {
        progress("scanning", index, sessions.len());
        let analyzed = files::resolve_in_home(home, &session.rel_path)
            .ok_or_else(|| "会话路径越界".into()).and_then(|path| ids::analyze(&path));
        match analyzed {
            Ok(analysis) if !analysis.mapping.is_empty() => {
                preview.files.push(RepairFile { rel_path: session.rel_path.clone(), ids: analysis.mapping.len() });
                planned.push(PlannedFile { rel_path: session.rel_path.clone(), analysis });
            }
            Ok(_) => {}
            Err(message) => preview.issues.push(RepairIssue { rel_path: session.rel_path.clone(), message }),
        }
    }
    progress("complete", sessions.len(), sessions.len());
    let plan = RepairPlan { token: preview.token.clone(), created: Instant::now(), home: home.into(), files: planned };
    Ok((preview, plan))
}

fn apply_at(home: &Path, data_dir: &Path, plan: RepairPlan, check_write: &dyn Fn() -> Result<(), String>, progress: &mut dyn FnMut(&str, usize, usize)) -> Result<RepairResult, String> {
    if plan.home != home || plan.created.elapsed() > Duration::from_secs(600) {
        return Err("修复预览已过期，请重新扫描".into());
    }
    let total = plan.files.len();
    let mut paths = Vec::new();
    for (index, entry) in plan.files.iter().enumerate() {
        progress("validating", index, total);
        let path = files::resolve_in_home(home, &entry.rel_path).ok_or_else(|| "会话路径已变化，请重新扫描".to_string())?;
        if ids::analyze(&path)?.digest != entry.analysis.digest {
            return Err(format!("{} 在预览后发生变化，未执行任何修复；请重新扫描", entry.rel_path));
        }
        paths.push(path);
    }
    check_write()?;
    let backup = data_dir.join("codex-session-id-backups").join(uuid::Uuid::new_v4().to_string());
    let mut result = RepairResult { changed_files: 0, changed_ids: 0, remaining_files: total, backup_dir: None, issues: Vec::new() };
    for (index, (entry, path)) in plan.files.iter().zip(paths.iter()).enumerate() {
        progress("repairing", index, total);
        match check_write().and_then(|_| ids::apply(path, &backup.join(&entry.rel_path), &entry.analysis)) {
            Ok(count) => {
                result.changed_files += 1;
                result.changed_ids += count;
                result.remaining_files -= 1;
            }
            Err(message) => {
                result.issues.push(RepairIssue { rel_path: entry.rel_path.clone(), message });
                break;
            }
        }
    }
    if backup.exists() { result.backup_dir = Some(backup.display().to_string()); }
    progress(if result.issues.is_empty() { "complete" } else { "partial" }, result.changed_files, total);
    Ok(result)
}

#[tauri::command]
pub async fn preview_codex_session_id_repair(on_progress: Channel<Progress>) -> Result<RepairPreview, String> {
    let home = super::super::codex_paths::codex_home().map_err(|error| error.to_string())?;
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = guard::acquire(&home)?;
        let (preview, plan) = preview_at(&home, &mut |phase, completed, total| notify(&on_progress, phase, completed, total))?;
        *PREVIEW.lock().map_err(|_| "修复预览状态不可用，请重启应用")? = Some(plan);
        Ok(preview)
    }).await.map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn apply_codex_session_id_repair(token: String, on_progress: Channel<Progress>) -> Result<RepairResult, String> {
    let home = super::super::codex_paths::codex_home().map_err(|error| error.to_string())?;
    let data_dir = crate::store::data_dir::app_data_dir().map_err(|error| error.to_string())?;
    tauri::async_runtime::spawn_blocking(move || {
        guard::ensure_stopped()?;
        let _guard = guard::acquire(&home)?;
        let plan = {
            let mut cached = PREVIEW.lock().map_err(|_| "修复预览状态不可用，请重启应用")?;
            if cached.as_ref().map_or(true, |plan| plan.token != token) {
                return Err("修复预览已失效，请重新扫描".into());
            }
            cached.take().ok_or_else(|| "缺少修复预览".to_string())?
        };
        apply_at(&home, &data_dir, plan, &guard::ensure_stopped, &mut |phase, completed, total| notify(&on_progress, phase, completed, total))
    }).await.map_err(|error| error.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn maintenance_commands_are_registered() {
        let source = include_str!("../lib.rs");
        for command in ["preview_codex_session_id_repair", "apply_codex_session_id_repair"] {
            assert!(source.contains(&format!("codex_sessions::maintenance::{command}")));
        }
    }

    fn fixture() -> (PathBuf, PathBuf) {
        let home = std::env::temp_dir().join(format!("synaroute-preview-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(home.join("sessions")).unwrap();
        let path = home.join(format!("sessions/rollout-{}.jsonl", uuid::Uuid::new_v4()));
        fs::write(&path, "{\"type\":\"session_meta\",\"payload\":{\"id\":\"demo\",\"model_provider\":\"relay\"}}\n{\"type\":\"response_item\",\"payload\":{\"type\":\"function_call\",\"id\":\"call_demo\",\"call_id\":\"call_demo\"}}\n").unwrap();
        (home, path)
    }

    #[test]
    fn preview_is_read_only_and_apply_does_not_change_provider() {
        let (home, path) = fixture();
        let original = fs::read(&path).unwrap();
        let (preview, plan) = preview_at(&home, &mut |_, _, _| {}).unwrap();
        assert_eq!(preview.files.len(), 1);
        assert_eq!(fs::read(&path).unwrap(), original);
        assert!(!home.join("data").exists());
        let result = apply_at(&home, &home.join("data"), plan, &|| Ok(()), &mut |_, _, _| {}).unwrap();
        assert_eq!(result.changed_ids, 1);
        assert_eq!(result.remaining_files, 0);
        let repaired = fs::read_to_string(&path).unwrap();
        assert!(repaired.contains("\"model_provider\":\"relay\""));
        assert!(repaired.contains("\"call_id\":\"call_demo\""));
        assert!(repaired.contains("fc_demo"));
        assert!(preview_at(&home, &mut |_, _, _| {}).unwrap().0.files.is_empty());
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn stale_preview_refuses_all_writes_including_same_length_changes() {
        let (home, path) = fixture();
        let (_, plan) = preview_at(&home, &mut |_, _, _| {}).unwrap();
        let changed = fs::read_to_string(&path).unwrap().replace("call_demo", "call_newx");
        fs::write(&path, &changed).unwrap();
        assert!(apply_at(&home, &home.join("data"), plan, &|| Ok(()), &mut |_, _, _| {}).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), changed);
        assert!(!home.join("data").exists());
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn expired_preview_refuses_writes() {
        let (home, _) = fixture();
        let (_, mut plan) = preview_at(&home, &mut |_, _, _| {}).unwrap();
        plan.created = Instant::now() - Duration::from_secs(601);
        assert!(apply_at(&home, &home.join("data"), plan, &|| Ok(()), &mut |_, _, _| {}).is_err());
        assert!(!home.join("data").exists());
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn a_late_failure_reports_partial_work_without_overwriting_new_content() {
        let (home, first) = fixture();
        let second = home.join(format!("sessions/rollout-{}.jsonl", uuid::Uuid::new_v4()));
        fs::copy(&first, &second).unwrap();
        let (_, plan) = preview_at(&home, &mut |_, _, _| {}).unwrap();
        assert_eq!(plan.files.len(), 2);
        let late_path = home.join(&plan.files[1].rel_path);
        let changed = fs::read_to_string(&late_path).unwrap().replace("call_demo", "call_late");
        let result = apply_at(&home, &home.join("data"), plan, &|| Ok(()), &mut |phase, completed, _| {
            if phase == "repairing" && completed == 1 { fs::write(&late_path, &changed).unwrap(); }
        }).unwrap();
        assert_eq!(result.changed_files, 1);
        assert_eq!(result.remaining_files, 1);
        assert_eq!(result.issues.len(), 1);
        assert!(result.backup_dir.is_some());
        assert_eq!(fs::read_to_string(late_path).unwrap(), changed);
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn a_client_started_after_preview_prevents_writes() {
        let (home, path) = fixture();
        let original = fs::read(&path).unwrap();
        let (_, plan) = preview_at(&home, &mut |_, _, _| {}).unwrap();
        assert!(apply_at(&home, &home.join("data"), plan, &|| Err("client running".into()), &mut |_, _, _| {}).is_err());
        assert_eq!(fs::read(path).unwrap(), original);
        assert!(!home.join("data").exists());
        fs::remove_dir_all(home).unwrap();
    }
}
