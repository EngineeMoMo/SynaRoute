// 预览模式的 Codex 会话样本。从 mockData.ts 抽出（那边一加就超新文件上限 900 行），
// 同 mockData.events.ts / mockData.usage.ts / mockData.vendors.ts 的做法。
//
// 样本刻意覆盖会话页要呈现的每一种形态 —— 官网截图与浏览器预览都只看得到这里：
// ① 一条 provider 与当前不一致（标红那一列是这个页面存在的主要理由）；
// ② 一条 fork 子会话（文件名带两个 UUID、会话库里没有它的记录 → 标题只能从正文读）；
// ③ 一条 Codex 内部派生的 guardian_review（要标注出来，否则「我明明只开了几个对话，
//    这里怎么多出一条」无从解释），顺带用它的超长标题验证截断；
// ④ 一条已归档。

import type {
  CodexProviderTargetList,
  CodexSessionIndexAudit,
  CodexSessionList,
} from "@/types";

const ROWS: CodexSessionList["rows"] = [
  {
    relPath:
      "sessions/2026/09/03/rollout-2026-09-03T10-12-00-01a06c85-7405-72c1-9fcb-778b38bf3907.jsonl",
    threadId: "01a06c85-7405-72c1-9fcb-778b38bf3907",
    provider: "synaroute",
    archived: false,
    cwd: "C:\\work\\demo",
    timestamp: "2026-09-03T10:12:00.000Z",
    bytes: 182_343,
    threadSource: "user",
    forked: false,
    title: "查询纽约今日天气",
    model: "gpt-5.6-luna",
    effort: "low",
    tokens: 25_454,
    modelUnserviceable: false,
    project: "demo3",
  },
  {
    relPath:
      "sessions/2026/09/01/rollout-2026-09-01T21-06-47-01a05d14-4e5b-7773-b425-25ae029f078f.jsonl",
    threadId: "01a05d14-4e5b-7773-b425-25ae029f078f",
    provider: "openai",
    archived: false,
    cwd: "C:\\work\\legacy",
    timestamp: "2026-09-01T21:06:47.000Z",
    bytes: 52_879,
    threadSource: "user",
    forked: false,
    title: "实现 Java 快速排序",
    model: "glm-5.3",
    effort: "xhigh",
    tokens: 2_283_034,
    // 刻意为 true：这一位只有代理侧能给，浏览器预览里必须看得见
    modelUnserviceable: true,
    project: "demo3",
  },
  {
    relPath:
      "sessions/2026/09/01/rollout-2026-09-01T21-08-35-01a05d14-4e5b-7773-b425-25ae029f078f_01a05d15-f502-7363-867c-b9ab2203be6f.jsonl",
    threadId: "01a05d15-f502-7363-867c-b9ab2203be6f",
    provider: "openai",
    archived: false,
    cwd: "C:\\work\\legacy",
    timestamp: "2026-09-01T21:08:35.000Z",
    bytes: 18_204,
    threadSource: "user",
    forked: true,
    title: "把上面那段改成迭代写法",
    model: "",
    effort: "",
    tokens: 0,
    modelUnserviceable: false,
    project: "",
  },
  {
    relPath:
      "sessions/2026/09/01/rollout-2026-09-01T21-49-02-01a05d3b-0017-7ec3-9cff-23ea32293b1b.jsonl",
    threadId: "01a05d3b-0017-7ec3-9cff-23ea32293b1b",
    provider: "synaroute",
    archived: false,
    cwd: "C:\\work\\legacy",
    timestamp: "2026-09-01T21:49:02.000Z",
    bytes: 96_431,
    threadSource: "guardian_review",
    forked: false,
    // ⚠️ 这里**刻意不再放那段注入文本**（原先是
    // "The following is the Codex agent history whose request action you are assessing…"）。
    // 后端 `view::is_injected_prompt` 现在会把它清掉并回落读正文，演示数据留着旧形态
    // 会让浏览器预览显示一个**后端已经不可能产出**的界面 —— 而预览是本仓验证布局的
    // 唯一手段，拿它当判据会得出错的结论（本轮就差点）。
    title: "权限裁决：exec_command",
    model: "gpt-5.6-luna",
    effort: "low",
    tokens: 36_184,
    modelUnserviceable: false,
    project: "demo3",
  },
  {
    relPath:
      "archived_sessions/2026/08/28/rollout-2026-08-28T09-00-00-01a04f10-1111-7222-8333-444455556666.jsonl",
    threadId: "01a04f10-1111-7222-8333-444455556666",
    provider: "synaroute",
    archived: true,
    cwd: "C:\\work\\old",
    timestamp: "2026-08-28T09:00:00.000Z",
    bytes: 697_060,
    threadSource: "user",
    forked: false,
    title: "重构缓存层",
    model: "claude-opus-5",
    effort: "high",
    tokens: 411_902,
    modelUnserviceable: false,
    project: "demo3",
  },
];

export function mockCodexSessions(): CodexSessionList {
  const current = "synaroute";
  return {
    rows: ROWS,
    currentProvider: current,
    unreadable: 0,
    pathRejected: 0,
    stats: {
      total: ROWS.length,
      active: ROWS.filter((r) => !r.archived).length,
      archived: ROWS.filter((r) => r.archived).length,
      mismatched: ROWS.filter((r) => r.provider !== current).length,
      dbPath: "C:\\Users\\demo\\.codex\\state_5.sqlite",
      // 与 total 的差额 = 会话库里没有记录的那些（这里是那条 fork 子会话）。
      inDb: ROWS.length - 1,
      modelGone: ROWS.filter((r) => r.modelUnserviceable).length,
    },
  };
}

export function mockCodexProviderTargets(): CodexProviderTargetList {
  return {
    current: "synaroute",
    ours: "synaroute",
    targets: [
      { id: "synaroute", sources: ["config", "rollout", "sqlite"], isCurrent: true },
      { id: "cc-switch-relay", sources: ["config"], isCurrent: false },
      { id: "openai", sources: ["config", "rollout"], isCurrent: false },
    ],
    prefs: { autoSyncDisabled: false, lastTarget: "" },
  };
}

export function mockCodexSessionIndexAudit(): CodexSessionIndexAudit {
  return { total: 5, orphans: 1, sample: ["01a04e00-dead-7000-8000-000000000001"] };
}
