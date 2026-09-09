/**
 * 环境变量告警横幅的三条纪律。本仓无 jsdom，故按「纯规则 + 源码形态」两层测。
 *
 * 三条各对应一个**实际修过的缺陷**（都是 2026-09-06 全链路审查查出来的）：
 *
 * 1. **桌面端不许报 `ANTHROPIC_*`** —— 它压根不读那些环境变量（走 `deploymentMode=3p` +
 *    `configLibrary`；`tools.rs` 模块头有取证：早期往 `claude_desktop_config.json` 写
 *    `baseUrl` 从未生效）。在那一页报它 = 叫用户删一个与故障无关的系统设置，而真正的成因
 *    被这条告警盖住。
 * 2. **切分类必须清掉上一个分类的 `note`/`found`** —— `note` 参与渲染判据，不清的表现是
 *    「在 claude-cli 点过移除后切到 codex，横幅照旧显示、写着 claude-cli 那次的结果」。
 * 3. **`await` 之后不许用闭包里的 `category` 判「现在是哪个分类」** —— 那是捕获的值，
 *    `started !== category` 恒为 false，守卫等于没写（第一版就是这么错的）。必须读 ref。
 */
import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";

const SRC = readFileSync("src/components/EnvConflictBanner.tsx", "utf8").replace(/\r\n/g, "\n");

/** 剥掉注释再看代码 —— 本仓栽过多次「注释里的字面量满足了断言」。 */
function code(text: string): string {
  return text
    .split("\n")
    .filter((l) => !/^\s*(\/\/|\/\*|\*)/.test(l))
    .join("\n");
}

/**
 * 把组件里的 `relevant` 抽出来跑。
 *
 * 刻意从**源码**取而不是在测试里抄一份：抄一份的话组件改了判据、测试照样绿 ——
 * 那正是这条判据要防的东西（同 `modelMappingRows` 的做法）。
 */
function loadRelevant(): (f: { severity: string; name: string }, c: string) => boolean {
  const m = /function relevant\(([\s\S]*?)\n\}/.exec(SRC);
  if (!m) throw new Error("relevant 改名或改形态了 —— 先修判据");
  const body = code("function relevant(" + m[1] + "\n}");
  // 去掉 TS 类型标注，得到可执行的 JS。
  const js = body
    .replace(/function relevant\([^)]*\)\s*:\s*boolean/, "function relevant(f, category)")
    .replace(/\bas\s+\w+/g, "");
  // eslint-disable-next-line no-new-func
  return new Function(`${js}; return relevant;`)() as ReturnType<typeof loadRelevant>;
}

describe("EnvConflictBanner", () => {
  const relevant = loadRelevant();
  const conflict = (name: string) => ({ severity: "conflict", name });

  it("桌面端不许报 ANTHROPIC_* —— 它不读那些环境变量，报了是指错方向", () => {
    expect(relevant(conflict("ANTHROPIC_BASE_URL"), "claude-cli")).toBe(true);
    expect(
      relevant(conflict("ANTHROPIC_BASE_URL"), "claude-desktop"),
      "桌面端走 deploymentMode=3p + configLibrary，压根不读 ANTHROPIC_BASE_URL",
    ).toBe(false);
    expect(relevant(conflict("ANTHROPIC_AUTH_TOKEN"), "claude-desktop")).toBe(false);
  });

  it("CODEX_* 只对 codex 分类相关", () => {
    expect(relevant(conflict("CODEX_HOME"), "codex")).toBe(true);
    expect(relevant(conflict("CODEX_HOME"), "claude-cli")).toBe(false);
  });

  /**
   * 🔴 **前缀判据必须大小写无关** —— 否则后端修了、这一层照旧筛掉。
   *
   * Windows 的环境变量名原生大小写不敏感：`Anthropic_Api_Key` 与 `ANTHROPIC_API_KEY`
   * 是同一个变量，客户端照样读得到。后端为此走 `env_name_eq`（按宿主平台取语义）把它
   * 判成 conflict；而这里若裸 `startsWith`，那条发现就死在前端 —— 横幅在它唯一该说话的
   * 那次**静默不出现**，用户看到的是「接入说成功、请求一个都没到代理」，而应用里一个字都没有。
   *
   * 这一条守的正是「后端改了、前端没跟上」这半条链，而它是静默的（后端用例全绿）。
   */
  it("前缀判据必须大小写无关 —— Windows 上混合大小写的变量名同样生效", () => {
    for (const name of ["Anthropic_Api_Key", "anthropic_base_url", "AnThRoPiC_AUTH_TOKEN"]) {
      expect(relevant(conflict(name), "claude-cli"), `${name} 必须被认出来`).toBe(true);
      expect(relevant(conflict(name), "claude-desktop"), `${name} 在桌面端仍不该报`).toBe(false);
    }
    expect(relevant(conflict("Codex_Home"), "codex"), "CODEX_ 那半同理").toBe(true);
    // 反向：不许因为放宽大小写就把别家的变量也收进来。
    expect(relevant(conflict("MY_ANTHROPIC_KEY"), "claude-cli"), "仍必须是前缀匹配").toBe(false);
  });

  it("notice 级一条都不上横幅（只进诊断报告）", () => {
    // OPENAI_* 落在 notice 上：我们的 Codex provider 用 experimental_bearer_token、
    // 不读环境变量，报成 conflict 对每个装过 OpenAI SDK 的用户都是假警。
    expect(relevant({ severity: "notice", name: "OPENAI_API_KEY" }, "codex")).toBe(false);
    expect(relevant({ severity: "notice", name: "ANTHROPIC_BASE_URL" }, "claude-cli")).toBe(false);
  });

  it("切分类必须清掉上一个分类的 note 与 found", () => {
    const effect = /useEffect\(\(\) => \{([\s\S]*?)\}, \[category\]\);/.exec(SRC);
    expect(effect, "那个 useEffect 改形态了 —— 先修判据").toBeTruthy();
    const body = code(effect![1]);
    for (const call of ['setNote("")', "setFound([])"]) {
      expect(
        body.includes(call),
        `切分类时必须 ${call} —— note 参与渲染判据，不清会让上一个分类的结果显示在新分类页上`,
      ).toBe(true);
    }
  });

  it("await 之后的守卫必须读 ref，不许比闭包里的 category", () => {
    const body = code(SRC);
    expect(
      body.includes("currentRef.current"),
      "守卫要读 ref —— 闭包里的 category 在 await 之后仍是这一趟开始时的值",
    ).toBe(true);
    // 反向：不许出现「拿 started 和闭包 category 比」那个恒假形态。
    expect(
      /started\s*[!=]==\s*category\b/.test(body),
      "started 与闭包 category 比是恒真/恒假的，那道守卫等于没写（第一版的缺陷本体）",
    ).toBe(false);
  });

  /**
   * 🔴 **「为什么不给删」必须逐条用后端的 `keepReason`，不许前端用一句固定文案覆盖。**
   *
   * `removable: false` 有两个成因、处置**相反**：目录类是我们刻意不删（正确处置是对齐两侧，
   * 「重启让我们继承到」是对的）；process-only 是我们删不掉（正确处置是去 HKLM / 父进程把
   * 它去掉）。上一版只有一句 `t("env.keepHint")`，内容只对目录类成立 —— 于是最常见的形态
   * （用户在系统设置里设了 `ANTHROPIC_API_KEY`，我们启动时继承到 → process 与 user 各一条）
   * 会被告知「重启 SynaRoute 让它继承到」，而他要的恰恰是**让它消失**。
   *
   * 这条判据钉两件事：渲染必须读 `keepReason`；那句已作废的固定文案不许再出现。
   */
  it("不给删的理由必须逐条来自后端，不许用一句固定文案覆盖两种相反的处置", () => {
    const body = code(SRC);
    expect(
      body.includes("keepReason"),
      "必须渲染后端逐条给的 keepReason —— 两个成因的处置方向相反，一句话覆盖不了",
    ).toBe(true);
    expect(
      body.includes("env.keepHint"),
      "那句固定文案已作废（它只对目录类成立，对 process-only 指的方向是反的）",
    ).toBe(false);
    // 反向：前端不许自己判「这是哪一类」—— 同一事实两处各判一遍必然漂移。
    expect(
      /keepReason\s*=|CODEX_HOME|CODEX_SQLITE_HOME/.test(body),
      "前端不许自己按变量名推断类别或自造理由，那一位是后端的单一事实来源",
    ).toBe(false);
  });
});
