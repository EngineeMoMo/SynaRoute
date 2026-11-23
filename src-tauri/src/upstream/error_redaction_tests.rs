use super::*;
use crate::{model::{ModelInfo, Protocol, UserPrefs}, store::Store, upstream};
use http_body_util::{BodyExt, Full};
use hyper::{body::Incoming, service::service_fn, Request, Response};
use hyper_util::rt::TokioIo;
use std::{convert::Infallible, sync::Arc, time::Duration};

#[test]
fn decoded_values_keys_numbers_and_malformed_bodies_do_not_expose_credentials() {
    let key = ProviderKey { headers_json:Some(r#"{"x-tenant":"header-secret"}"#.into()), ..Default::default() };
    let redactor = ErrorRedactor::new(&key, "123456");
    let body = redactor.body(br#"{"header\u002dsecret":[123456,"header\u002dsecret","123456"]}"#);
    assert_eq!(serde_json::from_str::<Value>(&body).unwrap(), serde_json::json!({"***":["***","***","***"]}));
    for body in [r#"{"error":"header\u002dsecret"#, r#"data: {"error":"header\u002dsecret"}"#, r#"{"error":"123456"#] {
        assert_eq!(redactor.body(body.as_bytes()), "Upstream returned an invalid error body");
    }
    assert_eq!(redactor.body(b"ordinary diagnostic header-secret 123456"), "ordinary diagnostic *** ***");
}

#[tokio::test]
async fn errors_from_discovery_health_and_tool_sessions_use_shared_redaction() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let reply = Arc::new(parking_lot::RwLock::new((401, r#"{"error":{"message":"fixture\u002dapi\u002dsecret header\u002dsecret"}}"#.to_owned())));
    let response = reply.clone();
    let server = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else { break; };
            let response = response.clone();
            tokio::spawn(async move {
                let service = service_fn(move |request: Request<Incoming>| {
                    let response = response.clone();
                    async move {
                        let _ = request.into_body().collect().await;
                        let (status, body) = response.read().clone();
                        Ok::<_, Infallible>(Response::builder().status(status).header("content-type", "application/json")
                            .body(Full::new(bytes::Bytes::from(body))).unwrap())
                    }
                });
                let _ = hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(stream), service).await;
            });
        }
    });
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::credential_fixture(directory.path().join("config.json"), directory.path().join("secrets.enc")).unwrap());
    for protocol in [Protocol::Anthropic, Protocol::OpenaiChat, Protocol::OpenaiResponses] {
        let key = ProviderKey { id:uuid::Uuid::new_v4().to_string(), base_url:endpoint.clone(), protocol,
            headers_json:Some(r#"{"x-fixture":"header-secret"}"#.into()),
            models:vec![ModelInfo { real_name:"fixture-model".into(), source:"manual".into(), fetched_at:None, context_window:None, max_output_tokens:None }], ..Default::default() };
        crate::service::save_key(&store, key.clone()).unwrap();
        crate::service::save_secret(&store, &key.id, "fixture-api-secret").unwrap();
        for status in [401, 200] {
            reply.write().0 = status;
            let error = upstream::fetch_models(&key, "fixture-api-secret").await.unwrap_err().to_string();
            assert_sanitized(&error);
            let error = upstream::text_completion(&key, "fixture-api-secret", "fixture-model", "probe", Some(1), false, Duration::from_secs(5)).await.unwrap_err().to_string();
            assert_sanitized(&error);
            assert!(error.contains("***"));
            let mut session = upstream::ToolSession::new(protocol, &upstream::MultimodalPrompt::from_text("probe"));
            let params = upstream::TurnParams { key:&key, secret:"fixture-api-secret", model:"fixture-model", max_tokens:Some(1), retry:false, request_timeout:Duration::from_secs(5) };
            let error = session.turn(&params, &[]).await.unwrap_err().to_string();
            assert_sanitized(&error);
            assert!(error.contains("***"));
        }
        reply.write().0 = 401;
        let mut prefs = UserPrefs::from(&store.get_settings());
        prefs.health_probe_real_completion = true;
        store.save_settings(prefs).unwrap();
        crate::health::check_one(&store, &key.id).await;
        let events = store.list_events(key.category_id);
        let health = events.iter().find(|event| event.key_id.as_deref() == Some(&key.id) && event.kind == "health").unwrap();
        assert_sanitized(&health.detail);
        assert!(health.detail.contains("***"));
    }
    server.abort();
}

fn assert_sanitized(message: &str) {
    for forbidden in ["fixture-api-secret", "header-secret", r#"fixture\u002dapi\u002dsecret"#, r#"header\u002dsecret"#] {
        assert!(!message.contains(forbidden), "credential escaped error sanitization: {message}");
    }
}
