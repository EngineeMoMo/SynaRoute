// 分区块截图：滚到指定区块顶部，拍一屏。用于人工核对单个区块的排版。
// 全页图太长（10000+ px）不便查看，故按区块切。
import { spawn } from "node:child_process";
import { existsSync, mkdirSync, writeFileSync, rmSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { tmpdir } from "node:os";

const OUT = join(dirname(fileURLToPath(import.meta.url)), "..", ".shots-review");
const BASE = process.env.SITE_URL ?? "http://localhost:1430";
const PORT = 9224;
const W = 1280;
const H = 900;

/** 要拍的区块：CSS 选择器 → 文件名。传 null 表示页面顶部（Hero） */
const TARGETS = [
  { sel: null, id: "01-hero" },
  { sel: "#benefits", id: "02-benefits" },
  { sel: "#brain", id: "03-brain" },
  { sel: "#features", id: "04-features" },
  { sel: "#security", id: "05-security" },
  { sel: "#faq", id: "06-faq" },
];

const CHROME = [
  "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe",
  "C:\\Program Files (x86)\\Google\\Chrome\\Application\\chrome.exe",
  "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe",
].find(existsSync);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

class CDP {
  constructor(ws) {
    this.ws = ws; this.id = 0; this.pending = new Map();
    ws.addEventListener("message", (ev) => {
      const m = JSON.parse(ev.data); const p = this.pending.get(m.id);
      if (!p) return; this.pending.delete(m.id);
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

const lang = process.argv[2] ?? "zh";
const theme = process.argv[3] ?? "light";

mkdirSync(OUT, { recursive: true });
const profile = join(tmpdir(), `synaroute-sec-${process.pid}`);
const proc = spawn(CHROME, [
  "--headless=new", `--remote-debugging-port=${PORT}`, `--window-size=${W},${H}`,
  "--hide-scrollbars", "--no-first-run", "--no-default-browser-check",
  `--user-data-dir=${profile}`, "about:blank",
], { stdio: "ignore" });

try {
  let targets = null;
  for (let i = 0; i < 40; i++) {
    try { targets = await (await fetch(`http://127.0.0.1:${PORT}/json/list`)).json(); if (targets?.length) break; } catch {}
    await sleep(250);
  }
  const ws = new WebSocket(targets.find((t) => t.type === "page").webSocketDebuggerUrl);
  await new Promise((res, rej) => { ws.addEventListener("open", res, { once: true }); ws.addEventListener("error", rej, { once: true }); });
  const cdp = new CDP(ws);
  await cdp.send("Page.enable");
  await cdp.send("Runtime.enable");
  await cdp.send("Emulation.setDeviceMetricsOverride", { width: W, height: H, deviceScaleFactor: 1, mobile: false });
  await cdp.send("Page.navigate", { url: `${BASE}/${lang}` });
  for (let i = 0; i < 60; i++) { if (await cdp.evaluate(`!!document.querySelector('#brain')`)) break; await sleep(250); }
  await cdp.evaluate(`document.documentElement.classList.toggle('dark', ${theme === "dark"})`);
  await cdp.evaluate(`(async () => {
    document.querySelectorAll('.reveal').forEach(e => e.classList.add('is-visible'));
    document.querySelectorAll('img[loading="lazy"]').forEach(i => i.loading = 'eager');
    const h = document.body.scrollHeight;
    for (let y = 0; y < h; y += 600) { window.scrollTo(0, y); await new Promise(r => setTimeout(r, 50)); }
    window.scrollTo(0, 0);
    await Promise.all([...document.images].filter(i=>!i.complete).map(i=>new Promise(r=>{i.onload=i.onerror=r})));
  })()`);
  await sleep(500);

  for (const tg of TARGETS) {
    await cdp.evaluate(
      tg.sel
        ? `window.scrollTo(0, document.querySelector('${tg.sel}').getBoundingClientRect().top + window.scrollY - 8)`
        : `window.scrollTo(0, 0)`
    );
    await sleep(350);
    const { data } = await cdp.send("Page.captureScreenshot", { format: "jpeg", quality: 82 });
    writeFileSync(join(OUT, `${lang}-${theme}-${tg.id}.jpg`), Buffer.from(data, "base64"));
    console.log(`  ✓ ${lang}-${theme}-${tg.id}.jpg`);
  }
  ws.close();
} finally {
  proc.kill();
  await sleep(500);
  try { rmSync(profile, { recursive: true, force: true }); } catch {}
}
