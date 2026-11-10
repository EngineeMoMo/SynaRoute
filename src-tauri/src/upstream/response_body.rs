use crate::error::{AppError, AppResult};
use bytes::{Bytes, BytesMut};
use futures_util::StreamExt;

pub(crate) const RESPONSE_LIMIT: usize = 64 * 1024 * 1024;
pub(crate) const ERROR_LIMIT: usize = 1024 * 1024;

pub(crate) async fn read(
    response: reqwest::Response,
    received: impl FnMut(usize),
) -> AppResult<Bytes> {
    let limit = if response.status().is_success() {
        RESPONSE_LIMIT
    } else {
        ERROR_LIMIT
    };
    read_limited(response, limit, received).await
}

pub(crate) async fn read_limited(
    response: reqwest::Response,
    limit: usize,
    mut received: impl FnMut(usize),
) -> AppResult<Bytes> {
    let oversized = || AppError::upstream_http(502, format!("上游响应体超过 {limit} 字节限制"));
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(oversized());
    }
    let mut stream = response.bytes_stream();
    let mut bytes = BytesMut::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        received(chunk.len());
        if chunk.len() > limit.saturating_sub(bytes.len()) {
            return Err(oversized());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes.freeze())
}

pub(crate) async fn text(response: reqwest::Response) -> AppResult<String> {
    let bytes = read(response, |_| {}).await?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn response(headers: &str, body: &str) -> reqwest::Response {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let wire = format!("HTTP/1.1 {headers}\r\nConnection: close\r\n\r\n{body}");
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            assert!(socket.read(&mut request).await.unwrap() > 0);
            socket.write_all(wire.as_bytes()).await.unwrap();
        });
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .get(format!("http://{address}"))
            .timeout(std::time::Duration::from_secs(2))
            .send()
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn rejects_declared_oversize_without_reading_body() {
        let response = response("200 OK\r\nContent-Length: 100", "").await;
        let error = read_limited(response, 10, |_| panic!("must reject before reading"))
            .await
            .unwrap_err();
        assert_eq!(error.upstream_status(), Some(502));
        assert!(error.to_string().contains("10 字节限制"));
    }

    #[tokio::test]
    async fn rejects_chunked_oversize() {
        let response = response(
            "200 OK\r\nTransfer-Encoding: chunked",
            "6\r\n123456\r\n6\r\n789012\r\n0\r\n\r\n",
        )
        .await;
        assert!(read_limited(response, 10, |_| {})
            .await
            .unwrap_err()
            .to_string()
            .contains("字节限制"));
    }

    #[tokio::test]
    async fn accepts_exact_limit_and_reports_progress() {
        let response = response(
            "200 OK\r\nTransfer-Encoding: chunked",
            "5\r\n12345\r\n5\r\n67890\r\n0\r\n\r\n",
        )
        .await;
        let mut received = 0;
        let bytes = read_limited(response, 10, |length| received += length)
            .await
            .unwrap();
        assert_eq!(bytes.as_ref(), b"1234567890");
        assert_eq!(received, 10);
    }

    #[tokio::test]
    async fn error_responses_have_a_smaller_limit() {
        let response = response(
            &format!(
                "500 Internal Server Error\r\nContent-Length: {}",
                ERROR_LIMIT + 1
            ),
            "",
        )
        .await;
        assert!(read(response, |_| {})
            .await
            .unwrap_err()
            .to_string()
            .contains(&ERROR_LIMIT.to_string()));
    }
}
