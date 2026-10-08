//! Read-only discovery. Never run shell wrappers or infer a writable root from app cwd.
use crate::model::CategoryType;
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Defaults {
    runtime: &'static str,
    executable: Option<String>,
    cwd: Option<String>,
}

fn runtime(category: CategoryType) -> &'static str {
    match category {
        CategoryType::Codex => "codex",
        CategoryType::ClaudeCli | CategoryType::ClaudeDesktop => "claude",
    }
}

fn candidates(
    name: &str,
    path_dirs: &[PathBuf],
    home: Option<&Path>,
    local: Option<&Path>,
) -> Vec<PathBuf> {
    let filename = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.into()
    };
    let mut paths = Vec::new();
    for dir in path_dirs.iter().filter(|p| p.is_absolute()) {
        paths.push(dir.join(&filename));
        if name == "claude" {
            paths.push(
                dir.join("node_modules/@anthropic-ai/claude-code/bin")
                    .join(&filename),
            );
        } else {
            let package = dir.join("node_modules/@openai/codex");
            collect(&package, &filename, 7, &mut 512, &mut paths);
        }
    }
    if let Some(home) = home {
        paths.push(home.join(".local/bin").join(&filename));
        paths.push(home.join(".cargo/bin").join(&filename));
        paths.push(home.join(".npm-global/bin").join(&filename));
    }
    if name == "codex" {
        if let Some(local) = local {
            collect(
                &local.join("OpenAI/Codex/bin"),
                &filename,
                3,
                &mut 256,
                &mut paths,
            );
        }
        #[cfg(target_os = "macos")]
        paths.push(PathBuf::from(
            "/Applications/Codex.app/Contents/Resources/codex",
        ));
    }
    paths
}

fn collect(
    root: &Path,
    filename: &str,
    depth: usize,
    budget: &mut usize,
    paths: &mut Vec<PathBuf>,
) {
    if depth == 0 || *budget == 0 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    let mut entries: Vec<_> = entries.take(*budget).filter_map(Result::ok).collect();
    *budget = budget.saturating_sub(entries.len());
    // Prefer the newest installed version when desktop updates leave older binaries behind.
    entries
        .sort_by_key(|entry| std::cmp::Reverse(entry.metadata().and_then(|m| m.modified()).ok()));
    for entry in entries {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_file() && entry.file_name() == filename {
            paths.push(entry.path());
        } else if kind.is_dir() {
            collect(&entry.path(), filename, depth - 1, budget, paths);
        }
    }
}

fn executable(name: &str) -> Option<String> {
    let dirs: Vec<_> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    let home = dirs::home_dir();
    let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    candidates(name, &dirs, home.as_deref(), local.as_deref())
        .into_iter()
        .find(|p| p.is_absolute() && p.is_file())
        .map(|p| p.to_string_lossy().into_owned())
}

#[tauri::command]
pub async fn native_defaults(
    state: tauri::State<'_, crate::AppState>,
    category: CategoryType,
    work_dir: Option<String>,
    auto_follow_active: bool,
) -> Result<Defaults, String> {
    let mut brain = state.store.get_brain(category);
    // Use the currently displayed category configuration, including unsaved directory edits.
    brain.work_dir = work_dir;
    brain.auto_follow_active = auto_follow_active;
    tokio::task::spawn_blocking(move || {
        let runtime = runtime(category);
        let cwd = crate::aggregate::write::resolve_writable_work_dir(&brain)
            .filter(|p| Path::new(p).is_absolute() && Path::new(p).is_dir());
        Defaults {
            runtime,
            executable: executable(runtime),
            cwd,
        }
    })
    .await
    .map_err(|_| "自动检测失败 / discovery failed".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "manual read-only check on a machine with both native clients installed"]
    fn installed_native_clients() {
        for name in ["codex", "claude"] {
            let found = executable(name).expect("installed native executable not found");
            println!("{name}: {found}");
        }
    }
    #[test]
    fn category_selects_protocol_client() {
        assert_eq!(runtime(CategoryType::Codex), "codex");
        assert_eq!(runtime(CategoryType::ClaudeCli), "claude");
        assert_eq!(runtime(CategoryType::ClaudeDesktop), "claude");
    }
    #[test]
    fn resolves_npm_native_binary_without_using_wrapper() {
        let root = std::env::temp_dir().join(format!("native-discovery-{}", uuid::Uuid::new_v4()));
        let folder = root.join("node_modules/@anthropic-ai/claude-code/bin");
        std::fs::create_dir_all(&folder).unwrap();
        let filename = if cfg!(windows) {
            "claude.exe"
        } else {
            "claude"
        };
        let native = folder.join(filename);
        std::fs::write(&native, []).unwrap();
        std::fs::write(root.join("claude.cmd"), []).unwrap();
        let found = candidates("claude", std::slice::from_ref(&root), None, None)
            .into_iter()
            .find(|p| p.is_file());
        assert_eq!(found, Some(native));
        // Relative PATH entries must add nothing; macOS still has a built-in app candidate.
        assert_eq!(
            candidates("codex", &[PathBuf::from("relative")], None, None),
            candidates("codex", &[], None, None)
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
