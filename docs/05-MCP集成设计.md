# SynaRoute MCP 集成设计大纲

> 本文是 MCP（Model Context Protocol）集成功能的设计定稿，供后续编写用户手册、项目说明书使用。
> 所有决策均由用户逐条确认（2026-07-17）。

## 1. 背景与定位

SynaRoute 的核心定位是「AI API 请求的中转港口」——路由、故障转移、协议转换、多模型聚合。

**大脑聚合（Brain Aggregation）** 让多个模型协作：参与者（只读）分析问题给建议，决策者综合意见输出最终方案。此前该功能只能在 SynaRoute 桌面端手动触发。

**MCP 集成** 的目标：让用户在 **Codex CLI / Claude Code** 等 AI 编程客户端里，直接调用 SynaRoute 的多模型聚合能力——用户在哪个项目里发起对话，聚合就针对哪个项目的代码进行分析，各项目互相隔离、独立并发。

**关键定位原则：SynaRoute 只出主意，不动手改文件。**
决策者返回「修改计划 / 建议」给 Codex 或 Claude Code，用户在客户端里确认后，由**客户端自己的原生工具**（apply_patch / edit_file）执行文件修改并弹出审批 UI。SynaRoute 不经手文件写入——保持「中转港口」的纯粹性。

## 2. 已确认的设计决策（Q1–Q10）

| # | 决策点 | 结论 |
|---|--------|------|
| Q1 | MCP 服务器运行形式 | **HTTP MCP，桌面端内置**，监听 `127.0.0.1:9527/mcp`（Streamable HTTP 传输） |
| Q2 | Hook（钩子）支持 | **两个都提供**：Claude Code hooks（`.claude/settings.json`）+ Codex AGENTS.md 提示词 |
| Q3 | 暴露几个工具 | **合并为 1 个工具** `synaroute_ai`，通过参数区分意图 |
| Q4 | 返回内容格式 | **兼容 Codex 和 Claude Code**：标准 MCP `content: [{type:"text", text}]`，text 用 Markdown |
| Q5 | MCP 通道是否写文件 | **不写文件**。决策者返回计划到 Codex/Claude，用户同意后由客户端自己改文件 |
| Q6 | 工具参数 schema | **基本参数**：`prompt`(必填)、`cwd`(可选)、`category`(可选)、`languageHint`(可选) |
| Q7 | 端口 | 默认 **9527**，可在设置页修改 |
| Q8 | 启动时机 | **启用即自动启动**，无独立「启动/停止」按钮，只有一个「启用 MCP 服务器」开关 |
| Q9 | 鉴权 | **不鉴权**（仅监听 127.0.0.1，本机可访问） |
| Q10 | 调用日志 | 写到**软件安装路径下的 logs 目录**（与运行日志一致） |

## 3. 架构

```
┌─────────────┐         ┌──────────────────────────────────────┐
│  Codex CLI  │         │            SynaRoute 桌面端             │
│  Claude Code│         │                                        │
│             │  MCP    │  ┌────────────────────────────────┐   │
│  (MCP       │ ──HTTP─>│  │  MCP Server (127.0.0.1:9527)   │   │
│   Client)   │ <──────  │  │  tool: synaroute_ai            │   │
│             │  JSON-  │  └───────────────┬────────────────┘   │
│             │  RPC    │                  │                     │
│  用户在客户端 │         │                  ▼                     │
│  里审批+改文件│         │  ┌────────────────────────────────┐   │
└─────────────┘         │  │  aggregate.rs (复用)           │   │
                        │  │  - cwd 感知 → 文件检索          │   │
                        │  │  - 参与者并行分析（只读）        │   │
                        │  │  - 决策者综合 → 计划/建议        │   │
                        │  └────────────────────────────────┘   │
                        └──────────────────────────────────────┘
```

### 3.1 调用时序

1. 用户在 Codex 项目里说「审查我的鉴权代码」（或任意任务）
2. Codex 的模型决定调用 `synaroute_ai` 工具，带上 `prompt` + `cwd`（当前项目路径）
3. SynaRoute MCP Server 收到请求 → 用 `cwd` 定位项目 → 检索相关文件
4. 参与者（多个模型）**并行**读取项目文件、分析、给建议（只读，不改文件）
5. 决策者综合所有意见 → 输出**修改计划 / 建议文本**
6. MCP Server 以标准 `content[].text`（Markdown）返回给 Codex
7. Codex 展示建议 → 用户确认 → **Codex 用自己的 apply_patch 改文件**（SynaRoute 不参与）

### 3.2 多项目隔离

- 每次工具调用携带自己的 `cwd`，独立成一次聚合任务（`tokio::spawn`）
- **每聚合独立并发池**：项目1 用 N 个参与者、项目2 又用 N 个，互不抢占
- 项目1 和项目2 的参与者、决策者完全隔离，不共享上下文

## 4. 工具定义：`synaroute_ai`

### 4.1 参数 schema

```json
{
  "name": "synaroute_ai",
  "description": "调用 SynaRoute 多模型大脑聚合：多个模型并行分析当前项目代码并给出综合建议/修改计划。适用于代码审查、方案设计、疑难排查等需要多视角的任务。",
  "inputSchema": {
    "type": "object",
    "properties": {
      "prompt":       { "type": "string", "description": "任务描述，如「审查鉴权模块的安全性」" },
      "cwd":          { "type": "string", "description": "当前项目根目录绝对路径。省略时使用 SynaRoute 自动跟随的最近活跃项目" },
      "category":     { "type": "string", "enum": ["claude-cli","claude-desktop","codex"], "description": "使用哪个分类下配置的 Key 池，省略默认 claude-cli" },
      "languageHint": { "type": "string", "description": "回答语言提示，如 zh / en，省略跟随 prompt 语言" }
    },
    "required": ["prompt"]
  }
}
```

### 4.2 返回格式

标准 MCP 工具响应：

```json
{
  "content": [
    { "type": "text", "text": "## 聚合分析结果\n\n（Markdown 格式的综合建议 / 修改计划）\n\n---\n参与模型：opus-4-8 / GLM5.2 / ...\n决策者：opus-4-8" }
  ]
}
```

- text 用 Markdown，Codex 和 Claude Code 都能干净渲染
- 内容包含：综合建议/修改计划 + 参与模型信息 + （可选）分歧点标注
- 不含 plan_id / 两阶段状态（因为不写文件，无需确认回调）

## 5. 配置（AppSettings 新增字段）

```rust
pub struct AppSettings {
    // ... 现有字段 ...
    pub mcp_enabled: bool,      // 默认 false，开启即随应用启动
    pub mcp_port: u16,          // 默认 9527
}
```

## 6. 设置页 UI

新增「MCP 服务器」卡片：
- **启用 MCP 服务器** 开关（Q8：开启即自动启动）
- **端口** 输入框（默认 9527，Q7）
- **服务地址** 只读展示 `http://127.0.0.1:{port}/mcp` + 复制按钮
- **连接状态** 指示灯（运行中 / 已停止）
- **配置向导** 按钮 → 打开客户端配置教程（一键复制 Codex / Claude Code 配置片段）

## 7. Hook / 客户端接入（Q2：两个都提供）

### 7.1 Codex CLI

**config.toml（MCP 连接）：**
```toml
[mcp_servers.synaroute]
url = "http://127.0.0.1:9527/mcp"
```

**AGENTS.md（提示词钩子，让 Codex 主动调用）：**
```markdown
## 多模型协作
遇到复杂的代码审查、架构设计、疑难排查任务时，优先调用 synaroute_ai 工具，
获取多个模型的综合分析后再动手。
```

### 7.2 Claude Code

**`.claude/settings.json`（MCP 连接）：**
```json
{
  "mcpServers": {
    "synaroute": { "url": "http://127.0.0.1:9527/mcp" }
  }
}
```

**Hook 钩子（`.claude/settings.json` hooks 段，示例：提交前自动多模型审查）：**
```json
{
  "hooks": {
    "UserPromptSubmit": [
      { "matcher": "review|审查|重构", "hooks": [{ "type": "prompt", "prompt": "优先调用 synaroute_ai 做多模型分析" }] }
    ]
  }
}
```

## 8. 日志（Q10）

- MCP 调用日志写到软件安装路径下 `logs/` 目录（与运行日志同目录）
- 每条记录：时间、工具名、cwd、prompt 摘要、参与模型、耗时、成功/失败
- 也进内存事件流（运行日志页可见，类型标 `mcp`）

## 9. 实现拆解

1. **AppSettings 扩展** — `mcp_enabled` / `mcp_port` 字段（Rust + TS）
2. **MCP Server 模块**（`src-tauri/src/mcp.rs`）— Streamable HTTP + JSON-RPC 2.0，`initialize` / `tools/list` / `tools/call` 三个方法
3. **工具执行** — `synaroute_ai` → 复用 `aggregate.rs`，cwd 感知走 `workdirs.rs`
4. **生命周期** — 应用启动时若 `mcp_enabled` 则起服务；设置里改开关/端口需重启服务
5. **前端设置页** — MCP 卡片 + 配置向导弹窗
6. **文档** — `docs/06-MCP使用手册.md`（面向最终用户的配置和使用教程）
7. **i18n** — 中英文案

## 10. 不在本次范围

- MCP 工具的两阶段确认（因为不写文件，无需要）
- 远程 MCP（仅本机 127.0.0.1）
- MCP 鉴权（Q9）
- 流式返回（首版一次性返回聚合结果，后续可加 SSE 进度）
