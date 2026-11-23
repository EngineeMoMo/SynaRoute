#![cfg(target_os = "windows")]

use serde_json::Value;
use std::{borrow::Cow, sync::Mutex, time::Duration};
use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};
use tauri::utils::assets::{AssetKey, AssetsIter, CspHash};

struct ProbeAssets(Box<dyn tauri::Assets<tauri::Wry>>);
impl tauri::Assets<tauri::Wry> for ProbeAssets {
    fn get(&self, key: &AssetKey) -> Option<Cow<'_, [u8]>> {
        let path: &str = key.as_ref();
        if path.trim_start_matches('/') == "security-probe.js" { return Some(Cow::Borrowed(include_bytes!("probe.js"))); }
        self.0.get(key)
    }
    fn iter(&self) -> Box<AssetsIter<'_>> { self.0.iter() }
    fn csp_hashes(&self, path: &AssetKey) -> Box<dyn Iterator<Item = CspHash<'_>> + '_> { self.0.csp_hashes(path) }
}

#[derive(Default)]
struct Report(Mutex<Option<Value>>);

#[tauri::command]
fn csp_report(app: tauri::AppHandle, report: tauri::State<Report>, result: Value) {
    *report.0.lock().unwrap() = Some(result);
    app.exit(0);
}

#[test]
#[ignore = "requires Windows WebView2 and built frontend assets"]
fn production_csp_allows_assets_and_ipc_but_blocks_injected_scripts() {
    let directory = tempfile::tempdir().unwrap();
    let profile = directory.path().join("webview");
    let mut context = tauri::generate_context!();
    context.assets = Box::new(ProbeAssets(context.assets));
    context.config_mut().app.windows.clear();
    let app = tauri::Builder::default().any_thread()
        .manage(Report::default())
        .invoke_handler(tauri::generate_handler![csp_report])
        .setup(move |app| {
            WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
                .data_directory(profile).incognito(true).visible(false)
                .on_page_load(|webview, payload| {
                    if payload.event() == tauri::webview::PageLoadEvent::Finished {
                        webview.eval("{ const probe = document.createElement('script'); probe.src = new URL('security-probe.js', location.href).href; document.head.append(probe); }").unwrap();
                    }
                }).build()?;
            let handle = app.handle().clone();
            std::thread::spawn(move || { std::thread::sleep(Duration::from_secs(40)); handle.exit(2); });
            Ok(())
        }).build(context).unwrap();
    let handle = app.handle().clone();
    assert_eq!(app.run_return(|_, _| {}), 0, "CSP probe timed out");
    let report = handle.state::<Report>().0.lock().unwrap().clone().expect("IPC report missing");
    println!("SECURITY_WEBVIEW={report}");
    for flag in ["inlineBlocked", "dataScriptBlocked", "evalBlocked", "connectionBlocked", "assetsLoaded", "rootRendered", "dataImageLoaded"] {
        assert_eq!(report[flag], true, "{flag}: {report}");
    }
}
