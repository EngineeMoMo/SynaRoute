/**
 * 演示数据：客户端环境变量冲突（`EnvConflictBanner` / 设置页那一段）。
 *
 * 从 `mockData.ts` 抽出来 —— 那个文件顶着新文件上限 900 行，而本仓腾空间的正确动作是
 * **删或搬**，不是重排（CLAUDE.md 记过：为腾 1 行去改注释、结果三行还是三行）。
 * 同族的既有分片：`mockData.events.ts`、`mockData.sessions.ts`。
 *
 * ⚠️ 策略门 `no-hardcoded-local-paths` 按 `src/lib/mockData.` **前缀**跳过，所以新分片
 * 天然在跳过范围内 —— 那条跳过规则原先写死单个文件名，拆分片时踩过一次。
 */
import type { EnvFinding, EnvRemovalResult } from "../types";

/**
 * 三条各有用途，别随手删：
 * - `conflict` 那条是这个功能存在的理由（接入提示说成功、请求却一个都没到代理）；
 * - `notice` 刻意用 `OPENAI_API_KEY` —— 用来验「它不该被报成 conflict」在界面上也成立
 *   （取证见 `tools/env_conflicts.rs` 模块头 ①：我们的 provider 块不写 `env_key`）；
 * - 目录类那条**报得出来但不给删**，用来验「按钮上的条数 < 报出来的条数 + 一句解释」
 *   这个形态在浏览器预览里真的出现。
 */
export const MOCK_ENV_FINDINGS: EnvFinding[] = [
  {
    name: "ANTHROPIC_BASE_URL",
    source: "user",
    severity: "conflict",
    value: "https://api.anthropic.com",
    hasValue: true,
    note: "它把 Claude CLI 指向 https://api.anthropic.com，而 SynaRoute 的入口是 http://127.0.0.1:47100。两者哪个生效取决于客户端 —— 若接入完成后请求仍不经过代理，先移除它再试。",
    removable: true,
  },
  {
    name: "OPENAI_API_KEY",
    source: "process",
    severity: "notice",
    // 凭据类：值恒为空串、只报「已设置」（后端 `shown_value` 就是这么做的）。
    value: "",
    hasValue: true,
    note: "SynaRoute 写的 Codex provider 用 experimental_bearer_token、不读环境变量，所以它通常无害。只有在你手改过 config.toml 用了 env_key 时，它才会顶掉那里的值。",
    removable: true,
  },
  {
    // ⚠️ 反斜杠必须转义。原先这两处写的是 `"D:\codex-home"` —— TS 里 `\c` 就是 `c`，
    // 于是演示数据渲染出来是 `D:codex-home`，一个不存在的路径形态。
    name: "CODEX_HOME",
    source: "user",
    severity: "conflict",
    value: "D:\\codex-home",
    hasValue: true,
    note: "Windows 用户级环境变量里有 CODEX_HOME=D:\\codex-home，而 SynaRoute 这个进程没继承到它 —— 我们写的是默认目录，Codex 读的是这个。重启 SynaRoute 即可对齐。",
    removable: false,
  },
];

export function mockEnvRemoval(names: string[]): EnvRemovalResult {
  return {
    removed: names,
    backupPath:
      "%APPDATA%\\SynaRoute\\backups\\env-conflicts\\env-conflicts-20260906-000000.000.json",
    failed: [],
    note: "已改的是「用户级环境变量」。已经在运行的程序（包括 Codex / Claude Code）仍持有旧值，要重启它们才生效。",
  };
}
