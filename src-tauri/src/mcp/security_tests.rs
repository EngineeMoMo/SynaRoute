//! Exercise the actual HTTP handlers with harmless requests, without upstream credentials.
use super::*;
use http_body_util::BodyExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn both_services_reject_untrusted_origins_and_unbounded_bodies() {
    for mcp in [true, false] {
        let (dir, store) =
            super::tests::tmp_store(if mcp { "mcp_ingress" } else { "proxy_ingress" });
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let store = store.clone();
                connections.spawn(async move {
                    let svc = service_fn(move |req| {
                        let store = store.clone();
                        async move {
                            if mcp {
                                handle_http(store, req).await.map(|res| {
                                    res.map(|body| {
                                        body.map_err(|never| -> std::io::Error { match never {} })
                                            .boxed()
                                    })
                                })
                            } else {
                                crate::proxy::handle_request(
                                    store,
                                    CategoryType::Codex,
                                    "ingress-test".into(),
                                    req,
                                )
                                .await
                            }
                        }
                    });
                    let _ = crate::http_ingress::http1()
                        .serve_connection(TokioIo::new(stream), svc)
                        .await;
                });
            }
        });
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(std::time::Duration::from_secs(3))
            .build()
            .unwrap();
        let url = format!("http://{addr}/{}", if mcp { "mcp" } else { "v1/responses" });
        for origin in ["null", "https://evil.example", "app://evil", ""] {
            let resp = client
                .post(&url)
                .header("origin", origin)
                .header("content-type", "text/plain")
                .body("{}")
                .send()
                .await
                .unwrap();
            assert_eq!(
                resp.status(),
                StatusCode::FORBIDDEN,
                "mcp={mcp}, origin={origin}"
            );
        }
        let resp = client
            .post(&url)
            .header("content-type", "text/plain")
            .body("{}")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
        let resp = client.put(&url).json(&json!({})).send().await.unwrap();
        assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
        // Native callers still work; the proxy's empty model catalogue needs no keys.
        let resp = if mcp {
            client
                .post(&url)
                .json(&json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}))
                .send()
                .await
                .unwrap()
        } else {
            client
                .get(format!("http://{addr}/v1/models"))
                .send()
                .await
                .unwrap()
        };
        assert_eq!(resp.status(), StatusCode::OK);
        let limit = if mcp {
            crate::http_ingress::MCP_BODY_LIMIT
        } else {
            crate::http_ingress::PROXY_BODY_LIMIT
        };
        let mut socket = tokio::net::TcpStream::connect(addr).await.unwrap();
        socket.write_all(format!("POST /mcp HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n", limit + 1).as_bytes()).await.unwrap();
        let mut buf = [0; 1024];
        let n = tokio::time::timeout(std::time::Duration::from_secs(2), socket.read(&mut buf))
            .await
            .unwrap()
            .unwrap();
        assert!(
            String::from_utf8_lossy(&buf[..n]).contains("413"),
            "mcp={mcp}"
        );
        task.abort();
        let _ = task.await;
        std::fs::remove_dir_all(dir).ok();
    }
}
