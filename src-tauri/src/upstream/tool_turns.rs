//! Responses 历史 → Chat 消息时的**工具轮次分组**。
//!
//! `#[path]` 挂在 [`super::convert`] 下（同 `sse_bounds` 的理由）：`convert.rs` 棘轮冻结在
//! 1667、余量为 0，而这一族自洽 —— 只回答「攒着的工具调用与结果该按什么顺序落进 messages」。
//!
//! # 🔴 它修的洞（2026-09-15 用户实报）
//!
//! ```text
//! Anthropic Claude bad request: tool_use must be followed by a user tool_result turn
//! ```
//!
//! Codex 一轮可以发**多个** `function_call`（并行工具调用是常态，不是边缘）。此前每个 call
//! 各自 push 一条 assistant 消息，于是 `input` 里那串
//! `function_call, function_call, function_call_output, function_call_output`
//! 变成了 Chat 侧的：
//!
//! ```text
//! assistant(tool_calls:[c1]) → assistant(tool_calls:[c2]) → tool(c1) → tool(c2)
//! ```
//!
//! 而 Anthropic 要求 `tool_use` 之后**紧跟** `tool_result` 轮次。第一条 assistant 后面跟的
//! 是另一条 assistant → 400。
//!
//! 这个缺陷之所以难归因：请求看起来**完全正常** —— 工具声明、参数、`call_id` 全都在，
//! 数量也对，成因只在**消息分组**上，而日志里那一大段 JSON 不会让人一眼看出分组错了。
//! 而且它只在「一轮发多个工具」时触发，单工具会话永远碰不到。
//!
//! # 判据：合并同一轮，隔开不同轮
//!
//! 两个方向都要钉，缺一条都不对：
//!
//! - 同一轮的多个调用**必须合并**成一条 assistant（否则就是上面那个 400）；
//! - 连续两轮**必须各自成对**（把两轮也合并的话，第一轮的 `tool_result` 会排到第二轮的
//!   调用后面 —— 同一个 400 的镜像形态）。
//!
//! 故「遇到新的调用而手上已攒着结果」是轮次边界，见 `responses_to_chat` 里那几处
//! `if !pending_tool_results.is_empty()`。

use serde_json::{json, Value};

/// 遍历 `input[]` 时攒着的一轮工具调用与其结果。
#[derive(Default)]
pub(super) struct Grouper {
    calls: Vec<Value>,
    results: Vec<Value>,
}

impl Grouper {
    /// 记一个工具调用。**手上已攒着结果 = 上一轮结束**，先把上一轮落下去
    /// （否则两轮会被并成一条 assistant，第一轮的结果排到第二轮调用之后）。
    pub(super) fn call(&mut self, messages: &mut Vec<Value>, call: Value) {
        if !self.results.is_empty() {
            self.flush(messages);
        }
        self.calls.push(call);
    }

    /// 记一条工具结果。
    pub(super) fn result(&mut self, result: Value) {
        self.results.push(result);
    }

    /// 把攒着的这一轮按 Chat 要求的相邻顺序落进 `messages`（无待落项时什么都不做）。
    pub(super) fn flush(&mut self, messages: &mut Vec<Value>) {
        if !self.calls.is_empty() {
            messages.push(json!({
                "role": "assistant",
                // content 恒为 null：Responses 的 assistant 文本走独立的 message item，
                // 不与工具调用同项（真有文本时它自己会成为一条 assistant 消息）。
                "content": Value::Null,
                "tool_calls": std::mem::take(&mut self.calls),
            }));
        }
        messages.append(&mut self.results);
    }
}
