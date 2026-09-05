// Codex 会话管理页的本地化词条。
//
// 从 i18n.ts 拆出来的：那个文件冻结在棘轮上、余量为 0。拆的粒度按**页面**（同 i18n.usage.ts
// 的理由）—— 改一个页面的文案时要同时看 zh/en 两份，同页放在一起最省事。
//
// ⚠️ **新分片必须同时加进 tests/../i18n.test.ts 的 `SOURCES` 与 `CHUNKS`**。那里有一条
// `CHUNKS.length === SOURCES.length - 1` 专门钉这件事 —— 历史上 usage.* 那 32 条被拆出去后
// 测试仍只读 i18n.ts，照样全绿而那些词条已经不在保护范围内了。

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
  "sessions.colProvider": "会话记的 provider",
  "sessions.colCwd": "工作目录",
  "sessions.colSize": "大小",
  "sessions.colOps": "操作",
  "sessions.current": "当前生效：",
  "sessions.mismatch": "指向别处",
  "sessions.archived": "已归档",
  "sessions.export": "导出 Markdown",
  "sessions.exported": "已导出到",
  "sessions.exportFailed": "导出失败",
  "sessions.delete": "删除",
  "sessions.deleteSelected": "删除选中",
  "sessions.selected": "已选 {n} 条",
  "sessions.confirmTitle": "确认删除会话？",
  "sessions.confirmBody":
    "将删除 {n} 条会话的 rollout 文件、列表索引与数据库记录。此操作不可逆 —— SynaRoute 不会为它们留备份。",
  "sessions.confirmOk": "确认删除",
  "sessions.cancel": "取消",
  "sessions.mismatchHint":
    "有 {n} 条会话指向别的 provider。它们在 Codex 里打开会走那个上游 —— 若那是官方 openai，就会用占位凭据换来 401。点「启动」重新接入一次即可把它们指向 SynaRoute。",
  "sessions.unreadable": "{n} 个文件的首行无法解析（Codex 可能改过 rollout 格式），已跳过",
  "sessions.pathRejected": "{n} 个文件的路径无法安全定位，已跳过",
  "sessions.loadFailed": "读取会话列表失败",
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
  "sessions.colProvider": "Provider in session",
  "sessions.colCwd": "Working directory",
  "sessions.colSize": "Size",
  "sessions.colOps": "Actions",
  "sessions.current": "Active:",
  "sessions.mismatch": "points elsewhere",
  "sessions.archived": "Archived",
  "sessions.export": "Export Markdown",
  "sessions.exported": "Exported to",
  "sessions.exportFailed": "Export failed",
  "sessions.delete": "Delete",
  "sessions.deleteSelected": "Delete selected",
  "sessions.selected": "{n} selected",
  "sessions.confirmTitle": "Delete these sessions?",
  "sessions.confirmBody":
    "This deletes the rollout files, the list index and the database rows for {n} session(s). This cannot be undone — SynaRoute keeps no backup of them.",
  "sessions.confirmOk": "Delete",
  "sessions.cancel": "Cancel",
  "sessions.mismatchHint":
    "{n} session(s) point at a different provider. Opening them in Codex routes to that upstream — if it is the official openai, the placeholder credential comes back as a 401. Click Start to re-apply and point them at SynaRoute.",
  "sessions.unreadable":
    "{n} file(s) had an unparsable first line (Codex may have changed the rollout format); skipped",
  "sessions.pathRejected": "{n} file(s) could not be safely located; skipped",
  "sessions.loadFailed": "Failed to read the session list",
};
