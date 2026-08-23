// **安装器冒烟**：静默装 NSIS 包 → 启动 exe → 等它自证「我起来了」→ 收尾。
//
// ## 为什么必须有这个门
//
// CLAUDE.md 里挂着一条已经悬了很久的话：
//
//   > v0.1.6 已 bump 并出包……**仍未验证：NSIS 安装器能否装成**
//   > （历史上静默安装卡死过两次，此前部署都是绕过安装器直接覆盖 exe；
//   >  F 盘现在放的也是直接覆盖的）。
//
// 也就是说：**交付给用户的那个安装包，从来没有任何流程执行过它**。
// OmniRoute 的 `check-pack-boot.mjs` 是为一模一样的事故建的 —— 他们连续三个版本发出了
// 开机就崩的包，根因写在脚本注释里：「结构检查（check:pack-artifact）验的是列表，不是运行时」。
// 本仓的 `audit-release-bundle.mjs` 也是内容/列表检查，`check-frontend-embedded.mjs` 是静态
// 内容判据 —— 都不执行产物。这个脚本补上「真装真启动」那一环。
//
// ## 启动判据
//
// 不看窗口、不看退出码（GUI 进程正常运行时不退出），而是等**产物自己写出**那行启动自检：
//
//   启动自检 · 配置=… · keys=… · 用户=… · exe=…
//
// 它落在 exe 同级的 `logs\`（刻意不走 %APPDATA%，为的就是躲 MSIX 虚拟化，
// 见 CLAUDE.md「平行宇宙」一节）。这条日志本来就是为「核对每实例实际配置路径」而设的，
// 拿它当 boot 判据不需要新增任何产物侧代码。
//
// 与 OmniRoute 轮询 `/api/monitoring/health` 是同一手法：**等产物自己说话**，
// 而不是靠外部猜它起来了没有。
//
// ## 🔴 为什么本地跑需要显式确认
//
// 静默安装会写 `HKCU\…\Uninstall\SynaRoute` 注册表项，并可能要求关闭正在运行的实例。
// 在**开发机**上跑，就有可能：
//   - 顶掉用户自己那份安装的卸载入口；
//   - 把用户正在用的 SynaRoute 关掉。
// CI 上无所谓（一次性容器），本地必须由人点头。故本地运行要带 `--yes-install-on-this-machine`。
import { existsSync, readdirSync, readFileSync, mkdtempSync, rmSync, statSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { spawn, execFileSync } from "node:child_process";

const NSIS_DIR = "src-tauri/target/release/bundle/nsis";
const BOOT_MARKER = "启动自检";
const INSTALL_TIMEOUT_MS = 180_000; // 历史上静默安装卡死过两次 → 必须有硬超时
const BOOT_TIMEOUT_MS = 120_000;
const POLL_MS = 2_000;

const IN_CI = process.env.CI === "true" || process.env.GITHUB_ACTIONS === "true";
const CONFIRMED = process.argv.includes("--yes-install-on-this-machine");

if (process.platform !== "win32") {
  console.log("⏭️  非 Windows，跳过 NSIS 安装器冒烟");
  process.exit(0);
}
if (!IN_CI && !CONFIRMED) {
  console.error(
    "❌ 本地运行需要显式确认：\n" +
      "     node scripts/smoke-installer.mjs --yes-install-on-this-machine\n\n" +
      "   原因：静默安装会写 HKCU 卸载注册表项、并可能关闭正在运行的 SynaRoute，\n" +
      "   于是可能顶掉你自己那份安装的卸载入口。CI 上无此顾虑（一次性环境）。"
  );
  process.exit(2);
}

// ---- 找安装包（取最新的一个） ----
if (!existsSync(NSIS_DIR)) {
  console.error(`❌ 找不到 ${NSIS_DIR} —— 先 \`npm run tauri build\``);
  process.exit(2);
}
const setups = readdirSync(NSIS_DIR)
  .filter((f) => /-setup\.exe$/i.test(f))
  .map((f) => ({ f, m: statSync(join(NSIS_DIR, f)).mtimeMs }))
  .sort((a, b) => b.m - a.m);
if (setups.length === 0) {
  console.error(`❌ ${NSIS_DIR} 里没有 *-setup.exe`);
  process.exit(2);
}
const setup = join(process.cwd(), NSIS_DIR, setups[0].f);
const expectedVersion = JSON.parse(readFileSync("package.json", "utf8")).version;
if (!setups[0].f.includes(expectedVersion)) {
  // 不判失败：本地可能刻意在测旧包。但必须说出来 —— 「验了一个不是当次版本的包」
  // 是最容易自欺的一种绿。
  console.log(
    `⚠️  取到的安装包是 ${setups[0].f}，而 package.json 版本是 ${expectedVersion} —— 确认这是你要验的那个包`
  );
}
console.log(`安装包：${setup}`);

const tmp = mkdtempSync(join(tmpdir(), "synaroute-smoke-"));
const prefix = join(tmp, "app");
let child = null;
let exitCode = 1;

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/** 在 logs\ 里找启动自检行；返回那一行或 null。 */
function findBootLine(dir) {
  const logs = join(dir, "logs");
  if (!existsSync(logs)) return null;
  for (const f of readdirSync(logs)) {
    let txt;
    try {
      txt = readFileSync(join(logs, f), "utf8");
    } catch {
      continue; // 正在被写、占用中 → 下一轮再看
    }
    const hit = txt.split("\n").find((l) => l.includes(BOOT_MARKER));
    if (hit) return hit.trim();
  }
  return null;
}

try {
  // ---- 1. 静默安装到临时前缀 ----
  // Tauri 的 NSIS 安装器支持 /S（静默）与 /D=<目录>（必须是**最后一个**参数、不加引号）。
  console.log(`静默安装到 ${prefix} …（硬超时 ${INSTALL_TIMEOUT_MS / 1000}s）`);
  execFileSync(setup, ["/S", `/D=${prefix}`], {
    timeout: INSTALL_TIMEOUT_MS,
    stdio: "inherit",
    windowsVerbatimArguments: true,
  });

  const exe = join(prefix, "synaroute.exe");
  if (!existsSync(exe)) {
    // 装完却没有 exe：安装器「成功」返回但什么都没放下来，是最坑的一种失败形态。
    const listing = existsSync(prefix) ? readdirSync(prefix).join(", ") : "(目录都没建)";
    throw new Error(`安装后找不到 ${exe}。目录内容：${listing}`);
  }
  console.log(`✅ 装成了：${exe}`);

  // ---- 2. 启动并等它自证 ----
  console.log(`启动并等启动自检日志 …（超时 ${BOOT_TIMEOUT_MS / 1000}s）`);
  child = spawn(exe, [], { cwd: prefix, detached: true, stdio: "ignore" });
  let died = null;
  child.on("exit", (code) => (died = code ?? -1));

  const deadline = Date.now() + BOOT_TIMEOUT_MS;
  let bootLine = null;
  while (Date.now() < deadline) {
    if (died !== null) throw new Error(`进程在自证之前就退出了（code=${died}）`);
    bootLine = findBootLine(prefix);
    if (bootLine) break;
    await sleep(POLL_MS);
  }
  if (!bootLine) {
    throw new Error(
      `${BOOT_TIMEOUT_MS / 1000}s 内没等到「${BOOT_MARKER}」日志。\n` +
        `   这正是本门要抓的形态：装成了、进程也在，但界面/后端没真正起来。`
    );
  }
  console.log(`✅ 产物自证启动：${bootLine}`);
  // 顺带核对它读的是**exe 同级**的配置，而不是被虚拟化重定向到了别处
  if (!bootLine.includes("exe=")) {
    console.log("⚠️  启动自检行里没有 exe= 段 —— 该行格式变了？（判据依赖它）");
  }
  exitCode = 0;
  console.log("\n✅ 安装器冒烟通过：装得上、起得来");
} catch (e) {
  console.error(`\n❌ 安装器冒烟失败：${e.message}`);
} finally {
  if (child?.pid) {
    try {
      execFileSync("taskkill", ["/PID", String(child.pid), "/T", "/F"], { stdio: "ignore" });
    } catch {
      /* 已经没了 */
    }
  }
  // 卸载器**刻意不跑**：它会动注册表与用户数据目录。这里只删临时前缀。
  // 代价是本机会残留一条 HKCU 卸载项（CI 上无所谓；本地已在入口处告知）。
  await sleep(1_000);
  try {
    rmSync(tmp, { recursive: true, force: true });
  } catch {
    /* 文件可能还被占用，交给系统临时目录清理 */
  }
}
process.exit(exitCode);
