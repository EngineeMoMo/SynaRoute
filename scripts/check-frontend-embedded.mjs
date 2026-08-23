// **前端是否真的嵌进产物**：把 CLAUDE.md 里那条人工判据变成机械检查。
//
// ## 为什么这条必须机械化
//
// CLAUDE.md 的构建硬规则写着：
//
//   > 生产 exe 必须用 `tauri build`，禁止裸 `cargo build --release`。
//   > 裸 cargo build 不会嵌入前端资源，产出的 exe 运行时去连 localhost:1420（devUrl），
//   > 生产环境无 dev server → ERR_CONNECTION_REFUSED，界面打不开。
//   > **部署前必须用可证伪证据验证前端已嵌入**：`dist/assets/` 的 chunk 名要能在产物 exe
//   > 里 `grep -c` 到（> 0）。裸 cargo build 产物该值为 0。
//
// 判据早就定义好了，只是**靠人执行**。而这类失败的形态是「exe 跑起来一片空白」——
// 装完打开才发现，那时包已经发出去了。
//
// 借鉴 OmniRoute 的 `check-pack-boot.mjs`：它的存在理由是「连续三个版本发出了开机就崩的包，
// 因为**从来没有任何门执行过那个产物**」。这里是同一件事的静态版（内容判据），
// 动态版见 `scripts/smoke-installer.mjs`（真装真启动）。
//
// ## 时序要求
//
// 必须在 `npm run tauri build` **之后**、且 `dist/` 未再被重建时运行。
// 若中间又跑了一次 `npm run build`（chunk 名带内容哈希，会变），
// 本检查会因为「新 dist 的名字不在旧 exe 里」而报红 —— 那不是误报，
// 而是如实指出「这个 exe 不是当前 dist 打出来的」。
import { readFileSync, readdirSync, existsSync, statSync } from "node:fs";
import { join } from "node:path";

const DIST = "dist/assets";
const TARGET = "src-tauri/target";

/**
 * 找产物二进制。必须**target 感知**：release 流程里 macOS 走 `--target aarch64-apple-darwin`，
 * 产物落在 `target/<triple>/release/` 而不是 `target/release/`。
 * 第一版只写死了三条路径，在带 `--target` 的构建上会「找不到产物 → exit 2」——
 * 那会让这个门在最需要它的那条流水线上恒红（而恒红的门迟早被注掉，等于没有）。
 *
 * 可用 `--exe <path>` 显式指定，跳过搜索。
 */
function findBinaries() {
  const idx = process.argv.indexOf("--exe");
  if (idx >= 0 && process.argv[idx + 1]) return [process.argv[idx + 1]];

  const names = [
    "synaroute.exe",
    "synaroute",
    join("bundle", "macos", "SynaRoute.app", "Contents", "MacOS", "SynaRoute"),
  ];
  const releaseDirs = [];
  if (existsSync(TARGET)) {
    for (const e of readdirSync(TARGET, { withFileTypes: true })) {
      if (!e.isDirectory()) continue;
      if (e.name === "release") releaseDirs.push(join(TARGET, "release"));
      // `target/<triple>/release`（带 --target 的构建）
      else if (existsSync(join(TARGET, e.name, "release")))
        releaseDirs.push(join(TARGET, e.name, "release"));
    }
  }
  const out = [];
  for (const d of releaseDirs) {
    for (const n of names) {
      const p = join(d, n);
      // 目录也可能同名（`target/release/synaroute` 在某些 target 下是构建目录），故要 isFile
      if (existsSync(p) && statSync(p).isFile()) out.push(p);
    }
  }
  return out;
}

if (!existsSync(DIST)) {
  console.error(`❌ 找不到 ${DIST} —— 先 \`npm run build\``);
  process.exit(2);
}
const bins = findBinaries();
if (bins.length === 0) {
  console.error(
    `❌ 在 ${TARGET}/**/release/ 下找不到产物二进制。\n` +
      `   先 \`npm run tauri build\`（或 \`-- --no-bundle\` 只出 exe），或用 --exe <path> 指定。`
  );
  process.exit(2);
}

// 取 dist 里的 chunk 文件名（含内容哈希，唯一性足够做判据）
const chunks = readdirSync(DIST).filter((f) => /\.(js|css)$/.test(f));
if (chunks.length === 0) {
  console.error(`❌ ${DIST} 里没有 js/css chunk —— 前端构建产物不完整`);
  process.exit(2);
}

// 找到多个产物（多 target 构建）时**逐个都查**：只要有一个没嵌入就判失败。
// 「其中一个装上去是空白界面」不因为「另一个是好的」而变成可接受。
let failed = false;
for (const exe of bins) {
  // 二进制里搜的是**文件名字符串**，故按 latin1 读（逐字节 → 逐字符，不做 UTF-8 解码）。
  // 用 utf8 读会把非法字节替换成 U+FFFD，可能吃掉紧邻的 ASCII 名字。
  const bin = readFileSync(exe, "latin1");
  const sizeMb = (statSync(exe).size / 1024 / 1024).toFixed(1);

  const found = chunks.filter((c) => bin.includes(c));
  const missing = chunks.filter((c) => !bin.includes(c));

  console.log(`\n产物：${exe}（${sizeMb} MB）`);
  console.log(`dist chunk：${chunks.length} 个，命中 ${found.length} 个`);

  if (found.length === 0) {
    console.error(
      `❌ 一个 chunk 名都没嵌进去（判据值 = 0）。两种可能，都必须处理：\n` +
        `   1. 这个 exe 是裸 \`cargo build --release\` 产出的 —— 它运行时会去连 devUrl，界面打不开；\n` +
        `   2. 这个 exe 是**旧的**，而 dist/ 之后又被重建过（chunk 名带内容哈希）。\n` +
        `   正确做法：\`npm run tauri build\`，然后立刻跑本检查。\n` +
        `   未命中的名字（前 5 个）：\n${missing.slice(0, 5).map((c) => "     " + c).join("\n")}`
    );
    failed = true;
    continue;
  }
  if (missing.length) {
    // 部分命中：入口 html 引用到的 chunk 一定在，按需分包的可能被 Tauri 资源打包器
    // 以不同形式收录。故不判失败，但如实说出来 —— 沉默的部分命中比报错更危险。
    console.log(
      `⚠️  有 ${missing.length} 个 chunk 名没在二进制里搜到（部分命中通常正常，但值得看一眼）：\n` +
        missing.slice(0, 5).map((c) => "     " + c).join("\n")
    );
  }
  console.log(`✅ 前端已嵌入（判据值 ${found.length} > 0）`);
}
if (failed) process.exit(1);
console.log(`\n✅ ${bins.length} 个产物全部嵌入了前端`);
