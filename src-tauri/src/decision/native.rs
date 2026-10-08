//! Owned CLI sessions. Every turn is serialized and native settings precede user input.
use super::native_judge;
use crate::model::CategoryType;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::Path,
    process::Stdio,
    sync::{Arc, OnceLock},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin},
    sync::{mpsc, Mutex, Notify},
};

type Result<T> = std::result::Result<T, String>;
fn sessions() -> &'static parking_lot::Mutex<HashMap<String, Arc<Session>>> {
    static SESSIONS: OnceLock<parking_lot::Mutex<HashMap<String, Arc<Session>>>> = OnceLock::new();
    SESSIONS.get_or_init(Default::default)
}

#[derive(Clone, Copy, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Runtime {
    Codex,
    Claude,
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeModel {
    id: String,
    efforts: Vec<String>,
}
#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    id: String,
    models: Vec<NativeModel>,
    status: String,
    output: String,
    decision: String,
    approval: Option<Value>,
}
struct Session {
    process: Mutex<Process>,
    view: RwLock<Snapshot>,
    cancel: Notify,
    approval: mpsc::Sender<(String, bool)>,
}
struct Process {
    child: Child,
    input: ChildStdin,
    messages: mpsc::Receiver<Result<Value>>,
    approvals: mpsc::Receiver<(String, bool)>,
    runtime: Runtime,
    sequence: u64,
    buffered: VecDeque<Value>,
    thread: Option<String>,
    model: Option<String>,
    cwd: String,
}

fn session(id: &str) -> Result<Arc<Session>> {
    sessions()
        .lock()
        .get(id)
        .cloned()
        .ok_or_else(|| "会话已关闭 / session closed".into())
}

impl Process {
    async fn write(&mut self, value: Value) -> Result<()> {
        let mut bytes = serde_json::to_vec(&value).map_err(|e| e.to_string())?;
        bytes.push(b'\n');
        self.input
            .write_all(&bytes)
            .await
            .map_err(|e| e.to_string())
    }
    async fn read(&mut self) -> Result<Value> {
        if let Some(message) = self.buffered.pop_front() {
            return Ok(message);
        }
        self.messages
            .recv()
            .await
            .ok_or("客户端已退出 / client exited")?
    }
    async fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        self.sequence += 1;
        let id = self.sequence.to_string();
        self.write(if self.runtime == Runtime::Codex {
            json!({"id":id,"method":method,"params":params})
        } else {
            let mut request = params;
            request["subtype"] = json!(method);
            json!({"type":"control_request","request_id":id,"request":request})
        })
        .await?;
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let msg = self
                    .messages
                    .recv()
                    .await
                    .ok_or("客户端已退出 / client exited")??;
                if self.runtime == Runtime::Codex && msg["id"] == id {
                    if !msg["error"].is_null() {
                        return Err("原生控制请求被拒绝 / native control rejected".into());
                    }
                    return Ok(msg["result"].clone());
                }
                if self.runtime == Runtime::Claude && msg["response"]["request_id"] == id {
                    if msg["response"]["subtype"] != "success" {
                        return Err("原生控制请求被拒绝 / native control rejected".into());
                    }
                    return Ok(msg["response"]["response"].clone());
                }
                // Initialization/settings must not silently authorize a server request.
                if msg["type"] == "control_request"
                    || (!msg["method"].is_null() && !msg["id"].is_null())
                {
                    self.reject(&msg).await?;
                } else {
                    if self.buffered.len() >= 128 {
                        return Err("原生事件积压 / native event backlog".into());
                    }
                    self.buffered.push_back(msg);
                }
            }
        })
        .await
        .map_err(|_| "原生控制超时 / native control timeout".to_string())?
    }
    async fn reject(&mut self, msg: &Value) -> Result<()> {
        self.write(if self.runtime == Runtime::Codex {
            json!({"id":msg["id"],"error":{"code":-32601,"message":"Unsupported control request"}})
        } else {
            json!({"type":"control_response","response":{"subtype":"error","request_id":msg["request_id"],"error":"Unsupported control request"}})
        }).await
    }
    async fn permission(&mut self, msg: &Value, session: &Session) -> Result<()> {
        let method = msg["method"].as_str().unwrap_or_default();
        let supported = if self.runtime == Runtime::Codex {
            matches!(
                method,
                "item/commandExecution/requestApproval" | "item/fileChange/requestApproval"
            )
        } else {
            msg["request"]["subtype"] == "can_use_tool"
        };
        if !supported {
            return self.reject(msg).await;
        }
        let ticket = uuid::Uuid::new_v4().to_string();
        session.view.write().approval = Some(
            json!({"ticket":ticket,"request":if self.runtime == Runtime::Codex { &msg["params"] } else { &msg["request"] }}),
        );
        let allow = loop {
            let (received, allow) = self
                .approvals
                .recv()
                .await
                .ok_or("审批通道已关闭 / approval channel closed")?;
            if received == ticket {
                break allow;
            }
        };
        session.view.write().approval = None;
        self.write(if self.runtime == Runtime::Codex {
            json!({"id":msg["id"],"result":{"decision":if allow { "accept" } else { "decline" }}})
        } else {
            let response = if allow { json!({"behavior":"allow","updatedInput":msg["request"]["input"]}) }
                else { json!({"behavior":"deny","message":"User denied this tool request"}) };
            json!({"type":"control_response","response":{"subtype":"success","request_id":msg["request_id"],"response":response}})
        }).await
    }
    async fn turn(
        &mut self,
        session: &Session,
        model: &str,
        effort: &str,
        prompt: &str,
    ) -> Result<()> {
        if self.model.as_deref().is_some_and(|v| v != model) {
            return Err("请新建会话以更换模型 / open a new session to change model".into());
        }
        if self.runtime == Runtime::Codex {
            if self.thread.is_none() {
                let result = self.call("thread/start", json!({"model":model,"cwd":self.cwd,"approvalPolicy":"on-request","sandbox":"workspace-write","ephemeral":true})).await?;
                self.thread = Some(
                    result["thread"]["id"]
                        .as_str()
                        .ok_or("缺少原生线程 ID / missing thread ID")?
                        .into(),
                );
            }
            self.call("turn/start", json!({"threadId":self.thread,"input":[{"type":"text","text":prompt,"text_elements":[]}],"effort":effort})).await?;
        } else {
            self.call(
                "apply_flag_settings",
                json!({"settings":{"model":model,"effortLevel":effort}}),
            )
            .await?;
            self.write(json!({"type":"user","message":{"role":"user","content":prompt}}))
                .await?;
        }
        self.model = Some(model.into());
        {
            let mut view = session.view.write();
            view.status = "running".into();
            view.decision
                .push_str(" · 原生控制已确认 / native control acknowledged");
        }
        let mut streamed_items = HashSet::new();
        loop {
            let msg = self.read().await?;
            if msg["type"] == "control_request"
                || (!msg["method"].is_null() && !msg["id"].is_null())
            {
                self.permission(&msg, session).await?;
                continue;
            }
            let text = if self.runtime == Runtime::Codex {
                if msg["method"] == "item/agentMessage/delta" {
                    if let Some(id) = msg["params"]["itemId"].as_str() {
                        streamed_items.insert(id.to_owned());
                    }
                    msg["params"]["delta"].as_str()
                } else if msg["method"] == "item/completed"
                    && msg["params"]["item"]["type"] == "agentMessage"
                    && !msg["params"]["item"]["id"]
                        .as_str()
                        .is_some_and(|id| streamed_items.contains(id))
                {
                    msg["params"]["item"]["text"].as_str()
                } else {
                    None
                }
            } else {
                None
            };
            if let Some(text) = text {
                let mut view = session.view.write();
                if view.output.len() + text.len() > 1_000_000 {
                    return Err("输出达到预览上限 / output limit reached".into());
                }
                view.output.push_str(text);
            }
            if self.runtime == Runtime::Claude && msg["type"] == "assistant" {
                if let Some(content) = msg["message"]["content"].as_array() {
                    let mut view = session.view.write();
                    for block in content {
                        if let Some(text) = block["text"].as_str() {
                            if view.output.len() + text.len() > 1_000_000 {
                                return Err("输出达到预览上限 / output limit reached".into());
                            }
                            view.output.push_str(text);
                        }
                    }
                }
            }
            if self.runtime == Runtime::Codex && msg["method"] == "turn/completed" {
                return if msg["params"]["turn"]["status"] == "completed" {
                    Ok(())
                } else {
                    Err("原生轮次未成功完成 / native turn failed".into())
                };
            }
            if self.runtime == Runtime::Claude && msg["type"] == "result" {
                return if msg["subtype"] == "success" && msg["is_error"] != true {
                    Ok(())
                } else {
                    Err("原生轮次未成功完成 / native turn failed".into())
                };
            }
        }
    }
}

fn catalog(runtime: Runtime, body: &Value) -> Vec<NativeModel> {
    let rows = if runtime == Runtime::Codex {
        &body["data"]
    } else {
        &body["models"]
    };
    rows.as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| {
            let id = if runtime == Runtime::Codex {
                row["model"].as_str()
            } else {
                row["resolvedModel"]
                    .as_str()
                    .or_else(|| row["value"].as_str())
            }?;
            let efforts = if runtime == Runtime::Codex {
                row["supportedReasoningEfforts"]
                    .as_array()?
                    .iter()
                    .filter_map(|e| e["reasoningEffort"].as_str())
                    .map(String::from)
                    .collect::<Vec<_>>()
            } else {
                row["supportedEffortLevels"]
                    .as_array()?
                    .iter()
                    .filter_map(Value::as_str)
                    .map(String::from)
                    .collect()
            };
            (!efforts.is_empty()).then(|| NativeModel {
                id: id.into(),
                efforts,
            })
        })
        .collect()
}

#[tauri::command]
pub async fn native_open(runtime: Runtime, executable: String, cwd: String) -> Result<Snapshot> {
    let exe = Path::new(&executable);
    if !exe.is_absolute()
        || !exe.is_file()
        || !Path::new(&cwd).is_absolute()
        || !Path::new(&cwd).is_dir()
    {
        return Err("请选择客户端可执行文件和工作目录的绝对路径 / absolute executable and workspace paths required".into());
    }
    #[cfg(windows)]
    if !exe
        .extension()
        .is_some_and(|v| v.eq_ignore_ascii_case("exe"))
    {
        return Err("请选择原生 .exe，不能使用脚本包装器 / select native .exe".into());
    }
    if sessions().lock().len() >= 4 {
        return Err("最多同时运行四个会话 / maximum four sessions".into());
    }
    let mut command = crate::proc::hidden(exe);
    command
        .current_dir(&cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    if runtime == Runtime::Codex {
        command.args(["app-server", "--listen", "stdio://"]);
    } else {
        command.args([
            "-p",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
            "--permission-prompt-tool",
            "stdio",
        ]);
    }
    let mut child = command.spawn().map_err(|e| e.to_string())?;
    let input = child.stdin.take().ok_or("stdin unavailable")?;
    let output = child.stdout.take().ok_or("stdout unavailable")?;
    let (tx, messages) = mpsc::channel(128);
    tokio::spawn(async move {
        let mut reader = BufReader::new(output);
        loop {
            let mut bytes = Vec::new();
            // Bound allocation even for a client emitting a malformed line without a newline.
            let read = tokio::io::AsyncReadExt::take(&mut reader, 1_048_577)
                .read_until(b'\n', &mut bytes)
                .await;
            let value = match read {
                Ok(0) => break,
                Ok(_) if bytes.len() > 1_048_576 => {
                    Err("原生消息过大 / oversized native message".into())
                }
                Ok(_) => serde_json::from_slice(&bytes)
                    .map_err(|_| "原生协议格式无效 / invalid native protocol".into()),
                Err(_) => Err("原生输出读取失败 / native output read failed".into()),
            };
            let failed = value.is_err();
            if tx.send(value).await.is_err() || failed {
                break;
            }
        }
    });
    let (approval, approvals) = mpsc::channel(8);
    let mut process = Process {
        child,
        input,
        messages,
        approvals,
        runtime,
        sequence: 0,
        buffered: VecDeque::new(),
        thread: None,
        model: None,
        cwd,
    };
    let body = if runtime == Runtime::Codex {
        process.call("initialize", json!({"clientInfo":{"name":"synaroute","version":"1"},"capabilities":{"experimentalApi":true}})).await?;
        process.write(json!({"method":"initialized"})).await?;
        process.call("model/list", json!({})).await?
    } else {
        process.call("initialize", json!({})).await?
    };
    let models = catalog(runtime, &body);
    if models.is_empty() {
        return Err(
            "客户端没有返回可调档位，请升级客户端 / no native efforts advertised; update client"
                .into(),
        );
    }
    let view = Snapshot {
        id: uuid::Uuid::new_v4().to_string(),
        models,
        status: "ready".into(),
        ..Default::default()
    };
    let mut sessions = sessions().lock();
    if sessions.len() >= 4 {
        return Err("最多同时运行四个会话 / maximum four sessions".into());
    }
    sessions.insert(
        view.id.clone(),
        Arc::new(Session {
            process: Mutex::new(process),
            view: RwLock::new(view.clone()),
            cancel: Notify::new(),
            approval,
        }),
    );
    Ok(view)
}

#[tauri::command]
pub fn native_snapshot(id: String) -> Result<Snapshot> {
    Ok(session(&id)?.view.read().clone())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnRequest {
    id: String,
    category: CategoryType,
    model: String,
    effort: String,
    prompt: String,
    auto: bool,
}
#[tauri::command]
pub async fn native_turn(
    state: tauri::State<'_, crate::AppState>,
    request: TurnRequest,
) -> Result<Snapshot> {
    run_turn(&state.store, request).await
}

async fn run_turn(store: &super::Store, request: TurnRequest) -> Result<Snapshot> {
    let session = session(&request.id)?;
    let mut process = session
        .process
        .try_lock()
        .map_err(|_| "会话正在运行 / session busy")?;
    if session.view.read().status == "closed" {
        return Err("会话已关闭 / session closed".into());
    }
    if process
        .model
        .as_deref()
        .is_some_and(|model| model != request.model)
    {
        return Err("请新建会话以更换模型 / open a new session to change model".into());
    }
    if request.prompt.trim().is_empty() || request.prompt.len() > 100_000 {
        return Err("问题为空或过长 / empty or oversized prompt".into());
    }
    let efforts = session
        .view
        .read()
        .models
        .iter()
        .find(|m| m.id == request.model)
        .map(|m| m.efforts.clone())
        .ok_or("模型不在原生目录中 / model outside native catalog")?;
    if !efforts.contains(&request.effort) {
        return Err("档位不在原生目录中 / effort outside native catalog".into());
    }
    {
        let mut view = session.view.write();
        view.status = "judging".into();
        view.output.clear();
        view.decision.clear();
    }
    let operation = async {
        let mut effort = request.effort.clone();
        let decision = if request.auto {
            let config = store
                .snapshot_config()
                .decision
                .get(&request.category)
                .cloned()
                .unwrap_or_default();
            match native_judge::select(store, config, &request.prompt, &efforts).await {
                Ok(selected) => {
                    effort = selected;
                    format!("自动选择 / selected: {effort}")
                }
                Err(reason) => format!("回退到 / fallback: {effort} · {reason}"),
            }
        } else {
            format!("手动档位 / manual: {effort}")
        };
        session.view.write().decision = decision;
        process
            .turn(&session, &request.model, &effort, &request.prompt)
            .await
    };
    let result = tokio::select! {
        biased;
        _ = session.cancel.notified() => Err("已停止并关闭会话 / stopped and closed".into()),
        result = tokio::time::timeout(Duration::from_secs(1800), operation) => result.unwrap_or(Err("会话超时 / session timeout".into())),
    };
    if result.is_err() {
        let _ = process.child.kill().await;
    }
    {
        let mut view = session.view.write();
        view.status = if result.is_ok() { "ready" } else { "closed" }.into();
        view.approval = None;
    }
    result?;
    let snapshot = session.view.read().clone();
    Ok(snapshot)
}

#[tauri::command]
pub async fn native_approve(id: String, ticket: String, allow: bool) -> Result<()> {
    let session = session(&id)?;
    if session
        .view
        .read()
        .approval
        .as_ref()
        .and_then(|a| a["ticket"].as_str())
        != Some(ticket.as_str())
    {
        return Err("审批已过期 / stale approval".into());
    }
    session
        .approval
        .try_send((ticket, allow))
        .map_err(|_| "审批通道忙 / approval channel busy".into())
}

#[tauri::command]
pub async fn native_close(id: String) -> Result<()> {
    let session = sessions().lock().remove(&id);
    if let Some(session) = session {
        session.cancel.notify_one();
        let mut process = session.process.lock().await;
        let _ = process.child.kill().await;
        session.view.write().status = "closed".into();
    }
    Ok(())
}
pub(crate) async fn shutdown() {
    let ids: Vec<_> = sessions().lock().keys().cloned().collect();
    for id in ids {
        let _ = native_close(id).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Only launched by the loopback harness with isolated CLI homes and dummy credentials.
    #[tokio::test]
    #[ignore = "requires scripts/native-effort/probe.mjs --adapter-tests"]
    async fn native_fixture() {
        let cwd = std::env::var("SYNAROUTE_FIXTURE_ROOT").expect("loopback harness required");
        let store = super::super::Store::new_at(
            Path::new(&cwd).join("app-config.json"),
            Path::new(&cwd).join("app-secrets.enc"),
        )
        .unwrap();
        for (runtime, env) in [
            (Runtime::Codex, "SYNAROUTE_FIXTURE_CODEX"),
            (Runtime::Claude, "SYNAROUTE_FIXTURE_CLAUDE"),
        ] {
            super::super::save(&store, CategoryType::Codex, Default::default(), None, true)
                .unwrap();
            let view = native_open(runtime, std::env::var(env).unwrap(), cwd.clone())
                .await
                .unwrap();
            let model = view
                .models
                .iter()
                .find(|m| m.efforts.contains(&"low".into()) && m.efforts.contains(&"high".into()))
                .expect("low/high capable native model");
            let session = session(&view.id).unwrap();
            for effort in ["low", "high"] {
                let result = tokio::time::timeout(
                    Duration::from_secs(30),
                    run_turn(
                        &store,
                        TurnRequest {
                            id: view.id.clone(),
                            category: CategoryType::Codex,
                            model: model.id.clone(),
                            effort: effort.into(),
                            prompt: "Reply fixture ok.".into(),
                            auto: effort == "low",
                        },
                    ),
                )
                .await
                .unwrap()
                .unwrap();
                assert_eq!(result.status, "ready");
                assert!(
                    result.output.contains("fixture ok"),
                    "missing native output: {}",
                    result.output
                );
                assert!(result.decision.contains(if effort == "low" {
                    "fallback"
                } else {
                    "manual"
                }));
            }
            {
                let mut process = session.process.lock().await;
                assert_eq!(process.model.as_deref(), Some(model.id.as_str()));
                assert!(process
                    .turn(&session, "different-model", "low", "must not run")
                    .await
                    .is_err());
            }
            assert!(native_approve(view.id.clone(), "stale".into(), true)
                .await
                .is_err());
            super::super::save(
                &store,
                CategoryType::Codex,
                super::super::DecisionConfig {
                    mode: super::super::Mode::Suggest,
                    provider: super::super::Provider::Openai,
                    endpoint: std::env::var("SYNAROUTE_FIXTURE_JUDGE").unwrap(),
                    model: "fixture-judge".into(),
                    ..Default::default()
                },
                None,
                true,
            )
            .unwrap();
            let (cancelled, closed) = tokio::join!(
                run_turn(
                    &store,
                    TurnRequest {
                        id: view.id.clone(),
                        category: CategoryType::Codex,
                        model: model.id.clone(),
                        effort: "low".into(),
                        prompt: "cancelled task must not reach upstream".into(),
                        auto: true
                    }
                ),
                async {
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    native_close(view.id.clone()).await
                }
            );
            assert!(cancelled.is_err());
            closed.unwrap();
            tokio::time::sleep(Duration::from_millis(650)).await;
            assert_eq!(session.view.read().status, "closed");
            native_close(view.id.clone()).await.unwrap();
            assert!(super::session(&view.id).is_err());
            assert!(session
                .process
                .lock()
                .await
                .child
                .try_wait()
                .unwrap()
                .is_some());
        }
    }
    #[test]
    fn catalogs_never_invent_efforts() {
        let codex = catalog(
            Runtime::Codex,
            &json!({"data":[{"model":"fixed","supportedReasoningEfforts":[{"reasoningEffort":"ultra"}]}]}),
        );
        assert_eq!(codex[0].efforts, ["ultra"]);
        let claude = catalog(
            Runtime::Claude,
            &json!({"models":[{"value":"sonnet","supportedEffortLevels":["low","max"]},{"value":"haiku"}]}),
        );
        assert_eq!(claude.len(), 1);
        assert_eq!(claude[0].efforts, ["low", "max"]);
    }
}
