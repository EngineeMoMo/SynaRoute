import { describe, it, expect } from "vitest";
import { readFileSync, readdirSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { execSync } from "node:child_process";

/**
 * 防「静默失效的类名」回归。
 *
 * 本仓已被这类问题咬过两次，共同特征是：**类名写错不报错、只是一条 CSS 都不生成**，
 * 元素照常渲染，缺的那部分视觉（底色 / 动效）悄悄没有，肉眼极难发现。
 *
 * 1. Tailwind 透明度修饰符只认标度内的值 —— `bg-warning/8` 曾全仓 20 处无底色
 *    （已在 tailwind.config.js 补 8 / 12 两档）。
 * 2. `animate-in` / `fade-in-0` / `zoom-in-95` / `slide-in-from-bottom-4` 这套类名
 *    来自 `tailwindcss-animate` 插件，而本仓 `plugins: []` —— 用了就是零 CSS。
 *    现改为在 styles.css 里自建 keyframes。
 *
 * 判据取自**产物 CSS**而不是源码：源码写了什么不代表编译出了什么，这正是两次事故的根因。
 *
 * **本文件刻意放在 `tests/` 而不是 `src/`**：它要用 node:fs 读产物，而 `tsconfig.json`
 * 的 `include` 只有 `src`、且没装 `@types/node` —— 放进 src 会让 `npm run build`
 * （= `tsc && vite build`）因找不到 node 模块类型而直接失败。vitest 默认扫全仓，照样能跑到。
 */

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");

/** 取产物 CSS 全文。没有 dist 时先构建一次（CI 首跑 / clean checkout）。 */
function readBuiltCss(): string {
  const assets = join(ROOT, "dist", "assets");
  const cssFiles = () => {
    try {
      return readdirSync(assets).filter((f) => f.endsWith(".css"));
    } catch {
      return [] as string[];
    }
  };
  let files = cssFiles();
  if (files.length === 0) {
    execSync("npm run build", { cwd: ROOT, stdio: "ignore" });
    files = cssFiles();
  }
  return files.map((f) => readFileSync(join(assets, f), "utf8")).join("\n");
}

/** 转义成 CSS 选择器里的写法：bg-warning/8 → .bg-warning\/8 */
const sel = (cls: string) => "." + cls.replace(/\//g, "\\/");

describe("静默失效的类名（判据取自产物 CSS）", () => {
  const css = readBuiltCss();

  it.each(["bg-warning/8", "bg-danger/8", "bg-primary/8", "bg-warning/12", "bg-danger/12"])(
    "透明度修饰符 %s 生成了真实 CSS 规则",
    (cls) => {
      // 若有人把 tailwind.config.js 里 opacity 的 8 / 12 两档删掉，这里会红。
      expect(css).toContain(sel(cls));
    },
  );

  it.each(["animate-in", "fade-in-0", "zoom-in-95", "slide-in-from-bottom-4"])(
    "入场动效类 %s 生成了真实 CSS 规则",
    (cls) => {
      // 若有人删掉 styles.css 里自建的那段（以为 Tailwind 自带），或改用
      // tailwindcss-animate 的类名却没装插件，这里会红。
      expect(css).toContain(sel(cls));
    },
  );

  it.each(["status-pulse", "status-shake"])("状态动效类 %s 生成了真实 CSS 规则", (cls) => {
    // 这两个是自建类（UI-1），同样不属于 Tailwind 内置——删了 styles.css 里对应段落
    // 就会静默失效：徽标照常渲染、只是不再有脉冲/抖动。
    expect(css).toContain(sel(cls));
  });

  it("渐变色标 to-primary-deep 已在 Tailwind 注册（未注册则零 CSS）", () => {
    // `primary.deep` 若没在 tailwind.config.js 的 colors 里注册，
    // `to-primary-deep` 不报错也不生成任何规则 —— 渐变会退化成「单色到透明」。
    // 实测踩过：改完 config 不重启 dev server 时就是这个表现。
    expect(css).toContain(sel("to-primary-deep"));
  });

  it("动效尊重 prefers-reduced-motion（无障碍要求，不是可选装饰）", () => {
    // 无限循环的脉冲对前庭敏感用户是实际障碍。这条兜底若被删掉，
    // 表现是「功能全对、但开了系统减少动效的用户仍会看到动画」——没人会报这个 bug。
    expect(css).toContain("prefers-reduced-motion");
    // 且必须真的把两个自建动效关掉，不能只写个空媒体查询
    const mq = css.match(/@media\s*\(prefers-reduced-motion:\s*reduce\)\s*\{([\s\S]*?)\}\s*\}/);
    expect(mq, "未找到 prefers-reduced-motion 媒体查询块").not.toBeNull();
    expect(mq![1]).toMatch(/status-pulse/);
    expect(mq![1]).toMatch(/status-shake/);
  });

  it("动效类引用的 keyframes 都真的有定义（不能只引用不定义）", () => {
    // 从 CSS 里把 animation-name 引用到的自建动效名抓出来，逐个确认有对应 @keyframes。
    // 名字打错时（typo）类名与规则都在、动画却不会跑——这正是最难肉眼发现的形态。
    //
    // 正则**不能假设 `{` 前后有空格**：产物 CSS 是压缩过的（`.fade-in-0{animation-name:x}`），
    // 按未压缩格式写的模式会一条都匹配不到，于是「零引用」被误读成「全都合规」而恒绿。
    // 这个测试自己就先踩过一次，故此处显式断言引用数下限。
    const referenced = new Set(
      [...css.matchAll(/animation-name:\s*(synaroute-[a-z-]+)/g)].map((m) => m[1]),
    );
    // 三个自建动效各至少被一个类引用；抓不到就说明自建段被删了或模式失配。
    expect(referenced.size).toBeGreaterThanOrEqual(3);

    // 定义侧要**精确整名**比对，不能用 `toContain("@keyframes " + name)`：
    // 名字打错成 `synaroute-fade-slide-up-TYPO` 时，它把正确名当前缀包含，
    // 子串断言照样通过 —— 缺失的定义会被判成存在（此处踩过一次）。
    const defined = new Set(
      [...css.matchAll(/@keyframes\s+([A-Za-z0-9_-]+)/g)].map((m) => m[1]),
    );
    for (const name of referenced) {
      expect(defined.has(name), `${name} 被引用但没有 @keyframes 定义`).toBe(true);
    }
  });

  it("源码里用到的 /N 透明度修饰符都落在 Tailwind 标度内", () => {
    // 全仓扫源码，收集 bg-/text-/border- 等后带 /数字 的类名，
    // 逐个确认它是 5 的倍数或 tailwind.config.js 显式补过的档位。
    const allowed = new Set([0, 5, 8, 12, 100]);
    const files: string[] = [];
    const walk = (dir: string) => {
      for (const e of readdirSync(dir, { withFileTypes: true })) {
        const p = join(dir, e.name);
        if (e.isDirectory()) walk(p);
        else if (/\.tsx?$/.test(e.name)) files.push(p);
      }
    };
    walk(join(ROOT, "src"));

    const offenders: string[] = [];
    for (const f of files) {
      const src = readFileSync(f, "utf8");
      for (const m of src.matchAll(/\b(?:bg|text|border|ring|from|to)-[a-z-]+\/(\d{1,3})\b/g)) {
        const n = Number(m[1]);
        if (n % 5 !== 0 && !allowed.has(n)) offenders.push(`${f}: ${m[0]}`);
      }
    }
    expect(offenders).toEqual([]);
  });
});
