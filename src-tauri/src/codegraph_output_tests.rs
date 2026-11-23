use super::*;
use std::io::Write;

fn fixture(mode: &str) -> Command {
    let mut command = hidden(std::env::current_exe().unwrap());
    command.args(["--exact", "codegraph::output_tests::output_fixture", "--nocapture"]);
    command.env("SYNAROUTE_CODEGRAPH_OUTPUT_FIXTURE", mode);
    command
}

#[test]
#[allow(clippy::zombie_processes)]
fn output_fixture() {
    match std::env::var("SYNAROUTE_CODEGRAPH_OUTPUT_FIXTURE").as_deref() {
        Ok("stdout") => { let _ = io::stdout().write_all(&vec![b'x'; 5 * 1024 * 1024]); }
        Ok("stderr") => { let _ = io::stderr().write_all(&vec![b'x'; 5 * 1024 * 1024]); }
        Ok("inherited_pipe") => {
            let mut command = fixture("wait");
            command.stdout(Stdio::inherit()).stderr(Stdio::inherit());
            command.as_std_mut().spawn().unwrap();
        }
        Ok("wait") => std::thread::sleep(Duration::from_secs(2)),
        Ok("tree_wait" | "tree_success") => {
            let heartbeat = std::env::var("SYNAROUTE_CODEGRAPH_HEARTBEAT").unwrap();
            let mut command = fixture("tree_child");
            command.stdout(Stdio::null()).stderr(Stdio::null());
            command.as_std_mut().spawn().unwrap();
            for _ in 0..200 {
                if std::path::Path::new(&heartbeat).exists() { break; }
                std::thread::sleep(Duration::from_millis(10));
            }
            if std::env::var("SYNAROUTE_CODEGRAPH_OUTPUT_FIXTURE").unwrap() == "tree_wait" { std::thread::sleep(Duration::from_secs(20)); }
        }
        Ok("tree_child") => {
            let heartbeat = std::env::var("SYNAROUTE_CODEGRAPH_HEARTBEAT").unwrap();
            for tick in 0..1000 {
                std::fs::write(&heartbeat, tick.to_string()).unwrap();
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        _ => {}
    }
}

#[tokio::test]
async fn subprocess_output_is_bounded_on_both_pipes() {
    for mode in ["stdout", "stderr"] {
        let error = timeout_output_with_limit(&mut fixture(mode), Duration::from_secs(5)).await.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }
}

#[tokio::test]
async fn inherited_pipes_close_with_the_process_tree_after_parent_exit() {
    let started = std::time::Instant::now();
    let output = timeout_output_with_limit(&mut fixture("inherited_pipe"), Duration::from_secs(3)).await.unwrap();
    assert!(output.status.success());
    assert!(started.elapsed() < Duration::from_millis(1500));
}

#[tokio::test]
async fn descendants_stop_after_timeout_cancellation_and_parent_success() {
    for ending in ["timeout", "cancel", "success"] {
        let directory = tempfile::tempdir().unwrap();
        let heartbeat = directory.path().join("heartbeat.txt");
        let mut command = fixture(if ending == "success" { "tree_success" } else { "tree_wait" });
        command.env("SYNAROUTE_CODEGRAPH_HEARTBEAT", &heartbeat);
        let limit = if ending == "timeout" { Duration::from_secs(1) } else { Duration::from_secs(10) };
        let worker = tokio::spawn(async move { timeout_output_with_limit(&mut command, limit).await });
        tokio::time::timeout(Duration::from_secs(5), async {
            while !heartbeat.exists() { tokio::time::sleep(Duration::from_millis(10)).await; }
        }).await.expect("descendant must start before cleanup is exercised");
        if ending == "cancel" { worker.abort(); assert!(worker.await.unwrap_err().is_cancelled()); }
        else {
            let result = worker.await.unwrap();
            if ending == "timeout" { assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut); }
            else { assert!(result.unwrap().status.success()); }
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
        let stopped = std::fs::read(&heartbeat).unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(stopped, std::fs::read(&heartbeat).unwrap(), "descendant survived {ending}");
    }
}

#[test]
fn codegraph_metadata_uses_read_file_path_policy() {
    let directory = tempfile::tempdir().unwrap();
    for file in ["valid.rs", ".env", "credentials.json", "notes.md"] { std::fs::write(directory.path().join(file), "fixture").unwrap(); }
    for file in [".env", "credentials.json", "../outside.rs", ".git/config", "missing.rs"] {
        assert!(crate::agent_tools::resolve_readable(directory.path(), file).is_err(), "{file}");
    }
    assert!(crate::agent_tools::resolve_readable(directory.path(), "valid.rs").is_ok());
}

fn json_cli(directory: &Path, output: &str) -> String {
    #[cfg(windows)]
    let (name, script) = ("fixture.cmd", format!("@echo off\r\necho {output}\r\n"));
    #[cfg(not(windows))]
    let (name, script) = ("fixture.sh", format!("#!/bin/sh\nprintf '%s\\n' '{output}'\n"));
    let path = directory.join(name);
    std::fs::write(&path, script).unwrap();
    #[cfg(unix)] {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    path.to_string_lossy().into_owned()
}

#[tokio::test]
async fn cli_metadata_filters_sensitive_and_escaping_paths_before_returning() {
    let directory = tempfile::tempdir().unwrap();
    let project = directory.path().join("project");
    std::fs::create_dir(&project).unwrap();
    for file in ["valid.rs", ".env", "credentials.json"] { std::fs::write(project.join(file), "fixture").unwrap(); }
    std::fs::write(directory.path().join("outside.rs"), "fixture").unwrap();
    let nodes: Vec<serde_json::Value> = [".env", "credentials.json", "../outside.rs", "missing.rs", "valid.rs"].into_iter()
        .map(|path| serde_json::json!({"kind":"function","name":"fixture","filePath":path,"startLine":1})).collect();
    let hits: Vec<_> = nodes.iter().map(|node| serde_json::json!({"node":node})).collect();
    let program = json_cli(directory.path(), &serde_json::to_string(&hits).unwrap());
    let symbols = query_symbols(&program, &project, "fixture", 1).await.unwrap();
    assert_eq!(symbols.len(), 1);
    assert_eq!(symbols[0].file_path, "valid.rs");
    let related = serde_json::json!({"affected":nodes,"callers":nodes,"callees":nodes});
    let program = json_cli(directory.path(), &related.to_string());
    for result in [impact(&program, &project, "fixture", 2).await.unwrap(), callers(&program, &project, "fixture", 1).await.unwrap(), callees(&program, &project, "fixture", 1).await.unwrap()] {
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].file_path, "valid.rs");
    }
    let program = json_cli(directory.path(), "sensitive-fixture-output");
    let error = query_symbols(&program, &project, "fixture", 1).await.unwrap_err();
    assert!(!error.contains("sensitive-fixture-output"));
    assert!(error.contains("JSON"));
}
