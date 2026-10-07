//! The listener owns its connections: aborting it drops the JoinSet and closes them.
use super::*;

pub(super) async fn serve(listener: TcpListener, store: Arc<Store>) {
    let mut connections = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            biased;
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else { break };
                if connections.len() >= crate::http_ingress::CONNECTION_LIMIT {
                    drop(stream);
                    continue;
                }
                let store = store.clone();
                connections.spawn(async move {
                    let svc = service_fn(move |req| handle_http(store.clone(), req));
                    let _ = crate::http_ingress::http1()
                        .serve_connection(TokioIo::new(stream), svc).await;
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn stopping_listener_closes_existing_keepalive_connections() {
        let (dir, store) = super::super::tests::tmp_store("stop_connections");
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let mut stream = tokio::net::TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let task = tokio::spawn(serve(listener, store));
        stream
            .write_all(b"GET /mcp HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        let mut buf = [0; 1024];
        let n = tokio::time::timeout(std::time::Duration::from_secs(2), stream.read(&mut buf))
            .await
            .unwrap()
            .unwrap();
        assert!(String::from_utf8_lossy(&buf[..n]).contains("405"));
        task.abort();
        let _ = task.await;
        // A server-side close may appear as EOF or a reset on Windows.
        let closed = tokio::time::timeout(std::time::Duration::from_secs(2), stream.read(&mut buf))
            .await
            .unwrap();
        assert!(
            matches!(closed, Ok(0) | Err(_)),
            "connection remained live: {closed:?}"
        );
        std::fs::remove_dir_all(dir).ok();
    }
}
