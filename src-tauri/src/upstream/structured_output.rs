//! 「我要 JSON」这件事在三个协议里的三种写法，以及它们之间的换算。
//!
//! `#[path]` 挂在 [`super::convert`] 下（同 `lan_guard`/`log_rotate`/`sse_error` 的理由）：
//! `convert.rs` 棘轮冻结在 1690，本轮补 Anthropic 方向后越界，而这四个函数是自洽的一族
//! —— 都只做「同一份 schema 换个位置」，不碰消息体、不碰工具、不碰用量。
//!
//! # 🔴 为什么这一族必须存在：丢掉它是**静默**的
//!
//! 三协议表达同一个约束的字段完全不同名、不同层：
//!
//! | 协议 | 字段位置 | json_schema 形态 |
//! |---|---|---|
//! | Chat Completions | 顶层 `response_format` | 带 `json_schema` 内层包裹 |
//! | Responses | `text.format` | **摊平**，`name`/`schema`/`strict` 直挂 |
//! | Anthropic Messages | `output_config.format` | 摊平，同 Responses |
//!
//! 任一方向漏掉，请求都会**200 成功**、模型回散文，客户端在 `JSON.parse` 上炸掉。
//! 用户看到的是「我的程序解析失败」，而根因在代理的协议转换里 —— 状态码、错误信息、
//! 日志里没有任何线索。本仓已为 Chat↔Responses 修过这条，Anthropic 方向是同一个坑的第二半
//! （故障转移落到一条 Anthropic Key 就复现，而用户完全不知道自己碰了什么）。
//!
//! # 🔴 Anthropic 侧的字段名是 `output_config.format`，不是 `output_format`
//!
//! 后者是**已废弃的 beta 期名字**。照它写的代价不是静默失效而是**响亮失败** ——
//! Anthropic 对未知顶层字段返 400，于是每个带结构化输出的请求当场报错。
//! 取的是官方 structured-outputs 文档的现行形态；改这个名字前请重新核一次官方页，
//! 别照任何二手转述（本轮就差点照一份二手转述写成 `output_format`）。
//!
//! # 边界：只搬形态，不校验 schema
//!
//! 三个函数都只做 `as_object()?` 这一道存在性检查，schema 本身原样传给上游 ——
//! 校验是上游的事，我们替它判会引入「我们判合法、上游判不合法」的第三种真相。

use serde_json::{json, Value};

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
pub(super) fn chat_response_format_to_responses_text(rf: &Value) -> Option<Value> {
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
pub(super) fn responses_text_to_chat_response_format(text: &Value) -> Option<Value> {
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

/// Chat 的 `response_format` → Anthropic 的 `output_config.format` 内层。
///
/// Anthropic 的形态与 Responses 的 `text.format` **同构**（都是摊平的 `{type, schema, …}`），
/// 故直接借道 [`chat_response_format_to_responses_text`] 取它的 `format`，不再写第二份解析
/// —— 两份必然漂移，而本仓为这类重复吃过账（保留字段清单那条）。
pub(super) fn chat_response_format_to_anthropic_format(rf: &Value) -> Option<Value> {
    let converted = chat_response_format_to_responses_text(rf)?;
    converted.get("format").cloned()
}

/// Responses 的 `text.format` → Anthropic 的 `output_config.format` 内层（同构，原样搬）。
///
/// 只接受对象形态：`text.format` 缺失或不是对象时返回 `None`，让上层的 `.or_else` 链落空 ——
/// 塞一个畸形值进去会让 Anthropic 400，而这条路径上「没配结构化输出」是绝大多数请求的常态。
pub(super) fn responses_text_format_to_anthropic_format(format: &Value) -> Option<Value> {
    format.as_object()?;
    Some(format.clone())
}
