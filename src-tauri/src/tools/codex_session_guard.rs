use std::fs::{self, File, OpenOptions};
use std::path::Path;

pub(super) struct MaintenanceGuard {
    _file: File,
}

pub(super) fn acquire(home: &Path) -> Result<MaintenanceGuard, String> {
    let directory = home.join("tmp");
    fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    let path = directory.join("synaroute-session-maintenance.lock");
    if fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err("会话维护锁路径不能是符号链接".into());
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(0);
    }
    let file = options.open(&path)
        .map_err(|error| format!("会话维护正在其他进程执行，或锁文件不可访问：{error}"))?;
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(format!("会话维护锁不可用：{}", std::io::Error::last_os_error()));
        }
    }
    Ok(MaintenanceGuard { _file: file })
}

fn is_codex_process(line: &str) -> bool {
    let name = line.trim().trim_start_matches('"').split('"').next().unwrap_or("");
    let name = name.rsplit(['/', '\\']).next().unwrap_or(name).to_ascii_lowercase();
    matches!(name.as_str(), "codex" | "codex.exe" | "codex-cli" | "codex-cli.exe" | "chatgpt" | "chatgpt.exe")
}

pub(super) fn ensure_stopped() -> Result<(), String> {
    #[cfg(windows)]
    let result = crate::proc::hidden("tasklist").args(["/FO", "CSV", "/NH"]).as_std_mut().output();
    #[cfg(not(windows))]
    let result = crate::proc::hidden("ps").args(["-A", "-o", "comm="]).as_std_mut().output();
    let output = result.map_err(|error| format!("无法检查 Codex 运行状态，未执行写入：{error}"))?;
    if !output.status.success() {
        return Err("进程检查失败，未执行写入；请退出 Codex/ChatGPT 后重试".into());
    }
    if String::from_utf8_lossy(&output.stdout).lines().any(is_codex_process) {
        return Err("Codex/ChatGPT 仍在运行，请完全退出（包括 CLI 和后台进程）后再执行会话维护".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maintenance_lock_child_probe() {
        let Some(home) = std::env::var_os("SYNAROUTE_MAINTENANCE_LOCK_TEST_HOME") else { return };
        assert!(acquire(Path::new(&home)).is_err());
    }

    #[test]
    fn maintenance_lock_also_blocks_a_separate_process() {
        let home = std::env::temp_dir().join(format!("synaroute-process-lock-{}", uuid::Uuid::new_v4()));
        let lock = acquire(&home).unwrap();
        let output = crate::proc::hidden(std::env::current_exe().unwrap())
            .args(["--exact", "tools::codex::codex_sessions::guard::tests::maintenance_lock_child_probe", "--nocapture"])
            .env("SYNAROUTE_MAINTENANCE_LOCK_TEST_HOME", &home).as_std_mut().output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stdout));
        drop(lock);
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn maintenance_lock_is_exclusive_and_released_without_deleting_the_file() {
        let home = std::env::temp_dir().join(format!("synaroute-guard-{}", uuid::Uuid::new_v4()));
        let first = acquire(&home).unwrap();
        assert!(acquire(&home).is_err());
        drop(first);
        assert!(home.join("tmp/synaroute-session-maintenance.lock").exists());
        assert!(acquire(&home).is_ok());
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn process_detection_accepts_csv_and_unix_paths_without_matching_other_apps() {
        for name in ["\"Codex.exe\",\"123\",\"Console\"", "/Applications/Codex.app/Contents/MacOS/Codex", "codex-cli", "ChatGPT.exe"] {
            assert!(is_codex_process(name), "{name}");
        }
        for name in ["SynaRoute.exe", "codex-plus-manager.exe", "node", "codex-helper"] {
            assert!(!is_codex_process(name), "{name}");
        }
    }
}
