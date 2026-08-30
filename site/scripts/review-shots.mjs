// 一次性：给本地官网拍全页截图，用于人工核对排版。
// 复用 capture-shots.mjs 的 CDP 做法（系统 Chrome + 远程调试，零新增依赖）。
// 产物写到 site/.shots-review/（已在 .gitignore 的点目录规则里，不会进产物）。
import { spawn } from "node:child_process";
import { existsSync, mkdirSync, writeFileSync, rmSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { tmpdir } from "node:os";

const OUT = join(dirname(fileURLToPath(import.meta.url)), "..", ".shots-review");
const BASE = process.env.SITE_URL ?? "http://localhost:1430";
const PORT = 9223;
const WIDTH = 1280;

const PAGES = [
  { path: "/zh", id: "home-zh" },
  { path: "/en", id: "home-en" },
];

const CHROME = [
  "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe",
  "C:\\Program Files (x86)\\Google\\Chrome\\Application\\chrome.exe",
  "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe",
].find(existsSync);

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

class CDP {
  constructor(ws) {
    this.ws = ws;
    this.id = 0;
    this.pending = new Map();
    ws.addEventListener("message", (ev) => {
      const m = JSON.parse(ev.data);
      const p = this.pending.get(m.id);
      if (!p) return;
      this.pending.delete(m.id);
      m.error ? p.reject(new Error(JSON.stringify(m.error))) : p.resolve(m.result);
    });
  }
  send(method, params = {}) {
    const id = ++this.id;
    this.ws.send(JSON.stringify({ id, method, params }));
    return new Promise((res, rej) => {
      this.pending.set(id, { resolve: res, reject: rej });
      setTimeout(() => this.pending.delete(id) && rej(new Error("超时 " + method)), 30000);
    });
  }
  async evaluate(expression) {
    const r = await this.send("Runtime.evaluate", { expression, returnByValue: true, awaitPromise: true });
    if (r.exceptionDetails) throw new Error(r.exceptionDetails.text);
    return r.result.value;
  }
}

if (!CHROME) {
  console.error("找不到 Chrome/Edge");
  process.exit(1);
}
try {
  const r = await fetch(BASE, { signal: AbortSignal.timeout(5000) });
  if (!r.ok) throw new Error("HTTP " + r.status);
} catch (e) {
  console.error(`${BASE} 连不上：${e.message}（先在 site/ 跑 npm run dev）`);
  process.exit(1);
}

mkdirSync(OUT, { recursive: true });
const profile = join(tmpdir(), `synaroute-review-${process.pid}`);
const proc = spawn(
  CHROME,
  [
    "--headless=new",
    `--remote-debugging-port=${PORT}`,
    `--window-size=${WIDTH},900`,
    "--hide-scrollbars",
    "--no-first-run",
    "--no-default-browser-check",
    `--user-data-dir=${profile}`,
    "about:blank",
  ],
  { stdio: "ignore" }
);

try {
  let targets = null;
  for (let i = 0; i < 40; i++) {
    try {
      targets = await (await fetch(`http://127.0.0.1:${PORT}/json/list`)).json();
      if (targets?.length) break;
    } catch {}
    await sleep(250);
  }
  const page = targets.find((t) => t.type === "page");
  const ws = new WebSocket(page.webSocketDebuggerUrl);
  await new Promise((res, rej) => {
    ws.addEventListener("open", res, { once: true });
    ws.addEventListener("error", rej, { once: true });
  });
  const cdp = new CDP(ws);
  await cdp.send("Page.enable");
  await cdp.send("Runtime.enable");

  for (const p of PAGES) {
    for (const theme of ["light", "dark"]) {
      await cdp.send("Emulation.setDeviceMetricsOverride", {
        width: WIDTH,
        height: 900,
        deviceScaleFactor: 1,
        mobile: false,
      });
      /**
       * 🔴 主题必须在**页面加载之前**写进 localStorage。
       *
       * 原先这里是加载完再 `document.documentElement.classList.toggle('dark')` ——
       * 那只改了 CSS，而 `useTheme` 的主题状态是模块级的、只在模块初始化时读一次
       * （`readInitialTheme`）。于是 React 侧仍然是 `light`，Hero / Screenshots /
       * BrainSpotlight 三处**拍到的是浅色产品截图**，配在深色页面上 ——
       * 也就是这个脚本存在的目的（核对深色排版）恰好是它拍不准的那一项。
       * 所以先访问一次拿到同源上下文，写 localStorage，再重新导航。
       */
      await cdp.send("Page.navigate", { url: BASE + p.path });
      await sleep(1200);
      await cdp.evaluate(`localStorage.setItem('synaroute-site-theme', ${JSON.stringify(theme)})`);
      await cdp.send("Page.navigate", { url: BASE + p.path });
      // 等 React 挂载 + 首屏图片
      for (let i = 0; i < 60; i++) {
        if (await cdp.evaluate(`!!document.querySelector('#brain')`)) break;
        await sleep(250);
      }
      // 让所有 reveal 动画立即完成、懒加载图片全部进入加载：滚到底再回顶。
      // ⚠️ 加的类是 `reveal-in`（styles.css 里那个），不是 `is-visible` ——
      // 后者是个**不存在的类名**，加了什么都不会发生。此前它一直「看起来能用」
      // 只是因为下面那趟滚动顺带触发了 IntersectionObserver。
      await cdp.evaluate(`(async () => {
        document.querySelectorAll('.reveal').forEach(e => {
          e.style.animationDelay = '0ms';
          e.classList.add('reveal-in');
        });
        document.querySelectorAll('img[loading="lazy"]').forEach(i => i.loading = 'eager');
        const h = document.body.scrollHeight;
        for (let y = 0; y < h; y += 600) { window.scrollTo(0, y); await new Promise(r => setTimeout(r, 60)); }
        window.scrollTo(0, 0);
        await Promise.all([...document.images].filter(i=>!i.complete).map(i=>new Promise(r=>{i.onload=i.onerror=r})));
      })()`);
      await sleep(700);
      const full = await cdp.evaluate(`document.body.scrollHeight`);
      await cdp.send("Emulation.setDeviceMetricsOverride", {
        width: WIDTH,
        height: Math.min(full, 20000),
        deviceScaleFactor: 1,
        mobile: false,
      });
      await sleep(400);
      const { data } = await cdp.send("Page.captureScreenshot", { format: "jpeg", quality: 80 });
      const file = join(OUT, `${p.id}-${theme}.jpg`);
      writeFileSync(file, Buffer.from(data, "base64"));
      console.log(`  ✓ ${p.id}-${theme}.jpg  (${WIDTH}×${full})`);
    }
  }
  ws.close();
} finally {
  proc.kill();
  await sleep(500);
  try {
    rmSync(profile, { recursive: true, force: true });
  } catch {}
}
console.log(`\n→ ${OUT}`);
