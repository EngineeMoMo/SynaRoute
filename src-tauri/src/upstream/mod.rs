//! 上游厂商通信 + 协议适配层。
//!
//! MVP 范围（arch-decisions §10）：
//! - 非流式：Anthropic Messages ↔ OpenAI Chat Completions 双向字段转换。
//! - 拉取模型：兼容 Anthropic /v1/models 与 OpenAI /v1/models。
//! - 简单文本请求用于健康检查与聚合成员调用。
//!
//! 流式 SSE 的完整转发在 proxy 模块处理；此处提供非流式的一次性调用。

// ---- 子模块（P2-1 目录化）----
//
// 拆分承诺：对外的 `crate::upstream::X` 路径**一个都不变**（外部 40 多处引用）。
// 故这里用**具名** re-export，不用 glob：
// ① 两个子模块出现同名项时 glob 会 E0659 歧义；
// ② glob 会把只在内部用的名字也导出去，而未被使用的 re-export 会触发 unused_imports。
// 契约由文件末尾的 api_surface 守卫在编译期兜住。
mod cache;
mod client;
mod endpoint;
mod usage;
mod util;

pub use client::{is_retriable_upstream_error, shared_client};
pub use endpoint::join_endpoint;
pub use usage::{extract_usage, with_usage, TokenUsage};

// 子模块里被本文件使用的项。`pub(super)` 的项对父模块可见需要显式 use ——
// Rust 的私有项可见性只向**下**流（父的私有项对子可见），反向必须显式提升并引入。
use client::{
    apply_auth, apply_client_identity, apply_models_auth, build_client, fast_timeout,
    truncate_body, RETRY_BASE_BACKOFF_MS, RETRY_MAX_ATTEMPTS,
};
use cache::{cache_known_unsupported, inject_anthropic_cache, looks_like_cache_rejection, mark_cache_unsupported};
use endpoint::model_endpoints;
use usage::record_usage_from_raw;
use util::{extract_text_content, uuid_like};

use crate::error::{AppError, AppResult};
use crate::model::{Protocol, ProviderKey};
use serde_json::{json, Value};
use std::time::Duration;

/// 拉取模型列表（FR-004）。返回真实模型名数组。
pub async fn fetch_models(key: &ProviderKey, secret: &str) -> AppResult<Vec<String>> {
    let client = build_client(key)?;

    // 不同厂商的模型端点路径不一致（DeepSeek 等第三方对 /v1/models 返回 404），
    // 依次尝试候选路径，任一 2xx 即用；全失败则汇总错误。
    let mut last_err = String::from("无候选端点");
    for url in model_endpoints(&key.base_url) {
        let mut req = client.get(&url).timeout(fast_timeout(key));
        // /models 用双鉴权头（Bearer + x-api-key），兼容把 Anthropic 挂子路径的 OpenAI 风格 /models
        req = apply_models_auth(req, secret);

        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                // 连接层失败：区分「域名解析不了/连不上」与「超时」，并直接给出该查什么。
                // 原先只有 `e.to_string()`（`error sending request for url (…)`），
                // 用户看不出是自己网络问题、base_url 写错、还是上游真的挂了。
                last_err = if e.is_timeout() {
                    format!("连接超时（{url}）：上游无响应，可能是网络受限或该站点当前不可用")
                } else if e.is_connect() {
                    format!(
                        "连不上 {url}：请检查 Base URL 是否写错、域名能否解析、\
                         以及是否需要代理/VPN 才能访问该站点"
                    )
                } else {
                    format!("请求 {url} 失败：{e}")
                };
                continue;
            }
        };
        let status = resp.status();
        if !status.is_success() {
            // 带上状态码的可行动含义：401/403 是密钥问题，404 是路径问题（会自动换下个候选）。
            let hint = match status.as_u16() {
                401 | 403 => "（密钥无效或无权限，请检查密钥是否填对、是否已过期）",
                404 | 405 => "（该路径不存在，将自动尝试其他候选路径）",
                429 => "（被限流，稍后再试）",
                s if s >= 500 => "（上游服务异常，与本地配置无关）",
                _ => "",
            };
            last_err = format!("HTTP {status} @ {url}{hint}");
            // 404/405 说明路径不对，换下一个候选；其他状态码同样重试下一个
            continue;
        }
        // 响应体必须是 JSON。**不能直接 `resp.json().await?`**：上游返回 HTML 错误页/登录页时，
        // serde 只会抛出 `expected value at line 1 column 1`（实测用户就是被这句卡住的）——
        // 那是「第 1 行第 1 列不是合法 JSON」的字面意思，对用户毫无指向性。
        // 这里先取文本再解析，失败时报出「拿到的不是 JSON」并附开头片段，让用户一眼看出
        // 究竟是被挡在了登录页、还是 base_url 少了 `/v1`。
        let text = match resp.text().await {
            Ok(t) => t,
            Err(e) => {
                last_err = format!("读取 {url} 的响应失败：{e}");
                continue;
            }
        };
        let body: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => {
                last_err = non_json_models_hint(&url, &text);
                continue;
            }
        };
        let names = parse_model_names(&body);
        if !names.is_empty() {
            return Ok(names);
        }
        last_err = format!("{url} 返回了 JSON 但其中没有模型列表（该站点可能不提供模型查询接口）");
    }
    // 末尾统一附「可手动录入」的出路：这条链路失败不阻塞用户继续配置。
    Err(AppError::upstream_msg(format!("拉取模型失败：{last_err}")))
}

/// 拉取模型时上游返回**非 JSON** 的可行动提示。
///
/// 拆成纯函数是为了**可验证**：这条判据的价值全在措辞上，而端到端跑一次拉取需要真实上游。
///
/// 真机背景（2026-08-02）：用户配了一个中转商，拉取模型时只看到
/// `error decoding response body ← expected value at line 1 column 1`。
/// 那是 serde 在说「第 1 行第 1 列不是合法 JSON」，对用户零指向性 —— 实际原因是该站点
/// 把请求挡在了 HTML 页面上。提示必须说清「拿到的是网页」以及「该去改 Base URL」。
fn non_json_models_hint(url: &str, text: &str) -> String {
    let head: String = text.trim().chars().take(80).collect();
    let low = head.to_ascii_lowercase();
    if low.starts_with("<!doctype") || low.starts_with("<html") || low.starts_with("<?xml") {
        format!(
            "{url} 返回的是网页而不是 JSON（可能被挡在登录页/防护页，或 Base URL 指向了站点首页）。\
             请确认 Base URL 填的是接口地址（通常以 /v1 结尾）"
        )
    } else if head.is_empty() {
        format!("{url} 返回了空响应（该地址可能不是模型查询接口）")
    } else {
        format!("{url} 返回的内容不是合法 JSON，开头是「{head}」")
    }
}


/// 从模型列表响应解析模型名，兼容多种结构：
/// - OpenAI/Anthropic: `{data:[{id}]}`
/// - 部分厂商: `{models:[{id/name}]}` 或顶层数组 `[{id/name}]`
fn parse_model_names(body: &Value) -> Vec<String> {
    let pick = |item: &Value| -> Option<String> {
        item.get("id")
            .or_else(|| item.get("name"))
            .or_else(|| item.get("model"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            // 纯字符串数组（["gpt-4", ...]）也支持
            .or_else(|| item.as_str().map(|s| s.to_string()))
    };
    let arr = body
        .get("data")
        .and_then(|d| d.as_array())
        .or_else(|| body.get("models").and_then(|d| d.as_array()))
        .or_else(|| body.as_array());
    match arr {
        Some(items) => items.iter().filter_map(pick).collect(),
        None => vec![],
    }
}


/// 一次性文本请求（用于聚合成员/决策者/汇总模型调用）。
/// prompt 作为单条 user 消息发送，返回助手文本。
/// `retry` 为 true 时，对临时性上游错误（502/503/504/429/连接失败）自动重试。
///
/// `request_timeout` 为**单次 HTTP 请求**的超时（含上游完整生成时间）。此前硬用
/// `key_timeout`（默认 30s）——那是给代理转发/健康探测的量级；非流式补全要等上游把
/// 整篇内容生成完才返回，30s 必然掐死正常长回答，再叠加 3 次重试 ≈ 91s 全灭，
/// 表象是「error sending request / error decoding response body」而健康探测（max_tokens=1
/// 秒回）始终显示可用。聚合路径应传入 brain 总预算量级的超时。
pub async fn text_completion(
    key: &ProviderKey,
    secret: &str,
    model: &str,
    prompt: &str,
    max_tokens: u32,
    retry: bool,
    request_timeout: Duration,
) -> AppResult<String> {
    let max_attempts = if retry { RETRY_MAX_ATTEMPTS } else { 1 };
    let mut last_err = None;
    for attempt in 1..=max_attempts {
        let client = build_client(key)?;
        let result = match key.protocol {
            Protocol::Anthropic => {
                anthropic_message(&client, key, secret, model, prompt, max_tokens, request_timeout).await
            }
            // 大脑聚合成员探测走 Chat Completions；Responses 上游此处也用 Chat 形态发最小请求
            // （聚合成员探测为尽力而为，非主转发路径）。
            Protocol::OpenaiChat | Protocol::OpenaiResponses => {
                openai_chat(&client, key, secret, model, prompt, max_tokens, request_timeout).await
            }
        };
        match result {
            Ok(text) => return Ok(text),
            Err(e) => {
                // 不可重试错误（鉴权/参数）或已是最后一次：直接返回。
                if attempt >= max_attempts || !is_retriable_upstream_error(&e) {
                    return Err(e);
                }
                last_err = Some(e);
                // 线性退避后重试。
                tokio::time::sleep(std::time::Duration::from_millis(
                    RETRY_BASE_BACKOFF_MS * attempt as u64,
                ))
                .await;
            }
        }
    }
    // 循环必然在上面 return，这里兜底（不应到达）。
    Err(last_err.unwrap_or_else(|| AppError::upstream_msg("未知上游错误")))
}


/// 从原始响应体解析 Anthropic 文本，兼容两种形态：
/// ① 普通 JSON：`{"content":[{"type":"text","text":".."}]}`
/// ② SSE 流：多行 `data: {...}`，逐行取 content_block_delta 的 text 累加。
fn parse_anthropic_text(raw: &str) -> Option<String> {
    // 先试普通 JSON
    if let Ok(body) = serde_json::from_str::<Value>(raw) {
        if let Some(text) = anthropic_text_from_json(&body) {
            return Some(text);
        }
    }
    // 再试 SSE：累加 content_block_delta 的 delta.text
    let mut acc = String::new();
    let mut saw_event = false;
    for line in raw.lines() {
        let line = line.trim_start();
        let Some(data) = line.strip_prefix("data:") else { continue };
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        if let Ok(ev) = serde_json::from_str::<Value>(data) {
            saw_event = true;
            if let Some(t) = ev
                .get("delta")
                .and_then(|d| d.get("text"))
                .and_then(|t| t.as_str())
            {
                acc.push_str(t);
            }
        }
    }
    if saw_event {
        Some(acc)
    } else {
        None
    }
}

/// 从已解析的 Anthropic JSON 取 content 里的文本拼接。
fn anthropic_text_from_json(body: &Value) -> Option<String> {
    let arr = body.get("content")?.as_array()?;
    Some(
        arr.iter()
            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join(""),
    )
}

/// 从原始响应体解析 OpenAI 文本，兼容普通 JSON 与 SSE 流（choices[].delta.content）。
fn parse_openai_text(raw: &str) -> Option<String> {
    if let Ok(body) = serde_json::from_str::<Value>(raw) {
        if let Some(text) = openai_text_from_json(&body) {
            return Some(text);
        }
    }
    let mut acc = String::new();
    let mut saw_event = false;
    for line in raw.lines() {
        let line = line.trim_start();
        let Some(data) = line.strip_prefix("data:") else { continue };
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        if let Ok(ev) = serde_json::from_str::<Value>(data) {
            saw_event = true;
            if let Some(t) = ev
                .get("choices")
                .and_then(|c| c.as_array())
                .and_then(|arr| arr.first())
                .and_then(|c| c.get("delta"))
                .and_then(|d| d.get("content"))
                .and_then(|c| c.as_str())
            {
                acc.push_str(t);
            }
        }
    }
    if saw_event {
        Some(acc)
    } else {
        None
    }
}

/// 从已解析的 OpenAI JSON 取 choices[0].message.content。
fn openai_text_from_json(body: &Value) -> Option<String> {
    body.get("choices")?
        .as_array()?
        .first()?
        .get("message")?
        .get("content")?
        .as_str()
        .map(|s| s.to_string())
}

async fn anthropic_message(
    client: &reqwest::Client,
    key: &ProviderKey,
    secret: &str,
    model: &str,
    prompt: &str,
    max_tokens: u32,
    request_timeout: Duration,
) -> AppResult<String> {
    let url = join_endpoint(&key.base_url, "/v1/messages");
    let payload = json!({
        "model": model,
        "max_tokens": max_tokens,
        "messages": [{ "role": "user", "content": prompt }]
    });
    // 版本头由 apply_auth 按协议统一添加（见那里的注释），此处不再单独补。
    let mut req = client.post(&url).json(&payload).timeout(request_timeout);
    req = apply_auth(req, key, secret);
    req = apply_client_identity(req, key.protocol);

    let resp = req.send().await?;
    let status = resp.status();
    // 先读文本再解析：上游可能返回 SSE 流（data: {...}）、HTML 错误页或非标准 JSON，
    // 直接 resp.json() 会得到笼统的「error decoding response body」，看不到上游到底返回了啥。
    let raw = resp.text().await?;
    if !status.is_success() {
        // 传**真实状态码**：消息里拼了响应体，绝不能让重试判定去里面猜（见
        // `is_retriable_upstream_error` 的文档：401 + 响应体含「请检查网络连接」曾被误判可重试）。
        return Err(AppError::upstream_http(
            status.as_u16(),
            format!("Anthropic HTTP {status}: {}", truncate_body(&raw)),
        ));
    }
    record_usage_from_raw(&raw);
    // content: [{type:"text", text:"..."}]。兼容普通 JSON 与 SSE 流两种返回形态。
    // 解析失败**不是** HTTP 错误（状态码是 2xx）：status 传 None 会被判为「连接层失败可重试」，
    // 而重试一个格式不对的响应毫无意义。故显式给 status，让它落进「非临时错误」分支。
    let text = parse_anthropic_text(&raw).ok_or_else(|| {
        AppError::upstream_http(
            status.as_u16(),
            format!("Anthropic 响应无法解析: {}", truncate_body(&raw)),
        )
    })?;
    Ok(text)
}

async fn openai_chat(
    client: &reqwest::Client,
    key: &ProviderKey,
    secret: &str,
    model: &str,
    prompt: &str,
    max_tokens: u32,
    request_timeout: Duration,
) -> AppResult<String> {
    let url = join_endpoint(&key.base_url, "/v1/chat/completions");
    let payload = json!({
        "model": model,
        "max_tokens": max_tokens,
        "messages": [{ "role": "user", "content": prompt }]
    });
    let mut req = client.post(&url).json(&payload).timeout(request_timeout);
    req = apply_auth(req, key, secret);
    req = apply_client_identity(req, key.protocol);

    let resp = req.send().await?;
    let status = resp.status();
    let raw = resp.text().await?;
    if !status.is_success() {
        // 传真实状态码，理由同 `anthropic_message`。
        return Err(AppError::upstream_http(
            status.as_u16(),
            format!("OpenAI HTTP {status}: {}", truncate_body(&raw)),
        ));
    }
    record_usage_from_raw(&raw);
    // 解析失败：状态码是 2xx，显式带上以免被当成「连接层失败」而白重试。
    let text = parse_openai_text(&raw).ok_or_else(|| {
        AppError::upstream_http(
            status.as_u16(),
            format!("OpenAI 响应无法解析: {}", truncate_body(&raw)),
        )
    })?;
    Ok(text)
}

// ===========================================================================
// 多模态（图片）+ 工具调用：大脑聚合成员的 agent 循环用
//
// 为什么与上面的 text_completion 并列、而不是给它加参数：text_completion 有三个调用点
// （聚合成员、决策者、汇总者）。后两者的职责是「综合已有分析」，给它们工具会让整轮耗时
// 不可控；硬塞参数则让那两条不需要工具的路径也背上多轮循环的复杂度。
// ===========================================================================

/// 一张随 prompt 发给模型的图片。`base64` 是裸编码，**不含** `data:` 前缀
/// —— OpenAI 侧需要的 data URL 在 [`openai_user_content`] 里现拼，Anthropic 侧则要裸串。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImagePart {
    /// MIME 类型。只允许两家协议**共同**支持的四种（png/jpeg/gif/webp）。
    /// 校验在入口（MCP 的 `images` 参数）做，协议层只负责按各自形状拼装。
    pub media_type: String,
    pub base64: String,
}

/// 聚合调用的输入：文本 + 可选图片。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MultimodalPrompt {
    pub text: String,
    pub images: Vec<ImagePart>,
}

impl MultimodalPrompt {
    /// 纯文本输入（与旧的 `&str` prompt 等价）。
    pub fn from_text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            images: Vec::new(),
        }
    }

    pub fn has_images(&self) -> bool {
        !self.images.is_empty()
    }
}

/// 一个可供模型调用的工具声明（协议无关，由 `agent_tools` 提供）。
#[derive(Debug, Clone, PartialEq)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    /// 参数的 JSON Schema（`{"type":"object","properties":{..}}` 形态）。
    pub input_schema: Value,
}

/// 模型请求的一次工具调用。
#[derive(Debug, Clone, PartialEq)]
pub struct ToolInvocation {
    /// 协议侧的调用 id（Anthropic `tool_use.id` / OpenAI `tool_calls[].id`）。
    /// 回填结果时必须**原样带回**，否则上游认不出这条结果属于哪次调用。
    pub id: String,
    pub name: String,
    /// 已解析的参数。OpenAI 的 `arguments` 是 JSON **字符串**，此处已解析成对象；
    /// 若模型吐出的不是合法 JSON，则为 [`Value::Null`]（执行层据此回一条可读错误给模型）。
    pub args: Value,
}

/// 一次工具执行的结果，待回填进消息历史。
#[derive(Debug, Clone, PartialEq)]
pub struct ToolResultMsg {
    /// 对应的 [`ToolInvocation::id`]。
    pub id: String,
    pub content: String,
    pub is_error: bool,
}

/// 一轮模型调用的产出。
#[derive(Debug, Clone, PartialEq)]
pub enum TurnOutcome {
    /// 模型给了最终文本、没有工具调用 → 循环结束。
    Text(String),
    /// 模型要调工具（`calls` 必非空）。
    ToolCalls {
        /// 本轮模型在调工具**之前**说的话（Anthropic 常先出一个 text block 再出 tool_use）。
        /// 保留它是为了轮数/时间预算到顶时能把「已说的部分」当作阶段结论交出去，而不是返回空串。
        text: String,
        /// 原样回填历史的 assistant 消息（协议原生形态）。
        ///
        /// **照抄上游返回、不重建**：重建会丢掉 thinking block 及其 `signature`，而 Anthropic
        /// 在带扩展思考的多轮里校验签名，缺失直接 400；OpenAI 侧同理会丢 `refusal` 等字段。
        assistant: Value,
        calls: Vec<ToolInvocation>,
    },
}

/// 构造 Anthropic 的 user content。
///
/// 无图片时返回**字符串**而非单元素 block 数组：与旧的纯文本请求逐字节一致，避免给不吃
/// content 数组的老网关平添兼容风险。有图片时图片在前、文本在后 —— 两家官方文档都建议
/// 图片先于引用它的文字，反过来放部分模型会答「没看到图片」。
fn anthropic_user_content(p: &MultimodalPrompt) -> Value {
    if p.images.is_empty() {
        return json!(p.text);
    }
    let mut blocks: Vec<Value> = p
        .images
        .iter()
        .map(|img| {
            json!({
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": img.media_type,
                    "data": img.base64,
                }
            })
        })
        .collect();
    blocks.push(json!({ "type": "text", "text": p.text }));
    Value::Array(blocks)
}

/// 构造 OpenAI Chat 的 user content。图片走 `image_url` + inline data URL
/// （不用外链 URL：本地文件根本没有可访问的 URL，且外链会把用户截图发给第三方图床）。
fn openai_user_content(p: &MultimodalPrompt) -> Value {
    if p.images.is_empty() {
        return json!(p.text);
    }
    let mut blocks: Vec<Value> = p
        .images
        .iter()
        .map(|img| {
            json!({
                "type": "image_url",
                "image_url": { "url": format!("data:{};base64,{}", img.media_type, img.base64) }
            })
        })
        .collect();
    blocks.push(json!({ "type": "text", "text": p.text }));
    Value::Array(blocks)
}

/// Anthropic 工具声明：`{name, description, input_schema}`。
fn anthropic_tools(tools: &[ToolDef]) -> Value {
    Value::Array(
        tools
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.input_schema,
                })
            })
            .collect(),
    )
}

/// OpenAI 工具声明：外层包一层 `{type:"function", function:{..}}`，且 schema 字段叫
/// `parameters` 而非 `input_schema` —— 这两处是两家协议最容易写错的差异点。
fn openai_tools(tools: &[ToolDef]) -> Value {
    Value::Array(
        tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.input_schema,
                    }
                })
            })
            .collect(),
    )
}

/// 解析 Anthropic 一轮响应，同时取出文本与工具调用。
///
/// 与 [`parse_anthropic_text`] 的区别：那个只取 text block。这里还要认 `tool_use`，
/// 并把整份 content 数组原样留下作为 assistant 历史。
///
/// **SSE 兜底**：工具循环发的是非流式请求，正常必是普通 JSON；但个别网关无论请求怎么发都回
/// SSE。那种情况下退化成「只取文本、当作没有工具调用」——比整轮报错好：成员至少还能给出
/// 一个基于已注入上下文的答案（降级而非失灵）。
fn parse_anthropic_turn(raw: &str) -> Option<TurnOutcome> {
    let Ok(body) = serde_json::from_str::<Value>(raw) else {
        return parse_anthropic_text(raw).map(TurnOutcome::Text);
    };
    let Some(content) = body.get("content").and_then(|c| c.as_array()) else {
        return parse_anthropic_text(raw).map(TurnOutcome::Text);
    };
    let calls: Vec<ToolInvocation> = content
        .iter()
        .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_use"))
        .filter_map(|b| {
            Some(ToolInvocation {
                id: b.get("id")?.as_str()?.to_string(),
                name: b.get("name")?.as_str()?.to_string(),
                // input 缺失按「无参数」处理：无参工具（如 list_dir 默认根目录）合法。
                args: b.get("input").cloned().unwrap_or_else(|| json!({})),
            })
        })
        .collect();
    let text = content
        .iter()
        .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
        .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
        .collect::<Vec<_>>()
        .join("");
    if calls.is_empty() {
        return Some(TurnOutcome::Text(text));
    }
    Some(TurnOutcome::ToolCalls {
        text,
        assistant: json!({ "role": "assistant", "content": content }),
        calls,
    })
}

/// 解析 OpenAI Chat 一轮响应（文本 + tool_calls）。SSE 兜底同 [`parse_anthropic_turn`]。
fn parse_openai_turn(raw: &str) -> Option<TurnOutcome> {
    let msg = serde_json::from_str::<Value>(raw).ok().and_then(|body| {
        body.get("choices")?
            .as_array()?
            .first()?
            .get("message")
            .cloned()
    });
    let Some(msg) = msg else {
        return parse_openai_text(raw).map(TurnOutcome::Text);
    };
    let text = msg
        .get("content")
        .and_then(|c| c.as_str())
        .unwrap_or_default()
        .to_string();
    let calls: Vec<ToolInvocation> = msg
        .get("tool_calls")
        .and_then(|t| t.as_array())
        .map(|arr| arr.iter().filter_map(parse_openai_tool_call).collect())
        .unwrap_or_default();
    if calls.is_empty() {
        return Some(TurnOutcome::Text(text));
    }
    // 部分网关回的 message 不带 role；缺了它下一轮上游会把这条消息当成非法角色而 400。
    let mut assistant = msg;
    if assistant.get("role").and_then(|r| r.as_str()) != Some("assistant") {
        assistant["role"] = json!("assistant");
    }
    Some(TurnOutcome::ToolCalls {
        text,
        assistant,
        calls,
    })
}

/// 解析单条 OpenAI `tool_calls[]`。`function.arguments` 是 JSON **字符串**（不是对象）。
fn parse_openai_tool_call(item: &Value) -> Option<ToolInvocation> {
    let f = item.get("function")?;
    let raw_args = f.get("arguments").and_then(|a| a.as_str()).unwrap_or("");
    let args = if raw_args.trim().is_empty() {
        // 无参工具的 arguments 常是 "" 或 "{}"，都按空对象处理。
        json!({})
    } else {
        // 模型偶尔吐出被截断/带多余文字的 arguments。此时留 Null，由执行层回一条
        // 「参数不是合法 JSON 对象」的 tool_result 让模型自己重试 —— 比静默当空参数
        // 去执行（可能读错文件）安全，也比整轮失败友好。
        serde_json::from_str::<Value>(raw_args).unwrap_or(Value::Null)
    };
    Some(ToolInvocation {
        id: item.get("id")?.as_str()?.to_string(),
        name: f.get("name")?.as_str()?.to_string(),
        args,
    })
}

/// 一次 [`ToolSession::turn`] 需要的上游参数。
///
/// 收成结构体而非逐个传：逐个会到 8 个参数，而调用方（聚合的成员循环）在整个循环里持有的
/// 本来就是同一份，每轮重复展开只会让调用点变噪音。
///
/// `Copy`：成员循环每轮要按**剩余时间预算**复制一份、只改 `request_timeout`
/// （见 `aggregate::run_member_turns`）。字段全是引用/标量，复制成本与 `&self` 等同。
#[derive(Clone, Copy)]
pub struct TurnParams<'a> {
    pub key: &'a ProviderKey,
    pub secret: &'a str,
    pub model: &'a str,
    pub max_tokens: u32,
    /// 对临时性上游错误（502/503/504/429/连接失败）自动重试。
    pub retry: bool,
    /// **单次** HTTP 请求的超时（含上游完整生成时间）。
    pub request_timeout: Duration,
}

/// 带工具的多轮会话。协议差异（消息形态、工具声明、结果回填）全收在这里，
/// 调用方（`aggregate` 的成员循环）只处理 [`TurnOutcome`]，不碰 JSON 形状。
///
/// 两家 API 都是无状态的：每轮把**整份历史**重发。这也是工具循环显著更耗 token 的原因，
/// 故上层开关默认关闭。
/// 小于这个长度的工具结果不值得压缩（占位说明自身就有几十字符，压了反而更长）。
const TRIM_PLACEHOLDER_MIN: usize = 200;

/// 被裁剪掉的工具结果留下的占位说明。
///
/// **刻意不留空串**：空内容会让模型以为那次调用什么都没读到，于是重新调一遍同样的工具 ——
/// 本意是省额度，结果更贵。这里如实写明「已省略、需要就重新取」，让模型能判断是否值得再调。
fn trim_placeholder(original_chars: usize) -> String {
    format!(
        "[此前一轮的工具结果（约 {original_chars} 字符）已省略以控制上下文长度。\
         若这部分内容对回答仍然必要，请重新调用相应工具获取。]"
    )
}

pub struct ToolSession {
    protocol: Protocol,
    /// 协议原生形态的消息历史。
    messages: Vec<Value>,
}

impl ToolSession {
    /// 以一条 user 消息开局（可含图片）。
    pub fn new(protocol: Protocol, prompt: &MultimodalPrompt) -> Self {
        let content = if protocol.is_openai() {
            openai_user_content(prompt)
        } else {
            anthropic_user_content(prompt)
        };
        Self {
            protocol,
            messages: vec![json!({ "role": "user", "content": content })],
        }
    }

    /// 当前消息历史（只读）。仅测试断言形状用 —— 生产路径不该直接摸 JSON。
    #[cfg(test)]
    pub fn messages(&self) -> &[Value] {
        &self.messages
    }

    /// 回填工具执行结果，供下一轮使用。
    ///
    /// 调用方必须为**上一轮的每一个** [`ToolInvocation`] 都给出结果（失败也要给，带
    /// `is_error`）：两家协议都要求调用与结果一一对应，缺一条上游直接 400 而不是忽略。
    pub fn push_tool_results(&mut self, results: &[ToolResultMsg]) {
        if results.is_empty() {
            return;
        }
        if self.protocol.is_openai() {
            // OpenAI：一条结果一条 `role:"tool"` 消息。协议里**没有** is_error 字段，
            // 故把错误标记写进正文 —— 否则模型看不出这次调用是失败的，会拿错误文本当数据用。
            for r in results {
                let content = if r.is_error {
                    format!("[工具执行失败] {}", r.content)
                } else {
                    r.content.clone()
                };
                self.messages.push(json!({
                    "role": "tool",
                    "tool_call_id": r.id,
                    "content": content,
                }));
            }
        } else {
            // Anthropic：所有结果打包进**一条** user 消息的 content 数组。
            // 拆成多条 user 消息会被判为连续 user 轮次而报错。
            let blocks: Vec<Value> = results
                .iter()
                .map(|r| {
                    json!({
                        "type": "tool_result",
                        "tool_use_id": r.id,
                        "content": r.content,
                        "is_error": r.is_error,
                    })
                })
                .collect();
            self.messages.push(json!({ "role": "user", "content": blocks }));
        }
    }

    /// 历史里工具结果正文的总字符数（用于判断是否该裁剪）。
    fn tool_result_chars(&self) -> usize {
        self.messages
            .iter()
            .map(|m| {
                let role = m.get("role").and_then(|r| r.as_str()).unwrap_or("");
                if role == "tool" {
                    // OpenAI：一条 tool 消息一个结果
                    m.get("content").and_then(|c| c.as_str()).map_or(0, |s| s.chars().count())
                } else if role == "user" {
                    // Anthropic：tool_result 块打包在 user 消息的 content 数组里
                    m.get("content")
                        .and_then(|c| c.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_result"))
                                .map(|b| {
                                    b.get("content").and_then(|c| c.as_str()).map_or(0, |s| s.chars().count())
                                })
                                .sum()
                        })
                        .unwrap_or(0)
                } else {
                    0
                }
            })
            .sum()
    }

    /// 把历史里**较早轮次**的工具结果正文压成占位说明，直到总量落回 `budget` 以内。
    ///
    /// ## 为什么不是删消息
    ///
    /// 两家协议都要求 `tool_use` / `tool_result` **一一对应**，删掉任何一条结果都会让上游
    /// 直接 400（不是忽略）。assistant 消息更不能动 —— 里面带扩展思考的 `signature`，
    /// 改一个字节签名校验就失败。故这里**只替换 tool_result 的正文字符串**：
    /// 消息条数、角色顺序、id 配对关系全部原样保留。
    ///
    /// ## 为什么从最旧的开始压
    ///
    /// 工具循环的信息价值随轮次递增：模型最后几轮读的正是它判断出「关键」的文件，
    /// 而第一轮往往是 `list_dir` 摸目录结构这类一次性信息。保新弃旧。
    ///
    /// ## 为什么保留占位说明而不是空串
    ///
    /// 空串会让模型以为那次调用什么都没读到、于是**重新读一遍**，反而更贵。
    /// 占位里写明「已省略、如仍需要请重新调用」，让它能判断是否值得再取。
    ///
    /// 返回被压缩的结果条数（0 = 未触发裁剪），供日志如实告知用户。
    pub fn trim_tool_history(&mut self, budget: usize) -> usize {
        // 本地递减计数，不在 iter_mut 循环里重新借用 self.messages。
        let mut total = self.tool_result_chars();
        if total <= budget {
            return 0;
        }
        // 最后一条带工具结果的消息**永不压缩**：那是模型刚拿到、正要用的材料。
        let last_tool_idx = self.messages.iter().rposition(|m| {
            let role = m.get("role").and_then(|r| r.as_str()).unwrap_or("");
            role == "tool"
                || (role == "user"
                    && m.get("content").and_then(|c| c.as_array()).is_some_and(|a| {
                        a.iter()
                            .any(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_result"))
                    }))
        });
        let mut compressed = 0usize;
        // 就地把一个 tool_result 正文换成占位，并把节省量从 total 里扣掉。
        let squash = |c: &mut Value, total: &mut usize, compressed: &mut usize| {
            let before = c.as_str().map_or(0, |s| s.chars().count());
            if before <= TRIM_PLACEHOLDER_MIN {
                return;
            }
            let ph = trim_placeholder(before);
            *total = total.saturating_sub(before.saturating_sub(ph.chars().count()));
            *c = json!(ph);
            *compressed += 1;
        };
        for (i, m) in self.messages.iter_mut().enumerate() {
            // 到最近一轮就停；已达标也停（避免把还够用的历史全压掉）
            if Some(i) == last_tool_idx || total <= budget {
                break;
            }
            let role = m.get("role").and_then(|r| r.as_str()).unwrap_or("").to_string();
            if role == "tool" {
                if let Some(c) = m.get_mut("content") {
                    squash(c, &mut total, &mut compressed);
                }
            } else if role == "user" {
                if let Some(arr) = m.get_mut("content").and_then(|c| c.as_array_mut()) {
                    for b in arr.iter_mut() {
                        if b.get("type").and_then(|t| t.as_str()) != Some("tool_result") {
                            continue; // text / image 块不动
                        }
                        if let Some(c) = b.get_mut("content") {
                            squash(c, &mut total, &mut compressed);
                        }
                    }
                }
            }
        }
        compressed
    }

    /// 追加一条来自 user 的补充指示（如「已到轮数上限，请直接出结论」）。
    ///
    /// Anthropic 侧**合并进上一条 user 消息**而不是新开一条：紧邻的两条 user 消息在部分
    /// 网关/版本上会被判为角色未交替而 400。OpenAI 侧 `tool` 之后接 `user` 是合法的，直接新增。
    pub fn push_user_note(&mut self, note: &str) {
        if !self.protocol.is_openai() {
            if let Some(last) = self.messages.last_mut() {
                if last.get("role").and_then(|r| r.as_str()) == Some("user") {
                    if let Some(arr) = last.get_mut("content").and_then(|c| c.as_array_mut()) {
                        arr.push(json!({ "type": "text", "text": note }));
                        return;
                    }
                }
            }
        }
        self.messages
            .push(json!({ "role": "user", "content": note }));
    }

    /// 发一轮请求。
    ///
    /// `tools` 传空时等价于「带完整历史的普通补全」—— 轮数/预算到顶后的**强制出结论**那一次
    /// 就这么调：不给工具，模型只能拿已有材料作答，不会再要求调用。
    ///
    /// 返回 [`TurnOutcome::ToolCalls`] 时 assistant 消息**已**追加进历史，调用方接着执行工具
    /// 并调 [`Self::push_tool_results`]；返回 [`TurnOutcome::Text`] 时历史不变（该轮即终点）。
    ///
    /// `p.retry` 对临时性上游错误重试。重发是安全的：失败时消息历史未被改动，重发的是同一份。
    pub async fn turn(
        &mut self,
        p: &TurnParams<'_>,
        tools: &[ToolDef],
    ) -> AppResult<TurnOutcome> {
        let max_attempts = if p.retry { RETRY_MAX_ATTEMPTS } else { 1 };
        let mut last_err = None;
        for attempt in 1..=max_attempts {
            let result = self.turn_once(p, tools).await;
            match result {
                Ok(outcome) => {
                    if let TurnOutcome::ToolCalls { assistant, .. } = &outcome {
                        self.messages.push(assistant.clone());
                    }
                    return Ok(outcome);
                }
                Err(e) => {
                    if attempt >= max_attempts || !is_retriable_upstream_error(&e) {
                        return Err(e);
                    }
                    last_err = Some(e);
                    tokio::time::sleep(Duration::from_millis(
                        RETRY_BASE_BACKOFF_MS * attempt as u64,
                    ))
                    .await;
                }
            }
        }
        Err(last_err.unwrap_or_else(|| AppError::upstream_msg("未知上游错误")))
    }

    /// 单次请求（不重试）。协议分支只在这里，上面的重试逻辑与协议无关。
    async fn turn_once(
        &self,
        p: &TurnParams<'_>,
        tools: &[ToolDef],
    ) -> AppResult<TurnOutcome> {
        let (key, model) = (p.key, p.model);
        let client = build_client(key)?;
        let openai = self.protocol.is_openai();
        let url = if openai {
            join_endpoint(&key.base_url, "/v1/chat/completions")
        } else {
            join_endpoint(&key.base_url, "/v1/messages")
        };
        let mut payload = json!({
            "model": model,
            "max_tokens": p.max_tokens,
            "messages": self.messages,
        });
        // tools 为空时**不发** tools 字段：部分网关对 `tools: []` 直接 400，
        // 而「强制出结论」那一轮正是空 tools。
        if !tools.is_empty() {
            if openai {
                payload["tools"] = openai_tools(tools);
                payload["tool_choice"] = json!("auto");
            } else {
                payload["tools"] = anthropic_tools(tools);
            }
        }

        // Prompt caching：
        // - OpenAI 协议**自动**缓存(≥1024 token 前缀,无需任何字段),我们已保证 messages
        //   前缀稳定(assistant 照抄、tool_result 只追加),它自然命中,故这里不动。
        // - Anthropic 协议要显式打 `cache_control` 断点。但你路由的是一堆第三方中转,
        //   个别严格中转会对未知字段回 400。故:已知不支持的端点直接不带;其余先带,
        //   若因它回 400 则自愈(去掉重发 + 记住该端点)。
        let want_cache = !openai && !cache_known_unsupported(&key.base_url);
        if want_cache {
            inject_anthropic_cache(&mut payload, !tools.is_empty());
        }

        let send = |body: &Value| {
            let mut req = client.post(&url).json(body).timeout(p.request_timeout);
            if !openai {
                // 版本头由 apply_auth 统一添加；这里只补缓存 beta 头。
                // 官方要求携带该 beta 头才启用扩展缓存能力；真 Anthropic 认，
                // 兼容中转忽略,严格中转的 400 会走下面的自愈。
                req = req.header("anthropic-beta", "prompt-caching-2024-07-31");
            }
            req = apply_auth(req, key, p.secret);
            req = apply_client_identity(req, self.protocol);
            req.send()
        };

        let resp = send(&payload).await?;
        let status = resp.status();
        // 同 text_completion：先读文本再解析，否则上游回 HTML 错误页时只能看到笼统的
        // 「error decoding response body」，看不出上游到底说了什么。
        let raw = resp.text().await?;
        let label = if openai { "OpenAI" } else { "Anthropic" };

        // 自愈：带了缓存字段、上游回 400、且响应体确认是缓存问题 → 去掉缓存重发一次,
        // 并记住该端点以后不再带。判据保守(见 looks_like_cache_rejection),不吞真正的 400。
        if want_cache && status == reqwest::StatusCode::BAD_REQUEST && looks_like_cache_rejection(&raw)
        {
            mark_cache_unsupported(&key.base_url);
            let mut plain = json!({
                "model": model,
                "max_tokens": p.max_tokens,
                "messages": self.messages,
            });
            if !tools.is_empty() {
                plain["tools"] = anthropic_tools(tools);
            }
            let resp2 = send(&plain).await?;
            let status2 = resp2.status();
            let raw2 = resp2.text().await?;
            if !status2.is_success() {
                return Err(AppError::upstream_http(
                    status2.as_u16(),
                    format!("{label} HTTP {status2}: {}", truncate_body(&raw2)),
                ));
            }
            record_usage_from_raw(&raw2);
            return parse_anthropic_turn(&raw2).ok_or_else(|| {
                AppError::upstream_http(
                    status2.as_u16(),
                    format!("{label} 响应无法解析: {}", truncate_body(&raw2)),
                )
            });
        }

        if !status.is_success() {
            return Err(AppError::upstream_http(
                status.as_u16(),
                format!("{label} HTTP {status}: {}", truncate_body(&raw)),
            ));
        }
        // 记 token 用量(含 cache_read/cache_creation)。命中缓存时 cache_read 会显著大于 0,
        // 即可在日志徽标里看到「缓存生效了」——这是本次改动可证伪的验收点。
        record_usage_from_raw(&raw);
        let parsed = if openai {
            parse_openai_turn(&raw)
        } else {
            parse_anthropic_turn(&raw)
        };
        parsed.ok_or_else(|| {
            AppError::upstream_http(
                status.as_u16(),
                format!("{label} 响应无法解析: {}", truncate_body(&raw)),
            )
        })
    }
}

/// 某次探测拿到的 HTTP 状态码是否代表「该 Key 可作路由候选」。
///
/// 借鉴 cc-switch 的健康判定：**可达性 ≠ 特定 API 路径返回 2xx**。只要拿到 HTTP 响应，
/// 就说明 endpoint 活着；4xx/5xx 多是路径不支持（如上游不暴露 /models 返 404/405）、
/// 限流（429）或临时故障（5xx），这些都不代表该 Key 的 chat 端点不可用——真正的可用性
/// 由请求时的故障转移兜底。唯一例外是鉴权失败（401/403）：密钥本身无效，留在候选池只会
/// 每次请求白跑一轮再转移，故直接判为不可用。
fn status_is_healthy(status: u16) -> bool {
    !matches!(status, 401 | 403)
}

/// 轻量健康探测（判「可达性」，而非旧版的「/v1/models 返回 2xx」）。
///
/// 旧实现用 `fetch_models` 判活：上游若不暴露 /models（404/405）会被误判为不可用、被路由
/// 排除，即便其 chat 端点完全正常（DeepSeek 等第三方即命中此坑）。现改为：拿到任意 HTTP
/// 响应即视为可达（鉴权失败 401/403 除外），仅连接层失败（DNS/连接/超时）判为不可达。
/// 返回 (是否健康, 延迟毫秒, 失败原因)。失败原因带出具体状态码或连接错误详情，供落日志排查——
/// 旧实现只返回 bool，日志只能打一句笼统的「连接层错误或 401/403」，无从定位。
pub async fn health_probe(key: &ProviderKey, secret: &str) -> (bool, u64, Option<String>) {
    let client = match build_client(key) {
        Ok(c) => c,
        Err(e) => return (false, 0, Some(format!("构建 HTTP 客户端失败：{e}"))),
    };
    // 用最便宜的 models 候选端点探测；只关心「有没有回应 + 状态码」，不解析 body。
    let url = model_endpoints(&key.base_url)
        .into_iter()
        .next()
        .unwrap_or_else(|| key.base_url.trim_end_matches('/').to_string());
    let mut req = apply_models_auth(client.get(&url).timeout(fast_timeout(key)), secret);
    // Anthropic 真实 API 的 GET /v1/models 需带版本头，否则 400（不影响健康判定，但让
    // 有效 Key 能拿到真实 200 与准确延迟）。
    //
    // 这里不能用 `apply_auth`（它会按协议只设一种鉴权头），因为 `apply_models_auth` 刻意
    // **两种鉴权头都带**——兼容把 Anthropic 协议挂在子路径、而模型列表是 OpenAI 风格的厂商。
    // 但版本头仍走 Protocol 的穷举能力方法，与其余四处同源。
    if let Some((h, v)) = key.protocol.version_header() {
        req = req.header(h, v);
    }

    let start = std::time::Instant::now();
    let (healthy, reason) = match req.send().await {
        Ok(resp) => {
            let code = resp.status().as_u16();
            if status_is_healthy(code) {
                (true, None)
            } else {
                // 拿到响应但鉴权失败（401/403）：密钥无效。带出确切状态码与端点。
                (false, Some(format!("连通探测鉴权失败 HTTP {code}（GET {url}）")))
            }
        }
        // 连接层失败（超时/连不上/DNS）：带出 reqwest 错误详情。
        Err(e) => (false, Some(format!("连接层失败（GET {url}）：{e}"))),
    };
    let latency = start.elapsed().as_millis() as u64;
    (healthy, latency, reason)
}

/// 真实补全健康探测（用户开启后使用）：发一个极小的真实 completion 请求，判「业务是否真能出结果」。
///
/// 与轻量探测的区别：轻量只看端点连通性（能 ping 通就算 up），真实探测发一次最小 completion
/// （max_tokens=1、prompt 一个字），能拿到成功响应才算 up。这样「可用/熔断」与真实业务一致，
/// 消除「连通正常却熔断」的割裂——代价是消耗极少量额度（1 token 输出）。
///
/// 探测模型用 `key.probe_model()`：优先取「映射 real_name / default_model / 模型列表 / 三档」里
/// **保证被上游接受的真实模型名**——这正是真实请求经映射改写后发出去的名字，使探测与业务同路。
/// 修复了旧实现「只看 default_model+models、Key 仅配自由映射时 models 为空 → 退回轻量 /models 探测
/// → 被 401/403 误杀熔断」的 bug。都没有可探测模型时才退回轻量探测。
///
/// 返回 (是否成功, 延迟毫秒, 失败原因)。失败原因供调用方落日志——旧实现丢弃了它，导致探测
/// 失败静默、无从排查。
pub async fn health_probe_real(
    key: &ProviderKey,
    secret: &str,
    message: &str,
) -> (bool, u64, Option<String>) {
    let Some(model) = key.probe_model() else {
        // 该 Key 没有任何可探测的真实模型名 → 无法发补全，退回轻量连通探测。
        let (ok, latency, reason) = health_probe(key, secret).await;
        return (
            ok,
            latency,
            reason.map(|r| format!("无可探测模型，退回轻量连通探测：{r}")),
        );
    };
    let start = std::time::Instant::now();
    // 极小请求：一个字 prompt、max_tokens=1。不重试（探测要快、如实反映当下）。
    // 探测超时封顶 8s（fast_timeout）：1 token 秒回，不跟随用户为慢厂商设的长超时，
    // 否则一个挂掉的慢 Key 会把它所在的那条探测并发槽占满（见 health::sweep_all_enabled，
    // PROBE_CONCURRENCY = 4），拖慢整轮扫描。
    let result = text_completion(key, secret, &model, message, 1, false, fast_timeout(key)).await;
    let latency = start.elapsed().as_millis() as u64;
    match result {
        Ok(_) => (true, latency, None),
        Err(e) => (false, latency, Some(format!("模型 {model}：{e}"))),
    }
}

// ---- 协议字段转换（proxy 跨协议故障转移时使用）----
//
// 覆盖范围（本轮从「纯文本」扩展）：
// - system：兼容 string 与 block 数组（[{type:"text",text}]）
// - 采样/控制字段：temperature / top_p / stop(_sequences) / stream 双向透传
// - 工具：tools / tool_choice 定义转换；tool_use ↔ tool_calls、tool_result ↔ role:"tool" 消息转换
// - max_tokens：OpenAI 侧兼容 max_completion_tokens；Anthropic 必填故缺省兜底
// 目的：跨协议故障转移对 agentic 客户端（Claude Code / Codex）仍可用，且不产生空 content 触发 400。

/// 抽取 Anthropic system 字段为纯文本，兼容 string 与 block 数组。
fn anthropic_system_text(body: &Value) -> Option<String> {
    match body.get("system") {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        Some(v @ Value::Array(_)) => {
            let t = extract_text_content(Some(v));
            (!t.is_empty()).then_some(t)
        }
        _ => None,
    }
}

/// 把源 body 中存在的键原样拷进目标 map（用于两协议同名的采样/控制字段）。
fn copy_through(src: &Value, dst: &mut serde_json::Map<String, Value>, keys: &[&str]) {
    for k in keys {
        if let Some(v) = src.get(*k) {
            dst.insert((*k).to_string(), v.clone());
        }
    }
}

/// 从请求体里读出 OpenAI 推理强度档位。
/// Codex/Responses 用 `reasoning.effort`（对象），Chat Completions 用顶层 `reasoning_effort`
/// （字符串）。两种形态都读，取值 minimal/low/medium/high/xhigh，让下游是 Chat 客户端时
/// 顶层 reasoning_effort 也能被 openai_to_anthropic 映射成 thinking 预算，不丢推理强度。
fn read_reasoning_effort(body: &Value) -> Option<String> {
    body.get("reasoning")
        .and_then(|r| r.get("effort"))
        .and_then(|e| e.as_str())
        .or_else(|| body.get("reasoning_effort").and_then(|e| e.as_str()))
        .map(|s| s.to_string())
}

/// OpenAI 推理强度档位 → Anthropic thinking 的 budget_tokens。
/// 两套机制不同：OpenAI 用离散档位，Anthropic 用 token 预算。取常见推荐区间做映射，
/// 并按 max_tokens 钳制（budget 必须 < max_tokens，且留出足够输出空间，否则 Anthropic 400）。
/// minimal → 不开思考（返回 None）；其余档位给递增预算。
fn effort_to_thinking_budget(effort: &str, max_tokens: u64) -> Option<u64> {
    let base = match effort.to_ascii_lowercase().as_str() {
        "minimal" => return None, // 最低档：不启用扩展思考，直接普通回答
        "low" => 2048,
        "medium" => 8192,
        "high" => 16384,
        "xhigh" => 32768,
        _ => return None, // 未知档位：不擅自开思考
    };
    // Anthropic 要求 thinking.budget_tokens < max_tokens，且要给最终回答留空间。
    // 取 max_tokens 的一半为上限钳制；若 max_tokens 过小（<2048）则不开思考。
    if max_tokens < 2048 {
        return None;
    }
    let cap = max_tokens / 2;
    Some(base.min(cap).max(1024))
}

/// Anthropic thinking.budget_tokens → OpenAI 推理强度档位（反向，补全对称）。
/// 按预算落到最接近的档位，供下游 Chat/Responses 客户端连 Anthropic-thinking 上游时还原语义。
fn thinking_budget_to_effort(budget: u64) -> &'static str {
    match budget {
        0..=3072 => "low",
        3073..=12288 => "medium",
        12289..=24576 => "high",
        _ => "xhigh",
    }
}

/// Chat Completions API 的顶层 `reasoning_effort` 只认 minimal/low/medium/high（无 xhigh），
/// 且是**字符串**而非 Responses 的 `reasoning:{effort}` 对象。把中枢里的档位归一到 Chat 认的集合：
/// xhigh 钳到 high，其余原样；未知值返回 None（不落字段，避免上游 400）。
fn effort_for_chat_completions(effort: &str) -> Option<&'static str> {
    match effort.to_ascii_lowercase().as_str() {
        "minimal" => Some("minimal"),
        "low" => Some("low"),
        "medium" => Some("medium"),
        "high" => Some("high"),
        "xhigh" => Some("high"), // Chat API 无 xhigh 档，钳到 high
        _ => None,
    }
}

/// Anthropic tools → OpenAI tools。
fn anthropic_tools_to_openai(tools: &Value) -> Option<Value> {
    let arr = tools.as_array()?;
    let out: Vec<Value> = arr
        .iter()
        .filter_map(|t| {
            let name = t.get("name")?.as_str()?;
            let mut f = serde_json::Map::new();
            f.insert("name".into(), json!(name));
            if let Some(d) = t.get("description") {
                f.insert("description".into(), d.clone());
            }
            if let Some(s) = t.get("input_schema") {
                f.insert("parameters".into(), s.clone());
            }
            Some(json!({ "type": "function", "function": f }))
        })
        .collect();
    (!out.is_empty()).then(|| json!(out))
}

/// Codex 桌面端把工具声明塞进 `input` 数组里的这种 item type，而**不用**顶层 `tools`。
const ADDITIONAL_TOOLS_ITEM: &str = "additional_tools";

/// Codex 「延迟工具检索器」的 Responses 工具 type。该 type 的声明**没有 `name` 字段**，
/// 名字即 type 本身；`execution:"client"` 表示由 Codex 客户端本地用 BM25 执行、不经上游。
const TOOL_SEARCH_TYPE: &str = "tool_search";

/// Codex 客户端执行完检索后回传的结果 item type：其 `tools[]` 才带回 MCP 工具的**真 schema**。
const TOOL_SEARCH_OUTPUT_ITEM: &str = "tool_search_output";

/// 模型发起检索的 item type（对应上游模型对 `tool_search` 的一次工具调用）。
const TOOL_SEARCH_CALL_ITEM: &str = "tool_search_call";

/// 收集本次 Responses 请求**声明**的全部工具（保持 Responses 原始形态，不做转换）。
///
/// 三处都要收：
/// 1. 顶层 `tools`；
/// 2. `input[]` 里 `type=="additional_tools"` 项的 `tools`（Codex 桌面端 exec 编排范式）；
/// 3. `input[]` 里 `type=="tool_search_output"` 项的 `tools`（**MCP 工具的唯一来源**）。
///
/// 为什么必须收第 2 处（2026-07-30 实测）：Codex 桌面端在 `tool_mode="code_mode_only"` 的模型
/// （gpt-5.6 系）下**顶层根本没有 `tools` 字段**，工具全在
/// `input[0] = {"type":"additional_tools","role":"developer","tools":[…]}` 里。
///
/// 为什么必须收第 3 处（2026-07-30 实测）：Codex 把 MCP 工具标 `defer_loading:true` **扣在客户端
/// 本地**，顶层 `tools` 里**永远不出现** `mcp__*` namespace（59 条含 `mcp__synaroute` 的抓包请求中，
/// 顶层命中数为 0）。模型必须先调 `tool_search`，Codex 本地检索后把真 schema 放进
/// `tool_search_output.tools[]` 回灌历史——该 item 是「下一次模型调用可用工具」的唯一载体。
/// 不收这一处，即使模型成功检索过，下一轮发往上游的请求里依旧没有 `synaroute_ai`，
/// 表现为「MCP 服务端握手正常、但模型坚称没有这个工具」。
///
/// 顶层在前，保证既有（CLI 等把工具放顶层的客户端）行为与顺序不变。
pub fn collect_declared_tools(body: &Value) -> Vec<Value> {
    let mut out: Vec<Value> = body
        .get("tools")
        .and_then(|t| t.as_array())
        .cloned()
        .unwrap_or_default();
    if let Some(items) = body.get("input").and_then(|i| i.as_array()) {
        for it in items {
            let hoist = matches!(
                it.get("type").and_then(|t| t.as_str()),
                Some(ADDITIONAL_TOOLS_ITEM) | Some(TOOL_SEARCH_OUTPUT_ITEM)
            );
            if !hoist {
                continue;
            }
            if let Some(arr) = it.get("tools").and_then(|t| t.as_array()) {
                out.extend(arr.iter().cloned());
            }
        }
    }
    out
}

/// 某个 Responses 工具声明对上游模型暴露的名字。
///
/// 多数工具带 `name`；`tool_search` 这类**无 `name`** 的 Codex 内置类型，名字即其 `type`。
/// 此前请求侧统一 `let Some(name) = t.get("name") else { continue }`，直接把 `tool_search`
/// 跳过 → 模型不知道有检索器可用 → 永远发不出 `tool_search_call` → MCP 工具永远拿不到 schema。
///
/// 只对**白名单内**的无名类型放行（当前仅 `tool_search`）。刻意不放行 `web_search`：
/// 那是**服务商侧**执行的内置工具（Codex 侧无 `execution` 字段、由 OpenAI 后端跑），
/// 经 SynaRoute 转到 Anthropic 上游后没有任何一方能执行它，暴露只会诱导模型空调。
fn declared_tool_name(t: &Value) -> Option<String> {
    if let Some(n) = t.get("name").and_then(|n| n.as_str()) {
        if !n.is_empty() {
            return Some(n.to_string());
        }
    }
    match t.get("type").and_then(|ty| ty.as_str()) {
        Some(TOOL_SEARCH_TYPE) => Some(TOOL_SEARCH_TYPE.to_string()),
        _ => None,
    }
}

/// 收集本次请求声明的「客户端执行型检索工具」名字集合（当前即 `tool_search`）。
/// 供响应侧判定：模型对该名字的调用要回程成 `tool_search_call` item（而非 `function_call`），
/// 否则 Codex 认不出、检索发不起来，延迟加载的 MCP 工具就永远解锁不了。
pub fn collect_search_tools(body: &Value) -> std::collections::HashSet<String> {
    collect_declared_tools(body)
        .iter()
        .filter(|t| t.get("type").and_then(|ty| ty.as_str()) == Some(TOOL_SEARCH_TYPE))
        .filter_map(declared_tool_name)
        .collect()
}

/// 从请求 tools 收集所有 Codex namespace 折叠工具的 namespace 名（如 `mcp__synaroute`）。
/// 供响应侧把上游模型回调的全名 `<ns>__<sub>` 拆回 Codex router 需要的 {name, namespace} 两字段。
/// 按长度降序排列，保证前缀匹配时优先匹配更长（更具体）的 namespace。
/// 经 [`collect_declared_tools`] 取工具，故顶层 `tools`、`additional_tools`、`tool_search_output`
/// 三种承载都覆盖——尤其第三种：MCP 的 `mcp__*` namespace **只**出现在那里，漏了就拆不回
/// `{namespace:"mcp__synaroute", name:"synaroute_ai"}`，Codex router 查表失败报 unsupported call。
pub fn collect_tool_namespaces(body: &Value) -> Vec<String> {
    let mut names: Vec<String> = collect_declared_tools(body)
        .iter()
        .filter(|t| t.get("type").and_then(|ty| ty.as_str()) == Some("namespace"))
        .filter_map(|t| t.get("name").and_then(|n| n.as_str()).map(|s| s.to_string()))
        .filter(|s| !s.is_empty())
        .collect();
    // 长的在前：`a__b` 与 `a` 同时存在时，`a__b__x` 应归到 `a__b` 而非 `a`。
    names.sort_by_key(|b| std::cmp::Reverse(b.len()));
    names.dedup();
    names
}

/// 从请求 tools 收集所有 Codex `type:"custom"` 工具名（如 `apply_patch`、桌面端的 `exec`）。
/// Codex 对这类工具期望响应侧回程 item type 为 `custom_tool_call`（非 `function_call`），
/// 否则 Codex router 认不出、工具执行失败。响应侧据此集合判定每个工具调用该发哪种 item type。
/// 经 [`collect_declared_tools`] 取工具，故三种承载都覆盖。
pub fn collect_custom_tools(body: &Value) -> std::collections::HashSet<String> {
    collect_declared_tools(body)
        .iter()
        .filter(|t| t.get("type").and_then(|ty| ty.as_str()) == Some("custom"))
        .filter_map(|t| t.get("name").and_then(|n| n.as_str()).map(|s| s.to_string()))
        .filter(|s| !s.is_empty())
        .collect()
}

/// 把 custom 工具的 Chat 形态参数（JSON 字符串 `arguments`）解包成 Codex 期望的裸字符串 `input`。
///
/// Codex 的 `custom_tool_call` item 携带的是**裸字符串** `input` 字段（如 apply_patch 的 patch 正文、
/// exec 的命令），而非 `function_call` 那样的 JSON `arguments`。上游模型（走 Anthropic 标准 tool_use）
/// 拿到的是 `{type:object, properties:{input:{type:string}}}` 之类 schema，回来的 arguments 通常形如
/// `{"input":"*** Begin Patch\n..."}`。此处按优先级解包：
/// 1. 能解析成 JSON 对象且含字符串键 `input` → 取该字符串（最常见）；
/// 2. 对象只有单个字符串值字段 → 取该值（模型用了别的键名时兜底）；
/// 3. 本身就是 JSON 字符串标量 → 取其内容；
/// 4. 其余（无法解析/结构不符）→ 原样返回（避免吞掉内容）。
pub fn unpack_custom_tool_input(arguments: &str) -> String {
    let trimmed = arguments.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    match serde_json::from_str::<Value>(trimmed) {
        Ok(Value::Object(map)) => {
            if let Some(s) = map.get("input").and_then(|v| v.as_str()) {
                return s.to_string();
            }
            // 单字段对象且值为字符串：容忍模型换了键名
            if map.len() == 1 {
                if let Some(s) = map.values().next().and_then(|v| v.as_str()) {
                    return s.to_string();
                }
            }
            arguments.to_string()
        }
        Ok(Value::String(s)) => s,
        _ => arguments.to_string(),
    }
}

/// 把上游模型回调的工具全名按已知 namespace 列表拆回 (namespace, sub_name)。
/// 全名形如 `<ns>__<sub>`；命中某 namespace 前缀则返回 (Some(ns), sub)，否则 (None, 原名)。
/// 平铺工具（如 Codex 内置 update_plan）无 namespace 前缀，原样返回，不受影响。
pub fn split_namespaced_tool_name(full: &str, namespaces: &[String]) -> (Option<String>, String) {
    for ns in namespaces {
        let prefix = format!("{ns}__");
        if let Some(sub) = full.strip_prefix(&prefix) {
            if !sub.is_empty() {
                return (Some(ns.clone()), sub.to_string());
            }
        }
    }
    (None, full.to_string())
}

/// [`split_namespaced_tool_name`] 的逆运算：把 Codex 历史 item 的 `{name, namespace}` 两字段
/// 拼回上游模型看到的**全名** `<ns>__<sub>`。无 `namespace` 字段时原样返回 `name`。
///
/// 为什么必需（2026-07-30 实测 `unsupported call` 根因）：请求侧把 namespace 折叠工具展开成
/// 全名（`mcp__synaroute__synaroute_ai`）暴露给上游模型，但历史里 Codex 存的是拆开的两字段
/// （`name:"synaroute_ai"` + `namespace:"mcp__synaroute"`）。若还原历史时只取 `name`，模型看到
/// 「我上一轮用 `synaroute_ai` 这个名字调用过」，下一轮就照抄这个短名 → 响应侧
/// `split_namespaced_tool_name` 拆不出 namespace（短名无前缀）→ 回程 item 缺 `namespace` 字段
/// → Codex router 查 `{namespace:None, name:"synaroute_ai"}` 匹配不到 → `unsupported call`。
/// 实机 rollout 三次调用中，唯一失败的那次正是 `ns=-`（模型抄了短名）。
///
/// 已是全名（`name` 本身就带 `<ns>__` 前缀）时不重复拼接，避免 `mcp__x__mcp__x__foo`。
fn join_namespaced_tool_name(item: &Value) -> String {
    let name = item.get("name").and_then(|n| n.as_str()).unwrap_or("");
    let Some(ns) = item.get("namespace").and_then(|n| n.as_str()) else {
        return name.to_string();
    };
    if ns.is_empty() || name.is_empty() {
        return name.to_string();
    }
    let prefix = format!("{ns}__");
    if name.starts_with(&prefix) {
        name.to_string()
    } else {
        format!("{prefix}{name}")
    }
}

/// OpenAI tools → Anthropic tools。
///
/// 支持三种上游工具形态,统一转成 Anthropic 的 `{name, description, input_schema}`:
/// 1. **Chat 嵌套**:`{type:"function", function:{name, description, parameters}}`。
/// 2. **Responses 扁平**:`{type:"function", name, description, parameters}`（无 function 包一层）。
/// 3. **Codex namespace 折叠**:`{type:"namespace", name:"mcp__x", tools:[{type:"function",
///    name:"foo", parameters}]}`——Codex 把 MCP 工具折叠进 namespace 容器,子工具在 `tools[]` 里。
///    原生 OpenAI 模型认识 namespace 并会用全名 `mcp__x__foo` 调用;但 Anthropic 无 namespace
///    概念,故在此**展开**:每个子工具提升为独立工具,名字拼成 `<namespace>__<子工具>`
///    （正是上游模型回调时用的名）。响应侧再靠 [`split_namespaced_tool_name`] 拆回
///    {name, namespace} 两字段——Codex router 用结构化 ToolName{namespace, name} 查表，
///    **不拆 name 字符串**，故必须分开填，否则查 {namespace:None, name:全名} 匹配不上 → `unsupported call`。
fn openai_tools_to_anthropic(tools: &Value) -> Option<Value> {
    let arr = tools.as_array()?;
    let mut out: Vec<Value> = vec![];
    for t in arr {
        match t.get("type").and_then(|ty| ty.as_str()) {
            // Codex namespace 折叠工具:展开 tools[] 里的每个子工具为 <namespace>__<子工具>。
            Some("namespace") => {
                let ns = t.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let empty_vec = vec![];
                let subs = t.get("tools").and_then(|s| s.as_array()).unwrap_or(&empty_vec);
                for sub in subs {
                    // 子工具可能是扁平 {name,parameters} 或嵌套 {function:{..}}——都兼容。
                    let inner = sub.get("function").unwrap_or(sub);
                    let Some(sub_name) = inner.get("name").and_then(|n| n.as_str()) else { continue };
                    // 全名:namespace 非空时拼 `<ns>__<sub>`,否则退化为子工具名本身。
                    let full = if ns.is_empty() {
                        sub_name.to_string()
                    } else {
                        format!("{ns}__{sub_name}")
                    };
                    let mut a = serde_json::Map::new();
                    a.insert("name".into(), json!(full));
                    if let Some(d) = inner.get("description") {
                        a.insert("description".into(), d.clone());
                    }
                    a.insert(
                        "input_schema".into(),
                        inner.get("parameters").cloned().unwrap_or_else(|| json!({ "type": "object" })),
                    );
                    out.push(Value::Object(a));
                }
            }
            // 普通函数:Chat 嵌套（有 function 子对象）或 Responses 扁平（顶层直接带 name）。
            _ => {
                let f = t.get("function").unwrap_or(t);
                let Some(name) = f.get("name").and_then(|n| n.as_str()) else { continue };
                let mut a = serde_json::Map::new();
                a.insert("name".into(), json!(name));
                if let Some(d) = f.get("description") {
                    a.insert("description".into(), d.clone());
                }
                // Anthropic 要求 input_schema 至少是个 object schema。
                // Codex 的 type:"custom" 工具（apply_patch 等）用驼峰 `inputSchema` 承载 schema，
                // 普通 function 用 `parameters`；两者都兜底，避免 custom 工具丢 schema → 上游拿到空对象。
                a.insert(
                    "input_schema".into(),
                    f.get("parameters")
                        .or_else(|| f.get("inputSchema"))
                        .cloned()
                        .unwrap_or_else(|| json!({ "type": "object" })),
                );
                out.push(Value::Object(a));
            }
        }
    }
    (!out.is_empty()).then(|| json!(out))
}

/// 将 Anthropic Messages 请求体转为 OpenAI Chat Completions 请求体。
pub fn anthropic_to_openai(body: &Value) -> Value {
    let mut out = serde_json::Map::new();
    out.insert("model".into(), body.get("model").cloned().unwrap_or(Value::Null));
    out.insert(
        "max_tokens".into(),
        body.get("max_tokens").cloned().unwrap_or(json!(4096)),
    );

    let mut messages: Vec<Value> = vec![];
    // system（string 或 block 数组）→ system 消息
    if let Some(sys) = anthropic_system_text(body) {
        messages.push(json!({ "role": "system", "content": sys }));
    }

    if let Some(arr) = body.get("messages").and_then(|m| m.as_array()) {
        for m in arr {
            let role = m.get("role").and_then(|r| r.as_str()).unwrap_or("user");
            let content = m.get("content");
            match content {
                // 字符串 content：直接透传
                Some(Value::String(s)) => {
                    messages.push(json!({ "role": role, "content": s }));
                }
                // block 数组：拆出 text / tool_use / tool_result
                Some(Value::Array(blocks)) => {
                    let mut text = String::new();
                    let mut tool_calls: Vec<Value> = vec![];
                    let mut tool_results: Vec<Value> = vec![];
                    for b in blocks {
                        match b.get("type").and_then(|t| t.as_str()) {
                            Some("text") => {
                                if let Some(t) = b.get("text").and_then(|t| t.as_str()) {
                                    text.push_str(t);
                                }
                            }
                            Some("tool_use") => {
                                let args = b
                                    .get("input")
                                    .map(|i| i.to_string())
                                    .unwrap_or_else(|| "{}".into());
                                tool_calls.push(json!({
                                    "id": b.get("id").cloned().unwrap_or(json!("")),
                                    "type": "function",
                                    "function": {
                                        "name": b.get("name").cloned().unwrap_or(json!("")),
                                        "arguments": args
                                    }
                                }));
                            }
                            Some("tool_result") => {
                                // → OpenAI 独立的 role:"tool" 消息
                                tool_results.push(json!({
                                    "role": "tool",
                                    "tool_call_id": b.get("tool_use_id").cloned().unwrap_or(json!("")),
                                    "content": extract_text_content(b.get("content"))
                                }));
                            }
                            _ => {}
                        }
                    }
                    if role == "assistant" && !tool_calls.is_empty() {
                        // assistant + 工具调用：content 允许为 null
                        let c = if text.is_empty() { Value::Null } else { json!(text) };
                        messages.push(json!({ "role": "assistant", "content": c, "tool_calls": tool_calls }));
                    } else {
                        if !text.is_empty() {
                            messages.push(json!({ "role": role, "content": text }));
                        }
                        messages.append(&mut tool_results);
                    }
                }
                _ => {}
            }
        }
    }
    out.insert("messages".into(), json!(messages));

    // 采样/控制字段透传（键名两协议一致）
    copy_through(body, &mut out, &["temperature", "top_p", "stream"]);
    // Anthropic thinking.budget_tokens → OpenAI reasoning.effort（反向映射，补全对称）：
    // 下游 Anthropic 客户端开了扩展思考、上游是 OpenAI 协议时，把 token 预算落到最近的推理档位，
    // 使推理强度语义不在跨协议时丢失。
    if let Some(budget) = body
        .get("thinking")
        .and_then(|t| t.get("budget_tokens"))
        .and_then(|b| b.as_u64())
    {
        out.insert("reasoning".into(), json!({ "effort": thinking_budget_to_effort(budget) }));
    }
    // Anthropic stop_sequences → OpenAI stop
    if let Some(s) = body.get("stop_sequences") {
        out.insert("stop".into(), s.clone());
    }
    // tools / tool_choice
    if let Some(t) = body.get("tools").and_then(anthropic_tools_to_openai) {
        out.insert("tools".into(), t);
    }
    if let Some(tc) = body.get("tool_choice") {
        let mapped = match tc.get("type").and_then(|t| t.as_str()) {
            Some("auto") => json!("auto"),
            Some("any") => json!("required"),
            Some("tool") => json!({ "type": "function", "function": { "name": tc.get("name").cloned().unwrap_or(json!("")) } }),
            _ => tc.clone(),
        };
        out.insert("tool_choice".into(), mapped);
    }
    Value::Object(out)
}

/// 将 OpenAI Chat Completions 请求体转为 Anthropic Messages 请求体。
pub fn openai_to_anthropic(body: &Value) -> Value {
    let mut out = serde_json::Map::new();
    out.insert("model".into(), body.get("model").cloned().unwrap_or(Value::Null));
    // Anthropic max_tokens 必填：优先 max_tokens，回退 max_completion_tokens，最后兜底
    let max_tokens = body
        .get("max_tokens")
        .or_else(|| body.get("max_completion_tokens"))
        .cloned()
        .unwrap_or(json!(4096));
    out.insert("max_tokens".into(), max_tokens);

    let mut system = String::new();
    let mut messages: Vec<Value> = vec![];
    // 连续的 role:"tool" 消息要合并进「一个 user 消息」的 tool_result 块（Anthropic 要求）。
    let mut pending_tool_results: Vec<Value> = vec![];
    let flush = |pending: &mut Vec<Value>, messages: &mut Vec<Value>| {
        if !pending.is_empty() {
            messages.push(json!({ "role": "user", "content": std::mem::take(pending) }));
        }
    };

    if let Some(arr) = body.get("messages").and_then(|m| m.as_array()) {
        for m in arr {
            let role = m.get("role").and_then(|r| r.as_str()).unwrap_or("user");
            if role == "tool" {
                // 累积为 tool_result 块，稍后并入 user 消息
                pending_tool_results.push(json!({
                    "type": "tool_result",
                    "tool_use_id": m.get("tool_call_id").cloned().unwrap_or(json!("")),
                    "content": extract_text_content(m.get("content"))
                }));
                continue;
            }
            // 遇到非 tool 消息，先把待定的 tool_result 收尾成一个 user 消息
            flush(&mut pending_tool_results, &mut messages);

            match role {
                // `developer` 与 `system` 同等对待：OpenAI 侧 developer 就是「开发者指令」，
                // 语义等价于旧的 system（o1 系起改名，Responses 协议沿用）。
                //
                // 此前 developer 落进下面的 `_ =>` 分支被降级成普通 user 消息。这不只是形式问题：
                // Codex 桌面端把**技能（skills）说明**放在 developer 消息里（`# Using skills` +
                // `### Available skills` 清单，实测 offset 见 docs/13 第十一节），其中含
                // 「Trigger rules: 用户点名或任务匹配时**必须**使用该 skill」这类强指令。
                // Anthropic 的 `system` 是独立字段、权重高于对话消息；降级成 user 后这些指令
                // 与用户自己的话混在一起，模型遵守程度下降。多条 developer 消息按出现顺序拼接。
                "system" | "developer" => {
                    let text = extract_text_content(m.get("content"));
                    if !text.is_empty() {
                        // 多段之间补换行，避免「上一段末尾」与「下一段开头」黏成一行改变语义。
                        if !system.is_empty() {
                            system.push_str("\n\n");
                        }
                        system.push_str(&text);
                    }
                }
                "assistant" => {
                    let text = extract_text_content(m.get("content"));
                    let mut blocks: Vec<Value> = vec![];
                    if !text.is_empty() {
                        blocks.push(json!({ "type": "text", "text": text }));
                    }
                    // tool_calls → tool_use 块
                    if let Some(tcs) = m.get("tool_calls").and_then(|t| t.as_array()) {
                        for tc in tcs {
                            let f = tc.get("function");
                            let input = f
                                .and_then(|f| f.get("arguments"))
                                .and_then(|a| a.as_str())
                                .and_then(|s| serde_json::from_str::<Value>(s).ok())
                                .unwrap_or(json!({}));
                            blocks.push(json!({
                                "type": "tool_use",
                                "id": tc.get("id").cloned().unwrap_or(json!("")),
                                "name": f.and_then(|f| f.get("name")).cloned().unwrap_or(json!("")),
                                "input": input
                            }));
                        }
                    }
                    // 空 assistant（无文本无工具）跳过，避免 Anthropic 400
                    if !blocks.is_empty() {
                        messages.push(json!({ "role": "assistant", "content": blocks }));
                    }
                }
                _ => {
                    // user（及未知 role 归一为 user）
                    let text = extract_text_content(m.get("content"));
                    if !text.is_empty() {
                        messages.push(json!({ "role": "user", "content": text }));
                    }
                }
            }
        }
    }
    // 收尾：末尾若还有待定 tool_result
    flush(&mut pending_tool_results, &mut messages);

    out.insert("messages".into(), json!(messages));
    if !system.is_empty() {
        out.insert("system".into(), json!(system));
    }

    copy_through(body, &mut out, &["temperature", "top_p", "stream"]);
    // 推理强度：OpenAI reasoning.effort → Anthropic thinking.budget_tokens（两套机制的语义映射）。
    // Codex 改推理强度经此落到 Claude 上游的扩展思考预算；minimal/未知档不开思考。
    // 注意 Anthropic 开 thinking 时要求 temperature=1（否则 400），故一并归一。
    if let Some(effort) = read_reasoning_effort(body) {
        let max_tokens = out
            .get("max_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(4096);
        if let Some(budget) = effort_to_thinking_budget(&effort, max_tokens) {
            out.insert(
                "thinking".into(),
                json!({ "type": "enabled", "budget_tokens": budget }),
            );
            // Anthropic 扩展思考要求 temperature 固定为 1（top_p 亦不可与 thinking 同用）。
            out.insert("temperature".into(), json!(1));
            out.remove("top_p");
        }
    }
    // OpenAI stop → Anthropic stop_sequences（Anthropic 要求数组）
    if let Some(s) = body.get("stop") {
        let seqs = match s {
            Value::String(_) => json!([s.clone()]),
            other => other.clone(),
        };
        out.insert("stop_sequences".into(), seqs);
    }
    if let Some(t) = body.get("tools").and_then(openai_tools_to_anthropic) {
        out.insert("tools".into(), t);
    }
    if let Some(tc) = body.get("tool_choice") {
        let mapped = match tc {
            Value::String(s) if s == "auto" => json!({ "type": "auto" }),
            Value::String(s) if s == "required" => json!({ "type": "any" }),
            Value::String(s) if s == "none" => json!({ "type": "auto" }), // Anthropic 无 none，退回 auto
            Value::Object(_) => tc
                .get("function")
                .and_then(|f| f.get("name"))
                .map(|n| json!({ "type": "tool", "name": n.clone() }))
                .unwrap_or(json!({ "type": "auto" })),
            _ => json!({ "type": "auto" }),
        };
        out.insert("tool_choice".into(), mapped);
    }
    Value::Object(out)
}

/// 将 OpenAI Chat Completions 响应体转为 Anthropic Messages 响应体。
/// 文本与**工具调用**都转（tool_calls → tool_use 块）。
/// 用于下游是 Anthropic 客户端、上游 Key 是 OpenAI 协议的跨协议故障转移。
pub fn openai_resp_to_anthropic(body: &Value) -> Value {
    // OpenAI: choices[0].message.{content,tool_calls}、finish_reason、usage.{prompt,completion}_tokens
    let choice0 = body
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|arr| arr.first());
    let message = choice0.and_then(|c| c.get("message"));
    let text = message
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or("");
    let model = body.get("model").cloned().unwrap_or(Value::Null);
    let id = body
        .get("id")
        .and_then(|i| i.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("msg_{}", uuid_like()));
    let finish = choice0
        .and_then(|c| c.get("finish_reason"))
        .and_then(|r| r.as_str())
        .unwrap_or("end_turn");
    // OpenAI finish_reason → Anthropic stop_reason
    let stop_reason = match finish {
        "stop" => "end_turn",
        "length" => "max_tokens",
        "tool_calls" | "function_call" => "tool_use",
        other => other,
    };
    let input_tokens = body
        .get("usage")
        .and_then(|u| u.get("prompt_tokens"))
        .and_then(|t| t.as_u64())
        .unwrap_or(0);
    let output_tokens = body
        .get("usage")
        .and_then(|u| u.get("completion_tokens"))
        .and_then(|t| t.as_u64())
        .unwrap_or(0);
    // content 块：文本（非空才发）+ 每个 tool_call 一个 tool_use 块。
    // 工具必须翻译：早期实现只取文本，却把 finish_reason:"tool_calls" 映射成
    // stop_reason:"tool_use"，产出「声明了工具调用但 content 里没有 tool_use 块」的自相矛盾响应
    // → 下游客户端（Claude 桌面端 / CLI）无工具可执行，表现为模型从不调用工具。
    let mut content: Vec<Value> = vec![];
    if !text.is_empty() {
        content.push(json!({ "type": "text", "text": text }));
    }
    if let Some(tcs) = message.and_then(|m| m.get("tool_calls")).and_then(|t| t.as_array()) {
        for tc in tcs {
            let f = tc.get("function");
            let name = f
                .and_then(|f| f.get("name"))
                .and_then(|n| n.as_str())
                .unwrap_or("");
            if name.is_empty() {
                continue;
            }
            let raw_args = f
                .and_then(|f| f.get("arguments"))
                .and_then(|a| a.as_str())
                .unwrap_or("");
            // Anthropic 的 tool_use.input 必须是 JSON **对象**；Chat 的 arguments 是 JSON 字符串。
            // 解析失败/为空时兜底空对象，避免下游 schema 校验直接报错。
            let input = serde_json::from_str::<Value>(raw_args.trim())
                .ok()
                .filter(|v| v.is_object())
                .unwrap_or_else(|| json!({}));
            let id = tc
                .get("id")
                .and_then(|i| i.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("toolu_{}", uuid_like()));
            content.push(json!({ "type": "tool_use", "id": id, "name": name, "input": input }));
        }
    }
    // 全空（既无文本又无工具）时保留一个空文本块：Anthropic 响应的 content 不应为空数组。
    if content.is_empty() {
        content.push(json!({ "type": "text", "text": "" }));
    }
    json!({
        "id": id,
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": content,
        "stop_reason": stop_reason,
        "stop_sequence": Value::Null,
        "usage": { "input_tokens": input_tokens, "output_tokens": output_tokens }
    })
}

/// 将 Anthropic Messages 响应体转为 OpenAI Chat Completions 响应体。
/// 文本与**工具调用**都转（tool_use 块 → tool_calls）。
/// 用于下游是 OpenAI 客户端、上游 Key 是 Anthropic 协议的跨协议故障转移。
pub fn anthropic_resp_to_openai(body: &Value) -> Value {
    // Anthropic: content[].{text,tool_use}、stop_reason、usage.{input,output}_tokens
    let text = extract_text_content(body.get("content"));
    let model = body.get("model").cloned().unwrap_or(Value::Null);
    let id = body
        .get("id")
        .and_then(|i| i.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("chatcmpl-{}", uuid_like()));
    let stop_reason = body
        .get("stop_reason")
        .and_then(|r| r.as_str())
        .unwrap_or("end_turn");
    // Anthropic stop_reason → OpenAI finish_reason
    let finish = match stop_reason {
        "end_turn" | "stop_sequence" => "stop",
        "max_tokens" => "length",
        "tool_use" => "tool_calls",
        other => other,
    };
    let prompt_tokens = body
        .get("usage")
        .and_then(|u| u.get("input_tokens"))
        .and_then(|t| t.as_u64())
        .unwrap_or(0);
    let completion_tokens = body
        .get("usage")
        .and_then(|u| u.get("output_tokens"))
        .and_then(|t| t.as_u64())
        .unwrap_or(0);
    // content[] 里的 tool_use 块 → Chat 的 tool_calls（input 对象序列化回 arguments JSON 字符串）。
    // 与 [`openai_resp_to_anthropic`] 对称：任一侧只搬文本，跨协议故障转移就会静默吃掉工具调用。
    let mut tool_calls: Vec<Value> = vec![];
    if let Some(blocks) = body.get("content").and_then(|c| c.as_array()) {
        for b in blocks {
            if b.get("type").and_then(|t| t.as_str()) != Some("tool_use") {
                continue;
            }
            let name = b.get("name").and_then(|n| n.as_str()).unwrap_or("");
            if name.is_empty() {
                continue;
            }
            let arguments = b
                .get("input")
                .map(|i| i.to_string())
                .unwrap_or_else(|| "{}".to_string());
            let call_id = b
                .get("id")
                .and_then(|i| i.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("call_{}", uuid_like()));
            tool_calls.push(json!({
                "id": call_id,
                "type": "function",
                "function": { "name": name, "arguments": arguments }
            }));
        }
    }
    let mut message = serde_json::Map::new();
    message.insert("role".into(), json!("assistant"));
    // Chat 语义：有工具调用时 content 可为 null；无则给文本（可能是空串）。
    message.insert(
        "content".into(),
        if text.is_empty() && !tool_calls.is_empty() {
            Value::Null
        } else {
            json!(text)
        },
    );
    if !tool_calls.is_empty() {
        message.insert("tool_calls".into(), json!(tool_calls));
    }
    json!({
        "id": id,
        "object": "chat.completion",
        "created": chrono::Utc::now().timestamp(),
        "model": model,
        "choices": [ {
            "index": 0,
            "message": Value::Object(message),
            "finish_reason": finish
        } ],
        "usage": {
            "prompt_tokens": prompt_tokens,
            "completion_tokens": completion_tokens,
            "total_tokens": prompt_tokens + completion_tokens
        }
    })
}

// ---- Responses API ↔ Chat Completions 转换（Codex 走 Responses，多数第三方上游只支持 Chat）----
//
// 设计：以 Chat Completions 为中枢做归一化。下游 Responses 请求 → Chat 发给上游；上游 Chat
// 响应 → Responses 回给 Codex。仅在「下游是 Responses、上游是 Chat」这一跨形态时启用；
// 两端同为 Responses 时纯透传（不进中枢，避免无谓的信息损耗）。

/// 跨协议**请求体**转换：下游协议 `from` → 上游协议 `to`，以 Chat Completions 为中枢。
/// 同协议直通（不经中枢，避免 Responses→Chat→Responses 往返丢信息）。
/// `model` 已由调用方写入 body，此处只做协议形态转换。
/// 同 [`convert_request`]，但**按值接收**：同协议时零拷贝直接把 body 移动出去（P2-5）。
///
/// 为什么需要它：同协议是**最常见**的场景（Claude Code → Anthropic Key、
/// Codex → Responses Key），而 `convert_request(&payload, ..)` 在这条路上要做一次
/// **整棵树的深拷贝**（`body.clone()`）。`serde_json::Value` 是指针密集树，200 KB 的 JSON
/// 展开后堆占用常达 1~2 MB、节点数以万计——白做一次全树拷贝就是数万次小分配 + 指针追逐，
/// 而调用方拿到结果后原 body 立刻不再需要。故障转移时每个候选还要重来一遍。
///
/// 跨协议路径与 [`convert_request`] 完全一致（转换本身必须重建结构，无法零拷贝）。
pub fn convert_request_owned(body: Value, from: Protocol, to: Protocol) -> Value {
    if from == to {
        // 零拷贝：直接移动。这也保住了「同协议原样透传」的语义
        // （见下方 convert_request 里同一分支的注释）。
        return body;
    }
    convert_request(&body, from, to)
}

pub fn convert_request(body: &Value, from: Protocol, to: Protocol) -> Value {
    if from == to {
        return body.clone();
    }
    // 1. 下游 → Chat 中枢
    let chat = match from {
        Protocol::Anthropic => anthropic_to_openai(body),
        Protocol::OpenaiChat => body.clone(),
        Protocol::OpenaiResponses => responses_to_chat(body),
    };
    // 2. Chat 中枢 → 上游
    match to {
        Protocol::Anthropic => openai_to_anthropic(&chat),
        // Chat 上游：中枢携带的是 Responses 风格 `reasoning:{effort}` 对象，而 Chat Completions
        // API 认的是顶层 `reasoning_effort` 字符串（minimal/low/medium/high，无 xhigh）。若原样把
        // reasoning 对象发给 Chat 上游，推理强度会被忽略，严格上游还可能因未知字段报 400。
        // 故在此把 reasoning.effort 归一并落成顶层 reasoning_effort，同时移除 reasoning 对象。
        Protocol::OpenaiChat => {
            let mut chat = chat;
            if let Some(obj) = chat.as_object_mut() {
                let effort = obj
                    .get("reasoning")
                    .and_then(|r| r.get("effort"))
                    .and_then(|e| e.as_str())
                    .and_then(effort_for_chat_completions);
                if let Some(e) = effort {
                    obj.insert("reasoning_effort".into(), Value::String(e.to_string()));
                }
                obj.remove("reasoning");
            }
            chat
        }
        Protocol::OpenaiResponses => chat_to_responses(&chat),
    }
}

/// 跨协议**响应体**转换：上游协议 `from` → 下游协议 `to`，以 Chat Completions 为中枢。
/// 同协议直通。用于非流式响应回写给下游客户端。
/// 生产非流式路径统一走 [`convert_response_ext`]（可带 custom / search 工具集合）；此简单签名
/// 保留供测试与无特殊工具场景，等价于 `convert_response_ext(.., &空集合, &空集合)`。
#[allow(dead_code)]
pub fn convert_response(body: &Value, from: Protocol, to: Protocol) -> Value {
    convert_response_ext(
        body,
        from,
        to,
        &std::collections::HashSet::new(),
        &std::collections::HashSet::new(),
    )
}

/// 同 [`convert_response`]，但在 Chat→Responses 路径上用 [`chat_resp_to_responses_ext`]：
/// 把 `custom_tools` 命中的回程 item type 改写为 `custom_tool_call`、
/// `search_tools` 命中的改写为 `tool_search_call`。
/// 非流式路径专用；流式路径在 [`SseTranslator`] 内直接判定。
pub fn convert_response_ext(
    body: &Value,
    from: Protocol,
    to: Protocol,
    custom_tools: &std::collections::HashSet<String>,
    search_tools: &std::collections::HashSet<String>,
) -> Value {
    if from == to {
        return body.clone();
    }
    let chat = match from {
        Protocol::Anthropic => anthropic_resp_to_openai(body),
        Protocol::OpenaiChat => body.clone(),
        Protocol::OpenaiResponses => responses_resp_to_chat(body),
    };
    match to {
        Protocol::Anthropic => openai_resp_to_anthropic(&chat),
        Protocol::OpenaiChat => chat,
        Protocol::OpenaiResponses => {
            chat_resp_to_responses_ext(&chat, custom_tools, search_tools)
        }
    }
}

/// Responses 请求体 → Chat Completions 请求体。
/// 映射：instructions → system 消息；input（字符串或 item 数组）→ messages；
/// max_output_tokens → max_tokens；tools（Responses 扁平 {type:function,name,..}）→ Chat {type:function,function:{..}}。
pub fn responses_to_chat(body: &Value) -> Value {
    let mut out = serde_json::Map::new();
    out.insert("model".into(), body.get("model").cloned().unwrap_or(Value::Null));

    let mut messages: Vec<Value> = vec![];
    // instructions → system 消息（Responses 用顶层 instructions 承载 system 语义）
    if let Some(instr) = body.get("instructions").and_then(|v| v.as_str()) {
        if !instr.is_empty() {
            messages.push(json!({ "role": "system", "content": instr }));
        }
    }
    // input：可能是纯字符串，或 item 数组（{type:"message",role,content:[{type:"input_text"|"output_text",text}]}）
    match body.get("input") {
        Some(Value::String(s)) => {
            messages.push(json!({ "role": "user", "content": s }));
        }
        Some(Value::Array(items)) => {
            for it in items {
                // function_call / function_call_output item → 对应 Chat 消息
                match it.get("type").and_then(|t| t.as_str()) {
                    Some("function_call") => {
                        let call_id = it.get("call_id").or_else(|| it.get("id")).cloned().unwrap_or(json!(""));
                        messages.push(json!({
                            "role": "assistant",
                            "content": Value::Null,
                            "tool_calls": [ {
                                "id": call_id,
                                "type": "function",
                                "function": {
                                    // 必须拼回全名（见 join_namespaced_tool_name）：历史里 Codex 把
                                    // MCP 工具存成 {name, namespace} 两字段，只取 name 会让模型下一轮
                                    // 照抄短名，回程拆不出 namespace → Codex 报 unsupported call。
                                    "name": join_namespaced_tool_name(it),
                                    "arguments": it.get("arguments").and_then(|a| a.as_str()).unwrap_or("{}"),
                                }
                            } ]
                        }));
                    }
                    Some("function_call_output") => {
                        messages.push(json!({
                            "role": "tool",
                            "tool_call_id": it.get("call_id").cloned().unwrap_or(json!("")),
                            "content": it.get("output").and_then(|o| o.as_str()).unwrap_or(""),
                        }));
                    }
                    // Codex 多轮会把上一轮的 custom 工具调用（apply_patch/exec）作为历史带回。
                    // custom_tool_call 携带裸字符串 `input`；还原成 assistant.tool_calls 时把 arguments
                    // 重新包成 {"input":"<裸串>"}，与响应侧解包（unpack_custom_tool_input）对称——
                    // 模型看到自己上一轮产出的同一形态。不处理会落到 `_` 分支被当成空 user 消息，
                    // 多轮里工具调用与结果全丢失，模型失去上下文。
                    Some("custom_tool_call") => {
                        let call_id = it
                            .get("call_id")
                            .or_else(|| it.get("id"))
                            .cloned()
                            .unwrap_or(json!(""));
                        let input_str = it.get("input").and_then(|i| i.as_str()).unwrap_or("");
                        let arguments = json!({ "input": input_str }).to_string();
                        messages.push(json!({
                            "role": "assistant",
                            "content": Value::Null,
                            "tool_calls": [ {
                                "id": call_id,
                                "type": "function",
                                "function": {
                                    // 同 function_call：带 namespace 时拼回全名，与请求侧暴露的工具名一致。
                                    // custom 工具（apply_patch/exec）通常平铺无 namespace，此时等价于取 name。
                                    "name": join_namespaced_tool_name(it),
                                    "arguments": arguments,
                                }
                            } ]
                        }));
                    }
                    // custom 工具执行结果回传：同 function_call_output → role:"tool"。
                    Some("custom_tool_call_output") => {
                        messages.push(json!({
                            "role": "tool",
                            "tool_call_id": it.get("call_id").cloned().unwrap_or(json!("")),
                            "content": it.get("output").and_then(|o| o.as_str()).unwrap_or(""),
                        }));
                    }
                    // 模型上一轮对延迟工具检索器的调用。`arguments` 在此 item 上是**对象**
                    // （`{"query":"…","limit":8}`），而 Chat 的 tool_calls.function.arguments 要求
                    // JSON **字符串**，故序列化后再放。不处理会落到 `_` 分支成空消息，模型看不到
                    // 自己检索过什么 → 反复用同义词重复检索（实测同一会话 5 次同义查询）。
                    Some(TOOL_SEARCH_CALL_ITEM) => {
                        let call_id = it
                            .get("call_id")
                            .or_else(|| it.get("id"))
                            .cloned()
                            .unwrap_or(json!(""));
                        let arguments = match it.get("arguments") {
                            Some(Value::String(s)) => s.clone(),
                            Some(v) => v.to_string(),
                            None => "{}".to_string(),
                        };
                        messages.push(json!({
                            "role": "assistant",
                            "content": Value::Null,
                            "tool_calls": [ {
                                "id": call_id,
                                "type": "function",
                                "function": { "name": TOOL_SEARCH_TYPE, "arguments": arguments }
                            } ]
                        }));
                    }
                    // Codex 客户端本地检索的结果。该 item **无 `output` 字段**，检索到的工具在
                    // `tools[]` 里（也是 MCP 真 schema 的唯一来源，已由 collect_declared_tools 提升
                    // 成真正的工具声明）。这里额外给模型一条 role:"tool" 回执，说明检索命中了什么——
                    // 否则模型发出调用却收不到结果，会认为检索失败而放弃、或重复检索。
                    // 只列名字不塞完整 schema：schema 已在 tools 里，重复塞会白烧大量 token。
                    Some(TOOL_SEARCH_OUTPUT_ITEM) => {
                        let mut found: Vec<String> = Vec::new();
                        if let Some(arr) = it.get("tools").and_then(|t| t.as_array()) {
                            for t in arr {
                                let ns = t.get("name").and_then(|n| n.as_str()).unwrap_or("");
                                match t.get("tools").and_then(|s| s.as_array()) {
                                    // namespace 折叠容器：列出展开后的全名（模型据此调用）
                                    Some(subs) => {
                                        for sub in subs {
                                            if let Some(sn) =
                                                sub.get("name").and_then(|n| n.as_str())
                                            {
                                                found.push(if ns.is_empty() {
                                                    sn.to_string()
                                                } else {
                                                    format!("{ns}__{sn}")
                                                });
                                            }
                                        }
                                    }
                                    None => {
                                        if let Some(n) = declared_tool_name(t) {
                                            found.push(n);
                                        }
                                    }
                                }
                            }
                        }
                        let content = if found.is_empty() {
                            "No matching tools found.".to_string()
                        } else {
                            format!(
                                "Matched tools now available for use: {}",
                                found.join(", ")
                            )
                        };
                        messages.push(json!({
                            "role": "tool",
                            "tool_call_id": it.get("call_id").cloned().unwrap_or(json!("")),
                            "content": content,
                        }));
                    }
                    // 普通 message item：role + content 分块
                    _ => {
                        // Codex 桌面端的工具声明项（{type:"additional_tools",role:"developer",tools:[…]}）
                        // 没有 content，落到这里会变成一条空 developer 消息（纯噪音）。它的 tools 由
                        // collect_declared_tools 单独提取转成真正的工具，故此处直接跳过。
                        if it.get("type").and_then(|t| t.as_str()) == Some(ADDITIONAL_TOOLS_ITEM) {
                            continue;
                        }
                        let role = it.get("role").and_then(|r| r.as_str()).unwrap_or("user");
                        let text = responses_content_text(it.get("content"));
                        messages.push(json!({ "role": role, "content": text }));
                    }
                }
            }
        }
        _ => {}
    }
    out.insert("messages".into(), json!(messages));

    // max_output_tokens → max_tokens
    if let Some(mt) = body.get("max_output_tokens").or_else(|| body.get("max_tokens")) {
        out.insert("max_tokens".into(), mt.clone());
    }
    copy_through(body, &mut out, &["temperature", "top_p", "stream"]);
    // reasoning（Codex 推理强度）透传到 Chat 中枢：中枢→上游那一跳按上游协议决定如何落地
    // （Anthropic 上游映射成 thinking.budget_tokens；原生 Responses 上游原样带回）。此前被丢弃
    // 导致「改了推理强度还走默认」。
    if let Some(r) = body.get("reasoning") {
        out.insert("reasoning".into(), r.clone());
    }
    // tools：Responses 扁平形态 → Chat 嵌套形态。经 collect_declared_tools 取工具，故顶层 `tools`、
    // Codex 桌面端的 `input[].additional_tools`、以及 `input[].tool_search_output` 三种承载都覆盖
    // （后两者分别是「桌面端工具全调不起来」与「MCP 工具永远调不起来」的根因）。
    // 含 Codex 的 namespace 折叠工具（{type:"namespace", name:"mcp__x", tools:[{function}]}）：
    // 必须把内部 tools[] 展开成一个个 `mcp__<ns>__<子工具>` 独立 function，否则只会得到一个
    // 无参数的 `mcp__x` 假工具，下游模型（如经 SynaRoute 路由的 Claude）拿不到真正的子工具、
    // 只能瞎调裸 `mcp__x` → Codex router 报 `unsupported call`。展开后的全名正是 Codex 期望的调用名。
    {
        let declared = collect_declared_tools(body);
        let mut mapped: Vec<Value> = Vec::new();
        for t in &declared {
            // Codex type:"custom" 工具（apply_patch 等）：schema 在驼峰 `inputSchema`（也兜底 parameters）。
            // 转成标准 Chat function，让上游模型拿到真实 schema。响应侧靠 collect_custom_tools
            // 集合把回程 item type 还原为 custom_tool_call。放在 namespace 判定之前，避免误入其他分支。
            if t.get("type").and_then(|ty| ty.as_str()) == Some("custom") {
                let Some(name) = t.get("name").and_then(|n| n.as_str()) else { continue };
                let mut f = serde_json::Map::new();
                f.insert("name".into(), json!(name));
                if let Some(d) = t.get("description") {
                    f.insert("description".into(), d.clone());
                }
                let schema = t
                    .get("inputSchema")
                    .or_else(|| t.get("parameters"))
                    .cloned()
                    .unwrap_or_else(freeform_custom_tool_schema);
                f.insert("parameters".into(), schema);
                mapped.push(json!({ "type": "function", "function": f }));
                continue;
            }
            // namespace 折叠工具：展开内部 tools[]，名字拼成 <ns>__<子工具>。
            if t.get("type").and_then(|ty| ty.as_str()) == Some("namespace") {
                let ns = t.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let empty_vec2 = vec![];
                let subs2 = t.get("tools").and_then(|s| s.as_array()).unwrap_or(&empty_vec2);
                for sub in subs2 {
                    let Some(sub_name) = sub.get("name").and_then(|n| n.as_str()) else { continue };
                    let full = if ns.is_empty() { sub_name.to_string() } else { format!("{ns}__{sub_name}") };
                    let mut f = serde_json::Map::new();
                    f.insert("name".into(), json!(full));
                    if let Some(d) = sub.get("description") {
                        f.insert("description".into(), d.clone());
                    }
                    if let Some(p) = sub.get("parameters").or_else(|| sub.get("inputSchema")) {
                        f.insert("parameters".into(), p.clone());
                    }
                    mapped.push(json!({ "type": "function", "function": f }));
                }
                continue;
            }
            // Responses function tool：{type:"function", name, description, parameters}
            // 也覆盖 `tool_search` 这类**无 name** 的 Codex 内置类型（名字取自 type，见
            // declared_tool_name）：此前一律 continue 跳过，模型不知道有检索器 → 发不出
            // tool_search_call → 延迟加载的 MCP 工具永远解锁不了。
            let Some(name) = declared_tool_name(t) else { continue };
            let mut f = serde_json::Map::new();
            f.insert("name".into(), json!(name));
            if let Some(d) = t.get("description") {
                f.insert("description".into(), d.clone());
            }
            if let Some(p) = t.get("parameters").or_else(|| t.get("inputSchema")) {
                f.insert("parameters".into(), p.clone());
            }
            mapped.push(json!({ "type": "function", "function": f }));
        }
        // 同名去重：`tool_search_output` 会在多轮里反复回灌同一批工具（每轮一份），
        // 不去重会让同一个 `mcp__synaroute__synaroute_ai` 在 tools 里出现 N 份，
        // 既白烧 token 又可能触发上游「重复工具名」校验失败。保留首次出现（顶层优先）。
        {
            let mut seen = std::collections::HashSet::new();
            mapped.retain(|t| {
                let name = t
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(|n| n.as_str())
                    .unwrap_or("")
                    .to_string();
                seen.insert(name)
            });
        }
        if !mapped.is_empty() {
            out.insert("tools".into(), json!(mapped));
        }
    }
    // tool_choice：Responses 扁平 {type:function,name} → Chat 嵌套 {type:function,function:{name}}；
    // 字符串档两协议同名，原样透传。丢掉它会让「强制调用某工具」降级成自由选择。
    if let Some(tc) = body.get("tool_choice") {
        let mapped = match tc {
            Value::Object(o) if o.get("type").and_then(|t| t.as_str()) == Some("function") => {
                let name = o
                    .get("name")
                    .or_else(|| o.get("function").and_then(|f| f.get("name")))
                    .cloned()
                    .unwrap_or(json!(""));
                json!({ "type": "function", "function": { "name": name } })
            }
            other => other.clone(),
        };
        out.insert("tool_choice".into(), mapped);
    }
    Value::Object(out)
}

/// freeform（无 JSON schema）custom 工具在 Chat/Anthropic 侧的兜底 schema。
///
/// Codex 桌面端的 `exec` 是 `{"type":"custom","format":{"type":"grammar",…}}`——载荷是**裸文本**
/// （一段 JS 源码），没有 `inputSchema`/`parameters`。若兜底成 `{"type":"object"}`（无 properties），
/// 上游模型拿到一个「没有任何入参」的工具，压根无处安放要执行的代码 → 要么不调、要么调了空参。
/// 故给出单字符串入参 `input`，与响应侧 [`unpack_custom_tool_input`] 的解包口径对称：
/// 模型回 `{"input":"<裸文本>"}` → 解包成裸串 → 作为 `custom_tool_call.input` 交还 Codex。
fn freeform_custom_tool_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "input": {
                "type": "string",
                "description": "The raw text payload for this tool, passed through verbatim."
            }
        },
        "required": ["input"]
    })
}

/// 抽取 Responses content 分块（input_text / output_text / text）为纯文本。
fn responses_content_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

/// Chat Completions 响应体 → Responses 响应体。
/// choices[0].message.content → output[].message；tool_calls → output[].function_call；
/// usage.{prompt,completion}_tokens → usage.{input,output}_tokens。
pub fn chat_resp_to_responses(body: &Value) -> Value {
    let choice0 = body
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|arr| arr.first());
    let message = choice0.and_then(|c| c.get("message"));
    let id = body
        .get("id")
        .and_then(|i| i.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("resp_{}", uuid_like()));
    let model = body.get("model").cloned().unwrap_or(Value::Null);

    let mut output: Vec<Value> = vec![];
    // 文本消息
    let text = message
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or("");
    if !text.is_empty() {
        output.push(json!({
            "type": "message",
            "id": format!("msg_{}", uuid_like()),
            "role": "assistant",
            "status": "completed",
            "content": [ { "type": "output_text", "text": text, "annotations": [] } ]
        }));
    }
    // 工具调用 → function_call item
    if let Some(tcs) = message.and_then(|m| m.get("tool_calls")).and_then(|t| t.as_array()) {
        for tc in tcs {
            let f = tc.get("function");
            output.push(json!({
                "type": "function_call",
                "id": format!("fc_{}", uuid_like()),
                "call_id": tc.get("id").cloned().unwrap_or(json!("")),
                "name": f.and_then(|f| f.get("name")).cloned().unwrap_or(json!("")),
                "arguments": f.and_then(|f| f.get("arguments")).and_then(|a| a.as_str()).unwrap_or("{}"),
                "status": "completed"
            }));
        }
    }

    let input_tokens = body
        .get("usage")
        .and_then(|u| u.get("prompt_tokens"))
        .and_then(|t| t.as_u64())
        .unwrap_or(0);
    let output_tokens = body
        .get("usage")
        .and_then(|u| u.get("completion_tokens"))
        .and_then(|t| t.as_u64())
        .unwrap_or(0);

    json!({
        "id": id,
        "object": "response",
        "created_at": chrono::Utc::now().timestamp(),
        "status": "completed",
        "model": model,
        "output": output,
        "usage": {
            "input_tokens": input_tokens,
            "output_tokens": output_tokens,
            "total_tokens": input_tokens + output_tokens
        }
    })
}

/// 同 [`chat_resp_to_responses`]，但按请求侧收集的两个集合改写回程 item type：
/// - `custom_tools` 命中 → `custom_tool_call`（Codex 的 apply_patch / exec 等 type:"custom" 工具）；
/// - `search_tools` 命中 → `tool_search_call`（Codex 的延迟工具检索器，客户端本地执行）。
///
/// 以基函数产出为底，仅对 output[] 里命中的工具调用项改写——避免复制整段逻辑。
/// 非流式路径专用；流式路径在 [`SseTranslator::emit_responses_completed`] 内直接判定。
pub fn chat_resp_to_responses_ext(
    body: &Value,
    custom_tools: &std::collections::HashSet<String>,
    search_tools: &std::collections::HashSet<String>,
) -> Value {
    let mut resp = chat_resp_to_responses(body);
    if custom_tools.is_empty() && search_tools.is_empty() {
        return resp;
    }
    if let Some(output) = resp.get_mut("output").and_then(|o| o.as_array_mut()) {
        for item in output.iter_mut() {
            if item.get("type").and_then(|t| t.as_str()) != Some("function_call") {
                continue;
            }
            let name = item
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_string();
            let Some(obj) = item.as_object_mut() else { continue };
            if search_tools.contains(&name) {
                rewrite_to_tool_search_call(obj);
            } else if custom_tools.contains(&name) {
                obj.insert("type".into(), json!("custom_tool_call"));
                // 同流式路径：custom_tool_call 用裸字符串 `input`，不用 JSON `arguments`。
                let args = obj
                    .get("arguments")
                    .and_then(|a| a.as_str())
                    .unwrap_or("")
                    .to_string();
                obj.insert("input".into(), json!(unpack_custom_tool_input(&args)));
                obj.remove("arguments");
            }
        }
    }
    resp
}

/// 把一个已构造好的 `function_call` item 原地改写成 Codex 的 `tool_search_call` 形态。
///
/// 与 `function_call` 的差异（对齐抓包实证，`~/.codex/logs_2.sqlite`）：
/// - `type` = `tool_search_call`，且 `id` 用 `tsc_` 前缀（Codex 自身产出即此前缀）；
/// - **无 `name` 字段**（工具身份由 type 表达，多一个 name 反而与 Codex 的反序列化结构不符）；
/// - `execution: "client"` —— 声明该调用由 Codex 客户端本地执行（BM25 检索），不回上游；
/// - `arguments` 是**对象**（`{"query":…,"limit":…}`），而非 function_call 的 JSON 字符串。
///   上游模型按 schema 回的是 JSON 字符串，此处解析回对象；解析失败则退化为 `{"query": 原文}`，
///   保证检索仍能带着模型意图跑起来（宁可查得糙，不要静默丢调用）。
fn rewrite_to_tool_search_call(obj: &mut serde_json::Map<String, Value>) {
    obj.insert("type".into(), json!(TOOL_SEARCH_CALL_ITEM));
    obj.insert("execution".into(), json!("client"));
    obj.remove("name");
    let raw = obj
        .get("arguments")
        .and_then(|a| a.as_str())
        .unwrap_or("")
        .to_string();
    let parsed = match serde_json::from_str::<Value>(raw.trim()) {
        Ok(v @ Value::Object(_)) => v,
        _ => json!({ "query": raw }),
    };
    obj.insert("arguments".into(), parsed);
    // id 用 Codex 同款前缀，避免其内部按前缀分派时认不出。
    let need_prefix = obj
        .get("id")
        .and_then(|i| i.as_str())
        .map(|s| !s.starts_with("tsc_"))
        .unwrap_or(true);
    if need_prefix {
        obj.insert("id".into(), json!(format!("tsc_{}", uuid_like())));
    }
}

/// Chat Completions 请求体 → Responses 请求体。
/// 映射：system 消息 → instructions；其余 messages → input item 数组
/// （assistant.tool_calls → function_call item；role:"tool" → function_call_output item；
/// 普通消息 → {type:message,role,content:[{type:input_text,text}]}）；
/// max_tokens → max_output_tokens；tools（Chat 嵌套）→ Responses 扁平。
pub fn chat_to_responses(body: &Value) -> Value {
    let mut out = serde_json::Map::new();
    out.insert("model".into(), body.get("model").cloned().unwrap_or(Value::Null));

    let mut instructions = String::new();
    let mut input: Vec<Value> = vec![];
    if let Some(arr) = body.get("messages").and_then(|m| m.as_array()) {
        for m in arr {
            let role = m.get("role").and_then(|r| r.as_str()).unwrap_or("user");
            match role {
                "system" => {
                    // 多条 system 累加（Responses 只有单一 instructions 槽）
                    let t = m.get("content").and_then(|c| c.as_str()).unwrap_or("");
                    if !t.is_empty() {
                        if !instructions.is_empty() {
                            instructions.push_str("\n\n");
                        }
                        instructions.push_str(t);
                    }
                }
                "assistant" if m.get("tool_calls").is_some() => {
                    if let Some(tcs) = m.get("tool_calls").and_then(|t| t.as_array()) {
                        for tc in tcs {
                            let f = tc.get("function");
                            input.push(json!({
                                "type": "function_call",
                                "call_id": tc.get("id").cloned().unwrap_or(json!("")),
                                "name": f.and_then(|f| f.get("name")).cloned().unwrap_or(json!("")),
                                "arguments": f.and_then(|f| f.get("arguments")).and_then(|a| a.as_str()).unwrap_or("{}"),
                            }));
                        }
                    }
                    // assistant 可能同时有文本
                    if let Some(t) = m.get("content").and_then(|c| c.as_str()) {
                        if !t.is_empty() {
                            input.push(json!({
                                "type": "message", "role": "assistant",
                                "content": [ { "type": "output_text", "text": t } ]
                            }));
                        }
                    }
                }
                "tool" => {
                    input.push(json!({
                        "type": "function_call_output",
                        "call_id": m.get("tool_call_id").cloned().unwrap_or(json!("")),
                        "output": m.get("content").and_then(|c| c.as_str()).unwrap_or(""),
                    }));
                }
                _ => {
                    // 普通 user/assistant 文本消息。assistant 用 output_text，其余 input_text。
                    let text = extract_text_content(m.get("content"));
                    let block_type = if role == "assistant" { "output_text" } else { "input_text" };
                    input.push(json!({
                        "type": "message", "role": role,
                        "content": [ { "type": block_type, "text": text } ]
                    }));
                }
            }
        }
    }
    if !instructions.is_empty() {
        out.insert("instructions".into(), json!(instructions));
    }
    out.insert("input".into(), json!(input));

    // max_tokens → max_output_tokens
    if let Some(mt) = body.get("max_tokens").or_else(|| body.get("max_completion_tokens")) {
        out.insert("max_output_tokens".into(), mt.clone());
    }
    copy_through(body, &mut out, &["temperature", "top_p", "stream"]);
    // reasoning：Chat 中枢里若带 reasoning（来自 Codex Responses 透传或 Anthropic thinking 反映射），
    // 原样带给 Responses 上游——它原生认 reasoning.effort，推理强度直达。
    if let Some(r) = body.get("reasoning") {
        out.insert("reasoning".into(), r.clone());
    }
    // tools：Chat 嵌套 {type:function,function:{name,..}} → Responses 扁平 {type:function,name,..}
    if let Some(tools) = body.get("tools").and_then(|t| t.as_array()) {
        let mapped: Vec<Value> = tools
            .iter()
            .filter_map(|t| {
                let f = t.get("function")?;
                let name = f.get("name").and_then(|n| n.as_str())?;
                let mut o = serde_json::Map::new();
                o.insert("type".into(), json!("function"));
                o.insert("name".into(), json!(name));
                if let Some(d) = f.get("description") {
                    o.insert("description".into(), d.clone());
                }
                if let Some(p) = f.get("parameters") {
                    o.insert("parameters".into(), p.clone());
                }
                Some(Value::Object(o))
            })
            .collect();
        if !mapped.is_empty() {
            out.insert("tools".into(), json!(mapped));
        }
    }
    // tool_choice：Chat 的 {type:function,function:{name}} → Responses 扁平 {type:function,name}；
    // 字符串档（auto/none/required）两协议同名，原样透传。丢掉它会让「强制调用某工具」降级成自由选择。
    if let Some(tc) = body.get("tool_choice") {
        let mapped = match tc {
            Value::Object(o) if o.get("type").and_then(|t| t.as_str()) == Some("function") => {
                let name = o
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .or_else(|| o.get("name"))
                    .cloned()
                    .unwrap_or(json!(""));
                json!({ "type": "function", "name": name })
            }
            other => other.clone(),
        };
        out.insert("tool_choice".into(), mapped);
    }
    Value::Object(out)
}

/// Responses 响应体 → Chat Completions 响应体。
/// output[] 里的 message（content[].output_text 文本）与 function_call 分别还原为
/// choices[0].message.content 与 tool_calls；usage.{input,output}_tokens → {prompt,completion}_tokens。
pub fn responses_resp_to_chat(body: &Value) -> Value {
    let id = body
        .get("id")
        .and_then(|i| i.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("chatcmpl-{}", uuid_like()));
    let model = body.get("model").cloned().unwrap_or(Value::Null);

    let mut text = String::new();
    let mut tool_calls: Vec<Value> = vec![];
    if let Some(output) = body.get("output").and_then(|o| o.as_array()) {
        for item in output {
            match item.get("type").and_then(|t| t.as_str()) {
                Some("message") => {
                    // content[].output_text 累加
                    if let Some(cs) = item.get("content").and_then(|c| c.as_array()) {
                        for c in cs {
                            if let Some(t) = c.get("text").and_then(|t| t.as_str()) {
                                text.push_str(t);
                            }
                        }
                    }
                }
                Some("function_call") => {
                    tool_calls.push(json!({
                        "id": item.get("call_id").or_else(|| item.get("id")).cloned().unwrap_or(json!("")),
                        "type": "function",
                        "function": {
                            // 同请求侧：带 namespace 时拼回全名，保证下游客户端看到的工具名
                            // 与工具声明一致（见 join_namespaced_tool_name）。
                            "name": join_namespaced_tool_name(item),
                            "arguments": item.get("arguments").and_then(|a| a.as_str()).unwrap_or("{}"),
                        }
                    }));
                }
                _ => {}
            }
        }
    }

    let mut message = serde_json::Map::new();
    message.insert("role".into(), json!("assistant"));
    message.insert("content".into(), if text.is_empty() { Value::Null } else { json!(text) });
    let finish = if !tool_calls.is_empty() {
        message.insert("tool_calls".into(), json!(tool_calls));
        "tool_calls"
    } else {
        "stop"
    };

    let input_tokens = body
        .get("usage")
        .and_then(|u| u.get("input_tokens"))
        .and_then(|t| t.as_u64())
        .unwrap_or(0);
    let output_tokens = body
        .get("usage")
        .and_then(|u| u.get("output_tokens"))
        .and_then(|t| t.as_u64())
        .unwrap_or(0);

    json!({
        "id": id,
        "object": "chat.completion",
        "created": chrono::Utc::now().timestamp(),
        "model": model,
        "choices": [ {
            "index": 0,
            "message": Value::Object(message),
            "finish_reason": finish
        } ],
        "usage": {
            "prompt_tokens": input_tokens,
            "completion_tokens": output_tokens,
            "total_tokens": input_tokens + output_tokens
        }
    })
}

// ---- 流式 SSE 跨协议翻译（Task #16）----
//
// Codex 下游默认 stream:true 且用 Responses 形态；多数第三方上游只支持 Chat。需把上游的
// Chat SSE 增量实时重组成 Responses 事件序列（反之亦然）。用「有状态、行缓冲」翻译器：
// 逐块喂入上游字节，按行切分缓冲不完整行，解析 `data: {json}`，产出下游协议的 SSE 文本。
//
// 能力边界（已在方案中说清）：Chat 上游只会产出「文本增量 / tool_call 增量 / finish / usage」，
// 故翻译器覆盖这几类并重组为对应 Responses 事件；Responses 独有的 reasoning_summary /
// image / code_interpreter 等事件因 Chat 源头无数据而不出现——这是能力上限，非遗漏。

/// SSE 流的跨协议翻译方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SseDirection {
    /// 上游 Chat SSE → 下游 Responses SSE（Codex 连 Chat-only 厂商，主场景）。
    ChatToResponses,
    /// 上游 Responses SSE → 下游 Chat SSE（下游 Chat 客户端连 Responses 上游）。
    ResponsesToChat,
    /// 上游 Chat SSE → 下游 Anthropic SSE。
    ChatToAnthropic,
    /// 上游 Anthropic SSE → 下游 Chat SSE。
    AnthropicToChat,
    /// 上游 Anthropic SSE → 下游 Responses SSE（Codex 连 Claude/Anthropic 上游，主诉求）。
    AnthropicToResponses,
    /// 上游 Responses SSE → 下游 Anthropic SSE（镜像方向，补全 3×3 矩阵）。
    ResponsesToAnthropic,
}

/// 根据下游协议(`downstream`)与上游 Key 协议(`upstream`)决定 SSE 翻译方向。
/// 同协议或暂不支持的组合返回 None（调用方走同协议直通或跳过）。
pub fn sse_direction(downstream: Protocol, upstream: Protocol) -> Option<SseDirection> {
    use Protocol::*;
    match (downstream, upstream) {
        (OpenaiResponses, OpenaiChat) => Some(SseDirection::ChatToResponses),
        (OpenaiChat, OpenaiResponses) => Some(SseDirection::ResponsesToChat),
        (Anthropic, OpenaiChat) => Some(SseDirection::ChatToAnthropic),
        (OpenaiChat, Anthropic) => Some(SseDirection::AnthropicToChat),
        (OpenaiResponses, Anthropic) => Some(SseDirection::AnthropicToResponses),
        (Anthropic, OpenaiResponses) => Some(SseDirection::ResponsesToAnthropic),
        _ => None,
    }
}

/// 有状态 SSE 翻译器：喂入上游字节块，产出下游协议的 SSE 文本块。
/// 内部按行缓冲——上游一个 chunk 可能切在半行中间，累积到 `\n` 才处理整行。
pub struct SseTranslator {
    dir: SseDirection,
    /// 未处理完的上游字节。**缓冲字节而非字符串**：多字节字符可能被 TCP 分段切开，
    /// 逐块解码会把它腐蚀成 U+FFFD（详见 push 的注释）。
    buf: Vec<u8>,
    /// 目标形态里输出消息/响应的 id（首次需要时惰性生成）。
    resp_id: String,
    msg_id: String,
    /// 是否已发出「起始」事件（Responses 的 response.created / output_item.added）。
    started: bool,
    /// 累计文本增量长度（用于 Responses 的 output_text.done 是否需补发）。
    saw_text: bool,
    /// 累计 usage（Chat 流末尾的 usage chunk → Responses response.completed）。
    model: String,
    /// tool_call 增量按 index 累积 (id, name, arguments)。
    tool_calls: Vec<(String, String, String)>,
    /// Anthropic 上游的 usage 分散在 message_start（input_tokens）与 message_delta
    /// （output_tokens），需累积后在收尾（Responses response.completed）时统一归位。
    input_tokens: u64,
    output_tokens: u64,
    /// Anthropic content block 的 index → tool_calls 槽位下标映射（tool_use 块与 text 块
    /// 共用同一 index 空间，需按 content_block_start 记下 tool_use 块落在哪个槽位）。
    block_tool_slot: std::collections::HashMap<usize, usize>,
    /// 累积 assistant 文本全文。Codex 靠 `response.output_item.done`（带完整 message item）
    /// 把 assistant 回复持久化进会话；仅发 output_text.delta（实时增量）只够即时显示、
    /// 重开会话就丢。故这里累积全文，在收尾时回填进 message 的 output_item.done。
    text_accum: String,
    /// Anthropic thinking 块的 content block index 集合（type=thinking / redacted_thinking）。
    /// 用于把 thinking_delta 与普通 text_delta 区分开——thinking 增量要转成 Codex 的
    /// reasoning_summary 事件（让 Codex 显示 Claude 的思考过程），而非 output_text。
    thinking_blocks: std::collections::HashSet<usize>,
    /// 是否已发出 reasoning summary 的起始事件（part.added）。Codex 的 ReasoningSummaryDelta
    /// 需先有 part.added 起头；用此标志保证只发一次。
    reasoning_started: bool,
    /// 累积 thinking 全文，供收尾时发 reasoning_summary_text.done（带完整文本）。
    reasoning_accum: String,
    /// 请求里 Codex namespace 折叠工具的 namespace 名列表（如 `mcp__synaroute`），按长度降序。
    /// 收尾生成 Responses function_call 时，用它把上游回调的全名 `<ns>__<sub>` 拆回
    /// {name, namespace} 两字段（Codex router 用结构化 ToolName 查表，不拆 name 字符串）。
    tool_namespaces: Vec<String>,
    /// 请求里 type:"custom" 工具名集合（apply_patch 等）。
    /// 响应侧据此把对应工具调用的 item type 输出为 "custom_tool_call" 而非 "function_call"。
    custom_tools: std::collections::HashSet<String>,
    /// 请求里 type:"tool_search" 客户端检索工具名集合（当前即 `tool_search`）。
    /// 响应侧据此把对应调用输出为 `tool_search_call`（Codex 本地执行 BM25 检索），
    /// 否则 Codex 认不出、延迟加载的 MCP 工具永远解锁不了。
    search_tools: std::collections::HashSet<String>,
    /// Anthropic 下游方向的 content block 游标（下一个可用 index）。Anthropic 要求同一条
    /// 消息内 block index 唯一且**递增出现**，故 text 块与 tool_use 块共用这一个游标。
    anthropic_next_block: usize,
    /// Anthropic 下游方向：当前打开着的 text 块 index（None = 无打开的 text 块）。
    /// 发 tool_use 块前必须先 stop 它；收尾时也要兜底 stop。
    anthropic_text_open: Option<usize>,
    /// Anthropic 下游方向：已登记的工具调用去重键（call_id，缺失时退化为 name+arguments）。
    /// `response.output_item.done` 与 `response.completed.output[]` 可能重复携带同一个调用，
    /// 靠它保证同一个工具调用只翻成一个 tool_use 块。
    anthropic_tool_seen: std::collections::HashSet<String>,
}

impl SseTranslator {
    #[allow(dead_code)] // 仅测试与非 Codex 场景用；Codex 流式走 with_namespaces。
    pub fn new(dir: SseDirection) -> Self {
        Self::with_namespaces(dir, Vec::new())
    }

    /// 带 namespace 列表构造：Codex（Responses 下游）跨协议流式时传入请求里的 namespace 名，
    /// 使响应侧能把折叠工具的全名拆回 {name, namespace}。其余场景用 [`SseTranslator::new`] 即可。
    pub fn with_namespaces(dir: SseDirection, tool_namespaces: Vec<String>) -> Self {
        Self {
            dir,
            buf: Vec::new(),
            resp_id: format!("resp_{}", uuid_like()),
            msg_id: format!("msg_{}", uuid_like()),
            started: false,
            saw_text: false,
            model: String::new(),
            tool_calls: Vec::new(),
            input_tokens: 0,
            output_tokens: 0,
            block_tool_slot: std::collections::HashMap::new(),
            text_accum: String::new(),
            thinking_blocks: std::collections::HashSet::new(),
            reasoning_started: false,
            reasoning_accum: String::new(),
            tool_namespaces,
            custom_tools: std::collections::HashSet::new(),
            search_tools: std::collections::HashSet::new(),
            anthropic_next_block: 0,
            anthropic_text_open: None,
            anthropic_tool_seen: std::collections::HashSet::new(),
        }
    }

    /// 在 [`with_namespaces`] 基础上带上两个工具集合：
    /// - `custom_tools`（Codex 的 apply_patch / exec 等 type:"custom"）→ 回程发 `custom_tool_call`；
    /// - `search_tools`（Codex 的 `tool_search` 延迟检索器）→ 回程发 `tool_search_call`。
    ///
    /// Codex（Responses 下游）跨协议流式时传入。不改写的话 Codex router 认不出这两类调用：
    /// custom 工具执行失败；检索发不起来 → MCP 工具（`mcp__*`）永远拿不到 schema。
    pub fn with_namespaces_and_custom(
        dir: SseDirection,
        tool_namespaces: Vec<String>,
        custom_tools: std::collections::HashSet<String>,
        search_tools: std::collections::HashSet<String>,
    ) -> Self {
        let mut s = Self::with_namespaces(dir, tool_namespaces);
        s.custom_tools = custom_tools;
        s.search_tools = search_tools;
        s
    }

    /// 喂入一块上游字节，返回应发给下游的 SSE 文本（可能为空）。
    ///
    /// **缓冲的是字节而不是字符串**，且只对**完整行**解码 —— 这一点是必须的，不是风格问题。
    /// 原先写的是 `buf.push_str(&String::from_utf8_lossy(chunk))`：对每一块入参各自解码。
    /// 而上游是流式的，一个 3 字节的中文字符完全可能被 TCP 分段切开、分两次 push 进来，
    /// 逐块解码时前后两半各自都是非法 UTF-8、各自被替换成 U+FFFD，
    /// 于是用户看到的回答里凭空出现「」。
    ///
    /// 按完整行解码则安全：SSE 协议保证上游按行发 JSON，行内必然是完整的 UTF-8 序列。
    /// 回归测试见 `sse_multibyte_text_survives_arbitrary_chunk_boundaries`。
    pub fn push(&mut self, chunk: &[u8]) -> String {
        self.buf.extend_from_slice(chunk);
        let mut out = String::new();
        // 逐个完整行处理，保留最后不完整的一段在 buf。
        while let Some(nl) = self.buf.iter().position(|&b| b == b'\n') {
            // raw 是独立的 owned Vec，text 借的是 raw 而非 self，
            // 故下面调 `self.process_line(&mut self, ..)` 不会撞借用检查。
            let raw: Vec<u8> = self.buf.drain(..=nl).collect();
            let text = String::from_utf8_lossy(&raw);
            let line = text.trim_end_matches(['\r', '\n']);
            if let Some(ev) = self.process_line(line) {
                out.push_str(&ev);
            }
        }
        out
    }

    /// 流结束时冲刷收尾事件（Responses 需要 response.completed；Chat 需 [DONE]）。
    pub fn finish(&mut self) -> String {
        match self.dir {
            SseDirection::ChatToResponses | SseDirection::AnthropicToResponses => {
                self.emit_responses_completed(None)
            }
            SseDirection::ChatToAnthropic | SseDirection::ResponsesToAnthropic => {
                self.emit_anthropic_stop()
            }
            SseDirection::ResponsesToChat | SseDirection::AnthropicToChat => {
                "data: [DONE]\n\n".to_string()
            }
        }
    }

    fn process_line(&mut self, line: &str) -> Option<String> {
        let data = line.strip_prefix("data:")?.trim();
        if data.is_empty() {
            return None;
        }
        if data == "[DONE]" {
            // Chat 上游结束标记：对 Responses 方向由 finish() 统一收尾，这里吞掉。
            return match self.dir {
                SseDirection::ResponsesToChat | SseDirection::AnthropicToChat => {
                    Some("data: [DONE]\n\n".to_string())
                }
                _ => None,
            };
        }
        let json: Value = serde_json::from_str(data).ok()?;
        match self.dir {
            SseDirection::ChatToResponses => Some(self.chat_chunk_to_responses(&json)),
            SseDirection::ResponsesToChat => Some(self.responses_event_to_chat(&json)),
            SseDirection::ChatToAnthropic => Some(self.chat_chunk_to_anthropic(&json)),
            SseDirection::AnthropicToChat => Some(self.anthropic_event_to_chat(&json)),
            SseDirection::AnthropicToResponses => Some(self.anthropic_chunk_to_responses(&json)),
            SseDirection::ResponsesToAnthropic => Some(self.responses_event_to_anthropic(&json)),
        }
    }

    /// 一个 Chat SSE chunk → Responses 事件序列文本。
    fn chat_chunk_to_responses(&mut self, chunk: &Value) -> String {
        let mut out = String::new();
        if self.model.is_empty() {
            if let Some(m) = chunk.get("model").and_then(|m| m.as_str()) {
                self.model = m.to_string();
            }
        }
        // 起始事件：response.created + output_item.added（message）
        if !self.started {
            self.started = true;
            let created = json!({
                "type": "response.created",
                "response": { "id": self.resp_id, "object": "response", "status": "in_progress", "model": self.model }
            });
            out.push_str(&sse("response.created", &created));
            let item_added = json!({
                "type": "response.output_item.added",
                "output_index": 0,
                "item": { "id": self.msg_id, "type": "message", "role": "assistant", "status": "in_progress", "content": [] }
            });
            out.push_str(&sse("response.output_item.added", &item_added));
        }
        let choice0 = chunk.get("choices").and_then(|c| c.as_array()).and_then(|a| a.first());
        let delta = choice0.and_then(|c| c.get("delta"));
        // 文本增量
        if let Some(t) = delta.and_then(|d| d.get("content")).and_then(|c| c.as_str()) {
            if !t.is_empty() {
                self.saw_text = true;
                self.text_accum.push_str(t); // 累积全文，收尾时回填 output_item.done 供 Codex 落盘
                let ev = json!({
                    "type": "response.output_text.delta",
                    "item_id": self.msg_id, "output_index": 0, "content_index": 0, "delta": t
                });
                out.push_str(&sse("response.output_text.delta", &ev));
            }
        }
        // tool_call 增量：按 index 累积 name/arguments
        if let Some(tcs) = delta.and_then(|d| d.get("tool_calls")).and_then(|t| t.as_array()) {
            for tc in tcs {
                let idx = tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                while self.tool_calls.len() <= idx {
                    self.tool_calls.push((String::new(), String::new(), String::new()));
                }
                if let Some(id) = tc.get("id").and_then(|i| i.as_str()) {
                    if !id.is_empty() { self.tool_calls[idx].0 = id.to_string(); }
                }
                if let Some(f) = tc.get("function") {
                    if let Some(n) = f.get("name").and_then(|n| n.as_str()) {
                        if !n.is_empty() { self.tool_calls[idx].1 = n.to_string(); }
                    }
                    if let Some(a) = f.get("arguments").and_then(|a| a.as_str()) {
                        self.tool_calls[idx].2.push_str(a);
                    }
                }
            }
        }
        // finish_reason + usage：Chat 末尾 chunk。usage 常在最后一个（stream_options）chunk。
        let finished = choice0.and_then(|c| c.get("finish_reason")).and_then(|r| r.as_str()).is_some();
        if finished && self.saw_text {
            // 文本 done：带完整全文（此前空串，Codex 落盘需要正文）。
            let done = json!({
                "type": "response.output_text.done",
                "item_id": self.msg_id, "output_index": 0, "content_index": 0, "text": self.text_accum
            });
            out.push_str(&sse("response.output_text.done", &done));
            // 关键修复：文本 message 也发 output_item.done（带完整 text）——Codex 靠该事件把
            // assistant 回复持久化进会话；此前只发 delta（实时流），重开对话文本回复全丢。
            out.push_str(&self.emit_text_item_done());
        }
        // usage 单独出现（无 choices 或 choices 空）时，触发 completed
        if chunk.get("usage").is_some() && chunk.get("usage") != Some(&Value::Null) {
            out.push_str(&self.emit_responses_completed(chunk.get("usage")));
        }
        out
    }

    /// 发文本 message 的 output_item.done（带累积全文）。Codex 靠此事件把 assistant 文本回复
    /// 持久化进会话；此前只发 output_text.delta（实时流），重开对话文本回复全丢（工具调用因已
    /// 发 output_item.done 而正常保存）。text/message item 固定占 output_index 0。
    fn emit_text_item_done(&self) -> String {
        let item = json!({
            "type": "message",
            "id": self.msg_id,
            "role": "assistant",
            "status": "completed",
            "content": [ { "type": "output_text", "text": self.text_accum, "annotations": [] } ]
        });
        sse("response.output_item.done", &json!({
            "type": "response.output_item.done",
            "output_index": 0,
            "item": item
        }))
    }

    /// 冲刷 Responses 收尾：先补 function_call item（若有），再 response.completed。
    fn emit_responses_completed(&mut self, usage: Option<&Value>) -> String {
        let mut out = String::new();
        // 已发出过 completed 则不重复（用 started 兼作幂等：completed 后置 false）。
        if !self.started {
            return out;
        }
        self.started = false;
        // Codex 的 Responses SSE 解析器（codex-api/src/sse/responses.rs）只在
        // `response.output_item.done` 事件里把 item 反序列化为 ResponseItem::FunctionCall
        // 并执行工具；`response.completed` 段只读 id/usage/end_turn，**完全忽略 output[]**。
        // 故每个累积的工具调用都必须作为独立的 output_item.added + output_item.done 事件流式
        // 投递（此前只塞进 completed.output → Codex 收不到工具调用、卡死等待，纯文本却正常）。
        // 字段严格对齐 Codex 的 ResponseItem::FunctionCall：name / arguments（JSON 字符串）/
        // call_id（必填，用上游 tool_use/tool_call 的 id；缺失则兜底生成，保证工具结果可回配）。
        let mut output: Vec<Value> = vec![];
        // output_index：文本消息占 0（若有），工具调用依次往后排。
        let tool_base = if self.saw_text { 1u64 } else { 0 };
        if self.saw_text {
            output.push(json!({
                "type": "message", "id": self.msg_id, "role": "assistant", "status": "completed",
                "content": [ { "type": "output_text", "text": self.text_accum, "annotations": [] } ]
            }));
        }
        for (i, (id, name, args)) in self.tool_calls.iter().enumerate() {
            let output_index = tool_base + i as u64;
            let fc_id = if id.is_empty() { format!("fc_{}", uuid_like()) } else { id.clone() };
            // call_id 必填且用于回配工具结果：上游给了 id 就用它，没有则退回生成的 fc_id。
            let call_id = if id.is_empty() { fc_id.clone() } else { id.clone() };
            // arguments 必须是可解析的 JSON 字符串：无参工具（args 为空）兜底成 "{}"，
            // 否则 Codex 侧 serde_json 解析空串失败、工具无法执行。
            let arguments = if args.trim().is_empty() { "{}" } else { args.as_str() };
            // 关键：Codex router 用结构化 {namespace, name} 查工具注册表，不拆 name 字符串。
            // 上游模型按展开的全名 `mcp__x__foo` 回调，这里必须拆回 name="foo" + namespace="mcp__x"
            // 两个独立字段，否则 router 查 {namespace:None, name:"mcp__x__foo"} 匹配不到 → unsupported call。
            let (ns, real_name) = split_namespaced_tool_name(name, &self.tool_namespaces);
            let mut item_map = serde_json::Map::new();
            // Codex 的 type:"custom" 工具（apply_patch 等）期望 item type 为 custom_tool_call；
            // 其余（含 namespace 展开的 MCP 工具、普通 function）用 function_call。用请求侧收集的
            // custom_tools 集合按拆出的真实名判定，否则 Codex router 认不出 custom 工具 → 执行失败。
            let item_type = if self.custom_tools.contains(&real_name) {
                "custom_tool_call"
            } else {
                "function_call"
            };
            item_map.insert("type".into(), json!(item_type));
            item_map.insert("id".into(), json!(fc_id));
            item_map.insert("call_id".into(), json!(call_id));
            item_map.insert("name".into(), json!(real_name));
            if let Some(ns) = ns {
                item_map.insert("namespace".into(), json!(ns));
            }
            // custom_tool_call 的 payload 是裸字符串 `input`（apply_patch 的 patch 正文、exec 的命令），
            // 而非 function_call 的 JSON `arguments`。否则 Codex 反序列化 custom_tool_call 拿不到 input
            // → 工具空跑或被拒（type 改对了但字段名不对，等于没修）。从 {"input":"..."} 解包成裸串。
            if item_type == "custom_tool_call" {
                item_map.insert("input".into(), json!(unpack_custom_tool_input(arguments)));
            } else {
                item_map.insert("arguments".into(), json!(arguments));
            }
            item_map.insert("status".into(), json!("completed"));
            // `tool_search` 调用要改写成 Codex 的 `tool_search_call`（客户端本地执行 BM25 检索）。
            // 放在最后统一改写，复用上面已填好的 id/call_id/status，只调整 type/arguments/execution
            // 并去掉 name —— 与非流式路径 [`chat_resp_to_responses_ext`] 同一个函数，口径不分叉。
            if self.search_tools.contains(&real_name) {
                rewrite_to_tool_search_call(&mut item_map);
            }
            let item = Value::Object(item_map);
            // 关键修复：作为流式事件投递（added 宣告 → done 交付完整调用），Codex 据 done 执行工具。
            out.push_str(&sse("response.output_item.added", &json!({
                "type": "response.output_item.added",
                "output_index": output_index,
                "item": item.clone()
            })));
            out.push_str(&sse("response.output_item.done", &json!({
                "type": "response.output_item.done",
                "output_index": output_index,
                "item": item.clone()
            })));
            output.push(item);
        }
        // usage 来源二选一：Chat 上游末尾 chunk 传入 usage（prompt/completion_tokens）；
        // Anthropic 上游无末尾 usage chunk，token 在流中分散累积到字段，此处兜底取字段值。
        let (it, ot) = match usage {
            Some(u) => (
                u.get("prompt_tokens").and_then(|t| t.as_u64()).unwrap_or(self.input_tokens),
                u.get("completion_tokens").and_then(|t| t.as_u64()).unwrap_or(self.output_tokens),
            ),
            None => (self.input_tokens, self.output_tokens),
        };
        let completed = json!({
            "type": "response.completed",
            "response": {
                "id": self.resp_id, "object": "response", "status": "completed", "model": self.model,
                "output": output,
                "usage": { "input_tokens": it, "output_tokens": ot, "total_tokens": it + ot }
            }
        });
        out.push_str(&sse("response.completed", &completed));
        out
    }

    /// 一个 Responses SSE 事件 → Chat SSE chunk 文本。
    ///
    /// 覆盖文本增量与**工具调用**（Responses `function_call` item → Chat `delta.tool_calls`）。
    /// 工具必须翻译，理由同其余方向：丢掉工具调用，下游 Chat 客户端只见纯文本、永不执行工具。
    fn responses_event_to_chat(&mut self, ev: &Value) -> String {
        let ty = ev.get("type").and_then(|t| t.as_str()).unwrap_or("");
        match ty {
            "response.output_text.delta" => {
                let delta = ev.get("delta").and_then(|d| d.as_str()).unwrap_or("");
                let chunk = json!({
                    "object": "chat.completion.chunk",
                    "choices": [ { "index": 0, "delta": { "content": delta }, "finish_reason": Value::Null } ]
                });
                sse_data(&chunk)
            }
            // 工具调用：Responses 在 output_item.done 时给出完整 item（arguments 已齐），
            // 一次性翻成 Chat 的单片 tool_calls 增量（id+name+完整 arguments）。
            "response.output_item.done" => ev
                .get("item")
                .map(|item| self.chat_tool_call_chunk_from_item(item))
                .unwrap_or_default(),
            "response.completed" => {
                // 兜底：部分上游只在 completed.output[] 给工具调用；靠 chat_tool_seen 去重。
                let mut out = String::new();
                if let Some(items) = ev
                    .get("response")
                    .and_then(|r| r.get("output"))
                    .and_then(|o| o.as_array())
                {
                    let items = items.clone();
                    for item in &items {
                        out.push_str(&self.chat_tool_call_chunk_from_item(item));
                    }
                }
                let finish = if self.tool_calls.is_empty() { "stop" } else { "tool_calls" };
                out.push_str(&sse_data(&json!({
                    "object": "chat.completion.chunk",
                    "choices": [ { "index": 0, "delta": {}, "finish_reason": finish } ]
                })));
                // usage 收尾 chunk。**此前这条方向从不发 usage**：`self.input_tokens` /
                // `output_tokens` 在本方向被写入却永不读出，下游拿不到任何 token 数字。
                // 与其它四个方向的能力漂移（各自都发 usage），修掉它。
                //
                // Chat Completions 的约定是：最后一个 chunk 带 `usage`、`choices` 为空数组
                // （OpenAI `stream_options.include_usage` 的形状）。取 Responses 事件里的
                // usage，取不到时回退到流中累积的字段值。
                let (it, ot) = ev
                    .get("response")
                    .and_then(|r| r.get("usage"))
                    .map(|u| {
                        (
                            u.get("input_tokens").and_then(|t| t.as_u64()).unwrap_or(self.input_tokens),
                            u.get("output_tokens").and_then(|t| t.as_u64()).unwrap_or(self.output_tokens),
                        )
                    })
                    .unwrap_or((self.input_tokens, self.output_tokens));
                if it > 0 || ot > 0 {
                    out.push_str(&sse_data(&json!({
                        "object": "chat.completion.chunk",
                        "choices": [],
                        "usage": {
                            "prompt_tokens": it,
                            "completion_tokens": ot,
                            "total_tokens": it + ot
                        }
                    })));
                }
                out
            }
            _ => String::new(),
        }
    }

    /// 把一个 Responses 输出 item 翻成 Chat 的一片 `delta.tool_calls` 增量（非工具 item 返回空串）。
    /// 用 `anthropic_tool_seen`（本方向复用同一去重集合）保证 output_item.done 与
    /// completed.output[] 重复携带时只发一次。
    fn chat_tool_call_chunk_from_item(&mut self, item: &Value) -> String {
        let ity = item.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if !matches!(ity, "function_call" | "custom_tool_call" | "tool_search_call") {
            return String::new();
        }
        let name = join_namespaced_tool_name(item);
        if name.is_empty() {
            return String::new();
        }
        let arguments = if ity == "custom_tool_call" {
            let input = item.get("input").and_then(|i| i.as_str()).unwrap_or("");
            json!({ "input": input }).to_string()
        } else {
            match item.get("arguments") {
                Some(Value::String(s)) => s.clone(),
                Some(v) => v.to_string(),
                None => String::new(),
            }
        };
        let call_id = item
            .get("call_id")
            .or_else(|| item.get("id"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let dedup_key = if call_id.is_empty() {
            format!("{name}\u{0}{arguments}")
        } else {
            call_id.clone()
        };
        if !self.anthropic_tool_seen.insert(dedup_key) {
            return String::new();
        }
        let slot = self.tool_calls.len();
        let id = if call_id.is_empty() {
            format!("call_{}", uuid_like())
        } else {
            call_id
        };
        let args = if arguments.trim().is_empty() { "{}".to_string() } else { arguments };
        self.tool_calls.push((id.clone(), name.clone(), args.clone()));
        sse_data(&json!({
            "object": "chat.completion.chunk",
            "choices": [ { "index": 0, "finish_reason": Value::Null, "delta": {
                "tool_calls": [ {
                    "index": slot,
                    "id": id,
                    "type": "function",
                    "function": { "name": name, "arguments": args }
                } ]
            } } ]
        }))
    }

    /// 一个 Chat SSE chunk → Anthropic SSE 事件文本。
    ///
    /// 覆盖文本增量与**工具调用**。工具必须翻译，理由同 [`SseTranslator::responses_event_to_anthropic`]：
    /// Anthropic 下游（Claude CLI / 桌面端）转到 Chat 上游时，模型的 `delta.tool_calls` 增量若被丢弃，
    /// 下游只见纯文本 → 工具永远不被调用。Chat 的 tool_calls 是**分片增量**（name 与 arguments 逐块到达），
    /// 故先按 index 累积到 `tool_calls`，在 finish_reason 到达时才成块发出。
    fn chat_chunk_to_anthropic(&mut self, chunk: &Value) -> String {
        let mut out = String::new();
        if self.model.is_empty() {
            if let Some(m) = chunk.get("model").and_then(|m| m.as_str()) {
                self.model = m.to_string();
            }
        }
        let choice0 = chunk.get("choices").and_then(|c| c.as_array()).and_then(|a| a.first());
        let delta = choice0.and_then(|c| c.get("delta"));
        out.push_str(&self.ensure_anthropic_started());
        if let Some(t) = delta.and_then(|d| d.get("content")).and_then(|c| c.as_str()) {
            if !t.is_empty() {
                let idx = self.ensure_anthropic_text_block(&mut out);
                self.saw_text = true;
                out.push_str(&sse("content_block_delta", &json!({
                    "type": "content_block_delta", "index": idx,
                    "delta": { "type": "text_delta", "text": t }
                })));
            }
        }
        // tool_call 增量：按 index 累积 (id, name, arguments)，与 chat_chunk_to_responses 同构。
        if let Some(tcs) = delta.and_then(|d| d.get("tool_calls")).and_then(|t| t.as_array()) {
            for tc in tcs {
                let idx = tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                while self.tool_calls.len() <= idx {
                    self.tool_calls.push((String::new(), String::new(), String::new()));
                }
                if let Some(id) = tc.get("id").and_then(|i| i.as_str()) {
                    if !id.is_empty() {
                        self.tool_calls[idx].0 = id.to_string();
                    }
                }
                if let Some(f) = tc.get("function") {
                    if let Some(n) = f.get("name").and_then(|n| n.as_str()) {
                        if !n.is_empty() {
                            self.tool_calls[idx].1 = n.to_string();
                        }
                    }
                    if let Some(a) = f.get("arguments").and_then(|a| a.as_str()) {
                        self.tool_calls[idx].2.push_str(a);
                    }
                }
            }
        }
        // finish_reason 到达 → tool_calls 已完整，成块发出（收尾事件由 finish()/message_stop 负责）。
        if choice0.and_then(|c| c.get("finish_reason")).and_then(|r| r.as_str()).is_some() {
            out.push_str(&self.flush_anthropic_tool_calls());
        }
        // usage（Chat 末尾 chunk，需 stream_options）→ 收尾时由 message_delta 带给 Anthropic 下游。
        if let Some(u) = chunk.get("usage").filter(|u| !u.is_null()) {
            if let Some(pt) = u.get("prompt_tokens").and_then(|t| t.as_u64()) {
                self.input_tokens = pt;
            }
            if let Some(ct) = u.get("completion_tokens").and_then(|t| t.as_u64()) {
                self.output_tokens = ct;
            }
        }
        out
    }

    /// 把累积的 Chat 风格 `tool_calls` 一次性翻成 Anthropic `tool_use` 块序列。
    /// 复用 [`SseTranslator::emit_anthropic_tool_block`]（同一套去重/命名/兜底口径），
    /// 故重复调用安全（第二次全部命中去重、返回空串）。
    fn flush_anthropic_tool_calls(&mut self) -> String {
        if self.tool_calls.is_empty() {
            return String::new();
        }
        let pending: Vec<(String, String, String)> = self.tool_calls.clone();
        let mut out = String::new();
        for (id, name, args) in pending {
            if name.is_empty() {
                continue;
            }
            out.push_str(&self.emit_anthropic_tool_block(&json!({
                "type": "function_call",
                "call_id": id,
                "name": name,
                "arguments": args,
            })));
        }
        out
    }

    /// Anthropic 流收尾：收 text 块 + 补发工具块 + message_delta(stop_reason) + message_stop。
    ///
    /// `stop_reason` 必须按是否有工具调用区分：发了 tool_use 块却报 `end_turn`，
    /// 下游客户端（Claude 桌面端 / CLI）会认为「本轮已结束」而不执行工具。
    fn emit_anthropic_stop(&mut self) -> String {
        if !self.started {
            return String::new();
        }
        // 上游没给 finish_reason 就断流时，累积的 tool_calls 还没成块，这里兜底冲刷。
        let mut out = self.flush_anthropic_tool_calls();
        self.started = false;
        out.push_str(&self.close_anthropic_text_block());
        let stop_reason = if self.anthropic_tool_seen.is_empty() {
            "end_turn"
        } else {
            "tool_use"
        };
        out.push_str(&sse("message_delta", &json!({
            "type": "message_delta",
            "delta": { "stop_reason": stop_reason },
            "usage": { "output_tokens": self.output_tokens }
        })));
        out.push_str(&sse("message_stop", &json!({ "type": "message_stop" })));
        out
    }

    /// 一个 Anthropic SSE 事件 → Chat SSE chunk 文本。
    ///
    /// 覆盖文本增量与**工具调用**（Anthropic `tool_use` 块 → Chat `delta.tool_calls` 增量）。
    /// 工具必须翻译，理由同另外两个 →Anthropic/→Chat 方向：丢掉工具调用会让下游客户端
    /// 只见纯文本、以为模型没打算用工具。复用 `block_tool_slot`（block index → tool_calls 槽位）
    /// 与 `tool_calls` 累积，与 [`SseTranslator::anthropic_chunk_to_responses`] 同构。
    fn anthropic_event_to_chat(&mut self, ev: &Value) -> String {
        let ty = ev.get("type").and_then(|t| t.as_str()).unwrap_or("");
        match ty {
            // Anthropic 的 token 用量分两处给：message_start 带 input_tokens，
            // message_delta 带累计的 output_tokens。**此前本方向完全不处理这两个事件**，
            // 于是 self.input_tokens / output_tokens 永不被写入、也永不发给下游——
            // 下游拿不到任何 token 数字（其它四个方向都发 usage，这是能力漂移）。
            "message_start" => {
                if let Some(it) = ev
                    .get("message")
                    .and_then(|m| m.get("usage"))
                    .and_then(|u| u.get("input_tokens"))
                    .and_then(|t| t.as_u64())
                {
                    self.input_tokens = it;
                }
                String::new()
            }
            "message_delta" => {
                if let Some(ot) = ev
                    .get("usage")
                    .and_then(|u| u.get("output_tokens"))
                    .and_then(|t| t.as_u64())
                {
                    self.output_tokens = ot;
                }
                String::new()
            }
            "content_block_start" => {
                let idx = ev.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                let block = ev.get("content_block");
                if block.and_then(|b| b.get("type")).and_then(|t| t.as_str()) == Some("tool_use") {
                    let id = block
                        .and_then(|b| b.get("id"))
                        .and_then(|i| i.as_str())
                        .unwrap_or("")
                        .to_string();
                    let name = block
                        .and_then(|b| b.get("name"))
                        .and_then(|n| n.as_str())
                        .unwrap_or("")
                        .to_string();
                    let slot = self.tool_calls.len();
                    self.tool_calls.push((id.clone(), name.clone(), String::new()));
                    self.block_tool_slot.insert(idx, slot);
                    // Chat 的 tool_calls 增量：首片带 id/name（arguments 随后逐片补）。
                    let chunk = json!({
                        "object": "chat.completion.chunk",
                        "choices": [ { "index": 0, "finish_reason": Value::Null, "delta": {
                            "tool_calls": [ {
                                "index": slot,
                                "id": id,
                                "type": "function",
                                "function": { "name": name, "arguments": "" }
                            } ]
                        } } ]
                    });
                    return sse_data(&chunk);
                }
                String::new()
            }
            "content_block_delta" => {
                let idx = ev.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                let delta = ev.get("delta");
                let dty = delta.and_then(|d| d.get("type")).and_then(|t| t.as_str()).unwrap_or("");
                // tool_use 块的参数增量 → Chat 的 function.arguments 分片。
                if dty == "input_json_delta" {
                    let Some(&slot) = self.block_tool_slot.get(&idx) else {
                        return String::new();
                    };
                    let pj = delta
                        .and_then(|d| d.get("partial_json"))
                        .and_then(|p| p.as_str())
                        .unwrap_or("");
                    if pj.is_empty() {
                        return String::new();
                    }
                    self.tool_calls[slot].2.push_str(pj);
                    let chunk = json!({
                        "object": "chat.completion.chunk",
                        "choices": [ { "index": 0, "finish_reason": Value::Null, "delta": {
                            "tool_calls": [ {
                                "index": slot,
                                "type": "function",
                                "function": { "arguments": pj }
                            } ]
                        } } ]
                    });
                    return sse_data(&chunk);
                }
                // thinking_delta 不属于对话正文，不当作 content 泄漏给 Chat 下游。
                if dty == "thinking_delta" {
                    return String::new();
                }
                let t = delta.and_then(|d| d.get("text")).and_then(|t| t.as_str()).unwrap_or("");
                if t.is_empty() {
                    return String::new();
                }
                let chunk = json!({
                    "object": "chat.completion.chunk",
                    "choices": [ { "index": 0, "delta": { "content": t }, "finish_reason": Value::Null } ]
                });
                sse_data(&chunk)
            }
            "message_stop" => {
                // 有工具调用时 finish_reason 必须是 tool_calls：报 stop 会让下游客户端
                // 认定本轮结束而不执行工具（与 →Anthropic 方向的 stop_reason 同一道理）。
                let finish = if self.tool_calls.is_empty() { "stop" } else { "tool_calls" };
                let mut out = sse_data(&json!({
                    "object": "chat.completion.chunk",
                    "choices": [ { "index": 0, "delta": {}, "finish_reason": finish }]
                }));
                // usage 收尾 chunk（Chat Completions 约定：末片带 usage、choices 为空数组）。
                if self.input_tokens > 0 || self.output_tokens > 0 {
                    out.push_str(&sse_data(&json!({
                        "object": "chat.completion.chunk",
                        "choices": [],
                        "usage": {
                            "prompt_tokens": self.input_tokens,
                            "completion_tokens": self.output_tokens,
                            "total_tokens": self.input_tokens + self.output_tokens
                        }
                    })));
                }
                out
            }
            _ => String::new(),
        }
    }

    /// 一个 Anthropic SSE 事件 → Responses 事件序列文本（Codex 连 Claude 上游，主诉求）。
    /// 覆盖 Codex 重度使用的 function calling：Anthropic 的 tool_use 块 + input_json_delta
    /// 增量 → Responses function_call output item。用 `block_tool_slot` 把 Anthropic 的
    /// content block index 映射到 `tool_calls` 槽位（text 块与 tool_use 块共用同一 index 空间）。
    fn anthropic_chunk_to_responses(&mut self, ev: &Value) -> String {
        let mut out = String::new();
        let ty = ev.get("type").and_then(|t| t.as_str()).unwrap_or("");
        match ty {
            // message_start：捕获 model 与 input_tokens，发起始事件（response.created + output_item.added）
            "message_start" => {
                let msg = ev.get("message");
                if self.model.is_empty() {
                    if let Some(m) = msg.and_then(|m| m.get("model")).and_then(|m| m.as_str()) {
                        self.model = m.to_string();
                    }
                }
                if let Some(it) = msg
                    .and_then(|m| m.get("usage"))
                    .and_then(|u| u.get("input_tokens"))
                    .and_then(|t| t.as_u64())
                {
                    self.input_tokens = it;
                }
                if !self.started {
                    self.started = true;
                    let created = json!({
                        "type": "response.created",
                        "response": { "id": self.resp_id, "object": "response", "status": "in_progress", "model": self.model }
                    });
                    out.push_str(&sse("response.created", &created));
                    let item_added = json!({
                        "type": "response.output_item.added",
                        "output_index": 0,
                        "item": { "id": self.msg_id, "type": "message", "role": "assistant", "status": "in_progress", "content": [] }
                    });
                    out.push_str(&sse("response.output_item.added", &item_added));
                }
            }
            // content_block_start：tool_use 块 → 记 tool_call 槽位 (id, name)；
            // thinking / redacted_thinking 块 → 记入 thinking_blocks，其增量转 reasoning summary；
            // text 块无需动作。
            "content_block_start" => {
                let idx = ev.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                let block = ev.get("content_block");
                match block.and_then(|b| b.get("type")).and_then(|t| t.as_str()) {
                    Some("tool_use") => {
                        let id = block
                            .and_then(|b| b.get("id"))
                            .and_then(|i| i.as_str())
                            .unwrap_or("")
                            .to_string();
                        let name = block
                            .and_then(|b| b.get("name"))
                            .and_then(|n| n.as_str())
                            .unwrap_or("")
                            .to_string();
                        let slot = self.tool_calls.len();
                        self.tool_calls.push((id, name, String::new()));
                        self.block_tool_slot.insert(idx, slot);
                    }
                    // 扩展思考块（Claude thinking / redacted_thinking）：Codex(Responses) 支持显示
                    // 推理摘要，故把它翻成 reasoning_summary 事件。首个 thinking 块发一次 part.added 起头。
                    Some("thinking") | Some("redacted_thinking") => {
                        self.thinking_blocks.insert(idx);
                        if !self.reasoning_started {
                            self.reasoning_started = true;
                            out.push_str(&sse(
                                "response.reasoning_summary_part.added",
                                &json!({
                                    "type": "response.reasoning_summary_part.added",
                                    "item_id": self.msg_id, "summary_index": 0
                                }),
                            ));
                        }
                    }
                    _ => {}
                }
            }
            // content_block_delta：text_delta → output_text.delta；input_json_delta → 累加工具参数
            "content_block_delta" => {
                let idx = ev.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                let delta = ev.get("delta");
                let dty = delta.and_then(|d| d.get("type")).and_then(|t| t.as_str()).unwrap_or("");
                match dty {
                    "text_delta" => {
                        if let Some(t) = delta.and_then(|d| d.get("text")).and_then(|t| t.as_str()) {
                            if !t.is_empty() {
                                self.saw_text = true;
                                self.text_accum.push_str(t); // 累积全文，供收尾 message output_item.done 落盘
                                let e = json!({
                                    "type": "response.output_text.delta",
                                    "item_id": self.msg_id, "output_index": 0, "content_index": 0, "delta": t
                                });
                                out.push_str(&sse("response.output_text.delta", &e));
                            }
                        }
                    }
                    "input_json_delta" => {
                        if let Some(pj) = delta.and_then(|d| d.get("partial_json")).and_then(|p| p.as_str()) {
                            if let Some(&slot) = self.block_tool_slot.get(&idx) {
                                self.tool_calls[slot].2.push_str(pj);
                            }
                        }
                    }
                    // thinking_delta：Claude 扩展思考的增量 → Codex reasoning_summary 增量事件，
                    // 让 Codex 显示思考过程。仅当该 index 是 thinking 块时才转（与普通文本区分）。
                    "thinking_delta" if self.thinking_blocks.contains(&idx) => {
                        if let Some(t) = delta.and_then(|d| d.get("thinking")).and_then(|t| t.as_str()) {
                            if !t.is_empty() {
                                self.reasoning_accum.push_str(t);
                                out.push_str(&sse(
                                    "response.reasoning_summary_text.delta",
                                    &json!({
                                        "type": "response.reasoning_summary_text.delta",
                                        "item_id": self.msg_id, "summary_index": 0, "delta": t
                                    }),
                                ));
                            }
                        }
                    }
                    _ => {}
                }
            }
            // message_delta：捕获 output_tokens（收尾 usage 归位）
            "message_delta" => {
                if let Some(ot) = ev
                    .get("usage")
                    .and_then(|u| u.get("output_tokens"))
                    .and_then(|t| t.as_u64())
                {
                    self.output_tokens = ot;
                }
            }
            // message_stop：发文本 done + message 的 output_item.done（关键：Codex 靠此落盘
            // assistant 文本回复，仅发 delta 不落盘会导致重开对话文本丢失）+ response.completed。
            "message_stop" => {
                // 思考摘要收尾：发 reasoning_summary_text.done（带累积全文），让 Codex 完成
                // 该 summary 段。放在文本 done 之前（推理先于回答）。
                if self.reasoning_started {
                    out.push_str(&sse(
                        "response.reasoning_summary_text.done",
                        &json!({
                            "type": "response.reasoning_summary_text.done",
                            "item_id": self.msg_id, "summary_index": 0,
                            "text": self.reasoning_accum
                        }),
                    ));
                }
                if self.saw_text {
                    let done = json!({
                        "type": "response.output_text.done",
                        "item_id": self.msg_id, "output_index": 0, "content_index": 0,
                        "text": self.text_accum
                    });
                    out.push_str(&sse("response.output_text.done", &done));
                    out.push_str(&self.emit_text_item_done());
                }
                out.push_str(&self.emit_responses_completed(None));
            }
            _ => {}
        }
        out
    }

    /// 一个 Responses SSE 事件 → Anthropic SSE 事件文本。
    ///
    /// 覆盖文本增量、**工具调用**与收尾。工具调用必须翻译（2026-07-30 实机根因）：
    /// Claude 桌面端（Anthropic 下游）故障转移到 Responses 上游时，上游模型对 MCP 工具的调用
    /// 走 `response.output_item.added/.done` 的 `function_call` item 投递；早期实现只认
    /// `output_text.delta` / `completed`，其余落进 `_ => {}` 被静默丢弃 → 桌面端只收到纯文本 +
    /// end_turn，表现为「模型从不调用 synaroute_ai」，MCP 侧永远等不到 tools/call。
    fn responses_event_to_anthropic(&mut self, ev: &Value) -> String {
        let ty = ev.get("type").and_then(|t| t.as_str()).unwrap_or("");
        let mut out = String::new();
        match ty {
            "response.created" => {
                if self.model.is_empty() {
                    if let Some(m) = ev
                        .get("response")
                        .and_then(|r| r.get("model"))
                        .and_then(|m| m.as_str())
                    {
                        self.model = m.to_string();
                    }
                }
            }
            "response.output_text.delta" => {
                out.push_str(&self.ensure_anthropic_started());
                let delta = ev.get("delta").and_then(|d| d.as_str()).unwrap_or("");
                if !delta.is_empty() {
                    let idx = self.ensure_anthropic_text_block(&mut out);
                    self.saw_text = true;
                    out.push_str(&sse("content_block_delta", &json!({
                        "type": "content_block_delta", "index": idx,
                        "delta": { "type": "text_delta", "text": delta }
                    })));
                }
            }
            // 工具调用：Responses 用独立 output item 承载。`added` 时 item 尚无完整 arguments
            // （上游可能后续用 function_call_arguments.delta 补），故只在 `done` 落块——彼时
            // arguments 已完整，Anthropic 侧可一次性发出 start + input_json_delta + stop。
            "response.output_item.done" => {
                if let Some(item) = ev.get("item") {
                    out.push_str(&self.emit_anthropic_tool_block(item));
                }
            }
            "response.completed" => {
                // usage 归位：Responses 的 usage 在 completed 里，Anthropic 由 message_delta 承载。
                if let Some(u) = ev.get("response").and_then(|r| r.get("usage")) {
                    if let Some(it) = u.get("input_tokens").and_then(|t| t.as_u64()) {
                        self.input_tokens = it;
                    }
                    if let Some(ot) = u.get("output_tokens").and_then(|t| t.as_u64()) {
                        self.output_tokens = ot;
                    }
                }
                // 兜底：部分上游只在 completed.output[] 里给工具调用，不发独立 output_item.done
                // （或事件被中间层裁剪）。这里补扫一遍，靠 anthropic_tool_seen 去重。
                if let Some(items) = ev
                    .get("response")
                    .and_then(|r| r.get("output"))
                    .and_then(|o| o.as_array())
                {
                    let items = items.clone();
                    for item in &items {
                        out.push_str(&self.emit_anthropic_tool_block(item));
                    }
                }
                out.push_str(&self.emit_anthropic_stop());
            }
            _ => {}
        }
        out
    }

    /// 确保 Anthropic 下游流已发 `message_start`（幂等）。
    /// 文本与工具调用都可能是流里第一个内容，故两处都先调它。
    fn ensure_anthropic_started(&mut self) -> String {
        if self.started {
            return String::new();
        }
        self.started = true;
        sse("message_start", &json!({
            "type": "message_start",
            "message": { "id": self.resp_id, "type": "message", "role": "assistant", "content": [],
                "model": self.model, "usage": { "input_tokens": 0, "output_tokens": 0 } }
        }))
    }

    /// 确保有一个打开着的 text 块，返回其 index。没有则新开一个（占用游标）。
    fn ensure_anthropic_text_block(&mut self, out: &mut String) -> usize {
        if let Some(idx) = self.anthropic_text_open {
            return idx;
        }
        let idx = self.anthropic_next_block;
        self.anthropic_next_block += 1;
        self.anthropic_text_open = Some(idx);
        out.push_str(&sse("content_block_start", &json!({
            "type": "content_block_start", "index": idx,
            "content_block": { "type": "text", "text": "" }
        })));
        idx
    }

    /// 关闭当前打开的 text 块（若有）。发 tool_use 块前与收尾时调用。
    fn close_anthropic_text_block(&mut self) -> String {
        match self.anthropic_text_open.take() {
            Some(idx) => sse(
                "content_block_stop",
                &json!({ "type": "content_block_stop", "index": idx }),
            ),
            None => String::new(),
        }
    }

    /// 把一个 Responses 输出 item 翻成 Anthropic 的 `tool_use` 内容块（非工具 item 返回空串）。
    ///
    /// 产出完整三段：`content_block_start`（带 id/name）+ `content_block_delta`
    /// （`input_json_delta` 承载 arguments JSON）+ `content_block_stop`。同时记住
    /// `stop_reason` 要改成 `tool_use`（见 [`SseTranslator::emit_anthropic_stop`]）。
    ///
    /// 工具名还原：Responses item 可能把名字拆成 `{name, namespace}` 两字段（Codex 范式），
    /// 而 Anthropic 下游客户端认的是**全名**，故用 [`join_namespaced_tool_name`] 拼回。
    fn emit_anthropic_tool_block(&mut self, item: &Value) -> String {
        let ity = item.get("type").and_then(|t| t.as_str()).unwrap_or("");
        // function_call / custom_tool_call / tool_search_call 都是「模型要调工具」，
        // 对 Anthropic 下游一律呈现为标准 tool_use 块。
        if !matches!(ity, "function_call" | "custom_tool_call" | "tool_search_call") {
            return String::new();
        }
        let name = join_namespaced_tool_name(item);
        // custom_tool_call 的 payload 是裸字符串 `input`；包回 {"input": "..."} 使其成为
        // 合法的 tool_use.input 对象（Anthropic 要求 input 是 JSON 对象）。
        let raw_args = if ity == "custom_tool_call" {
            let input = item.get("input").and_then(|i| i.as_str()).unwrap_or("");
            json!({ "input": input }).to_string()
        } else {
            match item.get("arguments") {
                Some(Value::String(s)) => s.clone(),
                // tool_search_call 的 arguments 是**对象**（非 JSON 字符串），原样序列化。
                Some(v) => v.to_string(),
                None => String::new(),
            }
        };
        let call_id = item
            .get("call_id")
            .or_else(|| item.get("id"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        // 去重键：有 call_id 用它（上游保证唯一），否则退化为 name+arguments。
        let dedup_key = if call_id.is_empty() {
            format!("{name}\u{0}{raw_args}")
        } else {
            call_id.clone()
        };
        if !self.anthropic_tool_seen.insert(dedup_key) {
            return String::new();
        }
        let tool_id = if call_id.is_empty() {
            format!("toolu_{}", uuid_like())
        } else {
            call_id
        };
        let mut out = self.ensure_anthropic_started();
        // tool_use 块不能嵌在打开的 text 块里，先收尾 text。
        out.push_str(&self.close_anthropic_text_block());
        let idx = self.anthropic_next_block;
        self.anthropic_next_block += 1;
        out.push_str(&sse("content_block_start", &json!({
            "type": "content_block_start", "index": idx,
            "content_block": { "type": "tool_use", "id": tool_id, "name": name, "input": {} }
        })));
        // 无参工具兜底成 "{}"：Anthropic 客户端会 JSON.parse 累积的 partial_json，空串会炸。
        let args = if raw_args.trim().is_empty() { "{}" } else { raw_args.as_str() };
        out.push_str(&sse("content_block_delta", &json!({
            "type": "content_block_delta", "index": idx,
            "delta": { "type": "input_json_delta", "partial_json": args }
        })));
        out.push_str(&sse(
            "content_block_stop",
            &json!({ "type": "content_block_stop", "index": idx }),
        ));
        out
    }
}

/// 构造带 `event:` 行的 SSE（Responses/Anthropic 事件流用具名事件）。
fn sse(event: &str, data: &Value) -> String {
    format!("event: {event}\ndata: {}\n\n", serde_json::to_string(data).unwrap_or_default())
}

/// 构造仅 `data:` 的 SSE（Chat 流不使用具名事件）。
fn sse_data(data: &Value) -> String {
    format!("data: {}\n\n", serde_json::to_string(data).unwrap_or_default())
}



#[cfg(test)]
mod sse_golden;

#[cfg(test)]
mod tests {
    use super::*;

    /// P2-2：`Protocol` 的能力方法必须对**每个变体**都有明确取值，不许靠 `_ =>` 兜底。
    ///
    /// 这条是「加第 4 种协议时的安全网」：遍历全变体逐项断言，若将来有人在能力方法里加了
    /// `_ =>` 兜底臂（那会让新协议被静默按某一族处理，向上游发错误的头 → 401 或
    /// `client_restricted` 403，排查方向被误导到「Key 配错了」），这条测试不会直接报错，
    /// 但配合下面「每个变体的取值都被显式列出」的断言，至少能保证现有三个变体的取值不被
    /// 无意改动。真正的编译期保障来自能力方法里的穷举 match 本身。
    #[test]
    fn protocol_capabilities_cover_all_variants() {
        use crate::model::AuthScheme;
        // 全变体清单：加变体后这里会因为 ALL 长度断言失败而被发现
        let all = [Protocol::Anthropic, Protocol::OpenaiChat, Protocol::OpenaiResponses];
        assert_eq!(all.len(), 3, "新增 Protocol 变体后，请逐项确认下面各能力方法的取值");

        // 鉴权形态
        assert_eq!(Protocol::Anthropic.auth_scheme(), AuthScheme::XApiKey);
        assert_eq!(Protocol::OpenaiChat.auth_scheme(), AuthScheme::Bearer);
        assert_eq!(Protocol::OpenaiResponses.auth_scheme(), AuthScheme::Bearer);

        // 头名与取值形态
        assert_eq!(AuthScheme::XApiKey.header_name(), "x-api-key");
        assert_eq!(AuthScheme::XApiKey.header_value("sk-1"), "sk-1");
        assert_eq!(AuthScheme::Bearer.header_name(), "authorization");
        assert_eq!(AuthScheme::Bearer.header_value("sk-1"), "Bearer sk-1");

        // 版本头：只有 Anthropic 需要，且取值必须逐字保持（改了会让真 Anthropic API 返 400）
        assert_eq!(
            Protocol::Anthropic.version_header(),
            Some(("anthropic-version", "2023-06-01"))
        );
        assert_eq!(Protocol::OpenaiChat.version_header(), None);
        assert_eq!(Protocol::OpenaiResponses.version_header(), None);

        // 1M 上下文 beta 是 Anthropic 特有
        assert!(Protocol::Anthropic.supports_1m_beta());
        assert!(!Protocol::OpenaiChat.supports_1m_beta());
        assert!(!Protocol::OpenaiResponses.supports_1m_beta());
    }

    /// P2-5：`convert_request_owned` 同协议时必须**零拷贝原样返回**，跨协议时与
    /// `convert_request` 结果完全一致。
    ///
    /// 「原样」是关键语义：同协议路径是最常见场景（Claude Code→Anthropic Key、
    /// Codex→Responses Key），此时请求体应逐字节透传，任何隐式改写都会让
    /// count_tokens 等子路径行为偏离。
    #[test]
    fn convert_request_owned_passes_through_and_matches_borrowed() {
        let body = json!({
            "model": "m",
            "max_tokens": 10,
            "messages": [ { "role": "user", "content": "hi" } ],
            // 放一个转换器不认识的字段：同协议必须原样保留
            "some_vendor_ext": { "a": [1, 2, 3] }
        });

        // 同协议：逐字节等价（零拷贝移动的结果就是原对象本身）
        for p in [Protocol::Anthropic, Protocol::OpenaiChat, Protocol::OpenaiResponses] {
            let out = convert_request_owned(body.clone(), p, p);
            assert_eq!(out, body, "同协议必须原样透传（{p:?}）");
        }

        // 跨协议：与按引用版本结果一致（不能因为换了入口就走出不同结果）
        let pairs = [
            (Protocol::Anthropic, Protocol::OpenaiChat),
            (Protocol::Anthropic, Protocol::OpenaiResponses),
            (Protocol::OpenaiChat, Protocol::Anthropic),
            (Protocol::OpenaiChat, Protocol::OpenaiResponses),
            (Protocol::OpenaiResponses, Protocol::Anthropic),
            (Protocol::OpenaiResponses, Protocol::OpenaiChat),
        ];
        for (from, to) in pairs {
            assert_eq!(
                convert_request_owned(body.clone(), from, to),
                convert_request(&body, from, to),
                "{from:?}→{to:?} 两个入口结果必须一致"
            );
        }
    }

    /// P3-1：两个 `→Chat` 方向必须发 usage 收尾 chunk。
    ///
    /// 这是一处**能力漂移**：另外四个方向都发 usage，只有 `anthropic_event_to_chat` 与
    /// `responses_event_to_chat` 不发——它们的 `self.input_tokens/output_tokens` 被写入
    /// （或压根没被写入）却永不读出，下游拿不到任何 token 数字，用户无法核对额度消耗。
    /// 成因是流式转换是「第二套矩阵」（6 个手写有向方法），改一处不会强制改另一处。
    ///
    /// 故障注入判据：删掉任一方向的 usage chunk，对应断言立刻变红。
    #[test]
    fn both_to_chat_directions_emit_usage() {
        // ---- Anthropic → Chat ----
        let mut t = SseTranslator::new(SseDirection::AnthropicToChat);
        // Anthropic 分两处给用量：message_start 带 input，message_delta 带累计 output
        let out = t.push(
            b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"model\":\"claude\",\"usage\":{\"input_tokens\":120}}}\n\n",
        );
        assert!(!out.contains("usage"), "message_start 阶段不该提前发 usage");
        t.push(b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\n");
        t.push(b"event: message_delta\ndata: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":34}}\n\n");
        let tail = t.push(b"event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n");
        assert!(
            tail.contains("\"prompt_tokens\":120"),
            "Anthropic→Chat 必须把 input_tokens 作为 prompt_tokens 发出: {tail}"
        );
        assert!(
            tail.contains("\"completion_tokens\":34"),
            "Anthropic→Chat 必须把 output_tokens 作为 completion_tokens 发出: {tail}"
        );
        assert!(tail.contains("\"total_tokens\":154"), "总数应为 120+34: {tail}");

        // ---- Responses → Chat ----
        let mut t2 = SseTranslator::new(SseDirection::ResponsesToChat);
        t2.push(b"event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"hi\"}\n\n");
        let tail2 = t2.push(
            b"event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":7,\"output_tokens\":9}}}\n\n",
        );
        assert!(
            tail2.contains("\"prompt_tokens\":7") && tail2.contains("\"completion_tokens\":9"),
            "Responses→Chat 必须发 usage: {tail2}"
        );
        assert!(tail2.contains("\"total_tokens\":16"), "总数应为 7+9: {tail2}");
        // Chat Completions 的约定：带 usage 的末片 choices 为空数组
        assert!(
            tail2.contains("\"choices\":[]"),
            "usage chunk 的 choices 应为空数组（OpenAI include_usage 形状）: {tail2}"
        );

        // 无用量时不硬造 0（如实陈述「上游没给」，与 extract_usage 返回 None 同一原则）
        let mut t3 = SseTranslator::new(SseDirection::ResponsesToChat);
        let tail3 = t3.push(
            b"event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{}}\n\n",
        );
        assert!(
            !tail3.contains("usage"),
            "上游没给用量时不应发 usage chunk（不写 0 冒充）: {tail3}"
        );
    }



    /// 构造一个跑了若干轮工具的会话历史（assistant tool_use ↔ user tool_result 成对）。
    /// `sizes` 是每轮 tool_result 正文的字符数。
    ///
    /// 每轮正文带**唯一标记** `<<ROUND_i>>`：断言必须能精确区分「哪一轮还在、哪一轮被压了」。
    /// 若都用同一个填充字符，长正文会包含短正文（`"x".repeat(20000)` 里必然含
    /// `"x".repeat(6000)`），「较早轮次已被压掉」这条就永远判不出来 —— 实测踩过。
    fn session_with_tool_rounds(protocol: Protocol, sizes: &[usize]) -> ToolSession {
        let mut s = ToolSession::new(protocol, &MultimodalPrompt::from_text("开始"));
        for (i, &n) in sizes.iter().enumerate() {
            let id = format!("call_{i}");
            // assistant 那条：形状按协议区分（trim 只读 role/tool_result，assistant 内容不重要）
            let assistant = if protocol.is_openai() {
                json!({ "role": "assistant", "content": null,
                        "tool_calls": [{ "id": &id, "type": "function",
                                         "function": { "name": "read_file", "arguments": "{}" } }] })
            } else {
                json!({ "role": "assistant",
                        "content": [{ "type": "tool_use", "id": &id, "name": "read_file", "input": {} }] })
            };
            s.messages.push(assistant);
            s.push_tool_results(&[ToolResultMsg {
                id: id.clone(),
                content: round_body(i, n),
                is_error: false,
            }]);
        }
        s
    }

    /// 第 i 轮的正文：唯一标记 + 填充到 n 字符。
    fn round_body(i: usize, n: usize) -> String {
        let marker = format!("<<ROUND_{i}>>");
        let pad = n.saturating_sub(marker.chars().count());
        format!("{marker}{}", "x".repeat(pad))
    }

    fn round_marker(i: usize) -> String {
        format!("<<ROUND_{i}>>")
    }

    /// 裁剪必须**保留 tool_use/tool_result 的一一对应**（删任何一条上游 400），
    /// 只把较早轮次的正文换成占位。两协议都验。




    #[test]
    fn trim_tool_history_preserves_pairing_and_keeps_latest() {
        for proto in [Protocol::Anthropic, Protocol::OpenaiChat] {
            // 5 轮，各 5000 字符 = 25000；预算 8000 → 应压掉最早几轮
            let mut s = session_with_tool_rounds(proto, &[5000, 5000, 5000, 5000, 5000]);
            let before_msgs = s.messages.len();
            let squashed = s.trim_tool_history(8000);

            assert!(squashed >= 3, "{proto:?}: 25000 压到 8000 至少要动 3 轮，实际 {squashed}");
            // 消息条数一条都不能少（配对不破）
            assert_eq!(s.messages.len(), before_msgs, "{proto:?}: 裁剪不得删除任何消息");
            // 总量已落回预算内
            assert!(s.tool_result_chars() <= 8000, "{proto:?}: 裁剪后仍超预算");
            let joined: String = s.messages.iter().map(|m| m.to_string()).collect();
            // 最近一轮（第 4 轮）必须原样保留 —— 那是模型正要用的材料
            assert!(
                joined.contains(&round_marker(4)),
                "{proto:?}: 最近一轮结果不该被压缩"
            );
            // 最早一轮必须已被压掉（标记随正文一起消失）
            assert!(
                !joined.contains(&round_marker(0)),
                "{proto:?}: 最早一轮应被压成占位"
            );
            // 占位说明必须非空且提示可重新获取（空串会诱使模型重复调用）
            assert!(joined.contains("已省略") && joined.contains("重新调用"), "{proto:?}: 占位文案缺失");
        }
    }

    /// 预算够大时不裁剪；小于阈值的结果也不值得压。
    #[test]
    fn trim_tool_history_noop_when_under_budget() {
        let mut s = session_with_tool_rounds(Protocol::Anthropic, &[3000, 3000]);
        assert_eq!(s.trim_tool_history(60000), 0, "总量 6000 < 预算，不该动");
        // 全是小结果（各 100 字符 < TRIM_PLACEHOLDER_MIN=200），即便超预算也压不动
        let mut s2 = session_with_tool_rounds(Protocol::Anthropic, &[100, 100, 100, 100, 100]);
        assert_eq!(s2.trim_tool_history(50), 0, "小于阈值的结果不压（占位比原文还长）");
    }

    /// **最新一轮永不压缩**，即便它自己就超预算。
    ///
    /// 这条比 preserves 那条更能锁住豁免逻辑：最后一轮 20000 > 预算 8000，若不豁免就会被压成占位。
    /// 去掉 `last_tool_idx` 豁免（改成 None）时，这条立刻变红。
    #[test]
    fn trim_tool_history_never_squashes_the_latest_even_if_it_alone_exceeds_budget() {
        for proto in [Protocol::Anthropic, Protocol::OpenaiChat] {
            let mut s = session_with_tool_rounds(proto, &[6000, 6000, 20000]);
            s.trim_tool_history(8000);
            let joined: String = s.messages.iter().map(|m| m.to_string()).collect();
            assert!(
                joined.contains(&round_marker(2)),
                "{proto:?}: 最新一轮（20000）必须原样保留，即便它自己就超预算"
            );
            // 较早两轮应已被压（用唯一标记判定，不受长短包含关系干扰）
            assert!(
                !joined.contains(&round_marker(0)) && !joined.contains(&round_marker(1)),
                "{proto:?}: 较早轮次应被压成占位"
            );
        }
    }

    /// 真机反例（2026-08-02）：中转商把请求挡在 HTML 页上，用户只看到
    /// `expected value at line 1 column 1`，完全不知道该改什么。
    /// 提示必须说清「拿到的是网页」并指向 Base URL。
    #[test]
    fn non_json_models_response_says_it_got_a_webpage_not_serde_jargon() {
        let url = "https://nimabo.cn/v1/models";
        for html in [
            "<!DOCTYPE html><html><head><title>登录</title></head></html>",
            "<html><body>Just a moment...</body></html>",
            "  \n<!doctype HTML>\n<html>",
        ] {
            let msg = non_json_models_hint(url, html);
            assert!(msg.contains("网页"), "应说明拿到的是网页，实际：{msg}");
            assert!(msg.contains("Base URL"), "应指向 Base URL，实际：{msg}");
            assert!(msg.contains(url), "应带上具体端点，实际：{msg}");
            // 反例护栏：不得再把 serde 的行列术语抛给用户
            assert!(
                !msg.contains("expected value") && !msg.contains("column"),
                "不该出现 serde 术语，实际：{msg}"
            );
        }
    }

    /// 非 HTML 的垃圾响应：报出开头片段，让用户自己判断拿到了什么；
    /// 空响应单独说，避免出现「开头是「」」这种空洞提示。
    #[test]
    fn non_json_models_response_quotes_head_and_handles_empty() {
        let url = "https://x.test/v1/models";

        let msg = non_json_models_hint(url, "upstream connect error or disconnect");
        assert!(msg.contains("不是合法 JSON"), "实际：{msg}");
        assert!(msg.contains("upstream connect error"), "应引用开头片段，实际：{msg}");

        for empty in ["", "   \n\t  "] {
            let msg = non_json_models_hint(url, empty);
            assert!(msg.contains("空响应"), "空响应应单独措辞，实际：{msg}");
            assert!(!msg.contains("开头是「」"), "不该出现空洞的开头引用，实际：{msg}");
        }
    }

    /// 过长响应体不得整段塞进错误信息（截到 80 字符）。
    #[test]
    fn non_json_models_hint_truncates_long_bodies() {
        let msg = non_json_models_hint("https://x.test/v1/models", &"A".repeat(5000));
        assert!(msg.len() < 400, "错误信息不该被响应体撑爆，长度：{}", msg.len());
    }

    #[test]
    fn health_status_treats_reachable_as_up() {
        // 拿到响应即「可达」：2xx 正常；404/405 多为不暴露该路径；429 限流；5xx 临时故障。
        // 这些都由请求时故障转移兜底，不应把 Key 踢出路由。
        for s in [200u16, 400, 404, 405, 429, 500, 502, 503] {
            assert!(status_is_healthy(s), "{s} 应判为可达");
        }
    }

    #[test]
    fn health_status_treats_auth_failure_as_down() {
        // 鉴权失败：密钥本身无效，留在候选池只会每次白跑一轮，直接判不可用。
        assert!(!status_is_healthy(401));
        assert!(!status_is_healthy(403));
    }


    #[test]
    fn a2o_keeps_system_array_and_sampling_and_tools() {
        let body = json!({
            "model": "claude-x", "max_tokens": 100,
            "system": [{ "type": "text", "text": "你是助手" }],
            "temperature": 0.5, "top_p": 0.9, "stop_sequences": ["END"],
            "tools": [{ "name": "get_weather", "description": "d", "input_schema": { "type": "object" } }],
            "tool_choice": { "type": "auto" },
            "messages": [{ "role": "user", "content": "hi" }]
        });
        let o = anthropic_to_openai(&body);
        // system 块数组不丢
        assert_eq!(o["messages"][0]["role"], "system");
        assert_eq!(o["messages"][0]["content"], "你是助手");
        // 采样字段透传
        assert_eq!(o["temperature"], 0.5);
        assert_eq!(o["top_p"], 0.9);
        assert_eq!(o["stop"][0], "END");
        // tools 转 function 形态
        assert_eq!(o["tools"][0]["type"], "function");
        assert_eq!(o["tools"][0]["function"]["name"], "get_weather");
        assert_eq!(o["tool_choice"], "auto");
    }

    #[test]
    fn o2a_expands_codex_namespace_tools() {
        // Codex（Responses）把 MCP 工具折叠进 type:"namespace" 容器的 tools[] 里。
        // 原生 OpenAI 模型认识这种折叠；但转发给 Anthropic 上游（Claude）时必须展开成
        // 独立的 type:function 工具、且用全名 <namespace>__<子工具>——否则 openai_tools_to_anthropic
        // 只认 type:function 会把整个 namespace 丢弃，Claude 收不到工具、只能瞎调 mcp__synaroute
        // 空参数，Codex router 报 unsupported call。这条测试锁住展开 + 全名。
        let tools = json!([
            { "type": "function", "name": "shell", "parameters": { "type": "object" } },
            {
                "type": "namespace",
                "name": "mcp__synaroute",
                "description": "ns",
                "tools": [
                    {
                        "type": "function",
                        "name": "synaroute_ai",
                        "description": "多模型聚合",
                        "parameters": { "type": "object", "properties": { "prompt": { "type": "string" } }, "required": ["prompt"] }
                    }
                ]
            }
        ]);
        let out = openai_tools_to_anthropic(&tools).expect("应产出工具");
        let arr = out.as_array().unwrap();
        // 顶层 function（Responses 扁平形态）保留
        assert!(arr.iter().any(|t| t["name"] == "shell"), "扁平 function 工具应保留");
        // namespace 子工具展开为全名，且带 input_schema
        let sub = arr
            .iter()
            .find(|t| t["name"] == "mcp__synaroute__synaroute_ai")
            .expect("namespace 子工具应展开为全名 mcp__synaroute__synaroute_ai");
        assert_eq!(sub["description"], "多模型聚合", "子工具描述应保留");
        assert_eq!(
            sub["input_schema"]["properties"]["prompt"]["type"],
            "string",
            "子工具 parameters 应映射为 input_schema"
        );
        // 空壳 namespace 本身不应作为工具留下
        assert!(
            !arr.iter().any(|t| t["name"] == "mcp__synaroute"),
            "namespace 容器本身不应作为工具"
        );
    }

    #[test]
    fn convert_request_responses_to_anthropic_expands_namespace_tools() {
        // 真实链路：Codex（Responses）→ Anthropic 上游，走 responses_to_chat → openai_to_anthropic
        // 两跳。namespace 折叠工具必须在第一跳就展开成 <ns>__<子工具> 全名，最终到达 Anthropic body
        // 的 tools 里、带 input_schema。这是「Codex 用中转 Claude 调 MCP 大脑聚合」调通的关键。
        let req = json!({
            "model": "claude-opus-4-7",
            "input": [{ "type": "message", "role": "user", "content": [{ "type": "input_text", "text": "hi" }] }],
            "tools": [
                {
                    "type": "namespace",
                    "name": "mcp__synaroute",
                    "description": "ns",
                    "tools": [
                        {
                            "type": "function",
                            "name": "synaroute_ai",
                            "description": "多模型聚合",
                            "parameters": { "type": "object", "properties": { "prompt": { "type": "string" } }, "required": ["prompt"] }
                        }
                    ]
                }
            ]
        });
        let out = convert_request(&req, Protocol::OpenaiResponses, Protocol::Anthropic);
        let tools = out["tools"].as_array().expect("Anthropic body 应含 tools");
        let t = tools
            .iter()
            .find(|t| t["name"] == "mcp__synaroute__synaroute_ai")
            .expect("namespace 子工具应展开为全名到达 Anthropic tools");
        assert_eq!(
            t["input_schema"]["properties"]["prompt"]["type"],
            "string",
            "子工具 schema 应完整到达 Anthropic input_schema"
        );
        assert!(
            !tools.iter().any(|t| t["name"] == "mcp__synaroute"),
            "空壳 namespace 不应到达 Anthropic tools"
        );
    }

    #[test]
    fn a2o_converts_tool_use_and_tool_result() {
        let body = json!({
            "model": "claude-x", "max_tokens": 100,
            "messages": [
                { "role": "assistant", "content": [
                    { "type": "text", "text": "调用工具" },
                    { "type": "tool_use", "id": "t1", "name": "search", "input": { "q": "x" } }
                ]},
                { "role": "user", "content": [
                    { "type": "tool_result", "tool_use_id": "t1", "content": "结果" }
                ]}
            ]
        });
        let o = anthropic_to_openai(&body);
        let msgs = o["messages"].as_array().unwrap();
        // assistant 带 tool_calls
        assert_eq!(msgs[0]["role"], "assistant");
        assert_eq!(msgs[0]["tool_calls"][0]["id"], "t1");
        assert_eq!(msgs[0]["tool_calls"][0]["function"]["name"], "search");
        // tool_result → role:tool 消息
        assert_eq!(msgs[1]["role"], "tool");
        assert_eq!(msgs[1]["tool_call_id"], "t1");
        assert_eq!(msgs[1]["content"], "结果");
    }

    #[test]
    fn o2a_handles_tool_role_and_max_completion_tokens() {
        let body = json!({
            "model": "gpt-x",
            "max_completion_tokens": 8000,
            "messages": [
                { "role": "system", "content": "sys" },
                { "role": "assistant", "content": null, "tool_calls": [
                    { "id": "c1", "type": "function", "function": { "name": "f", "arguments": "{\"a\":1}" } }
                ]},
                { "role": "tool", "tool_call_id": "c1", "content": "工具输出" }
            ]
        });
        let a = openai_to_anthropic(&body);
        // max_completion_tokens 兜底
        assert_eq!(a["max_tokens"], 8000);
        // system 提取
        assert_eq!(a["system"], "sys");
        let msgs = a["messages"].as_array().unwrap();
        // assistant tool_calls → tool_use 块
        assert_eq!(msgs[0]["role"], "assistant");
        assert_eq!(msgs[0]["content"][0]["type"], "tool_use");
        assert_eq!(msgs[0]["content"][0]["name"], "f");
        assert_eq!(msgs[0]["content"][0]["input"]["a"], 1);
        // tool 消息 → user + tool_result 块（不会产生 role:tool）
        assert_eq!(msgs[1]["role"], "user");
        assert_eq!(msgs[1]["content"][0]["type"], "tool_result");
        assert_eq!(msgs[1]["content"][0]["tool_use_id"], "c1");
        // 不残留 role:"tool"
        assert!(msgs.iter().all(|m| m["role"] != "tool"));
    }

    #[test]
    fn o2a_never_emits_empty_content_message() {
        // 空 assistant（无文本无工具）应被跳过，避免 Anthropic 400
        let body = json!({
            "model": "gpt-x", "max_tokens": 50,
            "messages": [
                { "role": "assistant", "content": "" },
                { "role": "user", "content": "hi" }
            ]
        });
        let a = openai_to_anthropic(&body);
        let msgs = a["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["role"], "user");
        assert!(msgs.iter().all(|m| {
            let c = &m["content"];
            c.as_str().map(|s| !s.is_empty()).unwrap_or(true)
        }));
    }

    // ---- 响应解析：兼容普通 JSON 与 SSE 流 ----

    #[test]
    fn parse_anthropic_plain_json() {
        let raw = r#"{"content":[{"type":"text","text":"你好"},{"type":"text","text":"世界"}]}"#;
        assert_eq!(parse_anthropic_text(raw).as_deref(), Some("你好世界"));
    }

    #[test]
    fn parse_anthropic_sse_stream() {
        // 中转站强制 SSE：逐行 data: 累加 delta.text
        let raw = "event: content_block_delta\n\
                   data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"你\"}}\n\n\
                   data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"好\"}}\n\n\
                   data: [DONE]\n";
        assert_eq!(parse_anthropic_text(raw).as_deref(), Some("你好"));
    }

    #[test]
    fn parse_openai_plain_json() {
        let raw = r#"{"choices":[{"message":{"content":"答案"}}]}"#;
        assert_eq!(parse_openai_text(raw).as_deref(), Some("答案"));
    }

    #[test]
    fn parse_openai_sse_stream() {
        let raw = "data: {\"choices\":[{\"delta\":{\"content\":\"答\"}}]}\n\n\
                   data: {\"choices\":[{\"delta\":{\"content\":\"案\"}}]}\n\n\
                   data: [DONE]\n";
        assert_eq!(parse_openai_text(raw).as_deref(), Some("答案"));
    }

    #[test]
    fn parse_returns_none_on_garbage() {
        // HTML 错误页 / 非 JSON 非 SSE → None，触发上层「响应无法解析」并带原文诊断。
        assert!(parse_anthropic_text("<html>502 Bad Gateway</html>").is_none());
        assert!(parse_openai_text("<html>502 Bad Gateway</html>").is_none());
    }


    // ---- Responses ↔ Chat 转换 ----

    /// `developer` 角色必须与 `system` 同等对待，落进 Anthropic 的 `system` 字段。
    ///
    /// 这条护栏的实际保护对象是 **Codex 的技能（skills）机制**：桌面端把技能说明放在
    /// developer 消息里（`# Using skills` + `### Available skills` 清单，含「用户点名或任务匹配时
    /// **必须**使用该 skill」这类强指令），而不是放在任何工具字段里（顶层 `tools` 与
    /// `additional_tools` 里都搜不到 skill）。
    ///
    /// 此前 developer 落进 `_ =>` 分支被降级成普通 user 消息 —— 那些强指令与用户自己的话
    /// 混在同一层，而 Anthropic 的 `system` 是独立字段、权重更高。功能不缺（模型读 SKILL.md
    /// 走的是已通的 shell_command），但遵守程度会下降。
    #[test]
    fn developer_role_maps_to_anthropic_system_not_user() {
        let chat = json!({
            "model": "m",
            "messages": [
                { "role": "developer", "content": "# Using skills\nTrigger rules: 必须使用该 skill" },
                { "role": "user", "content": "帮我建个 skill" }
            ]
        });
        let ant = openai_to_anthropic(&chat);
        let sys = ant["system"].as_str().unwrap_or_default();
        assert!(sys.contains("Using skills"), "developer 内容必须进 system: {ant}");
        assert!(sys.contains("必须使用该 skill"), "强指令不得丢: {sys}");

        // 用户消息仍是唯一的 user 消息 —— developer 不该再被降级混进对话层。
        let msgs = ant["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 1, "developer 不应再产出一条 user 消息: {ant}");
        assert_eq!(msgs[0]["role"], "user");
        assert_eq!(msgs[0]["content"], "帮我建个 skill");
    }

    /// 多条 system/developer 混排时按顺序拼接，且**补空行分隔**。
    ///
    /// Codex 实际会连发多条 developer 消息（一条是技能使用说明、一条是可用技能清单）。
    /// 直接首尾相接会让「上一段末尾」与「下一段标题」黏成一行，改变 Markdown 结构。
    #[test]
    fn multiple_system_and_developer_messages_are_joined_with_blank_line() {
        let chat = json!({
            "model": "m",
            "messages": [
                { "role": "system", "content": "段一" },
                { "role": "developer", "content": "## Skills" },
                { "role": "developer", "content": "### Available skills\n- imagegen: ..." },
                { "role": "user", "content": "hi" }
            ]
        });
        let ant = openai_to_anthropic(&chat);
        let sys = ant["system"].as_str().unwrap();
        assert_eq!(sys, "段一\n\n## Skills\n\n### Available skills\n- imagegen: ...");
    }

    /// 空 developer 消息不得往 system 里塞出多余空行（Codex 的 additional_tools 项被跳过后，
    /// 历史上曾产生过空 developer 消息）。
    #[test]
    fn empty_developer_message_does_not_pollute_system() {
        let chat = json!({
            "model": "m",
            "messages": [
                { "role": "system", "content": "真实系统提示" },
                { "role": "developer", "content": "" },
                { "role": "user", "content": "hi" }
            ]
        });
        let ant = openai_to_anthropic(&chat);
        assert_eq!(ant["system"], "真实系统提示");
    }

    #[test]
    fn responses_to_chat_maps_instructions_and_input() {
        // Codex 风格请求：instructions → system，input 字符串 → user 消息。
        let req = json!({
            "model": "gpt-5.5",
            "instructions": "You are helpful.",
            "input": "hello",
            "max_output_tokens": 100
        });
        let chat = responses_to_chat(&req);
        let msgs = chat["messages"].as_array().unwrap();
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[0]["content"], "You are helpful.");
        assert_eq!(msgs[1]["role"], "user");
        assert_eq!(msgs[1]["content"], "hello");
        assert_eq!(chat["max_tokens"], 100);
    }

    #[test]
    fn responses_to_chat_maps_input_item_array() {
        // input 为 item 数组（含 output_text 分块）。
        let req = json!({
            "model": "m",
            "input": [
                { "type": "message", "role": "user", "content": [{ "type": "input_text", "text": "hi there" }] }
            ]
        });
        let chat = responses_to_chat(&req);
        let msgs = chat["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["role"], "user");
        assert_eq!(msgs[0]["content"], "hi there");
    }

    #[test]
    fn responses_to_chat_preserves_reasoning() {
        // Codex 发的推理强度（reasoning.effort）必须透传到 Chat 中枢，供下游映射/透传，
        // 不能在第一跳就丢（此前 copy_through 只带 temperature/top_p/stream，导致强度失效）。
        let req = json!({
            "model": "m",
            "input": "hi",
            "reasoning": { "effort": "high" }
        });
        let chat = responses_to_chat(&req);
        assert_eq!(chat["reasoning"]["effort"], "high", "reasoning 应透传到 Chat 中枢");
    }

    #[test]
    fn openai_to_anthropic_maps_effort_to_thinking() {
        // 主链路：Codex(Responses,reasoning.effort) → Chat 中枢 → Anthropic 上游。
        // effort 档位须映射成 Anthropic thinking.budget_tokens，并归一 temperature=1、去 top_p。
        let chat = json!({
            "model": "claude-opus-4-8",
            "max_tokens": 20000,
            "messages": [ { "role": "user", "content": "hi" } ],
            "reasoning": { "effort": "high" },
            "temperature": 0.5,
            "top_p": 0.9
        });
        let a = openai_to_anthropic(&chat);
        assert_eq!(a["thinking"]["type"], "enabled", "high 档应开启扩展思考");
        assert!(a["thinking"]["budget_tokens"].as_u64().unwrap() > 0, "应有正的思考预算");
        assert_eq!(a["temperature"], 1, "开思考时 temperature 须归一为 1");
        assert!(a.get("top_p").is_none(), "开思考时须去掉 top_p（Anthropic 不允许同用）");
    }

    #[test]
    fn openai_to_anthropic_minimal_effort_no_thinking() {
        // minimal 档：不启用扩展思考，保持普通回答（不注入 thinking）。
        let chat = json!({
            "model": "claude-opus-4-8",
            "max_tokens": 20000,
            "messages": [ { "role": "user", "content": "hi" } ],
            "reasoning": { "effort": "minimal" }
        });
        let a = openai_to_anthropic(&chat);
        assert!(a.get("thinking").is_none(), "minimal 档不应开思考");
    }

    #[test]
    fn openai_to_anthropic_thinking_budget_clamped_by_max_tokens() {
        // budget 必须 < max_tokens 且留输出空间：high(16384) 在 max_tokens=6000 时应被钳到 ≤3000。
        let chat = json!({
            "model": "claude-opus-4-8",
            "max_tokens": 6000,
            "messages": [ { "role": "user", "content": "hi" } ],
            "reasoning": { "effort": "high" }
        });
        let a = openai_to_anthropic(&chat);
        let budget = a["thinking"]["budget_tokens"].as_u64().unwrap();
        assert!(budget <= 3000, "预算应被 max_tokens/2 钳制，实际 {budget}");
    }

    #[test]
    fn chat_to_responses_passes_reasoning_through() {
        // 上游若是原生 Responses：reasoning 直接透传（它认 effort 档位，无需映射）。
        let chat = json!({
            "model": "gpt-5.1",
            "messages": [ { "role": "user", "content": "hi" } ],
            "reasoning": { "effort": "medium" }
        });
        let r = chat_to_responses(&chat);
        assert_eq!(r["reasoning"]["effort"], "medium", "原生 Responses 上游应原样收到 reasoning");
    }

    #[test]
    fn anthropic_to_openai_maps_thinking_to_effort() {
        // 反向：Anthropic thinking.budget_tokens → Chat 中枢 reasoning.effort（补全对称）。
        let a = json!({
            "model": "claude-opus-4-8",
            "max_tokens": 20000,
            "messages": [ { "role": "user", "content": "hi" } ],
            "thinking": { "type": "enabled", "budget_tokens": 16384 }
        });
        let chat = anthropic_to_openai(&a);
        assert_eq!(chat["reasoning"]["effort"], "high", "16384 预算应映射为 high 档");
    }

    #[test]
    fn convert_request_responses_to_chat_lowers_effort_to_top_level() {
        // Codex(Responses,reasoning.effort=xhigh) → Chat 上游：Chat Completions 认的是顶层
        // reasoning_effort 字符串（无 xhigh 档），不认 Responses 的 reasoning:{effort} 对象。
        // 故转换须落成顶层 reasoning_effort 且 xhigh 钳到 high，并移除 reasoning 对象，
        // 否则推理强度被 Chat 上游忽略、严格上游还可能 400。
        let req = json!({
            "model": "gpt-5.1",
            "input": "hi",
            "reasoning": { "effort": "xhigh" }
        });
        let chat = convert_request(&req, Protocol::OpenaiResponses, Protocol::OpenaiChat);
        assert_eq!(chat["reasoning_effort"], "high", "xhigh 应钳到 high 并落顶层 reasoning_effort");
        assert!(chat.get("reasoning").is_none(), "Chat 上游不应残留 reasoning 对象");
    }

    #[test]
    fn convert_request_chat_downstream_effort_maps_to_anthropic_thinking() {
        // Chat 下游客户端发顶层 reasoning_effort 字符串 → Anthropic 上游：须被读到并映射成
        // thinking.budget_tokens（此前 read_reasoning_effort 只读对象形态，顶层字符串会丢）。
        let req = json!({
            "model": "claude-opus-4-8",
            "max_tokens": 20000,
            "messages": [ { "role": "user", "content": "hi" } ],
            "reasoning_effort": "high"
        });
        let a = convert_request(&req, Protocol::OpenaiChat, Protocol::Anthropic);
        assert_eq!(a["thinking"]["type"], "enabled", "顶层 reasoning_effort=high 应开启扩展思考");
        assert!(a["thinking"]["budget_tokens"].as_u64().unwrap() > 0, "应有正的思考预算");
    }

    #[test]
    fn chat_resp_to_responses_maps_text_and_usage() {
        // Chat 上游响应 → Responses 形态：output[].message + usage 键改名。
        let resp = json!({
            "id": "chatcmpl-1",
            "model": "deepseek-chat",
            "choices": [ { "index": 0, "message": { "role": "assistant", "content": "answer" }, "finish_reason": "stop" } ],
            "usage": { "prompt_tokens": 5, "completion_tokens": 3 }
        });
        let out = chat_resp_to_responses(&resp);
        assert_eq!(out["object"], "response");
        assert_eq!(out["status"], "completed");
        let items = out["output"].as_array().unwrap();
        assert_eq!(items[0]["type"], "message");
        assert_eq!(items[0]["content"][0]["type"], "output_text");
        assert_eq!(items[0]["content"][0]["text"], "answer");
        assert_eq!(out["usage"]["input_tokens"], 5);
        assert_eq!(out["usage"]["output_tokens"], 3);
        assert_eq!(out["usage"]["total_tokens"], 8);
    }

    #[test]
    fn chat_resp_to_responses_maps_tool_calls() {
        let resp = json!({
            "choices": [ { "message": { "role": "assistant", "content": null,
                "tool_calls": [ { "id": "call_1", "type": "function", "function": { "name": "get_weather", "arguments": "{\"city\":\"SF\"}" } } ] },
                "finish_reason": "tool_calls" } ]
        });
        let out = chat_resp_to_responses(&resp);
        let items = out["output"].as_array().unwrap();
        let fc = items.iter().find(|i| i["type"] == "function_call").unwrap();
        assert_eq!(fc["name"], "get_weather");
        assert_eq!(fc["call_id"], "call_1");
        assert_eq!(fc["arguments"], "{\"city\":\"SF\"}");
    }

    #[test]
    fn convert_request_same_protocol_is_passthrough() {
        let body = json!({ "model": "m", "input": "x" });
        let out = convert_request(&body, Protocol::OpenaiResponses, Protocol::OpenaiResponses);
        assert_eq!(out, body, "同协议应原样返回");
    }

    #[test]
    fn convert_request_responses_to_chat_via_hub() {
        // Codex(Responses) 下游 → Chat-only 上游（DeepSeek）：核心兼容路径。
        let body = json!({ "model": "m", "instructions": "sys", "input": "hi" });
        let out = convert_request(&body, Protocol::OpenaiResponses, Protocol::OpenaiChat);
        let msgs = out["messages"].as_array().unwrap();
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[1]["content"], "hi");
        assert!(out.get("input").is_none(), "应已转为 Chat 形态，无 input 字段");
    }

    #[test]
    fn convert_response_chat_to_responses_via_hub() {
        // Chat-only 上游响应 → Codex(Responses) 下游期望形态。
        let body = json!({
            "choices": [ { "message": { "role": "assistant", "content": "hi" }, "finish_reason": "stop" } ],
            "usage": { "prompt_tokens": 1, "completion_tokens": 1 }
        });
        let out = convert_response(&body, Protocol::OpenaiChat, Protocol::OpenaiResponses);
        assert_eq!(out["object"], "response");
        assert_eq!(out["output"][0]["content"][0]["text"], "hi");
    }

    #[test]
    fn convert_roundtrip_responses_chat_responses_preserves_text() {
        // Responses → Chat → Responses 往返，核心文本不丢。
        let req = json!({ "model": "m", "input": "roundtrip test" });
        let chat = convert_request(&req, Protocol::OpenaiResponses, Protocol::OpenaiChat);
        let back = convert_request(&chat, Protocol::OpenaiChat, Protocol::OpenaiResponses);
        // back 应能再转回 Chat 且 user 文本一致
        let chat2 = convert_request(&back, Protocol::OpenaiResponses, Protocol::OpenaiChat);
        let msgs = chat2["messages"].as_array().unwrap();
        assert!(msgs.iter().any(|m| m["content"] == "roundtrip test"));
    }

    /// Claude 桌面端（3p）真实请求骨架：Anthropic 协议，顶层 `tools` 里带 MCP 工具全名，
    /// 历史里含一轮完整的 tool_use → tool_result。这是 2026-07-30 实机场景的最小复现。
    fn claude_desktop_request_with_mcp_tool() -> Value {
        json!({
            "model": "claude-opus-4-7",
            "max_tokens": 4096,
            "tools": [ {
                "name": "mcp__synaroute__synaroute_ai",
                "description": "多模型大脑聚合",
                "input_schema": {
                    "type": "object",
                    "properties": { "prompt": { "type": "string" } },
                    "required": ["prompt"]
                }
            } ],
            "tool_choice": { "type": "auto" },
            "messages": [
                { "role": "user", "content": "调用 synaroute_ai 比较快排和归并" },
                { "role": "assistant", "content": [
                    { "type": "text", "text": "好，我问一下" },
                    { "type": "tool_use", "id": "toolu_1",
                      "name": "mcp__synaroute__synaroute_ai",
                      "input": { "prompt": "快排 vs 归并" } }
                ] },
                { "role": "user", "content": [
                    { "type": "tool_result", "tool_use_id": "toolu_1", "content": "两者各有取舍" }
                ] }
            ]
        })
    }

    #[test]
    fn desktop_anthropic_to_responses_carries_mcp_tool_and_history() {
        // 桌面端故障转移到 Responses 上游（实机命中的正是这条路）：
        // 工具声明、历史里的工具调用与结果都必须完整过去，否则模型无从知道能调什么、
        // 也读不到上一轮工具的返回。
        let out = convert_request(
            &claude_desktop_request_with_mcp_tool(),
            Protocol::Anthropic,
            Protocol::OpenaiResponses,
        );
        let tools = out["tools"].as_array().expect("Responses 请求须带 tools");
        assert_eq!(tools.len(), 1);
        assert_eq!(
            tools[0]["name"], "mcp__synaroute__synaroute_ai",
            "MCP 工具全名须原样保留"
        );
        assert_eq!(
            tools[0]["parameters"]["properties"]["prompt"]["type"], "string",
            "input_schema 须映射为 Responses 的 parameters"
        );
        assert_eq!(out["tool_choice"], json!("auto"), "tool_choice 须透传");

        // 历史：function_call（带 call_id）+ function_call_output 都要在。
        let input = out["input"].as_array().expect("须有 input");
        let call = input
            .iter()
            .find(|i| i["type"] == "function_call")
            .expect("历史里的工具调用丢失");
        assert_eq!(call["call_id"], "toolu_1", "call_id 须守恒，结果才能回配");
        assert_eq!(call["name"], "mcp__synaroute__synaroute_ai");
        assert_eq!(
            serde_json::from_str::<Value>(call["arguments"].as_str().unwrap()).unwrap(),
            json!({ "prompt": "快排 vs 归并" })
        );
        let output = input
            .iter()
            .find(|i| i["type"] == "function_call_output")
            .expect("历史里的工具结果丢失");
        assert_eq!(output["call_id"], "toolu_1");
        assert_eq!(output["output"], "两者各有取舍");
    }

    #[test]
    fn desktop_anthropic_to_chat_carries_mcp_tool_and_history() {
        // 同一请求转到 Chat-only 上游：口径必须与 →Responses 一致（不能只修一条路）。
        let out = convert_request(
            &claude_desktop_request_with_mcp_tool(),
            Protocol::Anthropic,
            Protocol::OpenaiChat,
        );
        let tools = out["tools"].as_array().expect("Chat 请求须带 tools");
        assert_eq!(tools[0]["function"]["name"], "mcp__synaroute__synaroute_ai");
        assert_eq!(out["tool_choice"], json!("auto"));
        let msgs = out["messages"].as_array().unwrap();
        let asst = msgs
            .iter()
            .find(|m| m.get("tool_calls").is_some())
            .expect("历史里的 assistant 工具调用丢失");
        assert_eq!(asst["tool_calls"][0]["id"], "toolu_1");
        assert_eq!(
            asst["tool_calls"][0]["function"]["name"],
            "mcp__synaroute__synaroute_ai"
        );
        let tool_msg = msgs
            .iter()
            .find(|m| m["role"] == "tool")
            .expect("历史里的工具结果丢失");
        assert_eq!(tool_msg["tool_call_id"], "toolu_1");
    }

    #[test]
    fn forced_tool_choice_survives_all_cross_protocol_hops() {
        // 「强制调用某工具」不能在任何一跳降级成自由选择——降级后模型可能干脆不调，
        // 而调用方（如桌面端某些编排）是按「必调」预期写的。
        let anthropic = json!({
            "model": "m",
            "max_tokens": 16,
            "messages": [ { "role": "user", "content": "go" } ],
            "tools": [ { "name": "f", "input_schema": { "type": "object" } } ],
            "tool_choice": { "type": "tool", "name": "f" }
        });
        let to_chat = convert_request(&anthropic, Protocol::Anthropic, Protocol::OpenaiChat);
        assert_eq!(
            to_chat["tool_choice"],
            json!({ "type": "function", "function": { "name": "f" } }),
            "Anthropic→Chat 强制档丢失"
        );
        let to_resp = convert_request(&anthropic, Protocol::Anthropic, Protocol::OpenaiResponses);
        assert_eq!(
            to_resp["tool_choice"],
            json!({ "type": "function", "name": "f" }),
            "Anthropic→Responses 强制档丢失（Responses 用扁平 name）"
        );
        // Responses 扁平形态 → Chat 嵌套形态，往返不失真。
        let back = convert_request(&to_resp, Protocol::OpenaiResponses, Protocol::OpenaiChat);
        assert_eq!(
            back["tool_choice"],
            json!({ "type": "function", "function": { "name": "f" } }),
            "Responses→Chat 强制档丢失"
        );
    }

    // ---- 流式 SSE 翻译（Task #16）----

    #[test]
    fn sse_direction_covers_supported_and_rejects_same() {
        use Protocol::*;
        assert_eq!(sse_direction(OpenaiResponses, OpenaiChat), Some(SseDirection::ChatToResponses));
        assert_eq!(sse_direction(OpenaiChat, OpenaiResponses), Some(SseDirection::ResponsesToChat));
        assert_eq!(sse_direction(Anthropic, OpenaiChat), Some(SseDirection::ChatToAnthropic));
        assert_eq!(sse_direction(OpenaiChat, Anthropic), Some(SseDirection::AnthropicToChat));
        assert_eq!(sse_direction(OpenaiResponses, Anthropic), Some(SseDirection::AnthropicToResponses));
        assert_eq!(sse_direction(Anthropic, OpenaiResponses), Some(SseDirection::ResponsesToAnthropic));
        // 同协议：None（原样直通，无需翻译）
        assert_eq!(sse_direction(OpenaiChat, OpenaiChat), None);
        assert_eq!(sse_direction(Anthropic, Anthropic), None);
        assert_eq!(sse_direction(OpenaiResponses, OpenaiResponses), None);
    }

    #[test]
    fn sse_chat_to_responses_reassembles_text_stream() {
        // 主场景：Codex(Responses 下游) 连 Chat-only 上游。上游按 Chat SSE 逐块吐文本增量，
        // 翻译器须重组为 Responses 事件序列（created → item.added → text.delta* → completed）。
        let mut tr = SseTranslator::new(SseDirection::ChatToResponses);
        let mut out = String::new();
        out.push_str(&tr.push(b"data: {\"model\":\"deepseek-chat\",\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\n"));
        out.push_str(&tr.push(b"data: {\"choices\":[{\"delta\":{\"content\":\"lo\"}}]}\n\n"));
        out.push_str(&tr.push(b"data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":1}}\n\n"));
        out.push_str(&tr.finish());
        // 起始事件
        assert!(out.contains("event: response.created"), "缺 response.created:\n{out}");
        assert!(out.contains("event: response.output_item.added"), "缺 output_item.added");
        // 两段文本增量
        assert!(out.contains("\"delta\":\"Hel\""), "缺第一段增量");
        assert!(out.contains("\"delta\":\"lo\""), "缺第二段增量");
        // 关键回归：文本 message 必须发 output_item.done 且带完整全文——Codex 靠该事件把
        // assistant 回复持久化进会话（此前只发 delta，重开对话文本回复全丢，只剩用户问题）。
        assert!(
            out.contains("event: response.output_item.done"),
            "文本缺 output_item.done（Codex 据此持久化 assistant 回复）:\n{out}"
        );
        assert!(
            out.contains("\"type\":\"message\"") && out.contains("\"text\":\"Hello\""),
            "output_item.done 的 message 未带完整全文 Hello:\n{out}"
        );
        // 收尾（usage 触发 completed，finish 幂等不重复）
        assert!(out.contains("event: response.completed"), "缺 response.completed");
        assert!(out.contains("\"input_tokens\":2"), "usage 未映射");
        assert_eq!(out.matches("event: response.completed").count(), 1, "completed 应只出现一次");
    }

    #[test]
    fn sse_chat_to_responses_handles_split_lines() {
        // 上游一个 chunk 切在半行中间：翻译器按行缓冲，凑齐 \n 才产出，不得丢字符。
        let mut tr = SseTranslator::new(SseDirection::ChatToResponses);
        let mut out = String::new();
        out.push_str(&tr.push(b"data: {\"choices\":[{\"delta\":{\"content\":\"AB")); // 半行
        out.push_str(&tr.push(b"C\"}}]}\n\n")); // 补齐
        out.push_str(&tr.finish());
        assert!(out.contains("\"delta\":\"ABC\""), "半行拼接后应得完整增量:\n{out}");
    }

    #[test]
    fn sse_chat_to_responses_maps_tool_call_deltas() {
        // Chat tool_call 增量（name 一次给全、arguments 分块）→ Responses function_call item。
        let mut tr = SseTranslator::new(SseDirection::ChatToResponses);
        let mut out = String::new();
        out.push_str(&tr.push(b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"get_weather\",\"arguments\":\"{\\\"ci\"}}]}}]}\n\n"));
        out.push_str(&tr.push(b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"ty\\\":\\\"SF\\\"}\"}}]}}]}\n\n"));
        out.push_str(&tr.push(b"data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n"));
        out.push_str(&tr.finish());
        assert!(out.contains("\"type\":\"function_call\""), "缺 function_call item:\n{out}");
        assert!(out.contains("\"name\":\"get_weather\""), "工具名未重组");
        assert!(out.contains("get_weather"));
        // arguments 分块应拼接完整
        assert!(out.contains("SF"), "参数分块未拼全");
        // 关键回归：工具调用必须作为 output_item.done 事件投递——Codex 只从该事件执行工具，
        // 仅塞进 completed.output 会被忽略、导致客户端卡死（本次修复的根因）。
        assert!(
            out.contains("event: response.output_item.done"),
            "工具调用缺 output_item.done 事件（Codex 据此执行工具）:\n{out}"
        );
        assert!(out.contains("\"call_id\":\"call_1\""), "call_id 未带出（工具结果无法回配）");
    }

    #[test]
    fn sse_responses_to_chat_reassembles_text() {
        // 反向：Responses 上游 → Chat 下游。output_text.delta → chat.completion.chunk。
        let mut tr = SseTranslator::new(SseDirection::ResponsesToChat);
        let mut out = String::new();
        out.push_str(&tr.push(b"event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"Hi\"}\n\n"));
        out.push_str(&tr.push(b"event: response.completed\ndata: {\"type\":\"response.completed\"}\n\n"));
        out.push_str(&tr.finish());
        assert!(out.contains("chat.completion.chunk"), "未转为 chat chunk:\n{out}");
        assert!(out.contains("\"content\":\"Hi\""), "文本增量丢失");
        assert!(out.contains("\"finish_reason\":\"stop\""), "缺 finish");
        assert!(out.contains("data: [DONE]"), "Chat 流须以 [DONE] 收尾");
    }

    #[test]
    fn sse_chat_to_anthropic_reassembles_text() {
        // Chat 上游 → Anthropic 下游（Claude CLI 连 Chat-only 厂商）。
        let mut tr = SseTranslator::new(SseDirection::ChatToAnthropic);
        let mut out = String::new();
        out.push_str(&tr.push(b"data: {\"model\":\"glm-4.6\",\"choices\":[{\"delta\":{\"content\":\"Yo\"}}]}\n\n"));
        out.push_str(&tr.push(b"data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n"));
        out.push_str(&tr.finish());
        assert!(out.contains("event: message_start"), "缺 message_start:\n{out}");
        assert!(out.contains("event: content_block_start"), "缺 content_block_start");
        assert!(out.contains("\"type\":\"text_delta\""), "缺 text_delta");
        assert!(out.contains("\"text\":\"Yo\""), "文本增量丢失");
        assert!(out.contains("event: message_stop"), "缺 message_stop");
    }

    #[test]
    fn sse_anthropic_to_chat_reassembles_text() {
        // Anthropic 上游 → Chat 下游。
        let mut tr = SseTranslator::new(SseDirection::AnthropicToChat);
        let mut out = String::new();
        out.push_str(&tr.push(b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hey\"}}\n\n"));
        out.push_str(&tr.push(b"event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"));
        out.push_str(&tr.finish());
        assert!(out.contains("chat.completion.chunk"));
        assert!(out.contains("\"content\":\"Hey\""), "文本增量丢失:\n{out}");
        assert!(out.contains("\"finish_reason\":\"stop\""));
        assert!(out.contains("data: [DONE]"));
    }

    #[test]
    fn sse_finish_is_idempotent_when_no_content() {
        // 空流（上游直接结束、无任何 chunk）：finish 不得产出半截事件。
        let mut tr = SseTranslator::new(SseDirection::ChatToResponses);
        let tail = tr.finish();
        assert!(tail.is_empty(), "未开始的流收尾应为空，实际:\n{tail}");
    }

    #[test]
    fn sse_direction_covers_anthropic_responses_pair() {
        // 新增两方向：Codex(Responses 下游) 连 Claude(Anthropic 上游) 及其镜像。
        use Protocol::*;
        assert_eq!(
            sse_direction(OpenaiResponses, Anthropic),
            Some(SseDirection::AnthropicToResponses)
        );
        assert_eq!(
            sse_direction(Anthropic, OpenaiResponses),
            Some(SseDirection::ResponsesToAnthropic)
        );
    }

    #[test]
    fn sse_anthropic_to_responses_reassembles_text_and_usage() {
        // 主诉求：Codex(Responses 下游) 连 Claude 上游。Anthropic SSE（message_start →
        // content_block_delta(text) → message_delta(usage) → message_stop）须重组为
        // Responses 事件序列（created → item.added → text.delta* → text.done → completed）。
        let mut tr = SseTranslator::new(SseDirection::AnthropicToResponses);
        let mut out = String::new();
        out.push_str(&tr.push(b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"model\":\"claude-opus-4-8\",\"usage\":{\"input_tokens\":7,\"output_tokens\":0}}}\n\n"));
        out.push_str(&tr.push(b"event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n"));
        out.push_str(&tr.push(b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi\"}}\n\n"));
        out.push_str(&tr.push(b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\" there\"}}\n\n"));
        out.push_str(&tr.push(b"event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":3}}\n\n"));
        out.push_str(&tr.push(b"event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"));
        out.push_str(&tr.finish());
        // 起始事件
        assert!(out.contains("event: response.created"), "缺 response.created:\n{out}");
        assert!(out.contains("event: response.output_item.added"), "缺 output_item.added");
        assert!(out.contains("\"model\":\"claude-opus-4-8\""), "model 未捕获");
        // 两段文本增量
        assert!(out.contains("\"delta\":\"Hi\""), "缺第一段增量");
        assert!(out.contains("\"delta\":\" there\""), "缺第二段增量");
        // 收尾：text.done + completed（usage 用 Anthropic 分散字段累积）
        assert!(out.contains("event: response.output_text.done"), "缺 output_text.done");
        // 关键回归：文本 message 必须发 output_item.done 且带完整全文（Hi there）——Codex 靠该事件
        // 持久化 assistant 回复；此前只发 delta，重开对话文本回复全丢，只剩用户问题。
        assert!(
            out.contains("event: response.output_item.done"),
            "文本缺 output_item.done（Codex 据此持久化 assistant 回复）:\n{out}"
        );
        assert!(
            out.contains("\"type\":\"message\"") && out.contains("\"text\":\"Hi there\""),
            "output_item.done 的 message 未带完整全文 Hi there:\n{out}"
        );
        assert!(out.contains("event: response.completed"), "缺 response.completed");
        assert!(out.contains("\"input_tokens\":7"), "input_tokens 未归位:\n{out}");
        assert!(out.contains("\"output_tokens\":3"), "output_tokens 未归位:\n{out}");
        assert_eq!(out.matches("event: response.completed").count(), 1, "completed 应只出现一次");
    }

    #[test]
    fn sse_anthropic_to_responses_maps_tool_use_deltas() {
        // Codex 重度用 function calling：Anthropic tool_use 块 + input_json_delta 分块累积
        // → Responses function_call output item，参数拼全。
        let mut tr = SseTranslator::new(SseDirection::AnthropicToResponses);
        let mut out = String::new();
        out.push_str(&tr.push(b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"model\":\"claude-opus-4-8\",\"usage\":{\"input_tokens\":5}}}\n\n"));
        out.push_str(&tr.push(b"event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"get_weather\"}}\n\n"));
        out.push_str(&tr.push(b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"ci\"}}\n\n"));
        out.push_str(&tr.push(b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"ty\\\":\\\"SF\\\"}\"}}\n\n"));
        out.push_str(&tr.push(b"event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n"));
        out.push_str(&tr.push(b"event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"));
        out.push_str(&tr.finish());
        assert!(out.contains("\"type\":\"function_call\""), "缺 function_call item:\n{out}");
        assert!(out.contains("\"name\":\"get_weather\""), "工具名未重组");
        assert!(out.contains("\"call_id\":\"toolu_1\""), "call_id 未带出");
        // 参数分块应拼接完整
        assert!(out.contains("SF"), "参数分块未拼全");
        // 关键修复回归：工具调用必须作为 output_item.done 事件投递——Codex 只从该事件取
        // function_call 执行工具，只塞进 completed.output 会被忽略、客户端卡死等待。
        assert!(
            out.contains("event: response.output_item.done"),
            "工具调用必须走 output_item.done 事件（Codex 据此执行工具）:\n{out}"
        );
    }

    #[test]
    fn sse_anthropic_to_responses_splits_namespaced_tool_call() {
        // Codex 大脑聚合根因回归：上游模型按展开全名 `mcp__synaroute__synaroute_ai` 回调工具，
        // 翻译器必须拆回 name="synaroute_ai" + namespace="mcp__synaroute" 两个独立字段。
        // Codex router 用结构化 ToolName{namespace,name} 查注册表，不拆 name 字符串——缺 namespace
        // 字段就查 {namespace:None, name:"mcp__synaroute__synaroute_ai"} 匹配不到 → unsupported call。
        let mut tr = SseTranslator::with_namespaces(
            SseDirection::AnthropicToResponses,
            vec!["mcp__synaroute".to_string()],
        );
        let mut out = String::new();
        out.push_str(&tr.push(b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"model\":\"claude-opus-4-8\",\"usage\":{\"input_tokens\":5}}}\n\n"));
        out.push_str(&tr.push(b"event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_9\",\"name\":\"mcp__synaroute__synaroute_ai\"}}\n\n"));
        out.push_str(&tr.push(b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"prompt\\\":\\\"hi\\\"}\"}}\n\n"));
        out.push_str(&tr.push(b"event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n"));
        out.push_str(&tr.push(b"event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"));
        out.push_str(&tr.finish());
        assert!(out.contains("\"type\":\"function_call\""), "缺 function_call item:\n{out}");
        // 关键：name 拆成裸子工具名，namespace 单独成字段。
        assert!(out.contains("\"name\":\"synaroute_ai\""), "name 未拆成裸子工具名:\n{out}");
        assert!(out.contains("\"namespace\":\"mcp__synaroute\""), "缺 namespace 独立字段:\n{out}");
        // 不能再把全名塞进 name（那正是 unsupported call 的根因）。
        assert!(
            !out.contains("\"name\":\"mcp__synaroute__synaroute_ai\""),
            "name 仍是未拆的全名（会导致 Codex unsupported call）:\n{out}"
        );
    }

    #[test]
    fn sse_anthropic_to_responses_flat_tool_call_keeps_no_namespace() {
        // 平铺工具（Codex 内置 update_plan，无 namespace 前缀）不受拆名影响：name 原样、无 namespace 字段。
        let mut tr = SseTranslator::with_namespaces(
            SseDirection::AnthropicToResponses,
            vec!["mcp__synaroute".to_string()],
        );
        let mut out = String::new();
        out.push_str(&tr.push(b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"model\":\"claude-opus-4-8\",\"usage\":{\"input_tokens\":5}}}\n\n"));
        out.push_str(&tr.push(b"event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_5\",\"name\":\"update_plan\"}}\n\n"));
        out.push_str(&tr.push(b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{}\"}}\n\n"));
        out.push_str(&tr.push(b"event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n"));
        out.push_str(&tr.push(b"event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"));
        out.push_str(&tr.finish());
        assert!(out.contains("\"name\":\"update_plan\""), "平铺工具名应原样:\n{out}");
        assert!(!out.contains("\"namespace\""), "平铺工具不应带 namespace 字段:\n{out}");
    }

    #[test]
    fn sse_anthropic_to_responses_maps_thinking_to_reasoning_summary() {
        // Claude 扩展思考（thinking 块）→ Codex Responses reasoning_summary 事件，
        // 让 Codex 显示思考过程（也据此可判推理强度是否真生效）。
        let mut tr = SseTranslator::new(SseDirection::AnthropicToResponses);
        let mut out = String::new();
        out.push_str(&tr.push(b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"model\":\"claude-opus-4-8\",\"usage\":{\"input_tokens\":5}}}\n\n"));
        // thinking 块：start → 两段 thinking_delta → stop
        out.push_str(&tr.push(b"event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\"}}\n\n"));
        out.push_str(&tr.push(b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"Let me\"}}\n\n"));
        out.push_str(&tr.push(b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\" think\"}}\n\n"));
        out.push_str(&tr.push(b"event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n"));
        // 随后普通文本块（回答）
        out.push_str(&tr.push(b"event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n"));
        out.push_str(&tr.push(b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi\"}}\n\n"));
        out.push_str(&tr.push(b"event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"));
        out.push_str(&tr.finish());
        // 思考起始 + 增量 + 收尾（Codex 认的三个 reasoning summary 事件名）
        assert!(out.contains("event: response.reasoning_summary_part.added"), "缺 reasoning summary 起始:\n{out}");
        assert!(out.contains("event: response.reasoning_summary_text.delta"), "缺 reasoning summary 增量");
        assert!(out.contains("event: response.reasoning_summary_text.done"), "缺 reasoning summary 收尾");
        // 思考增量应拼全
        assert!(out.contains("Let me") && out.contains(" think"), "思考增量未透传");
        // 思考不能混进正文 output_text（thinking_delta 不该走 output_text.delta）
        assert!(!out.contains("\"delta\":\"Let me\"") || out.contains("reasoning_summary_text.delta"), "思考不应作为普通文本增量");
        // 普通文本仍正常
        assert!(out.contains("\"delta\":\"Hi\""), "回答文本未透传");
    }

    #[test]
    fn sse_anthropic_to_responses_handles_split_lines() {
        // Anthropic 一个 chunk 切在半行中间：翻译器按行缓冲，凑齐 \n 才产出，不得丢字符。
        let mut tr = SseTranslator::new(SseDirection::AnthropicToResponses);
        let mut out = String::new();
        out.push_str(&tr.push(b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"model\":\"claude-opus-4-8\"}}\n\n"));
        out.push_str(&tr.push(b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"AB")); // 半行
        out.push_str(&tr.push(b"C\"}}\n\n")); // 补齐
        out.push_str(&tr.finish());
        assert!(out.contains("\"delta\":\"ABC\""), "半行拼接后应得完整增量:\n{out}");
    }

    #[test]
    fn sse_responses_to_anthropic_reassembles_text() {
        // 镜像方向：Responses 上游 → Anthropic 下游。output_text.delta → content_block_delta；
        // completed → message_stop 收尾。
        let mut tr = SseTranslator::new(SseDirection::ResponsesToAnthropic);
        let mut out = String::new();
        out.push_str(&tr.push(b"event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"model\":\"gpt-5.5\"}}\n\n"));
        out.push_str(&tr.push(b"event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"Yo\"}\n\n"));
        out.push_str(&tr.push(b"event: response.completed\ndata: {\"type\":\"response.completed\"}\n\n"));
        out.push_str(&tr.finish());
        assert!(out.contains("event: message_start"), "缺 message_start:\n{out}");
        assert!(out.contains("event: content_block_start"), "缺 content_block_start");
        assert!(out.contains("\"type\":\"text_delta\""), "缺 text_delta");
        assert!(out.contains("\"text\":\"Yo\""), "文本增量丢失");
        assert!(out.contains("event: message_stop"), "缺 message_stop");
        // 纯文本流不得声明工具：stop_reason 必须是 end_turn。
        assert!(out.contains("\"stop_reason\":\"end_turn\""), "纯文本应 end_turn:\n{out}");
    }

    /// 中文不得因 TCP 分段而腐蚀成 `�`（真实缺陷回归）。
    ///
    /// 缺陷形态：`push` 原先对**每一块**入参做 `String::from_utf8_lossy`。而上游是流式的，
    /// 一个 3 字节的中文字符完全可能被 TCP 分段切开、分两次 `push` 进来 ——
    /// 逐块解码时前半截和后半截各自都是非法 UTF-8，各自被替换成 U+FFFD，
    /// 于是用户看到的回答里凭空出现「」。
    ///
    /// 为什么此前没被发现：所有既有流式测试都是**整行整块**喂进去的，永远切不断字符。
    /// 而真机上分段位置由网络决定，表现为「中文偶尔乱码、重试一次又好了」——
    /// 典型的没人能稳定复现、最后归咎于「上游抽风」的那类问题。
    ///
    /// 判据：逐字节喂入（最狠的切分）的输出，必须与整块喂入完全一致。
    #[test]
    fn sse_multibyte_text_survives_arbitrary_chunk_boundaries() {
        // 含中文的文本增量。注意「你好，世界」每个字都是 3 字节。
        let raw = "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"你好，世界\"}}\n\n";

        let whole = {
            let mut tr = SseTranslator::new(SseDirection::AnthropicToChat);
            let mut o = tr.push(raw.as_bytes());
            o.push_str(&tr.finish());
            o
        };

        let by_byte = {
            let mut tr = SseTranslator::new(SseDirection::AnthropicToChat);
            let mut o = String::new();
            for b in raw.as_bytes() {
                o.push_str(&tr.push(&[*b]));
            }
            o.push_str(&tr.finish());
            o
        };

        assert!(whole.contains("你好，世界"), "整块喂入本就应该是好的：\n{whole}");
        assert!(
            !by_byte.contains('\u{FFFD}'),
            "逐字节喂入出现了替换字符 U+FFFD —— 中文被 TCP 分段切开后腐蚀了：\n{by_byte}"
        );
        assert_eq!(by_byte, whole, "翻译结果必须与字节切分方式无关");
    }

    /// 把 SSE 文本里的所有 `data:` 行解析成 JSON 值，便于对结构做精确断言
    /// （避免用子串匹配蒙对——子串能被无关字段碰巧满足）。
    fn sse_events(raw: &str) -> Vec<Value> {
        raw.lines()
            .filter_map(|l| l.trim().strip_prefix("data:"))
            .map(str::trim)
            .filter(|d| !d.is_empty() && *d != "[DONE]")
            .filter_map(|d| serde_json::from_str::<Value>(d).ok())
            .collect()
    }

    /// 取出 Anthropic 流里所有 `tool_use` 型 content_block_start 的 (index, id, name)。
    fn anthropic_tool_blocks(raw: &str) -> Vec<(u64, String, String)> {
        sse_events(raw)
            .into_iter()
            .filter(|e| e.get("type").and_then(|t| t.as_str()) == Some("content_block_start"))
            .filter_map(|e| {
                let b = e.get("content_block")?;
                if b.get("type").and_then(|t| t.as_str()) != Some("tool_use") {
                    return None;
                }
                Some((
                    e.get("index").and_then(|i| i.as_u64()).unwrap_or(u64::MAX),
                    b.get("id").and_then(|i| i.as_str()).unwrap_or("").to_string(),
                    b.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string(),
                ))
            })
            .collect()
    }

    #[test]
    fn sse_responses_to_anthropic_translates_tool_call() {
        // 2026-07-30 实机根因回归：Claude 桌面端（Anthropic 下游）转到 Responses 上游时，
        // 上游的 function_call item 必须翻成 Anthropic 的 tool_use 块，否则桌面端只收到文本，
        // MCP 工具永远等不到 tools/call。
        let mut tr = SseTranslator::new(SseDirection::ResponsesToAnthropic);
        let mut out = String::new();
        out.push_str(&tr.push(b"event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"model\":\"gpt-5.6\"}}\n\n"));
        out.push_str(&tr.push(b"event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"let me ask\"}\n\n"));
        out.push_str(&tr.push(
            br#"event: response.output_item.done
data: {"type":"response.output_item.done","item":{"type":"function_call","id":"fc_1","call_id":"call_abc","name":"synaroute_ai","arguments":"{\"prompt\":\"hi\"}","status":"completed"}}

"#,
        ));
        out.push_str(&tr.push(b"event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":11,\"output_tokens\":7}}}\n\n"));
        out.push_str(&tr.finish());

        let blocks = anthropic_tool_blocks(&out);
        assert_eq!(blocks.len(), 1, "应恰好一个 tool_use 块:\n{out}");
        let (idx, id, name) = &blocks[0];
        assert_eq!(name, "synaroute_ai");
        assert_eq!(id, "call_abc", "tool_use.id 须用上游 call_id，工具结果才能回配");
        assert_eq!(*idx, 1, "text 块占 0，tool_use 应排到 1");

        // 参数以 input_json_delta 承载，且是可解析的 JSON。
        let arg_delta = sse_events(&out)
            .into_iter()
            .find(|e| {
                e.get("delta").and_then(|d| d.get("type")).and_then(|t| t.as_str())
                    == Some("input_json_delta")
            })
            .expect("缺 input_json_delta");
        let pj = arg_delta
            .get("delta")
            .and_then(|d| d.get("partial_json"))
            .and_then(|p| p.as_str())
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(pj).unwrap(),
            json!({ "prompt": "hi" }),
            "参数须原样送达"
        );

        // 有工具调用 → stop_reason 必须是 tool_use，否则下游认为本轮已结束、不执行工具。
        assert!(
            out.contains("\"stop_reason\":\"tool_use\""),
            "有工具调用应 tool_use，实际:\n{out}"
        );
        assert!(out.contains("\"output_tokens\":7"), "usage 应从 completed 归位:\n{out}");
    }

    #[test]
    fn sse_responses_to_anthropic_dedups_tool_call_from_completed() {
        // 同一个工具调用既出现在 output_item.done、又出现在 completed.output[] 时只能翻一次，
        // 否则下游会执行两遍（重复副作用）。
        let mut tr = SseTranslator::new(SseDirection::ResponsesToAnthropic);
        let mut out = String::new();
        out.push_str(&tr.push(
            br#"event: response.output_item.done
data: {"type":"response.output_item.done","item":{"type":"function_call","call_id":"c1","name":"t","arguments":"{}"}}

"#,
        ));
        out.push_str(&tr.push(
            br#"event: response.completed
data: {"type":"response.completed","response":{"output":[{"type":"function_call","call_id":"c1","name":"t","arguments":"{}"}]}}

"#,
        ));
        out.push_str(&tr.finish());
        assert_eq!(anthropic_tool_blocks(&out).len(), 1, "重复投递应去重:\n{out}");
    }

    #[test]
    fn sse_responses_to_anthropic_recovers_tool_call_only_from_completed() {
        // 上游只在 completed.output[] 给工具调用（不发独立 output_item.done）时也要能捞到。
        let mut tr = SseTranslator::new(SseDirection::ResponsesToAnthropic);
        let mut out = String::new();
        out.push_str(&tr.push(
            br#"event: response.completed
data: {"type":"response.completed","response":{"output":[{"type":"function_call","call_id":"c9","name":"only_in_completed","arguments":"{\"a\":1}"}]}}

"#,
        ));
        out.push_str(&tr.finish());
        let blocks = anthropic_tool_blocks(&out);
        assert_eq!(blocks.len(), 1, "completed 兜底应捞出工具调用:\n{out}");
        assert_eq!(blocks[0].2, "only_in_completed");
        assert!(out.contains("\"stop_reason\":\"tool_use\""));
    }

    #[test]
    fn sse_responses_to_anthropic_rejoins_namespaced_tool_name() {
        // Codex 范式的 {name, namespace} 两字段 → Anthropic 下游认全名，须拼回。
        let mut tr = SseTranslator::new(SseDirection::ResponsesToAnthropic);
        let mut out = String::new();
        out.push_str(&tr.push(
            br#"event: response.output_item.done
data: {"type":"response.output_item.done","item":{"type":"function_call","call_id":"c2","name":"synaroute_ai","namespace":"mcp__synaroute","arguments":"{}"}}

"#,
        ));
        out.push_str(&tr.finish());
        assert_eq!(
            anthropic_tool_blocks(&out)[0].2,
            "mcp__synaroute__synaroute_ai",
            "须拼回全名:\n{out}"
        );
    }

    #[test]
    fn sse_responses_to_anthropic_wraps_custom_tool_input() {
        // custom_tool_call 的裸字符串 input 要包成 {"input": …}，因 Anthropic 的 tool_use.input
        // 必须是 JSON 对象；无参调用要兜底 "{}"（空串会让下游 JSON.parse 失败）。
        let mut tr = SseTranslator::new(SseDirection::ResponsesToAnthropic);
        let mut out = String::new();
        out.push_str(&tr.push(
            br#"event: response.output_item.done
data: {"type":"response.output_item.done","item":{"type":"custom_tool_call","call_id":"c3","name":"apply_patch","input":"*** Begin Patch"}}

"#,
        ));
        out.push_str(&tr.push(
            br#"event: response.output_item.done
data: {"type":"response.output_item.done","item":{"type":"function_call","call_id":"c4","name":"no_args"}}

"#,
        ));
        out.push_str(&tr.finish());
        let deltas: Vec<String> = sse_events(&out)
            .into_iter()
            .filter_map(|e| {
                let d = e.get("delta")?;
                if d.get("type").and_then(|t| t.as_str()) != Some("input_json_delta") {
                    return None;
                }
                Some(d.get("partial_json")?.as_str()?.to_string())
            })
            .collect();
        assert_eq!(deltas.len(), 2, "两个工具各一段参数:\n{out}");
        assert_eq!(
            serde_json::from_str::<Value>(&deltas[0]).unwrap(),
            json!({ "input": "*** Begin Patch" }),
            "custom 裸串须包成对象"
        );
        assert_eq!(deltas[1], "{}", "无参工具须兜底空对象，不能是空串");
    }

    #[test]
    fn sse_chat_to_anthropic_translates_tool_call_deltas() {
        // Chat 上游 → Anthropic 下游：tool_calls 是分片增量（name/arguments 逐块到达），
        // 须累积后成块发出，且 stop_reason 改 tool_use。
        let mut tr = SseTranslator::new(SseDirection::ChatToAnthropic);
        let mut out = String::new();
        out.push_str(&tr.push(b"data: {\"model\":\"glm-4.6\",\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\n"));
        out.push_str(&tr.push(br#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_x","function":{"name":"synaroute_ai","arguments":"{\"pro"}}]}}]}

"#));
        out.push_str(&tr.push(br#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"mpt\":\"hi\"}"}}]}}]}

"#));
        out.push_str(&tr.push(b"data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n"));
        out.push_str(&tr.finish());

        let blocks = anthropic_tool_blocks(&out);
        assert_eq!(blocks.len(), 1, "应恰好一个 tool_use 块:\n{out}");
        assert_eq!(blocks[0].2, "synaroute_ai");
        assert_eq!(blocks[0].1, "call_x");
        let pj = sse_events(&out)
            .into_iter()
            .find_map(|e| {
                let d = e.get("delta")?;
                if d.get("type").and_then(|t| t.as_str()) != Some("input_json_delta") {
                    return None;
                }
                Some(d.get("partial_json")?.as_str()?.to_string())
            })
            .expect("缺 input_json_delta");
        assert_eq!(
            serde_json::from_str::<Value>(&pj).unwrap(),
            json!({ "prompt": "hi" }),
            "分片参数须拼完整"
        );
        assert!(out.contains("\"stop_reason\":\"tool_use\""), "应 tool_use:\n{out}");
    }

    #[test]
    fn sse_chat_to_anthropic_flushes_tool_calls_without_finish_reason() {
        // 上游没给 finish_reason 就断流：收尾时必须兜底冲刷累积的工具调用，不能整个丢掉。
        let mut tr = SseTranslator::new(SseDirection::ChatToAnthropic);
        let mut out = String::new();
        out.push_str(&tr.push(br#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c5","function":{"name":"t","arguments":"{}"}}]}}]}

"#));
        out.push_str(&tr.finish());
        assert_eq!(anthropic_tool_blocks(&out).len(), 1, "断流也应交付工具:\n{out}");
        assert!(out.contains("\"stop_reason\":\"tool_use\""));
    }

    #[test]
    fn sse_anthropic_to_chat_translates_tool_use() {
        // Anthropic 上游 → Chat 下游：tool_use 块 → delta.tool_calls 增量，finish_reason 改 tool_calls。
        let mut tr = SseTranslator::new(SseDirection::AnthropicToChat);
        let mut out = String::new();
        out.push_str(&tr.push(br#"event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"synaroute_ai","input":{}}}

"#));
        out.push_str(&tr.push(br#"event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"prompt\":\"hi\"}"}}

"#));
        out.push_str(&tr.push(b"event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"));
        out.push_str(&tr.finish());

        let evs = sse_events(&out);
        let first = evs
            .iter()
            .find_map(|e| e.pointer("/choices/0/delta/tool_calls/0"))
            .expect("缺 tool_calls 增量");
        assert_eq!(first.pointer("/function/name").unwrap(), &json!("synaroute_ai"));
        assert_eq!(first.get("id").unwrap(), &json!("toolu_1"));
        // 参数分片拼起来须是完整 JSON。
        let args: String = evs
            .iter()
            .filter_map(|e| {
                e.pointer("/choices/0/delta/tool_calls/0/function/arguments")?
                    .as_str()
                    .map(str::to_string)
            })
            .collect();
        assert_eq!(
            serde_json::from_str::<Value>(&args).unwrap(),
            json!({ "prompt": "hi" })
        );
        assert!(
            out.contains("\"finish_reason\":\"tool_calls\""),
            "有工具应 tool_calls:\n{out}"
        );
    }

    #[test]
    fn sse_responses_to_chat_translates_tool_call() {
        // Responses 上游 → Chat 下游：function_call item → delta.tool_calls。
        let mut tr = SseTranslator::new(SseDirection::ResponsesToChat);
        let mut out = String::new();
        out.push_str(&tr.push(b"event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"hm\"}\n\n"));
        out.push_str(&tr.push(
            br#"event: response.output_item.done
data: {"type":"response.output_item.done","item":{"type":"function_call","call_id":"c7","name":"synaroute_ai","arguments":"{\"prompt\":\"x\"}"}}

"#,
        ));
        out.push_str(&tr.push(b"event: response.completed\ndata: {\"type\":\"response.completed\"}\n\n"));
        out.push_str(&tr.finish());

        let tc = sse_events(&out)
            .into_iter()
            .find_map(|e| e.pointer("/choices/0/delta/tool_calls/0").cloned())
            .expect("缺 tool_calls:\n");
        assert_eq!(tc.pointer("/function/name").unwrap(), &json!("synaroute_ai"));
        assert_eq!(tc.get("id").unwrap(), &json!("c7"));
        assert!(
            out.contains("\"finish_reason\":\"tool_calls\""),
            "有工具应 tool_calls:\n{out}"
        );
    }

    #[test]
    fn openai_resp_to_anthropic_carries_tool_calls() {
        // 非流式同口径：Chat 响应的 tool_calls 必须变成 Anthropic 的 tool_use 块。
        // 早期实现只搬文本却报 stop_reason:"tool_use" → 自相矛盾，下游无工具可执行。
        let body = json!({
            "id": "chatcmpl-1",
            "model": "gpt-5.6",
            "choices": [ {
                "finish_reason": "tool_calls",
                "message": {
                    "role": "assistant",
                    "content": "查一下",
                    "tool_calls": [ {
                        "id": "call_z",
                        "type": "function",
                        "function": { "name": "synaroute_ai", "arguments": "{\"prompt\":\"hi\"}" }
                    } ]
                }
            } ],
            "usage": { "prompt_tokens": 3, "completion_tokens": 4 }
        });
        let out = openai_resp_to_anthropic(&body);
        assert_eq!(out["stop_reason"], json!("tool_use"));
        let blocks = out["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 2, "文本 + 工具各一块: {out}");
        assert_eq!(blocks[0]["type"], json!("text"));
        assert_eq!(blocks[1]["type"], json!("tool_use"));
        assert_eq!(blocks[1]["id"], json!("call_z"));
        assert_eq!(blocks[1]["name"], json!("synaroute_ai"));
        // input 必须是**对象**（不是 JSON 字符串），否则下游 schema 校验失败。
        assert_eq!(blocks[1]["input"], json!({ "prompt": "hi" }));
    }

    #[test]
    fn openai_resp_to_anthropic_tool_only_has_no_empty_text_block() {
        // 纯工具调用（content 为 null）：不应塞入空文本块，只留 tool_use。
        let body = json!({
            "choices": [ {
                "finish_reason": "tool_calls",
                "message": {
                    "role": "assistant",
                    "content": Value::Null,
                    "tool_calls": [ {
                        "id": "c8", "type": "function",
                        "function": { "name": "t", "arguments": "not json" }
                    } ]
                }
            } ]
        });
        let out = openai_resp_to_anthropic(&body);
        let blocks = out["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 1, "不应有空文本块: {out}");
        assert_eq!(blocks[0]["type"], json!("tool_use"));
        // arguments 不是合法 JSON 对象时兜底空对象，不能把裸串塞进 input。
        assert_eq!(blocks[0]["input"], json!({}), "非法参数须兜底空对象");
    }

    #[test]
    fn anthropic_resp_to_openai_carries_tool_use() {
        // 反方向非流式：Anthropic 的 tool_use 块 → Chat 的 tool_calls。
        let body = json!({
            "id": "msg_1",
            "model": "claude-opus-4-7",
            "stop_reason": "tool_use",
            "content": [
                { "type": "text", "text": "稍等" },
                { "type": "tool_use", "id": "toolu_9", "name": "synaroute_ai", "input": { "prompt": "hi" } }
            ],
            "usage": { "input_tokens": 5, "output_tokens": 6 }
        });
        let out = anthropic_resp_to_openai(&body);
        assert_eq!(out["choices"][0]["finish_reason"], json!("tool_calls"));
        let tc = &out["choices"][0]["message"]["tool_calls"][0];
        assert_eq!(tc["id"], json!("toolu_9"));
        assert_eq!(tc["function"]["name"], json!("synaroute_ai"));
        // arguments 必须是 JSON **字符串**（Chat 语义），内容可解析回原对象。
        let args = tc["function"]["arguments"].as_str().expect("arguments 须为字符串");
        assert_eq!(
            serde_json::from_str::<Value>(args).unwrap(),
            json!({ "prompt": "hi" })
        );
        assert_eq!(out["choices"][0]["message"]["content"], json!("稍等"));
    }

    #[test]
    fn anthropic_resp_to_openai_tool_only_nulls_content() {
        // 纯工具调用：Chat 语义下 content 为 null（而非空串），避免下游把空串当成回答。
        let body = json!({
            "stop_reason": "tool_use",
            "content": [ { "type": "tool_use", "id": "t1", "name": "f", "input": {} } ]
        });
        let out = anthropic_resp_to_openai(&body);
        assert_eq!(out["choices"][0]["message"]["content"], Value::Null);
        assert_eq!(out["choices"][0]["message"]["tool_calls"][0]["id"], json!("t1"));
    }

    #[test]
    fn tool_calls_survive_full_roundtrip_both_ways() {
        // 端到端口径一致：Anthropic 下游 ↔ Chat 上游往返后，工具名与参数不失真。
        // 这条锁住「两个方向的非流式转换互为逆运算」，防止只修一侧造成新的单向丢失。
        let anthropic = json!({
            "stop_reason": "tool_use",
            "content": [
                { "type": "text", "text": "hi" },
                { "type": "tool_use", "id": "t1", "name": "mcp__synaroute__synaroute_ai",
                  "input": { "prompt": "p", "n": 2 } }
            ]
        });
        let back = openai_resp_to_anthropic(&anthropic_resp_to_openai(&anthropic));
        assert_eq!(back["stop_reason"], json!("tool_use"));
        let tu = back["content"]
            .as_array()
            .unwrap()
            .iter()
            .find(|b| b["type"] == json!("tool_use"))
            .expect("往返后工具块丢失");
        assert_eq!(tu["name"], json!("mcp__synaroute__synaroute_ai"));
        assert_eq!(tu["input"], json!({ "prompt": "p", "n": 2 }));
        assert_eq!(tu["id"], json!("t1"), "call id 须守恒，否则工具结果回配不上");
    }

    #[test]
    fn collect_custom_tools_finds_custom_type() {
        let body = json!({
            "tools": [
                { "type": "custom", "name": "apply_patch" },
                { "type": "function", "name": "read_file" },
                { "type": "namespace", "name": "mcp__x" },
                { "type": "custom", "name": "exec" },
            ]
        });
        let result = collect_custom_tools(&body);
        assert_eq!(result.len(), 2);
        assert!(result.contains("apply_patch"));
        assert!(result.contains("exec"));
        assert!(!result.contains("read_file"));
    }

    /// Codex 桌面端（26.x / gpt-5.6 系）真实请求骨架：顶层**没有** `tools`，工具全在
    /// `input[0] = {"type":"additional_tools","role":"developer","tools":[…]}`。
    /// 抓包来源：`~/.codex/logs_2.sqlite` 的 `codex_http_client::transport` 行（2026-07-30）。
    fn codex_desktop_request() -> Value {
        json!({
            "model": "gpt-5.6-sol",
            "tool_choice": "auto",
            "parallel_tool_calls": false,
            "input": [
                {
                    "type": "additional_tools",
                    "role": "developer",
                    "tools": [
                        {
                            "type": "custom",
                            "name": "exec",
                            "description": "Run JavaScript code to orchestrate/compose tool calls",
                            "format": { "type": "grammar", "syntax": "lark", "definition": "start: SOURCE" }
                        },
                        {
                            "type": "function",
                            "name": "wait",
                            "description": "Wait for a running exec cell.",
                            "parameters": { "type": "object", "properties": { "cell_id": { "type": "string" } } }
                        },
                        {
                            "type": "namespace",
                            "name": "collaboration",
                            "description": "Agent collaboration",
                            "tools": [
                                {
                                    "type": "function",
                                    "name": "spawn_agent",
                                    "description": "Spawn a sub-agent.",
                                    "parameters": { "type": "object", "properties": { "task": { "type": "string" } } }
                                }
                            ]
                        }
                    ]
                },
                { "type": "message", "role": "developer", "content": [{ "type": "input_text", "text": "系统提示" }] },
                { "type": "message", "role": "user", "content": [{ "type": "input_text", "text": "读一下这个文件" }] }
            ]
        })
    }

    #[test]
    fn collect_declared_tools_hoists_additional_tools_item() {
        // 根因回归护栏：顶层无 tools 时必须从 additional_tools 项里取。
        // 若退回只读顶层 `tools`，这里立即变红——那正是「Codex 桌面端工具/MCP 全调不起来」的成因。
        let body = codex_desktop_request();
        assert!(body.get("tools").is_none(), "夹具前提：顶层不应有 tools");
        let declared = collect_declared_tools(&body);
        let names: Vec<&str> = declared
            .iter()
            .filter_map(|t| t.get("name").and_then(|n| n.as_str()))
            .collect();
        assert_eq!(names, vec!["exec", "wait", "collaboration"], "三个声明工具都要收到");
    }

    #[test]
    fn collect_declared_tools_merges_top_level_and_additional() {
        // 两种承载并存时都收，且顶层在前（保持既有客户端行为与顺序不变）。
        let body = json!({
            "tools": [{ "type": "function", "name": "top_level_fn" }],
            "input": [
                { "type": "additional_tools", "role": "developer", "tools": [{ "type": "custom", "name": "exec" }] },
                { "type": "message", "role": "user", "content": [{ "type": "input_text", "text": "hi" }] }
            ]
        });
        let names: Vec<String> = collect_declared_tools(&body)
            .iter()
            .filter_map(|t| t.get("name").and_then(|n| n.as_str()).map(|s| s.to_string()))
            .collect();
        assert_eq!(names, vec!["top_level_fn", "exec"]);
    }

    #[test]
    fn custom_and_namespace_collectors_see_additional_tools() {
        // 响应侧两个收集器也必须覆盖 additional_tools：
        // - custom 集合缺 exec → 回程发 function_call + JSON arguments，Codex router 认不出；
        // - namespace 列表缺 collaboration → 子工具全名拆不回 {namespace,name}，报 unsupported call。
        let body = codex_desktop_request();
        let custom = collect_custom_tools(&body);
        assert!(custom.contains("exec"), "exec 必须被识别为 custom 工具");
        let ns = collect_tool_namespaces(&body);
        assert_eq!(ns, vec!["collaboration"], "namespace 必须被收集");
    }

    #[test]
    fn responses_to_chat_converts_codex_desktop_additional_tools() {
        // 端到端（请求侧）：桌面端形态请求转 Chat 后必须带上三个工具，
        // 且 additional_tools 项不得残留成一条空 developer 消息。
        let chat = responses_to_chat(&codex_desktop_request());
        let tools = chat["tools"].as_array().expect("转换后必须有 tools");
        let names: Vec<&str> = tools
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            vec!["exec", "wait", "collaboration__spawn_agent"],
            "custom 原名保留、function 原名保留、namespace 子工具展开为 <ns>__<sub>"
        );

        // freeform custom 工具（exec 只有 format、无 schema）须拿到单字符串入参 input，
        // 否则模型没有地方放要执行的代码。
        let exec_params = &tools[0]["function"]["parameters"];
        assert_eq!(exec_params["type"], "object");
        assert_eq!(
            exec_params["properties"]["input"]["type"], "string",
            "freeform custom 工具应兜底 {{input:string}} schema"
        );

        // 消息：只应有 developer 系统提示 + user 两条，没有空壳消息。
        let msgs = chat["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 2, "additional_tools 项不应残留成消息: {msgs:?}");
        assert!(
            !msgs.iter().any(|m| m["content"].as_str() == Some("")),
            "不应出现空 content 消息"
        );
    }

    #[test]
    fn convert_request_codex_desktop_to_anthropic_carries_tools() {
        // 真正发往上游（Anthropic 主 Key，如 opus）的请求必须带 tools —— 此前为空，
        // 模型自述「没有可调用的工具 schema」，于 Codex 里表现为工具与 MCP 全调不起来。
        let out = convert_request(
            &codex_desktop_request(),
            Protocol::OpenaiResponses,
            Protocol::Anthropic,
        );
        let tools = out["tools"].as_array().expect("Anthropic 请求必须带 tools");
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names, vec!["exec", "wait", "collaboration__spawn_agent"]);
        assert_eq!(
            tools[0]["input_schema"]["properties"]["input"]["type"], "string",
            "exec 的裸文本载荷要有 input 字符串入参"
        );
    }

    /// Codex direct 模式（`tool_mode: null` 的模型，如 gpt-5.5）真实请求骨架。
    /// 抓包来源：`~/.codex/logs_2.sqlite` 的 `codex_http_client::transport` 行（2026-07-30）。
    ///
    /// 两个关键实证：
    /// 1. `tool_search` 声明**没有 `name` 字段**（名字即 type），`execution:"client"`；
    /// 2. MCP 工具（`mcp__*` namespace）**从不出现在顶层 tools**——59 条含 `mcp__synaroute`
    ///    的抓包请求里顶层命中数为 0；它只在 `tool_search_output.tools[]` 里回灌。
    fn codex_direct_request_with_search() -> Value {
        json!({
            "model": "gpt-5.5",
            "tools": [
                {
                    "type": "function",
                    "name": "shell_command",
                    "description": "Run a shell command.",
                    "parameters": { "type": "object", "properties": { "cmd": { "type": "string" } } }
                },
                {
                    "type": "tool_search",
                    "execution": "client",
                    "description": "# Tool discovery\n\nSearches over deferred tool metadata with BM25.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "query": { "type": "string", "description": "Search query for deferred tools." },
                            "limit": { "type": "number", "description": "Maximum number of tools to return." }
                        },
                        "required": ["query"],
                        "additionalProperties": false
                    }
                },
                { "type": "web_search", "external_web_access": false, "search_content_types": ["text"] }
            ],
            "input": [
                { "type": "message", "role": "user", "content": [{ "type": "input_text", "text": "用 synaroute_ai 审查代码" }] },
                {
                    "type": "tool_search_call",
                    "id": "tsc_abc",
                    "call_id": "call_search1",
                    "status": "completed",
                    "execution": "client",
                    "arguments": { "query": "synaroute_ai 多模型会诊", "limit": 8 }
                },
                {
                    "type": "tool_search_output",
                    "id": "tso_xyz",
                    "call_id": "call_search1",
                    "status": "completed",
                    "execution": "client",
                    "tools": [{
                        "type": "namespace",
                        "name": "mcp__synaroute",
                        "description": "Tools in the mcp__synaroute namespace.",
                        "tools": [{
                            "type": "function",
                            "name": "synaroute_ai",
                            "description": "调用 SynaRoute 多模型大脑聚合",
                            "defer_loading": true,
                            "parameters": {
                                "type": "object",
                                "properties": { "prompt": { "type": "string" }, "category": { "type": "string" } },
                                "required": ["prompt"]
                            }
                        }]
                    }]
                }
            ]
        })
    }

    #[test]
    fn tool_search_is_exposed_despite_having_no_name() {
        // 根因护栏一：`tool_search` 声明无 `name` 字段。请求侧此前一律
        // `let Some(name) = t.get("name") else { continue }` 跳过它 → 模型不知道有检索器
        // → 永远发不出 tool_search_call → 延迟加载的 MCP 工具永远解锁不了。
        let chat = responses_to_chat(&codex_direct_request_with_search());
        let names: Vec<&str> = chat["tools"]
            .as_array()
            .expect("必须有 tools")
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap())
            .collect();
        assert!(
            names.contains(&"tool_search"),
            "tool_search 必须暴露给上游模型（名字取自 type），实际: {names:?}"
        );
        // schema 要带过去，否则模型不知道要传 query。
        let ts = chat["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["function"]["name"] == "tool_search")
            .unwrap();
        assert_eq!(ts["function"]["parameters"]["properties"]["query"]["type"], "string");
        // web_search 是服务商侧执行的内置工具，经 SynaRoute 到 Anthropic 上游无人能执行，
        // 刻意不暴露（否则诱导模型空调）。
        assert!(
            !names.contains(&"web_search"),
            "web_search 不应暴露（无执行方），实际: {names:?}"
        );
    }

    #[test]
    fn mcp_tools_hoisted_from_tool_search_output() {
        // 根因护栏二：MCP 工具的真 schema **只**在 tool_search_output.tools[] 里回灌，
        // 顶层 tools 永远没有 mcp__*。不提升这一处，模型即使检索过，下一轮依旧看不到
        // synaroute_ai —— 正是「MCP 服务端握手正常、模型坚称没这个工具」的成因。
        let body = codex_direct_request_with_search();
        let top_names: Vec<String> = body["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t.get("name").and_then(|n| n.as_str()).map(String::from))
            .collect();
        assert!(
            !top_names.iter().any(|n| n.contains("synaroute")),
            "夹具前提：顶层 tools 不含 synaroute"
        );

        let chat = responses_to_chat(&body);
        let names: Vec<&str> = chat["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap())
            .collect();
        assert!(
            names.contains(&"mcp__synaroute__synaroute_ai"),
            "namespace 子工具应展开为全名并暴露，实际: {names:?}"
        );
        // namespace 也要被收集，否则回程拆不回 {namespace, name} → Codex router 报 unsupported call。
        assert_eq!(collect_tool_namespaces(&body), vec!["mcp__synaroute"]);
        // 检索器本身也在集合里，供响应侧判定回程 item type。
        assert!(collect_search_tools(&body).contains("tool_search"));
    }

    #[test]
    fn tool_search_history_preserved_as_tool_calls() {
        // 根因护栏三：tool_search_call / tool_search_output 此前 type 未知 → 落默认分支 →
        // 取不存在的 content → 变成空消息。模型看不到自己检索过什么，会反复同义重复检索
        // （实测同一会话 5 次同义查询）。
        let chat = responses_to_chat(&codex_direct_request_with_search());
        let msgs = chat["messages"].as_array().unwrap();

        // 检索调用还原为 assistant.tool_calls，arguments 从对象序列化成 JSON 字符串
        // （Chat 协议要求字符串；该 item 上原本是对象）。
        let call = msgs
            .iter()
            .find(|m| m["tool_calls"][0]["function"]["name"] == "tool_search")
            .expect("缺 tool_search 调用消息");
        assert_eq!(call["tool_calls"][0]["id"], "call_search1");
        let args = call["tool_calls"][0]["function"]["arguments"]
            .as_str()
            .expect("arguments 必须是 JSON 字符串");
        let parsed: Value = serde_json::from_str(args).expect("arguments 应可解析");
        assert_eq!(parsed["query"], "synaroute_ai 多模型会诊");

        // 检索结果给一条 role:"tool" 回执，列出命中的工具全名（该 item 无 output 字段）。
        let out = msgs
            .iter()
            .find(|m| m["role"] == "tool" && m["tool_call_id"] == "call_search1")
            .expect("缺 tool_search 结果回执");
        let text = out["content"].as_str().unwrap();
        assert!(
            text.contains("mcp__synaroute__synaroute_ai"),
            "回执应列出命中的工具全名，实际: {text}"
        );

        // 不得残留空 content 消息（旧行为的症状）。
        assert!(
            !msgs.iter().any(|m| m["content"].as_str() == Some("")),
            "不应出现空 content 消息: {msgs:?}"
        );
    }

    #[test]
    fn declared_tools_dedup_across_repeated_search_outputs() {
        // 多轮里 Codex 会反复回灌同一批工具（每轮一份 tool_search_output）。
        // 不去重则同名工具在 tools 里出现 N 份：白烧 token，且可能触发上游「重复工具名」校验失败。
        let mut body = codex_direct_request_with_search();
        let dup = body["input"][2].clone();
        body["input"].as_array_mut().unwrap().push(dup);

        let chat = responses_to_chat(&body);
        let names: Vec<&str> = chat["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap())
            .collect();
        let hits = names.iter().filter(|n| **n == "mcp__synaroute__synaroute_ai").count();
        assert_eq!(hits, 1, "同名工具应去重，实际出现 {hits} 次: {names:?}");
        // namespace 列表同样不应重复。
        assert_eq!(collect_tool_namespaces(&body), vec!["mcp__synaroute"]);
    }

    #[test]
    fn non_stream_rewrites_tool_search_call() {
        // 回程（非流式）：模型对 tool_search 的调用必须改写成 tool_search_call，
        // 且 arguments 变回**对象**、带 execution:"client"、去掉 name、id 用 tsc_ 前缀。
        // 否则 Codex 认不出，本地 BM25 检索发不起来 → MCP 工具永远拿不到 schema。
        let body = json!({
            "choices": [{ "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{ "id": "tc_s", "type": "function", "function": {
                    "name": "tool_search",
                    "arguments": "{\"query\":\"synaroute_ai\",\"limit\":8}"
                } }]
            }, "finish_reason": "tool_calls" }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 1 }
        });
        let search = std::collections::HashSet::from(["tool_search".to_string()]);
        let result = chat_resp_to_responses_ext(&body, &Default::default(), &search);
        let item = &result["output"][0];
        assert_eq!(item["type"], "tool_search_call", "应改写为 tool_search_call");
        assert_eq!(item["execution"], "client", "须标明客户端执行");
        assert!(item.get("name").is_none(), "tool_search_call 不应带 name");
        assert!(item["arguments"].is_object(), "arguments 必须是对象而非字符串");
        assert_eq!(item["arguments"]["query"], "synaroute_ai");
        assert_eq!(item["arguments"]["limit"], 8);
        assert_eq!(item["call_id"], "tc_s", "call_id 须保留以回配结果");
        assert!(
            item["id"].as_str().unwrap().starts_with("tsc_"),
            "id 应用 Codex 同款 tsc_ 前缀，实际 {}", item["id"]
        );
    }

    #[test]
    fn non_stream_tool_search_falls_back_on_unparsable_arguments() {
        // 模型没按 schema 回 JSON（回了裸文本）时，不能静默丢掉调用：
        // 退化成 {"query": 原文}，宁可查得糙也要让检索跑起来。
        let body = json!({
            "choices": [{ "message": {
                "role": "assistant", "content": null,
                "tool_calls": [{ "id": "tc_s2", "type": "function", "function": {
                    "name": "tool_search", "arguments": "synaroute_ai"
                } }]
            }, "finish_reason": "tool_calls" }]
        });
        let search = std::collections::HashSet::from(["tool_search".to_string()]);
        let result = chat_resp_to_responses_ext(&body, &Default::default(), &search);
        let item = &result["output"][0];
        assert_eq!(item["type"], "tool_search_call");
        assert_eq!(item["arguments"]["query"], "synaroute_ai", "不可解析时退化为 query 原文");
    }

    #[test]
    fn sse_rewrites_tool_search_call() {
        // 流式与非流式必须同口径（共用 rewrite_to_tool_search_call）：
        // Codex 只在 response.output_item.done 里执行工具，故流式路径漏改写等于没修。
        let search = std::collections::HashSet::from(["tool_search".to_string()]);
        let mut tr = SseTranslator::with_namespaces_and_custom(
            SseDirection::AnthropicToResponses,
            vec![],
            Default::default(),
            search,
        );
        let mut out = String::new();
        out.push_str(&tr.push(b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"model\":\"claude-opus-5\"}}\n\n"));
        out.push_str(&tr.push(b"event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_s\",\"name\":\"tool_search\",\"input\":{}}}\n\n"));
        out.push_str(&tr.push(b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"query\\\":\\\"synaroute_ai\\\"}\"}}\n\n"));
        out.push_str(&tr.push(b"event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n"));
        out.push_str(&tr.push(b"event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"}}\n\n"));
        out.push_str(&tr.finish());
        assert!(out.contains("\"type\":\"tool_search_call\""), "流式应输出 tool_search_call:\n{out}");
        assert!(out.contains("\"execution\":\"client\""), "须标明客户端执行:\n{out}");
        assert!(out.contains("\"query\":\"synaroute_ai\""), "arguments 应为对象且含 query:\n{out}");
        // Codex 据 output_item.done 执行，必须走流式事件而非只塞 completed.output。
        assert!(out.contains("event: response.output_item.done"), "缺 output_item.done:\n{out}");
    }

    #[test]
    fn sse_non_search_tool_unaffected() {
        // 对照：普通 MCP 工具（namespace 展开的全名）仍走 function_call + arguments 字符串，
        // 且 name/namespace 正确拆分——不得被 tool_search 改写逻辑误伤。
        let mut tr = SseTranslator::with_namespaces_and_custom(
            SseDirection::AnthropicToResponses,
            vec!["mcp__synaroute".to_string()],
            Default::default(),
            std::collections::HashSet::from(["tool_search".to_string()]),
        );
        let mut out = String::new();
        out.push_str(&tr.push(b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"model\":\"claude-opus-5\"}}\n\n"));
        out.push_str(&tr.push(b"event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_m\",\"name\":\"mcp__synaroute__synaroute_ai\",\"input\":{}}}\n\n"));
        out.push_str(&tr.push(b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"prompt\\\":\\\"hi\\\"}\"}}\n\n"));
        out.push_str(&tr.push(b"event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n"));
        out.push_str(&tr.push(b"event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"}}\n\n"));
        out.push_str(&tr.finish());
        assert!(out.contains("\"type\":\"function_call\""), "普通工具应保持 function_call:\n{out}");
        assert!(!out.contains("tool_search_call"), "不应误改写:\n{out}");
        assert!(out.contains("\"name\":\"synaroute_ai\""), "name 应拆为子工具名:\n{out}");
        assert!(out.contains("\"namespace\":\"mcp__synaroute\""), "namespace 应独立字段:\n{out}");
    }

    #[test]
    fn convert_request_direct_mode_carries_mcp_tool_to_anthropic() {
        // 端到端：真正发往 Anthropic 上游（opus）的请求里必须同时有检索器与 MCP 工具。
        // 这是「opus 作 Codex 主 Key 能否用 MCP」的最终判据。
        let out = convert_request(
            &codex_direct_request_with_search(),
            Protocol::OpenaiResponses,
            Protocol::Anthropic,
        );
        let tools = out["tools"].as_array().expect("必须带 tools");
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"tool_search"), "缺检索器: {names:?}");
        assert!(
            names.contains(&"mcp__synaroute__synaroute_ai"),
            "缺 MCP 工具: {names:?}"
        );
        let syna = tools
            .iter()
            .find(|t| t["name"] == "mcp__synaroute__synaroute_ai")
            .unwrap();
        assert_eq!(
            syna["input_schema"]["properties"]["prompt"]["type"], "string",
            "MCP 工具的真 schema 要带到上游"
        );
    }

    // ---- 真实抓包回放（非手写夹具）----
    //
    // 上面的用例用手写夹具，风险是「把结构写成我以为的样子」。这里直接回放 Codex 真发出来的
    // 请求体：来自 `~/.codex/logs_2.sqlite` 的 `codex_http_client::transport` 行
    // （2026-07-30 实机会话，Codex 桌面端 26.721 / gpt-5.4 direct 模式）。
    // 仅截短了超长 description/text 文本，**所有字段名与结构一字未改**。
    // include_str! 保证内容不会被测试代码悄悄改写。
    const REAL_CODEX_CAPTURE: &str = include_str!("../testdata/codex_real_direct_request.json");

    fn real_codex_request() -> Value {
        serde_json::from_str(REAL_CODEX_CAPTURE).expect("真实抓包应为合法 JSON")
    }

    #[test]
    fn real_capture_matches_the_shape_we_claim() {
        // 先钉住「事实前提」：若 Codex 以后改了形态，这条先红，提醒重新抓包而非盲改逻辑。
        let body = real_codex_request();
        let tools = body["tools"].as_array().expect("顶层应有 tools");

        let ts = tools
            .iter()
            .find(|t| t["type"] == "tool_search")
            .expect("真实请求含 tool_search 声明");
        assert!(
            ts.get("name").is_none(),
            "前提：tool_search 声明无 name 字段（名字即 type），实际: {ts}"
        );
        assert_eq!(ts["execution"], "client", "前提：tool_search 由客户端执行");

        let top_names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
        assert!(
            !top_names.iter().any(|n| n.starts_with("mcp__")),
            "前提：MCP 工具从不出现在顶层 tools，实际: {top_names:?}"
        );

        let tso = body["input"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["type"] == "tool_search_output")
            .expect("真实请求含 tool_search_output");
        assert!(tso.get("output").is_none(), "前提：该 item 无 output 字段");
        let inner: Vec<&str> = tso["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["name"].as_str())
            .collect();
        assert!(
            inner.contains(&"mcp__synaroute"),
            "前提：MCP namespace 只在 tool_search_output 里，实际: {inner:?}"
        );
    }

    #[test]
    fn real_capture_delivers_search_and_mcp_tools_upstream() {
        // 核心验收（真实数据）：转换后发往上游的请求必须同时带检索器与 MCP 工具真 schema。
        let body = real_codex_request();

        let chat = responses_to_chat(&body);
        let names: Vec<&str> = chat["tools"]
            .as_array()
            .expect("转换后必须有 tools")
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"tool_search"), "真实数据下缺检索器: {names:?}");
        assert!(
            names.contains(&"mcp__synaroute__synaroute_ai"),
            "真实数据下缺从 tool_search_output 提升的 MCP 工具: {names:?}"
        );
        assert!(collect_search_tools(&body).contains("tool_search"));
        assert!(
            collect_tool_namespaces(&body).contains(&"mcp__synaroute".to_string()),
            "namespace 须收集，否则回程拆不回 {{namespace,name}}"
        );

        let up = convert_request(&body, Protocol::OpenaiResponses, Protocol::Anthropic);
        let up_tools = up["tools"].as_array().expect("上游请求必须带 tools");
        let up_names: Vec<&str> = up_tools.iter().filter_map(|t| t["name"].as_str()).collect();
        assert!(up_names.contains(&"tool_search"), "上游缺检索器: {up_names:?}");
        assert!(
            up_names.contains(&"mcp__synaroute__synaroute_ai"),
            "上游缺 MCP 工具: {up_names:?}"
        );
        let syna = up_tools
            .iter()
            .find(|t| t["name"] == "mcp__synaroute__synaroute_ai")
            .unwrap();
        assert_eq!(
            syna["input_schema"]["properties"]["prompt"]["type"], "string",
            "MCP 工具的真 schema（prompt 参数）要完整带到上游"
        );
        // apply_patch 是 freeform custom 工具（只有 format、无 schema）→ 兜底 {input:string}。
        let ap = up_tools.iter().find(|t| t["name"] == "apply_patch").unwrap();
        assert_eq!(ap["input_schema"]["properties"]["input"]["type"], "string");
    }

    #[test]
    fn real_capture_preserves_search_history() {
        // 检索调用与结果不得退化成空消息，否则模型反复同义重复检索（实测同会话 5 次）。
        let chat = responses_to_chat(&real_codex_request());
        let msgs = chat["messages"].as_array().unwrap();

        let call = msgs
            .iter()
            .find(|m| m["tool_calls"][0]["function"]["name"] == "tool_search")
            .expect("缺 tool_search 调用消息");
        let args = call["tool_calls"][0]["function"]["arguments"]
            .as_str()
            .expect("Chat 协议要求 arguments 为 JSON 字符串");
        let parsed: Value = serde_json::from_str(args).expect("arguments 应可解析");
        assert!(
            parsed["query"].as_str().unwrap().contains("synaroute_ai"),
            "检索 query 应保留，实际: {parsed}"
        );

        let receipt = msgs
            .iter()
            .find(|m| {
                m["role"] == "tool"
                    && m["content"]
                        .as_str()
                        .map(|c| c.contains("mcp__synaroute__synaroute_ai"))
                        .unwrap_or(false)
            })
            .expect("缺检索结果回执（应列出命中的工具全名）");
        assert!(receipt["tool_call_id"].is_string());

        assert!(
            !msgs.iter().any(|m| m["content"].as_str() == Some("")),
            "不应出现空 content 消息（旧行为症状）"
        );
    }

    // ---- namespace 全名往返（unsupported call 根因）----

    #[test]
    fn join_namespaced_name_rejoins_two_fields() {
        // Codex 历史里 MCP 工具存成 {name, namespace} 两字段，须拼回上游模型看到的全名。
        assert_eq!(
            join_namespaced_tool_name(&json!({ "name": "synaroute_ai", "namespace": "mcp__synaroute" })),
            "mcp__synaroute__synaroute_ai"
        );
        // 无 namespace（平铺工具，如 Codex 内置 update_plan）：原样返回。
        assert_eq!(
            join_namespaced_tool_name(&json!({ "name": "update_plan" })),
            "update_plan"
        );
        // 空 namespace 视作无。
        assert_eq!(
            join_namespaced_tool_name(&json!({ "name": "foo", "namespace": "" })),
            "foo"
        );
        // 已是全名时不得重复拼接（否则 mcp__x__mcp__x__foo）。
        assert_eq!(
            join_namespaced_tool_name(
                &json!({ "name": "mcp__synaroute__synaroute_ai", "namespace": "mcp__synaroute" })
            ),
            "mcp__synaroute__synaroute_ai"
        );
        // 缺 name：不 panic，返回空串。
        assert_eq!(
            join_namespaced_tool_name(&json!({ "namespace": "mcp__x" })),
            ""
        );
    }

    #[test]
    fn history_function_call_keeps_namespace_as_full_name() {
        // 根因回归护栏（实机 unsupported call）：历史里的 function_call 若只取 `name`，
        // 模型看到「我上一轮用 synaroute_ai 调用过」→ 下一轮照抄短名 → 响应侧
        // split_namespaced_tool_name 拆不出 namespace → Codex router 报 unsupported call。
        // 实机 rollout 三次调用中失败的那次正是 ns=- （模型抄了短名）。
        let body = json!({
            "model": "gpt-5.5",
            "input": [
                { "type": "function_call", "call_id": "c1", "name": "synaroute_ai",
                  "namespace": "mcp__synaroute", "arguments": "{\"prompt\":\"hi\"}" },
                { "type": "function_call_output", "call_id": "c1", "output": "done" },
                // 对照：平铺工具无 namespace，不得被改名
                { "type": "function_call", "call_id": "c2", "name": "update_plan", "arguments": "{}" }
            ]
        });
        let chat = responses_to_chat(&body);
        let msgs = chat["messages"].as_array().unwrap();
        let names: Vec<&str> = msgs
            .iter()
            .filter_map(|m| m["tool_calls"][0]["function"]["name"].as_str())
            .collect();
        assert_eq!(
            names,
            vec!["mcp__synaroute__synaroute_ai", "update_plan"],
            "带 namespace 的须拼回全名、平铺的须原样保留"
        );
        // arguments 与 call_id 不受影响
        let first = msgs
            .iter()
            .find(|m| m["tool_calls"][0]["function"]["name"] == "mcp__synaroute__synaroute_ai")
            .unwrap();
        assert_eq!(first["tool_calls"][0]["id"], "c1");
        assert_eq!(first["tool_calls"][0]["function"]["arguments"], "{\"prompt\":\"hi\"}");
    }

    #[test]
    fn history_custom_tool_call_keeps_namespace() {
        // custom_tool_call 历史同理（通常平铺，但带 namespace 时也要拼回，口径不分叉）。
        let body = json!({
            "model": "gpt-5.5",
            "input": [
                { "type": "custom_tool_call", "call_id": "c1", "name": "sub",
                  "namespace": "mcp__ns", "input": "PATCH" },
                { "type": "custom_tool_call", "call_id": "c2", "name": "apply_patch", "input": "P2" }
            ]
        });
        let chat = responses_to_chat(&body);
        let names: Vec<&str> = chat["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|m| m["tool_calls"][0]["function"]["name"].as_str())
            .collect();
        assert_eq!(names, vec!["mcp__ns__sub", "apply_patch"]);
    }

    #[test]
    fn namespace_name_round_trips_through_split() {
        // 闭环：拼回的全名必须能被响应侧 split_namespaced_tool_name 原样拆开，
        // 否则回程 item 依旧缺 namespace 字段。这条把请求侧与响应侧的口径钉在一起。
        let item = json!({ "name": "synaroute_ai", "namespace": "mcp__synaroute" });
        let full = join_namespaced_tool_name(&item);
        let (ns, sub) = split_namespaced_tool_name(&full, &["mcp__synaroute".to_string()]);
        assert_eq!(ns.as_deref(), Some("mcp__synaroute"), "namespace 应能拆回");
        assert_eq!(sub, "synaroute_ai", "子工具名应能拆回");
    }

    #[test]
    fn responses_resp_to_chat_function_call_keeps_namespace() {
        // 响应体方向（Responses 上游 → Chat 中枢）同样要拼全名：
        // 下游客户端看到的工具名须与工具声明一致，否则它同样查不到工具。
        let resp = json!({
            "id": "resp_1",
            "model": "gpt-5.5",
            "output": [
                { "type": "function_call", "call_id": "c1", "name": "synaroute_ai",
                  "namespace": "mcp__synaroute", "arguments": "{\"prompt\":\"hi\"}" }
            ]
        });
        let chat = responses_resp_to_chat(&resp);
        assert_eq!(
            chat["choices"][0]["message"]["tool_calls"][0]["function"]["name"],
            "mcp__synaroute__synaroute_ai"
        );
    }

    #[test]
    fn openai_tools_to_anthropic_uses_input_schema() {
        let tools = json!([{
            "type": "function",
            "function": {
                "name": "apply_patch",
                "description": "Apply a patch",
                "inputSchema": { "type": "object", "properties": { "patch": { "type": "string" } } }
            }
        }]);
        let result = openai_tools_to_anthropic(&tools).unwrap();
        let tool = &result[0];
        assert_eq!(tool["name"], "apply_patch");
        let schema = &tool["input_schema"];
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["patch"].is_object(), "inputSchema 应被读取为 input_schema");
    }

    #[test]
    fn responses_to_chat_maps_custom_tool_with_input_schema() {
        let body = json!({
            "model": "[REDACTED]",
            "input": [],
            "tools": [{
                "type": "custom",
                "name": "apply_patch",
                "description": "Apply a patch",
                "inputSchema": { "type": "object", "properties": { "patch": { "type": "string" } } }
            }]
        });
        let result = responses_to_chat(&body);
        let tools = result["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        let f = &tools[0]["function"];
        assert_eq!(f["name"], "apply_patch");
        assert_eq!(f["parameters"]["type"], "object", "inputSchema 应映射到 parameters");
    }

    #[test]
    fn chat_resp_to_responses_ext_custom_type() {
        let body = json!({
            "choices": [{ "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{ "id": "tc1", "type": "function", "function": { "name": "apply_patch", "arguments": "{\"input\":\"*** Begin Patch\"}" } }]
            }, "finish_reason": "tool_calls" }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 5 }
        });
        let custom = std::collections::HashSet::from(["apply_patch".to_string()]);
        let result = chat_resp_to_responses_ext(&body, &custom, &Default::default());
        let output = result["output"].as_array().unwrap();
        assert_eq!(output.len(), 1);
        assert_eq!(output[0]["type"], "custom_tool_call", "apply_patch 应输出 custom_tool_call");
        assert_eq!(output[0]["name"], "apply_patch");
        // custom_tool_call 必须携带裸字符串 input，且不得再带 arguments（Codex 反序列化按 input）
        assert_eq!(output[0]["input"], "*** Begin Patch", "input 应从 input 键解包成裸串");
        assert!(output[0].get("arguments").is_none(), "custom_tool_call 不应再带 arguments");
    }

    #[test]
    fn chat_resp_to_responses_ext_function_type() {
        let body = json!({
            "choices": [{ "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{ "id": "tc2", "type": "function", "function": { "name": "read_file", "arguments": "{}" } }]
            }, "finish_reason": "tool_calls" }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 5 }
        });
        let custom = std::collections::HashSet::from(["apply_patch".to_string()]);
        let result = chat_resp_to_responses_ext(&body, &custom, &Default::default());
        let output = result["output"].as_array().unwrap();
        assert_eq!(output.len(), 1);
        assert_eq!(output[0]["type"], "function_call", "非 custom 工具应保持 function_call");
    }

    #[test]
    fn unpack_custom_tool_input_variants() {
        // 1. 最常见：{"input":"<裸串>"} → 取 input
        assert_eq!(
            unpack_custom_tool_input("{\"input\":\"*** Begin Patch\\nhi\"}"),
            "*** Begin Patch\nhi"
        );
        // 2. 单字段对象换了键名 → 取该字符串值
        assert_eq!(unpack_custom_tool_input("{\"cmd\":\"ls -la\"}"), "ls -la");
        // 3. JSON 字符串标量 → 取其内容
        assert_eq!(unpack_custom_tool_input("\"raw string\""), "raw string");
        // 4. 空串 → 空串
        assert_eq!(unpack_custom_tool_input("   "), "");
        // 5. 非 JSON（本身就是裸串）→ 原样返回，不吞内容
        assert_eq!(unpack_custom_tool_input("*** Begin Patch"), "*** Begin Patch");
        // 6. 多字段对象无 input → 原样返回整串（避免误取）
        let multi = "{\"a\":\"x\",\"b\":\"y\"}";
        assert_eq!(unpack_custom_tool_input(multi), multi);
    }

    #[test]
    fn sse_emits_custom_tool_call_with_input_not_arguments() {
        // 流式：custom 工具集合命中 apply_patch 时，output_item 必须是 custom_tool_call
        // 且携带裸字符串 input（从 {"input":".."} 解包），不得携带 arguments。
        let custom = std::collections::HashSet::from(["apply_patch".to_string()]);
        let mut tr = SseTranslator::with_namespaces_and_custom(
            SseDirection::AnthropicToResponses,
            vec![],
            custom,
            Default::default(),
        );
        let mut out = String::new();
        out.push_str(&tr.push(b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"model\":\"claude-opus-4-8\"}}\n\n"));
        out.push_str(&tr.push(b"event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"apply_patch\",\"input\":{}}}\n\n"));
        out.push_str(&tr.push(b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"input\\\":\\\"PATCH\\\"}\"}}\n\n"));
        out.push_str(&tr.push(b"event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n"));
        out.push_str(&tr.push(b"event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"}}\n\n"));
        out.push_str(&tr.push(b"event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"));
        out.push_str(&tr.finish());
        assert!(out.contains("\"type\":\"custom_tool_call\""), "应输出 custom_tool_call:\n{out}");
        assert!(out.contains("\"input\":\"PATCH\""), "应携带解包后的裸字符串 input:\n{out}");
        assert!(!out.contains("\"arguments\""), "custom_tool_call 不应携带 arguments:\n{out}");
    }

    #[test]
    fn sse_emits_function_call_with_arguments_for_non_custom() {
        // 对照：非 custom 工具仍走 function_call + arguments。
        let mut tr = SseTranslator::with_namespaces_and_custom(
            SseDirection::AnthropicToResponses,
            vec![],
            std::collections::HashSet::new(),
            Default::default(),
        );
        let mut out = String::new();
        out.push_str(&tr.push(b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"model\":\"claude-opus-4-8\"}}\n\n"));
        out.push_str(&tr.push(b"event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_2\",\"name\":\"read_file\",\"input\":{}}}\n\n"));
        out.push_str(&tr.push(b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\\\"a.rs\\\"}\"}}\n\n"));
        out.push_str(&tr.push(b"event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n"));
        out.push_str(&tr.push(b"event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"}}\n\n"));
        out.push_str(&tr.finish());
        assert!(out.contains("\"type\":\"function_call\""), "非 custom 工具应输出 function_call:\n{out}");
        assert!(out.contains("\"arguments\""), "function_call 应携带 arguments:\n{out}");
        assert!(!out.contains("custom_tool_call"), "不应误判为 custom_tool_call:\n{out}");
    }

    #[test]
    fn responses_to_chat_maps_custom_tool_history_items() {
        // 多轮：Codex 把上一轮 custom 工具调用与结果作为历史带回（custom_tool_call +
        // custom_tool_call_output）。必须还原成 assistant.tool_calls + role:tool，否则丢上下文。
        let body = json!({
            "model": "[REDACTED]",
            "input": [
                { "type": "message", "role": "user", "content": [{ "type": "input_text", "text": "改一下" }] },
                { "type": "custom_tool_call", "call_id": "c1", "name": "apply_patch", "input": "*** Begin Patch" },
                { "type": "custom_tool_call_output", "call_id": "c1", "output": "done" }
            ]
        });
        let result = responses_to_chat(&body);
        let msgs = result["messages"].as_array().unwrap();
        // user + assistant(tool_calls) + tool
        let assistant = msgs.iter().find(|m| m["role"] == "assistant").expect("缺 assistant 消息");
        let tc = &assistant["tool_calls"][0];
        assert_eq!(tc["id"], "c1");
        assert_eq!(tc["function"]["name"], "apply_patch");
        // arguments 重新包成 {"input":".."}，与响应侧解包对称
        assert_eq!(tc["function"]["arguments"], "{\"input\":\"*** Begin Patch\"}");
        let tool_msg = msgs.iter().find(|m| m["role"] == "tool").expect("缺 tool 结果消息");
        assert_eq!(tool_msg["tool_call_id"], "c1");
        assert_eq!(tool_msg["content"], "done");
    }

    // ---- 多模态 + 工具调用（聚合成员 agent 循环的协议层）----

    fn img(mt: &str) -> ImagePart {
        ImagePart {
            media_type: mt.to_string(),
            base64: "AAAA".to_string(),
        }
    }

    #[test]
    fn user_content_stays_plain_string_without_images() {
        // 无图片时必须退化成字符串，与旧的纯文本请求逐字节一致（老网关可能不吃 content 数组）。
        let p = MultimodalPrompt::from_text("你好");
        assert_eq!(anthropic_user_content(&p), json!("你好"));
        assert_eq!(openai_user_content(&p), json!("你好"));
        assert!(!p.has_images());
    }

    #[test]
    fn anthropic_image_block_puts_image_before_text() {
        let p = MultimodalPrompt {
            text: "这个报错怎么回事".into(),
            images: vec![img("image/png")],
        };
        let c = anthropic_user_content(&p);
        let arr = c.as_array().expect("有图片时应为 block 数组");
        assert_eq!(arr.len(), 2);
        // 图片在前：反过来放部分模型会答「没看到图片」。
        assert_eq!(arr[0]["type"], "image");
        assert_eq!(arr[0]["source"]["type"], "base64");
        assert_eq!(arr[0]["source"]["media_type"], "image/png");
        assert_eq!(arr[0]["source"]["data"], "AAAA");
        assert_eq!(arr[1]["type"], "text");
        assert_eq!(arr[1]["text"], "这个报错怎么回事");
    }

    #[test]
    fn openai_image_block_uses_inline_data_url() {
        let p = MultimodalPrompt {
            text: "看图".into(),
            images: vec![img("image/jpeg")],
        };
        let arr = openai_user_content(&p);
        let arr = arr.as_array().unwrap();
        assert_eq!(arr[0]["type"], "image_url");
        // 必须是 data URL 内联，而不是外链（本地文件没有可访问 URL，外链等于发给第三方图床）
        assert_eq!(arr[0]["image_url"]["url"], "data:image/jpeg;base64,AAAA");
        assert_eq!(arr[1]["text"], "看图");
    }

    fn tool_def() -> ToolDef {
        ToolDef {
            name: "read_file".into(),
            description: "读文件".into(),
            input_schema: json!({ "type": "object", "properties": { "path": { "type": "string" } } }),
        }
    }

    #[test]
    fn tool_declaration_shapes_differ_per_protocol() {
        let tools = vec![tool_def()];
        // Anthropic：平铺 + input_schema
        let a = anthropic_tools(&tools);
        assert_eq!(a[0]["name"], "read_file");
        assert_eq!(a[0]["input_schema"]["type"], "object");
        assert!(a[0].get("function").is_none(), "Anthropic 不该有 function 包层");
        // OpenAI：包一层 function + 字段名叫 parameters（这两处最易写错）
        let o = openai_tools(&tools);
        assert_eq!(o[0]["type"], "function");
        assert_eq!(o[0]["function"]["name"], "read_file");
        assert_eq!(o[0]["function"]["parameters"]["type"], "object");
        assert!(
            o[0]["function"].get("input_schema").is_none(),
            "OpenAI 的 schema 字段是 parameters，不是 input_schema"
        );
    }

    #[test]
    fn parse_anthropic_turn_picks_up_tool_use_and_preamble_text() {
        let raw = json!({
            "content": [
                { "type": "text", "text": "我先看看那个文件" },
                { "type": "tool_use", "id": "toolu_1", "name": "read_file",
                  "input": { "path": "src/main.rs" } }
            ],
            "stop_reason": "tool_use"
        })
        .to_string();
        let TurnOutcome::ToolCalls { text, assistant, calls } =
            parse_anthropic_turn(&raw).expect("应解析成功")
        else {
            panic!("有 tool_use 时不该判为纯文本");
        };
        assert_eq!(text, "我先看看那个文件");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "toolu_1");
        assert_eq!(calls[0].name, "read_file");
        assert_eq!(calls[0].args["path"], "src/main.rs");
        // assistant 历史照抄原 content 数组（重建会丢 thinking 的 signature → 下一轮 400）
        assert_eq!(assistant["role"], "assistant");
        assert_eq!(assistant["content"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn parse_anthropic_turn_thinking_signature_survives_roundtrip() {
        // 带扩展思考的多轮：signature 必须原样留在 assistant 历史里，否则 Anthropic 校验失败 400。
        let raw = json!({
            "content": [
                { "type": "thinking", "thinking": "先读文件", "signature": "sig_abc" },
                { "type": "tool_use", "id": "toolu_9", "name": "grep", "input": { "pattern": "fn main" } }
            ]
        })
        .to_string();
        let TurnOutcome::ToolCalls { assistant, .. } = parse_anthropic_turn(&raw).unwrap() else {
            panic!("应是工具调用");
        };
        assert_eq!(assistant["content"][0]["signature"], "sig_abc");
    }

    #[test]
    fn parse_anthropic_turn_without_tool_use_is_final_text() {
        let raw = json!({ "content": [{ "type": "text", "text": "结论是 A" }] }).to_string();
        assert_eq!(
            parse_anthropic_turn(&raw).unwrap(),
            TurnOutcome::Text("结论是 A".into())
        );
    }

    #[test]
    fn parse_anthropic_turn_degrades_to_text_on_sse_body() {
        // 个别网关无论怎么发都回 SSE。此时退化成「纯文本、无工具调用」比整轮报错好：
        // 成员至少还能基于已注入的上下文作答。
        let raw = "data: {\"delta\":{\"text\":\"部\"}}\n\ndata: {\"delta\":{\"text\":\"分\"}}\n\ndata: [DONE]\n";
        assert_eq!(
            parse_anthropic_turn(raw).unwrap(),
            TurnOutcome::Text("部分".into())
        );
    }

    #[test]
    fn parse_openai_turn_parses_arguments_json_string() {
        let raw = json!({
            "choices": [{ "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{ "id": "call_1", "type": "function", "function": {
                    "name": "grep", "arguments": "{\"pattern\":\"fn main\"}"
                }}]
            }}]
        })
        .to_string();
        let TurnOutcome::ToolCalls { text, calls, .. } = parse_openai_turn(&raw).unwrap() else {
            panic!("应是工具调用");
        };
        assert_eq!(text, "", "content 为 null 时文本应是空串而非 panic");
        assert_eq!(calls[0].id, "call_1");
        // arguments 是 JSON 字符串，必须解析成对象后再交给执行层
        assert_eq!(calls[0].args["pattern"], "fn main");
    }

    #[test]
    fn parse_openai_turn_malformed_arguments_becomes_null() {
        // 被截断的 arguments 留 Null，由执行层回「参数不是合法 JSON 对象」让模型重试；
        // 若当成空对象去执行，read_file 可能读错目标。
        let raw = json!({
            "choices": [{ "message": { "role": "assistant", "tool_calls": [
                { "id": "c1", "function": { "name": "read_file", "arguments": "{\"path\":\"sr" } }
            ]}}]
        })
        .to_string();
        let TurnOutcome::ToolCalls { calls, .. } = parse_openai_turn(&raw).unwrap() else {
            panic!("应是工具调用");
        };
        assert!(calls[0].args.is_null());
        // 空 arguments（无参工具）反过来要按空对象处理，不能也判成 Null
        let raw2 = json!({
            "choices": [{ "message": { "role": "assistant", "tool_calls": [
                { "id": "c2", "function": { "name": "list_dir", "arguments": "" } }
            ]}}]
        })
        .to_string();
        let TurnOutcome::ToolCalls { calls, .. } = parse_openai_turn(&raw2).unwrap() else {
            panic!("应是工具调用");
        };
        assert_eq!(calls[0].args, json!({}));
    }

    #[test]
    fn parse_openai_turn_backfills_missing_role() {
        // 部分网关回的 message 不带 role；缺了它下一轮会被上游判为非法角色而 400。
        let raw = json!({
            "choices": [{ "message": { "tool_calls": [
                { "id": "c1", "function": { "name": "list_dir", "arguments": "{}" } }
            ]}}]
        })
        .to_string();
        let TurnOutcome::ToolCalls { assistant, .. } = parse_openai_turn(&raw).unwrap() else {
            panic!("应是工具调用");
        };
        assert_eq!(assistant["role"], "assistant");
    }

    #[test]
    fn parse_openai_turn_without_tool_calls_is_final_text() {
        let raw = json!({ "choices": [{ "message": { "role": "assistant", "content": "结论 B" } }] })
            .to_string();
        assert_eq!(
            parse_openai_turn(&raw).unwrap(),
            TurnOutcome::Text("结论 B".into())
        );
    }

    #[test]
    fn anthropic_tool_results_pack_into_single_user_message() {
        let mut s = ToolSession::new(Protocol::Anthropic, &MultimodalPrompt::from_text("问题"));
        s.push_tool_results(&[
            ToolResultMsg { id: "t1".into(), content: "内容1".into(), is_error: false },
            ToolResultMsg { id: "t2".into(), content: "文件不存在".into(), is_error: true },
        ]);
        // 两条结果必须打包进**一条** user 消息：拆成两条会被判连续 user 轮次而报错。
        assert_eq!(s.messages().len(), 2, "开局 user + 一条结果消息");
        let m = &s.messages()[1];
        assert_eq!(m["role"], "user");
        let blocks = m["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0]["type"], "tool_result");
        assert_eq!(blocks[0]["tool_use_id"], "t1");
        assert_eq!(blocks[0]["is_error"], false);
        assert_eq!(blocks[1]["is_error"], true);
        assert_eq!(blocks[1]["content"], "文件不存在");
    }

    #[test]
    fn openai_tool_results_are_one_message_each_with_error_marker() {
        let mut s = ToolSession::new(Protocol::OpenaiChat, &MultimodalPrompt::from_text("问题"));
        s.push_tool_results(&[
            ToolResultMsg { id: "c1".into(), content: "内容1".into(), is_error: false },
            ToolResultMsg { id: "c2".into(), content: "被拒".into(), is_error: true },
        ]);
        assert_eq!(s.messages().len(), 3, "OpenAI 一条结果一条消息");
        assert_eq!(s.messages()[1]["role"], "tool");
        assert_eq!(s.messages()[1]["tool_call_id"], "c1");
        assert_eq!(s.messages()[1]["content"], "内容1");
        // OpenAI 协议无 is_error 字段，错误标记只能写进正文，否则模型会把错误文本当数据用。
        assert_eq!(
            s.messages()[2]["content"].as_str().unwrap(),
            "[工具执行失败] 被拒"
        );
    }

    #[test]
    fn empty_tool_results_do_not_append_message() {
        // 空结果集不该塞一条空 content 消息（Anthropic 对空 content 数组会 400）。
        let mut s = ToolSession::new(Protocol::Anthropic, &MultimodalPrompt::from_text("问题"));
        s.push_tool_results(&[]);
        assert_eq!(s.messages().len(), 1);
    }

    #[test]
    fn tool_session_seeds_images_per_protocol() {
        let p = MultimodalPrompt { text: "看图".into(), images: vec![img("image/webp")] };
        let a = ToolSession::new(Protocol::Anthropic, &p);
        assert_eq!(a.messages()[0]["content"][0]["type"], "image");
        // Responses 协议在聚合路径按 Chat 形态发（与 text_completion 一致）
        let o = ToolSession::new(Protocol::OpenaiResponses, &p);
        assert_eq!(o.messages()[0]["content"][0]["type"], "image_url");
    }
}
