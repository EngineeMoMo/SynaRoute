# 会话同步与工具 ID 修复

手动「立刻同步」在切换 provider 前检查用户会话的历史工具 ID。即使会话已经指向所选 provider，也可以再次同步来修复旧历史。请先退出 Codex，修复后重新打开会话，避免内存中的历史覆盖磁盘修复。

## 边界

- `model_provider` 是同步目标，不是工具的 `namespace`。
- 只规范化已有完整工具调用条目的异常 `id`：`function_call` 使用 `fc_`，`custom_tool_call` 使用 `ctc_`。同文件内的对应条目引用同步更新。
- 保留 `call_id`、工具结果关联、`namespace`、`name`、参数、结果和 reasoning 内容。不对文本做全局替换。
- 同时检查普通 `response_item` 与 `compacted.payload.replacement_history`，包括已归档的用户会话；内部派生会话沿用 provider 同步的排除规则。
- 合法 ID 保持原样。OpenAI → 其他 provider → OpenAI 不会反向改回坏 ID；重复同步不重复备份或改写已修好的正文。
- 缺失调用关联、映射冲突、无法解析、超出 128 MiB 或文件占用等情况会中止此次手动 provider 切换，并返回原因。此前已完成的文件修复不自动撤销，错误消息会列出已修复数量及备份目录。
- 自动接入同步和停止时的 provider 还原不执行正文修复。

## 备份与回滚

仅对需要修改的文件创建逐字节原文备份，位置是应用数据目录下的 `codex-session-id-backups/<批次 UUID>/<原相对路径>`。结果提示显示实际目录。备份成功后才写临时文件并替换原文件，保留修改时间。

provider 回滚清单与这份正文备份独立。停止代理只按原有逻辑还原 provider，不重新引入坏 ID。如必须撤销正文修复，应退出 Codex，先另行保存当前会话文件，再从对应批次备份恢复；整文件恢复会丢失备份后新增的对话，也会恢复备份时的 provider，不能无条件覆盖。

## 新记录

Chat/Anthropic 转 Responses 的流式工具调用使用独立条目 ID；上游调用关联 ID 不再充当条目 ID。非流式 custom 工具同样使用 `ctc_`。工具命名空间的既有拆分、拼接逻辑不变。

此修复针对 ID 格式错误，不保证服务器端条目引用、加密 reasoning、模型能力或其他跨上游差异均兼容。测试使用本地夹具，不会调用 OpenAI，也不会改写用户真实会话。
