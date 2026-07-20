# 实现方案：MCP 端口稳定 + 自动注册 + 日志可展开

## 背景与根因

1. **端口占用后要重配**：[mcp.rs:74](src-tauri/src/mcp.rs) 端口被占会 `port..+20` 向上找，但换端口后客户端里 `claude mcp add` 写死的旧地址失效，需手动重配。
2. **开大脑聚合不自动配 MCP**：代码里根本没有写 MCP 配置的功能，向导只"展示"手动命令。
3. **MCP/长日志看不了**：[LogsPage.tsx:96](src/pages/LogsPage.tsx) `detail` 单行 `truncate`，只有 `request` 类型能展开；`mcp`/`failover` 等长内容无法展开。

## 用户已确认的决策

- 端口：**固定端口 + 自动重写客户端配置**；绑不上时回退找空闲端口并重写配置。
- 自动注册：**只注册当前分类对应的工具**；**启用 MCP 开关时**触发。
- 写入方式：**后端直接改配置文件**（不调 CLI），带备份 + 原子写。
- 日志：**所有类型都可展开**；**MCP 记完整 trace**。

## 改动清单

### A. 后端：MCP 客户端配置自动写入（新增 `mcp_config` 模块或并入 tools.rs）

- 新增 `register_mcp_client(category, mcp_url)`：
  - **Claude CLI / Claude 桌面端** → 写 `~/.claude.json` 的 `mcpServers.synaroute = {type:"http", url:"http://127.0.0.1:{port}/mcp"}`
  - **Codex** → 写 `~/.codex/config.toml` 的 `[mcp_servers.synaroute]`（http transport）
  - 复用 tools.rs 的 `read_json_or_empty` / `backup_and_write_json` / 原子写；TOML 走 `apply_codex` 同款读改写。
- 新增 `unregister_mcp_client()`：关闭开关时移除 `synaroute` 项（可选，见触发）。
- 幂等：同 url 已存在则跳过写盘（避免无谓备份）。

### B. 后端：端口变化时重写已注册客户端

- `McpManager.start()` 里，`bound` 端口确定后，若 `bound != 上次写入端口`，对**已注册过的分类**重写其客户端配置为新 url。
- 记住"已注册分类集合"：存到 `AppSettings`（新增 `mcp_registered_categories: Vec<String>` 或复用现有结构）。

### C. 后端：启用 MCP 开关时自动注册

- [lib.rs:233](src-tauri/src/lib.rs) `save_settings` 里 mcp_enabled 变 true 时：start 成功后，对**当前活跃分类**调 `register_mcp_client`。
- 关闭开关：stop + （可选）unregister。

### D. 后端：MCP 调用记完整 trace

- [mcp.rs:344](src-tauri/src/mcp.rs) `append_event` 改为 `append_event_trace`，带 `RequestTrace`（prompt 摘要、work_dir、参与者、耗时、完整分析结果）。
- 复用 proxy.rs 已有的 `RequestTrace` 结构（可能需加 mcp 适配字段）。

### E. 前端：所有日志类型可展开

- [LogsPage.tsx:74](src/pages/LogsPage.tsx) `expandable` 条件从 `type==="request" && trace` 放宽到 `!!entry.trace`。
- 无 trace 的长 detail：点击展开显示完整 detail 全文（不 truncate）。
- `mcp` 类型加进 `TYPE_META`（当前缺失，会导致 `TYPE_META[entry.type]` 为 undefined 崩溃风险）。

### F. 前端：设置页提示自动注册状态

- MCP 区块显示"已自动注册到 {工具}"，替代原来纯手动向导（向导保留作为兜底/其它客户端）。

## 验证

- `cargo test --lib`（新增：register 写入幂等、端口变化重写、TOML/JSON 结构正确）
- 前端 `tsc --noEmit`
- 浏览器预览验证日志可展开（mock）
- 打包 → 静默安装 F:\SynaRoute → 真实验证：开 MCP 开关后 `~/.claude.json` 出现 synaroute 项、`claude mcp list` 显示 connected

## 交付

打包 + 复制/安装到 F:\SynaRoute（按既定规则自动执行）。
