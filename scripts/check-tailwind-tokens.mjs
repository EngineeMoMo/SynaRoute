// 找「代码里写了、但构建产物里一条 CSS 都没生成」的 Tailwind 颜色类。
//
// ## 为什么这样验，而不是比对 tailwind.config.js
//
// Tailwind 对**未注册**的 token 既不报错也不生成 CSS：`bg-surface-elevated`、`bg-accent/10`
// 这类写法在编辑器里正常、构建也通过，只是那条规则**一条 CSS 都不生成**，界面上是「无底色」。
// 本项目已栽过两次（`bg-warning/8` 透明度不在标度、`bg-bg`/`bg-surface-elevated` token 不存在）。
//
// 直接比对配置很容易误报：`theme.extend.colors` 是**追加**到 Tailwind 默认色板上的，
// 故 `gray-100` 合法；`border-b` / `divide-y` / `bg-gradient-to-r` 更不是颜色类。
// 所以这里改用**产物判据**（也是 CLAUDE.md 记的那条）：构建后去 dist CSS 里搜类名，
// 搜不到就是真的没生成。判错的可能性只剩「这个类根本不是颜色类」，而那不影响结论
// —— 不生成 CSS 的类名本来就都该被看一眼。
import { readFileSync, readdirSync, existsSync } from "node:fs";
import { inspectableLines } from "./lib/rust-source.mjs";
import { join, sep } from "node:path";

const DIST = "dist/assets";
if (!existsSync(DIST)) {
  console.error(`找不到 ${DIST} —— 请先 \`npm run build\`（本检查依赖构建产物）`);
  process.exit(2);
}
const css = readdirSync(DIST)
  .filter((f) => f.endsWith(".css"))
  .map((f) => readFileSync(join(DIST, f), "utf8"))
  .join("\n");

function walk(d, acc = []) {
  for (const e of readdirSync(d, { withFileTypes: true })) {
    const p = join(d, e.name);
    if (e.isDirectory()) walk(p, acc);
    // 跳过 i18n 字典：里面是自然语言文案，`top-to-bottom` 这类连字符短语会被
    // 正则当成 `to-bottom` 类名而误报。文案里不含 className，漏扫无代价。
    else if (/\.tsx?$/.test(e.name) && !/i18n\.ts$/.test(e.name)) acc.push(p);
  }
  return acc;
}

// 只看会产出颜色规则的属性前缀（含可选的 /透明度 与状态前缀 hover: 等）
const RE =
  /\b(?:[a-z-]+:)*(bg|text|border|ring|from|to|via|fill|stroke|divide|placeholder|decoration|outline|shadow|accent|caret)-([a-z][a-z0-9]*(?:-[a-z0-9]+)*)(?:\/(\d+))?(?=["'\s`}\]])/g;

// CSS 里类名要转义特殊字符：`/`（透明度）与 `:`（变体前缀 dark:/hover: 都编进类名本身）
const escapeForCss = (cls) => cls.replace(/[/:]/g, (c) => `\\${c}`);

const missing = new Map();
for (const f of walk("src")) {
  // 🔴 **剥注释后再扫**：判据说「代码里别这么写」，就只能看代码。
  // 原先逐行裸扫，于是一句「别用 `bg-surface-subtle`，它不存在」的**注释本身**
  // 被报成「写了但不生效」—— 把坑记进注释这个动作恰好触发了那个坑的判据。
  // 判据收在 lib/rust-source.mjs（与 check-forbidden 共用，别再抄第二份）。
  inspectableLines(readFileSync(f, "utf8"), f)
    .forEach(({ n: lineNo, text: line }) => {
      for (const m of line.matchAll(RE)) {
        // **带变体前缀的类名整体**去查（`dark:bg-gray-100` 在 CSS 里是 `.dark\:bg-gray-100`）。
        // 早先剥掉前缀只查基础类，会把「只在 dark 下用到」的类误报成零 CSS ——
        // Tailwind 按**实际出现的字面量**生成，不会额外生成无前缀版本。
        const cls = m[0];
        if (css.includes(`.${escapeForCss(cls)}`)) continue; // 生成了，正常
        if (!missing.has(cls)) missing.set(cls, []);
        missing.get(cls).push(`${f.split(sep).join("/")}:${lineNo}`);
      }
    });
}

if (missing.size === 0) {
  console.log("✅ 所有颜色类在构建产物里都有对应 CSS（无静默零 CSS）");
} else {
  console.log(`❌ ${missing.size} 个类名在 dist CSS 里查无此规则（写了但不生效）：\n`);
  for (const [cls, locs] of [...missing].sort()) {
    console.log(`  ${cls}`);
    console.log(`     ${locs.slice(0, 6).join("  ")}${locs.length > 6 ? `  …共 ${locs.length} 处` : ""}`);
  }
  process.exitCode = 1;
}
