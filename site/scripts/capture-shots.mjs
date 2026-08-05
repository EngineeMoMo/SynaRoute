// 采集官网用的产品截图。
//
// 为什么自己写 CDP 客户端：本机没有 playwright/puppeteer，而只为拍 10 张图去装一个
// 会下载整套浏览器的依赖不划算。系统已装 Chrome，直接用它的远程调试协议即可，
// 零新增依赖。
//
// 图片来源是**应用的浏览器预览模式**（走 src/lib/mockData.ts 的示例数据），
// 界面真实、数据是假的，不存在泄露真实密钥或厂商地址的风险。详见 capture-shots.md。
//
// 用法：
//   1) 先起应用 dev server：npm run dev（仓库根目录，端口 1420）
//   2) node site/scripts/capture-shots.mjs
import { spawn } from "node:child_process";
import { existsSync, mkdirSync, writeFileSync, rmSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { tmpdir } from "node:os";

const OUT_DIR = join(dirname(fileURLToPath(import.meta.url)), "..", "public", "screenshots");
const APP_URL = process.env.APP_URL ?? "http://localhost:1420";
const DEBUG_PORT = 9222;

// 与 site/src/data/screenshots.ts 的 SHOT_WIDTH / SHOT_HEIGHT 保持一致，改了要两边一起改
const WIDTH = 1440;
const HEIGHT = 900;
// 2 倍像素密度：高分屏上不糊。声明尺寸仍按 CSS 像素写在 <img width/height> 上。
const SCALE = 2;

/** 要拍的页面：侧栏按钮的可见文字 → 输出文件名前缀 */
const PAGES = [
  { nav: "Claude CLI", id: "category" },
  { nav: "大脑聚合", id: "brain" },
  { nav: "运行日志", id: "logs" },
  { nav: "厂商管理", id: "vendors" },
  { nav: "设置", id: "settings" },
];

const CHROME_CANDIDATES = [
  "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe",
  "C:\\Program Files (x86)\\Google\\Chrome\\Application\\chrome.exe",
  "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe",
];

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// ---------- 极简 CDP 客户端 ----------
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
  /** 在页面里求值并取回结果（自动展开 Promise） */
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

async function main() {
  const chrome = CHROME_CANDIDATES.find(existsSync);
  if (!chrome) {
    console.error("找不到 Chrome 或 Edge，请手动按 capture-shots.md 采集");
    process.exit(1);
  }

  // 先确认 dev server 真的在跑 —— 否则会拍到一堆连接失败页，比报错更难发现
  try {
    const res = await fetch(APP_URL, { signal: AbortSignal.timeout(5000) });
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
  } catch (e) {
    console.error(`${APP_URL} 连不上：${e.message}`);
    console.error("请先在仓库根目录跑 `npm run dev`，再执行本脚本。");
    process.exit(1);
  }

  mkdirSync(OUT_DIR, { recursive: true });

  // 临时 Chrome 用户目录**必须放在 public/ 之外**。
  // 之前它建在 public/.chrome-capture，Vite 会把 public/ 整个复制进 dist/ ——
  // 于是 24MB 的浏览器配置目录（含 Cookie/缓存数据库）会被一起发布到公网。
  // 放系统临时目录，并在结束时删掉。
  const profileDir = join(tmpdir(), `synaroute-capture-${process.pid}`);

  const proc = spawn(
    chrome,
    [
      "--headless=new",
      `--remote-debugging-port=${DEBUG_PORT}`,
      `--window-size=${WIDTH},${HEIGHT}`,
      "--hide-scrollbars",
      "--no-first-run",
      "--no-default-browser-check",
      // 独立用户目录：避免复用日常配置里的插件/主题影响画面
      `--user-data-dir=${profileDir}`,
      "about:blank",
    ],
    { stdio: "ignore", detached: false }
  );

  try {
    // 等调试端口就绪
    let targets = null;
    for (let i = 0; i < 40; i++) {
      try {
        targets = await (await fetch(`http://127.0.0.1:${DEBUG_PORT}/json/list`)).json();
        if (targets?.length) break;
      } catch {
        /* 还没起来，继续等 */
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
    await cdp.send("Emulation.setDeviceMetricsOverride", {
      width: WIDTH,
      height: HEIGHT,
      deviceScaleFactor: SCALE,
      mobile: false,
    });

    await cdp.send("Page.navigate", { url: APP_URL });
    // 等到侧栏渲染出来才算加载完 —— 单纯等 load 事件时 React 可能还没挂载
    for (let i = 0; i < 60; i++) {
      const ready = await cdp.evaluate(`!!document.querySelector('aside, nav')`);
      if (ready) break;
      await sleep(250);
    }

    // 必须确认是预览模式，否则可能连的是真实后端、画面里有真实数据
    const isMock = await cdp.evaluate(
      `document.body.innerText.includes('浏览器预览模式') || document.body.innerText.includes('browser preview')`
    );
    if (!isMock) {
      console.warn("⚠️ 没检测到「浏览器预览模式」提示条 —— 请人工确认画面里没有真实数据");
    }

    // 首启向导（OnboardingWizard）会盖住整个界面，必须先跳过。
    // 它只在「未完成引导 且 一条 Key 都没有」时出现，mock 数据下会命中。
    const skipped = await cdp.evaluate(`(() => {
      const btn = [...document.querySelectorAll('button')]
        .find(b => ['跳过', 'Skip'].includes(b.textContent.trim()));
      if (!btn) return 'absent';
      btn.click();
      return 'clicked';
    })()`);
    if (skipped === "clicked") {
      await sleep(600);
      const still = await cdp.evaluate(
        `document.body.innerText.includes('欢迎使用') || document.body.innerText.includes('Welcome to')`
      );
      if (still) throw new Error("点了「跳过」但首启向导仍在，会挡住所有截图");
      console.log("  · 已跳过首启向导");
    }

    // 隐藏「浏览器预览模式」角标：它是给开发者看的，出现在官网截图里会让人以为
    // 软件本身带这么一条提示。上面已经用它验过是 mock 数据，验完即可藏。
    await cdp.evaluate(`(() => {
      const s = document.createElement('style');
      s.textContent = '.fixed.bottom-3.left-1\\\\/2 { display: none !important; }';
      document.head.appendChild(s);
    })()`);

    let count = 0;
    for (const theme of ["light", "dark"]) {
      // 直接切 <html> 的 dark 类：这是应用 applyTheme 的最终效果（Tailwind darkMode:class），
      // 比去点具体的主题控件稳 —— 控件长什么样会随 UI 改版变化，这个类不会。
      await cdp.evaluate(
        `document.documentElement.classList.toggle('dark', ${theme === "dark"}); ` +
          `document.documentElement.classList.contains('dark')`
      );

      for (const p of PAGES) {
        const clicked = await cdp.evaluate(`(() => {
          const btns = [...document.querySelectorAll('button, a')];
          const el = btns.find(b => b.textContent.trim() === ${JSON.stringify(p.nav)});
          if (!el) return false;
          el.click();
          return true;
        })()`);
        if (!clicked) {
          console.warn(`  跳过 ${p.id}：侧栏里找不到「${p.nav}」`);
          continue;
        }
        await sleep(900); // 等页面切换与 mock 数据渲染完

        const { data } = await cdp.send("Page.captureScreenshot", { format: "png" });
        const file = join(OUT_DIR, `${p.id}-${theme}.png`);
        writeFileSync(file, Buffer.from(data, "base64"));
        count++;
        console.log(`  ✓ ${p.id}-${theme}.png`);
      }
    }

    ws.close();
    console.log(`\n[capture] 完成，共 ${count} 张 → ${OUT_DIR}`);
  } finally {
    proc.kill();
    // 等浏览器真正退出再删，否则 Windows 上文件仍被占用、删除会失败
    await sleep(500);
    try {
      rmSync(profileDir, { recursive: true, force: true });
    } catch {
      // 删不掉不算失败：它在系统临时目录里，不会进产物
    }
  }
}

main().catch((e) => {
  console.error("[capture] 失败:", e.message);
  process.exit(1);
});
