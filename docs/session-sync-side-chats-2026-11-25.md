# 会话同步中的侧边聊天重复（2026-11-25）

Codex 的侧边聊天可以仍标记为 `thread_source = user`。仅过滤子代理来源，不能阻止同步程序把它补进 Desktop 主列表。截图中的两条具体会话未在当前本机数据库找到，但已用同名主会话、侧边聊天及普通分支的夹具复现：旧实现补入三条，正确结果应为两条独立会话。

官方实现依据：openai/codex PR #56351，提交 `30587d78850abf2bfc484ffb0ab0f71497cc9c05`。`SessionMeta.attached_thread = true` 与 `forked_from_id` 共同表示附属关系；SQLite 对应 `threads.attached_thread_parent_id`。普通 `forked_from_id` 本身不表示侧边聊天。

修复在扫描首行时保留附属父 ID，并在索引计划中读取可选数据库列。已观察到的 rollout 元数据优先于可能陈旧的数据库副本。没有新列的数据库仍能通过 rollout 正确识别；解除附属关系的普通聊天可以重新成为独立条目。

同步继续更新真实会话的 provider，但不再将侧边聊天插入本地主列表。已有误补条目只按明确附属关系从 local host 的 catalog 清理，写前备份及 catalog revision 事务规则沿用原流程。实际 threads、rollout、正文、父关系及 remote host 索引不删除；标题不参与删除或合并判定。

回归覆盖新旧数据库、首次补索引、已污染索引、provider 已正确时的清理、重复执行、普通同名分支、解除附属关系、无效父 ID 和远端条目保护。会话模块 154 项测试通过，3 项既有依赖真实环境的测试仍 ignored；Rust clippy 全目标零警告通过。

升级后需先退出 Codex，再在 SynaRoute 执行一次会话同步，才能清理该机器上已误补且有明确附属证据的条目。本次开发未直接改动用户的真实会话库。
