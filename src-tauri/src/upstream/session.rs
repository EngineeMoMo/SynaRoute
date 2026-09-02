//! 多模态 prompt + 工具调用的 agent 循环（大脑聚合成员用）。
//!
//! 与 completion 的 text_completion **并列**而不是给它加参数：那个函数有三个调用点，
//! 其中决策者与汇总者的职责是「综合已有分析」，给它们工具会让整轮耗时不可控；
//! 硬塞参数则让那两条不需要工具的路径也背上多轮循环的复杂度。
//!
//! 测试与 ToolSession 同住是**必须的**：helper 直接写 `s.messages`（私有字段），
//! 而 Rust 的私有项只对定义模块及其后代可见 —— 放到兄弟模块立刻 E0616。

use crate::error::{AppError, AppResult};
use crate::model::{Protocol, ProviderKey};
use serde_json::{json, Value};
use std::time::Duration;

use super::cache::{
    cache_known_unsupported, inject_anthropic_cache, looks_like_cache_rejection,
    mark_cache_unsupported,
};
use super::client::{
    apply_auth, apply_client_identity, build_client, is_retriable_upstream_error, truncate_body,
    RETRY_BASE_BACKOFF_MS, RETRY_MAX_ATTEMPTS,
};
use super::completion::{parse_anthropic_text, parse_openai_text};
use super::endpoint::join_endpoint;
use super::usage::record_usage_from_raw;





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
    /// 输出上限；`None` = 请求体**不带**该字段（OpenAI 侧才可能为 `None`）。
    ///
    /// **不要在这里塞一个默认值**。工具循环每轮都重发整份历史，历史越长可用输出空间越小，
    /// 故 Anthropic 的值由调用方**每轮重算**（见 `aggregate::run_member_turns`）；
    /// 而 OpenAI 侧恒为 `None`，让上游自己决定长度。
    pub max_tokens: Option<u32>,
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

    /// 本轮**将要发出去**的输入内容的 token 估计（消息历史 + 工具声明）。
    ///
    /// 给 Anthropic 算输出预算用（`max_tokens ≤ 窗口 − 输入`）。必须每轮重新调用：
    /// 工具循环每轮把整份历史重发，第 5 轮的输入可能是第 1 轮的十几倍，
    /// 沿用首轮的估计会把输出预算算得过大 → 输入加输出超窗口 → 上游 400。
    ///
    /// 按结构遍历 JSON 估算，工具 schema 计入，但**图片 base64 正文不按文本计**：
    /// 那是传输编码，不是模型看到的 token。一张允许的 5MB 图片 base64 后约 670 万字符，
    /// 当文本算会被估成数百万 token，于是「窗口 − 输入」直接见底、明明有效的视觉请求
    /// 被本地判成没有输出空间。见 [`crate::upstream::estimate_json_tokens_without_image_transport`]。
    pub fn estimated_input_tokens(&self, tools: &[ToolDef]) -> u32 {
        let mut total = 0u32;
        for m in &self.messages {
            total = total.saturating_add(
                crate::upstream::estimate_json_tokens_without_image_transport(m),
            );
        }
        if !tools.is_empty() {
            // 按本协议实际会发出的形状算：两家的工具声明字段名/嵌套不同，字符数也不同。
            let decl = if self.protocol.is_openai() {
                openai_tools(tools)
            } else {
                anthropic_tools(tools)
            };
            total = total.saturating_add(
                crate::upstream::estimate_json_tokens_without_image_transport(&decl),
            );
        }
        total
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
            "messages": self.messages,
        });
        // 输出上限：OpenAI 侧为 None 时**不写这个字段**（可选项，省略即不由请求方限制）；
        // Anthropic 必填，故 None 时补协议下限 1 而不是省略（省略必然 400，理由同
        // completion.rs::anthropic_message）。正常路径上调用方总会给 Anthropic 一个 Some。
        match p.max_tokens {
            Some(n) => {
                payload["max_tokens"] = json!(n);
            }
            None if !openai => {
                payload["max_tokens"] = json!(1);
            }
            None => {}
        }
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
            // 这条重发路径只在 `want_cache` 为真时可达，而 `want_cache = !openai` ——
            // 即必然是 Anthropic，`max_tokens` 是必填字段，不能像 OpenAI 那样省略。
            let mut plain = json!({
                "model": model,
                "max_tokens": p.max_tokens.unwrap_or(1),
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
        // 🔴 状态码是 2xx 不等于成功：上游可能回 200 而正文是一个错误对象
        // （`{"error":{…}}` / `{"type":"error",…}`，中转站过载时的常见形态）。
        //
        // 不判它的话这条路**不会**静默成功，但会报成「响应无法解析」——
        // `parse_*_turn` 找不到 content/choices，兜底文本提取也返回 None。
        // 那句话把排障的人指向我们的解析器，而真相是上游返了个错误
        // （上游原文虽然带在后面，第一眼的方向已经错了）。
        //
        // 判据与转发路径**共用** `body_is_upstream_error`：只堵一条链路就是同一个洞
        // 只堵一半。`upstream_http` 如实带上游给的 200 —— 那是事实，
        // 而消息里写明「HTTP 200 但正文是错误」才是可行动的信息。
        if let Some(msg) = super::body_is_upstream_error(raw.as_bytes()) {
            return Err(AppError::upstream_http(
                status.as_u16(),
                format!("{label} 上游回了 HTTP {status} 但正文是一个错误：{msg}"),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 🔴 **接线判据**：软错误那道门必须在**这条链路上**、且排在解析之前。
    ///
    /// 行为用例在 `aggregate.rs`（那里有 HTTP mock），但它证明不了「门排在哪」——
    /// 把门挪到 `parse_*_turn` 之后，解析会先失败并 return，门永远走不到，
    /// 而那正是修之前的行为（报「响应无法解析」）。本仓在这类
    /// 「组件覆盖了、接线没覆盖」的盲区上已栽过十余次。
    #[test]
    fn the_soft_error_gate_must_precede_parsing() {
        let src = include_str!("session.rs");
        let prod = crate::proxy::custom_headers::production_code_only(src);
        let n = prod.matches("body_is_upstream_error(").count();
        assert_eq!(n, 1, "这条链路上必须恰好有一处软错误门，实际 {n} 处");
        let gate = prod.find("body_is_upstream_error(").expect("上面刚断言过存在");
        // 找**调用**形态而不是函数定义（定义排在文件前部，会让顺序断言恒真）。
        let parse = prod
            .find("parse_anthropic_turn(&raw)")
            .expect("解析调用点的形态变了，本判据要跟着改");
        assert!(
            gate < parse,
            "软错误门必须排在解析之前（gate@{gate} vs parse@{parse}）——\
             排在后面时解析已经先失败并报出「响应无法解析」了"
        );
    }

    // ---- 多模态 + 工具调用（聚合成员 agent 循环的协议层）----

    fn img(mt: &str) -> ImagePart {
        ImagePart {
            media_type: mt.to_string(),
            base64: "AAAA".to_string(),
        }
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
