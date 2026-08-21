//! 非流式的跨协议字段转换（proxy 故障转移时，下游协议与上游 Key 协议不同就走这里）。
//!
//! 走 **hub-and-spoke**：以 Chat 为中枢，3 个协议只需 6 个单边函数而不是 9 个有向函数。
//! 流式那套是并行的第二套矩阵（见 sse.rs），两者的能力差异由 sse_golden 的能力矩阵钉住。

use crate::model::Protocol;
use serde_json::{json, Value};

use super::tools_meta::*;
use super::util::{extract_text_content, uuid_like};

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

/// 流式请求补上 `stream_options.include_usage = true`（**仅对 Chat Completions 上游**）。
///
/// ## 为什么必须补
///
/// OpenAI Chat 的流式响应**默认不带 usage**：要拿 token 用量必须显式请求
/// `stream_options: {include_usage: true}`，上游才会在末尾多发一个只含 usage 的 chunk。
/// 不补的后果是「跨协议流式的 token 用量恒为 0」——用量页那一行永远是 0，
/// 而用户正拿它判断额度花在哪。这属于**静默失效**：不报错，只是数字一直不动。
///
/// Anthropic 与 Responses 上游不需要（前者 usage 在 message_start/message_delta 里、
/// 后者在 response.completed 里，都无需额外声明），故只在产出 Chat 请求体的两个转换函数里调。
///
/// **只在 stream 为真时加**：非流式响应本就带完整 usage，加了纯属多余字段，
/// 而部分中转站对未知字段严格（历史上 `_pending_effort` 就撞过 400）。
/// 用户已显式给了 `stream_options` 时不覆盖 —— 那是他自己的选择，可能刻意关掉了 usage。
fn request_usage_in_stream(dst: &mut serde_json::Map<String, Value>) {
    let streaming = dst.get("stream").and_then(|s| s.as_bool()).unwrap_or(false);
    if !streaming || dst.contains_key("stream_options") {
        return;
    }
    dst.insert("stream_options".into(), json!({ "include_usage": true }));
}

/// Chat 的 `response_format` → Responses 的 `text` 对象（结构化输出约束）。
///
/// ## 为什么必须转，而不是让 copy_through 带过去
///
/// 两协议表达「我要 JSON」的字段完全不同名：Chat 用顶层 `response_format`，
/// Responses 用 `text.format`（同一份 schema、不同位置）。`response_format` 从来没进过
/// `copy_through` 的白名单，于是 Chat→Responses 时它被**整个丢掉**。
///
/// 后果是纯静默失效里最难查的一种：请求 200、模型正常作答，只是回的是散文而不是 JSON，
/// 客户端在 `JSON.parse` 上炸掉。用户看到的是「我的程序解析失败」，而根因在代理的协议转换里
/// —— 日志、状态码、错误信息里没有任何线索。
///
/// ## json_schema 要**摊平**
///
/// Chat：`{"type":"json_schema","json_schema":{"name":…,"schema":…,"strict":true}}`
/// Responses：`{"format":{"type":"json_schema","name":…,"schema":…,"strict":true}}`
/// —— 内层 `json_schema` 包裹层没了，`name`/`schema`/`strict` 直接挂在 `format` 下。
///
/// 判据来源（非推测）：① Microsoft Learn 的结构化输出文档明确写「Chat Completions 在
/// `response_format` 里定义 schema，Responses 在 `text.format` 里定义」；② 本机
/// `codex.exe`（Responses 原生客户端）的 serde 字段名串里，`strict`/`schema`/`format`/
/// `json_schema` 是同级相邻字段，与摊平形态一致、与嵌套形态不一致。
///
/// 未知 `type` **原样搬进 format 而不是丢掉**：上游若不认会明确报错，那比静默降级成散文好 ——
/// 后者用户查不到，前者一次就定位。
fn chat_response_format_to_responses_text(rf: &Value) -> Option<Value> {
    let obj = rf.as_object()?;
    let mut format = serde_json::Map::new();
    match obj.get("type").and_then(|t| t.as_str()) {
        Some("json_schema") => {
            format.insert("type".into(), json!("json_schema"));
            // 摊平内层：把 name/schema/strict 及未来新增字段一并提上来。
            if let Some(inner) = obj.get("json_schema").and_then(|j| j.as_object()) {
                for (k, v) in inner {
                    format.insert(k.clone(), v.clone());
                }
            }
        }
        // json_object / text 及其它：只有 type，位置换一下即可
        _ => {
            for (k, v) in obj {
                format.insert(k.clone(), v.clone());
            }
        }
    }
    Some(json!({ "format": Value::Object(format) }))
}

/// Responses 的 `text.format` → Chat 的 `response_format`（[`chat_response_format_to_responses_text`] 的逆向）。
///
/// 反向同样漏过：Codex（Responses 客户端）配一个 Chat 协议的 Key 时，结构化输出约束
/// 一样会被丢掉。两向都补才对称 —— 只补一向的话，同一个功能在「哪种 Key」下可用
/// 取决于用户碰巧选了谁，而那是他最不该需要知道的事。
///
/// `json_schema` 要重新**包回**内层对象（`type` 留在外层，其余进 `json_schema`）。
fn responses_text_to_chat_response_format(text: &Value) -> Option<Value> {
    let format = text.get("format")?.as_object()?;
    let ty = format.get("type").and_then(|t| t.as_str())?;
    if ty != "json_schema" {
        return Some(json!({ "type": ty }));
    }
    let mut inner = serde_json::Map::new();
    for (k, v) in format {
        if k != "type" {
            inner.insert(k.clone(), v.clone());
        }
    }
    Some(json!({ "type": "json_schema", "json_schema": Value::Object(inner) }))
}

/// 从请求体里读出 OpenAI 推理强度档位。
///
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

/// 消费 `openai_to_anthropic` 暂存的 `_pending_effort`，在 max_tokens 补齐后补打 thinking。
///
/// **为什么需要这一步**（Codex→Anthropic 跨协议的扩展思考修复）：
/// thinking.budget_tokens 必须 < max_tokens，而 Codex 常态不发 max_tokens——转换时算不了，
/// 只能把 effort 暂存。proxy 层 `ensure_anthropic_output_budget` 按目标模型补完 max_tokens
/// 后调本函数，effort 才真正落成 thinking。不调它 = effort 被静默丢弃（扩展思考不生效）。
///
/// 无论是否命中，都会**移除** `_pending_effort` 这个内部中转字段——它绝不能发给上游
/// （Anthropic 会因未知字段 400，或至少是脏字段）。故对「已显式给 max_tokens、
/// 转换时已算过 thinking」的正常请求，这里只是清理一个不存在的键，无副作用。
pub fn apply_pending_thinking(payload: &mut Value, max_tokens: u64) {
    let Some(obj) = payload.as_object_mut() else { return };
    // take 出来即移除；无论后续是否开 thinking，这个中转字段都不留。
    let Some(effort) = obj.remove("_pending_effort").and_then(|v| v.as_str().map(String::from))
    else {
        return;
    };
    // 若 payload 已带 thinking（理论上不会——暂存路径与已算路径互斥），不覆盖。
    if obj.contains_key("thinking") {
        return;
    }
    if let Some(budget) = effort_to_thinking_budget(&effort, max_tokens) {
        obj.insert(
            "thinking".into(),
            json!({ "type": "enabled", "budget_tokens": budget }),
        );
        obj.insert("temperature".into(), json!(1));
        obj.remove("top_p");
    }
}

/// 兜底清除 `_pending_effort`：某些路径可能不走 `apply_pending_thinking`
/// （如同协议直通、或 max_tokens 已存在时根本没暂存），但只要它意外残留就绝不能发上游。
/// 幂等：字段不存在时无操作。
pub fn strip_pending_effort(payload: &mut Value) {
    if let Some(obj) = payload.as_object_mut() {
        obj.remove("_pending_effort");
    }
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
    // 输出上限**只在源请求真有时才带**：OpenAI Chat 的 max_tokens 是可选项，
    // 源没给就该省略。此前这里 `unwrap_or(4096)` 会凭空造一个上限 —— 那与
    // 「代理不替客户端决定输出长度」直接冲突（见 proxy::apply_key_params 的定调）。
    if let Some(mt) = body.get("max_tokens") {
        out.insert("max_tokens".into(), mt.clone());
    }

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
                    // 图片块 → OpenAI 的 `image_url` + inline data URL（形状与 session.rs 的
                    // `openai_user_content` 一致）。**必须翻译而不能丢**：此前 image 落进下面的
                    // `_ => {}` 被静默吞掉，上游只收到纯文字 → 模型回「我没有看到图片」，全程 200
                    // 无告警，用户只会怀疑模型或中转商；纯图消息还会让 text 为空、整条 user 消息
                    // 被跳过（messages 可能变空 → 上游 400）。跨协议是本项目核心能力，不是边缘路径。
                    let mut images: Vec<Value> = vec![];
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
                            Some("image") => {
                                // Anthropic: {"type":"image","source":{"type":"base64",
                                //             "media_type":"image/png","data":"…"}}
                                // OpenAI   : {"type":"image_url","image_url":{"url":"data:image/png;base64,…"}}
                                // 也兼容 source.type=="url"（部分中转商直接给外链）。
                                let src = b.get("source");
                                let url = match src.and_then(|s| s.get("type")).and_then(|t| t.as_str()) {
                                    Some("url") => src
                                        .and_then(|s| s.get("url"))
                                        .and_then(|u| u.as_str())
                                        .map(|u| u.to_string()),
                                    _ => {
                                        let mt = src
                                            .and_then(|s| s.get("media_type"))
                                            .and_then(|m| m.as_str())
                                            .unwrap_or("image/png");
                                        src.and_then(|s| s.get("data"))
                                            .and_then(|d| d.as_str())
                                            .map(|d| format!("data:{mt};base64,{d}"))
                                    }
                                };
                                if let Some(url) = url {
                                    images.push(json!({
                                        "type": "image_url",
                                        "image_url": { "url": url }
                                    }));
                                }
                            }
                            _ => {}
                        }
                    }
                    if role == "assistant" && !tool_calls.is_empty() {
                        // assistant + 工具调用：content 允许为 null
                        let c = if text.is_empty() { Value::Null } else { json!(text) };
                        messages.push(json!({ "role": "assistant", "content": c, "tool_calls": tool_calls }));
                    } else {
                        // **先 tool_results、后 user 文本**：一条 user 轮次可能同时含 tool_result 与
                        // text（如 [{tool_result},{text:"另外请…"}]）。OpenAI 要求 assistant.tool_calls
                        // 之后必须紧跟对应的 tool 消息，中间夹一条 role:"user" 会 400
                        // （"tool_call_ids did not have response messages"）。故先把 tool 结果贴上去
                        // 补齐上一条 assistant 的工具调用，再追加 user 文本（工具消息之后的 user 消息合法）。
                        messages.append(&mut tool_results);
                        if !images.is_empty() {
                            // 有图 → content 必须是**数组**（OpenAI 的多模态形态）：文本块在前、
                            // 图片块在后，与 session.rs 的 openai_user_content 顺序一致。
                            // 纯图（text 为空）也要发出去，否则整条消息被跳过、图片彻底消失。
                            let mut parts: Vec<Value> = vec![];
                            if !text.is_empty() {
                                parts.push(json!({ "type": "text", "text": text }));
                            }
                            parts.append(&mut images);
                            messages.push(json!({ "role": role, "content": parts }));
                        } else if !text.is_empty() {
                            messages.push(json!({ "role": role, "content": text }));
                        }
                    }
                }
                _ => {}
            }
        }
    }
    out.insert("messages".into(), json!(messages));

    // 采样/控制字段透传（键名两协议一致）。
    // `stream_options` 一并透传：它是 Chat 的合法字段，用户显式给了就该带过去 —— 不透传的话
    // 下面的 `request_usage_in_stream` 看不到它，会把用户「刻意关掉 usage」的设置顶掉。
    // （这条是写测试时发现的：静默丢弃用户显式设置，正是本项目最忌讳的形态。）
    copy_through(body, &mut out, &["temperature", "top_p", "stream", "stream_options"]);
    request_usage_in_stream(&mut out);
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
            // Anthropic（2024 末起）支持 {type:"none"} 禁用工具；OpenAI 的等价形式是**字符串** "none"，
            // 原样发对象 {"type":"none"} 会被严格 OpenAI 上游 400。
            Some("none") => json!("none"),
            // 未知形态不原样克隆（那会把 Anthropic 专有对象泄漏给 OpenAI 触发 400）——回落到 "auto"。
            _ => json!("auto"),
        };
        out.insert("tool_choice".into(), mapped);
    }
    Value::Object(out)
}

/// 将 OpenAI Chat Completions 请求体转为 Anthropic Messages 请求体。
pub fn openai_to_anthropic(body: &Value) -> Value {
    let mut out = serde_json::Map::new();
    out.insert("model".into(), body.get("model").cloned().unwrap_or(Value::Null));
    // Anthropic 的 `max_tokens` 必填，但**这里不负责凭空造一个**。
    //
    // 源请求给了就带上（含 Chat 的别名 `max_completion_tokens`）；源没给就先留空，
    // 由**知道目标 Key 与真实模型**的调用方按「窗口剩余 ∩ 模型最大输出」补齐
    // （见 `proxy::ensure_anthropic_max_tokens`）。此前这里 `unwrap_or(4096)`：
    // OpenAI 客户端本来没设上限，跨协议转到 Anthropic Key 后却被本地按 4096 截断，
    // 而用户只会以为是上游中转商的问题 —— 与本项目「不替客户端决定输出长度」的定调冲突。
    if let Some(mt) = body
        .get("max_tokens")
        .or_else(|| body.get("max_completion_tokens"))
    {
        out.insert("max_tokens".into(), mt.clone());
    }

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
    //
    // 只有已经有**真实输出上限**时才在这里打开 thinking：源请求没给 max_tokens 的跨协议
    // 情况，要等 proxy 层按目标模型窗口补齐后再判断（convert.rs 不知道 Key/真实模型，不能
    // 为了算 thinking 偷偷回退 4096）。
    if let Some(effort) = read_reasoning_effort(body) {
        match out.get("max_tokens").and_then(|v| v.as_u64()) {
            // 已有 max_tokens：直接算 thinking。
            Some(max_tokens) => {
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
            // 无 max_tokens（Codex 常态）：此刻算不了 thinking（budget 必须 < max_tokens），
            // 但**不能就此丢弃 effort** —— 否则 Codex→Anthropic 的扩展思考被静默关闭。
            // 暂存到私有字段，等 proxy::ensure_anthropic_output_budget 补完 max_tokens 后
            // 由 apply_pending_thinking 消费并移除。用 `_` 前缀标记这是内部中转字段、
            // 不该发给上游（apply_pending_thinking 会删掉它，兜底见那里）。
            None => {
                out.insert("_pending_effort".into(), json!(effort));
            }
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
            Value::String(s) if s == "none" => json!({ "type": "none" }), // Anthropic（2024末起）支持 none：禁用工具，勿再退回 auto（那会静默重新启用工具选择）
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
/// 生产非流式路径统一走 [`convert_response_ext`]（可带 custom / search 工具集合 + namespaces）；
/// 此简单签名保留供测试与无特殊工具场景，等价于 `convert_response_ext(.., &空, &空, &[])`。
#[allow(dead_code)]
pub fn convert_response(body: &Value, from: Protocol, to: Protocol) -> Value {
    convert_response_ext(
        body,
        from,
        to,
        &std::collections::HashSet::new(),
        &std::collections::HashSet::new(),
        &[],
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
    namespaces: &[String],
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
            chat_resp_to_responses_ext(&chat, custom_tools, search_tools, namespaces)
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
    // `stream_options` 一并透传，理由同 anthropic_to_openai 那处（尊重用户显式设置）。
    copy_through(body, &mut out, &["temperature", "top_p", "stream", "stream_options"]);
    request_usage_in_stream(&mut out);
    // 结构化输出：Responses 的 text.format → Chat 的 response_format（反向对称，
    // 见 responses_text_to_chat_response_format）。只补一向会让同一个功能在
    // 「哪种 Key」下可用取决于用户碰巧选了谁。
    if let Some(rf) = body.get("text").and_then(responses_text_to_chat_response_format) {
        out.insert("response_format".into(), rf);
    }
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
/// usage.{prompt,completion}_tokens → usage.{input,output}_tokens；
/// finish_reason:"length" → status:"incomplete"（A5-12 修复）。
pub fn chat_resp_to_responses(body: &Value) -> Value {
    let choice0 = body
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|arr| arr.first());
    let message = choice0.and_then(|c| c.get("message"));
    // finish_reason:"length" 表示截断，映射为 Responses status:"incomplete"（A5-12）
    let finish = choice0
        .and_then(|c| c.get("finish_reason"))
        .and_then(|r| r.as_str())
        .unwrap_or("stop");
    let resp_status = if finish == "length" { "incomplete" } else { "completed" };
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
        "status": resp_status,
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
    namespaces: &[String],
) -> Value {
    let mut resp = chat_resp_to_responses(body);
    if custom_tools.is_empty() && search_tools.is_empty() && namespaces.is_empty() {
        return resp;
    }
    if let Some(output) = resp.get_mut("output").and_then(|o| o.as_array_mut()) {
        for item in output.iter_mut() {
            if item.get("type").and_then(|t| t.as_str()) != Some("function_call") {
                continue;
            }
            let full = item
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_string();
            // 关键：Codex router 用结构化 {namespace, name} 查工具注册表，不拆 name 字符串。
            // 上游按展开的全名 `mcp__x__foo` 回调，这里必须拆回 name="foo" + namespace="mcp__x"
            // 两个独立字段，否则 router 查 {namespace:None, name:"mcp__x__foo"} 匹配不到 → unsupported
            // call。这与流式路径 SseTranslator::emit_responses_completed（sse.rs:396）口径一致；
            // 此前非流式漏了拆分，跨协议→Responses 的非流式响应里 MCP 工具全部认不出（本轮审查确认）。
            let (ns, real_name) = split_namespaced_tool_name(&full, namespaces);
            let Some(obj) = item.as_object_mut() else { continue };
            // custom / search 判定必须用**拆出的真实名**（与流式 sse.rs:401/425 一致），
            // 否则带 namespace 的 custom/search 工具也会被误判。
            if search_tools.contains(&real_name) {
                obj.insert("name".into(), json!(real_name));
                if let Some(ns) = ns {
                    obj.insert("namespace".into(), json!(ns));
                }
                rewrite_to_tool_search_call(obj);
            } else if custom_tools.contains(&real_name) {
                obj.insert("name".into(), json!(real_name));
                if let Some(ns) = ns {
                    obj.insert("namespace".into(), json!(ns));
                }
                obj.insert("type".into(), json!("custom_tool_call"));
                // 同流式路径：custom_tool_call 用裸字符串 `input`，不用 JSON `arguments`。
                let args = obj
                    .get("arguments")
                    .and_then(|a| a.as_str())
                    .unwrap_or("")
                    .to_string();
                obj.insert("input".into(), json!(unpack_custom_tool_input(&args)));
                obj.remove("arguments");
            } else {
                // 普通（含 namespace 展开的 MCP）function_call：拆出真实名 + namespace 两字段。
                obj.insert("name".into(), json!(real_name));
                if let Some(ns) = ns {
                    obj.insert("namespace".into(), json!(ns));
                }
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
pub(super) fn rewrite_to_tool_search_call(obj: &mut serde_json::Map<String, Value>) {
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
    // 结构化输出：Chat 的 response_format → Responses 的 text.format（见该函数的完整理由）。
    // 不转的话请求 200、模型回散文、客户端 JSON.parse 炸掉，而日志里毫无线索。
    if let Some(text) = body.get("response_format").and_then(chat_response_format_to_responses_text)
    {
        out.insert("text".into(), text);
    }
    // 反映射），原样带给 Responses 上游——它原生认 reasoning.effort，推理强度直达。
    // 若只有**顶层字符串** reasoning_effort（o/gpt-5 系 Chat API 的合法字段），也要归一成
    // reasoning:{effort:..}，否则 Chat→Responses 会把它整个丢掉、上游推理强度静默回落默认档
    // （反向 Responses→Chat 早已把 reasoning.effort 落成顶层 reasoning_effort，此向此前漏了对称处理）。
    if let Some(r) = body.get("reasoning").filter(|r| r.is_object()) {
        out.insert("reasoning".into(), r.clone());
    } else if let Some(effort) = read_reasoning_effort(body) {
        out.insert("reasoning".into(), json!({ "effort": effort }));
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

    // Responses status:"incomplete" → Chat finish_reason:"length"（A5-12 修复）
    let status = body.get("status").and_then(|s| s.as_str()).unwrap_or("completed");
    let finish = if !tool_calls.is_empty() {
        message.insert("tool_calls".into(), json!(tool_calls));
        "tool_calls"
    } else if status == "incomplete" {
        "length"
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::upstream::testfix::*;
    use crate::model::Protocol;
    use serde_json::json;

    /// 结构化输出约束必须跨协议**双向**保留，且 `json_schema` 的包裹层要正确摊平/包回。
    ///
    /// 不转的后果是最难查的一类静默失效：请求 200、模型正常作答、只是回的是散文而不是 JSON，
    /// 客户端在 `JSON.parse` 上炸掉。用户看到「我的程序解析失败」，而根因在代理的协议转换里
    /// —— 状态码、日志、错误信息里没有任何线索。
    ///
    /// 钉四件事：
    /// 1. Chat→Responses：`response_format` → `text.format`，且内层 `json_schema` **摊平**
    ///    （`name`/`schema`/`strict` 直接挂 `format` 下）；
    /// 2. 反向 Responses→Chat：`text.format` → `response_format`，`json_schema` **包回**内层
    ///    （只补一向会让同一功能在「哪种 Key」下可用取决于用户碰巧选了谁）；
    /// 3. `json_object` 这类只有 type 的形态两向都要过；
    /// 4. 没给约束时**不得**凭空造出 `text`/`response_format`（多余字段撞过严格中转站的 400）。
    #[test]
    fn structured_output_constraint_survives_both_directions() {
        let schema = json!({
            "type": "json_schema",
            "json_schema": {
                "name": "reply",
                "strict": true,
                "schema": { "type": "object", "properties": { "a": { "type": "string" } } }
            }
        });

        // 1) Chat → Responses：摊平
        let resp = chat_to_responses(&json!({
            "model": "m",
            "messages": [{"role": "user", "content": "hi"}],
            "response_format": schema
        }));
        let fmt = &resp["text"]["format"];
        assert_eq!(fmt["type"], "json_schema", "结构化输出约束不得被丢掉");
        assert_eq!(fmt["name"], "reply", "name 必须摊平到 format 下，而不是留在 json_schema 里");
        assert_eq!(fmt["strict"], true);
        assert_eq!(fmt["schema"]["type"], "object");
        assert!(
            resp["text"]["format"].get("json_schema").is_none(),
            "内层包裹层必须消失 —— Responses 的 text.format 是摊平形态"
        );

        // 2) Responses → Chat：包回
        let chat = responses_to_chat(&json!({
            "model": "m",
            "input": [],
            "text": { "format": {
                "type": "json_schema", "name": "reply", "strict": true,
                "schema": { "type": "object" }
            }}
        }));
        assert_eq!(chat["response_format"]["type"], "json_schema");
        assert_eq!(
            chat["response_format"]["json_schema"]["name"], "reply",
            "Chat 形态里 name 必须重新包进 json_schema"
        );
        assert!(
            chat["response_format"].get("name").is_none(),
            "Chat 的 response_format 顶层只有 type，其余在 json_schema 内"
        );

        // 3) 只有 type 的形态（json_object）两向都过
        let jo = chat_to_responses(&json!({
            "model": "m", "messages": [], "response_format": {"type": "json_object"}
        }));
        assert_eq!(jo["text"]["format"]["type"], "json_object");
        let back = responses_to_chat(&json!({
            "model": "m", "input": [], "text": {"format": {"type": "json_object"}}
        }));
        assert_eq!(back["response_format"]["type"], "json_object");

        // 4) 没给约束时不得凭空造字段
        let bare = chat_to_responses(&json!({"model": "m", "messages": []}));
        assert!(bare.get("text").is_none(), "没要求结构化输出就不该出现 text");
        let bare2 = responses_to_chat(&json!({"model": "m", "input": []}));
        assert!(
            bare2.get("response_format").is_none(),
            "多余字段撞过严格中转站的 400，不能凭空加"
        );
    }

    /// P2-5：`convert_request_owned` 同协议时必须**零拷贝原样返回**，跨协议时与
    /// `convert_request` 结果完全一致。
    ///
    /// 「原样」是关键语义：同协议路径是最常见场景（Claude Code→Anthropic Key、
    /// Codex→Responses Key），此时请求体应逐字节透传，任何隐式改写都会让
    /// count_tokens 等子路径行为偏离。
    /// **跨协议流式必须显式索要 usage**，否则 token 用量恒为 0（P2）。
    ///
    /// OpenAI Chat 的流式响应默认不带 usage：必须请求 `stream_options.include_usage = true`，
    /// 上游才会在末尾多发一个只含 usage 的 chunk。不补的后果是用量页那一行永远显示 0，
    /// 而用户正拿它判断额度花在哪 —— 典型的静默失效（不报错、数字就是不动）。
    ///
    /// 三条边界一并钉住：非流式不加（本就带完整 usage，多加字段还可能撞严格中转站的 400）、
    /// 用户已给 `stream_options` 时不覆盖（那是他的选择）、只对 Chat 上游加
    /// （Anthropic/Responses 的 usage 在各自事件里，无需声明）。
    ///
    /// 故障注入判据：删掉 `request_usage_in_stream` 的两个调用点，前两条断言立即变红。
    #[test]
    fn cross_protocol_streaming_requests_usage_from_chat_upstream() {
        // ① Anthropic 下游 → Chat 上游，流式：必须补 include_usage
        let a_stream = json!({
            "model": "m", "max_tokens": 100, "stream": true,
            "messages": [{ "role": "user", "content": "hi" }]
        });
        let out = anthropic_to_openai(&a_stream);
        assert_eq!(
            out["stream_options"]["include_usage"], json!(true),
            "Chat 上游流式必须索要 usage，否则用量恒为 0:\n{out}"
        );

        // ② Responses 下游（Codex）→ Chat 上游，流式：同样要补
        let r_stream = json!({
            "model": "gpt-5", "stream": true,
            "input": [{ "type": "message", "role": "user",
                        "content": [{ "type": "input_text", "text": "hi" }] }]
        });
        let out = responses_to_chat(&r_stream);
        assert_eq!(
            out["stream_options"]["include_usage"], json!(true),
            "Responses→Chat 流式同样必须索要 usage:\n{out}"
        );

        // ③ 非流式不加：响应本就带完整 usage，多余字段可能撞严格中转站的 400
        let a_plain = json!({
            "model": "m", "max_tokens": 100,
            "messages": [{ "role": "user", "content": "hi" }]
        });
        let out = anthropic_to_openai(&a_plain);
        assert!(
            out.get("stream_options").is_none(),
            "非流式不该加 stream_options:\n{out}"
        );

        // ④ 用户已显式给了 stream_options → 不覆盖（他可能刻意关掉了 usage）
        let explicit = json!({
            "model": "m", "max_tokens": 100, "stream": true,
            "stream_options": { "include_usage": false },
            "messages": [{ "role": "user", "content": "hi" }]
        });
        let out = anthropic_to_openai(&explicit);
        assert_eq!(
            out["stream_options"]["include_usage"], json!(false),
            "用户显式设置必须被尊重:\n{out}"
        );
    }

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
    fn o2a_omitted_cap_stays_omitted_for_key_aware_proxy_to_fill() {
        let body = json!({
            "model": "claude-sonnet-4-5",
            "messages": [{ "role": "user", "content": "hi" }]
        });
        let a = openai_to_anthropic(&body);
        assert!(
            a.get("max_tokens").is_none(),
            "转换器没有 Key/真实模型能力数据，绝不得凭空造 4096；proxy 层会补 Anthropic 必填值"
        );
    }

    #[test]
    fn a2o_omitted_cap_stays_omitted_for_optional_openai_target() {
        let body = json!({
            "model": "gpt-x",
            "messages": [{ "role": "user", "content": "hi" }]
        });
        let o = anthropic_to_openai(&body);
        assert!(
            o.get("max_tokens").is_none(),
            "OpenAI 输出上限是可选项，源没给就保持省略，不能凭空造 4096"
        );
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

    /// P2-2 回归：Codex→Anthropic 常态（有 effort、**无 max_tokens**）。
    ///
    /// 转换时算不了 thinking（budget 必须 < max_tokens），但 effort **不能丢**——
    /// 暂存到 `_pending_effort`，等 proxy 补完 max_tokens 后再落。这条钉住「暂存」这一步：
    /// 此刻不该有 thinking，但必须有 `_pending_effort`。
    #[test]
    fn codex_effort_without_max_tokens_is_stashed_not_dropped() {
        let chat = json!({
            "model": "claude-opus-4-8",
            "messages": [ { "role": "user", "content": "hi" } ],
            "reasoning": { "effort": "high" }
        });
        let a = openai_to_anthropic(&chat);
        assert!(
            a.get("thinking").is_none(),
            "无 max_tokens 时算不了 thinking（budget 必须 < max_tokens）"
        );
        assert_eq!(
            a["_pending_effort"], "high",
            "effort 必须暂存，否则 Codex→Anthropic 扩展思考被静默丢弃"
        );
    }

    /// P2-2 回归：apply_pending_thinking 在 max_tokens 补齐后把暂存的 effort 落成 thinking，
    /// 并**移除** `_pending_effort`（该中转字段绝不能发上游）。
    #[test]
    fn apply_pending_thinking_derives_and_removes_sentinel() {
        // 模拟 proxy 补完 max_tokens 后的 payload（转换阶段暂存了 effort）
        let mut payload = json!({
            "model": "claude-opus-4-8",
            "max_tokens": 64000,
            "messages": [ { "role": "user", "content": "hi" } ],
            "temperature": 0.5,
            "top_p": 0.9,
            "_pending_effort": "high"
        });
        apply_pending_thinking(&mut payload, 64000);
        assert_eq!(payload["thinking"]["type"], "enabled", "补完 max_tokens 后应落 thinking");
        assert!(payload["thinking"]["budget_tokens"].as_u64().unwrap() > 0);
        assert_eq!(payload["temperature"], 1, "开思考须归一 temperature=1");
        assert!(payload.get("top_p").is_none(), "开思考须去 top_p");
        assert!(
            payload.get("_pending_effort").is_none(),
            "中转字段必须被移除，绝不能发给上游（Anthropic 会因未知字段报错）"
        );
    }

    /// P2-2 回归：minimal 档暂存后，apply_pending_thinking 不开思考，但仍清掉中转字段。
    #[test]
    fn apply_pending_thinking_minimal_still_strips_sentinel() {
        let mut payload = json!({
            "model": "claude-opus-4-8",
            "max_tokens": 64000,
            "_pending_effort": "minimal"
        });
        apply_pending_thinking(&mut payload, 64000);
        assert!(payload.get("thinking").is_none(), "minimal 不开思考");
        assert!(
            payload.get("_pending_effort").is_none(),
            "即便不开思考，中转字段也必须清掉"
        );
    }

    /// P2-2 安全网：strip_pending_effort 幂等，且能兜住不走 apply 的路径。
    #[test]
    fn strip_pending_effort_is_idempotent_and_thorough() {
        let mut with = json!({ "max_tokens": 100, "_pending_effort": "high" });
        strip_pending_effort(&mut with);
        assert!(with.get("_pending_effort").is_none(), "必须移除中转字段");
        // 幂等：再调一次、以及对本就没有该字段的 payload 调，都不报错、不改别的
        strip_pending_effort(&mut with);
        let mut without = json!({ "max_tokens": 100 });
        strip_pending_effort(&mut without);
        assert_eq!(without["max_tokens"], 100, "不该动其它字段");
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
    fn chat_to_responses_lifts_top_level_reasoning_effort() {
        // 回归（对抗审查确认的 P2）：Chat 下游发**顶层字符串** reasoning_effort（o/gpt-5 系 Chat API
        // 合法字段），Chat→Responses 必须归一成 reasoning:{effort:..}，否则整个丢掉、上游推理强度
        // 静默回落默认档（反向 Responses→Chat 早有对称处理，此向此前漏了）。
        let chat = json!({
            "model": "gpt-5",
            "messages": [ { "role": "user", "content": "hi" } ],
            "reasoning_effort": "high"
        });
        let r = chat_to_responses(&chat);
        assert_eq!(
            r["reasoning"]["effort"], "high",
            "顶层 reasoning_effort 必须被提升成 reasoning.effort：{r}"
        );
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

    /// A5-12 回归：Chat finish_reason:"length" → Responses status:"incomplete"。
    ///
    /// 此前一律硬写 `"completed"`，截断信号在跨协议路径（Codex → Chat 上游）完全丢失。
    /// 故障注入验证：把判定从 `"length"` 改成 `"xxx"` 后，此测试的 `status` 断言变红。
    #[test]
    fn chat_resp_to_responses_length_becomes_incomplete() {
        let resp = json!({
            "id": "chatcmpl-trunc",
            "model": "gpt-4o",
            "choices": [ {
                "index": 0,
                "message": { "role": "assistant", "content": "部分回答…" },
                "finish_reason": "length"
            } ],
            "usage": { "prompt_tokens": 10, "completion_tokens": 50 }
        });
        let out = chat_resp_to_responses(&resp);
        assert_eq!(
            out["status"], "incomplete",
            "Chat finish_reason:\"length\" 必须映射为 Responses status:\"incomplete\""
        );
    }

    /// A5-12 回归（反向）：Responses status:"incomplete" → Chat finish_reason:"length"。
    ///
    /// 此前 `responses_resp_to_chat` 对 `finish_reason` 只有 `"tool_calls"` 和 `"stop"` 两条路，
    /// `"incomplete"` 被静默当成普通完成。
    #[test]
    fn responses_resp_to_chat_incomplete_becomes_length() {
        let resp = json!({
            "id": "resp-trunc",
            "model": "gpt-5",
            "status": "incomplete",
            "output": [ {
                "type": "message",
                "role": "assistant",
                "status": "completed",
                "content": [ { "type": "output_text", "text": "部分回答…" } ]
            } ],
            "usage": { "input_tokens": 10, "output_tokens": 50, "total_tokens": 60 }
        });
        let out = responses_resp_to_chat(&resp);
        let finish = out["choices"][0]["finish_reason"].as_str().unwrap_or("MISSING");
        assert_eq!(
            finish, "length",
            "Responses status:\"incomplete\" 必须映射为 Chat finish_reason:\"length\""
        );
    }

    /// is_truncated_response 辅助函数：三种协议的截断信号都能识别。
    #[test]
    fn is_truncated_detects_all_three_signals() {
        // Anthropic Messages
        let a = serde_json::json!({
            "stop_reason": "max_tokens",
            "content": [{"type":"text","text":"hi"}]
        });
        assert!(crate::upstream::is_truncated_response(&a), "Anthropic max_tokens 必须检出");

        // OpenAI Chat
        let o = serde_json::json!({
            "choices": [{"finish_reason": "length", "message": {"content": "hi"}}]
        });
        assert!(crate::upstream::is_truncated_response(&o), "OpenAI length 必须检出");

        // Responses API
        let r = serde_json::json!({
            "status": "incomplete",
            "output": []
        });
        assert!(crate::upstream::is_truncated_response(&r), "Responses incomplete 必须检出");

        // 正常结束不算截断
        let n = serde_json::json!({
            "stop_reason": "end_turn",
            "choices": [{"finish_reason": "stop"}],
            "status": "completed"
        });
        assert!(!crate::upstream::is_truncated_response(&n), "正常结束不得误判截断");
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

    #[test]
    fn tool_choice_none_maps_both_directions() {
        // 回归（对抗审查确认）：Anthropic {type:"none"}（禁用工具）→ OpenAI 是**字符串** "none"，
        // 原样发对象会被严格 OpenAI 上游 400。
        let anthropic = json!({
            "model": "m", "max_tokens": 16,
            "messages": [ { "role": "user", "content": "go" } ],
            "tools": [ { "name": "f", "input_schema": { "type": "object" } } ],
            "tool_choice": { "type": "none" }
        });
        let to_chat = convert_request(&anthropic, Protocol::Anthropic, Protocol::OpenaiChat);
        assert_eq!(to_chat["tool_choice"], json!("none"), "Anthropic none → OpenAI 字符串 none");

        // 反向：OpenAI "none" → Anthropic {type:"none"}，不得退回 auto（那会静默重新启用工具）。
        let chat = json!({
            "model": "m",
            "messages": [ { "role": "user", "content": "go" } ],
            "tools": [ { "type": "function", "function": { "name": "f", "parameters": { "type": "object" } } } ],
            "tool_choice": "none"
        });
        let to_anthropic = convert_request(&chat, Protocol::OpenaiChat, Protocol::Anthropic);
        assert_eq!(
            to_anthropic["tool_choice"], json!({ "type": "none" }),
            "OpenAI none → Anthropic none（不得降级成 auto 重新启用工具）"
        );
    }

    /// **图片块不得被静默丢弃**（本轮审计 P1）。
    ///
    /// Claude Code 粘一张报错截图、而该分类的 Key 是 OpenAI-Chat 协议时，原先 image 块落进
    /// block 匹配的 `_ => {}` 被吞掉：上游只收到纯文字 → 模型回「我没有看到图片」，全程 200
    /// 无任何告警，用户只会怀疑模型或中转商。纯图消息更糟：text 为空导致整条 user 消息被跳过，
    /// messages 可能变空 → 上游直接 400。
    ///
    /// 故障注入判据：删掉 `Some("image")` 分支或 `!images.is_empty()` 那段，本测试立即变红。
    #[test]
    fn anthropic_to_openai_translates_image_blocks_instead_of_dropping() {
        // ① 文本 + 图片：content 应为数组，文本在前、image_url 在后
        let body = json!({
            "model": "m",
            "messages": [{ "role": "user", "content": [
                { "type": "text", "text": "这个报错怎么回事" },
                { "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": "AAAB" } }
            ]}]
        });
        let out = anthropic_to_openai(&body);
        let msgs = out["messages"].as_array().expect("应有 messages");
        assert_eq!(msgs.len(), 1, "一条 user 轮次 → 一条消息:\n{out}");
        let parts = msgs[0]["content"].as_array().expect("有图时 content 必须是数组");
        assert_eq!(parts[0]["type"], "text");
        assert_eq!(parts[1]["type"], "image_url");
        assert_eq!(
            parts[1]["image_url"]["url"], "data:image/png;base64,AAAB",
            "base64 图片应转成 inline data URL"
        );

        // ② 纯图（无文本）：整条消息**必须仍然发出**，否则 messages 变空触发上游 400
        let only_img = json!({
            "model": "m",
            "messages": [{ "role": "user", "content": [
                { "type": "image", "source": { "type": "base64", "media_type": "image/jpeg", "data": "ZZZ" } }
            ]}]
        });
        let out = anthropic_to_openai(&only_img);
        let msgs = out["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 1, "纯图消息不得被整条跳过:\n{out}");
        let parts = msgs[0]["content"].as_array().unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0]["image_url"]["url"], "data:image/jpeg;base64,ZZZ");

        // ③ 无图时保持原有形态（content 仍是纯字符串，不得无谓改成数组）
        let plain = json!({
            "model": "m",
            "messages": [{ "role": "user", "content": [{ "type": "text", "text": "hi" }] }]
        });
        let out = anthropic_to_openai(&plain);
        assert_eq!(out["messages"][0]["content"], json!("hi"), "无图时不改变既有形态");
    }

    #[test]
    fn anthropic_to_openai_orders_tool_result_before_user_text() {
        // 回归（对抗审查确认的 P2）：一条 user 轮次同时含 tool_result 与 text 时，OpenAI 要求
        // assistant.tool_calls 后必须紧跟对应 tool 消息。若在中间插一条 role:"user" 文本 → 400。
        // 故产出顺序必须是 [assistant(tool_calls), tool(result), user(text)]。
        let anthropic = json!({
            "model": "m", "max_tokens": 16,
            "messages": [
                { "role": "user", "content": "调用工具" },
                { "role": "assistant", "content": [
                    { "type": "tool_use", "id": "toolu_1", "name": "f", "input": {} }
                ] },
                { "role": "user", "content": [
                    { "type": "tool_result", "tool_use_id": "toolu_1", "content": "结果" },
                    { "type": "text", "text": "另外请顺便总结一下" }
                ] }
            ]
        });
        let chat = anthropic_to_openai(&anthropic);
        let msgs = chat["messages"].as_array().expect("messages 数组");
        // 定位 assistant(tool_calls) 的下标，其后必须是 tool 消息，再后才能是 user 文本。
        let asst_idx = msgs.iter().position(|m| m.get("tool_calls").is_some()).expect("应有 assistant.tool_calls");
        assert_eq!(
            msgs[asst_idx + 1]["role"], "tool",
            "assistant.tool_calls 之后必须紧跟 tool 消息，不能夹 user 文本：{msgs:?}"
        );
        assert_eq!(msgs[asst_idx + 1]["tool_call_id"], "toolu_1");
        // user 文本应排在 tool 消息之后（合法），且内容保留。
        let user_text = msgs.iter().skip(asst_idx + 1).find(|m| m["role"] == "user").expect("user 文本应保留");
        assert_eq!(user_text["content"], "另外请顺便总结一下");
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
        let result = chat_resp_to_responses_ext(&body, &Default::default(), &search, &[]);
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
        let result = chat_resp_to_responses_ext(&body, &Default::default(), &search, &[]);
        let item = &result["output"][0];
        assert_eq!(item["type"], "tool_search_call");
        assert_eq!(item["arguments"]["query"], "synaroute_ai", "不可解析时退化为 query 原文");
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
        let result = chat_resp_to_responses_ext(&body, &custom, &Default::default(), &[]);
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
        let result = chat_resp_to_responses_ext(&body, &custom, &Default::default(), &[]);
        let output = result["output"].as_array().unwrap();
        assert_eq!(output.len(), 1);
        assert_eq!(output[0]["type"], "function_call", "非 custom 工具应保持 function_call");
    }

    #[test]
    fn nonstreaming_cross_protocol_to_responses_splits_namespaced_tool_name() {
        // 回归（全业务对抗审查确认的 P1）：非流式 Anthropic→Responses 回程，MCP 工具的展开全名
        // `mcp__synaroute__synaroute_ai` 必须拆成 name="synaroute_ai" + namespace="mcp__synaroute"
        // 两个独立字段（与流式 sse.rs:396 口径一致）。此前非流式不拆 → Codex router 查
        // {namespace:None, name:"mcp__synaroute__synaroute_ai"} 匹配不到 → unsupported call，
        // 大脑聚合在非流式 Responses 请求上失效。
        let anthropic_resp = json!({
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "model": "claude-x",
            "stop_reason": "tool_use",
            "content": [
                { "type": "tool_use", "id": "toolu_1",
                  "name": "mcp__synaroute__synaroute_ai",
                  "input": { "prompt": "比较快排与归并" } }
            ],
            "usage": { "input_tokens": 10, "output_tokens": 5 }
        });
        let namespaces = vec!["mcp__synaroute".to_string()];
        let out = convert_response_ext(
            &anthropic_resp,
            Protocol::Anthropic,
            Protocol::OpenaiResponses,
            &Default::default(),
            &Default::default(),
            &namespaces,
        );
        let items = out["output"].as_array().expect("Responses 输出应为数组");
        let fc = items
            .iter()
            .find(|i| i["type"] == "function_call")
            .expect("应有 function_call item");
        assert_eq!(
            fc["name"], "synaroute_ai",
            "全名必须拆出真实名，否则 Codex router 认不出：{fc}"
        );
        assert_eq!(
            fc["namespace"], "mcp__synaroute",
            "namespace 必须作为独立字段带上：{fc}"
        );
    }

    #[test]
    fn nonstreaming_namespaced_custom_tool_classified_by_real_name() {
        // custom/search 判定必须用**拆出的真实名**（对齐流式 sse.rs:401/425）：带 namespace 的
        // custom 工具全名不拆就会漏判、仍输出 function_call 而非 custom_tool_call。
        let chat_resp = json!({
            "choices": [{ "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{ "id": "tc1", "type": "function",
                    "function": { "name": "mcp__ns__apply_patch", "arguments": "{\"input\":\"*** Begin Patch\"}" } }]
            }, "finish_reason": "tool_calls" }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 1 }
        });
        let custom = std::collections::HashSet::from(["apply_patch".to_string()]);
        let namespaces = vec!["mcp__ns".to_string()];
        let result = chat_resp_to_responses_ext(&chat_resp, &custom, &Default::default(), &namespaces);
        let item = &result["output"].as_array().unwrap()[0];
        assert_eq!(item["type"], "custom_tool_call", "按真实名 apply_patch 应判为 custom：{item}");
        assert_eq!(item["name"], "apply_patch");
        assert_eq!(item["namespace"], "mcp__ns");
        assert_eq!(item["input"], "*** Begin Patch");
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
}
