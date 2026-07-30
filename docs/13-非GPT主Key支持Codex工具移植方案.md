# 非 GPT 主 Key 在 Codex 桌面端使用 MCP/工具的移植方案

> 目标:让用户把 codex 分类主 Key 配成非 GPT 模型(如 claude-opus),在 Codex 桌面端里仍能正常触发 MCP 工具(synaroute_ai)与内置工具。
>
> 结论:**技术可行**。参考 opencodex(TypeScript,~5k stars,活跃)已在生产验证同类做法。SynaRoute 已具备核心一半(namespace 处理),差的是 freeform/custom 工具那条链。

---

## 一、根因回顾(全部来自实测日志)

Codex 桌面端(gpt-5.6 系,client `Codex Desktop/0.146`)的工具调用**不是标准 OpenAI function calling**,而是一套 **GPT Responses 专有的 exec 编排范式**:

1. 工具打包进 `input` 里 `role:"developer"` 消息的 `tools` 字段(不是顶层 `tools`)
2. 模型在 V8 沙箱里写 JS:`await tools.mcp__synaroute__synaroute_ai({...})`
3. 内置工具含 `exec`/`apply_patch` 等 **freeform(custom)工具** —— 调用形态是 `custom_tool_call`(input 是裸代码字符串,非 JSON 参数)
4. MCP 工具以 `type:"namespace"` 折叠容器出现,子工具在 `tools[]` 里

**非 GPT 模型(opus)走 Anthropic 协议时**:当前 SynaRoute 把这套原样透传,opus 读不懂 exec 编排 → 只吐 `output_text` 纯文本假装调用工具(实测:`Tool: search` + 编造的 `path:"unknown"`)。

---

## 二、opencodex 的解法(源码实证)

opencodex 不去"翻译 exec 沙箱语义",而是做**降维 + 双向映射**:

### 请求侧(`src/responses/parser.ts` `buildTools`)
把 Codex 请求的 tools 解析成统一中间表示 `OcxTool`,按 4 类处理:
| Codex 类型 | 处理 | OcxTool 标记 |
|---|---|---|
| `function` | 直接收 | 普通 |
| `namespace`(MCP 折叠) | 展开内部 `tools[]`,名字拼 `<ns>__<sub>` | `namespace` 字段 |
| `custom`(exec/apply_patch) | 收下 | `freeform: true` |
| `tool_search` | 收下 | `toolSearch: true` |

关键:建立 `freeformToolNames` / `toolSearchToolNames` 两个 Set,回程查表用。

### 转发侧(`src/adapters/anthropic.ts` `toolsToAnthropicFormat`)
所有工具(含 freeform)统一转成 Anthropic 标准 `tools[{name, input_schema}]`,让 opus 用**标准 tool_use** 调用。freeform 工具也暴露成普通 tool,模型正常调。

### 回程侧(`src/bridge.ts`)
模型的 tool_use 响应,按类型查表转回 Codex 期望形态:
| OcxTool 标记 | 转回 Codex 形态 | 关键处理 |
|---|---|---|
| 普通 + namespace | `function_call` | 带 `namespace` 字段(Codex 按此路由 MCP,**不拆 name**) |
| `freeform` | `custom_tool_call` | input 从 `{input:"..."}` 解包成裸字符串 |
| `toolSearch` | `tool_search_call` | arguments 解析成 `{query,limit}` |

flat 名 `<ns>__<sub>` 回程拆回 `{namespace, name}` 两字段。

---

## 三、SynaRoute 现状对照

| 能力 | opencodex | SynaRoute 现状 |
|---|---|---|
| namespace 折叠展开 | ✅ | ✅ `openai_tools_to_anthropic` Some("namespace") |
| 回程 flat 名拆 {ns,name} | ✅ | ✅ `split_namespaced_tool_name` + `collect_tool_namespaces` |
| **custom/freeform 工具识别** | ✅ `freeform:true` | ❌ 无 |
| **freeform 回程转 custom_tool_call** | ✅ | ❌ 无 |
| **tool_search 处理** | ✅ | ❌ 无 |
| **developer 消息内嵌 tools 解析** | ✅(顶层 tools) | ✅ 已补(见下「六、桌面端请求结构已抓实」) |
| SSE 流式 tool_use↔custom_tool_call 翻译 | ✅ | ⚠️ 部分(namespace 已做,freeform 未做) |

**差距聚焦**:freeform 那条链(识别 → 转发暴露 → 回程转 custom_tool_call)+ 可能的 developer 内嵌 tools 解析。

---

## 四、落地步骤(SynaRoute)

### WP1:请求侧识别 freeform/custom 工具
- `upstream.rs` `openai_tools_to_anthropic` / `convert_request_responses_to_anthropic`:
  - 新增分支处理 `type:"custom"` → 转成 Anthropic tool(input_schema 用 `{type:object, properties:{input:{type:string}}}` 或自由 schema)
  - 收集 `freeform_tool_names: HashSet<String>`,随请求上下文传递到响应侧
- 若 Codex 桌面端把 tools 放 developer 消息:需在解析时下探 `input[].content` 里 role=developer 的 `tools` 字段(**需再抓一次桌面端完整请求确认结构**,当前只确认了 developer 消息里有 tools)

### WP2:回程侧 freeform → custom_tool_call
- `upstream.rs` SSE 翻译器(`SseTranslator`)与非流式响应转换:
  - 已有 `tool_namespaces` 机制,新增 `freeform_tool_names` 字段
  - 模型回 tool_use 时,若名字命中 freeform 集合 → 输出 `custom_tool_call`(input 解包成裸字符串),否则现有 `function_call` 逻辑
- `emit_responses_completed` 增加 freeform 分支

### WP3:测试 + 验证
- 单元测试:custom 工具往返(request 转换保留、response 转回 custom_tool_call)
- 端到端:codex 分类配 opus 主 Key,Codex 桌面端调 synaroute_ai,核对 rollout 里是 `custom_tool_call`/`function_call` 正确形态而非 `output_text`

### 风险与未知
1. **Codex 桌面端 exec 编排仍在演进** —— opencodex 自己有未闭合的相关 devlog(`260702_cursor-toolcall-mcp-empty`)。桌面端把工具嵌 developer 消息 + `ALL_TOOLS` 全局变量的机制,比标准 Responses 复杂,**移植前必须再抓一次桌面端完整请求体(未截断)确认 tools 到底在哪、什么结构**。
2. **exec 是"写 JS 编排多工具"范式** —— opus 发标准 tool_use 只能一次调一个具名工具。若 Codex 桌面端强依赖 JS 编排(而非单工具调用),单纯 freeform 映射可能不够。需实测 opus 发单个 tool_use 时 Codex 客户端认不认。
3. **Anthropic 账号风险** —— opencodex 自身警告:通过第三方代理路由 API 流量,Anthropic 可能限制账号。

---

## 五、决策建议

- **推荐(零成本)**:codex 分类主 Key 用 gpt(Codex MCP 原生就通),opus 作聚合成员参与会诊。**当前已验证可用**。
- **要做本方案**:先补 WP1 抓桌面端完整请求确认 tools 结构,再决定;核心代码差距不大(freeform 链),但桌面端 exec 编排的未知项需实测降险。直接参考 opencodex `parser.ts` + `bridge.ts` + `anthropic.ts` 移植,不从零写。

参考:opencodex https://github.com/lidge-jun/opencodex(`src/responses/parser.ts`、`src/adapters/anthropic.ts`、`src/bridge.ts`)

---

## 六、桌面端请求结构已抓实(2026-07-30)

WP1 里「需再抓一次桌面端完整请求确认 tools 到底在哪」这项已完成，**风险 1 关闭**。

### 取证手段(可复现)

Codex 把发给 SynaRoute 的完整请求体记在 `~/.codex/logs_2.sqlite`，`target='codex_http_client::transport'` 的行，正文形如 `POST to http://127.0.0.1:47101/v1/responses: {…}`（未截断）。该库有 WAL、体量数百 MB，用 `node:sqlite` 以 `readOnly: true` 直接打开即可，不必拷贝：

```
SELECT ts, feedback_log_body FROM logs
WHERE target='codex_http_client::transport' ORDER BY ts DESC LIMIT 1
```

### 确认的结构

**顶层没有 `tools` 字段**。工具全在 `input` 数组第 0 项，且**不是**嵌在 developer 消息的 `content` 里，而是一个独立 item type：

```json
{ "type": "additional_tools", "role": "developer", "tools": [ … ] }
```

该 item 只有 `type` / `role` / `tools` 三个键（**无 `content`**）。当次会话的 4 个声明工具：

| type | name | 载荷形态 |
|---|---|---|
| `custom` | `exec` | 有 `format: {type:"grammar", syntax:"lark", …}`，**无** `inputSchema`/`parameters` —— 载荷是裸 JS 源码 |
| `function` | `wait` | 常规 `parameters` JSON schema |
| `function` | `request_user_input` | 常规 `parameters` |
| `namespace` | `collaboration` | 内嵌 6 个子工具(`spawn_agent` 等)，各带 `parameters` |

`shell` / `exec_command` / `apply_patch` / `update_plan` / MCP 工具**都不是独立声明的工具**——它们只作为文字出现在 `exec` 那 14 KB 描述里，实际只能在 V8 沙箱内经全局 `tools` 对象调用（`await tools.exec_command(…)`、`await tools.mcp__synaroute__synaroute_ai(…)`）。`synaroute` 一词连描述里都没有，沙箱内靠 `ALL_TOOLS` 运行时枚举。

其余：`tool_choice: "auto"`、`parallel_tool_calls: false`、无 `tool_search` 类型工具（故 WP1 的 tool_search 分支暂不需要）。

### 由此定位的根因与修复

`collect_declared_tools`/`collect_custom_tools`/`collect_tool_namespaces` 与 `responses_to_chat` 原先只读顶层 `tools` → 收到空集 → 转换后发往上游的 Anthropic 请求**不带任何 tools**。模型侧铁证（同库 `codex_api::sse::responses` 行，opus 的推理摘要原文）：

> I'm realizing I don't actually have access to file reading tools in this context—there's no tool schema provided for me to invoke.

于是 opus 改为让用户手动跑 PowerShell，表现为「工具与 MCP 全都调不起来」。修复两处：

1. **提升 `additional_tools`**：新增 `collect_declared_tools`，取「顶层 `tools` ∪ 所有 `additional_tools` 项的 `tools`」，三个收集器与 `responses_to_chat` 全改走它；该 item 在消息循环里跳过，不再残留成空 developer 消息。
2. **freeform custom 工具兜底 schema**：`exec` 只有 `format` 没有 schema，原先兜底成 `{"type":"object"}`（无 properties），模型拿到一个「没有入参」的工具、无处安放代码。改为兜底 `{input: string}`，与响应侧 `unpack_custom_tool_input` 的解包口径对称。

WP2（freeform 回程转 `custom_tool_call` + 裸串 `input`，流式与非流式）在 v0.1.3/v0.1.4 已落地，此前只是因为收集器读不到 `exec` 而从未生效。

### 仍未闭合的风险 2

工具已能到达 opus，但 `exec` 本质仍是「写 JS 编排」范式：要读文件/调 MCP，opus 必须发一次 `exec` 调用、`input` 里是 JS 源码。它**能否稳定选择这条路**属模型行为，需实测。若不稳定，下一步是把沙箱内的工具拍平成一等公民 function 工具（opencodex 的降维思路），而非继续依赖模型理解 exec 契约。
