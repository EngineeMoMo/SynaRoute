// 预渲染（SSG）：把每条客户端路由渲染成带完整正文的静态 HTML。
//
// 为什么要它：官网是纯客户端渲染（CSR），产物 index.html 的 <body> 只有
// <div id="root">，正文全靠 JS 跑出来。Googlebot 虽然能执行 JS，但渲染是**第二阶段、
// 单独排队**——它抓到空壳、判定"内容太薄"，就会落进「已抓取-尚未编入索引」，
// 一个曾经排第一的页也会因此掉出索引。给每条路由预生成带正文的 HTML，
// Google 一抓就是实打实的内容，不依赖跑 JS。SPA 仍在其上启动，用户拿到完整交互。
//
// 为什么走 CDP 连系统 Chrome、而不是 renderToString / vite-react-ssg / puppeteer：
//   1) 零新增依赖、零 React 代码改动——本仓既定做法（见 capture-shots.mjs），
//      renderToString 要把 BrowserRouter 换 StaticRouter、还要给 detectLang/useTheme/
//      matchMedia/localStorage 逐个补 Node 守卫，牵动全站、风险大；
//   2) 真浏览器里 window/localStorage/DOMParser/useSeo 的 useEffect 全部照常执行——
//      逐页的 title / description / canonical / hreflang 会被真实写进 <head>，
//      而 renderToString **不跑 useEffect**，那些 meta 一个都不会出现。
//
// 用法：`npm run build` 会在 postbuild 之后自动跑；也可单独 `node scripts/prerender.mjs`。
import { spawn } from "node:child_process";
import { createServer } from "node:http";
import {
  existsSync,
  readFileSync,
  mkdirSync,
  writeFileSync,
} from "node:fs";
import { dirname, join, extname } from "node:path";
import { fileURLToPath } from "node:url";
import { tmpdir } from "node:os";

const root = dirname(fileURLToPath(import.meta.url));
const dist = join(root, "..", "dist");

// 与 scripts/postbuild.mjs 的 LANGS / PATHS 保持一致；加页面时两处一起改。
// （两份清单本可共享，但 postbuild 是纯产物处理、prerender 要起浏览器，
//  合并会把「不需要浏览器的那步」也拖上这套重机制，故各留一份、靠这行注释钉住。）
const LANGS = ["zh", "en"];
const PATHS = [
  "",
  "download",
  "docs",
  "docs/cli",
  "docs/brain",
  "docs/mcp",
  "changelog",
  "privacy",
  "terms",
];

// 端口按 pid 派生，避免与 capture-shots(9222) 或另一个并行构建打架。
const DEBUG_PORT = 9333 + (process.pid % 200);
const SERVE_PORT = 9533 + (process.pid % 200);

const CHROME_CANDIDATES = [
  process.env.CHROME_PATH,
  process.env.PUPPETEER_EXECUTABLE_PATH,
  // Windows
  "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe",
  "C:\\Program Files (x86)\\Google\\Chrome\\Application\\chrome.exe",
  "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe",
  // Linux（GitHub ubuntu runner 预装 google-chrome-stable）
  "/usr/bin/google-chrome",
  "/usr/bin/google-chrome-stable",
  "/usr/bin/chromium-browser",
  "/usr/bin/chromium",
  // macOS
  "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
].filter(Boolean);

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// ---------- 极简 CDP 客户端（与 capture-shots.mjs 同一份手法） ----------
class CDP {
  constructor(ws) {
    this.ws = ws;
    this.id = 0;
    this.pending = new Map();
    ws.addEventListener("message", (ev) => {
      const msg = JSON.parse(ev.data);
      const p = this.pending.get(msg.id);
      if (!p) return;
      this.pending.delete(msg.id);
      msg.error ? p.reject(new Error(JSON.stringify(msg.error))) : p.resolve(msg.result);
    });
  }
  send(method, params = {}) {
    const id = ++this.id;
    this.ws.send(JSON.stringify({ id, method, params }));
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      setTimeout(() => {
        if (this.pending.delete(id)) reject(new Error(`CDP 超时: ${method}`));
      }, 30000);
    });
  }
  async evaluate(expression) {
    const r = await this.send("Runtime.evaluate", {
      expression,
      returnByValue: true,
      awaitPromise: true,
    });
    if (r.exceptionDetails) throw new Error(r.exceptionDetails.text ?? "页面求值异常");
    return r.result.value;
  }
}

// ---------- 本地静态服务器（带 SPA 兜底） ----------
// Chrome 要访问 /zh、/zh/docs/cli 这类客户端路由，而 dist 里此刻还没有对应文件；
// 命中不到文件就回落 index.html（原始外壳），让 SPA 启动、前端路由再渲染出正确页面。
// 预渲染结果**全部收在内存里、Chrome 关掉后一次性写盘**——边渲染边写会让后一条路由的
// 兜底请求命中前一条刚写的文件，而不是外壳。
const MIME = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".mjs": "text/javascript; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".json": "application/json; charset=utf-8",
  ".svg": "image/svg+xml",
  ".png": "image/png",
  ".jpg": "image/jpeg",
  ".jpeg": "image/jpeg",
  ".webp": "image/webp",
  ".gif": "image/gif",
  ".ico": "image/x-icon",
  ".xml": "application/xml; charset=utf-8",
  ".txt": "text/plain; charset=utf-8",
  ".woff2": "font/woff2",
  ".woff": "font/woff",
};

function startServer(shellHtml) {
  const server = createServer((req, res) => {
    try {
      const urlPath = decodeURIComponent((req.url || "/").split("?")[0].split("#")[0]);
      // 归一：去掉前导斜杠，防 `..` 逃出 dist
      const rel = urlPath.replace(/^\/+/, "").replace(/\\/g, "/");
      if (rel.includes("..")) {
        res.writeHead(400).end("bad path");
        return;
      }
      const filePath = rel ? join(dist, rel) : join(dist, "index.html");
      // 真实存在、且带扩展名的路径直接服务（index.html / assets / sitemap.xml…）；
      // 无扩展名的客户端路由（/zh、/zh/docs/cli）落到下面的 SPA 兜底。
      if (rel && existsSync(filePath) && extname(filePath) !== "") {
        const buf = readFileSync(filePath);
        res.writeHead(200, { "Content-Type": MIME[extname(filePath)] ?? "application/octet-stream" });
        res.end(buf);
        return;
      }
      // 客户端路由（/zh、/zh/docs/cli…）无扩展名、无对应文件 → 回落 SPA 外壳
      res.writeHead(200, { "Content-Type": "text/html; charset=utf-8" });
      res.end(shellHtml);
    } catch (e) {
      res.writeHead(500).end(String(e?.message ?? e));
    }
  });
  return new Promise((resolve) => {
    server.listen(SERVE_PORT, "127.0.0.1", () => resolve(server));
  });
}

async function main() {
  const shellPath = join(dist, "index.html");
  if (!existsSync(shellPath)) {
    console.error("[prerender] 找不到 dist/index.html —— 先跑 vite build");
    process.exit(1);
  }
  const shellHtml = readFileSync(shellPath, "utf8");

  const chrome = CHROME_CANDIDATES.find(existsSync);
  if (!chrome) {
    // fail-loud：静默跳过等于把站点原样退回 CSR，正是本脚本要修的那个坑。
    console.error("[prerender] 找不到 Chrome/Edge/Chromium。");
    console.error("  探测过：" + CHROME_CANDIDATES.join(" , "));
    console.error("  设 CHROME_PATH 指向浏览器可执行文件后重试。");
    process.exit(1);
  }

  const server = await startServer(shellHtml);
  const profileDir = join(tmpdir(), `synaroute-prerender-${process.pid}`);
  const proc = spawn(
    chrome,
    [
      "--headless=new",
      `--remote-debugging-port=${DEBUG_PORT}`,
      "--no-first-run",
      "--no-default-browser-check",
      "--no-sandbox", // CI（Linux root）下必需；本机无害
      "--disable-gpu",
      `--user-data-dir=${profileDir}`,
      "about:blank",
    ],
    { stdio: "ignore", detached: false }
  );

  const rendered = new Map(); // route → html
  try {
    // 等调试端口就绪
    let targets = null;
    for (let i = 0; i < 60; i++) {
      try {
        targets = await (await fetch(`http://127.0.0.1:${DEBUG_PORT}/json/list`)).json();
        if (targets?.length) break;
      } catch {
        /* 还没起来 */
      }
      await sleep(250);
    }
    const page = targets?.find((t) => t.type === "page");
    if (!page) throw new Error("拿不到 Chrome 调试目标");

    const ws = new WebSocket(page.webSocketDebuggerUrl);
    await new Promise((res, rej) => {
      ws.addEventListener("open", res, { once: true });
      ws.addEventListener("error", rej, { once: true });
    });
    const cdp = new CDP(ws);
    await cdp.send("Page.enable");
    await cdp.send("Runtime.enable");

    for (const lang of LANGS) {
      for (const p of PATHS) {
        const route = `/${lang}${p ? `/${p}` : ""}`;
        const url = `http://127.0.0.1:${SERVE_PORT}${route}`;
        await cdp.send("Page.navigate", { url });

        // 等 React 挂载出内容：#root 有子节点即算渲染出结构。
        let ok = false;
        for (let i = 0; i < 80; i++) {
          const ready = await cdp.evaluate(
            `(() => { const r = document.getElementById('root'); return !!r && r.childElementCount > 0; })()`
          );
          if (ready) {
            ok = true;
            break;
          }
          await sleep(150);
        }
        if (!ok) throw new Error(`${route} 渲染超时：#root 一直是空的（线上 JS 可能坏了）`);

        // 让 useSeo 的 useEffect、markdown 渲染、release 兜底都落定
        await sleep(500);

        // 强制显形：.reveal 初始 opacity:0，滚动进视口才淡入。预渲染不滚动，
        // 首屏以下会停在透明态——内容在 DOM 里（Google 读得到），但索性显形，
        // 避免任何"内容不可见"的判定风险。这正是 useReveal 在无 IO 时的兜底行为。
        await cdp.evaluate(`(() => {
          document.querySelectorAll('.reveal').forEach((el) => {
            el.classList.remove('reveal');
            el.classList.add('reveal-in');
          });
        })()`);

        const html = await cdp.evaluate(
          `'<!DOCTYPE html>\\n' + document.documentElement.outerHTML`
        );

        // 落地判据：渲染后的页面必须含 <main（LangLayout 渲染出的正文容器），
        // 外壳里没有它——抓到空壳会在这里当场报错，而不是发布一个还是 CSR 的站。
        if (typeof html !== "string" || !html.includes("<main")) {
          throw new Error(`${route} 抓到的 HTML 里没有 <main，疑似只有外壳`);
        }
        rendered.set(route, html);
        console.log(`  ✓ ${route} (${(html.length / 1024).toFixed(0)} KB)`);
      }
    }
    ws.close();
  } finally {
    proc.kill();
    await sleep(300);
    server.close();
  }

  // Chrome 关掉后一次性写盘，**每条路由写两份**：扁平 `<route>.html` + 目录
  // `<route>/index.html`。
  //
  // 为什么两份都写：sitemap 与 canonical / hreflang 全是**不带尾斜杠**的 URL
  // （postbuild.mjs 与 useSeo.ts 生成的就是 /zh、/zh/docs/cli），要保证 Google 抓
  // 那条 URL 时**直接 200、不 404、不 301**。而 GitHub Pages 对无扩展名请求
  // `/zh/docs/cli` 的解析规则我们没有在这个环境实测过——只写一份就得赌它命中哪个。
  // 两份都写则**无论它把 `/zh/docs/cli` 解析成 `.html` 还是 `/index.html`，都必命中
  // 一个真实文件、返 200**，不再依赖那个未验证的解析行为。这是「不 404」从"很可能"
  // 变成"确定"的关键——本轮要修的就是 404，不该在收尾处留一个赌注。
  //
  // 代价只是产物里多一份同名 HTML（纯文本，全站合计几百 KB），而 canonical 统一
  // 指向不带斜杠的版本，两份指向同一规范网址、不构成重复内容。
  //
  // 根 index.html 与 404.html **不动**：前者是 `/` → /zh 的客户端重定向外壳，
  // 后者是深链兜底，都要保持 SPA 外壳。
  for (const [route, html] of rendered) {
    const rel = route.replace(/^\/+/, "");
    // 扁平：dist/zh/docs/cli.html
    const flat = join(dist, rel + ".html");
    mkdirSync(dirname(flat), { recursive: true });
    writeFileSync(flat, html, "utf8");
    // 目录：dist/zh/docs/cli/index.html
    const dir = join(dist, rel, "index.html");
    mkdirSync(dirname(dir), { recursive: true });
    writeFileSync(dir, html, "utf8");
  }
  console.log(
    `[prerender] 完成，共 ${rendered.size} 条路由 × 2 份（<route>.html + <route>/index.html）`
  );
}

main().catch((e) => {
  console.error("[prerender] 失败:", e.message);
  process.exit(1);
});
