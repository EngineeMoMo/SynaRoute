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
mod completion;
mod discovery;
mod endpoint;
mod probe;
mod session;
mod sse;
mod usage;
mod util;

pub use client::shared_client;
pub use completion::text_completion;
pub use discovery::fetch_models;
pub use probe::{health_probe, health_probe_real};
pub use sse::{sse_direction, SseTranslator};
pub use session::{
    ImagePart, MultimodalPrompt, ToolDef, ToolInvocation, ToolResultMsg, ToolSession, TurnOutcome,
    TurnParams,
};
pub use endpoint::join_endpoint;
pub use usage::{extract_usage, with_usage, TokenUsage};

// 子模块里被本文件使用的项。`pub(super)` 的项对父模块可见需要显式 use ——
// Rust 的私有项可见性只向**下**流（父的私有项对子可见），反向必须显式提升并引入。
use util::{extract_text_content, uuid_like};

use crate::model::Protocol;
use serde_json::{json, Value};


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


/// 测试共用的 SSE 解析 helper（P2-1：多个子模块的测试都要用，放这里避免复制后分叉）。
#[cfg(test)]
mod testfix {
    use serde_json::Value;

    /// 把 SSE 文本里的所有 `data:` 行解析成 JSON 值，便于对结构做精确断言
    /// （避免用子串匹配蒙对——子串能被无关字段碰巧满足）。
    pub(super) fn sse_events(raw: &str) -> Vec<Value> {
        raw.lines()
            .filter_map(|l| l.trim().strip_prefix("data:"))
            .map(str::trim)
            .filter(|d| !d.is_empty() && *d != "[DONE]")
            .filter_map(|d| serde_json::from_str::<Value>(d).ok())
            .collect()
    }

    /// 取出 Anthropic 流里所有 `tool_use` 型 content_block_start 的 (index, id, name)。
    pub(super) fn anthropic_tool_blocks(raw: &str) -> Vec<(u64, String, String)> {
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
