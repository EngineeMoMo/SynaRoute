# Codex 会话管理改进（2026-10-30）

本轮读取的 Codex++ 仓库为 `BigPizzaV3/CodexPlusPlus`，`main` 提交为 `1949f93e23e91ad9170ea3ee0d38006a89e9be2e`（2026-10-30）。对照范围为 `ProjectSessionPage.tsx`、`management-model.ts`、管理器的 `commands.rs` 会话命令，以及 `docs/session-paging.md`。实现按 SynaRoute 现有 React/Rust 结构独立编写。

| 对照项 | 改动前 | 本轮改动 |
|---|---|---|
| 页面主体 | provider 同步和修复区域在前，会话列表靠后 | 修复区默认折叠，保留顶部统计和会话浏览区 |
| 浏览方式 | 单张完整列表 | 按工作目录分组或按最近活动日期分组，各组可折叠 |
| 搜索和筛选 | 搜索标题、目录、provider、模型；两个异常筛选 | 增加会话 ID、项目、多关键词搜索，以及归档状态、来源筛选 |
| 顺序 | 按 rollout 创建时间 | 默认按数据库最近活动时间；缺失时回落文件修改时间，也可按创建时间或标题排序 |
| 分页 | 全量渲染 | 每页 25 / 50 / 100 条，搜索和分组在完整快照上进行 |
| 选择 | 逐行选择 | 全选当前页、选择一组全部筛选结果、跨页保留、提示隐藏的已选项 |
| 会话操作 | 批量删除、逐条导出 | 增加单条删除、单条及批量归档和取消归档、详情、复制 ID 与 CLI 继续会话命令 |
| 归档状态 | 只按 `archived_sessions` 目录判断 | 数据库 `threads.archived` 优先，缺少该字段才沿用目录判断 |
| 数据库发现 | `sqlite/` 内的数据库与固定 `state_5.sqlite` | 同时识别 `state_N.sqlite`，按版本数字从新到旧补充展示信息 |
| 加载 | 异步命令直接进行阻塞文件扫描，三个接口任意一个失败使整页失败 | 列表扫描放入阻塞线程；列表、目标、审计分别处理结果 |

默认展示未归档的用户对话；明确的内部派生会话可以切换筛选查看，普通 fork 和来源未知的会话保留。文件仍然是列表的基础，数据库缺少的分支不会因此消失。标题缓存现在包含 Codex 根目录，避免切换 `CODEX_HOME` 时复用另一目录的同路径标题。

### 同步后继续历史会话

列表及「指向别处」统计只读各文件的首条 `session_meta`。一份 rollout 可以有多条 `session_meta`（见 [CodexPlusPlus 的回归用例](https://github.com/BigPizzaV3/CodexPlusPlus/blob/e012af63ac4c0d4fa292a8bcde5ff3ed5e8f5293/crates/codex-plus-data/tests/all/provider_sync.rs#L1489)），还可以有 `event_msg → thread_settings_applied → thread_settings.model_provider_id`。v0.1.104 只检查了前者。此前在自定义 provider 上测通的是**本地摘要及下一轮消息**，不是截图中的远端压缩；两者都可能请求 `/v1/responses`。

用真实 Codex CLI `0.171.0-alpha.13.2`、假凭据和两个不同的回环端点验证了三个条件：

| 磁盘上的 provider | 实际结果 |
|---|---|
| 全部元数据 `synaroute`，恢复用的线程设置为 `openai` | `thread/resume` 拒绝切换 provider；未发送 HTTP 请求。这是同步漏项，但本身不是 401。 |
| 元数据 `openai`，无覆盖它的线程设置，根配置 `synaroute` | 恢复选中 `openai`。普通消息先成功；超过测试上下文阈值后，带 `compaction_trigger` 的请求到达模拟官方端点，返回与截图同类的 `Error running remote compact task … 401`。 |
| 元数据和线程设置均为 `synaroute` | 恢复成功；在测试中为代理显式声明 V2 压缩能力后，同样的自动压缩只到达模拟代理且完成。这个能力声明是为了验证远端分支，不是给用户的配置建议。 |

同步按 [Codex 的模型恢复规则](https://github.com/openai/codex/blob/ad9d659a8491fc5169a00dcf7b7e6ca8a87adf69/codex-rs/app-server/src/request_processors/persisted_resume_settings.rs#L116) 区分本线程、其他线程和未标归属的旧设置；不改 fork 复制来的其他线程设置。只改匹配记录里的 provider，正文及未修改行逐字节保留。元数据与设置分开记原值：直接还原旧清单不碰它未管理的线程设置。没有显式 provider 覆盖时，[Codex 也会从已存在的 `threads.model_provider` 恢复](https://github.com/openai/codex/blob/ad9d659a8491fc5169a00dcf7b7e6ca8a87adf69/codex-rs/app-server/src/request_processors/thread_processor.rs#L285)；读取会话库失败不能算作「已经一致」。明确内部来源的 Guardian / 子代理仍跳过。同步核对的是磁盘，不会更换密钥或接管已打开的线程，也不验证真实上游。

**已有 v0.1.104 不必重复同步即可自查：** 在报错电脑上从「会话管理 → 找到该对话 → 详情 → 复制会话 ID」获取 ID，在放有诊断脚本的目录打开 PowerShell：

```powershell
.\check-codex-session-route.ps1 -ThreadId (Read-Host 'Thread ID')
```

脚本在仓库 `scripts/` 下，只读该 ID 的 rollout 和索引记录；不读取 `auth.json`、不联网、不输出正文、目录或自定义 provider 名。非默认数据目录可用 `-CodexHome`、`-SqliteHome` 指定。`MetadataProviders` 列出各条会话元数据使用的 provider；`ResumeSettingsProvider` 是最近可用于恢复的线程设置，`(none)` 表示无设置，不等于 `openai`。`ThreadIndexProviders` 通过 Windows 自带的 SQLite 以只读模式查询该 ID；`(no row)` 表示没有索引行，`(unavailable)`、`(unreadable)` 或非零的 `UnparsedCandidates` 表示诊断不完整。三处都没指向官方却仍报官方 401 时，应核对报错窗口的会话 ID、数据目录以及出错时间的代理请求记录，而不是再次改写历史文件。

归档只修改 `threads` 的归档字段，不移动 rollout，也不改正文、provider、标题或最近活动时间。`archived_at` 使用秒，`archived_at_ms` 使用毫秒，取消归档时置空。修改前使用已有 SQLite Online Backup 保存包含已提交 WAL 页的一致快照，数据库更新使用事务。无法备份时不修改；没有数据库记录的会话返回逐项失败。操作入口要求先退出 Codex/ChatGPT，并共用已有的维护锁。备份目录为应用数据目录下的 `backups/codex-sqlite/`。

本轮分页限制的是页面渲染数量，首次加载仍扫描完整本地 rollout 快照。Codex++ 的数据库分页缓存尚未引入；SynaRoute 继续同时展示数据库里没有记录的文件，因此不能直接替换成数据库分页。详情提供元数据与 CLI 命令，未增加 Desktop 私有协议跳转或对话正文预览。

用户要求由本人测试，收到该要求后未继续运行验证。此前完成的会话相关 Rust 回归为 139 通过、2 项需真实客户端的测试按原配置忽略；前端相关测试 26 项通过。实际浏览器界面验证未完成：computer-use 工具因无法可靠识别当前浏览器地址停止了自动化。真机验收统一记在 `docs/14-交接与待办清单.md` §21.2 C-56。
