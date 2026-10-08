//! The embedded installer is available in packaged apps without a source checkout.
use serde::{Deserialize, Serialize};
#[cfg(any(windows, test))]
#[path = "laya_policy.rs"]
mod policy;
#[cfg(windows)]
#[path = "laya_windows.rs"]
mod windows;

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub supported: bool,
    pub phase: String,
    pub endpoint: String,
    pub logs: Vec<String>,
    pub error: Option<String>,
    #[serde(default)]
    pub error_code: Option<String>,
    #[serde(default)]
    pub elapsed_seconds: u64,
}
impl Default for Status {
    fn default() -> Self {
        Self {
            supported: cfg!(windows),
            phase: "idle".into(),
            endpoint: "http://127.0.0.1:8000/v1/systemone".into(),
            logs: Vec::new(),
            error: None,
            error_code: None,
            elapsed_seconds: 0,
        }
    }
}

#[tauri::command]
pub fn laya_local_status() -> Status {
    #[cfg(windows)]
    {
        windows::status()
    }
    #[cfg(not(windows))]
    {
        Status::default()
    }
}
#[tauri::command]
pub async fn laya_local_start(repair: Option<bool>) -> Result<Status, String> {
    #[cfg(windows)]
    {
        windows::start(repair.unwrap_or(false)).await
    }
    #[cfg(not(windows))]
    {
        let _ = repair;
        Err("一键部署目前支持 Windows / one-click setup currently supports Windows".into())
    }
}
#[tauri::command]
pub async fn laya_local_stop() -> Result<Status, String> {
    #[cfg(windows)]
    {
        windows::stop().await
    }
    #[cfg(not(windows))]
    {
        Ok(Status::default())
    }
}
pub(crate) async fn shutdown() {
    let _ = laya_local_stop().await;
}
