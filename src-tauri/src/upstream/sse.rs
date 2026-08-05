//! 流式 SSE 的跨协议翻译。
//!
//! 这是与非流式转换**并行的第二套矩阵**：非流式走 hub-and-spoke（3 协议只需 6 个单边
//! 函数），而这里是 6 个手写的有向方法。两套矩阵已经出现过能力漂移，
//! 回归安全网见 sse_golden.rs（六方向黄金样本 + 分块不变性 + 能力矩阵）。
//!
//! **整个 SseTranslator 留在同一个文件里**，不再按方向细分：那 20 多个私有字段被
//! 6 个方向方法共享，拆成兄弟模块的话每个字段都得加 pub(super)，
//! 等于把内部状态变成半公开的 API 面 —— 得不偿失。

use crate::model::Protocol;
use serde_json::{json, Value};

use super::util::uuid_like;
use super::{
    join_namespaced_tool_name, rewrite_to_tool_search_call, split_namespaced_tool_name,
    unpack_custom_tool_input,
};

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
mod tests {
    use super::*;
    use super::super::testfix::{anthropic_tool_blocks, sse_events};

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
}
