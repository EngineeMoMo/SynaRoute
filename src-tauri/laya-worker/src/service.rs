use bytes::Bytes;
use http_body_util::Full;
use hyper::{body::Incoming, service::service_fn, Method, Request, Response, StatusCode};
use laya::{LayaEngine, LayaOptions, Question, QuestionSet};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{convert::Infallible, io::Write, path::PathBuf, sync::Arc};
use tokio::{io::{AsyncBufReadExt, AsyncReadExt, BufReader}, sync::Semaphore};
use std::os::windows::io::AsRawSocket;
use windows::Win32::Networking::WinSock::{setsockopt, SOCKET, SOL_SOCKET, SO_EXCLUSIVEADDRUSE};

#[derive(Deserialize)]
struct Launch {
    bundle: PathBuf,
    port: u16,
    nonce: String,
}

pub(super) fn run() -> Result<(), String> {
    if std::env::args_os().len() != 1 { return Err("worker takes configuration through stdin".into()); }
    let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().map_err(|error| error.to_string())?;
    runtime.block_on(serve())
}

async fn serve() -> Result<(), String> {
    let mut input = BufReader::new(tokio::io::stdin());
    let mut line = String::new();
    (&mut input).take(16384).read_line(&mut line).await.map_err(|error| error.to_string())?;
    if !line.ends_with('\n') { return Err("invalid worker configuration".into()); }
    let launch: Launch = serde_json::from_str(&line).map_err(|error| error.to_string())?;
    if launch.nonce.len() != 32 { return Err("invalid health nonce".into()); }
    let bundle = launch.bundle;
    let engine = tokio::task::spawn_blocking(move || {
        crate::artifacts::verify_manifest(&bundle, &crate::artifacts::manifest())?;
        ort::init_from(bundle.join("runtime/onnxruntime.dll")).map_err(|error| error.to_string())?.commit();
        LayaEngine::create(LayaOptions { model_directory: Some(bundle.join("model")), intra_op_threads: Some(2),
            inter_op_threads: Some(1), allow_download: false, ..Default::default() }).map(Arc::new).map_err(|error| error.to_string())
    }).await.map_err(|error| error.to_string())??;
    let socket = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::STREAM, None).map_err(|error| error.to_string())?;
    if unsafe { setsockopt(SOCKET(socket.as_raw_socket() as usize), SOL_SOCKET, SO_EXCLUSIVEADDRUSE, Some(&1i32.to_ne_bytes())) } != 0 {
        return Err("could not reserve an exclusive worker socket".into());
    }
    socket.bind(&std::net::SocketAddr::from(([127, 0, 0, 1], launch.port)).into()).map_err(|error| format!("[port] {error}"))?;
    socket.listen(16).map_err(|error| error.to_string())?;
    socket.set_nonblocking(true).map_err(|error| error.to_string())?;
    let listener = tokio::net::TcpListener::from_std(socket.into()).map_err(|error| error.to_string())?;
    let port = listener.local_addr().map_err(|error| error.to_string())?.port();
    let nonce = Arc::new(launch.nonce);
    println!("SYNAROUTE_LAYA_READY:{port}");
    std::io::stdout().flush().map_err(|error| error.to_string())?;
    let connections = Arc::new(Semaphore::new(8));
    let inference = Arc::new(Semaphore::new(1));
    let mut tasks = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            _ = input.fill_buf() => return Ok(()),
            Some(_) = tasks.join_next(), if !tasks.is_empty() => {},
            accepted = listener.accept() => {
                let (stream, _) = accepted.map_err(|error| error.to_string())?;
                let Ok(permit) = connections.clone().try_acquire_owned() else { continue };
                let engine = engine.clone();
                let nonce = nonce.clone();
                let inference = inference.clone();
                tasks.spawn(async move {
                    let _permit = permit;
                    let service = service_fn(move |request| handle(request, engine.clone(), inference.clone(), nonce.clone(), port));
                    let _ = crate::http_ingress::http1().serve_connection(hyper_util::rt::TokioIo::new(stream), service).await;
                });
            }
        }
    }
}

fn reply(status: StatusCode, value: Value) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(Bytes::from(value.to_string())));
    *response.status_mut() = status;
    response.headers_mut().insert(hyper::header::CONTENT_TYPE, hyper::header::HeaderValue::from_static("application/json"));
    response
}

async fn handle(request: Request<Incoming>, engine: Arc<LayaEngine>, inference: Arc<Semaphore>, nonce: Arc<String>, port: u16) -> Result<Response<Full<Bytes>>, Infallible> {
    let authority = format!("127.0.0.1:{port}");
    if request.headers().get(hyper::header::HOST).and_then(|value| value.to_str().ok()) != Some(authority.as_str())
        || crate::http_ingress::check_origin(request.headers()).is_err() {
        return Ok(reply(StatusCode::FORBIDDEN, json!({"error":"origin rejected"})));
    }
    if request.method() == Method::GET && request.uri().path() == "/health" {
        return Ok(reply(StatusCode::OK, json!({"status":"ok", "loaded":["multilingual"], "backend":"rust", "nonce":*nonce, "version":crate::artifacts::manifest().version})));
    }
    if request.uri().path() != "/v1/systemone" { return Ok(reply(StatusCode::NOT_FOUND, json!({"error":"unknown endpoint"}))); }
    if let Err(status) = crate::http_ingress::check_json_request(request.method(), request.headers()) {
        return Ok(reply(status, json!({"error":"JSON POST required"})));
    }
    let Ok(permit) = inference.try_acquire_owned() else { return Ok(reply(StatusCode::TOO_MANY_REQUESTS, json!({"error":"inference busy"}))); };
    let bytes = match crate::http_ingress::read_body(request.into_body(), 1024 * 1024).await {
        Ok(bytes) => bytes,
        Err(status) => return Ok(reply(status, json!({"error":"request too large or incomplete"}))),
    };
    let body: Value = match serde_json::from_slice(&bytes) {
        Ok(body) => body,
        Err(_) => return Ok(reply(StatusCode::BAD_REQUEST, json!({"error":"invalid JSON"}))),
    };
    if body["model"] != "multilingual" || !body["state"].is_string()
        || body["questions"].as_object().is_none_or(|questions| questions.len() != 1)
        || body["questions"]["deep"]["type"] != "noul"
        || body["questions"]["deep"]["instructions"] != crate::question::QUESTION {
        return Ok(reply(StatusCode::BAD_REQUEST, json!({"error":"expected multilingual deep-reasoning question"})));
    }
    let prediction = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let questions = QuestionSet::new().with("deep", Question::noul(crate::question::QUESTION, Value::Null, Value::Null)).map_err(|error| error.to_string())?;
        let result = engine.predict(body["state"].clone(), &questions).map_err(|error| error.to_string())?;
        let score = result.get("deep").and_then(|answer| answer.as_noul()).map(|answer| answer.probability).filter(|score| score.is_finite()).ok_or("missing score")?;
        let diagnostics = result.state_usage();
        Ok::<_, String>(json!({"model":"multilingual", "answers":{"deep":{"noul":score}}, "usage":{
            "input_tokens":result.usage().input_tokens, "output_tokens":0,
            "truncated":diagnostics.is_none_or(|usage| usage.truncated),
            "state_tokens_dropped":diagnostics.map(|usage| usage.state_tokens_dropped),
            "state_tokens":diagnostics.map(|usage| usage.state_tokens)}}))
    }).await;
    Ok(match prediction {
        Ok(Ok(value)) => reply(StatusCode::OK, value),
        _ => reply(StatusCode::INTERNAL_SERVER_ERROR, json!({"error":"local inference failed"})),
    })
}
