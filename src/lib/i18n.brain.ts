// 大脑聚合「运行面板」+「一键会诊入口」的本地化词条（Phase1 计划 → Phase2a 预览 → Phase2b 落盘）。
//
// 从 i18n.ts 拆出来的（那边冻结在棘轮上、余量为 0）。粒度按**界面区块**分，
// 与 i18n.fields.ts / i18n.mapping.ts 同一口径。
//
// ⚠️ zh 与 en 的 key 集合必须完全一致，且**本文件必须在 src/lib/i18n.test.ts 的 SOURCES
// 与 CHUNKS 两张表里各有一条** —— 否则这一整页词条会静默脱离对称性保护。

type Dict = Record<string, string>;

export const brainRunZh: Dict = {
  "brain.runTitle": "发起会诊",
  "brain.runPlaceholder": "输入需求…如：给登录页添加记住密码功能",
  "brain.runStart": "开始会诊",
  "brain.runThinking": "会诊进行中：独立分析 → 汇总意见 → 生成报告。完成后会在下方显示结果。",
  "brain.runConfirm": "生成文件修改预览",
  "brain.runReset": "新一轮",
  "brain.runPlanTitle": "会诊报告",
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
  // 一键会诊入口（BrainQuickStart）：未配置时把招牌能力做成一键可发起
  "brain.consultTitle": "大脑会诊",
  "brain.consultDesc":
    "把同一个需求同时问多个模型，再由决策者综合出一份答案。首次点击会自动配好成员与决策者并保存，随后即可直接发起。",
  "brain.consultCost": "将同时咨询 {n} 个模型，每个都会消耗对应 Key 的额度，比单次调用更慢。",
  "brain.consultStart": "一键配置并开始会诊",
  "brain.consultStarting": "配置中…",
  "brain.consultConfigured": "已保存 {n} 位成员与决策者，可输入问题开始会诊。",
  "brain.consultIndependent": "使用你配置的模型独立分析并汇总，无需 Jev 或额外决策服务。场景模板只调整分析重点。",
  "brain.consultSavedConfig": "使用下方已保存的成员、汇总方式和工具轮数；修改配置后请先保存。各阶段消耗对应 Key 的额度。本次先出报告，不会修改文件。",
  "brain.consultQuestion": "你希望解决什么问题？",
  "brain.templateTitle": "会诊场景",
  "brain.template.general.title": "自由会诊",
  "brain.template.general.description": "各模型独立分析，再综合结论、分歧与下一步。",
  "brain.template.general.placeholder": "描述问题、背景和你最在意的结果…",
  "brain.template.debug.title": "疑难 Bug",
  "brain.template.debug.description": "对照现象与证据，区分根因假设，给出最小验证步骤。",
  "brain.template.debug.placeholder": "发生了什么？预期行为是什么？附上报错、复现步骤或相关文件…",
  "brain.template.debug.instruction": "本次是疑难 Bug 会诊。请按证据分析可能根因、反证和最小复现；提出能区分不同根因的验证步骤，再给修复建议。缺少日志或代码时明确说明，不编造运行结果。",
  "brain.template.compare.title": "方案对比",
  "brain.template.compare.description": "按约束比较取舍，明确推荐条件与可能改变选择的因素。",
  "brain.template.compare.placeholder": "要比较哪些方案？有哪些预算、性能、维护成本或交付时间约束？",
  "brain.template.compare.instruction": "本次是技术方案对比。请基于用户目标和约束比较候选方案的收益、成本、风险与可逆性，给出推荐及其成立条件。缺失条件标为假设，明确什么变化会让另一方案更合适。不要为了凑方案虚构事实。",
  "brain.template.review.title": "代码审查",
  "brain.template.review.description": "聚焦可复现的问题、影响范围与验证方法，避免泛泛建议。",
  "brain.template.review.placeholder": "说明要审查的改动、相关文件以及重点关注的行为…",
  "brain.template.review.instruction": "本次是代码审查。优先检查正确性、边界条件、兼容性和安全问题；每项发现给出文件或代码依据、触发条件、影响与验证方式。没有代码依据时明确标为待确认，不把个人风格偏好报告为缺陷。",
  "brain.report.conclusion": "推荐结论",
  "brain.report.evidence": "依据与共识",
  "brain.report.disagreements": "分歧与待确认",
  "brain.report.verification": "验证与下一步",
  "brain.reportCaution": "以下是模型综合意见；共识不代表事实，待验证项仍需通过代码、日志或实际检查确认。",
  "brain.reportCopy": "复制完整报告",
  "brain.reportCopied": "已复制",
  "brain.reportCopyFailed": "复制失败，请展开原文后手动选择并复制。",
};

export const brainRunEn: Dict = {
  "brain.runTitle": "Start a consultation",
  "brain.runPlaceholder": "Describe what you'd like to change, e.g. \"Add a dark mode toggle to Settings\"",
  "brain.runStart": "Start consultation",
  "brain.runThinking": "Consulting: independent analysis → synthesis → report. Results will appear below when ready.",
  "brain.runConfirm": "Preview file changes",
  "brain.runReset": "New round",
  "brain.runPlanTitle": "Consultation report",
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
  "brain.consultTitle": "Brain Consultation",
  "brain.consultDesc":
    "Ask several models the same request, then let the decider synthesize one answer. The first click auto-configures the members and a decider and saves them, so you can start right away.",
  "brain.consultCost":
    "Consults {n} model(s) at once; each spends its own key's quota and is slower than a single call.",
  "brain.consultStart": "Configure & start consultation",
  "brain.consultStarting": "Configuring…",
  "brain.consultConfigured": "Saved {n} members and a decider. Enter a question to start.",
  "brain.consultIndependent": "Your configured models analyze and synthesize independently. No Jev or additional decision service required. Templates only change the analysis focus.",
  "brain.consultSavedConfig": "Uses the saved members, synthesis mode and tool rounds below. Save configuration changes first. Each stage uses its key's quota. This step produces a report without changing files.",
  "brain.consultQuestion": "What would you like to resolve?",
  "brain.templateTitle": "Consultation focus",
  "brain.template.general.title": "Open discussion",
  "brain.template.general.description": "Independent analysis followed by a recommendation, disagreements and next steps.",
  "brain.template.general.placeholder": "Describe the problem, context and the outcome that matters to you…",
  "brain.template.debug.title": "Debugging",
  "brain.template.debug.description": "Compare symptoms with evidence, separate hypotheses and identify the smallest useful checks.",
  "brain.template.debug.placeholder": "What happened? What did you expect? Include errors, reproduction steps or relevant files…",
  "brain.template.debug.instruction": "This is a debugging consultation. Examine evidence, possible causes, counter-evidence and minimal reproduction steps. Propose checks that distinguish competing causes, then suggest fixes. State when logs or code are missing; never invent execution results.",
  "brain.template.compare.title": "Compare approaches",
  "brain.template.compare.description": "Compare tradeoffs against constraints and explain what could change the recommendation.",
  "brain.template.compare.placeholder": "Which approaches are you considering? Include budget, performance, maintenance or delivery constraints…",
  "brain.template.compare.instruction": "This is a technical approach comparison. Compare benefits, costs, risks and reversibility against the user's goals and constraints. Recommend an approach and state its conditions. Label missing constraints as assumptions and explain what would favor another approach. Do not fabricate facts to fill the comparison.",
  "brain.template.review.title": "Code review",
  "brain.template.review.description": "Focus on reproducible issues, their impact and verification rather than generic advice.",
  "brain.template.review.placeholder": "Describe the changes, relevant files and behavior you want reviewed…",
  "brain.template.review.instruction": "This is a code review. Prioritize correctness, edge cases, compatibility and security. For each finding provide file or code evidence, trigger conditions, impact and verification. Mark issues without code evidence as unconfirmed. Do not report personal style preferences as defects.",
  "brain.report.conclusion": "Recommendation",
  "brain.report.evidence": "Evidence and agreement",
  "brain.report.disagreements": "Disagreements and unknowns",
  "brain.report.verification": "Verification and next steps",
  "brain.reportCaution": "This report synthesizes model opinions. Agreement is not proof; verify open questions against code, logs or actual checks.",
  "brain.reportCopy": "Copy full report",
  "brain.reportCopied": "Copied",
  "brain.reportCopyFailed": "Copy failed. Select and copy the original text manually.",
};
