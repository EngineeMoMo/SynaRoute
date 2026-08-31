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
// 契约由 crate 根的 upstream_api_surface 守卫在编译期兜住（**必须在 upstream 外面**，
// 否则私有项对后代可见会让守卫恒真，详见该文件注释）。
mod budget;
mod cache;
mod client;
mod completion;
mod convert;
mod discovery;
mod endpoint;
mod probe;
mod session;
mod sse;
mod error_hint;
mod stream_idle;
/// 上游因思考签名验不过而拒绝时的请求整流。放在 `upstream` 而不是 `proxy` 下：
/// 它修的是「上游对请求体的兼容性要求」，与协议适配同一类事（cc-switch 也放在代理层）。
mod thinking_rectify;
mod tools_meta;
mod usage;
mod util;

/// 大脑聚合的输出预算（协议感知）。代理转发不用它——转发一律透明，见 proxy::apply_key_params。
pub use budget::{
    anthropic_required_max_tokens, estimate_json_tokens_without_image_transport, estimate_tokens,
    output_budget,
};
pub use client::shared_client;
/// 带自动解压的自建请求客户端（余额查询等直接取它，不经 build_client）。
pub use client::decoding_client;
/// 转发用的 per-Key 响应超时。流式探头阶段与非流式共用同一口径（见 proxy.rs 两处调用）。
pub use client::key_timeout;
/// 自建请求的客户端身份头（UA 等）。缺它会被部分中转渠道判 `detected: unknown` 而 403。
pub use client::apply_client_identity;
pub use completion::text_completion;
pub use discovery::fetch_models;
pub use probe::{health_probe, health_probe_real};
pub use sse::{sse_direction, SseTranslator};
pub(crate) use stream_idle::guard as guard_stream_idle;
pub(crate) use error_hint::annotate as annotate_upstream_error;
pub(crate) use thinking_rectify::rectify_on_signature_error as rectify_thinking_signature;
pub use convert::{
    apply_pending_thinking, convert_request_owned, convert_response_ext, strip_pending_effort,
};
pub use tools_meta::{collect_custom_tools, collect_search_tools, collect_tool_namespaces};
pub use session::{
    ImagePart, MultimodalPrompt, ToolDef, ToolInvocation, ToolResultMsg, ToolSession, TurnOutcome,
    TurnParams,
};
pub use endpoint::join_endpoint;
pub use usage::{extract_usage, extract_usage_from_sse, with_usage, TokenUsage};

/// 上游给的 `tool_calls[].index` → 我们内部的槽位下标。**越界返回 `None`，调用方必须跳过。**
///
/// # 🔴 为什么必须有上限：这是一条上游可触发的内存耗尽
///
/// `sse.rs` 的 Chat 增量累积写的是 `while self.tool_calls.len() <= idx { push(...) }` ——
/// 也就是**上游响应里一个整数直接决定我们分配多少内存**。上游发
/// `{"index": 4294967295}` 就能让我们 push 40 亿个 `(String, String, String)`
/// （每个至少 72 字节 → 数百 GB），进程当场被 OOM 杀掉。
///
/// 这不是理论情形：用户接的是**第三方中转站**，那正是这条链路上不可信的一方，
/// 而本仓在别处已经对上游做过同类防护（`TAIL_WINDOW_BYTES` / `REQ_LOG_CAP`）。
///
/// # 为什么是「跳过」而不是「钳制到上限」
///
/// 钳制会把不同 index 的增量挤进同一个槽，拼出一个**参数被拼接错的工具调用** ——
/// 那比丢掉一条增量糟得多（客户端会拿着错参数真的去执行）。
///
/// 上限 256 远宽于现实：OpenAI 并行工具调用实测不超过几十个。越界只警告一次
/// （同 `log_rotate::give_up_rolling` 的做法）—— 它每个 chunk 都可能触发，
/// 刷屏会把真正有用的日志挤掉。
pub(crate) fn tool_slot(index: Option<&serde_json::Value>) -> Option<usize> {
    const MAX_TOOL_SLOTS: u64 = 256;
    let idx = index.and_then(serde_json::Value::as_u64).unwrap_or(0);
    if idx >= MAX_TOOL_SLOTS {
        static WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if !WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
            tracing::warn!("上游返回的 tool_call index={idx} 超出上限，已忽略该增量");
        }
        return None;
    }
    Some(idx as usize)
}

/// 判断上游非流式响应体里是否出现截断信号。
///
/// - OpenAI Chat: `choices[0].finish_reason == "length"`
/// - Anthropic Messages: `stop_reason == "max_tokens"`
/// - Responses API: `status == "incomplete"`（由 Codex / Responses 下游使用）
///
/// 用于 proxy.rs 的 `log_success` 路径，向 `RequestTrace.was_truncated` 写入截断标志，
/// 从而在运行日志里对用户可见（A5-11/A5-12 修复）。
pub fn is_truncated_response(body: &serde_json::Value) -> bool {
    // Anthropic Messages
    if body.get("stop_reason").and_then(|r| r.as_str()) == Some("max_tokens") {
        return true;
    }
    // OpenAI Chat Completions
    if body.get("choices")
        .and_then(|c| c.as_array())
        .and_then(|arr| arr.first())
        .and_then(|c| c.get("finish_reason"))
        .and_then(|r| r.as_str()) == Some("length")
    {
        return true;
    }
    // Responses API
    if body.get("status").and_then(|s| s.as_str()) == Some("incomplete") {
        return true;
    }
    false
}

/// 流式 SSE（末尾窗口）里是否出现**上游报错事件**。
///
/// 上游可能先回 200 响应头、随后在 SSE 流内发错误（Anthropic 过载 / 生成中报错的常见形态：
/// `event: error` + `data: {"type":"error","error":{"type":"overloaded_error",...}}`；
/// OpenAI 兼容流有时也在末尾 chunk 塞 `{"error":{...}}`）。这类「200 后流内失败」若不识别，
/// 会被当成成功记账（清 fail_count、解除短路窗口），使熔断/短路在流式主路径上零保护。
///
/// 用于 proxy.rs 同协议流式的流末补记：命中则 `record_live_failure` 而非坐实 success。
/// 只看**已缓存的尾部窗口**（8KB）——终止性错误事件必在流末，尾窗必然覆盖到。
pub fn sse_stream_errored(sse: &str) -> bool {
    for line in sse.lines() {
        let Some(data) = line.trim().strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(data.as_bytes()) else {
            continue;
        };
        // Anthropic：顶层 {"type":"error",...}；OpenAI 兼容：顶层含非空 "error" 对象。
        if v.get("type").and_then(|t| t.as_str()) == Some("error") {
            return true;
        }
        if v.get("error").map(|e| !e.is_null()).unwrap_or(false) {
            return true;
        }
    }
    false
}

// 子模块里被本文件使用的项。`pub(super)` 的项对父模块可见需要显式 use ——
// Rust 的私有项可见性只向**下**流（父的私有项对子可见），反向必须显式提升并引入。





/// 测试共用的 SSE 解析 helper（P2-1：多个子模块的测试都要用，放这里避免复制后分叉）。
#[cfg(test)]
mod testfix {
    use serde_json::{json, Value};

    /// Claude 桌面端（3p）真实请求骨架：Anthropic 协议，顶层 `tools` 里带 MCP 工具全名，
    /// 历史里含一轮完整的 tool_use → tool_result。这是 2026-07-30 实机场景的最小复现。
    pub(super) fn claude_desktop_request_with_mcp_tool() -> Value {
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

    /// Codex 桌面端（26.x / gpt-5.6 系）真实请求骨架：顶层**没有** `tools`，工具全在
    /// `input[0] = {"type":"additional_tools","role":"developer","tools":[…]}`。
    /// 抓包来源：`~/.codex/logs_2.sqlite` 的 `codex_http_client::transport` 行（2026-07-30）。
    pub(super) fn codex_desktop_request() -> Value {
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

    /// Codex direct 模式（`tool_mode: null` 的模型，如 gpt-5.5）真实请求骨架。
    /// 抓包来源：`~/.codex/logs_2.sqlite` 的 `codex_http_client::transport` 行（2026-07-30）。
    ///
    /// 两个关键实证：
    /// 1. `tool_search` 声明**没有 `name` 字段**（名字即 type），`execution:"client"`；
    /// 2. MCP 工具（`mcp__*` namespace）**从不出现在顶层 tools**——59 条含 `mcp__synaroute`
    ///    的抓包请求里顶层命中数为 0；它只在 `tool_search_output.tools[]` 里回灌。
    pub(super) fn codex_direct_request_with_search() -> Value {
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

    // ---- 真实抓包回放（非手写夹具）----
    //
    // 上面的用例用手写夹具，风险是「把结构写成我以为的样子」。这里直接回放 Codex 真发出来的
    // 请求体：来自 `~/.codex/logs_2.sqlite` 的 `codex_http_client::transport` 行
    // （2026-07-30 实机会话，Codex 桌面端 26.721 / gpt-5.4 direct 模式）。
    // 仅截短了超长 description/text 文本，**所有字段名与结构一字未改**。
    // include_str! 保证内容不会被测试代码悄悄改写。
    pub(super) const REAL_CODEX_CAPTURE: &str = include_str!("../testdata/codex_real_direct_request.json");

    pub(super) fn real_codex_request() -> Value {
        serde_json::from_str(REAL_CODEX_CAPTURE).expect("真实抓包应为合法 JSON")
    }

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

    use super::testfix::*;

    /// 🔴 `proxy.rs` 那两处 `expect("…必然已收集")` 成立的**唯一依据**，钉住它的来源。
    ///
    /// 审查时确认过：它们**当前不会 panic** —— `tool_sets` 是
    /// `sse_dir.map(|_| …)` 出来的，`Option::map` 的语义已经保证「`sse_dir` 是 Some ⟹
    /// `tool_sets` 是 Some」；`resp_tool_sets` 同理由 `(downstream != key.protocol).then(…)`
    /// 派生，与使用它的那个分支条件是同一个判断。Rust 只是不知道两个 `Option` 的关联。
    ///
    /// 也就是说风险不在今天，而在**日后有人把生成条件改成别的** —— 那时 `expect` 会
    /// panic 在转发热路径上（那一个请求整个挂掉）。
    ///
    /// 用类型把两者绑成一个 `Option<(dir, sets)>` 能让编译器保证，但那是重构、
    /// 而 `proxy.rs` 棘轮余量为 0，且不该紧挨着一次未做的真机验证做（同 docs/15 那四项
    /// 「刻意未做」的理由）。这条判据用零成本换到同样的保护：改了派生方式就变红。
    #[test]
    fn the_two_sse_invariants_must_stay_derived_not_assumed() {
        let src = std::fs::read_to_string("src/proxy.rs").unwrap();
        let prod = crate::proxy::custom_headers::production_code_only(&src);
        assert!(
            prod.contains("let tool_sets = sse_dir.map("),
            "tool_sets 必须由 sse_dir.map 派生 —— 那是 expect(\"sse_dir 为 Some 时…\") \
             成立的唯一依据。改成别的条件就得同时把那个 expect 换成真正的错误处理"
        );
        assert!(
            prod.contains("let resp_tool_sets = (downstream != key.protocol).then("),
            "resp_tool_sets 必须由「跨协议」这同一个条件派生 —— 那是它那处 expect 的唯一依据"
        );
    }

    /// 🔴 上游给的 `tool_calls[].index` 曾经直接决定我们分配多少内存
    /// （`while self.tool_calls.len() <= idx { push(...) }`）。
    /// 一个 `{"index": 4294967295}` 就是 40 亿个三元组 → 进程被 OOM 杀掉。
    /// 用户接的是第三方中转站，那正是这条链路上不可信的一方。
    #[test]
    fn a_huge_tool_index_from_upstream_is_refused_not_allocated() {
        use serde_json::json;
        let slot = super::tool_slot;
        assert_eq!(slot(Some(&json!(0))), Some(0));
        assert_eq!(slot(Some(&json!(255))), Some(255), "现实用量远低于上限");
        assert_eq!(slot(Some(&json!(256))), None, "到上限就拒，不钳制");
        assert_eq!(slot(Some(&json!(u32::MAX))), None);
        assert_eq!(slot(Some(&json!(u64::MAX))), None);
        assert_eq!(slot(None), Some(0), "缺字段按 0，与原行为一致");
        assert_eq!(slot(Some(&json!("3"))), Some(0), "非数字按 0，同上");
        assert_eq!(slot(Some(&json!(-1))), Some(0), "负数 as_u64 取不到 → 0");
    }

    /// 🔴 接线判据：`sse.rs` 的两个 `Vec` 扩容点必须真的经过 `tool_slot`。
    ///
    /// 上面那条只测函数本身 —— 把 sse.rs 改回
    /// `tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize` 它照样绿，
    /// 而那正是那条内存耗尽路径本身。
    #[test]
    fn the_streaming_accumulators_must_go_through_tool_slot() {
        let src = std::fs::read_to_string("src/upstream/sse.rs").unwrap();
        let prod = crate::proxy::custom_headers::production_code_only(&src);
        assert_eq!(
            prod.matches("super::tool_slot(").count(),
            2,
            "Chat 增量累积有两处，都必须走上限判据"
        );
        assert!(
            !prod.contains("as_u64().unwrap_or(0) as usize"),
            "不许绕过 tool_slot 直接把上游整数当下标用"
        );
    }

    use crate::model::Protocol;
    

    /// P2-2：`Protocol` 的能力方法必须对**每个变体**都有明确取值，不许靠 `_ =>` 兜底。
    ///
    /// 这条是「加第 4 种协议时的安全网」：遍历全变体逐项断言，若将来有人在能力方法里加了
    /// `_ =>` 兜底臂（那会让新协议被静默按某一族处理，向上游发错误的头 → 401 或
    /// `client_restricted` 403，排查方向被误导到「Key 配错了」），这条测试不会直接报错，
    /// 但配合下面「每个变体的取值都被显式列出」的断言，至少能保证现有三个变体的取值不被
    /// 无意改动。真正的编译期保障来自能力方法里的穷举 match 本身。
    #[test]
    fn sse_stream_errored_detects_in_stream_errors() {
        // Anthropic：200 后流内发 error 事件（过载常见形态）
        let anthropic_err = "\
event: message_start\n\
data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":10}}}\n\
\n\
event: error\n\
data: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n\n";
        assert!(super::sse_stream_errored(anthropic_err), "Anthropic 流内 error 事件必须识别");

        // OpenAI 兼容：末尾 chunk 塞 error 对象
        let openai_err = "data: {\"choices\":[{\"delta\":{\"content\":\"a\"}}]}\n\ndata: {\"error\":{\"message\":\"upstream boom\"}}\n\n";
        assert!(super::sse_stream_errored(openai_err), "OpenAI 流内 error 对象必须识别");

        // 正常流：无 error → false（不能把成功流误判成失败去熔断好 Key）
        let ok = "\
event: message_start\n\
data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":10}}}\n\
\n\
event: message_delta\n\
data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":5}}\n\
\n\
data: [DONE]\n";
        assert!(!super::sse_stream_errored(ok), "正常流不得被判为错误");
        // error 为 null（部分上游总带该键但成功时为 null）→ 不算错
        assert!(!super::sse_stream_errored("data: {\"choices\":[],\"error\":null}\n\n"));
    }

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
















    // ---- 响应解析：兼容普通 JSON 与 SSE 流 ----







    // ---- Responses ↔ Chat 转换 ----
























    // ---- 流式 SSE 翻译（Task #16）----




















































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



    // ---- namespace 全名往返（unsupported call 根因）----














}
