# 非 GPT 主 Key 在 Codex 桌面端使用 MCP/工具的移植方案

> 目标:让用户把 codex 分类主 Key 配成非 GPT 模型(如 claude-opus),在 Codex 桌面端里仍能正常触发 MCP 工具(synaroute_ai)与内置工具。
>
> 结论:**技术可行**。参考 opencodex(TypeScript,~5k stars,活跃)已在生产验证同类做法。

---

## ✅ 已闭环（2026-07-31 实测取证，**本文第三、四、五节的判断已过时**）

本文写于 `2f51fa8` 之前。那个提交（"tool_search 链路打通 + namespace 全名往返"）
**已经把第三节列的四项差距做掉了三项**；而剩下那个「只能实测判定」的未知，
现已从 Codex 自己的日志库取证闭环 —— **结论是不需要再开发**。详见第十一节。

| 第三节列的差距 | 文档当时 | 现在（2026-07-31 核对代码） |
|---|---|---|
| custom/freeform 工具识别 | ❌ 无 | ✅ `upstream.rs` 有 `freeform_custom_tool_schema()`，为只带 `format:{type:"grammar"}` 的 freeform 工具（如 `exec`）合成 `{input: string}` schema |
| freeform 回程转 `custom_tool_call` | ❌ 无 | ✅ 已实现，三处：`upstream.rs:1397`（改写函数）、`:1477`（history 还原）、`:1623`（响应侧转换） |
| `tool_search` 处理 | ❌ 无 | ✅ 整条链打通：`collect_declared_tools` 从 `input[]` 里上提 `additional_tools`/`tool_search_output` 的 tools；`declared_tool_name` 认「无 name、名字即 type」的 `tool_search`；`rewrite_to_tool_search_call` 回程改写 |
| namespace 折叠展开 / 回程拆 `{ns,name}` | ✅ | ✅ 另加 `join_namespaced_tool_name` 补上了「history 里 `function_call` 丢 namespace 导致模型抄短名、响应侧拆不出」这个缺陷 |
| SSE 流式 freeform 翻译 | ⚠️ 部分 | ✅ `SseTranslator::with_namespaces_and_custom` 同时带 custom/search 两个集合 |

### 那个「只能实测」的未知 —— 已有答案

原问题：exec 是「模型写 JS 编排多个工具」的范式，而 opus 走 Anthropic 协议一次只能发一个具名
`tool_use`。**Codex 客户端会不会接受「一次只调一个工具」的形态？**

**答案：接受，且 opus 会主动写 JS。** 实测 9 次调用全部是
`custom_tool_call name=exec` + JS 源码，一次都没退化成 `output_text`。取证过程与原始记录见第十一节。

故：
- ~~「codex 分类主 Key 用 gpt」这条「当前推荐」已过时~~ —— 直接配 opus 即可。
- ~~拍平方案（把沙箱内工具降维成一等公民 function）~~ —— **不做**，前提已被证伪，见第十一节
  「为什么不做拍平」。

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

> ⚠️ **下表是 2026-07-30 的快照，已过时。** 其中三项在 `2f51fa8` 已完成 ——
> 看本文开头的「现状回填」表。保留原表是为了记录当时的判断依据。

| 能力 | opencodex | SynaRoute 现状（旧，勿用） |
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

> ⚠️ **WP1 / WP2 已完成**（`2f51fa8`），下面两节保留作实现记录。
> 未做的只有 WP3 的**端到端实测**，见本文开头的「实测步骤」。

### WP1:请求侧识别 freeform/custom 工具 ✅ 已完成
- `upstream.rs` `openai_tools_to_anthropic` / `convert_request_responses_to_anthropic`:
  - 新增分支处理 `type:"custom"` → 转成 Anthropic tool(input_schema 用 `{type:object, properties:{input:{type:string}}}` 或自由 schema)
  - 收集 `freeform_tool_names: HashSet<String>`,随请求上下文传递到响应侧
- 若 Codex 桌面端把 tools 放 developer 消息:需在解析时下探 `input[].content` 里 role=developer 的 `tools` 字段(**需再抓一次桌面端完整请求确认结构**,当前只确认了 developer 消息里有 tools)

### WP2:回程侧 freeform → custom_tool_call ✅ 已完成
- `upstream.rs` SSE 翻译器(`SseTranslator`)与非流式响应转换:
  - 已有 `tool_namespaces` 机制,新增 `freeform_tool_names` 字段
  - 模型回 tool_use 时,若名字命中 freeform 集合 → 输出 `custom_tool_call`(input 解包成裸字符串),否则现有 `function_call` 逻辑
- `emit_responses_completed` 增加 freeform 分支

### WP3:测试 + 验证 ✅ 单测已有，**端到端已从 Codex 日志库取证闭环**（见第十一节）
- 单元测试:custom 工具往返(request 转换保留、response 转回 custom_tool_call)
- 端到端:codex 分类配 opus 主 Key,Codex 桌面端调 synaroute_ai,核对 rollout 里是 `custom_tool_call`/`function_call` 正确形态而非 `output_text`

### 风险与未知
1. ✅ **已关闭**（2026-07-30，见第六节）：桌面端工具在 `input[0]` 的 `additional_tools` 独立 item 里，非 developer 消息的 `content`。
2. ✅ **已关闭**（2026-07-31，见第十一节）：实测 opus 在 EXEC 形态下 **9 次调用全部走 `exec`**，一次都没退化成纯文本假装调用，且自己写 JS 枚举 `ALL_TOOLS` 探测环境、成功调起 MCP。「模型读不懂 exec 契约」这个前提被证伪，故**不做**拍平（原第四节 WP1 提的降维方案）。
3. **Anthropic 账号风险** —— opencodex 自身警告:通过第三方代理路由 API 流量,Anthropic 可能限制账号。

---

## 五、决策建议 —— **已由第十一节的实测取证取代，本节仅存档**

> 下面两条是 2026-07-30 尚未取证时的判断。**第一条已过时**：实测证明 codex 分类主 Key 直接配
> opus 就能调起 MCP 与内置工具，不必退回 gpt。**第二条已否决**：拍平方案的前提（模型读不懂
> exec）被证伪。结论见第十一节。

- ~~**推荐(零成本)**:codex 分类主 Key 用 gpt(Codex MCP 原生就通),opus 作聚合成员参与会诊~~
  → 已过时：opus 作主 Key 亦可用。
- ~~**要做本方案**:先补 WP1 抓桌面端完整请求确认 tools 结构,再决定~~
  → 已否决：见第十一节「为什么不做拍平」。

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

### ~~仍未闭合的风险 2~~ → ✅ 已闭合（2026-07-31，见第十一节）

> 原文保留作记录：工具已能到达 opus，但 `exec` 本质仍是「写 JS 编排」范式：要读文件/调 MCP，
> opus 必须发一次 `exec` 调用、`input` 里是 JS 源码。它**能否稳定选择这条路**属模型行为，需实测。
> 若不稳定，下一步是把沙箱内的工具拍平成一等公民 function 工具（opencodex 的降维思路）。

**实测答案：稳定选择。** 9 次调用 9 次都走 `exec`（含自己写 JS 枚举 `ALL_TOOLS` 探测环境），
一次都没退化成 `output_text`。拍平方案的前提被证伪，**不做**。详见第十一节。

---

## 十一、实测取证：opus 主 Key 在 Codex 里已能用工具与 MCP（2026-07-31，风险 2 关闭）

本节是对第四节「风险与未知」第 2 条的**取证结论**。数据全部来自 Codex 自己的日志库，
非推测、非单次偶然。

### 取证方法（可复现）

Codex 把发出的完整请求体记进 `~/.codex/logs_2.sqlite`（本机约 400 MB，有 WAL，
用 `node:sqlite` 以 `readOnly:true` 直开，不必拷贝）：

```bash
node --experimental-sqlite -e '
const {DatabaseSync}=require("node:sqlite");
const db=new DatabaseSync("C:/Users/<你>/.codex/logs_2.sqlite",{readOnly:true});
const rows=db.prepare("SELECT ts,feedback_log_body FROM logs WHERE target=? AND length(feedback_log_body)>20000 ORDER BY ts DESC LIMIT 900").all("codex_http_client::transport");
for(const r of rows){
  // 注意：正文前面有一大段 tracing span 前缀，JSON 从 ": {" 处开始
  const at=r.feedback_log_body.indexOf(": {");
  const body=JSON.parse(r.feedback_log_body.slice(at+2));
  const url=(r.feedback_log_body.match(/POST to (\S+?):\s*\{/)||[])[1];
  // …
}'
```

两个关键技巧：

1. **JSON 起始位置**要用 `indexOf(": {")`，不能用 `indexOf("{")` —— 前缀里的
   `span{k=v}` 会让后者切错位置，解析必失败（我第一次就踩了，表现为「一条都解析不出来」）。
2. **怎么确认某条请求的真实上游是 Claude**：看 `input[]` 里 `call_id` 的前缀。
   `toolu_bdrk_*` 是 Anthropic/Bedrock 的工具调用 id 格式；`call_*` 是 OpenAI 的。
   Codex 请求体里的 `model` 字段是**用户在 Codex 侧配的名字**，不代表真实上游。

### 结论一：工具承载形态由「Codex 侧配的模型名」决定，与真实上游无关

900 条大请求按模型名统计：

| Codex 侧模型名 | `additional_tools` + exec 沙箱 | 顶层 `tools` |
|---|---|---|
| `gpt-5.6-terra` / `-sol` / `-luna` | 123 / 67 / 51 | 0 |
| `gpt-5.5` / `gpt-5.4` | 0 | 25 / 48 |
| `claude-opus-4-7` | 0 | 18 |
| `codex-auto-review` | 0 | 71 |

名字是 `gpt-5.6-*` → Codex 把工具塞进 `input[0]` 的 `additional_tools`（只声明一个 `exec`，
其余 34 个工具藏在它 10558 字符的 description 与运行时 `ALL_TOOLS` 里）；
其余名字 → 走顶层 `tools`（含 `function` / `custom` / `namespace` / `tool_search` / `web_search`）。

**SynaRoute 两种形态都已支持**，故这只是背景知识，不构成用户需要做的选择。

### 结论二：两种形态下 opus 都成功调起 MCP

**EXEC 形态**（`model=gpt-5.6-sol`，发往 `http://127.0.0.1:47101`，`call_id` 全为 `toolu_bdrk_*`）
—— opus 连发 9 次 `exec`，每次都拿到 `custom_tool_call_output`：

```
custom_tool_call name=exec input="const r = await tools.shell_command({command: \"Test-Path …
custom_tool_call name=exec input="const r = await tools.list_mcp_resources({}); text(\"RESULT: …
custom_tool_call name=exec input="const names = ALL_TOOLS.map(t => t.name); text(names.join(\"\\n\"));
```

最后那次的输出即沙箱内真实工具清单（34 个），`mcp__synaroute__synaroute_ai` 在列：

```
apply_patch, codex_app__*(14 个), create_goal, get_goal,
list_mcp_resource_templates, list_mcp_resources,
mcp__sqlcl__*(8 个), mcp__synaroute__synaroute_ai,
read_mcp_resource, shell_command, update_goal, update_plan, view_image
```

**标准形态**（`model=gpt-5.5`，同样 `toolu_bdrk_*`）—— 直接发具名调用并拿到真实结果：

```
function_call        name=synaroute_ai  namespace=mcp__synaroute
function_call_output outLen=2202   ← 真实聚合结果（含决策者降级提示与顾问意见全文）
tool_search_call     arguments={"limit":15,"query":"synaroute route"}
tool_search_output   tools=[{type:"namespace", name:"mcp__synaroute", tools:[synaroute_ai]}]
```

`tool_search` 那一对证明**延迟工具检索链也通**：opus 主动检索 → Codex 本地 BM25 返回
MCP 真 schema → 下一轮才有 `mcp__*` 工具可调。这条正是 `2f51fa8` 修的那个链路。

### 为什么不做拍平

原方案（第四节 WP1 的降维思路）的前提是「opus 读不懂 exec 契约、只会吐纯文本假装调用」。
实测 **9/9 全走 exec**，且它自己写 JS 枚举 `ALL_TOOLS` 探测环境 —— 前提不成立。

硬做拍平的代价：

1. 要从 `exec` 那 10558 字符的**自然语言描述**里解析 7 个
   ``` ```ts declare const tools: { shell_command(args: {…}) } ``` ``` 块来生成 JSON schema。
   Codex 一升级格式就散。
2. **注定拍不全**：exec 的描述自己写着
   > Some deferred nested tools may be omitted from this description. They are still available
   > on the global `tools` object and listed in `ALL_TOOLS`.

   有些工具只在运行时的 `ALL_TOOLS` 里，请求侧根本拿不到。
3. 回程还要把模型的具名调用**重新包装成 JS 源码**（因为 Codex 客户端只认
   `custom_tool_call name=exec`），等于代理替模型写 JS。

收益是「模型不必理解 exec」，而它已经理解了。故**关闭该项，不动代码**。

若日后 Codex 改版导致模型行为退化（表现为 rollout 里又出现 `output_text` 假装调用），
再按上面第 1~3 点评估；届时建议做成「exec 透传为主 + 连续 N 次假装调用才降级拍平」的兜底，
而非一上来替换主路径。

### 顺带修掉的一处协议映射偏差（skills 相关）

**skills 不是工具，是 developer 消息里的纯 Markdown。** 实测确认顶层 `tools` 与
`additional_tools` 里都**没有**任何 skill 字段；Codex 把技能说明放在两条 developer 消息里：

```
input[1] role=developer  "# Using skills" 使用说明（约 18 KB）
input[2] role=developer  "### Skill roots" + "### Available skills" 清单
   - `r0` = `C:/Users/<你>/.codex/skills`
   - `r1` = `C:/Users/<你>/.codex/skills/.system`
   - imagegen: … (file: r1/imagegen/SKILL.md)
   - skill-creator: … (file: r1/skill-creator/SKILL.md)
```

模型用 skill 的方式是**读 `SKILL.md` 文件再照做**，走的是 `shell_command` —— 那条已通。
所以功能上不缺东西。

但发现一处偏差：`openai_to_anthropic` 原先把 `developer` 角色落进 `_ =>` 分支、**降级成
普通 user 消息**。OpenAI 侧 `developer` 语义等价于 `system`（o1 系起改名），而 Anthropic 的
`system` 是独立字段、权重高于对话消息。skills 说明里「Trigger rules: 用户点名或任务匹配时
**必须**使用该 skill」这类强指令被降级后与用户自己的话混在同一层，遵守程度会下降。

已改为 `"system" | "developer"` 并列，多段之间补空行分隔（Codex 会连发多条 developer，
首尾相接会让上一段末尾与下一段标题黏成一行、破坏 Markdown 结构）。
护栏：`developer_role_maps_to_anthropic_system_not_user`、
`multiple_system_and_developer_messages_are_joined_with_blank_line`、
`empty_developer_message_does_not_pollute_system`（均已用故障注入验证过对旧行为变红）。

**这条是「语义权重」修复而非功能修复**，效果需真机观察：在 Codex 里对 opus 主 Key 显式点名
一个 skill（如 `$skill-creator`），看它是否确实先读 `SKILL.md` 再动手。
