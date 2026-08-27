#!/usr/bin/env node
/**
 * 官网的策略门。判据必须为 **0**，只能修、不能冻结（与主仓 `check-forbidden.mjs` 同一治理方式）。
 *
 * 为什么官网需要自己的门：主仓的三道门（`check-forbidden` / `check-ratchet` /
 * `check-secrets`）扫描根只有 `src-tauri/src` 与 `src`，**`site/` 在它们之外**。
 * 于是官网上的同类缺陷没有任何机械判据 —— 本轮实测到的两条都属于这种：
 * 文档教用户配一个已被修掉的裸 `/mcp` 地址、四处 JS 平滑滚动绕过了
 * `prefers-reduced-motion`。两者都不报错、不崩，只是静默地做错事。
 *
 * 跑法：`npm run check` （site 目录下），CI 的 gates 里也跑。
 */
import { readFileSync, readdirSync, statSync } from "node:fs";
import { join, dirname, extname } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const SRC = join(ROOT, "src");

function walk(dir, out = []) {
  for (const name of readdirSync(dir)) {
    const p = join(dir, name);
    if (statSync(p).isDirectory()) walk(p, out);
    else out.push(p);
  }
  return out;
}

const FILES = walk(SRC);
const code = FILES.filter((f) => [".ts", ".tsx"].includes(extname(f)));
const markdown = FILES.filter((f) => extname(f) === ".md");

let failed = 0;
const ok = (name, detail) => console.log(`✅ ${name} —— ${detail}`);
const bad = (name, hits, how) => {
  failed++;
  console.log(`❌ ${name}：${hits.length} 处违规`);
  for (const h of hits) console.log(`   ✗ ${h}`);
  console.log(`   → ${how}`);
};

// ---------------------------------------------------------------------------
// 1) JS 平滑滚动必须走 lib/motion.ts
// ---------------------------------------------------------------------------
//
// CSS 那条 `@media (prefers-reduced-motion: reduce) { html { scroll-behavior: auto } }`
// **管不到 JS**：`scrollIntoView({ behavior: "smooth" })` 的 behavior 是显式参数，
// 优先级高于 CSS。四个调用点（顶栏锚点 / 页脚锚点 / 返回顶部 / 路由 hash）此前逐个把
// 那道兜底盖掉了。首页高约 14000px —— 对晕动症用户就是一万像素的强制动画。
{
  const hits = [];
  for (const f of code) {
    if (f.endsWith(join("lib", "motion.ts"))) continue; // 事实来源自己可以写
    const src = readFileSync(f, "utf8");
    src.split("\n").forEach((line, i) => {
      if (line.trim().startsWith("//") || line.trim().startsWith("*")) return;
      if (/behavior:\s*["']smooth["']/.test(line)) {
        hits.push(`${f.slice(ROOT.length + 1)}:${i + 1}  ${line.trim().slice(0, 80)}`);
      }
    });
  }
  if (hits.length) {
    bad(
      "reduced-motion-must-go-through-motion-helper",
      hits,
      "改用 lib/motion.ts 的 scrollToId / scrollToTop（它们会查 prefers-reduced-motion）",
    );
  } else {
    ok("reduced-motion-must-go-through-motion-helper", `查过 ${code.length} 个源文件`);
  }
}

// ---------------------------------------------------------------------------
// 2) 文档里的 MCP 地址必须带分类段
// ---------------------------------------------------------------------------
//
// 分类身份靠 url 路径段携带（`/mcp/<分类>`，见 src-tauri/src/mcp.rs 的 client_url）。
// 照裸 `/mcp` 配的人：连得上、工具能调、但服务端认不出调用方 → 退回兜底分类
// （用错 Key 池、额度记在别的分类、聚合日志落错页），**全程无提示**。
// 这正是应用侧 0.1.33 刚修掉的缺陷 —— 而官网文档还在教旧写法。
{
  const CATEGORIES = ["claude-cli", "codex", "claude-desktop"];
  const hits = [];
  for (const f of markdown) {
    readFileSync(f, "utf8")
      .split("\n")
      .forEach((line, i) => {
        // 只看形如 http://host:port/mcp 的地址；带分类段的放过
        const m = line.match(/https?:\/\/[^\s"'`)]*\/mcp(?![/\w-])/);
        if (!m) return;
        if (CATEGORIES.some((c) => line.includes(`/mcp/${c}`))) return;
        hits.push(`${f.slice(ROOT.length + 1)}:${i + 1}  ${m[0]}`);
      });
  }
  if (hits.length) {
    bad(
      "mcp-url-must-carry-category",
      hits,
      "改成 /mcp/claude-cli 之类带分类段的地址（裸 /mcp 会让服务端认不出调用方）",
    );
  } else {
    ok("mcp-url-must-carry-category", `查过 ${markdown.length} 个文档`);
  }
}

// ---------------------------------------------------------------------------
// 3) 文档不得再提已从 schema 里摘掉的 `category` 参数
// ---------------------------------------------------------------------------
//
// 事实来源：src-tauri/src/mcp.rs 有一条测试断言工具 schema 里 **没有** category。
// 它一在文档里，模型就会去问用户「当前是哪个分类」—— 那正是用户报过的那个 bug。
{
  const hits = [];
  for (const f of markdown) {
    readFileSync(f, "utf8")
      .split("\n")
      .forEach((line, i) => {
        if (/^\s*\|?\s*`?category`?\s*\|/.test(line)) {
          hits.push(`${f.slice(ROOT.length + 1)}:${i + 1}  ${line.trim().slice(0, 80)}`);
        }
      });
  }
  if (hits.length) {
    bad(
      "no-documented-category-param",
      hits,
      "synaroute_ai 的 schema 里已经没有 category 参数（分类由接入时写死），把这一行删掉",
    );
  } else {
    ok("no-documented-category-param", `查过 ${markdown.length} 个文档`);
  }
}

// ---------------------------------------------------------------------------
// 4) zh / en 词条必须对称，且各自无重复键
// ---------------------------------------------------------------------------
//
// 官网没有测试运行器（package.json 里没有 vitest），所以这条判据放在策略门里 ——
// 加一个测试框架只为跑这一条不划算，而不查的代价是实打实的：
// en 缺键 → 英文页露中文；两边都缺 → 直接渲染出 `features.diag.name` 这种原始 key；
// 重复键 → 对象字面量里后者静默覆盖前者，改了前一处会「怎么改都不生效」。
// 三种都不报错、不崩。
{
  const keysOf = (f) =>
    [...readFileSync(f, "utf8").matchAll(/^ {2}"([^"]+)":/gm)].map((m) => m[1]);
  const zh = keysOf(join(SRC, "i18n", "zh.ts"));
  const en = keysOf(join(SRC, "i18n", "en.ts"));
  const zs = new Set(zh);
  const es = new Set(en);
  const dup = (a) => [...new Set(a.filter((k, i) => a.indexOf(k) !== i))];
  const hits = [
    ...zh.filter((k) => !es.has(k)).map((k) => `zh 有、en 缺：${k}（英文页会露中文）`),
    ...en.filter((k) => !zs.has(k)).map((k) => `en 有、zh 缺：${k}`),
    ...dup(zh).map((k) => `zh 重复键：${k}（后者静默覆盖前者）`),
    ...dup(en).map((k) => `en 重复键：${k}`),
  ];
  // 反向判据：解析到 0 个键说明正则坏了
  if (zh.length < 50) {
    bad("i18n-zh-en-parity", [`只解析出 ${zh.length} 个 zh 键 —— 判据在空转`], "先修脚本的正则");
  } else if (hits.length) {
    bad("i18n-zh-en-parity", hits, "补齐缺失的词条 / 删掉重复键");
  } else {
    ok("i18n-zh-en-parity", `zh 与 en 各 ${zh.length} 条，完全对称`);
  }
}

// ---------------------------------------------------------------------------
// 5) 反向判据：门不能空转
// ---------------------------------------------------------------------------
if (code.length < 20 || markdown.length < 5) {
  console.log(
    `❌ 判据在空转：只扫到 ${code.length} 个源文件 / ${markdown.length} 个文档 —— 先修脚本`,
  );
  failed++;
}

console.log(failed === 0 ? "\n✅ 官网策略门全部通过" : `\n❌ 官网策略门：${failed} 项未通过`);
process.exit(failed === 0 ? 0 : 1);
