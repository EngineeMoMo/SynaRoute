use super::{policy, Status};
use parking_lot::RwLock;
use std::{
    path::PathBuf,
    process::Stdio,
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Child,
    sync::Mutex,
};
use windows::Win32::{
    Foundation::{CloseHandle, HANDLE},
    System::{
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
            SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        },
        Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE},
    },
};

struct Job(isize);
impl Job {
    fn attach(pid: u32) -> Result<Self, String> {
        // The PowerShell script waits for stdin before it can start any child process.
        unsafe {
            let job = CreateJobObjectW(None, None).map_err(|e| e.to_string())?;
            let owned = Self(job.0 as isize);
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const _,
                std::mem::size_of_val(&info) as u32,
            )
            .map_err(|e| e.to_string())?;
            let process = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, false, pid)
                .map_err(|e| e.to_string())?;
            let result = AssignProcessToJobObject(job, process);
            let _ = CloseHandle(process);
            result.map_err(|e| e.to_string())?;
            Ok(owned)
        }
    }
}
impl Drop for Job {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(HANDLE(self.0 as *mut _));
        }
    }
}
struct Running {
    id: String,
    child: Child,
    _job: Job,
    view: Arc<RwLock<Status>>,
}
fn running() -> &'static Mutex<Option<Running>> {
    static VALUE: OnceLock<Mutex<Option<Running>>> = OnceLock::new();
    VALUE.get_or_init(Default::default)
}
fn state_file() -> Option<PathBuf> {
    dirs::data_local_dir().map(|p| p.join("SynaRoute/laya/last-status.json"))
}
#[cfg(not(test))]
fn persist(state: &Status) {
    if let (Some(path), Ok(data)) = (state_file(), serde_json::to_vec(state)) {
        let _ = std::fs::write(path, data);
    }
}
#[cfg(test)]
fn persist(_state: &Status) {}
fn recovered() -> Status {
    let mut state = state_file()
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|data| serde_json::from_slice::<Status>(&data).ok())
        .unwrap_or_default();
    if !matches!(state.phase.as_str(), "idle" | "stopped" | "failed") {
        state.phase = "stopped".into();
        state.error_code = Some("interrupted".into());
        state.error = Some("上次任务意外中断，可重试；已验证环境会复用 / Previous task was interrupted; retry to reuse the validated environment.".into());
    }
    state
}
fn last() -> &'static RwLock<Arc<RwLock<Status>>> {
    static VALUE: OnceLock<RwLock<Arc<RwLock<Status>>>> = OnceLock::new();
    VALUE.get_or_init(|| RwLock::new(Arc::new(RwLock::new(recovered()))))
}
pub(super) fn status() -> Status {
    last().read().read().clone()
}

fn log_line(view: &RwLock<Status>, raw: &str) {
    let mut state = view.write();
    if let Some(stage) = raw.trim().strip_prefix("SYNAROUTE_STAGE:") {
        if !matches!(state.phase.as_str(), "stopped" | "failed")
            && matches!(stage, "checking" | "python" | "dependencies" | "model")
        {
            state.phase = stage.into();
        }
    } else {
        // Pip can echo authenticated indexes or signed URLs. Keep those out of the UI log.
        let line = raw
            .split_whitespace()
            .map(|part| {
                if part.contains("http://") || part.contains("https://") {
                    "[download URL]"
                } else {
                    part
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        let line: String = line
            .chars()
            .filter(|c| !c.is_control() || *c == '\t')
            .take(1200)
            .collect();
        if !line.trim().is_empty() {
            state.logs.push(line);
        }
        if state.logs.len() > 40 {
            state.logs.remove(0);
        }
    }
}
async fn drain(mut reader: impl AsyncRead + Unpin, view: Arc<RwLock<Status>>) {
    let mut chunk = [0; 4096];
    let mut line = Vec::new();
    while let Ok(n) = reader.read(&mut chunk).await {
        if n == 0 {
            break;
        }
        for &byte in &chunk[..n] {
            if matches!(byte, b'\n' | b'\r') {
                log_line(&view, &String::from_utf8_lossy(&line));
                line.clear();
            } else if line.len() < 8192 {
                line.push(byte);
            }
        }
    }
    if !line.is_empty() {
        log_line(&view, &String::from_utf8_lossy(&line));
    }
}

pub(super) async fn start(repair: bool) -> Result<Status, String> {
    let mut slot = running().lock().await;
    let result = start_inner(repair, &mut slot).await;
    if let Err(error) = &result {
        let view = last().read().clone();
        let mut state = view.write();
        state.phase = "failed".into();
        state.error = Some(error.clone());
        state.error_code = Some("setup".into());
        persist(&state);
    }
    result
}
async fn start_inner(repair: bool, slot: &mut Option<Running>) -> Result<Status, String> {
    if let Some(run) = slot.as_ref() {
        return Ok(run.view.read().clone());
    }
    let install = dirs::data_local_dir()
        .ok_or("找不到本地数据目录 / local data directory unavailable")?
        .join("SynaRoute/laya");
    tokio::fs::create_dir_all(&install)
        .await
        .map_err(|e| e.to_string())?;
    let script = install.join("start-managed.ps1");
    let embedded = include_str!("../../../scripts/laya/start.ps1");
    tokio::fs::write(
        &script,
        format!("\u{feff}{}", embedded.trim_start_matches('\u{feff}')),
    )
    .await
    .map_err(|e| e.to_string())?;
    // Never stop an unrelated service occupying the preferred port.
    let socket = std::net::TcpListener::bind("127.0.0.1:8000")
        .or_else(|_| std::net::TcpListener::bind("127.0.0.1:0"))
        .map_err(|e| e.to_string())?;
    let port = socket.local_addr().map_err(|e| e.to_string())?.port();
    let endpoint = format!("http://127.0.0.1:{port}/v1/systemone");
    let shell = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .ok_or("SystemRoot unavailable")?
        .join("System32/WindowsPowerShell/v1.0/powershell.exe");
    let mut command = crate::proc::hidden(shell);
    let mut child = command
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(script)
        .arg("-InstallDir")
        .arg(install)
        .arg("-Port")
        .arg(port.to_string())
        .arg("-Managed")
        .args(if repair { vec!["-Repair"] } else { vec![] })
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| e.to_string())?;
    let job = Job::attach(child.id().ok_or("missing installer process")?)?;
    let stdout = child.stdout.take().ok_or("stdout unavailable")?;
    let stderr = child.stderr.take().ok_or("stderr unavailable")?;
    let mut input = child.stdin.take().ok_or("stdin unavailable")?;
    let view = Arc::new(RwLock::new(Status {
        phase: "checking".into(),
        endpoint,
        ..Default::default()
    }));
    drop(socket);
    input
        .write_all(b"start\n")
        .await
        .map_err(|e| e.to_string())?;
    *last().write() = view.clone();
    persist(&view.read());
    let id = uuid::Uuid::new_v4().to_string();
    *slot = Some(Running {
        id: id.clone(),
        child,
        _job: job,
        view: view.clone(),
    });
    tokio::spawn(drain(stdout, view.clone()));
    tokio::spawn(drain(stderr, view.clone()));
    tokio::spawn(monitor(id, port));
    let snapshot = view.read().clone();
    Ok(snapshot)
}

async fn healthy(client: &reqwest::Client, port: u16) -> bool {
    let Ok(mut response) = client
        .get(format!("http://127.0.0.1:{port}/health"))
        .send()
        .await
    else {
        return false;
    };
    if !response.status().is_success() {
        return false;
    }
    let mut bytes = Vec::new();
    loop {
        let chunk = match response.chunk().await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break,
            Err(_) => return false,
        };
        if bytes.len() + chunk.len() > 65536 {
            return false;
        }
        bytes.extend_from_slice(&chunk);
    }
    let Ok(body) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return false;
    };
    body["status"] == "ok"
        && body["loaded"]
            .as_array()
            .is_some_and(|models| models.iter().any(|m| m == "multilingual"))
}
async fn monitor(id: String, port: u16) {
    monitor_with_budget(id, port, policy::budget).await;
}
async fn monitor_with_budget(id: String, port: u16, budget: fn(&str) -> u64) {
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(1))
        .build()
        .ok();
    let started = Instant::now();
    let mut phase = String::new();
    let mut phase_started = Instant::now();
    let mut ever_ready = false;
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let mut slot = running().lock().await;
        let Some(run) = slot.as_mut().filter(|r| r.id == id) else {
            return;
        };
        match run.child.try_wait() {
            Ok(None) => {}
            result => {
                let mut state = run.view.write();
                state.phase = "failed".into();
                state.error = Some(format!(
                    "安装或服务已退出，请查看运行记录 / installer or service exited: {result:?}"
                ));
                state.error_code = Some(policy::classify(&state.logs).into());
                persist(&state);
                drop(state);
                slot.take();
                return;
            }
        }
        let view = run.view.clone();
        {
            let mut state = view.write();
            state.elapsed_seconds = started.elapsed().as_secs();
            if state.phase != phase {
                phase = state.phase.clone();
                phase_started = Instant::now();
                persist(&state);
            }
            if phase_started.elapsed().as_secs() >= budget(&phase) {
                state.phase = "failed".into();
                state.error_code = Some("timeout".into());
                state.error = Some(format!("阶段 {phase} 超时，已停止；检查网络或资源后重试 / Stage {phase} timed out and was stopped; check connectivity and resources, then retry."));
                persist(&state);
                drop(state);
                slot.take();
                return;
            }
        }
        let check = matches!(view.read().phase.as_str(), "model" | "running" | "degraded");
        drop(slot);
        if check {
            let ready = if let Some(client) = &client {
                healthy(client, port).await
            } else {
                false
            };
            let slot = running().lock().await;
            if !slot.as_ref().is_some_and(|r| r.id == id) {
                return;
            }
            let mut state = view.write();
            ever_ready |= ready;
            state.phase = if ready {
                "running"
            } else if ever_ready {
                "degraded"
            } else {
                "model"
            }
            .into();
        }
    }
}
pub(super) async fn stop() -> Result<Status, String> {
    let mut slot = running().lock().await;
    if let Some(mut run) = slot.take() {
        // Closing the job terminates the installer, Python server and all descendants.
        drop(run._job);
        let _ = tokio::time::timeout(Duration::from_secs(5), run.child.wait()).await;
        let mut state = run.view.write();
        state.phase = "stopped".into();
        state.error = None;
        state.error_code = None;
        persist(&state);
    }
    Ok(status())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn logs_are_bounded_and_urls_redacted() {
        let view = RwLock::new(Status::default());
        for _ in 0..60 {
            log_line(
                &view,
                "Downloading https://user:secret@example.test/a?token=secret",
            );
        }
        assert_eq!(view.read().logs.len(), 40);
        assert!(!view.read().logs.join("\n").contains("secret"));
    }
    #[tokio::test]
    async fn job_closure_terminates_owned_process() {
        let shell = PathBuf::from(std::env::var_os("SystemRoot").unwrap())
            .join("System32/WindowsPowerShell/v1.0/powershell.exe");
        let mut child = crate::proc::hidden(shell)
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "[Console]::ReadLine() | Out-Null; Start-Sleep -Seconds 60",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let job = Job::attach(child.id().unwrap()).unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"start\n")
            .await
            .unwrap();
        assert!(child.try_wait().unwrap().is_none());
        drop(job);
        let _exit = tokio::time::timeout(Duration::from_secs(5), child.wait())
            .await
            .unwrap()
            .unwrap();
        // Windows job teardown may report exit code zero; timely exit proves cleanup.
    }
    #[test]
    fn late_stage_output_cannot_revive_stopped_service() {
        let view = RwLock::new(Status {
            phase: "stopped".into(),
            ..Default::default()
        });
        log_line(&view, "SYNAROUTE_STAGE:model");
        assert_eq!(view.read().phase, "stopped");
    }
    #[tokio::test]
    async fn hung_setup_is_killed_and_can_be_retried() {
        let shell = PathBuf::from(std::env::var_os("SystemRoot").unwrap())
            .join("System32/WindowsPowerShell/v1.0/powershell.exe");
        let child = crate::proc::hidden(shell)
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "Start-Sleep -Seconds 60",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let job = Job::attach(child.id().unwrap()).unwrap();
        let view = Arc::new(RwLock::new(Status {
            phase: "dependencies".into(),
            ..Default::default()
        }));
        *running().lock().await = Some(Running {
            id: "fault-test".into(),
            child,
            _job: job,
            view: view.clone(),
        });
        tokio::time::timeout(
            Duration::from_secs(5),
            monitor_with_budget("fault-test".into(), 1, |_| 0),
        )
        .await
        .unwrap();
        assert_eq!(view.read().phase, "failed");
        assert_eq!(view.read().error_code.as_deref(), Some("timeout"));
        assert!(
            running().lock().await.is_none(),
            "retry must not be blocked by stale ownership"
        );
    }
    #[tokio::test]
    async fn health_requires_loaded_model_and_rejects_oversize_response() {
        for (body, expected) in [
            (r#"{"status":"ok","loaded":[]}"#.to_string(), false),
            (
                r#"{"status":"ok","loaded":["multilingual"]}"#.to_string(),
                true,
            ),
            ("x".repeat(65537), false),
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = [0; 2048];
                let _ = socket.read(&mut request).await;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = socket.write_all(response.as_bytes()).await;
            });
            let client = reqwest::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(2))
                .build()
                .unwrap();
            assert_eq!(healthy(&client, port).await, expected);
            server.await.unwrap();
        }
    }
    #[tokio::test]
    #[ignore = "Downloads real Python dependencies and model weights; opt-in desktop smoke test"]
    async fn real_install_and_inference_smoke() {
        start(false).await.unwrap();
        let mut previous = String::new();
        let result = tokio::time::timeout(Duration::from_secs(1800), async {
            loop {
                tokio::time::sleep(Duration::from_secs(2)).await;
                let snapshot = status();
                let message = format!("{}: {}", snapshot.phase, snapshot.logs.last().cloned().unwrap_or_default());
                if previous != message { println!("{message}"); previous = message; }
                if snapshot.phase == "failed" { return Err(format!("{:?}: {:?}\n{}", snapshot.error_code, snapshot.error, snapshot.logs.join("\n"))); }
                if snapshot.phase == "running" {
                    let client = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(30)).build().unwrap();
                    let response = client.post(&snapshot.endpoint).json(&serde_json::json!({"model":"multilingual","state":"What is 2 + 2?","questions":{"complexity":{"type":"choice","instructions":"Choose how much reasoning this question requires.","criteria":{"low":"Simple fact","high":"Complex reasoning"}}}})).send().await.map_err(|e| e.to_string())?;
                    let code = response.status(); let body: serde_json::Value = response.json().await.map_err(|e| e.to_string())?;
                    if !code.is_success() || body["answers"]["complexity"]["choice"].as_str().is_none() { return Err(format!("Inference failed: {code} {body}")); }
                    return Ok(());
                }
            }
        }).await;
        stop().await.unwrap();
        result.expect("smoke deadline exceeded").unwrap();
    }

    #[tokio::test]
    async fn closing_job_also_stops_spawned_descendant() {
        use tokio::io::{AsyncBufReadExt, BufReader};
        use windows::Win32::System::Threading::{
            GetExitCodeProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        let shell = PathBuf::from(std::env::var_os("SystemRoot").unwrap())
            .join("System32/WindowsPowerShell/v1.0/powershell.exe");
        let code = r#"[Console]::ReadLine() | Out-Null; $p = Start-Process powershell.exe -WindowStyle Hidden -ArgumentList '-NoProfile','-NonInteractive','-Command','Start-Sleep -Seconds 60' -PassThru; [Console]::WriteLine($p.Id); Start-Sleep -Seconds 60"#;
        let mut child = crate::proc::hidden(shell)
            .args(["-NoProfile", "-NonInteractive", "-Command", code])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let job = Job::attach(child.id().unwrap()).unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"start\n")
            .await
            .unwrap();
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        let pid: u32 = tokio::time::timeout(Duration::from_secs(10), lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).unwrap() };
        drop(job);
        tokio::time::timeout(Duration::from_secs(5), child.wait())
            .await
            .unwrap()
            .unwrap();
        let mut exit = 259;
        for _ in 0..50 {
            unsafe {
                GetExitCodeProcess(handle, &mut exit).unwrap();
            }
            if exit != 259 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        unsafe {
            let _ = CloseHandle(handle);
        }
        assert_ne!(exit, 259, "descendant survived job closure");
    }
}
