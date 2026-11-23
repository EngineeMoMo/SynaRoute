use super::*;
use crate::{model::{ModelInfo, Protocol}, proxy::ProxyManager, store::routing::{self, config, identity::{Hints, VerifiedPeer}, policy}};
use http_body_util::{BodyExt, Full};
use hyper::{body::Incoming, service::service_fn, Request, Response};
use hyper_util::rt::TokioIo;
use serde_json::{json, Value};
use std::{convert::Infallible, time::Duration};

struct Fixture {
    directory: tempfile::TempDir,
    store: Arc<Store>,
    key: ProviderKey,
    model: String,
}
impl Fixture {
    fn new(endpoint: String) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::credential_fixture(directory.path().join("config.json"), directory.path().join("secrets.enc")).unwrap());
        let key = ProviderKey { id:uuid::Uuid::new_v4().to_string(), category_id:CategoryType::Codex, enabled:true, protocol:Protocol::OpenaiResponses, base_url:endpoint,
            headers_json:Some(r#"{"x-tenant":"tenant-a"}"#.into()),
            models:vec![ModelInfo { real_name:"fixture-model".into(), source:"manual".into(), fetched_at:None, context_window:None, max_output_tokens:None }], ..Default::default() };
        crate::service::save_key(&store, key.clone()).unwrap();
        crate::service::save_secret(&store, &key.id, "fixture-secret-a").unwrap();
        let group = config::RouteGroup { id:uuid::Uuid::new_v4().to_string(), name:"Fixture".into(), enabled:true, default_model:"fixture-model".into(),
            judge:Some(config::ModelRef { key_id:key.id.clone(), model:"fixture-model".into() }), rules:Vec::new() };
        let model = group.model_id();
        routing::save_policy(&store, key.category_id, config::CategoryPolicy { groups:vec![group], ..Default::default() }, None, 0).unwrap();
        Self { directory, store, key, model }
    }

    async fn prepare(&self, previous: Option<&str>) -> Result<policy::Decision, Box<Response<crate::proxy::ResBody>>> {
        let mut request = Request::builder().header("thread-id", "identity-fixture").body(()).unwrap();
        request.extensions_mut().insert(VerifiedPeer("127.0.0.1".parse().unwrap()));
        let mut body = json!({"model":self.model,"input":"continue"});
        if let Some(previous) = previous { body["previous_response_id"] = json!(previous); }
        policy::prepare(&self.store, self.key.category_id, &body, self.key.protocol, &Hints::from_request(&request), &self.model, None).await
    }
}

#[tokio::test]
async fn identity_changes_reject_existing_and_late_references_but_same_secret_does_not() {
    for change in ["credential", "tenant", "credential-restored", "tenant-restored", "direct-import"] {
        let fixture = Fixture::new("http://127.0.0.1:1".into());
        let first = fixture.prepare(None).await.unwrap();
        first.observe_response(&fixture.store, &fixture.key, "fixture-model", fixture.key.protocol, &json!({"id":"resp_first","status":"completed"}));
        crate::service::save_secret(&fixture.store, &fixture.key.id, "fixture-secret-a").unwrap();
        assert!(fixture.prepare(Some("resp_first")).await.is_ok());
        let pending = fixture.prepare(Some("resp_first")).await.unwrap();
        if change.starts_with("credential") {
            crate::service::save_secret(&fixture.store, &fixture.key.id, "fixture-secret-b").unwrap();
            if change.ends_with("restored") { crate::service::save_secret(&fixture.store, &fixture.key.id, "fixture-secret-a").unwrap(); }
        } else if change.starts_with("tenant") {
            let mut key = fixture.key.clone();
            key.headers_json = Some(r#"{"x-tenant":"tenant-b"}"#.into());
            crate::service::save_key(&fixture.store, key).unwrap();
            if change.ends_with("restored") { crate::service::save_key(&fixture.store, fixture.key.clone()).unwrap(); }
        } else { fixture.store.secrets.write().set(&fixture.key.id, "fixture-secret-b").unwrap(); }
        assert!(secret_for_request(&fixture.store, &fixture.key, pending.lease.as_ref()).is_err(), "{change}");
        pending.observe_response(&fixture.store, &fixture.key, "fixture-model", fixture.key.protocol, &json!({"id":"resp_late","status":"completed"}));
        for reference in ["resp_first", "resp_late"] {
            assert_eq!(fixture.prepare(Some(reference)).await.err().expect("old reference rejected").status(), 422, "{change}: {reference}");
        }
    }
}

#[tokio::test]
async fn encryption_and_non_identity_edits_preserve_observed_references() {
    let fixture = Fixture::new("http://127.0.0.1:1".into());
    let first = fixture.prepare(None).await.unwrap();
    first.observe_response(&fixture.store, &fixture.key, "fixture-model", fixture.key.protocol, &json!({"id":"resp_first","status":"completed"}));
    let mut key = fixture.key.clone();
    key.name = "renamed".into();
    crate::service::save_key(&fixture.store, key).unwrap();
    #[cfg(not(target_os = "linux"))]
    fixture.store.secrets.write().enable_master_password("FixtureStrong2026!").unwrap();
    assert!(fixture.prepare(Some("resp_first")).await.is_ok());
    assert!(fixture.directory.path().exists());
}

#[tokio::test]
async fn real_http_continuations_never_forward_old_ids_with_new_credentials_or_tenants() {
    for streaming in [false, true] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let captured = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let captures = captured.clone();
        let server = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else { break; };
                let captures = captures.clone();
                tokio::spawn(async move {
                    let service = service_fn(move |request: Request<Incoming>| {
                        let captures = captures.clone();
                        async move {
                            let auth = request.headers()["authorization"].to_str().unwrap().to_owned();
                            let tenant = request.headers()["x-tenant"].to_str().unwrap().to_owned();
                            let body: Value = serde_json::from_slice(&request.into_body().collect().await.unwrap().to_bytes()).unwrap();
                            let number = { let mut captures = captures.lock(); captures.push((auth, tenant, body)); captures.len() };
                            let response = json!({"id":format!("resp_{number}"),"object":"response","model":"fixture-model","status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"ok"}]}]});
                            let (mime, body) = if streaming { ("text/event-stream", format!("event: response.completed\ndata: {}\n\n", json!({"type":"response.completed","response":response}))) }
                                else { ("application/json", response.to_string()) };
                            Ok::<_, Infallible>(Response::builder().header("content-type", mime).body(Full::new(bytes::Bytes::from(body))).unwrap())
                        }
                    });
                    let _ = hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(stream), service).await;
                });
            }
        });
        let fixture = Fixture::new(endpoint);
        fixture.store.set_proxy_port(fixture.key.category_id, 0).unwrap();
        let proxy = ProxyManager::new(fixture.store.clone());
        let port = proxy.start(fixture.key.category_id).await.unwrap();
        let client = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(5)).build().unwrap();
        for (index, previous) in [None, Some("resp_1"), None, Some("resp_2"), None].into_iter().enumerate() {
            if index == 1 { crate::service::save_secret(&fixture.store, &fixture.key.id, "fixture-secret-b").unwrap(); }
            if index == 3 {
                let mut key = fixture.key.clone(); key.headers_json = Some(r#"{"x-tenant":"tenant-b"}"#.into());
                crate::service::save_key(&fixture.store, key).unwrap();
            }
            let mut body = json!({"model":fixture.model,"input":"continue","stream":streaming});
            if let Some(previous) = previous { body["previous_response_id"] = json!(previous); }
            let response = client.post(format!("http://127.0.0.1:{port}/v1/responses")).header("thread-id", "http-identity-fixture").json(&body).send().await.unwrap();
            assert_eq!(response.status().as_u16(), if previous.is_some() { 422 } else { 200 });
            response.bytes().await.unwrap();
        }
        proxy.stop_all(); server.abort();
        let captured = captured.lock();
        assert_eq!(captured.len(), 3);
        assert!(captured.iter().all(|(_, _, body)| body.get("previous_response_id").is_none()));
        assert_eq!(captured[1].0, "Bearer fixture-secret-b");
        assert_eq!(captured[2].1, "tenant-b");
    }
}
