//! 工具声明的识别与命名：Codex 特有的 additional_tools / tool_search 形态，
//! 以及 namespace 的拆分与拼接。
//!
//! 单独成模块是因为 convert 与 sse **两条路径都要用**：请求侧要认出声明了哪些工具、
//! 响应侧要把 namespace 拆回结构化字段。两边各写一份必然分叉，
//! 而分叉的后果是 Codex router 查表匹配不上、报 unsupported call。

use serde_json::Value;

/// Codex 桌面端把工具声明塞进 `input` 数组里的这种 item type，而**不用**顶层 `tools`。
pub(super) const ADDITIONAL_TOOLS_ITEM: &str = "additional_tools";

/// Codex 「延迟工具检索器」的 Responses 工具 type。该 type 的声明**没有 `name` 字段**，
/// 名字即 type 本身；`execution:"client"` 表示由 Codex 客户端本地用 BM25 执行、不经上游。
pub(super) const TOOL_SEARCH_TYPE: &str = "tool_search";

/// Codex 客户端执行完检索后回传的结果 item type：其 `tools[]` 才带回 MCP 工具的**真 schema**。
pub(super) const TOOL_SEARCH_OUTPUT_ITEM: &str = "tool_search_output";

/// 模型发起检索的 item type（对应上游模型对 `tool_search` 的一次工具调用）。
pub(super) const TOOL_SEARCH_CALL_ITEM: &str = "tool_search_call";

/// 收集本次 Responses 请求**声明**的全部工具（保持 Responses 原始形态，不做转换）。
///
/// 三处都要收：
/// 1. 顶层 `tools`；
/// 2. `input[]` 里 `type=="additional_tools"` 项的 `tools`（Codex 桌面端 exec 编排范式）；
/// 3. `input[]` 里 `type=="tool_search_output"` 项的 `tools`（**MCP 工具的唯一来源**）。
///
/// 为什么必须收第 2 处（2026-07-30 实测）：Codex 桌面端在 `tool_mode="code_mode_only"` 的模型
/// （gpt-5.6 系）下**顶层根本没有 `tools` 字段**，工具全在
/// `input[0] = {"type":"additional_tools","role":"developer","tools":[…]}` 里。
///
/// 为什么必须收第 3 处（2026-07-30 实测）：Codex 把 MCP 工具标 `defer_loading:true` **扣在客户端
/// 本地**，顶层 `tools` 里**永远不出现** `mcp__*` namespace（59 条含 `mcp__synaroute` 的抓包请求中，
/// 顶层命中数为 0）。模型必须先调 `tool_search`，Codex 本地检索后把真 schema 放进
/// `tool_search_output.tools[]` 回灌历史——该 item 是「下一次模型调用可用工具」的唯一载体。
/// 不收这一处，即使模型成功检索过，下一轮发往上游的请求里依旧没有 `synaroute_ai`，
/// 表现为「MCP 服务端握手正常、但模型坚称没有这个工具」。
///
/// 顶层在前，保证既有（CLI 等把工具放顶层的客户端）行为与顺序不变。
pub fn collect_declared_tools(body: &Value) -> Vec<Value> {
    let mut out: Vec<Value> = body
        .get("tools")
        .and_then(|t| t.as_array())
        .cloned()
        .unwrap_or_default();
    if let Some(items) = body.get("input").and_then(|i| i.as_array()) {
        for it in items {
            let hoist = matches!(
                it.get("type").and_then(|t| t.as_str()),
                Some(ADDITIONAL_TOOLS_ITEM) | Some(TOOL_SEARCH_OUTPUT_ITEM)
            );
            if !hoist {
                continue;
            }
            if let Some(arr) = it.get("tools").and_then(|t| t.as_array()) {
                out.extend(arr.iter().cloned());
            }
        }
    }
    out
}

/// 某个 Responses 工具声明对上游模型暴露的名字。
///
/// 多数工具带 `name`；`tool_search` 这类**无 `name`** 的 Codex 内置类型，名字即其 `type`。
/// 此前请求侧统一 `let Some(name) = t.get("name") else { continue }`，直接把 `tool_search`
/// 跳过 → 模型不知道有检索器可用 → 永远发不出 `tool_search_call` → MCP 工具永远拿不到 schema。
///
/// 只对**白名单内**的无名类型放行（当前仅 `tool_search`）。刻意不放行 `web_search`：
/// 那是**服务商侧**执行的内置工具（Codex 侧无 `execution` 字段、由 OpenAI 后端跑），
/// 经 SynaRoute 转到 Anthropic 上游后没有任何一方能执行它，暴露只会诱导模型空调。
pub(super) fn declared_tool_name(t: &Value) -> Option<String> {
    if let Some(n) = t.get("name").and_then(|n| n.as_str()) {
        if !n.is_empty() {
            return Some(n.to_string());
        }
    }
    match t.get("type").and_then(|ty| ty.as_str()) {
        Some(TOOL_SEARCH_TYPE) => Some(TOOL_SEARCH_TYPE.to_string()),
        _ => None,
    }
}

/// 收集本次请求声明的「客户端执行型检索工具」名字集合（当前即 `tool_search`）。
/// 供响应侧判定：模型对该名字的调用要回程成 `tool_search_call` item（而非 `function_call`），
/// 否则 Codex 认不出、检索发不起来，延迟加载的 MCP 工具就永远解锁不了。
pub fn collect_search_tools(body: &Value) -> std::collections::HashSet<String> {
    collect_declared_tools(body)
        .iter()
        .filter(|t| t.get("type").and_then(|ty| ty.as_str()) == Some(TOOL_SEARCH_TYPE))
        .filter_map(declared_tool_name)
        .collect()
}

/// 从请求 tools 收集所有 Codex namespace 折叠工具的 namespace 名（如 `mcp__synaroute`）。
/// 供响应侧把上游模型回调的全名 `<ns>__<sub>` 拆回 Codex router 需要的 {name, namespace} 两字段。
/// 按长度降序排列，保证前缀匹配时优先匹配更长（更具体）的 namespace。
/// 经 [`collect_declared_tools`] 取工具，故顶层 `tools`、`additional_tools`、`tool_search_output`
/// 三种承载都覆盖——尤其第三种：MCP 的 `mcp__*` namespace **只**出现在那里，漏了就拆不回
/// `{namespace:"mcp__synaroute", name:"synaroute_ai"}`，Codex router 查表失败报 unsupported call。
pub fn collect_tool_namespaces(body: &Value) -> Vec<String> {
    let mut names: Vec<String> = collect_declared_tools(body)
        .iter()
        .filter(|t| t.get("type").and_then(|ty| ty.as_str()) == Some("namespace"))
        .filter_map(|t| t.get("name").and_then(|n| n.as_str()).map(|s| s.to_string()))
        .filter(|s| !s.is_empty())
        .collect();
    // 长的在前：`a__b` 与 `a` 同时存在时，`a__b__x` 应归到 `a__b` 而非 `a`。
    names.sort_by_key(|b| std::cmp::Reverse(b.len()));
    names.dedup();
    names
}

/// 从请求 tools 收集所有 Codex `type:"custom"` 工具名（如 `apply_patch`、桌面端的 `exec`）。
/// Codex 对这类工具期望响应侧回程 item type 为 `custom_tool_call`（非 `function_call`），
/// 否则 Codex router 认不出、工具执行失败。响应侧据此集合判定每个工具调用该发哪种 item type。
/// 经 [`collect_declared_tools`] 取工具，故三种承载都覆盖。
pub fn collect_custom_tools(body: &Value) -> std::collections::HashSet<String> {
    collect_declared_tools(body)
        .iter()
        .filter(|t| t.get("type").and_then(|ty| ty.as_str()) == Some("custom"))
        .filter_map(|t| t.get("name").and_then(|n| n.as_str()).map(|s| s.to_string()))
        .filter(|s| !s.is_empty())
        .collect()
}

/// 把 custom 工具的 Chat 形态参数（JSON 字符串 `arguments`）解包成 Codex 期望的裸字符串 `input`。
///
/// Codex 的 `custom_tool_call` item 携带的是**裸字符串** `input` 字段（如 apply_patch 的 patch 正文、
/// exec 的命令），而非 `function_call` 那样的 JSON `arguments`。上游模型（走 Anthropic 标准 tool_use）
/// 拿到的是 `{type:object, properties:{input:{type:string}}}` 之类 schema，回来的 arguments 通常形如
/// `{"input":"*** Begin Patch\n..."}`。此处按优先级解包：
/// 1. 能解析成 JSON 对象且含字符串键 `input` → 取该字符串（最常见）；
/// 2. 对象只有单个字符串值字段 → 取该值（模型用了别的键名时兜底）；
/// 3. 本身就是 JSON 字符串标量 → 取其内容；
/// 4. 其余（无法解析/结构不符）→ 原样返回（避免吞掉内容）。
pub fn unpack_custom_tool_input(arguments: &str) -> String {
    let trimmed = arguments.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    match serde_json::from_str::<Value>(trimmed) {
        Ok(Value::Object(map)) => {
            if let Some(s) = map.get("input").and_then(|v| v.as_str()) {
                return s.to_string();
            }
            // 单字段对象且值为字符串：容忍模型换了键名
            if map.len() == 1 {
                if let Some(s) = map.values().next().and_then(|v| v.as_str()) {
                    return s.to_string();
                }
            }
            arguments.to_string()
        }
        Ok(Value::String(s)) => s,
        _ => arguments.to_string(),
    }
}

/// 把上游模型回调的工具全名按已知 namespace 列表拆回 (namespace, sub_name)。
/// 全名形如 `<ns>__<sub>`；命中某 namespace 前缀则返回 (Some(ns), sub)，否则 (None, 原名)。
/// 平铺工具（如 Codex 内置 update_plan）无 namespace 前缀，原样返回，不受影响。
pub fn split_namespaced_tool_name(full: &str, namespaces: &[String]) -> (Option<String>, String) {
    for ns in namespaces {
        let prefix = format!("{ns}__");
        if let Some(sub) = full.strip_prefix(&prefix) {
            if !sub.is_empty() {
                return (Some(ns.clone()), sub.to_string());
            }
        }
    }
    (None, full.to_string())
}

/// [`split_namespaced_tool_name`] 的逆运算：把 Codex 历史 item 的 `{name, namespace}` 两字段
/// 拼回上游模型看到的**全名** `<ns>__<sub>`。无 `namespace` 字段时原样返回 `name`。
///
/// 为什么必需（2026-07-30 实测 `unsupported call` 根因）：请求侧把 namespace 折叠工具展开成
/// 全名（`mcp__synaroute__synaroute_ai`）暴露给上游模型，但历史里 Codex 存的是拆开的两字段
/// （`name:"synaroute_ai"` + `namespace:"mcp__synaroute"`）。若还原历史时只取 `name`，模型看到
/// 「我上一轮用 `synaroute_ai` 这个名字调用过」，下一轮就照抄这个短名 → 响应侧
/// `split_namespaced_tool_name` 拆不出 namespace（短名无前缀）→ 回程 item 缺 `namespace` 字段
/// → Codex router 查 `{namespace:None, name:"synaroute_ai"}` 匹配不到 → `unsupported call`。
/// 实机 rollout 三次调用中，唯一失败的那次正是 `ns=-`（模型抄了短名）。
///
/// 已是全名（`name` 本身就带 `<ns>__` 前缀）时不重复拼接，避免 `mcp__x__mcp__x__foo`。
pub(super) fn join_namespaced_tool_name(item: &Value) -> String {
    let name = item.get("name").and_then(|n| n.as_str()).unwrap_or("");
    let Some(ns) = item.get("namespace").and_then(|n| n.as_str()) else {
        return name.to_string();
    };
    if ns.is_empty() || name.is_empty() {
        return name.to_string();
    }
    let prefix = format!("{ns}__");
    if name.starts_with(&prefix) {
        name.to_string()
    } else {
        format!("{prefix}{name}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::upstream::testfix::*;
    use serde_json::json;
    #[test]
    fn collect_custom_tools_finds_custom_type() {
        let body = json!({
            "tools": [
                { "type": "custom", "name": "apply_patch" },
                { "type": "function", "name": "read_file" },
                { "type": "namespace", "name": "mcp__x" },
                { "type": "custom", "name": "exec" },
            ]
        });
        let result = collect_custom_tools(&body);
        assert_eq!(result.len(), 2);
        assert!(result.contains("apply_patch"));
        assert!(result.contains("exec"));
        assert!(!result.contains("read_file"));
    }

    #[test]
    fn collect_declared_tools_hoists_additional_tools_item() {
        // 根因回归护栏：顶层无 tools 时必须从 additional_tools 项里取。
        // 若退回只读顶层 `tools`，这里立即变红——那正是「Codex 桌面端工具/MCP 全调不起来」的成因。
        let body = codex_desktop_request();
        assert!(body.get("tools").is_none(), "夹具前提：顶层不应有 tools");
        let declared = collect_declared_tools(&body);
        let names: Vec<&str> = declared
            .iter()
            .filter_map(|t| t.get("name").and_then(|n| n.as_str()))
            .collect();
        assert_eq!(names, vec!["exec", "wait", "collaboration"], "三个声明工具都要收到");
    }

    #[test]
    fn collect_declared_tools_merges_top_level_and_additional() {
        // 两种承载并存时都收，且顶层在前（保持既有客户端行为与顺序不变）。
        let body = json!({
            "tools": [{ "type": "function", "name": "top_level_fn" }],
            "input": [
                { "type": "additional_tools", "role": "developer", "tools": [{ "type": "custom", "name": "exec" }] },
                { "type": "message", "role": "user", "content": [{ "type": "input_text", "text": "hi" }] }
            ]
        });
        let names: Vec<String> = collect_declared_tools(&body)
            .iter()
            .filter_map(|t| t.get("name").and_then(|n| n.as_str()).map(|s| s.to_string()))
            .collect();
        assert_eq!(names, vec!["top_level_fn", "exec"]);
    }

    #[test]
    fn custom_and_namespace_collectors_see_additional_tools() {
        // 响应侧两个收集器也必须覆盖 additional_tools：
        // - custom 集合缺 exec → 回程发 function_call + JSON arguments，Codex router 认不出；
        // - namespace 列表缺 collaboration → 子工具全名拆不回 {namespace,name}，报 unsupported call。
        let body = codex_desktop_request();
        let custom = collect_custom_tools(&body);
        assert!(custom.contains("exec"), "exec 必须被识别为 custom 工具");
        let ns = collect_tool_namespaces(&body);
        assert_eq!(ns, vec!["collaboration"], "namespace 必须被收集");
    }

    #[test]
    fn join_namespaced_name_rejoins_two_fields() {
        // Codex 历史里 MCP 工具存成 {name, namespace} 两字段，须拼回上游模型看到的全名。
        assert_eq!(
            join_namespaced_tool_name(&json!({ "name": "synaroute_ai", "namespace": "mcp__synaroute" })),
            "mcp__synaroute__synaroute_ai"
        );
        // 无 namespace（平铺工具，如 Codex 内置 update_plan）：原样返回。
        assert_eq!(
            join_namespaced_tool_name(&json!({ "name": "update_plan" })),
            "update_plan"
        );
        // 空 namespace 视作无。
        assert_eq!(
            join_namespaced_tool_name(&json!({ "name": "foo", "namespace": "" })),
            "foo"
        );
        // 已是全名时不得重复拼接（否则 mcp__x__mcp__x__foo）。
        assert_eq!(
            join_namespaced_tool_name(
                &json!({ "name": "mcp__synaroute__synaroute_ai", "namespace": "mcp__synaroute" })
            ),
            "mcp__synaroute__synaroute_ai"
        );
        // 缺 name：不 panic，返回空串。
        assert_eq!(
            join_namespaced_tool_name(&json!({ "namespace": "mcp__x" })),
            ""
        );
    }

    #[test]
    fn namespace_name_round_trips_through_split() {
        // 闭环：拼回的全名必须能被响应侧 split_namespaced_tool_name 原样拆开，
        // 否则回程 item 依旧缺 namespace 字段。这条把请求侧与响应侧的口径钉在一起。
        let item = json!({ "name": "synaroute_ai", "namespace": "mcp__synaroute" });
        let full = join_namespaced_tool_name(&item);
        let (ns, sub) = split_namespaced_tool_name(&full, &["mcp__synaroute".to_string()]);
        assert_eq!(ns.as_deref(), Some("mcp__synaroute"), "namespace 应能拆回");
        assert_eq!(sub, "synaroute_ai", "子工具名应能拆回");
    }

    #[test]
    fn unpack_custom_tool_input_variants() {
        // 1. 最常见：{"input":"<裸串>"} → 取 input
        assert_eq!(
            unpack_custom_tool_input("{\"input\":\"*** Begin Patch\\nhi\"}"),
            "*** Begin Patch\nhi"
        );
        // 2. 单字段对象换了键名 → 取该字符串值
        assert_eq!(unpack_custom_tool_input("{\"cmd\":\"ls -la\"}"), "ls -la");
        // 3. JSON 字符串标量 → 取其内容
        assert_eq!(unpack_custom_tool_input("\"raw string\""), "raw string");
        // 4. 空串 → 空串
        assert_eq!(unpack_custom_tool_input("   "), "");
        // 5. 非 JSON（本身就是裸串）→ 原样返回，不吞内容
        assert_eq!(unpack_custom_tool_input("*** Begin Patch"), "*** Begin Patch");
        // 6. 多字段对象无 input → 原样返回整串（避免误取）
        let multi = "{\"a\":\"x\",\"b\":\"y\"}";
        assert_eq!(unpack_custom_tool_input(multi), multi);
    }
}
