// 大脑聚合「运行面板」的本地化词条（Phase1 计划 → Phase2a 预览 → Phase2b 落盘）。
//
// 从 i18n.ts 拆出来的（那边冻结在棘轮上、余量为 0）。粒度按**界面区块**分，
// 与 i18n.fields.ts / i18n.mapping.ts 同一口径。
//
// ⚠️ zh 与 en 的 key 集合必须完全一致，且**本文件必须在 src/lib/i18n.test.ts 的 SOURCES
// 与 CHUNKS 两张表里各有一条** —— 否则这一整页词条会静默脱离对称性保护。

type Dict = Record<string, string>;

export const brainRunZh: Dict = {
  "brain.runTitle": "运行聚合",
  "brain.runPlaceholder": "输入需求…如：给登录页添加记住密码功能",
  "brain.runStart": "开始运行",
  "brain.runThinking": "参与者思考中…",
  "brain.runConfirm": "确认执行",
  "brain.runReset": "新一轮",
  "brain.runPlanTitle": "决策者计划",
  "brain.runResultTitle": "执行结果",
  // Phase2a：决策者出完整文件内容，此时一个字节都还没写。
  "brain.runPreviewing": "生成文件内容中…",
  "brain.runPreviewTitle": "将写入的文件",
  "brain.runPreviewHint":
    "以下是决策者生成的完整文件内容将要落到的位置。现在磁盘上什么都还没变 —— 点下面的按钮才会真正写入。",
  "brain.runWrite": "写入这 {n} 个文件",
  "brain.runWriting": "写入中…",
  "brain.runBackupNote": "覆盖已有文件前会把原文备份到同目录的 .synaroute.bak，写入是原子的（先写临时文件再替换）。",
  "brain.runNoChanges":
    "决策者没有输出任何可写入的文件块 —— 它给的多半是说明文字而不是完整文件。可以直接看下面的原文。",
  "brain.runNoWorkDir": "本轮没有工作目录，不会写入任何文件。下面是决策者的完整输出，可自行取用。",
  "brain.runDeciderOutput": "决策者原文",
  // 逐条预览的标记
  "brain.runOverwrite": "覆盖",
  "brain.runCreate": "新建",
  "brain.runRejectedTag": "已拒绝",
  "brain.runRejectedCount": "{n} 项被安全防线拒绝，不会写入。",
};

export const brainRunEn: Dict = {
  "brain.runTitle": "Run Aggregation",
  "brain.runPlaceholder": "Describe what you'd like to change, e.g. \"Add a dark mode toggle to Settings\"",
  "brain.runStart": "Run",
  "brain.runThinking": "Members thinking…",
  "brain.runConfirm": "Confirm & Execute",
  "brain.runReset": "New round",
  "brain.runPlanTitle": "Decider Plan",
  "brain.runResultTitle": "Result",
  "brain.runPreviewing": "Generating file contents…",
  "brain.runPreviewTitle": "Files to be written",
  "brain.runPreviewHint":
    "Below is where the decider's generated file contents would land. Nothing on disk has changed yet — the button below is what actually writes.",
  "brain.runWrite": "Write {n} file(s)",
  "brain.runWriting": "Writing…",
  "brain.runBackupNote":
    "Before overwriting an existing file, the original is backed up next to it as .synaroute.bak. Writes are atomic (temp file, then rename).",
  "brain.runNoChanges":
    "The decider didn't emit any writable file blocks — it most likely replied with prose rather than full file contents. See the raw output below.",
  "brain.runNoWorkDir":
    "No working directory for this round, so nothing will be written. The decider's full output is below.",
  "brain.runDeciderOutput": "Decider output",
  "brain.runOverwrite": "overwrite",
  "brain.runCreate": "new",
  "brain.runRejectedTag": "rejected",
  "brain.runRejectedCount": "{n} item(s) were rejected by the safety checks and will not be written.",
};
