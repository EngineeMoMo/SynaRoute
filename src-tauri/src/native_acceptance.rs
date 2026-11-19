use crate::store::decision::laya_local;
use crate::tools::codex::codex_sessions::{archive, ops};
use serde_json::{json, Value};
use std::{fs, path::PathBuf, sync::{Arc, Mutex}, time::Duration};
use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};

struct Probe(Mutex<Option<Value>>);

#[tauri::command]
fn probe_bundle() -> Result<String, String> {
    if !cfg!(windows) { return Ok(String::new()); }
    std::env::var("SYNAROUTE_PROBE_BUNDLE").map_err(|error| error.to_string())
}

#[tauri::command]
async fn probe_inference() -> Result<Value, String> {
    let endpoint = laya_local::laya_local_status().endpoint;
    let baseline = std::env::var_os("SYNAROUTE_PROBE_BASELINE").ok_or("Missing experiment baseline")?;
    let baseline: Value = serde_json::from_slice(&fs::read(baseline).map_err(|error| error.to_string())?).map_err(|error| error.to_string())?;
    let tasks: Value = serde_json::from_str(include_str!("../../scripts/laya/comparison-tasks.json")).unwrap();
    let client = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(15)).build().map_err(|error| error.to_string())?;
    let mut matched = 0;
    let mut latencies = Vec::new();
    for task in tasks["tasks"].as_array().unwrap() {
        let body = json!({"model":"multilingual", "state":task["text"], "questions":{"deep":{"type":"noul","instructions":tasks["question"]}}});
        let started = std::time::Instant::now();
        let response = client.post(&endpoint).json(&body).send().await.map_err(|error| error.to_string())?.error_for_status().map_err(|error| error.to_string())?;
        let result: Value = response.json().await.map_err(|error| error.to_string())?;
        latencies.push(started.elapsed().as_millis());
        let reference = baseline["results"].as_array().unwrap().iter().find(|row| row["id"] == task["id"]).ok_or("baseline missing task")?;
        let score = result["answers"]["deep"]["noul"].as_f64().ok_or("missing score")?;
        if (score - reference["score"].as_f64().unwrap()).abs() > 0.00005 || result["usage"]["input_tokens"] != reference["input_tokens"] || result["usage"]["truncated"] != false {
            return Err(format!("inference mismatch on {}", task["id"]));
        }
        matched += 1;
    }
    let long = json!({"model":"multilingual","state":"many tokens ".repeat(2000),"questions":{"deep":{"type":"noul","instructions":tasks["question"]}}});
    let truncated: Value = client.post(&endpoint).json(&long).send().await.map_err(|error| error.to_string())?.json().await.map_err(|error| error.to_string())?;
    if matched != 48 || truncated["usage"]["truncated"] != true || truncated["usage"]["state_tokens_dropped"].as_u64().unwrap_or(0) == 0 { return Err("truncation or sample coverage failed".into()); }
    latencies.sort_unstable();
    Ok(json!({"matched":matched,"p50Ms":latencies[latencies.len()/2],"truncated":true}))
}

#[tauri::command]
async fn probe_stopped() -> bool {
    let endpoint = laya_local::laya_local_status().endpoint;
    let port = url::Url::parse(&endpoint).unwrap().port().unwrap();
    tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_err()
}

#[tauri::command]
fn probe_done(app: tauri::AppHandle, state: tauri::State<Probe>, report: Value) {
    *state.0.lock().unwrap() = Some(report);
    app.exit(0);
}

pub fn run() {
    let root = PathBuf::from(std::env::var_os("SYNAROUTE_PROBE_ROOT").expect("fresh root required"));
    assert!(root.is_absolute());
    fs::create_dir(&root).expect("root must be new");
    let data = root.join("app");
    let home = root.join("codex");
    assert_eq!(std::env::var_os("SYNAROUTE_DATA_DIR").unwrap(), data.as_os_str());
    assert_eq!(std::env::var_os("CODEX_HOME").unwrap(), home.as_os_str());
    let id = uuid::Uuid::new_v4().to_string();
    let sessions = home.join("sessions/2026/11/18");
    fs::create_dir_all(&sessions).unwrap();
    let rollout = sessions.join(format!("rollout-2026-11-18T10-00-00-{id}.jsonl"));
    let original = format!("{}\n", json!({"type":"session_meta","payload":{"id":id,"model_provider":"fixture","thread_source":"user"}}));
    fs::write(&rollout, &original).unwrap();
    let database = rusqlite::Connection::open(home.join("state_5.sqlite")).unwrap();
    database.execute_batch("CREATE TABLE threads(id TEXT PRIMARY KEY, archived INTEGER NOT NULL DEFAULT 0, archived_at INTEGER, title TEXT);").unwrap();
    database.execute("INSERT INTO threads(id,title) VALUES(?1,'Native fixture')", [&id]).unwrap();
    drop(database);
    let store = Arc::new(crate::Store::init().unwrap());
    let state = crate::AppState {proxy:Arc::new(crate::ProxyManager::new(store.clone())),mcp:Arc::new(crate::McpManager::new(store.clone())),store};
    let profile = root.join("webview");
    let app = tauri::Builder::default().any_thread().manage(state).manage(Probe(Mutex::new(None)))
        .invoke_handler(tauri::generate_handler![probe_bundle,probe_inference,probe_stopped,probe_done,
            ops::list_codex_sessions,archive::set_codex_sessions_archived,crate::proxy::active_requests::list_active_requests,
            laya_local::laya_local_status,laya_local::laya_local_start,laya_local::laya_local_stop,laya_local::laya_local_set_backend,
            laya_local::laya_local_import_bundle,laya_local::laya_local_rollback_bundle])
        .setup(move |app| {
            WebviewWindowBuilder::new(app,"main",WebviewUrl::App("index.html".into())).data_directory(profile).visible(false).build()?;
            let handle = app.handle().clone();
            std::thread::spawn(move || { std::thread::sleep(Duration::from_secs(240)); handle.exit(2); });
            Ok(())
        }).build(tauri::generate_context!("testdata/native-acceptance/tauri.conf.json")).unwrap();
    let handle = app.handle().clone();
    assert_eq!(app.run_return(|_, _| {}), 0, "native probe timed out");
    let report = handle.state::<Probe>().0.lock().unwrap().take().expect("missing WebView report");
    println!("NATIVE_ACCEPTANCE={report}");
    assert!(report.get("error").is_none(), "{report}");
    if cfg!(windows) {
        assert_eq!(report["pythonRestored"], true);
        assert_eq!(report["rollback"], true);
    } else {
        assert_eq!(report["archive"], "restored");
    }
    assert_eq!(fs::read_to_string(rollout).unwrap(), original);
    let database = rusqlite::Connection::open_with_flags(home.join("state_5.sqlite"), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let archived: (i64, Option<i64>) = database.query_row("SELECT archived, archived_at FROM threads WHERE id = ?1", [&id], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
    assert_eq!(archived, (0, None));
}
