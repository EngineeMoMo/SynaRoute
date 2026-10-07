//! Shared limits and browser-origin checks for the local HTTP services.
use bytes::Bytes;
use http_body_util::{BodyExt, Limited};
use hyper::{body::Body, HeaderMap, StatusCode};
use std::time::Duration;

pub(crate) const PROXY_BODY_LIMIT: usize = 32 * 1024 * 1024;
pub(crate) const MCP_BODY_LIMIT: usize = 1024 * 1024;
pub(crate) const CONNECTION_LIMIT: usize = 64;
const BODY_TIMEOUT: Duration = Duration::from_secs(30);
static BODY_READERS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);

pub(crate) fn origin_allowed(origin: Option<&str>) -> bool {
    let Some(origin) = origin else { return true };
    // Opaque origins (including sandboxed remote pages) must never be trusted.
    if origin == "tauri://localhost" {
        return true;
    }
    let Ok(url) = url::Url::parse(origin) else {
        return false;
    };
    matches!(url.scheme(), "http" | "https")
        && matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"))
        && url.username().is_empty()
        && url.password().is_none()
        && url.path() == "/"
        && url.query().is_none()
        && url.fragment().is_none()
        && !origin.chars().any(char::is_whitespace)
}

pub(crate) fn check_origin(headers: &HeaderMap) -> Result<(), StatusCode> {
    let mut origins = headers.get_all(hyper::header::ORIGIN).iter();
    let first = origins.next();
    if origins.next().is_some() {
        return Err(StatusCode::FORBIDDEN);
    }
    let origin = match first {
        Some(value) => Some(value.to_str().map_err(|_| StatusCode::FORBIDDEN)?),
        None => None,
    };
    if origin_allowed(origin) {
        Ok(())
    } else {
        Err(StatusCode::FORBIDDEN)
    }
}

pub(crate) fn check_json_type(headers: &HeaderMap) -> Result<(), StatusCode> {
    // Native clients historically omit this header. Explicit non-JSON bodies are rejected.
    let Some(value) = headers.get(hyper::header::CONTENT_TYPE) else {
        return Ok(());
    };
    let media = value
        .to_str()
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim();
    if media.eq_ignore_ascii_case("application/json") {
        Ok(())
    } else {
        Err(StatusCode::UNSUPPORTED_MEDIA_TYPE)
    }
}

pub(crate) fn check_json_request(
    method: &hyper::Method,
    headers: &HeaderMap,
) -> Result<(), StatusCode> {
    if *method != hyper::Method::POST {
        return Err(StatusCode::METHOD_NOT_ALLOWED);
    }
    check_json_type(headers)
}

pub(crate) fn http1() -> hyper::server::conn::http1::Builder {
    let mut builder = hyper::server::conn::http1::Builder::new();
    builder
        .timer(hyper_util::rt::TokioTimer::new())
        .header_read_timeout(Duration::from_secs(15));
    builder
}

pub(crate) async fn read_body<B>(body: B, limit: usize) -> Result<Bytes, StatusCode>
where
    B: Body<Data = Bytes>,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let _permit = BODY_READERS
        .try_acquire()
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    read_body_with_timeout(body, limit, BODY_TIMEOUT).await
}

async fn read_body_with_timeout<B>(
    body: B,
    limit: usize,
    timeout: Duration,
) -> Result<Bytes, StatusCode>
where
    B: Body<Data = Bytes>,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    if body.size_hint().lower() > limit as u64 {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }
    match tokio::time::timeout(timeout, Limited::new(body, limit).collect()).await {
        Err(_) => Err(StatusCode::REQUEST_TIMEOUT),
        Ok(Err(e)) if e.is::<http_body_util::LengthLimitError>() => {
            Err(StatusCode::PAYLOAD_TOO_LARGE)
        }
        Ok(Err(_)) => Err(StatusCode::BAD_REQUEST),
        Ok(Ok(body)) => Ok(body.to_bytes()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::{Full, StreamBody};
    use hyper::body::Frame;
    use std::convert::Infallible;

    #[test]
    fn opaque_and_malformed_origins_fail_closed() {
        assert!(origin_allowed(None));
        for good in [
            "http://localhost:1420",
            "https://127.0.0.1",
            "http://[::1]:47100",
            "tauri://localhost",
        ] {
            assert!(origin_allowed(Some(good)), "{good}");
        }
        for bad in [
            "",
            "null",
            "app://evil",
            "tauri://evil",
            "https://evil.test",
            "http://localhost.evil.test",
            "http://user@localhost",
            "http://localhost/path",
            "http://localhost?x",
            "http://localhost#x",
            "http://[::1]:bad",
            " http://localhost",
        ] {
            assert!(!origin_allowed(Some(bad)), "{bad}");
        }
        let mut headers = HeaderMap::new();
        headers.append("origin", "http://localhost".parse().unwrap());
        headers.append("origin", "null".parse().unwrap());
        assert_eq!(check_origin(&headers), Err(StatusCode::FORBIDDEN));
        headers.clear();
        headers.insert("content-type", "text/plain".parse().unwrap());
        assert_eq!(
            check_json_type(&headers),
            Err(StatusCode::UNSUPPORTED_MEDIA_TYPE)
        );
    }

    #[tokio::test]
    async fn limits_cover_declared_chunked_and_stalled_bodies() {
        assert_eq!(
            read_body(Full::new(Bytes::from_static(b"abcd")), 4)
                .await
                .unwrap(),
            "abcd"
        );
        assert_eq!(
            read_body(Full::new(Bytes::from_static(b"abcde")), 4).await,
            Err(StatusCode::PAYLOAD_TOO_LARGE)
        );
        let chunks = futures_util::stream::iter([
            Ok::<_, Infallible>(Frame::data(Bytes::from_static(b"abc"))),
            Ok(Frame::data(Bytes::from_static(b"def"))),
        ]);
        assert_eq!(
            read_body(StreamBody::new(chunks), 4).await,
            Err(StatusCode::PAYLOAD_TOO_LARGE)
        );
        let pending = StreamBody::new(futures_util::stream::pending::<
            Result<Frame<Bytes>, Infallible>,
        >());
        assert_eq!(
            read_body_with_timeout(pending, 4, Duration::from_millis(20)).await,
            Err(StatusCode::REQUEST_TIMEOUT)
        );
    }
}
