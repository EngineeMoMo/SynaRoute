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
import {
  existsSync,
  readdirSync,
  readFileSync,
  mkdtempSync,
  rmSync,
  statSync,
} from "node:fs";
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

  // 🔴 数据目录**目前无法隔离** —— 这决定了本门只能在 CI（干净环境）跑。
  //
  // 实测（2026-08-23 第一次真跑）：不隔离时那条自检行写的是
  //   `配置=%APPDATA%\SynaRoute\config.json · keys=13`
  // —— 冒烟实例读的是**用户真实配置**。而真实配置里 `proxyRunningCategories` 非空，
  // 于是它开机会自动启动代理并顺带写工具配置（CLAUDE.md：起=写、停=还原）。
  // 那次没造成损害纯属运气：脚本一看到自检行就收尾，早于代理自启动写完
  // （已核对：`claude_desktop_config.json` mtime 仍是一个多月前、无 `.synaroute-created`）。
  // 慢一点、或用户那条分类是 claude-cli，就会把 `~/.claude/settings.json` 改写成指向一个
  // 随后被 kill 的临时实例的端口 —— 而「起了没还原」这种状态**不自愈**。
  // 第二重问题：与用户自己那份实例抢同一个代理端口。
  //
  // ❌ 试过并**证伪**的修法：给子进程传 `env: { APPDATA: <临时目录> }`。
  //    `dirs::data_dir()`（store.rs 解析配置路径用的）在 Windows 上走
  //    `SHGetKnownFolderPath(FOLDERID_RoamingAppData)` 这个已知文件夹 API，
  //    **不读 `APPDATA` 环境变量**。改了照样 keys=13。
  //    别再往这个方向试 —— 留一句在这里省下下一个人的半小时。
  //
  // ✅ 真正的修法（未做，见下方 TODO）：生产侧加一个 `SYNAROUTE_DATA_DIR` 覆盖，
  //    同 OmniRoute 的 `DATA_DIR` env（它的 check-pack-boot.mjs 正是靠这个隔离，
  //    注释写着 "DATA_DIR isolated"）。那是一处生产代码改动 + 回归测试，
  //    刻意不塞进这次发版（详见 CLAUDE.md 该条 TODO）。
  //
  // 在那之前：本地一律拒跑（下面的 keys 判据会红），CI 上因环境干净而 keys=0 自然通过。
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

  // 🔴 隔离判据（可证伪）：干净环境里不可能有 Key，故 `keys=` 必须是 0。
  //
  // 这条是**硬失败**而不是警告：一个静默失去隔离的门比一个红的门糟得多 ——
  // 它会一边报绿，一边拿用户的真实配置和真实代理端口做冒烟
  // （见上面那段注释里记的实测：本机跑时这里是 keys=13）。
  // `keys=` 整段缺失只警告：那是自检行格式变了，属于判据自身失效，与隔离无关，
  // 两种失败形态不该共用一个出口。
  const keysMatch = bootLine.match(/keys=(\d+)/);
  if (!keysMatch) {
    console.log("⚠️  启动自检行里没有 keys= 段 —— 该行格式变了？（隔离判据依赖它）");
  } else if (keysMatch[1] !== "0") {
    throw new Error(
      `冒烟实例读到了真实运行数据（keys=${keysMatch[1]}，干净环境应为 0）。\n` +
        `   配置段：${(bootLine.match(/配置=[^·]+/) || ["?"])[0].trim()}\n` +
        `   这台机器上有真实配置，而产物**目前没有数据目录覆盖**（见上方注释：\n` +
        `   dirs::data_dir() 走 Win32 已知文件夹 API，不读 APPDATA），故无法隔离。\n` +
        `   本门在 CI（干净环境）上才能安全跑。要在本机跑，先给生产侧加 SYNAROUTE_DATA_DIR。\n` +
        `   \n` +
        `   ⚠️ 注意：安装与启动这两步**已经做过了**（上面两条 ✅），\n` +
        `      也就是说「装得上、起得来」这个结论本身是成立的，红的只是隔离这一项。`
    );
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
