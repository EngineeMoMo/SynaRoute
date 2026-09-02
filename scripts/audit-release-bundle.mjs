// 发布前的**外泄审计**：确认安装包里没有开发机的运行数据、密钥、演示数据。
//
// ## 为什么必须是机械检查
//
// 这类事故一旦发生就是不可撤回的：包已经发出去了。而人工「我记得没加进去」不构成判据 ——
// tauri.conf.json 的 `bundle.resources` 只要多一行、或 dist/ 里混进一份 config.json，
// 打包器就会照样收进去，全程不报错。
//
// ## 判据设计
//
// 查的是**文件内容特征**，不是路径常量。代码里出现 `secrets.enc` 字样是正常的
//（运行时才在 %APPDATA% 下解析），不正常的是出现**真实密钥值**、开发机用户名、
// 或演示数据集里那些假 Key。故对每一类都挑一个「只会出现在内容里、不会出现在代码里」的串。
import { readFileSync, existsSync, readdirSync, statSync } from "node:fs";
import { join } from "node:path";
import { execSync } from "node:child_process";
import { scanRepo } from "./lib/secret-scan.mjs";

const NSIS_DIR = "src-tauri/target/release/bundle/nsis";
const RAW_EXE = "src-tauri/target/release/synaroute.exe";

let failed = 0;
const fail = (msg) => { console.log(`❌ ${msg}`); failed++; };
const pass = (msg) => console.log(`✅ ${msg}`);

// ---- 1. git 不该跟踪运行数据 / 明文密钥 ----
{
  const tracked = execSync("git ls-files", { encoding: "utf8" }).split("\n");
  // `.key.gpg`（GPG 对称加密的签名私钥）是**刻意入库**的，见 secrets/README.md；
  // 明文 `.key` 由 .gitignore 拦着。这里只抓明文与运行数据。
  const bad = tracked.filter((f) =>
    /(^|\/)config\.json$/.test(f) ||
    /(^|\/)secrets\.enc$/.test(f) ||
    // `usage.json` / `usage-keys.json` 是用量运行数据（后者含用户给厂商起的名字、
    // 代表模型与计费倍率）。不含密钥，但同属「开发机的数据不该进包」那一类。
    /(^|\/)usage(-keys)?\.json$/.test(f) ||
    /(^|\/)logs\//.test(f) ||
    /(^|\/)latest\.json$/.test(f) ||
    (/\.key$|\.pem$/.test(f) && !f.endsWith(".gpg")),
  );
  if (bad.length) fail(`git 跟踪了敏感文件：${bad.join(", ")}`);
  else pass("git 未跟踪运行数据 / 明文密钥");
}

// ---- 1b. 仓库内容里不该有私钥材料或签名口令（按内容判，不按文件名） ----
{
  // 🔴 上面第 1 项按**文件名**判，2026-08-14 的泄露正是从那个缝里过去的：
  // 更新签名私钥 + 解密口令贴在 `GITHUB_SECRETS_SETUP.md` 正文里，文件名毫无异样，
  // 而且密钥还包了一层 base64 —— 连内容 grep 都抓不到。判据见 lib/secret-scan.mjs。
  //
  // 主战场是 `npm run check:forbidden`（每次 gates 与 CI 都跑）；这里是**发版前的第二道**。
  // 那次泄露在仓库里躺了 9 天，正因为唯一的门只在发版时跑。
  const { findings, stats } = scanRepo();
  if (stats.scanned === 0) fail("密钥扫描扫到 0 个文件 —— 解析器坏了，本项形同虚设");
  else if (findings.length) {
    for (const f of findings) {
      fail(`${f.file ?? "(仓库)"}${f.line ? `:${f.line}` : ""} ${f.what}`);
    }
  } else {
    pass(`仓库内容无私钥材料 / 签名口令（扫过 ${stats.scanned} 个文件、${stats.b64Runs} 段 base64）`);
  }
}

// ---- 2. bundle 配置不该引用仓库外的资源 ----
{
  const cfg = JSON.parse(readFileSync("src-tauri/tauri.conf.json", "utf8"));
  const b = cfg.bundle ?? {};
  const refs = [
    ...(Array.isArray(b.resources) ? b.resources : Object.keys(b.resources ?? {})),
    ...(b.externalBin ?? []),
  ];
  const outside = refs.filter((r) => r.includes("..") || /^[A-Za-z]:[\\/]/.test(r) || r.startsWith("~"));
  if (outside.length) fail(`bundle 引用了仓库外路径：${outside.join(", ")}`);
  else pass(`bundle.resources/externalBin 无仓库外引用（共 ${refs.length} 项）`);
}

// ---- 3. dist/ 不该含演示数据或运行数据 ----
{
  if (!existsSync("dist")) {
    fail("dist/ 不存在 —— 先 npm run build");
  } else {
    const files = [];
    const walk = (d) => {
      for (const e of readdirSync(d, { withFileTypes: true })) {
        const p = join(d, e.name);
        if (e.isDirectory()) walk(p);
        else files.push(p);
      }
    };
    walk("dist");
    const strays = files.filter((f) => /config\.json$|secrets\.enc$|\.key$|auth\.json$/.test(f));
    if (strays.length) fail(`dist/ 里混入了数据文件：${strays.join(", ")}`);

    const js = files.filter((f) => f.endsWith(".js")).map((f) => readFileSync(f, "utf8")).join("\n");

    // dist 里也要查密钥内容：前端 bundle 是纯文本，任何被误 import 进来的密钥都会明文可见
    // （比二进制更容易泄，因为它连压缩都没有）。判据同第 4 项。
    const distKeyHits = [
      ["minisign 私钥内容", "RWRTY0Iy"],
      ["真实 OpenAI 密钥", "sk-proj-"],
    ].filter(([, n]) => js.includes(n)).map(([label]) => label);
    if (distKeyHits.length) fail(`dist/ 前端 bundle 含密钥内容：${distKeyHits.join("、")}`);

    // 演示数据集（mockData.ts）绝不该进生产包 —— 生产构建应被 vite alias 换成空桩。
    //
    // 判据必须挑**只在 mockData 里出现**的串。第一版用了「厂商1（官方直连）」，那是
    // i18n 的 `editor.namePlaceholder`（输入框灰字提示），**每个包里都有且应该有** ——
    // 结果对一个干净的包报了假警。判错方向在这里代价很高：审计一喊狼来了就会被忽略。
    // 现用三个 mock 独有的锚点：假 cc-switch 库路径、假密钥前缀、导出的桩名。
    const demoMarkers = ["cc-switch.db", "sk-aaaaaaaa", "mockBridge"];
    const leaked = demoMarkers.filter((m) => js.includes(m));
    if (leaked.length) fail(`生产包含演示数据（vite alias 未生效？）：${leaked.join(", ")}`);
    else if (!strays.length && !distKeyHits.length) pass("dist/ 无数据文件、无密钥、无演示数据集");
  }
}

// ---- 4. 产物二进制里不该出现真实密钥内容 ----
{
  // **只审当次版本的产物**：`target/` 里会积压历史版本的包，把它们一起审会让结论含混
  // （旧包干净 ≠ 这次干净，反之亦然）。版本号取自 package.json —— 与 bump 后的实际产物同源。
  const version = JSON.parse(readFileSync("package.json", "utf8")).version;
  const targets = [];
  if (existsSync(RAW_EXE)) targets.push(RAW_EXE);
  if (existsSync(NSIS_DIR)) {
    for (const f of readdirSync(NSIS_DIR)) {
      if (f.endsWith(".exe") && f.includes(version)) targets.push(join(NSIS_DIR, f));
    }
  }
  if (targets.length === 0) {
    fail(`找不到 v${version} 的产物（先 npm run tauri build）`);
  } else if (!targets.some((t) => t !== RAW_EXE)) {
    fail(`只有裸 exe、没有 v${version} 的安装包 —— 交付物缺失`);
  } else {
    // 挑「只会出现在**内容**里」的串：
    // - `RWRTY0Iy` 是 minisign **私钥** base64 的固定前缀（公钥前缀 `RWR` 不同，故不误伤）
    // - `sk-proj-` 是真实 OpenAI 密钥形态
    // - `"entries":{"k` 是 secrets.enc 里 DPAPI 密文表的形态
    //
    // **刻意不查开发机用户名**：Rust 会把 `.cargo/registry` 源码路径嵌进二进制供 panic
    // 消息使用，每个 Rust 程序都有、不含任何用户数据。第一版查了它，对一个干净的包报了
    // 假警。真正该防的是「%APPDATA%\SynaRoute 下那三个文件的**内容**」，上面三条覆盖了。
    const needles = [
      ["minisign 私钥内容", "RWRTY0Iy"],
      ["真实 OpenAI 密钥", "sk-proj-"],
      ["DPAPI 密钥库内容", '"entries":{"k'],
    ];

    for (const t of targets) {
      const buf = readFileSync(t);
      const hay = buf.toString("latin1"); // 逐字节，不做 UTF-8 解码（二进制里混着任意字节）
      const hits = needles.filter(([, n]) => hay.includes(n)).map(([label]) => label);
      const kb = Math.round(statSync(t).size / 1024);
      if (hits.length) fail(`${t}（${kb} KB）含敏感内容：${hits.join("、")}`);
      else pass(`${t}（${kb} KB）无敏感内容`);
    }
  }
}

console.log("");
if (failed) {
  console.log(`❌ 审计未通过：${failed} 项。**不要发布**。`);
  process.exit(1);
}
console.log("✅ 发布前外泄审计全部通过");
