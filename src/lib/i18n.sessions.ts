// Codex 会话管理页的本地化词条。
//
// 从 i18n.ts 拆出来的：那个文件冻结在棘轮上、余量为 0。拆的粒度按**页面**（同 i18n.usage.ts
// 的理由）—— 改一个页面的文案时要同时看 zh/en 两份，同页放在一起最省事。
//
// ⚠️ **新分片必须同时加进 tests/../i18n.test.ts 的 `SOURCES` 与 `CHUNKS`**。那里有一条
// `CHUNKS.length === SOURCES.length - 1` 专门钉这件事 —— 历史上 usage.* 那 32 条被拆出去后
// 测试仍只读 i18n.ts，照样全绿而那些词条已经不在保护范围内了。
//
// ⚠️ **词条值里不许写 Markdown**：应用侧是纯文本渲染（全仓无 markdown 渲染器），
// `**强调**` 会把星号原样显示给用户。i18n.test.ts 有一条判据钉着这件事。

type Dict = Record<string, string>;

export const sessionsZh: Dict = {
  "nav.sessions": "Codex 会话",
  "sessions.title": "Codex 会话管理",
  "sessions.subtitle":
    "本地历史对话。每条会话自带一个 provider —— 与当前生效的不一致时，打开它会走错上游",
  "sessions.refresh": "刷新",
  "sessions.loading": "加载中…",
  "sessions.empty": "没有找到本地会话（Codex 还没产生过对话，或 CODEX_HOME 指向了别处）",
  "sessions.colTime": "时间",
  "sessions.colTitle": "对话",
  "sessions.colProvider": "会话记的 provider",
  "sessions.colModel": "模型",
  "sessions.colCwd": "工作目录",
  "sessions.project": "项目：{name}",
  "sessions.noProject": "未归入项目（Codex 的项目侧栏里看不到它）",
  "sessions.colSize": "大小",
  "sessions.colOps": "操作",
  "sessions.current": "当前生效：",
  "sessions.mismatch": "指向别处",
  "sessions.archived": "已归档",
  "sessions.forked": "分支",
  "sessions.derived": "Codex 内部派生",
  "sessions.noTitle": "（读不出首条消息）",
  "sessions.export": "导出 Markdown",
  "sessions.exported": "已导出到",
  "sessions.exportFailed": "导出失败",
  "sessions.delete": "删除",
  "sessions.deleteSelected": "删除选中",
  "sessions.selected": "已选 {n} 条",
  "sessions.confirmTitle": "确认删除会话？",
  "sessions.confirmBody":
    "将删除 {n} 条会话的 rollout 文件、列表索引与数据库记录。删除前会先把 rollout 原文备份到应用数据目录的 backups/codex-sessions-deleted/（保留 30 天）；备份失败的那一条不会被删。",
  "sessions.confirmOk": "确认删除",
  "sessions.cancel": "取消",
  "sessions.mismatchHint":
    "有 {n} 条会话指向别的 provider。它们在 Codex 里打开会走那个上游 —— 若那是官方 openai，就会用占位凭据换来 401。用下面的「立刻同步」把它们指向 SynaRoute 即可。",
  "sessions.unreadable": "{n} 个文件的首行无法解析（Codex 可能改过 rollout 格式），已跳过",
  "sessions.pathRejected": "{n} 个文件的路径无法安全定位，已跳过",
  "sessions.loadFailed": "读取会话列表失败",

  "sessions.statTotal": "会话总数",
  "sessions.statActive": "未归档",
  "sessions.statArchived": "已归档",
  "sessions.statMismatched": "指向别处",
  "sessions.statDb": "会话库",
  "sessions.modelGone": "模型已失效",
  "sessions.modelGoneHint":
    "有 {n} 条对话用的模型现在没有任何启用的 Key 能服务（Key 被删了，或模型映射改过）。它们的 provider 是对的，但打开后会被降级到兜底模型、或直接报错 —— 这一位只有代理侧看得到。",
  "sessions.filter": "筛选",
  "sessions.filterPlaceholder": "按标题 / 目录 / provider / 模型筛选",
  "sessions.onlyMismatched": "只看指向别处",
  "sessions.onlyModelGone": "只看模型已失效",
  "sessions.filtered": "筛出 {n} / {total} 条",
  "sessions.pickedHidden": "其中 {n} 条已被当前筛选隐藏，但仍会被删除",
  "sessions.syncConfirmTitle": "确认同步历史会话？",
  "sessions.syncConfirmBody":
    "将把 {n} 条会话记的 provider 改成 {target}（当前列表里与它不一致的那些）。只改每个 rollout 首行的这一个字段，对话正文与文件修改时间都不动；原值会记进回滚清单，点「停止」时按它逐条改回。",
  "sessions.syncConfirmNone":
    "当前列表里的 {total} 条会话都已经指向 {target}，无需改动。",
  "sessions.syncConfirmOk": "确认同步",
  "sessions.dbNone": "未找到（不影响路由，只影响列表里的标题与模型）",
  "sessions.inDbHint":
    "会话库里有 {n} 条记录，磁盘上有 {total} 个会话文件。差额是库里没有记录的那些（fork 出来的分支就属于这一类）—— 它们同样带 provider，所以我们按文件而不是按库列表。",

  "sessions.targetLabel": "同步目标",
  "sessions.targetOurs": "推荐",
  "sessions.targetHint":
    "只改历史会话记的 provider，不动 config.toml —— 在这里选 openai 不等于切回官方登录（那要点「还原」）。",
  "sessions.srcConfig": "配置",
  "sessions.srcRollout": "会话",
  "sessions.srcSqlite": "索引",
  "sessions.syncNow": "立刻同步",
  "sessions.syncing": "同步中…",
  "sessions.syncHint":
    "接入时已经自动同步过一次。这个按钮用于补做当时没成功的那些 —— 最常见的成因是接入那一刻 Codex 正开着、文件被独占。",
  "sessions.autoSync": "接入时自动同步历史会话",
  "sessions.autoSyncHint":
    "这是 SynaRoute 唯一会主动改你对话文件的自动动作（只改首行那一个字段，正文与文件修改时间都不动）。同时在用 cc-switch 之类工具时可以关掉它，关掉后仍可用上面的按钮手动同步。",
  "sessions.autoSyncSaved": "已保存",

  "sessions.indexTitle": "会话索引",
  "sessions.indexClean": "session_index.jsonl 里没有失效条目（共 {n} 行）",
  "sessions.indexOrphans":
    "session_index.jsonl 里有 {n} 条指向已不存在会话的记录。它们在 Codex 的会话列表里是点开即报错的死条目，通常来自在别处手删过 rollout 文件。",
  "sessions.indexPrune": "清理失效条目",
  "sessions.indexBackupNote": "清理前会把整份索引备份到应用数据目录。",
  // 客户端环境变量冲突（分类页那条常驻告警）。刻意与会话页放同一分片：它们都是
  // 「接入完成但不生效」这一族的排障入口，改文案时会一起看。
  "env.title": "检测到 {n} 个可能顶掉 SynaRoute 配置的环境变量",
  "env.valueHidden": "（已设置，值不外发）",
  "env.srcUser": "Windows 用户级",
  "env.srcProcess": "本进程",
  "env.remove": "移除这 {n} 个",
  "env.confirmBody":
    "这会改你的 Windows 用户级环境变量（不只是 SynaRoute 的配置）。移除前会先把当前清单备份成 JSON；已经在运行的程序仍持有旧值，要重启它们才生效。",
  "env.confirmOk": "确认移除",
  "env.removed": "已移除 {n} 个。",
  "env.backupAt": "备份：{path}",
  "env.keepHint":
    "目录类变量（CODEX_HOME 等）不提供移除 —— 正确的处置是让两侧一致：重启 SynaRoute 让它继承到，或在系统设置里把它去掉。",
};

export const sessionsEn: Dict = {
  "nav.sessions": "Codex sessions",
  "sessions.title": "Codex session management",
  "sessions.subtitle":
    "Local conversation history. Each session carries its own provider — when it differs from the active one, opening it routes to the wrong upstream",
  "sessions.refresh": "Refresh",
  "sessions.loading": "Loading…",
  "sessions.empty":
    "No local sessions found (Codex hasn't created any, or CODEX_HOME points elsewhere)",
  "sessions.colTime": "Time",
  "sessions.colTitle": "Conversation",
  "sessions.colProvider": "Provider in session",
  "sessions.colModel": "Model",
  "sessions.colCwd": "Working directory",
  "sessions.project": "Project: {name}",
  "sessions.noProject": "No project (not shown in Codex's project sidebar)",
  "sessions.colSize": "Size",
  "sessions.colOps": "Actions",
  "sessions.current": "Active:",
  "sessions.mismatch": "points elsewhere",
  "sessions.archived": "Archived",
  "sessions.forked": "Branch",
  "sessions.derived": "Codex-internal",
  "sessions.noTitle": "(first message unreadable)",
  "sessions.export": "Export Markdown",
  "sessions.exported": "Exported to",
  "sessions.exportFailed": "Export failed",
  "sessions.delete": "Delete",
  "sessions.deleteSelected": "Delete selected",
  "sessions.selected": "{n} selected",
  "sessions.confirmTitle": "Delete these sessions?",
  "sessions.confirmBody":
    "This deletes the rollout files, the list index and the database rows for {n} session(s). Each rollout is first copied to backups/codex-sessions-deleted/ in the app data directory (kept 30 days); any session whose backup fails is not deleted.",
  "sessions.confirmOk": "Delete",
  "sessions.cancel": "Cancel",
  "sessions.mismatchHint":
    "{n} session(s) point at a different provider. Opening them in Codex routes to that upstream — if it is the official openai, the placeholder credential comes back as a 401. Use \"Sync now\" below to point them at SynaRoute.",
  "sessions.unreadable":
    "{n} file(s) had an unparsable first line (Codex may have changed the rollout format); skipped",
  "sessions.pathRejected": "{n} file(s) could not be safely located; skipped",
  "sessions.loadFailed": "Failed to read the session list",

  "sessions.statTotal": "Total",
  "sessions.statActive": "Active",
  "sessions.statArchived": "Archived",
  "sessions.statMismatched": "Points elsewhere",
  "sessions.statDb": "Session database",
  "sessions.modelGone": "model gone",
  "sessions.modelGoneHint":
    "{n} conversation(s) use a model no enabled key can serve any more (the key was deleted, or a model mapping changed). Their provider is correct, but opening them falls back to a default model or fails outright — only the proxy side can see this.",
  "sessions.filter": "Filter",
  "sessions.filterPlaceholder": "Filter by title / directory / provider / model",
  "sessions.onlyMismatched": "Only those pointing elsewhere",
  "sessions.onlyModelGone": "Only those with a gone model",
  "sessions.filtered": "{n} of {total} shown",
  "sessions.pickedHidden": "{n} of them are hidden by the current filter but will still be deleted",
  "sessions.syncConfirmTitle": "Sync past sessions?",
  "sessions.syncConfirmBody":
    "This rewrites the provider recorded in {n} session(s) to {target} (those in the current list that differ from it). Only that one field on each rollout's first line changes; the conversation body and the file's modification time stay untouched. The original values go into the rollback manifest and are restored one by one when you press Stop.",
  "sessions.syncConfirmNone":
    "All {total} session(s) in the current list already point at {target}; nothing to change.",
  "sessions.syncConfirmOk": "Sync",
  "sessions.dbNone": "Not found (routing is unaffected; only titles and models in this list are)",
  "sessions.inDbHint":
    "The session database has {n} row(s) while {total} session file(s) exist on disk. The difference is sessions the database has no row for (forked branches are one such case) — they carry a provider too, which is why we list files rather than database rows.",

  "sessions.targetLabel": "Sync target",
  "sessions.targetOurs": "recommended",
  "sessions.targetHint":
    "This only rewrites the provider recorded in past sessions; config.toml is untouched — picking openai here is not the same as switching back to the official login (that's what Restore does).",
  "sessions.srcConfig": "config",
  "sessions.srcRollout": "sessions",
  "sessions.srcSqlite": "index",
  "sessions.syncNow": "Sync now",
  "sessions.syncing": "Syncing…",
  "sessions.syncHint":
    "Applying already synced once. This button is for finishing whatever failed then — most often because Codex was running and held the files open.",
  "sessions.autoSync": "Sync past sessions when applying",
  "sessions.autoSyncHint":
    "This is the only automatic action in which SynaRoute modifies your conversation files (only that one field on the first line; the body and the file's modification time stay untouched). Turn it off if you also use something like cc-switch — the manual button above still works.",
  "sessions.autoSyncSaved": "Saved",

  "sessions.indexTitle": "Session index",
  "sessions.indexClean": "No stale rows in session_index.jsonl ({n} row(s) total)",
  "sessions.indexOrphans":
    "session_index.jsonl has {n} row(s) pointing at sessions that no longer exist. In Codex's session list those are dead entries that error out when opened, usually left behind by deleting rollout files elsewhere.",
  "sessions.indexPrune": "Remove stale rows",
  "sessions.indexBackupNote": "The whole index is backed up to the app data directory first.",
  "env.title": "{n} environment variable(s) may override SynaRoute's configuration",
  "env.valueHidden": " (set; value not shown)",
  "env.srcUser": "Windows user level",
  "env.srcProcess": "this process",
  "env.remove": "Remove {n}",
  "env.confirmBody":
    "This changes your Windows user-level environment variables, not just SynaRoute's own configuration. The current list is backed up to JSON first; programs already running keep the old values until you restart them.",
  "env.confirmOk": "Remove",
  "env.removed": "Removed {n}.",
  "env.backupAt": "Backup: {path}",
  "env.keepHint":
    "Directory variables (CODEX_HOME and friends) are not offered for removal — the fix is to make both sides agree: restart SynaRoute so it inherits the value, or unset it in system settings.",
};
