//! 上游厂商通信 + 协议适配层。
//!
//! MVP 范围（arch-decisions §10）：
//! - 非流式：Anthropic Messages ↔ OpenAI Chat Completions 双向字段转换。
//! - 拉取模型：兼容 Anthropic /v1/models 与 OpenAI /v1/models。
//! - 简单文本请求用于健康检查与聚合成员调用。
//!
//! 流式 SSE 的完整转发在 proxy 模块处理；此处提供非流式的一次性调用。

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
                last_err = e.to_string();
                continue;
            }
        };
        let status = resp.status();
        if !status.is_success() {
            last_err = format!("HTTP {status} @ {url}");
            // 404/405 说明路径不对，换下一个候选；其他状态码同样重试下一个
            continue;
        }
        let body: Value = resp.json().await?;
        let names = parse_model_names(&body);
        if !names.is_empty() {
            return Ok(names);
        }
        last_err = format!("响应无模型列表 @ {url}");
    }
    Err(AppError::Upstream(format!("拉取模型失败：{last_err}")))
}

/// 已知的「协议兼容子路径」后缀（借鉴 cc-switch `model_fetch.rs::KNOWN_COMPAT_SUFFIXES`）。
/// 这些不是版本段，而是厂商把 Anthropic/Coding 协议挂在子路径上的兼容前缀
/// （如 DeepSeek `https://api.deepseek.com/anthropic`）。命中时模型列表通常在 host 根、
/// 而非该子路径下，故需剥离后缀回根再探。按长度降序，最长优先匹配。
const KNOWN_COMPAT_SUFFIXES: &[&str] = &[
    "/api/claudecode",
    "/api/anthropic",
    "/apps/anthropic",
    "/api/coding",
    "/claudecode",
    "/anthropic",
    "/step_plan",
    "/coding",
    "/claude",
];

/// 若 base_url 以某个已知兼容子路径结尾，返回剥离后缀后的剩余部分。
fn strip_compat_suffix(base: &str) -> Option<&str> {
    for suffix in KNOWN_COMPAT_SUFFIXES {
        if let Some(root) = base.strip_suffix(suffix) {
            return Some(root);
        }
    }
    None
}

/// 判断 base_url 的最后一段是否是 OpenAI 风格版本段 `v{N}`（v1/v4/v1beta/v2alpha）。
/// 版本段已在路径里 → 模型/资源端点直接接 `/models`、`/messages`，不再补 `/v1`。
/// 注意：`/anthropic`、`/coding` 等**不是**版本段（不以 v+数字开头）。
fn ends_with_version_segment(base: &str) -> bool {
    let last = base.trim_end_matches('/').rsplit('/').next().unwrap_or("");
    match last.strip_prefix('v').or_else(|| last.strip_prefix('V')) {
        // v 后至少一位数字，其余可为字母（兼容 v1beta / v2alpha），整体为字母数字
        Some(rest) => {
            rest.chars().next().is_some_and(|c| c.is_ascii_digit())
                && rest.chars().all(|c| c.is_ascii_alphanumeric())
        }
        None => false,
    }
}

/// 生成模型列表的候选端点（按优先级）。借鉴 cc-switch `build_models_url_candidates`：
/// - base 已以版本段结尾（/v1、/v4…）：`{base}/models`（非 /v1 时再兜底 `{base}/v1/models`）
/// - 否则（裸 host 或兼容子路径）：`{base}/v1/models`、`{base}/models`
/// - 若 base 命中兼容子路径（/anthropic 等）：追加剥离后的 `{root}/v1/models`、`{root}/models`
///   （DeepSeek 等的 /models 在 host 根，不在 /anthropic 子路径下）
///
/// 结果去重且保持顺序。
fn model_endpoints(base: &str) -> Vec<String> {
    let base = base.trim_end_matches('/');
    let mut c: Vec<String> = Vec::new();
    if ends_with_version_segment(base) {
        c.push(format!("{base}/models"));
        if !base.ends_with("/v1") {
            c.push(format!("{base}/v1/models"));
        }
    } else {
        c.push(format!("{base}/v1/models"));
        c.push(format!("{base}/models"));
    }
    if let Some(root) = strip_compat_suffix(base) {
        let root = root.trim_end_matches('/');
        if root.contains("://") {
            c.push(format!("{root}/v1/models"));
            c.push(format!("{root}/models"));
        }
    }
    // 线性去重（候选很少）
    let mut out: Vec<String> = Vec::with_capacity(c.len());
    for url in c {
        if !out.iter().any(|u| u == &url) {
            out.push(url);
        }
    }
    out
}

/// 把「默认带 /v1」的资源路径接到 base_url 上，兼容 base 是否已含版本段（FR-004 修复）。
///
/// 判据是「最后一段是不是版本段 v{N}」（借鉴 cc-switch），而非「有没有路径」——
/// 后者会把 DeepSeek 的兼容前缀 `/anthropic` 误当版本、把 `/v1` 吞掉拼成错误 URL。
/// - base 最后一段是版本段（/v1、/v4、/v1beta）：只接资源名（去掉 path 的 `/v1` 前缀）
/// - 否则（裸 host 或兼容子路径 /anthropic）：原样接 path（补默认 `/v1`）
///
/// 例：
/// - `https://api.anthropic.com` + `/v1/messages` → `.../v1/messages`
/// - `https://api.openai.com/v1` + `/v1/chat/completions` → `.../v1/chat/completions`
/// - `https://open.bigmodel.cn/api/paas/v4` + `/v1/chat/completions` → `.../v4/chat/completions`
/// - `https://api.deepseek.com/anthropic` + `/v1/messages` → `.../anthropic/v1/messages`（关键修复）
pub fn join_endpoint(base: &str, path: &str) -> String {
    let base = base.trim_end_matches('/');
    if ends_with_version_segment(base) {
        let resource = path.strip_prefix("/v1").unwrap_or(path);
        format!("{base}{resource}")
    } else {
        format!("{base}{path}")
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

/// 上游临时错误自动重试的最大尝试次数（含首次）。
const RETRY_MAX_ATTEMPTS: u32 = 3;
/// 重试基础退避（毫秒），按尝试次数线性递增（300 / 600 ...）。
const RETRY_BASE_BACKOFF_MS: u64 = 300;

/// 判断上游错误是否「临时性、值得重试」：502/503/504 网关错误、429 限流、连接层失败。
/// 4xx（除 429）多为鉴权/参数问题，重试无意义，不重试。
pub fn is_retriable_upstream_error(e: &AppError) -> bool {
    let AppError::Upstream(msg) = e else { return false };
    msg.contains("HTTP 502")
        || msg.contains("HTTP 503")
        || msg.contains("HTTP 504")
        || msg.contains("HTTP 429")
        // 连接层失败（build_client / send 失败）："连接 xxx 失败"
        || msg.contains("连接")
        || msg.contains("error sending request")
        || msg.contains("error decoding response body")
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
    Err(last_err.unwrap_or_else(|| AppError::Upstream("未知上游错误".into())))
}

/// 截断上游响应体用于错误展示（避免超长 body 撑爆日志/错误信息）。
fn truncate_body(raw: &str) -> String {
    const CAP: usize = 400;
    let t = raw.trim();
    if t.chars().count() <= CAP {
        t.to_string()
    } else {
        let head: String = t.chars().take(CAP).collect();
        format!("{head}…（已截断，共 {} 字符）", t.chars().count())
    }
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
    let mut req = client.post(&url).json(&payload).timeout(request_timeout);
    req = req.header("anthropic-version", "2023-06-01");
    req = apply_auth(req, key, secret);

    let resp = req.send().await?;
    let status = resp.status();
    // 先读文本再解析：上游可能返回 SSE 流（data: {...}）、HTML 错误页或非标准 JSON，
    // 直接 resp.json() 会得到笼统的「error decoding response body」，看不到上游到底返回了啥。
    let raw = resp.text().await?;
    if !status.is_success() {
        return Err(AppError::Upstream(format!(
            "Anthropic HTTP {status}: {}",
            truncate_body(&raw)
        )));
    }
    // content: [{type:"text", text:"..."}]。兼容普通 JSON 与 SSE 流两种返回形态。
    let text = parse_anthropic_text(&raw).ok_or_else(|| {
        AppError::Upstream(format!("Anthropic 响应无法解析: {}", truncate_body(&raw)))
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

    let resp = req.send().await?;
    let status = resp.status();
    let raw = resp.text().await?;
    if !status.is_success() {
        return Err(AppError::Upstream(format!(
            "OpenAI HTTP {status}: {}",
            truncate_body(&raw)
        )));
    }
    let text = parse_openai_text(&raw).ok_or_else(|| {
        AppError::Upstream(format!("OpenAI 响应无法解析: {}", truncate_body(&raw)))
    })?;
    Ok(text)
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
/// 返回 (是否健康, 延迟毫秒)。
pub async fn health_probe(key: &ProviderKey, secret: &str) -> (bool, u64) {
    let client = match build_client(key) {
        Ok(c) => c,
        Err(_) => return (false, 0),
    };
    // 用最便宜的 models 候选端点探测；只关心「有没有回应 + 状态码」，不解析 body。
    let url = model_endpoints(&key.base_url)
        .into_iter()
        .next()
        .unwrap_or_else(|| key.base_url.trim_end_matches('/').to_string());
    let mut req = apply_models_auth(client.get(&url).timeout(fast_timeout(key)), secret);
    // Anthropic 真实 API 的 GET /v1/models 需带版本头，否则 400（不影响健康判定，但让
    // 有效 Key 能拿到真实 200 与准确延迟）。
    if matches!(key.protocol, Protocol::Anthropic) {
        req = req.header("anthropic-version", "2023-06-01");
    }

    let start = std::time::Instant::now();
    let healthy = match req.send().await {
        Ok(resp) => status_is_healthy(resp.status().as_u16()),
        Err(_) => false, // 连接层失败（超时/连不上/DNS）：不可达
    };
    let latency = start.elapsed().as_millis() as u64;
    (healthy, latency)
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
pub async fn health_probe_real(key: &ProviderKey, secret: &str) -> (bool, u64, Option<String>) {
    let Some(model) = key.probe_model() else {
        // 该 Key 没有任何可探测的真实模型名 → 无法发补全，退回轻量连通探测。
        let (ok, latency) = health_probe(key, secret).await;
        return (ok, latency, (!ok).then(|| "轻量连通探测失败（无可探测模型）".to_string()));
    };
    let start = std::time::Instant::now();
    // 极小请求：一个字 prompt、max_tokens=1。不重试（探测要快、如实反映当下）。
    // 探测超时封顶 30s（fast_timeout）：1 token 秒回,不跟随用户为慢厂商设的长超时,
    // 否则串行探测循环会被一个挂掉的慢 Key 拖住几分钟。
    let result = text_completion(key, secret, &model, "hi", 1, false, fast_timeout(key)).await;
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

/// OpenAI tools → Anthropic tools。
fn openai_tools_to_anthropic(tools: &Value) -> Option<Value> {
    let arr = tools.as_array()?;
    let out: Vec<Value> = arr
        .iter()
        .filter_map(|t| {
            let f = t.get("function")?;
            let name = f.get("name")?.as_str()?;
            let mut a = serde_json::Map::new();
            a.insert("name".into(), json!(name));
            if let Some(d) = f.get("description") {
                a.insert("description".into(), d.clone());
            }
            // Anthropic 要求 input_schema 至少是个 object schema
            a.insert(
                "input_schema".into(),
                f.get("parameters").cloned().unwrap_or_else(|| json!({ "type": "object" })),
            );
            Some(Value::Object(a))
        })
        .collect();
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
                "system" => system.push_str(&extract_text_content(m.get("content"))),
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

/// 将 OpenAI Chat Completions 响应体转为 Anthropic Messages 响应体（MVP：纯文本）。
/// 用于下游是 Anthropic 客户端、上游 Key 是 OpenAI 协议的跨协议故障转移。
pub fn openai_resp_to_anthropic(body: &Value) -> Value {
    // OpenAI: choices[0].message.content（文本）、finish_reason、usage.{prompt,completion}_tokens
    let text = body
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|arr| arr.first())
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or("");
    let model = body.get("model").cloned().unwrap_or(Value::Null);
    let id = body
        .get("id")
        .and_then(|i| i.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("msg_{}", uuid_like()));
    let finish = body
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|arr| arr.first())
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
    json!({
        "id": id,
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": [ { "type": "text", "text": text } ],
        "stop_reason": stop_reason,
        "stop_sequence": Value::Null,
        "usage": { "input_tokens": input_tokens, "output_tokens": output_tokens }
    })
}

/// 将 Anthropic Messages 响应体转为 OpenAI Chat Completions 响应体（MVP：纯文本）。
/// 用于下游是 OpenAI 客户端、上游 Key 是 Anthropic 协议的跨协议故障转移。
pub fn anthropic_resp_to_openai(body: &Value) -> Value {
    // Anthropic: content[].text（拼接）、stop_reason、usage.{input,output}_tokens
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
    json!({
        "id": id,
        "object": "chat.completion",
        "created": chrono::Utc::now().timestamp(),
        "model": model,
        "choices": [ {
            "index": 0,
            "message": { "role": "assistant", "content": text },
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
        Protocol::OpenaiChat => chat,
        Protocol::OpenaiResponses => chat_to_responses(&chat),
    }
}

/// 跨协议**响应体**转换：上游协议 `from` → 下游协议 `to`，以 Chat Completions 为中枢。
/// 同协议直通。用于非流式响应回写给下游客户端。
pub fn convert_response(body: &Value, from: Protocol, to: Protocol) -> Value {
    if from == to {
        return body.clone();
    }
    // 1. 上游 → Chat 中枢
    let chat = match from {
        Protocol::Anthropic => anthropic_resp_to_openai(body),
        Protocol::OpenaiChat => body.clone(),
        Protocol::OpenaiResponses => responses_resp_to_chat(body),
    };
    // 2. Chat 中枢 → 下游
    match to {
        Protocol::Anthropic => openai_resp_to_anthropic(&chat),
        Protocol::OpenaiChat => chat,
        Protocol::OpenaiResponses => chat_resp_to_responses(&chat),
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
                                    "name": it.get("name").cloned().unwrap_or(json!("")),
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
                    // 普通 message item：role + content 分块
                    _ => {
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
    // tools：Responses 扁平形态 → Chat 嵌套形态
    if let Some(tools) = body.get("tools").and_then(|t| t.as_array()) {
        let mapped: Vec<Value> = tools
            .iter()
            .filter_map(|t| {
                // Responses function tool：{type:"function", name, description, parameters}
                let name = t.get("name").and_then(|n| n.as_str())?;
                let mut f = serde_json::Map::new();
                f.insert("name".into(), json!(name));
                if let Some(d) = t.get("description") {
                    f.insert("description".into(), d.clone());
                }
                if let Some(p) = t.get("parameters") {
                    f.insert("parameters".into(), p.clone());
                }
                Some(json!({ "type": "function", "function": f }))
            })
            .collect();
        if !mapped.is_empty() {
            out.insert("tools".into(), json!(mapped));
        }
    }
    Value::Object(out)
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
                            "name": item.get("name").cloned().unwrap_or(json!("")),
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
        _ => None,
    }
}

/// 有状态 SSE 翻译器：喂入上游字节块，产出下游协议的 SSE 文本块。
/// 内部按行缓冲——上游一个 chunk 可能切在半行中间，累积到 `\n` 才处理整行。
pub struct SseTranslator {
    dir: SseDirection,
    buf: String,
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
}

impl SseTranslator {
    pub fn new(dir: SseDirection) -> Self {
        Self {
            dir,
            buf: String::new(),
            resp_id: format!("resp_{}", uuid_like()),
            msg_id: format!("msg_{}", uuid_like()),
            started: false,
            saw_text: false,
            model: String::new(),
            tool_calls: Vec::new(),
        }
    }

    /// 喂入一块上游字节，返回应发给下游的 SSE 文本（可能为空）。
    pub fn push(&mut self, chunk: &[u8]) -> String {
        self.buf.push_str(&String::from_utf8_lossy(chunk));
        let mut out = String::new();
        // 逐个完整行处理，保留最后不完整的一段在 buf。
        while let Some(nl) = self.buf.find('\n') {
            let line: String = self.buf.drain(..=nl).collect();
            let line = line.trim_end_matches(['\r', '\n']);
            if let Some(ev) = self.process_line(line) {
                out.push_str(&ev);
            }
        }
        out
    }

    /// 流结束时冲刷收尾事件（Responses 需要 response.completed；Chat 需 [DONE]）。
    pub fn finish(&mut self) -> String {
        match self.dir {
            SseDirection::ChatToResponses => self.emit_responses_completed(None),
            SseDirection::ChatToAnthropic => self.emit_anthropic_stop(),
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
        if finished {
            // 文本 done + content_part/item done
            if self.saw_text {
                let done = json!({
                    "type": "response.output_text.done",
                    "item_id": self.msg_id, "output_index": 0, "content_index": 0, "text": ""
                });
                out.push_str(&sse("response.output_text.done", &done));
            }
        }
        // usage 单独出现（无 choices 或 choices 空）时，触发 completed
        if chunk.get("usage").is_some() && chunk.get("usage") != Some(&Value::Null) {
            out.push_str(&self.emit_responses_completed(chunk.get("usage")));
        }
        out
    }

    /// 冲刷 Responses 收尾：先补 function_call item（若有），再 response.completed。
    fn emit_responses_completed(&mut self, usage: Option<&Value>) -> String {
        let mut out = String::new();
        // 已发出过 completed 则不重复（用 started 兼作幂等：completed 后置 false）。
        if !self.started {
            return out;
        }
        self.started = false;
        // function_call 项作为独立 output item 补发
        let mut output: Vec<Value> = vec![];
        if self.saw_text {
            output.push(json!({
                "type": "message", "id": self.msg_id, "role": "assistant", "status": "completed",
                "content": [ { "type": "output_text", "text": "", "annotations": [] } ]
            }));
        }
        for (id, name, args) in &self.tool_calls {
            let fc_id = if id.is_empty() { format!("fc_{}", uuid_like()) } else { id.clone() };
            output.push(json!({
                "type": "function_call", "id": fc_id, "call_id": id,
                "name": name, "arguments": args, "status": "completed"
            }));
        }
        let (it, ot) = usage
            .map(|u| (
                u.get("prompt_tokens").and_then(|t| t.as_u64()).unwrap_or(0),
                u.get("completion_tokens").and_then(|t| t.as_u64()).unwrap_or(0),
            ))
            .unwrap_or((0, 0));
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
            "response.completed" => {
                let chunk = json!({
                    "object": "chat.completion.chunk",
                    "choices": [ { "index": 0, "delta": {}, "finish_reason": "stop" } ]
                });
                sse_data(&chunk)
            }
            _ => String::new(),
        }
    }

    /// 一个 Chat SSE chunk → Anthropic SSE 事件文本（文本增量 → content_block_delta）。
    fn chat_chunk_to_anthropic(&mut self, chunk: &Value) -> String {
        let mut out = String::new();
        let choice0 = chunk.get("choices").and_then(|c| c.as_array()).and_then(|a| a.first());
        let delta = choice0.and_then(|c| c.get("delta"));
        if !self.started {
            self.started = true;
            out.push_str(&sse("message_start", &json!({
                "type": "message_start",
                "message": { "id": self.resp_id, "type": "message", "role": "assistant", "content": [],
                    "model": chunk.get("model").cloned().unwrap_or(Value::Null),
                    "usage": { "input_tokens": 0, "output_tokens": 0 } }
            })));
            out.push_str(&sse("content_block_start", &json!({
                "type": "content_block_start", "index": 0,
                "content_block": { "type": "text", "text": "" }
            })));
        }
        if let Some(t) = delta.and_then(|d| d.get("content")).and_then(|c| c.as_str()) {
            if !t.is_empty() {
                self.saw_text = true;
                out.push_str(&sse("content_block_delta", &json!({
                    "type": "content_block_delta", "index": 0,
                    "delta": { "type": "text_delta", "text": t }
                })));
            }
        }
        out
    }

    /// Anthropic 流收尾：content_block_stop + message_delta(stop) + message_stop。
    fn emit_anthropic_stop(&mut self) -> String {
        if !self.started {
            return String::new();
        }
        self.started = false;
        let mut out = String::new();
        out.push_str(&sse("content_block_stop", &json!({ "type": "content_block_stop", "index": 0 })));
        out.push_str(&sse("message_delta", &json!({
            "type": "message_delta", "delta": { "stop_reason": "end_turn" }, "usage": { "output_tokens": 0 }
        })));
        out.push_str(&sse("message_stop", &json!({ "type": "message_stop" })));
        out
    }

    /// 一个 Anthropic SSE 事件 → Chat SSE chunk 文本。
    fn anthropic_event_to_chat(&mut self, ev: &Value) -> String {
        let ty = ev.get("type").and_then(|t| t.as_str()).unwrap_or("");
        match ty {
            "content_block_delta" => {
                let t = ev.get("delta").and_then(|d| d.get("text")).and_then(|t| t.as_str()).unwrap_or("");
                let chunk = json!({
                    "object": "chat.completion.chunk",
                    "choices": [ { "index": 0, "delta": { "content": t }, "finish_reason": Value::Null } ]
                });
                sse_data(&chunk)
            }
            "message_stop" => {
                let chunk = json!({
                    "object": "chat.completion.chunk",
                    "choices": [ { "index": 0, "delta": {}, "finish_reason": "stop" } ]
                });
                sse_data(&chunk)
            }
            _ => String::new(),
        }
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

/// 轻量随机 id 片段（无需强随机性，仅用于响应缺 id 时兜底）。
fn uuid_like() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// content 可能是字符串或分块数组，统一抽取为纯文本（MVP 仅文本）。
fn extract_text_content(content: Option<&Value>) -> String {
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

// ---- 内部工具 ----

/// 全局共享 HTTP 客户端：复用连接池 / TLS 会话，避免每请求重做 TCP+TLS 握手。
/// 不设总超时（response timeout）——总超时由各调用点按 Key 用 `.timeout()` 逐请求指定；
/// 仅设连接超时（连接层握手上限）。reqwest::Client 内部是 Arc，clone 廉价、共享同一连接池。
pub fn shared_client() -> reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(30))
                .build()
                .expect("构建共享 HTTP 客户端失败")
        })
        .clone()
}

/// 某 Key 的单请求总超时（缺省 30s）。可在 Key 编辑器设置（timeoutMs），
/// 服务「非流式转发」这类要等上游完整生成的请求——慢厂商可调大。
pub fn key_timeout(key: &ProviderKey) -> Duration {
    Duration::from_millis(key.params.timeout_ms.unwrap_or(30_000))
}

/// 元数据级快请求（健康探测 / 拉模型列表）的超时：取 key_timeout 但**封顶 30s**。
///
/// 为什么封顶：Key 超时开放用户设置后，慢厂商可能设 300s+；但
/// - 健康检查是**串行**循环（health::check_category 逐 Key await），一个挂掉的慢 Key
///   若跟随大超时，会把整个分类的探测阻塞几分钟；
/// - 拉模型按候选端点顺序试（最多 4 个），跟随大超时时「拉取模型」按钮最坏挂 4×超时。
/// 这些都是秒级应答的元数据 GET / 1-token 探测，30s 是宽裕上限，不该继承长生成超时。
pub fn fast_timeout(key: &ProviderKey) -> Duration {
    key_timeout(key).min(Duration::from_secs(30))
}

/// 返回共享客户端（保留旧签名以最小化改动；总超时改由调用点逐请求 `.timeout()` 指定）。
fn build_client(key: &ProviderKey) -> AppResult<reqwest::Client> {
    let _ = key;
    Ok(shared_client())
}

/// 按协议注入鉴权头
fn apply_auth(
    req: reqwest::RequestBuilder,
    key: &ProviderKey,
    secret: &str,
) -> reqwest::RequestBuilder {
    // Chat 与 Responses 同属 OpenAI 家族，均用 Bearer 鉴权；Anthropic 用 x-api-key。
    if key.protocol.is_openai() {
        req.header("authorization", format!("Bearer {secret}"))
    } else {
        req.header("x-api-key", secret)
    }
}

/// /models 探测专用鉴权：同时带 `Authorization: Bearer` 与 `x-api-key`。
/// 兼容厂商（DeepSeek/Kimi/GLM 等）把 Anthropic 协议挂在子路径、但模型列表是 OpenAI 风格
/// （需 Bearer）；而真 Anthropic 的 /v1/models 需 x-api-key。GET /models 只读，多带一个
/// 不被识别的鉴权头无害，故两个都带以最大化兼容（借鉴 cc-switch 对 /models 统一用 Bearer 的思路，
/// 并叠加 x-api-key 以不牺牲真 Anthropic）。
fn apply_models_auth(req: reqwest::RequestBuilder, secret: &str) -> reqwest::RequestBuilder {
    req.header("authorization", format!("Bearer {secret}"))
        .header("x-api-key", secret)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn join_endpoint_handles_version_segments() {
        // 裸 host：补默认 /v1
        assert_eq!(
            join_endpoint("https://api.anthropic.com", "/v1/messages"),
            "https://api.anthropic.com/v1/messages"
        );
        assert_eq!(
            join_endpoint("https://api.deepseek.com", "/v1/chat/completions"),
            "https://api.deepseek.com/v1/chat/completions"
        );
        // base 已含 /v1：不重复
        assert_eq!(
            join_endpoint("https://api.openai.com/v1", "/v1/chat/completions"),
            "https://api.openai.com/v1/chat/completions"
        );
        // base 含非 /v1 版本段：只接资源名（核心修复——旧实现会拼出 /v4/v1/...）
        assert_eq!(
            join_endpoint("https://open.bigmodel.cn/api/paas/v4", "/v1/chat/completions"),
            "https://open.bigmodel.cn/api/paas/v4/chat/completions"
        );
        assert_eq!(
            join_endpoint("https://generativelanguage.googleapis.com/v1beta", "/v1/messages"),
            "https://generativelanguage.googleapis.com/v1beta/messages"
        );
        // trailing slash 归一
        assert_eq!(
            join_endpoint("https://api.openai.com/v1/", "/v1/chat/completions"),
            "https://api.openai.com/v1/chat/completions"
        );
        // DeepSeek 兼容前缀 /anthropic 不是版本段：保留 /v1（关键修复，此前会拼成 .../anthropic/messages）
        assert_eq!(
            join_endpoint("https://api.deepseek.com/anthropic", "/v1/messages"),
            "https://api.deepseek.com/anthropic/v1/messages"
        );
    }

    #[test]
    fn version_segment_detection() {
        assert!(ends_with_version_segment("https://x.com/v1"));
        assert!(ends_with_version_segment("https://x.com/api/paas/v4"));
        assert!(ends_with_version_segment("https://x.com/v1beta")); // v + 数字 + 字母
        assert!(!ends_with_version_segment("https://x.com/anthropic")); // 兼容前缀，非版本
        assert!(!ends_with_version_segment("https://api.deepseek.com")); // 裸 host
        assert!(!ends_with_version_segment("https://x.com/coding"));
    }

    #[test]
    fn model_endpoints_cover_deepseek_anthropic_compat() {
        // 关键场景：DeepSeek Anthropic 兼容前缀。/models 在 host 根，故需剥离 /anthropic 追加根候选。
        let eps = model_endpoints("https://api.deepseek.com/anthropic");
        assert!(eps.contains(&"https://api.deepseek.com/anthropic/v1/models".to_string()));
        assert!(eps.contains(&"https://api.deepseek.com/v1/models".to_string()), "应含剥离后的 host 根候选");
        assert!(eps.contains(&"https://api.deepseek.com/models".to_string()));
    }

    #[test]
    fn model_endpoints_version_and_bare() {
        // 版本段 base：{base}/models 优先，非 /v1 再兜底 {base}/v1/models
        assert_eq!(
            model_endpoints("https://open.bigmodel.cn/api/paas/v4"),
            vec![
                "https://open.bigmodel.cn/api/paas/v4/models".to_string(),
                "https://open.bigmodel.cn/api/paas/v4/v1/models".to_string(),
            ]
        );
        // 裸 host：/v1/models 优先，回退 /models
        assert_eq!(
            model_endpoints("https://api.deepseek.com"),
            vec![
                "https://api.deepseek.com/v1/models".to_string(),
                "https://api.deepseek.com/models".to_string()
            ]
        );
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

    #[test]
    fn truncate_body_caps_long_text() {
        let short = "abc";
        assert_eq!(truncate_body(short), "abc");
        let long: String = "x".repeat(500);
        let out = truncate_body(&long);
        assert!(out.contains("已截断"));
        assert!(out.chars().count() < 500);
    }

    // ---- Responses ↔ Chat 转换 ----

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

    // ---- 流式 SSE 翻译（Task #16）----

    #[test]
    fn sse_direction_covers_supported_and_rejects_same() {
        use Protocol::*;
        assert_eq!(sse_direction(OpenaiResponses, OpenaiChat), Some(SseDirection::ChatToResponses));
        assert_eq!(sse_direction(OpenaiChat, OpenaiResponses), Some(SseDirection::ResponsesToChat));
        assert_eq!(sse_direction(Anthropic, OpenaiChat), Some(SseDirection::ChatToAnthropic));
        assert_eq!(sse_direction(OpenaiChat, Anthropic), Some(SseDirection::AnthropicToChat));
        // 同协议 / 无中枢路径的跨协议：None
        assert_eq!(sse_direction(OpenaiChat, OpenaiChat), None);
        assert_eq!(sse_direction(Anthropic, OpenaiResponses), None);
        assert_eq!(sse_direction(OpenaiResponses, Anthropic), None);
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
}
