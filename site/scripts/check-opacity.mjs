/**
 * 判据脚本：源码里用到的每一个 Tailwind 透明度类，产物 CSS 里都必须真的生成了规则。
 *
 * 为什么需要它：Tailwind 3.4 的 `opacity` 标度默认只有 5 的倍数，写 `bg-warning/8`
 * **不报错、也一条 CSS 都不生成** —— 表现是「有边框有字色、独独没底色」，
 * 肉眼很难发现。这个仓库曾有 20 处这么写而长期无人察觉。
 *
 * 跑法：npm run build 之后 node scripts/check-opacity.mjs
 * 退出码非 0 即有类名没生成。
 */
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const assets = join(root, "dist", "assets");

const cssName = readdirSync(assets).find((f) => f.endsWith(".css"));
if (!cssName) {
  console.error("[opacity] 找不到 dist/assets/*.css，先跑 npm run build");
  process.exit(1);
}
const css = readFileSync(join(assets, cssName), "utf8");

// 收集源码里出现的透明度类
const files = [];
(function walk(dir) {
  for (const e of readdirSync(dir)) {
    const p = join(dir, e);
    if (statSync(p).isDirectory()) walk(p);
    else if (/\.(tsx?|css)$/.test(e)) files.push(p);
  }
})(join(root, "src"));

const used = new Set();
for (const f of files) {
  const src = readFileSync(f, "utf8");
  for (const m of src.matchAll(/\b(?:bg|border|text|ring|from|to|via|divide)-[a-z0-9-]+\/\d+\b/g)) {
    used.add(m[0]);
  }
}

/**
 * Tailwind 把类名里的 `/` 转义成 `\/` 写进选择器。
 *
 * 只找**转义后的类名片段**、不要求前面紧跟 `.`：带变体的类在 CSS 里长成
 * `.hover\:bg-black\/70:hover`，要求前导点会把这些全判成缺失（问过一次了）。
 * 转义序列 `\/` 只可能出现在 Tailwind 生成的选择器里，不存在误判。
 */
const missing = [...used].sort().filter((cls) => !css.includes(cls.replace("/", String.raw`\/`)));

console.log(`[opacity] 源码用到 ${used.size} 个透明度类，产物缺失 ${missing.length} 个`);
if (missing.length) {
  console.error(
    missing.map((m) => `  ✗ ${m} —— 该档不在 tailwind.config.js 的 opacity 标度里`).join("\n")
  );
  process.exit(1);
}
