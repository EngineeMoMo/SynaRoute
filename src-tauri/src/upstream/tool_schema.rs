//! 发往 OpenAI 系上游的工具 schema 边界校验。
//!
//! # 它修的洞（2026-09-15 用户实报）
//!
//! ```text
//! HTTP 400: Invalid schema for function 'mcp__codex_app__automation_update':
//! schema must have type 'object' and not have 'oneOf'/'anyOf'/'allOf'/'enum'/'const'/'not'
//! at the top level.
//! ```
//!
//! MCP 那侧可以**合法地**声明 union、标量、顶层 `enum`（MCP 的 schema 就是标准 JSON Schema），
//! 而 OpenAI 的工具校验器在请求边界拒绝这些形态。我们此前把 schema 原样透传 ——
//! 两条路径都会（跨协议由 `convert` 搬运 `parameters`，同协议直通压根不看），
//! 于是这个 400 由**上游**说出来，而排障的人手里只有一句英文和一个工具名。
//!
//! # 🔴 为什么是拒绝，而不是改写或删掉那个工具
//!
//! 两种「更聪明」的做法都更糟：
//!
//! - **改写 schema**（把 union 塞进 `properties`）= 猜模型该收什么参数。猜错的表现是
//!   工具能调、参数错位，而那比 400 难查得多。
//! - **静默删掉这个工具** = 历史里可能已经有它的 `tool_use`（Codex 多轮会把上一轮的调用
//!   带回来），删声明留历史换来的是**另一条** 400（`tool_use` 引用了未声明的工具），
//!   而那句错误不含原工具名 —— 同 `thinking_rectify` 记的「修一个 400 换来另一个 400」。
//!
//! 故这里只做一件事：**在本地就拒绝，并点名是哪个工具**。用户拿到的是可行动的中文说明，
//! 而不是一句需要他自己去翻 OpenAI 文档的英文。走 [`crate::error::AppError::Invalid`]
//! （本地配置错误）而非 `Upstream`：它永不自愈，不该熔断好 Key、也不该让客户端退避重试。
//!
//! # 边界：只看顶层
//!
//! 嵌套层里的 `anyOf` 是**合法的**（`properties.p.anyOf` 是标准写法，OpenAI 也接受）。
//! 判据只扫顶层，多扫一层就会把大量正常工具误拒 —— 而误拒的方向同样是「工具突然不可用」。

use serde_json::Value;

/// OpenAI 顶层不接受的组合关键字。
const FORBIDDEN_ROOT_KEYWORDS: [&str; 6] = ["oneOf", "anyOf", "allOf", "enum", "const", "not"];

/// 校验请求体里每个工具的 `parameters` 顶层是不是 OpenAI 能接受的 object schema。
///
/// 两种承载都覆盖：Chat 嵌套（`function.parameters`）与 Responses 扁平（顶层 `parameters`）。
/// 没有 `tools`、或某个工具没有 `parameters`（无参工具）时一律放行。
pub fn validate_openai_tool_schemas(body: &Value) -> Result<(), String> {
    let Some(tools) = body.get("tools").and_then(Value::as_array) else {
        return Ok(());
    };
    for tool in tools {
        // Chat 嵌套优先；没有 `function` 子对象时按 Responses 扁平形态读顶层。
        let holder = tool.get("function").unwrap_or(tool);
        let name = holder.get("name").and_then(Value::as_str).unwrap_or("<unknown>");
        let Some(schema) = holder.get("parameters") else { continue };
        let Some(object) = schema.as_object() else {
            return Err(reject(name, "顶层必须是一个 object schema"));
        };
        // 🔴 **组合关键字要排在 `type` 之前判。** 顶层 union 通常压根不带 `type`
        // （`{"anyOf":[…]}` 就是标准写法），先判 `type` 会给出「必须声明 type=object」——
        // 那句话虽然为真，却把用户引向「加个 type 就行」，而真正要做的是把 union 挪进
        // properties。写这条判据的用例当场抓住了这个顺序（同「指错方向的提示比没有提示更糟」）。
        if let Some(bad) = FORBIDDEN_ROOT_KEYWORDS.iter().find(|k| object.contains_key(**k)) {
            return Err(reject(name, &format!("顶层不能带 `{bad}`（把它挪进 properties 里）")));
        }
        if object.get("type").and_then(Value::as_str) != Some("object") {
            return Err(reject(name, "顶层必须声明 `type: \"object\"`"));
        }
    }
    Ok(())
}

/// 按上游协议决定要不要校验，命中即返回 [`crate::error::AppError::Invalid`]。
///
/// 转发路径统一走它（而不是各自写 `if is_openai() { … map_err … }`）：那三行在两条路径上
/// 各写一遍必然漂移，而漏掉一条的表现是「非流式被拦住、流式照旧发出去」——
/// 按客户端而异的分叉，本仓已为同一形态栽过多次。
pub fn validate_tools_for(upstream: crate::model::Protocol, body: &Value) -> crate::error::AppResult<()> {
    if !upstream.is_openai() {
        return Ok(());
    }
    validate_openai_tool_schemas(body).map_err(crate::error::AppError::Invalid)
}

/// 报错文案。**必须点名工具** —— 一个工具池里几十个工具，不点名等于没说。
fn reject(name: &str, why: &str) -> String {
    format!(
        "工具 `{name}` 的参数 schema 不被 OpenAI 系上游接受：{why}。\
         该请求已在本地拒绝，未发往上游。"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn chat_tool(name: &str, params: Value) -> Value {
        json!({ "tools": [ { "type": "function", "function": { "name": name, "parameters": params } } ] })
    }

    /// 🔴 用户实报的那个形态：顶层 union。
    #[test]
    fn a_union_root_is_refused_and_the_tool_is_named() {
        let body = chat_tool(
            "mcp__codex_app__automation_update",
            json!({ "oneOf": [{ "type": "object" }, { "type": "string" }] }),
        );
        let err = validate_openai_tool_schemas(&body).expect_err("必须被拒");
        assert!(err.contains("mcp__codex_app__automation_update"), "必须点名工具：{err}");
        assert!(err.contains("oneOf"), "必须说清是哪个关键字：{err}");
        assert!(err.contains("未发往上游"), "必须说明这是本地拒绝，否则用户会去查中转站");
    }

    /// 六个禁用关键字、非 object 的 type、以及压根不是对象的 schema 都要拒。
    #[test]
    fn every_forbidden_root_shape_is_refused() {
        for bad in [
            json!({ "anyOf": [{ "type": "object" }] }),
            json!({ "allOf": [{ "type": "object" }] }),
            json!({ "type": "object", "enum": ["a"] }),
            json!({ "type": "object", "const": 1 }),
            json!({ "type": "object", "not": { "type": "string" } }),
            json!({ "type": "string" }),
            json!({ "type": "array", "items": { "type": "string" } }),
            json!("not-even-an-object"),
            json!([1, 2]),
        ] {
            assert!(
                validate_openai_tool_schemas(&chat_tool("t", bad.clone())).is_err(),
                "这个 schema 必须被拒：{bad}"
            );
        }
    }

    /// 🔴 反面同样重要：误拒的表现是「工具突然全不可用」。
    ///
    /// 嵌套层的 `anyOf` 合法、Responses 扁平形态要认、无参工具与没有 tools 都放行。
    #[test]
    fn legitimate_schemas_are_never_refused() {
        let ok = json!({
            "tools": [
                { "type": "function", "function": { "name": "a",
                  "parameters": { "type": "object", "properties": { "p": { "type": "string" } } } } },
                // Responses 扁平形态（无 function 包一层）
                { "type": "function", "name": "flat", "parameters": { "type": "object", "properties": {} } },
                // 无参工具：没有 parameters 字段
                { "type": "function", "function": { "name": "no_params" } },
                // 嵌套层里的 anyOf 是标准写法，只看顶层
                { "type": "function", "function": { "name": "nested", "parameters": {
                    "type": "object",
                    "properties": { "p": { "anyOf": [{ "type": "string" }, { "type": "number" }] } },
                    "required": ["p"] } } }
            ]
        });
        assert!(validate_openai_tool_schemas(&ok).is_ok(), "合法 schema 被误拒");
        assert!(validate_openai_tool_schemas(&json!({ "model": "m" })).is_ok(), "没有 tools 不该报错");
        assert!(validate_openai_tool_schemas(&json!({ "tools": [] })).is_ok(), "空 tools 不该报错");
    }

    /// 🔴 **接线判据**：两条转发路径与聚合路径都必须真的调它。
    ///
    /// 上面的用例全都直调函数 —— 把 `proxy.rs` / `session.rs` 那几行摘掉它们照样全绿，
    /// 而那正是缺陷本体（schema 照旧原样发给上游）。本仓已为同一类盲区栽过 20 多次。
    #[test]
    fn the_send_paths_must_actually_call_this() {
        // 判据钉「校验被调用了」这个**性质**，两个入口任一都算 —— 钉某一个函数名会在
        // 有人合并/改名调用形态时制造假红（本仓为此付过一次代价）。
        let called = |src: &str| {
            let prod = crate::proxy::custom_headers::production_code_only(src);
            prod.matches("validate_tools_for(").count()
                + prod.matches("validate_openai_tool_schemas(").count()
        };
        assert!(
            called(include_str!("session.rs")) >= 1,
            "聚合的工具循环没有接上这道校验 —— schema 会原样发给上游"
        );
        // 非流式与流式两条转发路径**各自**都要有（漏一条的表现是「按客户端而异」）。
        assert_eq!(
            called(include_str!("../proxy.rs")),
            2,
            "proxy.rs 必须在流式与非流式两处都校验"
        );
    }
}
