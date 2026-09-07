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
});
