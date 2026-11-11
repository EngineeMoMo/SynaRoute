use super::*;
use crate::error::AppError;
use std::time::Duration;

fn isolated_store(root: &std::path::Path) -> Arc<Store> {
    let store =
        Arc::new(Store::new_at(root.join("config.json"), root.join("secrets.enc")).unwrap());
    let mut settings = store.get_settings();
    settings.log_dir = Some(root.join("logs").display().to_string());
    store
        .save_settings(crate::model::UserPrefs::from(&settings))
        .unwrap();
    store
}

fn success(analysis: &str) -> crate::error::AppResult<aggregate::McpAggregateResult> {
    Ok(aggregate::McpAggregateResult {
        analysis: analysis.into(),
        work_dir: None,
        member_labels: vec![],
        decider_ref: String::new(),
        file_count: 0,
        members_attempted: 0,
        members_failed: 0,
        members_skipped_disabled: 0,
    })
}

#[tokio::test]
async fn default_disabled_trace_never_persists_mcp_bodies() {
    let directory = tempfile::tempdir().unwrap();
    let store = isolated_store(directory.path());
    let marker = "synthetic-private-mcp-body";
    assert!(!store.get_settings().aggregate_trace_enabled);
    let result = handle_tool_call(
        &store,
        json!({"name":"synaroute_ai","arguments":{"prompt":marker}}),
        McpCaller::Bound(CategoryType::ClaudeCli),
    )
    .await
    .unwrap();
    assert_eq!(result["isError"], true);
    for outcome in [success(marker), Err(AppError::Other(marker.into()))] {
        logging::record_outcome(
            &store,
            CategoryType::ClaudeCli,
            marker,
            Some(marker),
            &outcome,
            1,
        );
    }
    let events = store.list_events(CategoryType::ClaudeCli);
    assert!(events
        .iter()
        .filter(|event| event.kind == "mcp")
        .all(|event| !event.has_trace && store.event_trace(&event.id).is_none()));
    assert!(!serde_json::to_string(&events).unwrap().contains(marker));
    store.flush_logs();
    for entry in std::fs::read_dir(directory.path().join("logs"))
        .unwrap()
        .flatten()
    {
        if entry.file_type().unwrap().is_file() {
            assert!(!std::fs::read_to_string(entry.path())
                .unwrap()
                .contains(marker));
        }
    }
}

#[test]
fn enabled_trace_bounds_both_success_and_error_bodies() {
    let directory = tempfile::tempdir().unwrap();
    let store = isolated_store(directory.path());
    let mut settings = store.get_settings();
    settings.aggregate_trace_enabled = true;
    store
        .save_settings(crate::model::UserPrefs::from(&settings))
        .unwrap();
    let long_text = "文".repeat(25_000);
    for outcome in [success(&long_text), Err(AppError::Other(long_text.clone()))] {
        logging::record_outcome(
            &store,
            CategoryType::ClaudeCli,
            &long_text,
            None,
            &outcome,
            1,
        );
    }
    let events = store.list_events(CategoryType::ClaudeCli);
    let traces: Vec<_> = events
        .iter()
        .filter(|event| event.kind == "mcp")
        .map(|event| store.event_trace(&event.id).unwrap())
        .collect();
    assert_eq!(traces.len(), 2);
    for trace in traces {
        assert!(trace.request_body.chars().count() < 20_100);
        assert!(trace.response_body.chars().count() < 20_100);
        assert!(trace.request_body.contains("已截断"));
        assert!(trace.response_body.contains("已截断"));
    }
}

async fn assert_closed(ports: &[u16]) {
    for _attempt in 0..100 {
        let mut any_live = false;
        for port in ports {
            any_live |= tokio::net::TcpStream::connect(("127.0.0.1", *port))
                .await
                .is_ok();
        }
        if !any_live {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("MCP left an untracked listener: {ports:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn concurrent_starts_share_one_fallback_listener_and_stop_closes_it() {
    let directory = tempfile::tempdir().unwrap();
    let manager = Arc::new(McpManager::new(isolated_store(directory.path())));
    let occupied = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let preferred = occupied.local_addr().unwrap().port();
    let barrier = Arc::new(tokio::sync::Barrier::new(8));
    let mut tasks = Vec::new();
    for _attempt in 0..8 {
        let manager = manager.clone();
        let barrier = barrier.clone();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            manager.start(preferred).await.unwrap()
        }));
    }
    let mut ports = Vec::new();
    for task in tasks {
        ports.push(task.await.unwrap());
    }
    assert!(ports.iter().all(|port| *port == ports[0]));
    assert_ne!(ports[0], preferred);
    manager.stop();
    assert!(!manager.is_running());
    assert_closed(&ports).await;
}

#[tokio::test]
async fn dropping_manager_closes_owned_listener() {
    let directory = tempfile::tempdir().unwrap();
    let manager = McpManager::new(isolated_store(directory.path()));
    let port = manager.start(0).await.unwrap();
    drop(manager);
    assert_closed(&[port]).await;
}
