// 构建后处理：GitHub Pages 的 SPA 兜底 + sitemap 生成。
//
// 用 Node 脚本而不是 Vite 插件：两件事都发生在产物目录上，写成脚本更直白，
// 也方便单独跑一次核对结果。
import { copyFileSync, existsSync, writeFileSync, readdirSync, statSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const root = dirname(fileURLToPath(import.meta.url));
const dist = join(root, "..", "dist");

const SITE_URL = "https://synaroute.mofamilys.com";
const LANGS = ["zh", "en"];
/** 与 src/App.tsx 的路由表保持一致；加页面时两处一起改 */
const PATHS = ["", "download", "docs", "docs/cli", "docs/brain", "docs/mcp", "changelog", "privacy", "terms"];

// ---- 1. SPA 兜底 ----
// GitHub Pages 对未知路径直接返回 404.html，不像 Netlify 那样有 rewrite 规则。
// 把 index.html 原样复制成 404.html，深链（/zh/docs/cli）刷新才不会真的 404 ——
// 浏览器拿到的是同一份应用外壳，前端路由再按当前 URL 渲染出正确页面。
const indexHtml = join(dist, "index.html");
if (!existsSync(indexHtml)) {
  console.error("[postbuild] 找不到 dist/index.html，构建可能失败了");
  process.exit(1);
}
copyFileSync(indexHtml, join(dist, "404.html"));
console.log("[postbuild] 已生成 404.html（SPA 深链兜底）");

// ---- 2. sitemap.xml ----
// 每个 URL 都带上另一语言版本的 hreflang，避免中英两版被判为重复内容。
const today = new Date().toISOString().slice(0, 10);
const urls = [];
for (const lang of LANGS) {
  for (const p of PATHS) {
    const loc = `${SITE_URL}/${lang}${p ? `/${p}` : ""}`;
    const alternates = LANGS.map(
      (l) => `    <xhtml:link rel="alternate" hreflang="${l}" href="${SITE_URL}/${l}${p ? `/${p}` : ""}"/>`
    ).join("\n");
    urls.push(
      [
        "  <url>",
        `    <loc>${loc}</loc>`,
        alternates,
        `    <xhtml:link rel="alternate" hreflang="x-default" href="${SITE_URL}/zh${p ? `/${p}` : ""}"/>`,
        `    <lastmod>${today}</lastmod>`,
        // 首页权重最高，文档次之，条款类最低
        `    <priority>${p === "" ? "1.0" : p.startsWith("docs") ? "0.8" : "0.6"}</priority>`,
        "  </url>",
      ].join("\n")
    );
  }
}

const sitemap = `<?xml version="1.0" encoding="UTF-8"?>
<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9" xmlns:xhtml="http://www.w3.org/1999/xhtml">
${urls.join("\n")}
</urlset>
`;
writeFileSync(join(dist, "sitemap.xml"), sitemap, "utf8");
console.log(`[postbuild] 已生成 sitemap.xml（${urls.length} 条 URL）`);

// ---- 3. 产物体检 ----
// 两件事最容易在改完 UI 后被忘掉：截图没重拍、或采集脚本把临时目录留在了 public/。
// 前者让官网展示旧界面，后者会把几十 MB 的浏览器配置目录发布到公网。
// 都不该靠人记得，放在这里每次构建自动查。
const shotsDir = join(dist, "screenshots");
const EXPECTED_SHOTS = ["category", "brain", "logs", "settings", "vendors"].flatMap((id) => [
  `${id}-light.png`,
  `${id}-dark.png`,
]);

if (!existsSync(shotsDir)) {
  console.warn("[postbuild] ⚠ 没有 screenshots 目录 —— 官网会显示「产品截图占位」。采集见 scripts/capture-shots.mjs");
} else {
  const present = new Set(readdirSync(shotsDir));
  const missing = EXPECTED_SHOTS.filter((f) => !present.has(f));
  if (missing.length) {
    console.warn(`[postbuild] ⚠ 缺 ${missing.length} 张截图（会显示占位）：${missing.join(", ")}`);
  } else {
    console.log(`[postbuild] 截图齐全（${EXPECTED_SHOTS.length} 张）`);
  }
}

// 任何以点开头的目录都不该出现在产物里 —— public/ 会被整个复制过来，
// 采集脚本的临时目录一旦建错地方就会随之发布。
const junk = readdirSync(dist).filter((n) => n.startsWith(".") && statSync(join(dist, n)).isDirectory());
if (junk.length) {
  console.error(`[postbuild] ✗ 产物里混入了不该发布的目录：${junk.join(", ")}`);
  process.exit(1);
}
