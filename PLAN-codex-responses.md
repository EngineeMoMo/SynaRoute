# 实现方案：Codex Responses API ↔ Chat 双向转换（取长补短 cc-switch）

## 背景与根因（已实查确认）

- **缺口位置**：[proxy.rs:194](src-tauri/src/proxy.rs:194) 把下游协议只二分为
  Anthropic（`/messages`）vs "OpenAI"（其它）。"OpenAI" 把两种**形态完全不同**的协议混为一谈：
  - **Chat Completions**：请求 `messages[]`、响应 `choices[].message`、端点 `/chat/completions`
  - **Responses API**：请求 `input`/`instructions`、响应 `output[]`、端点 `/responses`
- **Codex 走 Responses**（`wire_api="responses"`，默认 `stream:true`），而第三方厂商
  （DeepSeek/GLM/Kimi 等）大多**只支持 Chat**。故 Codex → Chat-only 厂商这条路现在是断的：
  端点错（打 `/chat/completions` 却发 Responses body）、形态错（上游收不到 `messages`）。
- **cc-switch 的做法**（已核实）：provider 标声明式 `apiFormat`（如 `openai_chat`），route 据此把
  `/responses` 改写成 `/chat/completions` 并双向转换 body 与 SSE；上游原生支持 Responses 则直通。

## 用户已确认的决策

- **数据模型**：三态枚举 `Protocol = anthropic | openai_chat | openai_responses`，对齐 cc-switch 的 apiFormat。
- **流式范围**：含流式全事件双向翻译（Codex 默认 stream）。
- **诚实边界**：Chat 上游的 SSE 只产出「文本增量 + tool_call 增量 + finish + usage」。Responses 的 53 种
  事件里 image/code_interpreter/file_search/web_search/audio/reasoning_summary 等是 Responses 原生能力，
  Chat 上游根本产不出来。故"翻译全事件"= 把 **Chat 能表达的那整套**完整、正确地重组成 Responses 事件序列；
  其余冷门事件因源头无数据而不出现。这是能力上限，非偷工。

## 改动清单

### A. 数据模型：Protocol 三态化（后端 + 前端）

1. `src-tauri/src/model.rs` — `enum Protocol` 增 `OpenaiResponses`；`Openai` 重命名为 `OpenaiChat`。
   - **平滑迁移旧配置**：`#[serde(rename_all=...)]` + 对旧值 `"openai"` 加 `#[serde(alias="openai")]`
     映射到 `OpenaiChat`（现有 DeepSeek/GLM/Kimi 预设与用户已存 Key 都是 Chat 语义，零破坏）。
   - 厂商预设 [model.rs:100+](src-tauri/src/model.rs:100)：OpenAI 官方预设可标 `OpenaiResponses`（原生支持），
     其余第三方保持 `OpenaiChat`。
2. `src/types.ts:10` — `Protocol` 改为 `"anthropic" | "openai_chat" | "openai_responses"`。
3. `src/components/KeyEditor.tsx:234` — 协议下拉增第三项「OpenAI Responses」；默认值/预设联动同步。
   - i18n：`src/lib/i18n.ts` 增 `editor.protocol.*` 选项文案（中英）。

### B. 下游协议判定：三态化（proxy.rs）

- 现 `downstream_is_anthropic = path.contains("/messages")` 二值判定，改为三值枚举
  `DownstreamKind { Anthropic, OpenaiChat, OpenaiResponses }`：
  - `path` 含 `/messages` → Anthropic
  - `path` 含 `/responses` → OpenaiResponses
  - 其余（`/chat/completions` 等） → OpenaiChat
- 影响点：[proxy.rs:194](src-tauri/src/proxy.rs:194)、[proxy.rs:279/334](src-tauri/src/proxy.rs:279)
  （流式直通/跨协议判定）、[proxy.rs:662-755](src-tauri/src/proxy.rs:662)（forward_to_key 转换矩阵）。
- **同格式直通**判定从「二值相等」升级为「DownstreamKind 与 Key.Protocol 同族」：
  - Anthropic↔Anthropic、OpenaiChat↔OpenaiChat、OpenaiResponses↔OpenaiResponses 直通（不转换）。
  - 其余组合走转换。

### C. 请求/响应转换（upstream.rs，非流式）

复用现有 [anthropic_to_openai](src-tauri/src/upstream.rs:560) 等函数的风格，新增 Responses 相关：

1. `responses_to_chat(body) -> Value`：Responses 请求 → Chat 请求
   - `input`（string 或 item 数组）→ `messages[]`；`instructions` → system 消息；
     `tools`（Responses function 形态）→ Chat `tools`；透传 `temperature/top_p/stream/max_output_tokens→max_tokens`。
2. `chat_resp_to_responses(body) -> Value`：Chat 非流式响应 → Responses 响应
   - `choices[0].message.content` → `output[{type:"message",content:[{type:"output_text",text}]}]`；
     `tool_calls` → `output[{type:"function_call",name,arguments,call_id}]`；
     `usage`（prompt/completion→input/output_tokens）；补 `id/status:"completed"`。
3. 转换矩阵（forward_to_key）扩为 3×3（下游 Kind × Key.Protocol）。跨到 Responses 上游的组合
   （下游非 Responses、Key 是 OpenaiResponses）也顺带支持，保证对称。
   - 端点选择 [proxy.rs:682-689](src-tauri/src/proxy.rs:682)：跨协议时按 Key.Protocol 选主端点
     （Anthropic→`/v1/messages`、OpenaiChat→`/v1/chat/completions`、OpenaiResponses→`/v1/responses`）。

### D. 流式 SSE 双向翻译（proxy.rs + upstream.rs，重点）

现 [try_stream_to_key](src-tauri/src/proxy.rs:530) 仅「同协议直通」，跨协议流式被
[proxy.rs:334](src-tauri/src/proxy.rs:334) 直接跳过。改为：**当下游 Responses、上游 Chat（或反向）时，
插入一个流式转换器**，逐块解析上游 SSE、增量重组为下游期望的事件序列。

- 新增 `sse.rs`（或 upstream.rs 内模块）：一个有状态的 `ChatToResponsesSse` 转换器：
  - 收到上游 Chat SSE `data: {choices:[{delta:{content}}]}` → 发下游
    `response.output_text.delta`（首次先发 `response.created`/`response.output_item.added`/`content_part.added`）。
  - 上游 `delta.tool_calls[].function.arguments` 增量 → `response.function_call_arguments.delta`；
    结束发 `.done`。
  - 上游 `data: [DONE]` / finish_reason → 补 `response.output_text.done`/`output_item.done`/
    `response.completed`（带 usage，若上游 `stream_options.include_usage` 提供；否则 usage 置零并标注）。
  - 反向 `ResponsesToChatSse`（下游 Chat、上游 Responses）：把 Responses 事件塌缩回 Chat `choices.delta`。
- 实现方式：把 reqwest `bytes_stream()` 过一个 `scan`/自定义 `Stream`，按 SSE 帧边界（`\n\n`）切分、
  逐帧转换后再 `Frame::data` 下发。保持现有「先探状态码、非 2xx 走 HttpError 切换」的安全语义。
- 流式跨协议不再无脑跳过；仅在**无任何可转换/同族候选**时才回退报错。

### E. Codex 配置写入修正（tools.rs，附带发现的问题）

[apply_codex](src-tauri/src/tools.rs:84) 目前给 Codex 写的是 `ANTHROPIC_BASE_URL`，这对走 OpenAI/Responses
的 Codex 存疑。**待确认**：Codex 该配 `OPENAI_BASE_URL` + `model_providers` 的 `wire_api="responses"` 指向
本地路由。此项影响"一键写配置"是否真能让 Codex 用起来。计划实现时先只读确认 Codex 实际配置键，再决定是否改
（可能独立小改动，不与 A–D 耦合）。

## 测试计划

- `upstream.rs` 单测：`responses_to_chat` / `chat_resp_to_responses` 往返与字段映射（含 tool_call、usage、
  instructions→system、input string vs 数组）。
- SSE 转换单测：喂一段真实 Chat SSE 分片（含跨帧半包），断言产出的 Responses 事件序列（created→delta*→
  output_text.done→completed）正确、usage 归位、tool_call 增量拼接正确。
- proxy 层：DownstreamKind 判定单测（/messages、/responses、/chat/completions）。
- 回归：现有 61 项后端测试 + `tsc` 全绿。

## 风险与边界

- **不改上游能力**：Chat-only 上游产不出 image/audio/reasoning_summary 等事件，翻译层只覆盖 Chat 可表达集。
- **usage 精度**：Chat 上游若不回 usage（未开 `include_usage`），Responses `completed.usage` 只能置零/估算——
  会在响应里如实标注，不伪造。
- **serde alias 迁移**：旧配置 `"openai"` 必须无损映射为 `openai_chat`，否则用户已存 Key 全部失效——这是最高危点，
  单测必须覆盖旧 JSON 反序列化。
