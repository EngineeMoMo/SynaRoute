import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { pickPrefs } from "../src/lib/prefs";

/**
 * `UserPrefs` 的**跨语言不变量**：Rust `model.rs::UserPrefs` 的字段集必须与
 * TS `prefs.ts::pickPrefs` 返回的键集逐条对齐。
 *
 * 🔴 **这条判据补的是一个已经以 P0 形态发生过两次的失效方向**，而在此之前它只是
 * `model.rs` 里的一句注释（原文就写着「判据：本结构体的字段集必须与 pickPrefs 逐字段对齐」
 * —— 把一条纪律称作判据，却没有任何机械检查，正是本仓「判据存在 ≠ 判对了维度」那条教训）。
 *
 * 失效链：Rust 侧 `UserPrefs` 加一个字段 → 忘了同步 `pickPrefs` → 前端每次
 * `saveSettings` 提交的 JSON **缺这个键** → `#[serde(default)]` 补成 `false`/零值 →
 * `apply_to` 把它写回配置。用户视角是「切一下主题或语言，刚打开的开关就没了」。
 *
 * 已发生过的两次：
 * - 两个悬浮球开关误入 `UserPrefs` 而前端白名单没有 → 静默关掉用户开关；
 * - `auto_start` 方向相反 —— 把用户**刚关掉**的开机自启动装回系统（定级 P0）。
 *
 * 反方向也要管：`pickPrefs` 多一个 Rust 侧没有的键，那个字段会被后端
 * `deny_unknown_fields` 之外的默认行为静默丢掉，用户改了却不生效。
 *
 * 放在 `tests/` 而非 `src/`：要用 node:fs 读 Rust 源码。理由同
 * `reservedHeadersParity.test.ts` / `mcpEndpointParity.test.ts`。
 */

const here = dirname(fileURLToPath(import.meta.url));
const rustSrc = readFileSync(join(here, "../src-tauri/src/model.rs"), "utf8");

/**
 * 剥掉 TS 的块注释与行注释，只留可执行代码。
 *
 * 存在的理由见下方 "不得用解构 rest" 那条用例：本仓的注释里经常**逐字写出**
 * 「别这么写」的反例代码，裸 grep 必然命中它们。
 */
function stripComments(src: string): string {
  return src.replace(/\/\*[\s\S]*?\*\//g, "").replace(/^\s*\/\/.*$/gm, "");
}

/** snake_case → camelCase（serde 的 rename_all = "camelCase" 口径）。 */
function toCamel(s: string): string {
  return s.replace(/_([a-z])/g, (_, c: string) => c.toUpperCase());
}

/**
 * 从 Rust 源码里抽 `pub struct UserPrefs { … }` 的字段名。
 *
 * 只取 `pub <name>:` 形态，跳过属性行（`#[serde(...)]`）与注释 ——
 * 那两类里也会出现看着像字段名的东西（尤其注释里成段解释历史教训时会点名 `auto_start`、
 * `lan_exposure`，而它们恰恰是**刻意不在**这个结构体里的字段。把注释算进来会让判据
 * 反过来要求前端加上它们，正好推翻这个设计）。
 */
function rustUserPrefsFields(): string[] {
  const at = rustSrc.indexOf("pub struct UserPrefs {");
  expect(at, "Rust 侧找不到 `pub struct UserPrefs {` —— 结构体改名了？").toBeGreaterThan(-1);
  const body = rustSrc.slice(at, rustSrc.indexOf("\n}", at));
  const names = body
    .split("\n")
    .map((l) => l.trim())
    .filter((l) => !l.startsWith("//") && !l.startsWith("#["))
    .map((l) => /^pub\s+([a-z0-9_]+)\s*:/.exec(l)?.[1])
    .filter((n): n is string => !!n);
  // 解析到 0 个（或异常少）就主动失败：字段改写法或结构体挪走时，一个恒绿的判据
  // 比没有判据更糟 —— 本仓 `invoke-command-must-exist` 那条门就是这么踩过的
  // （第一版查 `invoke("...")`，实测命中 0 处而门是绿的）。
  expect(names.length, "解析出的字段数异常少，判据可能已失效").toBeGreaterThan(8);
  return names;
}

describe("UserPrefs 的跨语言一致性", () => {
  it("Rust UserPrefs 与前端 pickPrefs 是同一个字段集", () => {
    const rust = rustUserPrefsFields().map(toCamel).sort();
    // 用一份「全字段都有值」的假设置过一遍 pickPrefs，取它实际会提交的键。
    // 判据取**运行时返回的键**而不是 `UserPrefs` 那个 TS 类型：类型是编译期的，
    // 而真正决定「提交哪些键」的是这个函数体。两者也可能分叉（类型里列了、
    // 函数体忘了写），那种分叉 tsc 不报错 —— 因为多写的键在返回对象里就是缺失，
    // 而 Pick 出来的类型要求它存在……除非有人加了 `as`。故这里钉运行时。
    const ts = Object.keys(pickPrefs(fakeSettings())).sort();
    expect(ts).toEqual(rust);
  });

  it("带系统副作用的开关绝不能进这个集合", () => {
    // `auto_start` 要写系统注册表、`lan_exposure` 要重建监听 socket。
    // 它们一旦进 UserPrefs，前端的陈旧快照就能改写它们 ——
    // 前者是「把刚关掉的自启动装回去」（已发生过，P0），
    // 后者是「界面说局域网已关闭，而端口仍在 0.0.0.0 上」（安全方向）。
    const ts = Object.keys(pickPrefs(fakeSettings()));
    for (const forbidden of [
      "autoStart",
      "lanExposure",
      "masterPasswordEnabled",
      "proxyPorts",
      "activeModels",
      "proxyRunningCategories",
      "onboardingDone",
      "mcpEnabled",
      "mcpPort",
    ]) {
      expect(ts, `${forbidden} 属后端自管字段，不该由 saveSettings 提交`).not.toContain(
        forbidden,
      );
    }
  });

  it("pickPrefs 不得用解构 rest（否则新增的后端字段会默认漏出去）", () => {
    // 「加字段默认不发」必须是结构性保证，不是纪律。
    //
    // ⚠️ **必须先剥注释**：`prefs.ts` 的文档注释里正写着「禁止用解构 rest
    // （`const { autoStart, ...rest } = s`）」，裸 grep 会命中那句警告本身。
    // 与本仓 `data-dir-env-name-must-match` 第一版命中自己注释里那段
    // 「❌ 已证伪的修法」同一类假阳性 —— **判据说「代码里别这么写」，就只能看代码。**
    const src = stripComments(readFileSync(join(here, "../src/lib/prefs.ts"), "utf8"));
    expect(src).not.toMatch(/\.\.\.rest/);
    expect(src).not.toMatch(/\.\.\.s[,\s}]/);
    // 顺带确认剥注释没把函数体也剥掉（否则这条又是个空洞的绿）。
    expect(src).toContain("export function pickPrefs");
  });
});

/**
 * 造一份把**所有** AppSettings 键都填上的对象。
 *
 * 刻意不用 `mockData` 里的样本：那份是给界面看的，可能恰好缺某个字段，
 * 于是 `Object.keys(pickPrefs(...))` 会少一个键 —— 判据就会反过来要求 Rust 删字段。
 * 这里直接从 Rust 的 `AppSettings` 抽全部字段名，保证「前端漏掉一个」一定被看见。
 */
function fakeSettings() {
  const at = rustSrc.indexOf("pub struct AppSettings {");
  const body = rustSrc.slice(at, rustSrc.indexOf("\n}", at));
  const obj: Record<string, unknown> = {};
  for (const line of body.split("\n")) {
    const t = line.trim();
    if (t.startsWith("//") || t.startsWith("#[")) continue;
    const name = /^pub\s+([a-z0-9_]+)\s*:/.exec(t)?.[1];
    if (name) obj[toCamel(name)] = "x";
  }
  expect(Object.keys(obj).length, "AppSettings 字段解析失败").toBeGreaterThan(15);
  return obj as never;
}
