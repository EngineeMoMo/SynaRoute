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
 * 四条各有用途，别随手删：
 * - `conflict` 那条是这个功能存在的理由（接入提示说成功、请求却一个都没到代理）；
 * - `notice` 刻意用 `OPENAI_API_KEY` —— 用来验「它不该被报成 conflict」在界面上也成立
 *   （取证见 `tools/env_conflicts.rs` 模块头 ①：我们的 provider 块不写 `env_key`）；
 * - 目录类那条**报得出来但不给删**，用来验「按钮上的条数 < 报出来的条数 + 一句解释」
 *   这个形态在浏览器预览里真的出现；
 * - process-only 那条与目录类**成因不同、处置相反**，用来验两句 `keepReason` 各自出现
 *   （上一版界面只有一句固定文案，对这一条给的指路方向是反的）。
 *
 * 🔴 **`source: "process"` 必须配 `removable: false`**：后端 `classify` 里
 * `removable: source == "user"` —— HKCU 里没有的条目我们删不掉。演示数据造一个
 * 后端产不出来的形态，浏览器预览与官网截图就都在展示一个不存在的界面。
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
    keepReason: "",
  },
  {
    name: "OPENAI_API_KEY",
    source: "user",
    severity: "notice",
    // 凭据类：值恒为空串、只报「已设置」（后端 `shown_value` 就是这么做的）。
    value: "",
    hasValue: true,
    note: "SynaRoute 写的 Codex provider 用 experimental_bearer_token、不读环境变量，所以它通常无害。只有在你手改过 config.toml 用了 env_key 时，它才会顶掉那里的值。",
    removable: true,
    keepReason: "",
  },
  {
    // process-only：值来自 HKLM 或启动我们的那个父进程，我们**删不掉**。
    // 与下面目录类那条的 `keepReason` 刻意不同 —— 处置方向相反。
    name: "ANTHROPIC_API_KEY",
    source: "process",
    severity: "conflict",
    value: "",
    hasValue: true,
    note: "它会顶掉 SynaRoute 写进 settings.json 的 env 块，于是 Claude CLI 拿它直连官方、完全不经过代理。",
    removable: false,
    keepReason:
      "这一条不在 Windows 用户级环境变量里，本应用删不掉 —— 它来自系统级（HKLM）变量或启动 SynaRoute 的那个父进程（终端 / 脚本 / 启动器）。去那个来源把它去掉，然后重启 SynaRoute 与客户端。",
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
    keepReason:
      "目录类变量不提供移除 —— 删掉会破坏你自己配的 Codex 布局。正确处置是让两侧一致：重启 SynaRoute 让它继承到，或在系统设置里把它去掉。",
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
