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
/// Temp file plus rename: a torn write would drop `ready_endpoint`, and the next launch-time start
/// would then come up on the default port while saved settings point at the old one.
#[cfg(not(test))]
fn persist(state: &Status) {
    let (Some(path), Ok(data)) = (state_file(), serde_json::to_vec(state)) else { return };
    let tmp = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4().simple()));
    if std::fs::write(&tmp, data).and_then(|_| std::fs::rename(&tmp, &path)).is_err() {
        let _ = std::fs::remove_file(&tmp);
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
pub(super) fn installed() -> bool {
    install_dir().is_some_and(|dir| installed_at(&dir))
}
fn install_dir() -> Option<PathBuf> {
    dirs::data_local_dir().map(|p| p.join("SynaRoute/laya"))
}
/// Same pointer rule as start.ps1: `environment.txt` names `venv` or `env-<32 hex>`, else `venv`.
fn installed_at(dir: &std::path::Path) -> bool {
    let saved = std::fs::read_to_string(dir.join("environment.txt")).unwrap_or_default();
    let saved = saved.trim();
    let valid = saved == "venv"
        || saved.strip_prefix("env-").is_some_and(|id| id.len() == 32 && id.chars().all(|c| c.is_ascii_hexdigit()));
    let scripts = dir.join(if valid { saved } else { "venv" }).join("Scripts");
    scripts.join("python.exe").is_file() && scripts.join("laya-serve.exe").is_file()
}

fn log_line(view: &RwLock<Status>, raw: &str) {
    let mut state = view.write();
    if let Some(stage) = raw.trim().strip_prefix("SYNAROUTE_STAGE:") {
        if !matches!(state.phase.as_str(), "stopped" | "failed" | "stopping")
            && matches!(stage, "checking" | "python" | "dependencies" | "model")
        {
            state.phase = stage.into();
        }
    } else if let Some(mark) = raw.trim().strip_prefix("SYNAROUTE_CRITICAL:") {
        // Only an explicit begin defers a stop; anything else (including a garbled marker) releases it.
        state.critical = mark == "begin";
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

const PREFERRED_PORT: u16 = 8000;
/// Whether some other program already accepts on loopback `port`.
async fn answers(port: u16) -> bool {
    let connect = tokio::net::TcpStream::connect(("127.0.0.1", port));
    matches!(tokio::time::timeout(Duration::from_millis(300), connect).await, Ok(Ok(_)))
}
/// Never take a port another service already answers on. On Windows a plain `127.0.0.1:P` bind
/// succeeds while another process listens on `0.0.0.0:P` or `[::]:P`, and then *steals* that
/// service's loopback traffic (`python -m http.server 8000` binds `[::]` dual-stack). So bind is
/// only a reservation (never `listen`, so the probe cannot reach ourselves) and connect decides.
///
/// Residual window: the reservation ends before PowerShell starts, and start.ps1 probes again just
/// before launch, so only a program grabbing the port in between remains. uvicorn then fails to bind
/// (classified `port`), or a foreign service answers; the health check requires `loaded` to list
/// `multilingual`, so that service is never recorded as ready and its endpoint is never saved.
async fn pick_port(preferred: u16, exact: bool) -> Result<u16, String> {
    pick_port_with(preferred, exact, answers).await
}
/// The port of the last endpoint that passed the health check; settings were saved against it.
fn preferred_port(ready: Option<&str>) -> u16 {
    ready
        .and_then(|e| reqwest::Url::parse(e).ok())
        .filter(|u| u.host_str() == Some("127.0.0.1"))
        .and_then(|u| u.port())
        .filter(|p| *p >= 1024) // start.ps1 ValidateRange
        .unwrap_or(PREFERRED_PORT)
}
/// `exact` is the launch-time start: no user is watching to save a new port, so moving would leave
/// saved settings pointing at whatever now owns the old one. It fails visibly instead.
async fn pick_port_with<F, Fut>(preferred: u16, exact: bool, answers: F) -> Result<u16, String>
where
    F: Fn(u16) -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    use socket2::{Domain, Socket, Type};
    let reserve = |port: u16| -> std::io::Result<(Socket, u16)> {
        let socket = Socket::new(Domain::IPV4, Type::STREAM, None)?;
        socket.bind(&std::net::SocketAddr::from(([127, 0, 0, 1], port)).into())?;
        let bound = socket.local_addr()?.as_socket().map_or(port, |a| a.port());
        Ok((socket, bound))
    };
    let fallback = if exact { 0 } else { 8 };
    for port in std::iter::once(preferred).chain(std::iter::repeat(0).take(fallback)) {
        let Ok((_reservation, bound)) = reserve(port) else { continue };
        if !answers(bound).await {
            return Ok(bound);
        }
    }
    Err(if exact {
        format!("[port] 端口 {preferred} 已被其他程序占用，未自动启动；保存的本地接口可能正指向那个程序。点击重试会选择新端口 / port {preferred} is in use; not started automatically.")
    } else {
        "找不到未被其他程序占用的本机端口 / no free loopback port".into()
    })
}

struct Launch {
    repair: bool,
    no_install: bool,
    exact_port: bool,
    mirror: bool,
}
pub(super) async fn start(repair: bool, no_install: bool, mirror: bool) -> Result<Status, String> {
    launch(Launch { repair, no_install, exact_port: false, mirror }).await
}
/// Never installs, and only on the port saved settings point at (see `pick_port_with`).
pub(super) async fn start_on_launch(mirror: bool) -> Result<Status, String> {
    launch(Launch { repair: false, no_install: true, exact_port: true, mirror }).await
}
async fn launch(plan: Launch) -> Result<Status, String> {
    let mut slot = running().lock().await;
    let result = start_inner(&plan, &mut slot).await;
    if let Err(error) = &result {
        let view = last().read().clone();
        let mut state = view.write();
        state.phase = "failed".into();
        state.error_code = Some(failure_code(error).into());
        state.error = Some(error.clone());
        persist(&state);
    }
    result
}
/// A launch-time port conflict is "port" (retry picks a new one), not "setup" (check permissions).
fn failure_code(error: &str) -> &'static str {
    policy::explicit_failure(&[error.to_string()]).map_or("setup", |(code, _)| code)
}
async fn start_inner(plan: &Launch, slot: &mut Option<Running>) -> Result<Status, String> {
    if let Some(run) = slot.as_ref() {
        return Ok(run.view.read().clone());
    }
    let install = install_dir().ok_or("找不到本地数据目录 / local data directory unavailable")?;
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
    // The script installs only from the lock next to it ($PSScriptRoot) and refuses without it.
    tokio::fs::write(
        install.join("requirements.lock.txt"),
        include_str!("../../../scripts/laya/requirements.lock.txt"),
    )
    .await
    .map_err(|e| e.to_string())?;
    let previous = last().read().read().clone();
    let ready_endpoint = previous.ready_endpoint;
    let port = pick_port(preferred_port(ready_endpoint.as_deref()), plan.exact_port).await?;
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
        .args(if plan.repair { vec!["-Repair"] } else { vec![] })
        .args(if plan.no_install { vec!["-NoInstall"] } else { vec![] })
        .args(if plan.mirror { vec!["-ModelMirror"] } else { vec![] })
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
        ready_endpoint, // a failed run must not move the port the next launch expects
        served_endpoints: previous.served_endpoints,
        ..Default::default()
    }));
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
enum End {
    Stop,
    TimedOut,
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
    let mut ticks: u64 = 0;
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
                state.error = Some(policy::explicit_failure(&state.logs)
                    .map(|(_, message)| message.to_string())
                    .unwrap_or_else(|| format!("安装或服务已退出，请查看运行记录 / installer or service exited: {result:?}")));
                state.error_code = Some(policy::classify(&state.logs).into());
                persist(&state);
                drop(state);
                slot.take();
                return;
            }
        }
        let view = run.view.clone();
        // The write guard is not Send, so its block ends before anything below awaits.
        let ended = {
            let mut state = view.write();
            state.elapsed_seconds = started.elapsed().as_secs();
            if state.phase != phase {
                phase = state.phase.clone();
                phase_started = Instant::now();
                persist(&state);
            }
            let overdue = phase_started.elapsed().as_secs() >= budget(&phase);
            if phase == "stopping" {
                // A stop requested while the Python installer ran completes as soon as it returns,
                // or is forced once the installer has had its bounded wait.
                (!state.critical || overdue).then_some(End::Stop)
            } else if overdue {
                state.phase = "failed".into();
                state.error_code = Some("timeout".into());
                state.error = Some(format!("阶段 {phase} 超时，已停止；检查网络或资源后重试 / Stage {phase} timed out and was stopped; check connectivity and resources, then retry."));
                persist(&state);
                Some(End::TimedOut)
            } else {
                None
            }
        };
        match ended {
            Some(End::Stop) => {
                if let Some(run) = slot.take() {
                    finish_stop(run).await;
                }
                return;
            }
            Some(End::TimedOut) => {
                slot.take();
                return;
            }
            None => {}
        }
        ticks += 1;
        let check = health_due(&view.read().phase, ticks);
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
            apply_health(&mut view.write(), ready, &mut ever_ready);
        }
    }
}
/// Loading and recovery are checked every second; a healthy service only every 5 s, since a check
/// is an HTTP request that the server answers for as long as it runs.
fn health_due(phase: &str, tick: u64) -> bool {
    match phase {
        "model" | "degraded" => true,
        "running" => tick % 5 == 0,
        _ => false,
    }
}
/// Only an endpoint that actually served becomes the one a launch-time start insists on.
fn apply_health(state: &mut Status, ready: bool, ever_ready: &mut bool) {
    // A stop requested while this check was in flight must not be overwritten.
    if state.phase == "stopping" {
        return;
    }
    *ever_ready |= ready;
    state.phase = if ready {
        "running"
    } else if *ever_ready {
        "degraded"
    } else {
        "model"
    }
    .into();
    if ready {
        state.ready_endpoint = Some(state.endpoint.clone());
        if !state.served_endpoints.contains(&state.endpoint) {
            state.served_endpoints.push(state.endpoint.clone());
            let excess = state.served_endpoints.len().saturating_sub(16);
            state.served_endpoints.drain(..excess);
        }
    }
}
/// `force` is for app exit only. Otherwise a stop during the Python installer is deferred: the phase
/// becomes "stopping" and the monitor finishes it when the installer returns (or after its budget),
/// because cutting the installer off can leave a half-installed Python behind.
pub(super) async fn stop(force: bool) -> Result<Status, String> {
    let mut slot = running().lock().await;
    let defer = !force && slot.as_ref().is_some_and(|run| run.view.read().critical);
    if defer {
        if let Some(run) = slot.as_ref() {
            let mut state = run.view.write();
            state.phase = "stopping".into();
            persist(&state);
        }
    } else if let Some(run) = slot.take() {
        finish_stop(run).await;
    }
    drop(slot);
    Ok(status())
}
async fn finish_stop(mut run: Running) {
    // Closing the job terminates the installer, Python server and all descendants.
    drop(run._job);
    let _ = tokio::time::timeout(Duration::from_secs(5), run.child.wait()).await;
    let mut state = run.view.write();
    state.phase = "stopped".into();
    state.critical = false;
    state.error = None;
    state.error_code = None;
    persist(&state);
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
    async fn a_port_another_service_answers_on_is_never_taken() {
        // Real listener: both the reservation and the probe see it.
        let owner = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let taken = owner.local_addr().unwrap().port();
        assert!(answers(taken).await, "probe must see a live loopback listener");
        assert_ne!(pick_port(taken, false).await.unwrap(), taken);
        // Launch-time start: moving would leave saved settings pointing at the other program.
        let refused = pick_port(taken, true).await.unwrap_err();
        assert_eq!(failure_code(&refused), "port", "exact mode must fail with an explicit code: {refused}");
        assert_eq!(failure_code("SystemRoot unavailable"), "setup");
        let prod_launch = crate::proxy::custom_headers::production_code_only(include_str!("laya_windows.rs"));
        assert!(prod_launch.contains("state.error_code = Some(failure_code(error).into())"));
        drop(owner);
        // Windows wildcard owner: the reservation succeeds, only the probe can tell (measured with
        // a separate `http.server --bind ::` process). Simulated here to avoid non-loopback sockets.
        let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let picked = pick_port_with(free, false, |p| async move { p == free }).await.unwrap();
        assert_ne!(picked, free, "a successful bind must not override a positive probe");
        assert!(pick_port_with(free, true, |p| async move { p == free }).await.is_err());
        assert_eq!(pick_port_with(free, true, |_| async { false }).await.unwrap(), free, "free preferred port is kept");
        // Wiring: the cases above call the helper, so a bare bind in start_inner keeps them green.
        let prod = crate::proxy::custom_headers::production_code_only(include_str!("laya_windows.rs"));
        let body = &prod[prod.find("async fn start_inner(").unwrap()..];
        let body = &body[..body.find("\n}\n").unwrap()];
        assert!(body.contains("pick_port(preferred_port(ready_endpoint.as_deref()), plan.exact_port)") && !body.contains("TcpListener::bind"));
        assert!(body.contains("ready_endpoint,"), "a new run must carry the last served endpoint forward");
        assert!(body.contains("served_endpoints: previous.served_endpoints"), "the served history must survive a new run");
        // The no-dialog start is only safe because the script then refuses to install.
        assert!(body.contains(r#"vec!["-NoInstall"]"#), "no_install must reach start.ps1 as -NoInstall");
        assert!(body.contains(r#"vec!["-ModelMirror"]"#), "the mirror preference must reach start.ps1");
        // Without the lock beside the managed script the script refuses to install at all.
        assert!(body.contains(r#"install.join("requirements.lock.txt")"#));
    }
    #[test]
    fn installed_follows_the_script_pointer_rule() {
        let dir = std::env::temp_dir().join(format!("laya-installed-{}", uuid::Uuid::new_v4().simple()));
        let make = |env: &str| {
            let scripts = dir.join(env).join("Scripts");
            std::fs::create_dir_all(&scripts).unwrap();
            for exe in ["python.exe", "laya-serve.exe"] {
                std::fs::write(scripts.join(exe), b"x").unwrap();
            }
        };
        assert!(!installed_at(&dir), "empty directory");
        let env = format!("env-{}", "c".repeat(32));
        make(&env);
        std::fs::write(dir.join("environment.txt"), format!("{env}\r\n")).unwrap();
        assert!(installed_at(&dir), "pointer to a complete environment");
        std::fs::write(dir.join("environment.txt"), "../outside").unwrap();
        assert!(!installed_at(&dir), "an invalid pointer falls back to venv, which does not exist yet");
        make("venv");
        assert!(installed_at(&dir), "invalid pointer falls back to venv like the script");
        std::fs::write(dir.join("environment.txt"), &env).unwrap();
        std::fs::remove_file(dir.join(&env).join("Scripts/laya-serve.exe")).unwrap();
        assert!(!installed_at(&dir), "both executables are required");
        std::fs::remove_dir_all(&dir).unwrap();
        // Wiring: without this the flag stays false and the download dialog returns on every start.
        let prod = crate::proxy::custom_headers::production_code_only(include_str!("laya_windows.rs"));
        let body = &prod[prod.find("pub(super) fn installed()").unwrap()..];
        assert!(body[..body.find("\n}\n").unwrap()].contains("installed_at(&dir)"));
    }
    #[test]
    fn launch_time_start_keeps_the_endpoint_settings_were_saved_against() {
        assert_eq!(preferred_port(None), PREFERRED_PORT);
        assert_eq!(preferred_port(Some("http://127.0.0.1:52344/v1/systemone")), 52344);
        for odd in ["http://127.0.0.1:80/v1/systemone", "http://192.168.1.5:9000/x", "not a url"] {
            assert_eq!(preferred_port(Some(odd)), PREFERRED_PORT, "{odd}");
        }
        let mut state = Status { endpoint: "http://127.0.0.1:52344/v1/systemone".into(), ..Default::default() };
        let mut ever = false;
        apply_health(&mut state, false, &mut ever);
        assert_eq!((state.phase.as_str(), state.ready_endpoint.as_deref()), ("model", None), "loading never counts");
        apply_health(&mut state, true, &mut ever);
        assert_eq!(state.ready_endpoint.as_deref(), Some("http://127.0.0.1:52344/v1/systemone"));
        apply_health(&mut state, false, &mut ever);
        assert_eq!(state.phase, "degraded");
        assert_eq!(state.served_endpoints, vec!["http://127.0.0.1:52344/v1/systemone".to_string()]);
        state.phase = "stopping".into();
        apply_health(&mut state, true, &mut ever);
        assert_eq!(state.phase, "stopping", "a health result arriving after a stop request must not revive the service");
        for p in 0..20 {
            state.phase = "model".into();
            state.endpoint = format!("http://127.0.0.1:{}/v1/systemone", 40000 + p);
            apply_health(&mut state, true, &mut ever);
        }
        assert_eq!(state.served_endpoints.len(), 16, "served endpoints are bounded");
        assert!(state.served_endpoints.last().unwrap().contains("40019"), "the newest are kept");
        // Wiring: monitor records health through apply_health; launch never installs and never moves.
        let prod = crate::proxy::custom_headers::production_code_only(include_str!("laya_windows.rs"));
        assert!(prod.contains("apply_health(&mut view.write(), ready, &mut ever_ready)"));
        let body = &prod[prod.find("pub(super) async fn start_on_launch(").unwrap()..];
        let body = &body[..body.find("\n}\n").unwrap()];
        assert!(body.contains("no_install: true") && body.contains("exact_port: true"), "{body}");
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
    /// The running slot is process-global; tests that occupy it run one at a time.
    fn slot_test_lock() -> &'static tokio::sync::Mutex<()> {
        static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(Default::default)
    }
    fn sleeper() -> (Child, Job) {
        let shell = PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32/WindowsPowerShell/v1.0/powershell.exe");
        let child = crate::proc::hidden(shell)
            .args(["-NoProfile", "-NonInteractive", "-Command", "Start-Sleep -Seconds 60"])
            .stdout(Stdio::null()).stderr(Stdio::null()).kill_on_drop(true).spawn().unwrap();
        let job = Job::attach(child.id().unwrap()).unwrap();
        (child, job)
    }
    #[tokio::test]
    async fn a_stop_during_the_python_installer_waits_for_it() {
        let _serial = slot_test_lock().lock().await;
        let (child, job) = sleeper();
        let view = Arc::new(RwLock::new(Status { phase: "python".into(), ..Default::default() }));
        *running().lock().await = Some(Running { id: "critical-test".into(), child, _job: job, view: view.clone() });
        log_line(&view, "SYNAROUTE_CRITICAL:begin");
        assert!(view.read().critical && view.read().logs.is_empty(), "markers are protocol, not log lines");
        stop(false).await.unwrap();
        assert_eq!(view.read().phase, "stopping", "a stop inside the installer must wait");
        assert!(running().lock().await.is_some(), "the installer must keep running");
        log_line(&view, "SYNAROUTE_STAGE:dependencies");
        assert_eq!(view.read().phase, "stopping", "a later stage must not cancel the requested stop");
        log_line(&view, "SYNAROUTE_CRITICAL:end");
        tokio::time::timeout(Duration::from_secs(10), monitor_with_budget("critical-test".into(), 1, policy::budget)).await.unwrap();
        assert_eq!(view.read().phase, "stopped", "the stop completes once the installer returns");
        assert!(running().lock().await.is_none());
    }
    #[tokio::test]
    async fn a_deferred_stop_is_forced_after_its_budget_and_app_exit_never_waits() {
        let _serial = slot_test_lock().lock().await;
        for forced in [false, true] {
            let (child, job) = sleeper();
            let view = Arc::new(RwLock::new(Status { phase: "python".into(), critical: true, ..Default::default() }));
            *running().lock().await = Some(Running { id: "forced-test".into(), child, _job: job, view: view.clone() });
            stop(forced).await.unwrap();
            if forced {
                assert_eq!(view.read().phase, "stopped", "app exit must not wait for the installer");
            } else {
                assert_eq!(view.read().phase, "stopping");
                // An installer that never returns: a zero budget stands in for the 10-minute wait.
                tokio::time::timeout(Duration::from_secs(10), monitor_with_budget("forced-test".into(), 1, |_| 0)).await.unwrap();
                assert_eq!((view.read().phase.as_str(), view.read().error_code.clone()), ("stopped", None), "an overdue stop is forced, not reported as a failure");
            }
            assert!(running().lock().await.is_none());
        }
    }
    #[test]
    fn a_healthy_service_is_checked_less_often_than_a_loading_one() {
        assert!((1..=10).all(|t| health_due("model", t) && health_due("degraded", t)));
        assert_eq!((1..=10).filter(|t| health_due("running", *t)).count(), 2);
        assert!(!health_due("stopping", 5) && !health_due("python", 5));
        let prod = crate::proxy::custom_headers::production_code_only(include_str!("laya_windows.rs"));
        assert!(prod.contains("let check = health_due(&view.read().phase, ticks);"));
    }
    #[tokio::test]
    async fn hung_setup_is_killed_and_can_be_retried() {
        let _serial = slot_test_lock().lock().await;
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
        start(false, false, false).await.unwrap();
        let mut previous = String::new();
        let result = tokio::time::timeout(Duration::from_secs(1800), async {
            loop {
                tokio::time::sleep(Duration::from_secs(2)).await;
                let snapshot = status();
                let message = format!("{}: {}", snapshot.phase, snapshot.logs.last().cloned().unwrap_or_default());
                if previous != message { println!("{message}"); previous = message; }
                if snapshot.phase == "failed" { return Err(format!("{:?}: {:?}\n{}", snapshot.error_code, snapshot.error, snapshot.logs.join("\n"))); }
                if snapshot.phase == "running" {
                    // The endpoint that served is the one recorded, so a launch-time start returns to it.
                    if snapshot.ready_endpoint.as_deref() != Some(snapshot.endpoint.as_str()) || !snapshot.served_endpoints.contains(&snapshot.endpoint) {
                        return Err(format!("served endpoint not recorded: {:?} {:?}", snapshot.ready_endpoint, snapshot.served_endpoints));
                    }
                    // The production question (laya_judge), on the real model: a finite noul score.
                    let client = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(30)).build().unwrap();
                    let body = crate::store::decision::laya_judge::body("multilingual", "Design a distributed consensus protocol and prove it tolerates one crashed node.");
                    let response = client.post(&snapshot.endpoint).json(&body).send().await.map_err(|e| e.to_string())?;
                    let code = response.status(); let body: serde_json::Value = response.json().await.map_err(|e| e.to_string())?;
                    let score = body["answers"]["deep"]["noul"].as_f64();
                    if !code.is_success() || !score.is_some_and(f64::is_finite) { return Err(format!("Inference failed: {code} {body}")); }
                    println!("deep-reasoning score for a hard task: {score:?}");
                    return Ok(());
                }
            }
        }).await;
        stop(true).await.unwrap();
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
