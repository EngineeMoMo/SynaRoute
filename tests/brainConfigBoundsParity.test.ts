// 大脑聚合配置的**前后端边界对账**：界面允许填的范围必须落在后端会接受的范围里。
//
// # 这条判据补的是一个真缺陷（2026-09-04 审查发现）
//
// `brain_config::validate` 是这一轮新加的（此前 `save_brain` 零校验），它把总超时的下限
// 定在 10 秒 —— 理由是实在的：`decider_floor_ms` 要从整轮预算里切 35% 给决策者，再低就
// 连一次决策者调用都发不出去。而 `BrainPage.tsx` 那个输入框的 `min` 一直是 **5000**，
// 是校验加进来之前的旧值。
//
// 于是界面上 spinner 能一路点到 5 秒（`NumberField` 是裸 `<input type=number>`，
// `onChange` 直接 `Number(e.target.value)`、**不做 clamp**，所以手打甚至能填 0），
// 而点保存必被后端拒。这正是本仓「界面能填、存不进去」那一类（`headers_json` 同形）：
// 失效不是崩溃，是用户对着一个自己刚调过的数字被告知它不合法。
//
// # 为什么用机械判据而不是注释
//
// 两个数字分别活在 Rust 常量与 JSX 属性里，编译器管不到这条缝，而**漏掉的表现是静默的**
// （改后端下限时没人会想到去翻前端的 JSX）。JSX 属性区还不能写 `//` 注释，也就是说
// 「为什么是 10000」这句话在那一行根本放不下 —— 判据文件是它唯一的家。
//
// # 边界：只对账 `validate` 会**硬拒**的那几项
//
// 并发上限 / 工具轮数 / 字符预算这些在运行时都有 `clamp`，填夸张的数字不会被拒（只是
// 不生效），故不在这里对账 —— 把它们也收进来会让判据变成「前后端两份数字必须逐一相等」，
// 而那不是事实：前端刻意比后端更严（`maxContextTokens` 前端 50 万、后端 100 万）。
import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

const read = (p: string) => readFileSync(resolve(process.cwd(), p), "utf8");

/** Rust 侧的数字常量（`10_000` 这种下划线分隔要去掉）。 */
function rustConst(src: string, name: string): number {
  const m = new RegExp(`const ${name}[^=]*=\\s*([0-9_*\\s]+);`).exec(src);
  if (!m) throw new Error(`brain_config.rs 里找不到常量 ${name} —— 判据会空转，先修判据`);
  // `30 * 60 * 1_000` 这种乘法表达式也要算得出来。
  return m[1]
    .split("*")
    .map((p) => Number(p.replace(/[_\s]/g, "")))
    .reduce((a, b) => a * b, 1);
}

/** `BrainPage.tsx` 里某个 `NumberField` 的 min/max（按 onChange 写回的字段名定位）。 */
function fieldBounds(src: string, field: string): { min: number; max: number } {
  const blocks = src.split("<NumberField").slice(1);
  const hit = blocks.find((b) => b.includes(`update({ ${field}: v })`));
  if (!hit) throw new Error(`BrainPage.tsx 里找不到写 ${field} 的 NumberField —— 判据会空转`);
  const num = (attr: string) => {
    const m = new RegExp(`${attr}=\\{(\\d+)\\}`).exec(hit);
    if (!m) throw new Error(`${field} 的 NumberField 缺 ${attr}`);
    return Number(m[1]);
  };
  return { min: num("min"), max: num("max") };
}

const rs = read("src-tauri/src/brain_config.rs");
const page = read("src/pages/BrainPage.tsx");

describe("大脑聚合配置的前后端边界", () => {
  it("总超时：界面的 min/max 必须落在后端会接受的区间里", () => {
    const lo = rustConst(rs, "MIN_TOTAL_TIMEOUT_MS");
    const hi = rustConst(rs, "MAX_TOTAL_TIMEOUT_MS");
    const { min, max } = fieldBounds(page, "totalTimeoutMs");
    // 前端可以更严（min 更大 / max 更小），但绝不能更宽 —— 更宽 = 界面引导用户去填一个
    // 保存必被拒的值，而他改的可能是别的字段、只是顺手看了一眼这个 spinner。
    expect(min, `界面下限 ${min} < 后端下限 ${lo}：spinner 能点到一个存不进去的值`).toBeGreaterThanOrEqual(lo);
    expect(max, `界面上限 ${max} > 后端上限 ${hi}`).toBeLessThanOrEqual(hi);
  });

  it("注入文件 token 上限：界面上限不得超过后端硬拒的那条线", () => {
    const hi = rustConst(rs, "MAX_CONTEXT_TOKENS");
    const { max } = fieldBounds(page, "maxContextTokens");
    expect(max).toBeLessThanOrEqual(hi);
  });

  it("判据自己不许空转", () => {
    // 三个常量都还在（改名/删掉会让上面两条静默退化成什么都没查），且 validate 真的在读它们。
    for (const name of ["MIN_TOTAL_TIMEOUT_MS", "MAX_TOTAL_TIMEOUT_MS", "MAX_CONTEXT_TOKENS"]) {
      expect(rustConst(rs, name)).toBeGreaterThan(0);
      expect(rs, `${name} 定义了却没人用 = 校验被摘掉了`).toContain(`${name}`);
    }
    // 而 `save_brain` 必须真的调 validate —— 否则前后端边界一致也没有意义。
    expect(read("src-tauri/src/store.rs")).toContain("brain_config::validate(&brain)?");
  });
});
