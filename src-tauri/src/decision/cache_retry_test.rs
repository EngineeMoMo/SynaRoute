use super::*;
use crate::upstream::{MultimodalPrompt, ToolDef, ToolSession, TurnParams};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn cache_rejection_preserves_effort_on_retry_and_followup() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/effort-retry", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let mut requests = Vec::new();
        for index in 0..3 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut received = Vec::new();
            let body = loop {
                let mut buf = [0; 8192];
                let n = stream.read(&mut buf).await.unwrap();
                assert!(n > 0);
                received.extend_from_slice(&buf[..n]);
                if let Some(end) = received.windows(4).position(|w| w == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&received[..end]).to_lowercase();
                    let len: usize = headers
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length:"))
                        .unwrap()
                        .trim()
                        .parse()
                        .unwrap();
                    if received.len() >= end + 4 + len {
                        break serde_json::from_slice::<Value>(&received[end + 4..end + 4 + len])
                            .unwrap();
                    }
                }
            };
            requests.push(body);
            let (status, response) = if index == 0 {
                (
                    "400 Bad Request",
                    r#"{"error":"unsupported cache_control"}"#,
                )
            } else {
                (
                    "200 OK",
                    r#"{"content":[{"type":"text","text":"ok"}],"stop_reason":"end_turn"}"#,
                )
            };
            stream.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).as_bytes()).await.unwrap();
        }
        requests
    });
    let key = ProviderKey {
        id: "retry-test".into(),
        protocol: Protocol::Anthropic,
        base_url,
        ..Default::default()
    };
    let mut session = ToolSession::new(
        Protocol::Anthropic,
        &MultimodalPrompt::from_text("fixture task"),
    );
    let params = TurnParams {
        key: &key,
        secret: "fixture",
        model: "model",
        max_tokens: Some(1024),
        retry: false,
        request_timeout: Duration::from_secs(3),
    };
    let tools = [ToolDef {
        name: "fixture_tool".into(),
        description: "Read fixture".into(),
        input_schema: json!({"type":"object","properties":{}}),
    }];
    EFFORT
        .scope(
            EffortScope {
                level: Some(Level::High),
                targets: BTreeMap::from([("retry-test::model".into(), EffortApi::Adaptive)]),
            },
            async {
                session.turn(&params, &tools).await.unwrap();
                session.push_user_note("followup");
                session.turn(&params, &tools).await.unwrap();
            },
        )
        .await;
    let requests = tokio::time::timeout(Duration::from_secs(3), server)
        .await
        .unwrap()
        .unwrap();
    assert!(requests[0].to_string().contains("cache_control"));
    for request in &requests[1..] {
        assert!(!request.to_string().contains("cache_control"));
    }
    for request in requests {
        assert_eq!(request["thinking"], json!({"type":"adaptive"}));
        assert_eq!(request["output_config"]["effort"], "high");
        assert_eq!(request["max_tokens"], 1024);
    }
}
