use super::Status;
use crate::laya_runtime::bundle;
use parking_lot::RwLock;
use std::{path::PathBuf, process::Stdio, sync::OnceLock, time::{Duration, Instant}};
use tokio::{io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader}, process::Child, sync::Mutex};

struct Running {
    child: Child,
    _job: super::windows::Job,
    id: String,
    nonce: String,
    started: Instant,
}

fn running() -> &'static Mutex<Option<Running>> {
    static VALUE: OnceLock<Mutex<Option<Running>>> = OnceLock::new();
    VALUE.get_or_init(Default::default)
}

pub(super) fn root() -> Result<PathBuf, String> {
    super::data_dir().map(|path| path.join("laya-rust")).ok_or("data directory unavailable".into())
}

fn view() -> &'static RwLock<Status> {
    static VALUE: OnceLock<RwLock<Status>> = OnceLock::new();
    VALUE.get_or_init(|| {
        let mut status: Status = root().ok().and_then(|path| std::fs::read(path.join("last-status.json")).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default();
        status.phase = "stopped".into();
        status.backend = "rust".into();
        RwLock::new(status)
    })
}

fn persist() {
    if let (Ok(root), Ok(bytes)) = (root(), serde_json::to_vec(&*view().read())) {
        if let Err(error) = crate::secret::atomic_write(&root.join("last-status.json"), &bytes) {
            tracing::warn!("Could not save Rust Laya status: {error}");
        }
    }
}

pub(super) fn status() -> Status {
    let mut status = view().read().clone();
    status.version = Some(crate::laya_runtime::artifacts::manifest().version);
    status
}

pub(super) fn installed() -> bool { root().is_ok_and(|root| bundle::installed(&root)) }

pub(super) async fn import(source: PathBuf) -> Result<Status, String> {
    if running().lock().await.is_some() { return Err("Stop the local service before importing a bundle".into()); }
    let root = root()?;
    let version = tokio::task::spawn_blocking(move || bundle::install(&source, &root)).await.map_err(|error| error.to_string())??;
    let mut state = view().write();
    state.phase = "stopped".into();
    state.error = None;
    state.error_code = None;
    state.logs = vec![format!("Verified and imported {version}")];
    drop(state);
    Ok(status())
}

pub(super) async fn start(exact: bool) -> Result<Status, String> {
    let mut slot = running().lock().await;
    if slot.is_some() { return Ok(status()); }
    let root = root()?;
    let directory = bundle::current(&root)?;
    let executable = tokio::task::spawn_blocking(move || crate::laya_runtime::worker(&root)).await.map_err(|error| error.to_string())??;
    let previous = status();
    let preferred = previous.ready_endpoint.as_deref().and_then(|endpoint| url::Url::parse(endpoint).ok())
        .filter(|endpoint| endpoint.host_str() == Some("127.0.0.1")).and_then(|endpoint| endpoint.port()).unwrap_or(8000);
    let id = uuid::Uuid::new_v4().simple().to_string();
    let mut command = crate::proc::hidden(executable);
    let mut child = command.stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped())
        .kill_on_drop(true).spawn().map_err(|error| error.to_string())?;
    let job = super::windows::Job::attach(child.id().ok_or("worker PID missing")?)?;
    let mut input = child.stdin.take().ok_or("worker stdin missing")?;
    let error_stream = child.stderr.take().ok_or("worker stderr missing")?;
    let port = if exact { preferred } else { pick_port(preferred).await? };
    let configuration = serde_json::json!({"bundle":directory,"port":port,"nonce":id});
    input.write_all(format!("{configuration}\n").as_bytes()).await.map_err(|error| error.to_string())?;
    child.stdin = Some(input);
    *view().write() = Status { backend: "rust".into(), phase: "model".into(), endpoint: format!("http://127.0.0.1:{port}/v1/systemone"),
        ready_endpoint: previous.ready_endpoint, served_endpoints: previous.served_endpoints, ..Default::default() };
    *slot = Some(Running { child, _job: job, id: id.clone(), nonce: id.clone(), started: Instant::now() });
    tokio::spawn(async move {
        let mut lines = BufReader::new(error_stream.take(64 * 1024)).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let mut status = view().write();
            if status.logs.len() >= 30 { status.logs.remove(0); }
            status.logs.push(line.chars().take(1024).collect());
        }
    });
    tokio::spawn(monitor(id, port));
    persist();
    Ok(status())
}

async fn pick_port(preferred: u16) -> Result<u16, String> {
    for port in [preferred, 0] {
        if port != 0 && tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_ok() { continue; }
        if let Ok(listener) = std::net::TcpListener::bind(("127.0.0.1", port)) {
            return listener.local_addr().map(|address| address.port()).map_err(|error| error.to_string());
        }
    }
    Err("[port] no available loopback port".into())
}

async fn monitor(id: String, port: u16) {
    let Ok(client) = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(2)).build() else { return };
    loop {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let health = match client.get(format!("http://127.0.0.1:{port}/health")).send().await {
            Ok(response) if response.status().is_success() && response.content_length().is_some_and(|length| length < 4096) => response.json::<serde_json::Value>().await.ok(),
            _ => None,
        };
        let mut slot = running().lock().await;
        let Some(run) = slot.as_mut().filter(|run| run.id == id) else { return };
        let healthy = health.is_some_and(|health| health["status"] == "ok" && health["backend"] == "rust" && health["nonce"] == run.nonce);
        let exited = match run.child.try_wait() { Ok(exit) => exit.is_some(), Err(_) => true };
        let mut state = view().write();
        state.elapsed_seconds = run.started.elapsed().as_secs();
        if exited || (state.phase == "model" && run.started.elapsed() > Duration::from_secs(120)) {
            state.phase = "failed".into();
            state.error_code = Some(if exited { "process" } else { "timeout" }.into());
            state.error = Some(state.logs.last().cloned().unwrap_or_else(|| "Rust Laya did not become ready; verify or reimport its bundle".into()));
            slot.take();
            drop(state);
            persist();
            return;
        }
        if healthy && state.phase != "running" {
            state.phase = "running".into();
            let endpoint = state.endpoint.clone();
            state.ready_endpoint = Some(endpoint.clone());
            if !state.served_endpoints.contains(&endpoint) { state.served_endpoints.push(endpoint); }
            drop(state);
            persist();
        } else if !healthy && state.phase == "running" {
            state.phase = "degraded".into();
        }
    }
}

pub(super) async fn stop() -> Result<Status, String> {
    let Some(mut run) = running().lock().await.take() else { return Ok(status()); };
    let _ = run.child.kill().await;
    let _ = run.child.wait().await;
    view().write().phase = "stopped".into();
    persist();
    Ok(status())
}
