# 第三方组件声明

本产物内含以下第三方材料。

## OpenAI Codex —— 模型系统提示词

- **文件**：[`src-tauri/src/tools/codex_base_instructions.md`](src-tauri/src/tools/codex_base_instructions.md)
- **来源**：<https://github.com/openai/codex> 的 `codex-rs/models-manager/prompt.md`
- **取用时间 / 大小**：2026-08-28，20903 字节，逐字节未改
- **许可**：Apache License 2.0（<https://www.apache.org/licenses/LICENSE-2.0>）
- **原始声明**（openai/codex 的 `NOTICE`）：

  ```
  OpenAI Codex
  Copyright 2025 OpenAI

  This project includes code derived from [Ratatui](https://github.com/ratatui/ratatui),
  licensed under the MIT license.
  Copyright (c) 2016-2022 Florian Dehau
  Copyright (c) 2023-2025 The Ratatui Developers
  ```

### 为什么必须逐字带上它，而不是自己写一份

Codex 的模型目录（`model_catalog_json`）要求每个模型条目**至少有** `base_instructions` 或
`model_messages.instructions_template` 之一，缺了则**启动即报错**。而在没有目录的今天，
Codex 对未知模型（也就是走 SynaRoute 路由的每一个非 GPT 模型）用的正是这份 `prompt.md`
—— 见 `models-manager/src/model_info.rs` 的 `model_info_from_slug`：
`instructions_template: Some(BASE_INSTRUCTIONS.to_string())`，而
`BASE_INSTRUCTIONS = include_str!("../prompt.md")`。

它的 `# Tool Guidelines` / `## Shell commands` / `## update_plan` 三节是工具使用契约。
换成一份自写的短提示词，等于**静默削弱现有的工具调用能力** —— 而那恰恰是本功能
必须保住的东西。逐字带上是唯一零退化的做法。

它也是**模型中性**的：全文无 `GPT` 字样（那句 "a coding agent based on GPT-5" 在
`DEFAULT_PERSONALITY_HEADER` 常量里，不在这份文件里），故用于 claude / glm / deepseek
不会出现自相矛盾的自我描述。

### 已知代价

这份副本**冻结在取用时的版本**。Codex 日后修改 `prompt.md`，我们不会自动跟随。
影响面是「行为准则渐进落后」，不是断崖式失效 —— 工具的**格式契约**由 Codex 按
`tool_mode` 在运行时自己生成，不在这份文件里。升级 Codex 支持基线时应一并重新取用。
