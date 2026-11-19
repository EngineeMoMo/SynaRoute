#[cfg(all(target_os = "windows", target_arch = "x86_64", target_env = "msvc"))]
#[test]
#[ignore = "requires a fresh isolated home, WebView2 and the verified offline model"]
fn real_webview_managed_model() {
    synaroute_lib::native_acceptance::run();
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires a fresh isolated home and WebKitGTK display"]
fn real_webview_session_archive() {
    synaroute_lib::native_acceptance::run();
}
