//! 流式工具调用累积的体积上限。
//!
//! `#[path]` 挂在 [`super::sse`] 下（同 `sse_error` 的理由）：`sse.rs` 棘轮冻结在 1247、
//! 余量为 0，而这一族是自洽的 —— 只回答「一条流最多能让我们攒多少工具参数」。
//!
//! # 🔴 它修的是一条上游可触发的内存耗尽
//!
//! [`super::tool_slot`] 已经挡住了「上游用一个巨大 `index` 让我们 push 40 亿个槽位」，
//! 但**每个槽位里的 `arguments` 是无上限 `push_str`** —— 上游只要持续发增量，
//! 我们就一直攒。用户接的是第三方中转站，那正是这条链路上不可信的一方。
//!
//! 同族的第二条：Anthropic 上游侧 `content_block_start` 每来一个 `tool_use` 就 push 一个
//! 槽位，那个循环此前也没有上限（`tool_slot` 只管 Chat 那侧的 `index`）。
//!
//! # 🔴 为什么越界要拒绝并终止，而不是截断
//!
//! 截断出来的是一段**非法 JSON**。下游客户端会把它 `JSON.parse` 出错，或更糟 ——
//! 恰好解析成一个参数不全的调用然后**真的拿去执行**。同 [`super::tool_slot`] 那条
//! 「钳制会拼出参数错乱的工具调用，比丢掉一条增量糟得多」。
//!
//! 故命中即：置位 → 给下游发一条**它自己协议里的**错误事件 → 此后这条流一个字节都不再翻译
//! （含 `finish()` 的收尾事件 —— 否则下游先收到失败再收到 `response.completed`，
//! 两条自相矛盾而客户端多半以后者为准，又变回「成功的空回答」，同 `sse_error` 那条闩）。

use super::{sse, sse_data, SseDirection, SseTranslator};
use serde_json::json;
use sha2::{Digest, Sha256};

/// 单个工具调用的 `arguments` 累计上限。
///
/// 1 MiB 远宽于现实（实测最大的 MCP 工具入参在几十 KB 量级），只对**真正异常**的上游生效。
pub(super) const MAX_TOOL_ARGS_BYTES: usize = 1024 * 1024;

/// 槽位数上限。与 [`super::super::tool_slot`] 的口径一致 —— 两侧都是「上游说要几个」。
pub(super) const MAX_TOOL_SLOTS: usize = 256;

/// 行缓冲上限。
///
/// 🔴 **它补的是「上限一条都执行不到」那条路**：`push` 靠找 `\n` 切行，行内才轮到参数/
/// 槽位/正文那些上限。上游开流后**一个换行都不发**（畸形网关、把二进制体伪装成 SSE）时
/// 那个循环永不进入，而 `buf` 一直 `extend_from_slice`。
///
/// 8 MiB 远宽于任何真实 SSE 行。**判据只看「还没等到 `\n` 的那一段」**，不看整个 buf ——
/// 一个 chunk 里塞几十条完整行是正常的，按 buf 总长判会把它误杀。
pub(super) const MAX_LINE_BYTES: usize = 8 * 1024 * 1024;

/// 去重集合的条目上限。
///
/// 它与 `tool_calls` 是**两份独立累积**：上游每条 item 换一个 `call_id` 就绕过去重，
/// 于是集合无上限增长，而 [`MAX_TOOL_ARGS_BYTES`] 与 [`MAX_TOOL_SLOTS`] 都管不到它。
pub(super) const MAX_TOOL_SEEN: usize = 2 * MAX_TOOL_SLOTS;

/// 累积正文（assistant 全文 + thinking 全文）的上限。
///
/// 累积本身是刻意的（Codex 靠收尾那条 `output_item.done` 落盘，只发增量的话重开会话就丢），
/// 但此前没有任何上限。32 MiB ≈ 最极端合法回答的 8 倍（1M token × 4 字节/token ≈ 4 MiB），
/// 故只对真正异常的上游生效。
pub(super) const MAX_ACCUM_BYTES: usize = 32 * 1024 * 1024;

/// 去重键只保留 call_id，缺失时用固定长度摘要，不能复制整段不可信参数。
pub(super) fn tool_dedup_key(call_id: &str, name: &str, arguments: &str) -> String {
    if !call_id.is_empty() {
        return call_id.to_string();
    }
    let mut digest = Sha256::new();
    digest.update(name.as_bytes());
    digest.update([0]);
    digest.update(arguments.as_bytes());
    format!("{name}\u{0}#{:x}", digest.finalize())
}

///
/// 🔴 **必须分项，不能退回一个布尔**：行缓冲越界却报「工具参数超限」，会把排障的人送去查
/// 工具定义，而真实成因是上游压根没在发合法 SSE。同本仓「指错方向的提示比没有提示更糟」。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum LimitKind {
    ToolArgs,
    ToolSlots,
    ToolSeen,
    LineBuffer,
    Accum,
}

impl LimitKind {
    fn describe(self) -> String {
        match self {
            Self::ToolArgs => format!("单个工具调用的参数超过 {MAX_TOOL_ARGS_BYTES} 字节"),
            Self::ToolSlots => format!("工具调用数超过 {MAX_TOOL_SLOTS}"),
            Self::ToolSeen => format!("工具调用去重记录超过 {MAX_TOOL_SEEN} 条"),
            Self::LineBuffer => format!(
                "单行数据超过 {MAX_LINE_BYTES} 字节（上游可能并未在发送 SSE，或发送了超大完整行）"
            ),
            Self::Accum => format!("累积正文超过 {MAX_ACCUM_BYTES} 字节"),
        }
    }
}

/// 追加一段参数增量；超过上限返回 `false`（**不追加**，调用方置位）。
pub(super) fn append_tool_args(dst: &mut String, chunk: &str) -> bool {
    if dst.len().saturating_add(chunk.len()) > MAX_TOOL_ARGS_BYTES {
        return false;
    }
    dst.push_str(chunk);
    true
}

impl SseTranslator {
    /// 完整 item 的参数也必须走同一条上限；失败时直接返回下游错误事件。
    pub(super) fn accept_complete_tool_args(&mut self, args: &str) -> Result<String, String> {
        let mut bounded = String::new();
        if append_tool_args(&mut bounded, args) {
            return Ok(bounded);
        }
        self.note_limit(LimitKind::ToolArgs);
        Err(self.take_tool_limit_error().unwrap_or_default())
    }

    /// 在把完整行复制到临时 Vec 之前检查它的真实字节数。
    pub(super) fn complete_line_too_long(&mut self, len: usize) -> bool {
        if len > MAX_LINE_BYTES {
            self.note_limit(LimitKind::LineBuffer);
            true
        } else {
            false
        }
    }

    /// 记下越界项。**先到的那一项赢** —— 它是根因，后面那些多半是它的连带。
    fn note_limit(&mut self, kind: LimitKind) {
        if self.limit_hit.is_none() {
            self.limit_hit = Some(kind);
        }
    }

    /// 追加一段工具参数增量；越界即置位（调用点因此是一行，不用各写一遍 if）。
    ///
    /// 子模块能直接读父模块的私有字段，故这里不需要访问器 —— 少一层就少一处要同步的东西。
    pub(super) fn push_tool_args(&mut self, slot: usize, chunk: &str) {
        if !append_tool_args(&mut self.tool_calls[slot].2, chunk) {
            self.note_limit(LimitKind::ToolArgs);
        }
    }

    /// 占一个新槽位；到顶返回 `None`（调用方跳过该 tool_use 块）。
    ///
    /// 🔴 **每个 `tool_calls.push` 都必须先过这里。** 有一处漏掉就等于整条上限不存在
    /// —— 那不是假设：`chat_tool_call_chunk_from_item`（Responses→Chat）当初就是裸 push，
    /// 而它**没有任何用例压到**，故上限那批注入全绿、缺口一直开着。
    pub(super) fn reserve_tool_slot(&mut self) -> Option<usize> {
        if self.tool_calls.len() >= MAX_TOOL_SLOTS {
            self.note_limit(LimitKind::ToolSlots);
            return None;
        }
        Some(self.tool_calls.len())
    }

    /// 登记去重键；`false` = 这个调用不该再翻（重复，**或**去重集合到顶）。
    ///
    /// 🔴 它与 `tool_calls` 是**两份独立累积**：上游每条 item 换一个 `call_id` 就绕过去重，
    /// 而 `call_id` 缺失时键是 `name + SHA-256(arguments)`，不会把整份参数复制进集合。
    pub(super) fn register_tool_seen(&mut self, key: String) -> bool {
        if self.anthropic_tool_seen.len() >= MAX_TOOL_SEEN && !self.anthropic_tool_seen.contains(&key)
        {
            self.note_limit(LimitKind::ToolSeen);
            return false;
        }
        self.anthropic_tool_seen.insert(key)
    }

    /// 追加 assistant 全文；越界即置位并**停止累积**（已转发给下游的增量不受影响）。
    pub(super) fn push_text_accum(&mut self, chunk: &str) {
        if self.text_accum.len().saturating_add(chunk.len()) > MAX_ACCUM_BYTES {
            self.note_limit(LimitKind::Accum);
            return;
        }
        self.text_accum.push_str(chunk);
    }

    /// 同上，thinking 全文那一份。
    pub(super) fn push_reasoning_accum(&mut self, chunk: &str) {
        if self.reasoning_accum.len().saturating_add(chunk.len()) > MAX_ACCUM_BYTES {
            self.note_limit(LimitKind::Accum);
            return;
        }
        self.reasoning_accum.push_str(chunk);
    }

    /// 行缓冲上限检查；越界返回那条错误事件。`push` 在行循环**之后**调一次。
    ///
    /// 判据只看**还没等到 `\n` 的那一段**（完整行已被行循环 drain 掉）——
    /// 按整个 buf 判会把「一个 chunk 里塞几十条完整行」这个正常形态误杀。
    /// 放在循环之后也是必须的：一个 `\n` 都没有时循环压根不进入，
    /// 那正是这条上限要挡的形态。
    pub(super) fn line_buffer_error(&mut self) -> Option<String> {
        if self.buf.len() > MAX_LINE_BYTES {
            self.note_limit(LimitKind::LineBuffer);
        }
        self.take_tool_limit_error()
    }

    /// 越界后要发的那条事件（没越界返回 `None`）。`push` 每处理完一行就问它一次。
    pub(super) fn take_tool_limit_error(&mut self) -> Option<String> {
        let kind = self.limit_hit?;
        if self.limit_hit_emitted {
            return None;
        }
        self.limit_hit_emitted = true;
        Some(self.tool_args_limit_event(kind))
    }

    /// 把累积的 Chat 风格 `tool_calls` 一次性翻成 Anthropic `tool_use` 块序列。
    ///
    /// 复用 `emit_anthropic_tool_block`（同一套去重/命名/兜底口径），故重复调用安全
    /// （第二次全部命中 `anthropic_tool_seen` 去重、返回空串）。
    ///
    /// 🔴 **不克隆整份 `tool_calls`**：`arguments` 单条可达 [`MAX_TOOL_ARGS_BYTES`]，
    /// 而这个函数在流末与**每个** `finish_reason` 都会被调 —— 原实现每次都深拷贝整份，
    /// 一条带大参数的多工具流会反复拷几 MB。只克隆小的 id/name，把大的 args
    /// **移走**（`mem::take`）：槽位与下标原样保留，`block_tool_slot` 的映射不受影响。
    pub(super) fn flush_anthropic_tool_calls(&mut self) -> String {
        let mut out = String::new();
        for i in 0..self.tool_calls.len() {
            if self.tool_calls[i].1.is_empty() {
                continue;
            }
            let (id, name) = (self.tool_calls[i].0.clone(), self.tool_calls[i].1.clone());
            let args = std::mem::take(&mut self.tool_calls[i].2);
            out.push_str(&self.emit_anthropic_tool_block(&json!({
                "type": "function_call", "call_id": id, "name": name, "arguments": args,
            })));
        }
        out
    }

    /// 越界时给下游的错误事件（按下游协议成形），并落下「此后不再翻译」的闩。
    ///
    /// 消息里**点名越界项**（`kind.describe()`），不给一句笼统的「超出安全上限」：
    /// 客户端拿到的这句话往往是用户唯一能看到的线索。
    fn tool_args_limit_event(&mut self, kind: LimitKind) -> String {
        let message =
            format!("上游响应超出代理的安全上限（{}），已终止本次流。", kind.describe());
        // 与 `sse_error` 共用同一个闩：`started` 为假时两个收尾函数都直接返回空。
        self.started = false;
        match self.dir {
            SseDirection::ChatToAnthropic | SseDirection::ResponsesToAnthropic => sse(
                "error",
                &json!({ "type": "error",
                    "error": { "type": "invalid_request_error", "message": message } }),
            ),
            SseDirection::ResponsesToChat | SseDirection::AnthropicToChat => {
                sse_data(&json!({ "error": { "type": "invalid_request_error", "message": message } }))
            }
            SseDirection::ChatToResponses | SseDirection::AnthropicToResponses => sse(
                "response.failed",
                &json!({
                    "type": "response.failed",
                    "response": { "id": self.resp_id, "object": "response", "status": "failed",
                        "model": self.model, "error": { "code": "invalid_request", "message": message } }
                }),
            ),
        }
    }
}
