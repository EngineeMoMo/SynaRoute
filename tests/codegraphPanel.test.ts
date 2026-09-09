/**
 * codegraph 面板的接线纪律。**本仓第 23 次盯同一类盲区**：Rust 侧 12 条用例全都直调
 * `codegraph::detect`，而缺陷本体在「前端算出什么目录、传给谁」这条线上 ——
 * 把前端改回自己算目录，那 12 条照样全绿。
 *
 * 用户实报的形态（2026-09-08）：开着「自动跟随最近活动项目」、活跃项目就显示在同一屏上方
 * （`C:\workCode\reviewCode\jztac-server`）、项目里也确实有 `.codegraph/`，而面板报
 * **未索引** + 禁用「为当前项目建索引」+ 一句 **「请先设置工作目录」**。
 *
 * 根因是前端那行 `const effectiveDir = autoFollow ? undefined : workDir` —— 自动跟随时
 * 刻意传 `undefined`（旧注释理由：「此处拿不到具体路径」）。而后端 `detect(None)` 当时返回
 * `NotIndexed`，于是「我没查」被谎报成「我查了，没有」。那句旧注释也已过时：同一个文件里的
 * `ActiveProjectDisplay` 就在显示这个路径，来源与运行时口径（`workdirs::scan()[0]`）一致。
 *
 * 三条判据各对应修复的一半，且都**只看代码**（剥注释）—— 本仓已三次栽在
 * 「注释里的字面量满足了断言」上。
 */
import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";

// ⚠️ 面板 2026-09-08 从 `BrainPage.tsx` 抽成了独立组件（那个文件顶着棘轮冻结值）。
// 判据跟着搬 —— 本仓栽过：搬代码后源码级判据仍扫旧文件，于是它静默退化成什么都没查。
const PANEL = readFileSync("src/components/CodegraphPanel.tsx", "utf8").replace(/\r\n/g, "\n");
const BRIDGE = readFileSync("src/lib/bridge.ts", "utf8").replace(/\r\n/g, "\n");

/** 剥掉 `//` 与块注释行再看代码。 */
function code(text: string): string {
  return text
    .split("\n")
    .filter((l) => !/^\s*(\/\/|\/\*|\*)/.test(l))
    .join("\n");
}

/**
 * 取 `CodegraphPanel` 函数体（剥注释）。
 *
 * 签名是多行的（`function CodegraphPanel({\n  t,\n  categoryId,` …），故只锚到左括号；
 * 组件独占该文件，取到末尾即可。**先断言锚点在**，否则改名会让整份判据静默变成空洞的绿。
 */
function panelBody(): string {
  const start = PANEL.indexOf("function CodegraphPanel(");
  expect(start, "CodegraphPanel 改名或挪走了 —— 先修判据，别改断言").toBeGreaterThan(-1);
  return code(PANEL.slice(start));
}

describe("codegraph 面板：目录由后端判定", () => {
  const body = panelBody();

  it("面板不许自己算目录 —— 那会复刻后端 resolve_writable_work_dir 的优先级", () => {
    // 缺陷本体那行的形态：把 autoFollow 三元成 undefined。
    expect(
      /autoFollow\s*\?[^\n]*undefined/.test(body),
      "自动跟随时传 undefined 就是缺陷本体：后端因此判不出目录、报「索引状态未知」，" +
        "而活跃项目明明显示在同一屏上方",
    ).toBe(false);
    // 更一般的形态：任何本地算出的「有效目录」变量。
    expect(
      /const\s+effectiveDir\b/.test(body),
      "目录只能来自后端返回的 state.dir；本地再算一份必然与后端漂移，且漂移是静默的",
    ).toBe(false);
  });

  it("两个 codegraph 命令都按分类调，且不传目录", () => {
    expect(body).toMatch(/api\.detectCodegraph\(\s*categoryId\s*\)/);
    expect(body).toMatch(/api\.codegraphInit\(\s*categoryId\s*\)/);
    // bridge 侧同样不许再收 workDir —— 收了就等于把「任意目录写入」这个面留着
    // （codegraph init 会在给定目录里创建 .codegraph/）。
    const cg = code(BRIDGE.slice(BRIDGE.indexOf("detectCodegraph:")));
    const seg = cg.slice(0, cg.indexOf("mcpStatus"));
    expect(seg).toMatch(/detectCodegraph:\s*\(categoryId/);
    expect(seg).toMatch(/codegraphInit:\s*\(categoryId/);
    expect(
      /workDir/.test(seg),
      "bridge 的两个 codegraph 命令不该再提 workDir —— 目录在后端解析",
    ).toBe(false);
  });

  it("「请先设置工作目录」只许出现在 indexUnknown 态", () => {
    // 这句话在别的状态下都是把用户送去做一个他已经做过的操作
    //（本仓「指错方向的提示比没有提示更糟」那条）。
    const m = /\{s === "(\w+)" &&\s*\(?\s*<span[^>]*>\{t\("brain\.cgNeedWorkDir"\)\}/.exec(body);
    expect(m, "cgNeedWorkDir 的渲染形态变了 —— 先确认它仍然只在判不出目录时出现").not.toBeNull();
    expect(m?.[1]).toBe("indexUnknown");

    // 反向：未索引态的建索引按钮不许再被「没有目录」禁用 —— 后端已给出 dir。
    const btn = /\{s === "notIndexed" &&[\s\S]*?<\/Button>/.exec(body)?.[0] ?? "";
    expect(btn, "notIndexed 分支不见了").not.toBe("");
    expect(
      /disabled=\{[^}]*!\s*\w*[Dd]ir/.test(btn),
      "notIndexed 时目录已判定，按钮不该因「没目录」禁用 —— 那正是用户看到的那个死路",
    ).toBe(false);
  });

  it("判定到的目录要显示出来 —— 用户看到「未索引」后第一个问题是「哪个项目」", () => {
    expect(body).toMatch(/"dir" in state/);
    expect(body).toMatch(/state\.dir/);
  });
});
