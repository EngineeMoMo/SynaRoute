//! token 用量的解析与采集。
//!
//! `TokenUsage` 的**定义**在 crate::model（它是领域观测量，不是上游协议细节），
//! 这里只放**解析与采集**——那些依赖各协议的字段形态，属于 upstream 的职责。
//!
//! USAGE_ACC / record_usage / with_usage 必须同文件：后两者直接引用 task_local! 宏
//! 生成的那个静态项。

use serde_json::Value;


/// `TokenUsage` 的**定义已移至 [`crate::model`]**（它是领域观测量，不是上游协议细节；
/// 留在这里会让 `model.rs` 反向依赖本模块，`model` 对 upstream 的依赖原本仅此一处）。
///
/// 此处 re-export 保持 `crate::upstream::TokenUsage` 路径可用，故全部既有调用点零改动。
/// **解析与采集逻辑仍留在本模块**（[`extract_usage`] / [`extract_usage_from_sse`] /
/// [`record_usage`]）——那些依赖协议字段形态，属于 upstream 的职责。
pub use crate::model::TokenUsage;

/// 解析一个 usage 对象。不同协议的 envelope 在调用方统一处理，这里只负责字段归一化。
fn parse_usage_object(u: &Value) -> Option<TokenUsage> {
    let num = |keys: &[&str]| -> u64 {
        keys.iter()
            .find_map(|k| u.get(*k).and_then(|v| v.as_u64()))
            .unwrap_or(0)
    };
    let anthropic_cache_read = u
        .get("cache_read_input_tokens")
        .and_then(|v| v.as_u64());
    let chat_cache_read = u
        .get("prompt_tokens_details")
        .and_then(|d| d.get("cached_tokens"))
        .and_then(|v| v.as_u64());
    let responses_cache_read = u
        .get("input_tokens_details")
        .and_then(|d| d.get("cached_tokens"))
        .and_then(|v| v.as_u64());
    let cache_read = anthropic_cache_read
        .or(chat_cache_read)
        .or(responses_cache_read)
        .unwrap_or(0);
    // Anthropic 的 input_tokens 不含缓存；OpenAI Chat/Responses 的输入总量包含缓存。
    let input = match u.get("input_tokens").and_then(|v| v.as_u64()) {
        Some(total)
            if anthropic_cache_read.is_none()
                && (chat_cache_read.is_some() || responses_cache_read.is_some()) =>
        {
            total.saturating_sub(cache_read)
        }
        Some(input) => input,
        None => u
            .get("prompt_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
            .saturating_sub(cache_read),
    };
    let usage = TokenUsage {
        input,
        output: num(&["output_tokens", "completion_tokens"]),
        cache_read,
        cache_creation: u
            .get("cache_creation_input_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
    };
    (!usage.is_empty()).then_some(usage)
}

/// 从上游响应体里提取 token 用量。
///
/// 支持顶层 `usage`、Anthropic 的 `message.usage`，以及 OpenAI Responses 的
/// `response.usage`。取不到或 usage 为空就返回 `None`，不伪造全零用量。
pub fn extract_usage(body: &Value) -> Option<TokenUsage> {
    [
        body.get("usage"),
        body.get("message").and_then(|m| m.get("usage")),
        body.get("response").and_then(|r| r.get("usage")),
    ]
    .into_iter()
    .flatten()
    .find_map(parse_usage_object)
}

/// 从**流式** SSE 全文里提取 token 用量。
///
/// 流式的 usage 不在单个 chunk 的固定位置：Anthropic 放在 `message_start`（input）与
/// `message_delta`（output）两处，OpenAI Chat 放在最后一个带 `usage` 的 chunk，Responses
/// 放在 `response.completed.response.usage`。故扫描所有 data 行、把见到的最大值取出来。
pub fn extract_usage_from_sse(sse: &str) -> Option<TokenUsage> {
    let mut acc = TokenUsage::default();
    for line in sse.lines() {
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(data) else {
            continue;
        };
        if let Some(u) = extract_usage(&v) {
            acc.input = acc.input.max(u.input);
            acc.output = acc.output.max(u.output);
            acc.cache_read = acc.cache_read.max(u.cache_read);
            acc.cache_creation = acc.cache_creation.max(u.cache_creation);
        }
    }
    (!acc.is_empty()).then_some(acc)
}


tokio::task_local! {
    /// 当前 async 任务的 token 用量累加器。
    ///
    /// 为什么用 task_local 而不是改 `text_completion` 的返回类型：它有三个调用点
    /// （成员 / 汇总者 / 决策者）、又被 `ToolSession` 的多轮循环反复调用，把返回值从
    /// `String` 改成 `(String, Usage)` 会波及每一处解构与错误分支，而这些路径刚在
    /// 上一轮做过故障注入验证 —— 为了加一个观测字段去动它们，风险不划算。
    ///
    /// task_local 天然按 async 任务隔离：聚合的每个成员各跑在自己的 spawn 里，
    /// 用量不会互相串台；没有 scope 包裹时（如普通代理转发）写入直接是 no-op。
    pub static USAGE_ACC: std::cell::Cell<TokenUsage>;
}

/// 从原始响应体（普通 JSON 或 SSE 全文）提取用量并记进累加器。
///
/// 聚合走的是**非流式**调用，但部分中转商仍会以 SSE 形态返回，故两种形态都试。
pub(super) fn record_usage_from_raw(raw: &str) {
    let u = serde_json::from_str::<Value>(raw)
        .ok()
        .and_then(|v| extract_usage(&v))
        .or_else(|| extract_usage_from_sse(raw));
    if let Some(u) = u {
        record_usage(u);
    }
}

/// 把本次上游调用的用量记进当前任务的累加器（无 scope 时静默忽略）。
pub fn record_usage(u: TokenUsage) {
    let _ = USAGE_ACC.try_with(|acc| {
        let mut cur = acc.get();
        cur.add(&u);
        acc.set(cur);
    });
}

/// 在一个带用量累加器的 scope 里跑 `fut`，返回 `(结果, 累计用量)`。
pub async fn with_usage<T>(fut: impl std::future::Future<Output = T>) -> (T, TokenUsage) {
    let cell = std::cell::Cell::new(TokenUsage::default());
    USAGE_ACC
        .scope(cell, async move {
            let out = fut.await;
            let used = USAGE_ACC.with(|c| c.get());
            (out, used)
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 两家协议的 usage 字段名不同，必须都能取到 —— 取不到就等于用户看不到额度消耗。
    #[test]
    fn extract_usage_handles_both_protocol_field_names() {
        // Anthropic（含缓存创建字段）
        let a = serde_json::json!({
            "usage": {
                "input_tokens": 1200,
                "output_tokens": 340,
                "cache_read_input_tokens": 900,
                "cache_creation_input_tokens": 150
            }
        });
        let u = extract_usage(&a).expect("Anthropic usage 应能取到");
        assert_eq!((u.input, u.output, u.cache_read, u.cache_creation), (1200, 340, 900, 150));
        assert_eq!(u.total(), 1540);

        // OpenAI（缓存在 prompt_tokens_details.cached_tokens 里，无 cache_creation 等价字段）
        //
        // ⚠️ 这里的期望值在本轮审计后**改过**：OpenAI 的 `prompt_tokens` **已包含** cached_tokens，
        // 而 TokenUsage 按 Anthropic 语义定义（input 不含缓存）。旧断言写的是 input=800、
        // cache_read=512，等于把 512 个缓存 token 计了两遍 —— 用量页「总计」多算 512，
        // 且 pricing 对 input 与 cache_read 各乘一次单价（缓存价约为满价 1/10），金额虚高。
        // 归一后 input = 800 - 512 = 288，满足下面的不变式。
        let o = serde_json::json!({
            "usage": {
                "prompt_tokens": 800,
                "completion_tokens": 120,
                "total_tokens": 920,
                "prompt_tokens_details": { "cached_tokens": 512 }
            }
        });
        let u = extract_usage(&o).expect("OpenAI usage 应能取到");
        assert_eq!((u.input, u.output, u.cache_read, u.cache_creation), (288, 120, 512, 0));
        // **不变式**：OpenAI 形态下 input + cache_read 必须等于上游给的 prompt_tokens。
        // 这条比具体数字更能钉住语义 —— 谁把归一去掉，它立刻变红。
        assert_eq!(u.input + u.cache_read, 800, "input+cache_read 必须还原成 prompt_tokens");
        // Anthropic 形态**不做**减法（input_tokens 本就不含缓存），上面已断言 1200 原样保留。

        // 脏数据兜底：个别中转商给出 cached > prompt，减法不得下溢 panic。
        let dirty = serde_json::json!({
            "usage": { "prompt_tokens": 100, "completion_tokens": 5,
                       "prompt_tokens_details": { "cached_tokens": 999 } }
        });
        let u = extract_usage(&dirty).expect("脏数据也应能解析");
        assert_eq!(u.input, 0, "saturating_sub：不得下溢");

        // 上游没给 usage → None（不是 0）。写 0 会让日志显示「本次 0 token」，看着像 bug。
        assert!(extract_usage(&serde_json::json!({ "content": [] })).is_none());
        assert!(extract_usage(&serde_json::json!({ "usage": {} })).is_none());
    }

    /// 流式 SSE：Anthropic 把 input 放 message_start、output 放 message_delta，
    /// 分散在不同 chunk 里，只看某一条会漏。
    #[test]
    fn extract_usage_from_sse_merges_across_chunks() {
        let sse = "\
event: message_start\n\
data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":1500,\"output_tokens\":1}}}\n\
\n\
event: content_block_delta\n\
data: {\"type\":\"content_block_delta\",\"delta\":{\"text\":\"hi\"}}\n\
\n\
event: message_delta\n\
data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":420}}\n\
\n\
data: [DONE]\n";
        let u = extract_usage_from_sse(sse).expect("SSE 应能取到用量");
        assert_eq!(u.input, 1500, "input 在 message_start 里");
        assert_eq!(u.output, 420, "output 取累计后的最终值，不是首个 chunk 的 1");

        // OpenAI 形态：usage 在最后一个 chunk
        let sse2 = "\
data: {\"choices\":[{\"delta\":{\"content\":\"a\"}}]}\n\
\n\
data: {\"choices\":[],\"usage\":{\"prompt_tokens\":77,\"completion_tokens\":9}}\n\
\n\
data: [DONE]\n";
        let u = extract_usage_from_sse(sse2).expect("OpenAI SSE 应能取到");
        assert_eq!((u.input, u.output), (77, 9));

        // 无 usage 的流 → None
        assert!(extract_usage_from_sse("data: {\"choices\":[]}\n\ndata: [DONE]\n").is_none());
    }

    /// 长流场景：proxy 只留头窗（message_start）+ 尾窗（message_delta），中间正文被丢弃。
    /// 头尾拼接后 extract_usage_from_sse 必须仍能取到完整 input+output+cache（按字段取 max）。
    /// 回归：此前只留尾窗 8KB，>8KB 回答的 message_start 被挤掉 → input/cache 记成 0。
    #[test]
    fn extract_usage_from_head_plus_tail_recovers_input_and_cache() {
        // 头窗：message_start（input/cache 所在）
        let head = "\
event: message_start\n\
data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":5000,\"cache_read_input_tokens\":42000,\"output_tokens\":1}}}\n\n";
        // 尾窗：message_delta（output），中间几十万字节正文已被丢弃
        let tail = "\
event: message_delta\n\
data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":880}}\n\
\n\
data: [DONE]\n";
        let merged = format!("{head}\n{tail}");
        let u = extract_usage_from_sse(&merged).expect("头尾合并后应能取到用量");
        assert_eq!(u.input, 5000, "input 来自头窗 message_start");
        assert_eq!(u.cache_read, 42000, "缓存读取来自头窗，不能记成 0");
        assert_eq!(u.output, 880, "output 来自尾窗 message_delta");
        // 只有尾窗（模拟未修复前）：input/cache 丢失，坐实缺陷方向。
        let tail_only = extract_usage_from_sse(tail).expect("尾窗有 output");
        assert_eq!(tail_only.input, 0, "仅尾窗时 input 必为 0（这正是被修的缺陷）");
        assert_eq!(tail_only.cache_read, 0);
    }

    #[test]
    fn extract_usage_handles_responses_envelope_and_cache() {
        let body = serde_json::json!({
            "type": "response.completed",
            "response": {
                "usage": {
                    "input_tokens": 800,
                    "output_tokens": 120,
                    "input_tokens_details": { "cached_tokens": 512 }
                }
            }
        });
        let u = extract_usage(&body).expect("Responses response.usage 应能取到");
        assert_eq!((u.input, u.output, u.cache_read, u.cache_creation), (288, 120, 512, 0));
        assert_eq!(u.input + u.cache_read, 800, "Responses input+cache_read 必须还原原始 input_tokens");

        // 空的顶层候选不能遮住后面的有效 Responses envelope。
        let wrapped = serde_json::json!({
            "usage": {},
            "response": { "usage": { "input_tokens": 12, "output_tokens": 3 } }
        });
        let u = extract_usage(&wrapped).expect("空候选后仍应继续尝试 response.usage");
        assert_eq!((u.input, u.output), (12, 3));
    }

    #[test]
    fn extract_usage_from_sse_handles_responses_completed_usage() {
        let sse = "event: response.created\ndata: {\"type\":\"response.created\"}\n\n\
            event: response.completed\n\
            data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":1234,\"output_tokens\":56}}}\n\n\
            data: [DONE]\n";
        let u = extract_usage_from_sse(sse).expect("Responses completed 事件应能取到嵌套 usage");
        assert_eq!((u.input, u.output, u.cache_read), (1234, 56, 0));
    }

    #[test]
    fn extract_usage_from_head_plus_tail_handles_responses_completed() {
        let head = "event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"r1\"}}\n\n";
        let tail = "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":900,\"output_tokens\":80}}}\n\ndata: [DONE]\n";
        let u = extract_usage_from_sse(&format!("{head}{tail}")).expect("尾窗中的 Responses usage 应能恢复");
        assert_eq!((u.input, u.output), (900, 80));
    }

    #[test]
    fn token_usage_add_and_format() {
        let mut a = TokenUsage { input: 1200, output: 340, cache_read: 0, cache_creation: 0 };
        a.add(&TokenUsage { input: 800, output: 60, cache_read: 500, cache_creation: 150 });
        assert_eq!((a.input, a.output, a.cache_read, a.cache_creation), (2000, 400, 500, 150));
        // 展示：≥10k 用 k 缩写，缓存不为 0 才附加
        assert_eq!(
            TokenUsage { input: 12_345, output: 400, cache_read: 0, cache_creation: 0 }.fmt_compact(),
            "↑12.3k ↓400"
        );
        assert!(TokenUsage { input: 10, output: 2, cache_read: 900, cache_creation: 0 }
            .fmt_compact()
            .contains("缓存900"));
        assert!(TokenUsage { input: 10, output: 2, cache_read: 0, cache_creation: 300 }
            .fmt_compact()
            .contains("写缓存300"));
        assert!(TokenUsage::default().is_empty());
    }
}
