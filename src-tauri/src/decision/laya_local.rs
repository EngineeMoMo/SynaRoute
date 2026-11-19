//! The embedded installer is available in packaged apps without a source checkout.
use serde::{Deserialize, Serialize};
use std::path::Path;
#[cfg(any(windows, test))]
#[path = "laya_policy.rs"]
mod policy;
#[cfg(windows)]
#[path = "laya_windows.rs"]
mod windows;
#[cfg(all(windows, target_arch = "x86_64"))]
#[path = "laya_rust.rs"]
mod rust;

fn operations() -> &'static tokio::sync::Mutex<()> {
    static VALUE: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    VALUE.get_or_init(Default::default)
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    #[serde(default = "python_backend")]
    pub backend: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub rust_supported: bool,
    #[serde(default, skip_deserializing)]
    pub can_rollback: bool,
    pub supported: bool,
    pub phase: String,
    pub endpoint: String,
    pub logs: Vec<String>,
    pub error: Option<String>,
    #[serde(default)]
    pub error_code: Option<String>,
    #[serde(default)]
    pub elapsed_seconds: u64,
    /// An environment exists, so the panel may start it without the download dialog. Advisory:
    /// that start passes `-NoInstall`, and the script refuses rather than installs if it disagrees.
    #[serde(default, skip_deserializing)]
    pub installed: bool,
    /// The user's choice to start the installed service with SynaRoute (default off).
    #[serde(default, skip_deserializing)]
    pub autostart: bool,
    /// Last endpoint that passed the health check. Saved settings were written while it was
    /// running, so a launch-time start must come up on exactly this port.
    #[serde(default)]
    pub ready_endpoint: Option<String>,
    /// Every endpoint this managed service has served; only settings pointing at one of these may be
    /// moved to a new port (a user's own Laya on another port is never touched).
    #[serde(default)]
    pub served_endpoints: Vec<String>,
    /// The Python installer is running: a stop waits for it (phase "stopping") instead of cutting it off.
    #[serde(default, skip_deserializing)]
    pub critical: bool,
    /// The user's choice to download the model through hf-mirror.com (default off).
    #[serde(default, skip_deserializing)]
    pub mirror: bool,
}
impl Default for Status {
    fn default() -> Self {
        Self {
            backend: python_backend(),
            version: None,
            rust_supported: cfg!(all(windows, target_arch = "x86_64")),
            can_rollback: false,
            supported: cfg!(windows),
            phase: "idle".into(),
            endpoint: "http://127.0.0.1:8000/v1/systemone".into(),
            logs: Vec::new(),
            error: None,
            error_code: None,
            elapsed_seconds: 0,
            installed: false,
            autostart: false,
            ready_endpoint: None,
            served_endpoints: Vec::new(),
            critical: false,
            mirror: false,
        }
    }
}

/// Machine-local choices, outside `config.json`: they must not travel with an exported configuration
/// (the other machine may have no environment), and `SYNAROUTE_DATA_DIR` isolates them like the rest.
const PREFS_FILE: &str = "laya-local.json";
#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Prefs {
    #[serde(default = "python_backend")]
    backend: String,
    #[serde(default)]
    autostart: bool,
    #[serde(default)]
    mirror: bool,
}
fn python_backend() -> String { "python".into() }
#[cfg(all(windows, target_arch = "x86_64"))]
fn selected_backend() -> String {
    data_dir().map(|directory| prefs_in(&directory).backend).filter(|backend| backend == "rust").unwrap_or_else(python_backend)
}
/// Any read or parse failure means off: launching a 1.6 GB service, or sending downloads to a
/// third party, on a guess is the wrong default.
fn prefs_in(dir: &Path) -> Prefs {
    std::fs::read_to_string(dir.join(PREFS_FILE))
        .ok()
        .and_then(|text| serde_json::from_str::<Prefs>(&text).ok())
        .unwrap_or_default()
}
#[cfg(any(windows, test))]
fn autostart_in(dir: &Path) -> bool {
    prefs_in(dir).autostart
}
fn write_autostart_in(dir: &Path, autostart: bool) -> Result<(), String> {
    update_prefs(dir, |prefs| prefs.autostart = autostart)
}
fn write_mirror_in(dir: &Path, mirror: bool) -> Result<(), String> {
    update_prefs(dir, |prefs| prefs.mirror = mirror)
}
fn update_prefs(dir: &Path, edit: impl FnOnce(&mut Prefs)) -> Result<(), String> {
    static WRITES: parking_lot::Mutex<()> = parking_lot::Mutex::new(());
    let _write = WRITES.lock();
    let mut prefs = prefs_in(dir);
    edit(&mut prefs);
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let path = dir.join(PREFS_FILE);
    let tmp = dir.join(format!("{PREFS_FILE}.{}.tmp", uuid::Uuid::new_v4().simple()));
    let text = serde_json::to_string_pretty(&prefs).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, text)
        .and_then(|_| std::fs::rename(&tmp, &path))
        .map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("保存本地服务偏好失败 / could not save the preference: {e}")
        })
}
fn data_dir() -> Option<std::path::PathBuf> {
    crate::store::data_dir::app_data_dir().ok()
}

/// Every command returns through here, so the panel never sees `installed`/`autostart` flip
/// between a poll and the result of a click.
fn complete(mut status: Status) -> Status {
    status.rust_supported = cfg!(all(windows, target_arch = "x86_64"));
    #[cfg(all(windows, target_arch = "x86_64"))]
    if status.backend == "rust" {
        status.installed = rust::installed();
        status.can_rollback = rust::root().is_ok_and(|root| crate::laya_runtime::bundle::can_rollback(&root));
    } else {
        status.installed = windows::installed();
    }
    #[cfg(all(windows, not(target_arch = "x86_64")))]
    {
        status.installed = windows::installed();
    }
    let prefs = data_dir().map(|dir| prefs_in(&dir)).unwrap_or_default();
    status.autostart = prefs.autostart;
    status.mirror = prefs.mirror;
    status
}
#[cfg(windows)]
fn mirror_pref() -> bool {
    data_dir().is_some_and(|dir| prefs_in(&dir).mirror)
}

#[tauri::command]
pub fn laya_local_status() -> Status {
    #[cfg(all(windows, target_arch = "x86_64"))]
    if selected_backend() == "rust" { return complete(rust::status()); }
    #[cfg(windows)]
    let status = windows::status();
    #[cfg(not(windows))]
    let status = Status::default();
    complete(status)
}
#[tauri::command]
pub async fn laya_local_start(repair: Option<bool>, no_install: Option<bool>) -> Result<Status, String> {
    let _operation = operations().lock().await;
    #[cfg(all(windows, target_arch = "x86_64"))]
    if selected_backend() == "rust" {
        return rust::start(false).await.map(complete);
    }
    #[cfg(windows)]
    {
        windows::start(repair.unwrap_or(false), no_install.unwrap_or(false), mirror_pref()).await.map(complete)
    }
    #[cfg(not(windows))]
    {
        let _ = (repair, no_install);
        Err("一键部署目前支持 Windows / one-click setup currently supports Windows".into())
    }
}
/// While the Python installer runs this only requests the stop (phase "stopping"); the monitor
/// completes it when the installer returns, or forces it after a bounded wait.
#[tauri::command]
pub async fn laya_local_stop() -> Result<Status, String> {
    let _operation = operations().lock().await;
    #[cfg(all(windows, target_arch = "x86_64"))]
    if selected_backend() == "rust" { return rust::stop().await.map(complete); }
    #[cfg(windows)]
    let status = windows::stop(false).await?;
    #[cfg(not(windows))]
    let status = Status::default();
    Ok(complete(status))
}

#[tauri::command]
pub async fn laya_local_set_backend(backend: String) -> Result<Status, String> {
    let _operation = operations().lock().await;
    if !matches!(backend.as_str(), "python" | "rust") { return Err("Unknown local backend".into()); }
    if backend == "rust" && !cfg!(all(windows, target_arch = "x86_64")) { return Err("Rust Laya currently requires Windows x64".into()); }
    if !matches!(laya_local_status().phase.as_str(), "idle" | "stopped" | "failed") { return Err("Stop the local service before switching its backend".into()); }
    let directory = data_dir().ok_or("data directory unavailable")?;
    update_prefs(&directory, |prefs| prefs.backend = backend)?;
    Ok(laya_local_status())
}

#[tauri::command]
pub async fn laya_local_import_bundle(directory: String) -> Result<Status, String> {
    let _operation = operations().lock().await;
    #[cfg(all(windows, target_arch = "x86_64"))]
    {
        if selected_backend() != "rust" { return Err("Select Rust Laya before importing its bundle".into()); }
        rust::import(directory.into()).await.map(complete)
    }
    #[cfg(not(all(windows, target_arch = "x86_64")))]
    { let _ = directory; Err("Rust Laya currently requires Windows x64".into()) }
}

#[tauri::command]
pub async fn laya_local_rollback_bundle() -> Result<Status, String> {
    let _operation = operations().lock().await;
    #[cfg(all(windows, target_arch = "x86_64"))]
    {
        if selected_backend() != "rust" || !matches!(rust::status().phase.as_str(), "idle" | "stopped" | "failed") {
            return Err("Stop Rust Laya before restoring its previous bundle".into());
        }
        let directory = rust::root()?;
        tokio::task::spawn_blocking(move || crate::laya_runtime::bundle::rollback(&directory)).await.map_err(|error| error.to_string())??;
        Ok(complete(rust::status()))
    }
    #[cfg(not(all(windows, target_arch = "x86_64")))]
    { Err("Rust Laya currently requires Windows x64".into()) }
}
/// Takes effect at the next launch only: switching it never starts or stops a running service.
#[tauri::command]
pub fn laya_local_set_autostart(enabled: bool) -> Result<Status, String> {
    let dir = data_dir().ok_or("找不到数据目录 / data directory unavailable")?;
    write_autostart_in(&dir, enabled)?;
    Ok(laya_local_status())
}
/// Applies to the next model download; an existing model cache is never downloaded again.
#[tauri::command]
pub fn laya_local_set_mirror(enabled: bool) -> Result<Status, String> {
    let dir = data_dir().ok_or("找不到数据目录 / data directory unavailable")?;
    write_mirror_in(&dir, enabled)?;
    Ok(laya_local_status())
}
/// Moves other categories' settings that still point at an earlier port of this managed service to
/// `endpoint`, which must be the endpoint that just passed the health check.
#[tauri::command]
pub fn laya_local_sync_endpoints(state: tauri::State<crate::AppState>, endpoint: String) -> Result<Vec<crate::model::CategoryType>, String> {
    let status = laya_local_status();
    if status.ready_endpoint.as_deref() != Some(endpoint.as_str()) {
        return Err("本地服务尚未在该接口就绪 / the local service is not ready at that endpoint".into());
    }
    super::sync_local_endpoints(&state.store, &status.served_endpoints, &endpoint).map_err(|e| e.to_string())
}
/// Called once from app setup. Starts only an installed environment (never installs) and only on
/// the port saved settings point at; a failure shows in the panel like any other start.
pub(crate) fn autostart_on_launch() {
    #[cfg(windows)]
    if data_dir().is_some_and(|dir| autostart_in(&dir)) {
        tauri::async_runtime::spawn(async {
            // Let the window come up before PyTorch and the model load.
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            let _operation = operations().lock().await;
            #[cfg(all(windows, target_arch = "x86_64"))]
            if selected_backend() == "rust" { let _ = rust::start(true).await; return; }
            let _ = windows::start_on_launch(mirror_pref()).await;
        });
    }
}
/// The app is exiting and its job object kills the tree anyway, so this never defers.
pub(crate) async fn shutdown() {
    #[cfg(all(windows, target_arch = "x86_64"))]
    let _ = rust::stop().await;
    #[cfg(windows)]
    let _ = windows::stop(true).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn autostart_preference_defaults_off_and_round_trips() {
        let dir = std::env::temp_dir().join(format!("laya-prefs-{}", uuid::Uuid::new_v4().simple()));
        assert!(!autostart_in(&dir), "missing file");
        write_autostart_in(&dir, true).unwrap();
        assert!(autostart_in(&dir));
        write_autostart_in(&dir, false).unwrap();
        assert!(!autostart_in(&dir));
        // Each switch keeps the other: a read-modify-write, not two files racing for one name.
        write_mirror_in(&dir, true).unwrap();
        write_autostart_in(&dir, true).unwrap();
        assert!(prefs_in(&dir).mirror && prefs_in(&dir).autostart);
        write_mirror_in(&dir, false).unwrap();
        assert!(!prefs_in(&dir).mirror && prefs_in(&dir).autostart, "turning the mirror off must keep autostart");
        std::fs::write(dir.join(PREFS_FILE), "{ 坏 json").unwrap();
        assert!(!autostart_in(&dir) && !prefs_in(&dir).mirror, "a corrupt file must mean off");
        std::fs::remove_dir_all(&dir).unwrap();
    }
    #[test]
    fn every_command_reports_the_same_flags_and_launch_only_ever_starts() {
        let prod = crate::proxy::custom_headers::production_code_only(include_str!("laya_local.rs"));
        let body = |sig: &str| {
            let at = prod.find(sig).unwrap_or_else(|| panic!("{sig} not found"));
            let rest = &prod[at..];
            rest[..rest.find("\n}\n").unwrap()].to_string()
        };
        for cmd in ["fn laya_local_status(", "fn laya_local_start(", "fn laya_local_stop("] {
            assert!(body(cmd).contains("complete"), "{cmd} must report through complete()");
        }
        let launch = body("fn autostart_on_launch(");
        // The hook's own name contains "start_on_launch(", so only the qualified call proves it starts anything.
        assert!(launch.contains("autostart_in(") && launch.contains("windows::start_on_launch(mirror_pref())"));
        assert!(!launch.contains("windows::start("), "the launch hook must never take the installing path");
        // Both start paths follow the mirror preference; neither may hard-code it.
        assert!(body("fn laya_local_start(").contains("mirror_pref()"));
        // A user's stop waits for the Python installer; only app exit (the job dies anyway) forces it.
        assert!(body("fn laya_local_stop(").contains("windows::stop(false)"));
        assert!(body("fn shutdown(").contains("windows::stop(true)"));
        // Sync only ever targets the endpoint that just passed the health check.
        assert!(body("fn laya_local_sync_endpoints(").contains("status.ready_endpoint.as_deref() != Some(endpoint.as_str())"));
        let lib = crate::proxy::custom_headers::production_code_only(include_str!("../lib.rs"));
        assert!(lib.contains("laya_local::autostart_on_launch()"), "the setting does nothing unless setup calls it");
        for cmd in ["laya_local_set_autostart", "laya_local_set_mirror", "laya_local_sync_endpoints"] {
            assert!(lib.contains(&format!("laya_local::{cmd}")), "{cmd} must be registered");
        }
    }
}
