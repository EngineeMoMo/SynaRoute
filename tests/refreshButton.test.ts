import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

/**
 * `RefreshButton` 的行为判据（源码级 —— 本仓无 jsdom，组件的忙态时序测不到运行时）。
 *
 * 这个组件是两处用户实报问题的收口（2026-09-18）：
 * ① 会话页与用量页两个刷新按钮长得不一样；② 数据在缓存里、转圈闪一下就停，「像 bug」。
 * 两页都改成用它，所以它的这几条性质一旦回归，两页一起坏。
 *
 * 🔴 判据钉**性质**不钉写法（本仓已为「钉写法→改进被假红逼回」栽过多次）：
 * 只要求「有最短时长、且减少动态效果时跳过它」「忙态锁 disabled + aria-busy」，
 * 不锁具体的毫秒数或变量名。
 */

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const read = (...p: string[]) => readFileSync(join(ROOT, ...p), "utf8");

/** 剥注释再扫：判据说「代码里必须这么写」，就只能看代码。 */
const code = (src: string) =>
  src
    .replace(/\/\*[\s\S]*?\*\//g, "")
    .split("\n")
    .filter((l) => !/^\s*(\/\/|\*)/.test(l))
    .join("\n");

describe("RefreshButton 的行为", () => {
  const src = code(read("src", "components", "ui", "RefreshButton.tsx"));

  it("🔴 有最短可见时长，且减少动态效果时跳过它", () => {
    // 最短时长：数据在缓存里时 onRefresh 几十毫秒就 resolve，没有它转圈会闪一下就停。
    expect(src, "要有一个最短时长常量").toMatch(/MIN_SPIN_MS\s*=\s*\d+/);
    // 跳过的前提：减少动态效果时图标本就不转，硬锁只是拖慢操作、毫无信息价值。
    expect(src, "补足时长前要先查减少动态效果").toMatch(/prefersReducedMotion\(\)/);
    // 且两者要真的关联（补时长的分支以「未开启减少动态效果」为条件），不是各写各的。
    expect(src, "最短时长必须在非减少动态效果时才补").toMatch(
      /!prefersReducedMotion\(\)/,
    );
  });

  it("🔴 忙态锁住重复点击（ref，不只靠 state）", () => {
    // 连点第二下必须被挡：否则两趟 load 交叠，又回到各页 genRef 要防的那种竞态。
    expect(src, "要用 ref 记录在忙").toMatch(/busyRef\.current/);
    expect(src, "忙时直接 return").toMatch(/if \(busyRef\.current\) return/);
  });

  it("🔴 忙态下 disabled 且 aria-busy", () => {
    expect(src).toMatch(/disabled=\{busy/);
    expect(src).toMatch(/aria-busy=\{busy\}/);
    // 转圈只在忙时加，稳定态不转（否则又是「一直在动」的噪音）。
    expect(src).toMatch(/busy \? "animate-spin" : ""/);
  });

  it("两页都接了这个共享按钮（否则不一致的问题没真正消掉）", () => {
    for (const page of ["UsagePage.tsx", "CodexSessionsPage.tsx"]) {
      expect(
        read("src", "pages", page),
        `${page} 应当用 RefreshButton`,
      ).toContain("<RefreshButton");
    }
  });
});
