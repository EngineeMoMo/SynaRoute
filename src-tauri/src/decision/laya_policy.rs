//! Bounded setup budgets and actionable diagnostics, shared by monitoring and tests.
pub(super) fn budget(phase: &str) -> u64 {
    match phase {
        "python" => 900,
        "dependencies" => 1800,
        "model" => 1200,
        "degraded" => 60,
        "running" => u64::MAX,
        _ => 120,
    }
}
pub(super) fn classify(lines: &[String]) -> &'static str {
    let log = lines.join("\n").to_lowercase();
    if log.contains("[locked]") {
        "locked"
    } else if log.contains("[disk]")
        || log.contains("no space left")
        || log.contains("disk full")
        || log.contains("磁盘空间")
    {
        "disk"
    } else if log.contains("[permission]")
        || log.contains("access is denied")
        || log.contains("permission denied")
        || log.contains("拒绝访问")
    {
        "permission"
    } else if log.contains("[python]") || log.contains("winget") {
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
    }
}
