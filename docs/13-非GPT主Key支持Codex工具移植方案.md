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
| **developer 消息内嵌 tools 解析** | ✅(顶层 tools) | ❌ 未解析 developer 内嵌 |
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
