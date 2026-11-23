use crate::{model::{CategoryType, ModelInfo, Protocol, ProviderKey}, proxy::ProxyManager, store::{routing, Store}};
use http_body_util::{BodyExt, Full};
use hyper::{body::Incoming, service::service_fn, Request, Response};
use hyper_util::rt::TokioIo;
use serde_json::{json, Value};
use std::{convert::Infallible, sync::{Arc, atomic::{AtomicUsize, Ordering}}, time::Duration};

#[test]
fn failed_response_references_cannot_be_restored_or_clear_another_request() {
    let runtime = super::Runtime::default();
    let category = CategoryType::Codex;
    let first = runtime.begin(category, "session", "turn", 1, "config", routing::config::AffinityMode::Session).unwrap();
    runtime.choose(&first, "model");
    runtime.observe_response(&first, "resp_first", "key", "real");
    let second = runtime.begin(category, "session", "turn", 1, "config", routing::config::AffinityMode::Session).unwrap();
    runtime.observe_response(&second, "resp_second", "key", "real");
    runtime.failed(&first);
    runtime.observe_response(&first, "resp_first", "key", "real");
    assert!(runtime.response_for(category, "session", "resp_first", "config").is_none());
    assert!(runtime.response_for(category, "session", "resp_second", "config").is_some());
    runtime.reset(category);
    runtime.observe_response(&second, "resp_second", "key", "real");
    assert!(runtime.response_for(category, "session", "resp_second", "config").is_none());
}

#[tokio::test]
async fn real_http_observes_native_responses_ids_for_json_and_sse() {
    for streaming in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::credential_fixture(directory.path().join("config.json"), directory.path().join("secrets.enc")).unwrap());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let hits = Arc::new(AtomicUsize::new(0));
        let observed = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let count = hits.clone(); let bodies = observed.clone();
        let server = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else { break; };
                let count = count.clone(); let bodies = bodies.clone();
                tokio::spawn(async move {
                    let service = service_fn(move |request: Request<Incoming>| {
                        let count = count.clone(); let bodies = bodies.clone();
                        async move {
                            let body: Value = serde_json::from_slice(&request.into_body().collect().await.unwrap().to_bytes()).unwrap();
                            bodies.lock().push(body);
                            let number = count.fetch_add(1, Ordering::SeqCst) + 1;
                            let response = json!({"id":format!("resp_{number}"),"object":"response","model":"fixture-model","status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"ok"}]}]});
                            let (mime, wire) = if streaming {
                                ("text/event-stream", format!("event: response.completed\ndata: {}\n\n", json!({"type":"response.completed","response":response})))
                            } else { ("application/json", response.to_string()) };
                            Ok::<_, Infallible>(Response::builder().header("content-type", mime).body(Full::new(bytes::Bytes::from(wire))).unwrap())
                        }
                    });
                    let _ = hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(stream), service).await;
                });
            }
        });
        let category = CategoryType::Codex;
        let key = ProviderKey { id:uuid::Uuid::new_v4().to_string(), category_id:category, enabled:true, protocol:Protocol::OpenaiResponses, base_url:endpoint,
            models:vec![ModelInfo { real_name:"fixture-model".into(), source:"manual".into(), fetched_at:None, context_window:None, max_output_tokens:None }], ..Default::default() };
        store.upsert_key(key.clone()).unwrap(); store.secrets.write().set(&key.id, "fixture-secret").unwrap();
        let group = routing::config::RouteGroup { id:uuid::Uuid::new_v4().to_string(), name:"Fixture".into(), enabled:true, default_model:"fixture-model".into(),
            judge:Some(routing::config::ModelRef { key_id:key.id.clone(), model:"fixture-model".into() }), rules:Vec::new() };
        let model = group.model_id();
        routing::save_policy(&store, category, routing::config::CategoryPolicy { groups:vec![group], ..Default::default() }, None, 0).unwrap();
        store.set_proxy_port(category, 0).unwrap();
        let proxy = ProxyManager::new(store.clone()); let port = proxy.start(category).await.unwrap();
        let client = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(5)).build().unwrap();
        for previous in [None, Some("resp_1"), Some("resp_2"), Some("unknown")] {
            let mut body = json!({"model":model,"input":"continue","stream":streaming});
            if let Some(previous) = previous { body["previous_response_id"] = json!(previous); }
            let response = client.post(format!("http://127.0.0.1:{port}/v1/responses"))
                .header("thread-id", "native-http-fixture").json(&body).send().await.unwrap();
            assert_eq!(response.status().as_u16(), if previous == Some("unknown") { 422 } else { 200 });
            response.bytes().await.unwrap();
        }
        assert_eq!(hits.load(Ordering::SeqCst), 3);
        let bodies = observed.lock();
        assert!(bodies.iter().all(|body| body["model"] == "fixture-model"));
        assert_eq!(bodies[1]["previous_response_id"], "resp_1");
        assert_eq!(bodies[2]["previous_response_id"], "resp_2");
        drop(bodies); proxy.stop_all(); server.abort();
    }
}
