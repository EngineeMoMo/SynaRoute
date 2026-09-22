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

/// Anthropic 顶层不接受的组合关键字 —— **刻意只有三个，不是照搬上面那六个**。
///
/// 2026-09-20 用户实报的上游原话只点了这三个：
///
/// ```text
/// ***.***.custom.input_schema: input_schema does not support oneOf, allOf,
/// or anyOf at the top level  （reason: TOOL_SCHEMA_INVALID）
/// ```
///
/// `enum`/`const`/`not` 在 Anthropic 顶层**是否也被拒，没有任何取证**。把它们一起列进来
/// 是「顺手对齐」，而代价是误拒：一个顶层带 `enum` 的工具会被我们在本地拦下，用户看到的是
/// 「工具突然不可用」—— 那比上游的 400 更难查，也正是本模块开头论证过不做的事。
/// 故此处只列证据支持的三个；将来若有新的上游原话点到别的关键字，**带着那句原话**再加。
const FORBIDDEN_ROOT_KEYWORDS_ANTHROPIC: [&str; 3] = ["oneOf", "anyOf", "allOf"];

/// 一个工具声明里承载入参 schema 的字段名，**按协议而异**。
///
/// OpenAI 系叫 `parameters`（Chat 嵌在 `function` 里、Responses 扁平在顶层），
/// Anthropic 叫 `input_schema`（平铺）。这不是「两个都查一下」的细节：查错字段的表现是
/// 校验**静默空转** —— 每个工具都走 `continue`、函数返回 `Ok`，与「压根没有校验」
/// 一模一样，而测试还是绿的。2026-09-20 那条 Anthropic 上游 400 的原话点的正是
/// `input_schema`，若照搬 `parameters` 去查，这个洞会原样留着。
const SCHEMA_FIELD_OPENAI: &str = "parameters";
const SCHEMA_FIELD_ANTHROPIC: &str = "input_schema";

/// 两家协议共用的顶层 schema 判据，差异全部由参数传入。
///
/// 抽成一个函数而不是各写一份：两份必然漂移，而漏掉一条的表现是「某个协议悄悄没在查」，
/// 正是本模块开头那个洞的形状。
fn validate_tool_schemas(
    body: &Value,
    forbidden: &[&str],
    schema_field: &str,
    strict_object_root: bool,
    upstream_label: &str,
) -> Result<(), String> {
    let Some(tools) = body.get("tools").and_then(Value::as_array) else {
        return Ok(());
    };
    for tool in tools {
        // Chat 嵌套优先；没有 `function` 子对象时按扁平形态（Responses / Anthropic）读顶层。
        let holder = tool.get("function").unwrap_or(tool);
        let name = holder
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("<unknown>");
        // 无参工具、以及 Anthropic 的服务端工具（`{type:"web_search_…", name}` 无 schema）
        // 都落在这里放行。
        let Some(schema) = holder.get(schema_field) else {
            continue;
        };
        let Some(object) = schema.as_object() else {
            // OpenAI 那侧上游原话明确点了 "schema must have type 'object'"，故拒。
            // Anthropic 侧没有这句取证，且非 object 压根不可能带那三个关键字 —— 放行，
            // 让上游去说（同本模块「只拦有证据的形态」）。
            if strict_object_root {
                return Err(reject(name, "顶层必须是一个 object schema", upstream_label));
            }
            continue;
        };
        // 🔴 **组合关键字要排在 `type` 之前判。** 顶层 union 通常压根不带 `type`
        // （`{"anyOf":[…]}` 就是标准写法），先判 `type` 会给出「必须声明 type=object」——
        // 那句话虽然为真，却把用户引向「加个 type 就行」，而真正要做的是把 union 挪进
        // properties。写这条判据的用例当场抓住了这个顺序（同「指错方向的提示比没有提示更糟」）。
        if let Some(bad) = forbidden.iter().find(|k| object.contains_key(**k)) {
            return Err(reject(
                name,
                &format!("顶层不能带 `{bad}`（把它挪进 properties 里）"),
                upstream_label,
            ));
        }
        // `type: "object"` 只对 OpenAI 强制（上游原话里有）。Anthropic 侧不查：没有取证，
        // 而误拒的方向是「工具突然不可用」，比上游那句 400 更难查。
        if strict_object_root && object.get("type").and_then(Value::as_str) != Some("object") {
            return Err(reject(
                name,
                "顶层必须声明 `type: \"object\"`",
                upstream_label,
            ));
        }
    }
    Ok(())
}

/// 校验请求体里每个工具的 `parameters` 顶层是不是 OpenAI 能接受的 object schema。
///
/// 两种承载都覆盖：Chat 嵌套（`function.parameters`）与 Responses 扁平（顶层 `parameters`）。
/// 没有 `tools`、或某个工具没有 `parameters`（无参工具）时一律放行。
pub fn validate_openai_tool_schemas(body: &Value) -> Result<(), String> {
    validate_tool_schemas(
        body,
        &FORBIDDEN_ROOT_KEYWORDS,
        SCHEMA_FIELD_OPENAI,
        true,
        "OpenAI 系",
    )
}

/// 校验发往 Anthropic 上游的工具 `input_schema` 顶层没有 union（2026-09-20 用户实报）。
///
/// 判据比 OpenAI 那套**窄**，理由见 [`FORBIDDEN_ROOT_KEYWORDS_ANTHROPIC`]。
pub fn validate_anthropic_tool_schemas(body: &Value) -> Result<(), String> {
    validate_tool_schemas(
        body,
        &FORBIDDEN_ROOT_KEYWORDS_ANTHROPIC,
        SCHEMA_FIELD_ANTHROPIC,
        false,
        "Anthropic",
    )
}

/// 按上游协议决定用哪套判据，命中即返回 [`crate::error::AppError::Invalid`]。
///
/// 转发路径统一走它（而不是各自写 `if is_openai() { … map_err … }`）：那三行在两条路径上
/// 各写一遍必然漂移，而漏掉一条的表现是「非流式被拦住、流式照旧发出去」——
/// 按客户端而异的分叉，本仓已为同一形态栽过多次。
///
/// 🔴 **Anthropic 分支是 2026-09-20 补的。** 此前这里是 `if !upstream.is_openai() { return Ok }`
/// —— 一句「只有 OpenAI 有这个约束」的假设，而用户那条 `TOOL_SCHEMA_INVALID` 证明 Anthropic
/// 同样拒顶层 union。两条转发路径的调用点本来就把协议传进来了，所以修这个洞**不需要动调用点**，
/// 只需要这里不再提前放行。
pub fn validate_tools_for(
    upstream: crate::model::Protocol,
    body: &Value,
) -> crate::error::AppResult<()> {
    let checked = if upstream.is_openai() {
        validate_openai_tool_schemas(body)
    } else if upstream == crate::model::Protocol::Anthropic {
        validate_anthropic_tool_schemas(body)
    } else {
        Ok(())
    };
    checked.map_err(crate::error::AppError::Invalid)
}

/// 报错文案。**必须点名工具** —— 一个工具池里几十个工具，不点名等于没说。
/// 也必须点名是哪一侧的上游：用户下一步要去改的东西不同。
fn reject(name: &str, why: &str, upstream_label: &str) -> String {
    format!(
        "工具 `{name}` 的参数 schema 不被 {upstream_label}上游接受：{why}。\
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
        assert!(
            err.contains("mcp__codex_app__automation_update"),
            "必须点名工具：{err}"
        );
        assert!(err.contains("oneOf"), "必须说清是哪个关键字：{err}");
        assert!(
            err.contains("未发往上游"),
            "必须说明这是本地拒绝，否则用户会去查中转站"
        );
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
        assert!(
            validate_openai_tool_schemas(&ok).is_ok(),
            "合法 schema 被误拒"
        );
        assert!(
            validate_openai_tool_schemas(&json!({ "model": "m" })).is_ok(),
            "没有 tools 不该报错"
        );
        assert!(
            validate_openai_tool_schemas(&json!({ "tools": [] })).is_ok(),
            "空 tools 不该报错"
        );
    }

    fn anthropic_tool(name: &str, schema: Value) -> Value {
        json!({ "tools": [ { "name": name, "input_schema": schema } ] })
    }

    /// 🔴 2026-09-20 用户实报的形态：Anthropic 上游拒 `input_schema` 顶层 union。
    ///
    /// 上游原话（中转站逐字转发）：
    /// `input_schema does not support oneOf, allOf, or anyOf at the top level`
    /// （`reason: TOOL_SCHEMA_INVALID`）。此前 `validate_tools_for` 对非 OpenAI 上游
    /// 一律提前 `return Ok`，这个 400 只能由上游说出来，且用户看到的是英文 + 掩码后的工具名。
    ///
    /// **这条用例同时是字段名的故障注入判据**：body 用的是 `input_schema`，
    /// 若把 `SCHEMA_FIELD_ANTHROPIC` 写成 `parameters`，校验会静默空转（每个工具
    /// `continue`、返回 `Ok`）—— 与压根没修一模一样，而只有这条会变红。
    #[test]
    fn anthropic_union_root_is_refused_and_names_the_tool() {
        for bad in ["oneOf", "allOf", "anyOf"] {
            let body = anthropic_tool(
                "mcp__codex_app__automation_update",
                json!({ bad: [{ "type": "object" }, { "type": "string" }] }),
            );
            let err =
                validate_anthropic_tool_schemas(&body).expect_err("Anthropic 顶层 union 必须被拒");
            assert!(
                err.contains("mcp__codex_app__automation_update"),
                "必须点名工具：{err}"
            );
            assert!(err.contains(bad), "必须说清是哪个关键字：{err}");
            assert!(err.contains("Anthropic"), "必须点名是哪一侧上游：{err}");
            assert!(
                err.contains("未发往上游"),
                "必须说明是本地拒绝，否则用户会去查中转站：{err}"
            );
        }
    }

    /// 🔴 **判据不许照搬 OpenAI 那六个。**
    ///
    /// `enum`/`const`/`not` 在 Anthropic 顶层是否被拒**没有取证**，拦下它们就是误拒，
    /// 而误拒的表现是「工具突然不可用」—— 比上游那句 400 更难查。非 object 的根同理放行
    /// （没有取证，且非 object 压根不可能带那三个关键字）。
    ///
    /// 有人「顺手对齐」把 `FORBIDDEN_ROOT_KEYWORDS_ANTHROPIC` 换成那六个时，这条立刻变红。
    #[test]
    fn anthropic_judgment_stays_narrower_than_openai() {
        for tolerated in [
            json!({ "type": "object", "enum": ["a"] }),
            json!({ "type": "object", "const": 1 }),
            json!({ "type": "object", "not": { "type": "string" } }),
            json!({ "type": "string" }),
            json!("not-even-an-object"),
        ] {
            assert!(
                validate_anthropic_tool_schemas(&anthropic_tool("t", tolerated.clone())).is_ok(),
                "没有取证的形态不该在本地拦下（误拒 = 工具突然不可用）：{tolerated}"
            );
        }
        // 对照：同样这些形态在 OpenAI 侧**有**上游原话支持，仍须拒 —— 证明放宽只发生在
        // Anthropic 一侧，没有把 OpenAI 那套判据一起弄松。
        for still_bad in [
            json!({ "type": "object", "enum": ["a"] }),
            json!({ "type": "string" }),
        ] {
            assert!(
                validate_openai_tool_schemas(&chat_tool("t", still_bad.clone())).is_err(),
                "OpenAI 侧判据不该被放宽：{still_bad}"
            );
        }
    }

    /// 🔴 反面：合法的 Anthropic 工具声明一个都不许被拦。
    #[test]
    fn legitimate_anthropic_schemas_are_never_refused() {
        let ok = json!({
            "tools": [
                { "name": "read_file",
                  "input_schema": { "type": "object", "properties": { "path": { "type": "string" } },
                                    "required": ["path"] } },
                // 嵌套层的 anyOf 是标准写法，只看顶层
                { "name": "nested",
                  "input_schema": { "type": "object",
                                    "properties": { "p": { "anyOf": [{ "type": "string" }, { "type": "number" }] } } } },
                // Anthropic 服务端工具：没有 input_schema
                { "type": "web_search_20250305", "name": "web_search" },
            ]
        });
        assert!(
            validate_anthropic_tool_schemas(&ok).is_ok(),
            "合法 Anthropic schema 被误拒"
        );
        assert!(validate_anthropic_tool_schemas(&json!({ "model": "m" })).is_ok());
        assert!(validate_anthropic_tool_schemas(&json!({ "tools": [] })).is_ok());
    }

    /// 🔴 **`validate_tools_for` 的分派判据 —— 这是缺陷本体所在。**
    ///
    /// 上面那些用例直调 `validate_anthropic_tool_schemas`，把 `validate_tools_for` 里的
    /// Anthropic 分支删掉（退回 `if !is_openai() { return Ok }`）它们**照样全绿**，
    /// 而那正是 2026-09-20 那个洞。本仓已为这类「函数写对了但没接上」的盲区栽过 20 多次。
    #[test]
    fn validate_tools_for_dispatches_anthropic_not_just_openai() {
        use crate::model::Protocol;
        let bad = anthropic_tool("t", json!({ "oneOf": [{ "type": "object" }] }));
        assert!(
            validate_tools_for(Protocol::Anthropic, &bad).is_err(),
            "Anthropic 上游必须经由 validate_tools_for 拦下顶层 union —— 此前这里直接 return Ok"
        );
        // OpenAI 系仍走它自己那套（更严）判据。
        let bad_openai = chat_tool("t", json!({ "type": "string" }));
        assert!(validate_tools_for(Protocol::OpenaiChat, &bad_openai).is_err());
        assert!(validate_tools_for(Protocol::OpenaiResponses, &bad_openai).is_err());
        // Anthropic 的字段名是 input_schema：同样的坏形态挂在 parameters 上不该由
        // Anthropic 判据拦（它压根不看那个字段），这钉住两套判据没有互相串味。
        let anthropic_wrong_field = json!({ "tools": [ { "name": "t",
            "parameters": { "oneOf": [{ "type": "object" }] } } ] });
        assert!(validate_tools_for(Protocol::Anthropic, &anthropic_wrong_field).is_ok());
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
