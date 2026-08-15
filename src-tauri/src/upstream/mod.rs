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
mod tools_meta;
mod usage;
mod util;

/// 大脑聚合的输出预算（协议感知）。代理转发不用它——转发一律透明，见 proxy::apply_key_params。
pub use budget::{
    anthropic_required_max_tokens, estimate_json_tokens_without_image_transport, estimate_tokens,
    output_budget,
};
pub use client::shared_client;
/// 转发用的 per-Key 响应超时。流式探头阶段与非流式共用同一口径（见 proxy.rs 两处调用）。
pub use client::key_timeout;
/// 自建请求的客户端身份头（UA 等）。缺它会被部分中转渠道判 `detected: unknown` 而 403。
pub use client::apply_client_identity;
pub use completion::text_completion;
pub use discovery::fetch_models;
pub use probe::{health_probe, health_probe_real};
pub use sse::{sse_direction, SseTranslator};
pub use convert::{convert_request_owned, convert_response_ext};
pub use tools_meta::{collect_custom_tools, collect_search_tools, collect_tool_namespaces};
pub use session::{
    ImagePart, MultimodalPrompt, ToolDef, ToolInvocation, ToolResultMsg, ToolSession, TurnOutcome,
    TurnParams,
};
pub use endpoint::join_endpoint;
pub use usage::{extract_usage, extract_usage_from_sse, with_usage, TokenUsage};

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
    use crate::model::Protocol;
    

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
