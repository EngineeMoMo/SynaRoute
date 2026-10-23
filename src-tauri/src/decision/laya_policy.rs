//! Bounded setup budgets and actionable diagnostics, shared by monitoring and tests.
pub(super) fn budget(phase: &str) -> u64 {
    match phase {
        "python" => 900,
        "dependencies" => 1800,
        "model" => 1200,
        "degraded" => 60,
        "running" => u64::MAX,
        // A stop waiting for the Python installer to finish; past this it is forced.
        "stopping" => 600,
        _ => 120,
    }
}
pub(super) fn classify(lines: &[String]) -> &'static str {
    // Explicit installer failures take precedence over informational output.
    if let Some((code, _)) = explicit_failure(lines) {
        return code;
    }
    let log = lines.join("\n").to_lowercase();
    // laya's own refusal of a pinned file whose digest differs: a corrupted cache or a mirror that
    // served other bytes. It must not read as "network" or a generic exit.
    if log.contains("sha-256 mismatch") {
        return "integrity";
    }
    if log.contains("[locked]") {
        "locked"
    } else if log.contains("[disk]")
        || log.contains("no space left")
        || log.contains("disk full")
        || log.contains("磁盘空间不足")
        || log.contains("磁盘已满")
    {
        "disk"
    } else if log.contains("[permission]")
        || log.contains("access is denied")
        || log.contains("permission denied")
        || log.contains("拒绝访问")
    {
        "permission"
    } else if log.contains("[python]") {
        "python"
    } else if log.contains("address already in use") || log.contains("10048") {
        "port"
    } else if log.contains("out of memory")
        || log.contains("memoryerror")
        || log.contains("paging file")
        || log.contains("页面文件")
    {
        "memory"
    } else if log.contains("proxy")
        || log.contains("ssl")
        || log.contains("certificate")
        || log.contains("connection")
        || log.contains("timed out")
        || log.contains("resolve")
    {
        "network"
    } else if log.contains("[environment]")
        || log.contains("modulenotfound")
        || log.contains("importerror")
        || log.contains("dll load failed")
    {
        "environment"
    } else {
        "process"
    }
}
pub(super) fn explicit_failure(lines: &[String]) -> Option<(&'static str, &str)> {
    for line in lines.iter().rev() {
        // Every tag start.ps1 throws must be listed, or that failure falls back to substring guessing.
        for code in ["disk", "permission", "python", "locked", "environment", "network", "port", "install", "platform"] {
            if line.trim_start().starts_with(&format!("[{code}]")) {
                return Some((code, line.as_str()));
            }
        }
    }
    None
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failures_are_actionable() {
        for (line, expected) in [
            ("[locked] busy", "locked"),
            ("No space left on device", "disk"),
            ("Permission denied", "permission"),
            ("[python] missing", "python"),
            ("WinError 10048", "port"),
            ("MemoryError", "memory"),
            ("ProxyError certificate", "network"),
            ("ModuleNotFoundError", "environment"),
            ("exit 1", "process"),
            ("[platform] x86", "platform"),
            // laya's real message (verified by running laya-serve with a wrong pinned digest).
            ("ValueError: laya: SHA-256 mismatch for encoder/config.json: expected 0000, got 83f6. The artifact does not match the reviewed digest; refusing to load it.", "integrity"),
        ] {
            assert_eq!(classify(&[line.into()]), expected);
        }
    }
    #[test]
    fn startup_and_unhealthy_runtime_are_bounded() {
        assert_eq!(budget("checking"), 120);
        assert_eq!(budget("python"), 900);
        assert_eq!(budget("dependencies"), 1800);
        assert_eq!(budget("model"), 1200);
        assert_eq!(budget("degraded"), 60);
        assert_eq!(budget("running"), u64::MAX);
        assert_eq!(budget("stopping"), 600, "a deferred stop must be forced eventually");
    }
    /// The lock is the environment Test-Laya accepts; a version bump in one place only would build an
    /// environment the script then rejects, or skip one it should rebuild.
    #[test]
    fn the_dependency_lock_pins_the_laya_version_the_script_checks() {
        let lock = include_str!("../../../scripts/laya/requirements.lock.txt");
        let script = include_str!("../../../scripts/laya/start.ps1");
        let pinned = lock.lines().find_map(|l| l.strip_prefix("laya==")).and_then(|r| r.split_whitespace().next()).expect("lock must pin laya");
        assert!(script.contains(&format!("m.version('laya') == '{pinned}'")), "start.ps1 must check laya {pinned}");
        let pins = lock.lines().filter(|l| l.contains("==")).count();
        assert_eq!(pins, lock.matches("--hash=sha256:").count(), "every pin needs exactly one hash");
        assert!(pins >= 40, "lock looks truncated: {pins} pins");
        assert!(lock.lines().any(|l| l.starts_with("torch==") && l.contains("+cpu")), "torch must be the CPU build");
    }
    /// Informational output stays in the 40-line log, so a keyword in it misroutes every later
    /// failure (the "无需安装 winget" line turned a pip ProxyError into "python"). Checked against
    /// the real script so a new progress message cannot reintroduce it.
    #[test]
    fn installer_progress_lines_never_look_like_failures() {
        let script = include_str!("../../../scripts/laya/start.ps1");
        let mut checked = 0;
        for line in script.lines() {
            for cmdlet in ["Write-Host ", "Write-Output "] {
                for (at, _) in line.match_indices(cmdlet) {
                    let rest = line[at + cmdlet.len()..].trim_start();
                    let Some(quote) = rest.chars().next().filter(|c| matches!(c, '\'' | '"')) else { continue };
                    let Some(end) = rest[1..].find(quote) else { continue };
                    let text = &rest[1..1 + end];
                    // Protocol markers (stage, critical section) never reach the visible log.
                    if text.starts_with("SYNAROUTE_") {
                        continue;
                    }
                    checked += 1;
                    assert_eq!(classify(&[text.into()]), "process", "progress line classified as a failure: {text}");
                }
            }
        }
        assert!(checked >= 12, "extractor found only {checked} progress lines");
        let mut tags = 0;
        for (at, _) in script.match_indices("throw ") {
            let rest = script[at + 6..].trim_start_matches(['(', '\'', '"']);
            let Some(tag) = rest.strip_prefix('[').and_then(|r| r.split(']').next()) else { continue };
            if !tag.is_empty() && tag.chars().all(|c| c.is_ascii_lowercase()) {
                tags += 1;
                assert!(explicit_failure(&[format!("[{tag}] x")]).is_some(), "start.ps1 throws [{tag}] but it is not an explicit code");
            }
        }
        assert!(tags >= 7, "found only {tags} tagged throws");
    }
    #[test]
    fn startup_notice_must_not_mask_the_actual_failure() {
        let banner = "首次运行会联网安装依赖与下载模型，需要磁盘空间；模型占用以实际运行结果为准。";
        assert_eq!(classify(&[banner.into(), "exit 1".into()]), "process");
        for (failure, expected) in [
            ("[python] 请先安装 Python 3.12；winget 不可用", "python"),
            ("ProxyError: connection failed", "network"),
            ("[permission] directory is not writable", "permission"),
            ("[disk] 可用磁盘空间不足", "disk"),
            ("[environment] import failed", "environment"),
        ] {
            assert_eq!(classify(&[banner.into(), failure.into()]), expected);
        }
        let lines = vec![
            "No space left in an earlier attempt".into(),
            "[python] missing runtime".into(),
        ];
        assert_eq!(classify(&lines), "python");
        assert_eq!(
            explicit_failure(&lines).unwrap().1,
            "[python] missing runtime"
        );
    }
}
