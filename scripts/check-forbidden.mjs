// **硬规则策略门**：这些判据必须为 0，没有棘轮、没有基线、没有「先冻结以后再修」。
//
// 与 `check-ratchet.mjs` 的分工（借鉴 OmniRoute 把 gate 分成 pass/fail policy 与 ratchet 两类）：
//   - 棘轮：现状不理想但可接受，只要求不变坏（文件行数、裸 persist 计数）；
//   - 策略门：**任何一处都是缺陷**，只能修，不能冻结。
//
// 每条规则都对应 CLAUDE.md 里一条已经用文字写下、但此前**没有机械判据**的铁律。
// 「规则写在文档里 = 建议；有脚本 = 约束」—— 本仓的缺陷史反复证明前者会被跳过。
//
// ## 🔴 写这个脚本时踩到的两个判据坑（都在第一版里）
//
// 1. **路径规则最初裸 grep `C:\Users`**，于是把 11 处**文档示例与测试夹具**全报成违规
//    （`/// Windows: C:\Users\Alice\...`、`let exe = r"C:\Program Files\..."`）。
//    那些不是硬编码本机路径，是在**描述**路径。判据必须先剥掉注释、再排除测试段。
//    这与 CLAUDE.md 里「别用 i18n 占位文案当演示数据的判据」是同一个教训。
// 2. **命令对账最初查 `invoke("...")`，实测命中 0 处** —— 本仓的真实形态是
//    `call<T>("cmd", args, mock)`（bridge.ts 里 73 处），`invoke` 只在一个包装函数里
//    以变量形式出现。一个恒零命中的门是**静默无用**的：它永远绿，永远什么都没测。
//    所以下面对「解析到 0 个」这种情形**主动判失败**，不让它悄悄空转。
import { readFileSync, readdirSync, existsSync } from "node:fs";
import { join, sep } from "node:path";
import { scanRepo } from "./lib/secret-scan.mjs";
// 🔴 「测试段起始行」判据从共享模块导入，**不要**在这里再写一份。
// 本文件原先自带一份收窄版（只认裸 `mod tests`，漏掉 `pub(crate) mod tests`），
// 与 check-ratchet.mjs 那份已经漂移。在**本文件**里那个盲区的失效方向是隐蔽的：
// 测试段会被当成生产段去扫，于是测试夹具里的假路径会被 no-hardcoded-local-paths
// 报成违规 —— 正是下面注释开头记的第一个判据坑。现在没报，只是因为那些夹具里恰好没有 `C:\Users`。
import { testModStartLine } from "./lib/rust-source.mjs";

const SKIP_DIRS = new Set(["node_modules", "target", "dist", ".git"]);

function walk(dir, ext, acc = []) {
  if (!existsSync(dir)) return acc;
  for (const e of readdirSync(dir, { withFileTypes: true })) {
    if (e.isDirectory()) {
      if (!SKIP_DIRS.has(e.name)) walk(join(dir, e.name), ext, acc);
    } else if (ext.test(e.name)) {
      acc.push(join(dir, e.name).split(sep).join("/"));
    }
  }
  return acc;
}

/**
 * 一个文件里「值得检查」的行：生产段（.rs 排除尾部测试模块）里的**非注释**行。
 * 返回 [{ n, text }]（n 为 1-based 行号）。
 */
function inspectableLines(file) {
  const src = readFileSync(file, "utf8");
  const lines = src.split("\n");
  const cut = file.endsWith(".rs") ? testModStartLine(src) : null;
  const limit = cut === null ? lines.length : cut - 1;
  const out = [];
  let inBlockComment = false;
  for (let i = 0; i < limit; i++) {
    const raw = lines[i];
    const t = raw.trim();
    if (inBlockComment) {
      if (t.includes("*/")) inBlockComment = false;
      continue;
    }
    if (t.startsWith("/*")) {
      if (!t.includes("*/")) inBlockComment = true;
      continue;
    }
    // 行注释（Rust `//` `///` `//!`、TS `//`）与块注释续行 `*`
    if (t.startsWith("//") || t.startsWith("*")) continue;
    // 行尾注释也剥掉（避免「代码后面跟一句举例说明路径」被判违规）
    out.push({ n: i + 1, text: raw.replace(/\/\/.*$/, "") });
  }
  return out;
}

const RUST = walk("src-tauri/src", /\.rs$/);
const TS = walk("src", /\.(ts|tsx)$/);

let failed = 0;
const fail = (name, msg) => {
  console.log(`❌ ${name}`);
  console.log(msg);
  failed++;
};
const pass = (name, detail) => console.log(`✅ ${name}${detail ? ` —— ${detail}` : ""}`);

// ---------------------------------------------------------------------------
// 规则 1：禁止硬编码本机路径
// ---------------------------------------------------------------------------
{
  const WHY =
    "      CLAUDE.md 铁律：禁止把本机路径硬编码进代码，路径一律动态解析（面向通用用户）。\n" +
    "      硬编码的后果是「在开发机上一切正常、到用户机器上静默失效」——本仓最贵的那类缺陷。";
  // Windows 盘符下的用户目录 / 安装目录，以及 Git Bash / WSL 形态的同一路径。
  // 刻意**不**查 `C:\Program Files\`：那是标准安装位置，测试里作为夹具出现是合理的，
  // 且生产代码若真需要它也应经 `dirs`/`std::env` 解析（那时不会出现字面量）。
  const RE = /(?:"|'|`|r#?")(?:[A-Za-z]:[\\/]{1,2}Users|\/c\/Users\/|\/mnt\/[a-z]\/Users\/)/;
  // mockData.ts 是浏览器预览用的演示数据（生产构建被 vite alias 换成空桩，见 CLAUDE.md），
  // 里面的假路径是**刻意**的展示内容，不是运行时会用到的路径。
  const SKIP_FILES = new Set(["src/lib/mockData.ts"]);
  const files = [...RUST, ...TS].filter((f) => !SKIP_FILES.has(f));
  const hits = [];
  for (const f of files) {
    for (const { n, text } of inspectableLines(f)) {
      if (RE.test(text)) hits.push(`      ${f}:${n}  ${text.trim().slice(0, 110)}`);
    }
  }
  if (hits.length) fail("no-hardcoded-local-paths", `${WHY}\n${hits.join("\n")}`);
  else pass("no-hardcoded-local-paths", `查过 ${files.length} 个文件的生产段非注释行`);
}

// ---------------------------------------------------------------------------
// 规则 2：前端调的每个后端命令名都必须真的存在
// ---------------------------------------------------------------------------
{
  const WHY =
    '      前端调的命令名必须在 Rust 侧有对应的 #[tauri::command]。\n' +
    "      拼错一个命令名**没有编译错误**，只会在运行时抛「command not found」——\n" +
    "      而那条路径可能要到用户点某个按钮时才走到。（借鉴 OmniRoute 的 check-fetch-targets。）";

  // Rust 侧：`#[tauri::command]` 之后最近的 `fn <name>`
  const declared = new Set();
  for (const f of RUST) {
    const lines = readFileSync(f, "utf8").split("\n");
    for (let i = 0; i < lines.length; i++) {
      if (!/^\s*#\[tauri::command/.test(lines[i])) continue;
      for (let j = i + 1; j < Math.min(i + 8, lines.length); j++) {
        const m = lines[j].match(/^\s*(?:pub\s+)?(?:async\s+)?fn\s+([A-Za-z_]\w*)/);
        if (m) {
          declared.add(m[1]);
          break;
        }
      }
    }
  }

  // 前端侧：本仓的真实形态是 bridge.ts 的 `call<T>("cmd", …)` 包装；
  // 同时保留裸 `invoke("cmd")` 形态，将来有人绕过 bridge 直接调也能查到。
  const RE_CALL = /\bcall\s*(?:<[^>]*>)?\s*\(\s*"([a-z][a-z0-9_]*)"/g;
  const RE_INVOKE = /\binvoke\s*(?:<[^>]*>)?\s*\(\s*["'`]([a-z][a-z0-9_]*)["'`]/g;
  const used = new Map();
  for (const f of TS) {
    readFileSync(f, "utf8")
      .split("\n")
      .forEach((line, i) => {
        for (const re of [RE_CALL, RE_INVOKE]) {
          re.lastIndex = 0;
          for (const m of line.matchAll(re)) {
            if (!used.has(m[1])) used.set(m[1], `${f}:${i + 1}`);
          }
        }
      });
  }

  // 🔴 恒零命中的门是静默无用的门。主动判失败，别让它悄悄空转。
  if (declared.size === 0) {
    fail("invoke-command-must-exist", "      Rust 侧解析到 0 个 #[tauri::command] —— 解析器坏了");
  } else if (used.size === 0) {
    fail(
      "invoke-command-must-exist",
      "      前端侧解析到 0 个命令调用 —— 调用形态变了（第一版就栽在这：查 invoke() 命中 0 处），\n" +
        "      本检查已形同虚设，必须先修解析器再谈通过"
    );
  } else {
    const missing = [...used].filter(([name]) => !declared.has(name));
    if (missing.length) {
      fail(
        "invoke-command-must-exist",
        `${WHY}\n` +
          missing.map(([n, where]) => `      ${where}  "${n}" 在 Rust 侧不存在`).join("\n")
      );
    } else {
      pass(
        "invoke-command-must-exist",
        `${used.size} 个命令调用全部对上（Rust 侧共 ${declared.size} 个命令）`
      );
    }
  }
}

// ---------------------------------------------------------------------------
// 规则 3：仓库里不许有私钥材料或签名凭据口令
// ---------------------------------------------------------------------------
{
  const WHY =
    "      CLAUDE.md 铁律：签名密钥只经 TAURI_SIGNING_PRIVATE_KEY 环境变量传入，永不落仓库。\n" +
    "      2026-08-14 这条被破过一次（更新签名私钥 + 口令一起进了公开仓库，08-23 才发现），\n" +
    "      而当时**已有**一道门（audit:release）—— 它按文件名判，而密钥贴在 .md 正文里、\n" +
    "      还包了一层 base64。判据存在 ≠ 判对了维度，故本规则改为「解码后匹配内容特征」。";

  // 🔴 刻意放在**策略门**里而不是只放在 audit:release 里。
  // 那次泄露发生在一次 commit，而 audit:release 只在发版时跑 —— 于是密钥在仓库里
  // 躺了 9 天。门必须在它能拦住的那个时刻跑。
  const { findings, stats } = scanRepo();

  if (stats.scanned === 0) {
    fail("no-secrets-in-tracked-files", "      扫到 0 个文件 —— 解析器坏了，本检查形同虚设");
  } else if (findings.length) {
    const lines = findings.map((f) => {
      const where = f.file ? `${f.file}${f.line ? `:${f.line}` : ""}` : "(仓库)";
      return `      ${where}  ${f.what}`;
    });
    fail(
      "no-secrets-in-tracked-files",
      `${WHY}\n${lines.join("\n")}\n\n` +
        "      ⚠️ 注意：把文件删掉能让这道门变绿，但**那不等于泄露被修复了** ——\n" +
        "      本门只扫工作区，不扫 git 历史、不扫已推出去的 fork。已公开的密钥必须\n" +
        "      当作永久失效处置（换钥或明确接受风险）。",
    );
  } else {
    // 如实报「实际做了多少次 gpg 试解」。没有候选口令时是 0 次 —— 那也是通过，
    // 但不能说成「已交叉验证」：仓库里没有口令可试，本来就无从验证。
    // 把 0 次说成验过了，正是本仓最忌的那类「门说查过了、其实没查」。
    const cross =
      stats.ciphertext.length === 0
        ? ""
        : stats.gpgChecks > 0
          ? `，${stats.ciphertext.length} 份密文 × 候选口令共试解 ${stats.gpgChecks} 次均失败`
          : `，${stats.ciphertext.length} 份密文未试解（仓库里没有候选口令可试）`;
    pass("no-secrets-in-tracked-files", `扫过 ${stats.scanned} 个文件、${stats.b64Runs} 段 base64${cross}`);
  }
}

// ---------------------------------------------------------------------------
// 规则 4：版本号必须处处一致
// ---------------------------------------------------------------------------
{
  const WHY =
    "      CLAUDE.md 写的是「三处版本号一致」，但实际有**四个**文件带版本号，\n" +
    "      而第四个（package-lock.json）没人管：2026-08-27 实测它停在 0.1.33，\n" +
    "      其余三处已是 0.1.39 —— 落后 6 个版本，没有任何告警。\n" +
    "      危害不在构建（Tauri 不读 lock 的 version，npm ci 也只对账依赖、不看顶层版本），\n" +
    "      而在**取证**：排查「应用到底报了哪个版本」时，仓库里同时存在两个答案，\n" +
    "      却没有任何东西指出哪个是权威的。这一轮就是这么多花了一趟。";

  // 🔴 为什么 tauri.conf.json 必须与 Cargo.toml 一致（两者是**不同**的版本来源）：
  //   - `package_info().version`（get_app_version / check_for_updates / 诊断导出）
  //     取自 tauri.conf.json 的 `version`（tauri-codegen context.rs:273；
  //     该字段缺失时才回落 env!("CARGO_PKG_VERSION")）；
  //   - Windows VERSIONINFO 的 FileVersion/ProductVersion 取自**同一字段**
  //     （tauri-build lib.rs:632）；
  //   - 而 `x-synaroute-version` 响应头取自 CARGO_PKG_VERSION，即 Cargo.toml。
  // 两处一旦漂移，exe 属性页与响应头会给出两个不同的版本，且**不会有任何报错**。
  //
  // 刻意**不**查 Cargo.lock：cargo 每次构建会自己把它对齐，
  // 收进来只会让「刚 bump 完还没构建」这个正常中间态变红，是纯摩擦。
  const readVersions = () => {
    const json = (p) => JSON.parse(readFileSync(p, "utf8"));
    const cargoToml = () => {
      // 只取 [package] 段的 version —— 不能裸 grep /^version/，
      // 那会在将来有人加 [workspace.package] 或依赖段时抓错行。
      const lines = readFileSync("src-tauri/Cargo.toml", "utf8").split("\n");
      let inPackage = false;
      for (const line of lines) {
        const t = line.trim();
        if (/^\[.*\]$/.test(t)) {
          inPackage = t === "[package]";
          continue;
        }
        if (!inPackage) continue;
        const m = t.match(/^version\s*=\s*"([^"]+)"/);
        if (m) return m[1];
      }
      return undefined;
    };
    return [
      ["package.json", ".version", () => json("package.json").version],
      ["package-lock.json", ".version", () => json("package-lock.json").version],
      ["package-lock.json", '.packages[""].version', () => json("package-lock.json").packages[""].version],
      ["src-tauri/tauri.conf.json", ".version", () => json("src-tauri/tauri.conf.json").version],
      ["src-tauri/Cargo.toml", "[package] version", cargoToml],
    ];
  };

  const EXPECTED_FIELDS = 5;
  const found = [];
  const unreadable = [];
  for (const [file, field, read] of readVersions()) {
    let v;
    try {
      v = read();
    } catch (e) {
      unreadable.push(`      ${file} ${field}  读不出来：${e.message}`);
      continue;
    }
    if (typeof v !== "string" || !v) {
      unreadable.push(`      ${file} ${field}  缺失或不是字符串（拿到 ${JSON.stringify(v)}）`);
      continue;
    }
    found.push({ file, field, v });
  }

  // 🔴 同规则 2/3：解析不到预期数量的字段就**主动判失败**。
  // 文件被改名/字段被挪走会让「全部一致」退化成「一个都没查」，而那是静默的。
  if (found.length < EXPECTED_FIELDS) {
    fail(
      "version-must-be-consistent",
      `      只解析到 ${found.length}/${EXPECTED_FIELDS} 个版本字段 —— 解析器与仓库结构脱节了，\n` +
        "      本检查已形同虚设，必须先修解析器再谈通过。\n" +
        unreadable.join("\n")
    );
  } else {
    const distinct = [...new Set(found.map((f) => f.v))];
    if (distinct.length > 1) {
      fail(
        "version-must-be-consistent",
        `${WHY}\n` +
          `      发现 ${distinct.length} 个不同的版本号：${distinct.join(" / ")}\n` +
          found.map((f) => `      ${f.v.padEnd(10)} ← ${f.file} ${f.field}`).join("\n") +
          "\n\n      修法：先确定权威值（通常是 package.json），再同步其余；\n" +
          "      package-lock.json 用 `npm install --package-lock-only`，不要手改。"
      );
    } else {
      pass("version-must-be-consistent", `${found.length} 处版本号一致（${distinct[0]}）`);
    }
  }
}

// ---------------------------------------------------------------------------
// 规则 5：应用内更新必须保留「读 Windows 系统代理」的能力
// ---------------------------------------------------------------------------
{
  const WHY =
    "      应用内更新要访问 github.com。国内用户普遍靠系统代理上网，而**双击启动**的进程\n" +
    "      不会继承任何 shell 里的 HTTP_PROXY/HTTPS_PROXY —— 于是能不能读 Windows\n" +
    "      注册表里的系统代理设置，直接决定「检查更新」是通还是超时。\n" +
    "      🔴 而这个能力是**偶然**得来的，且一旦失去是**静默**的：\n" +
    "      reqwest 无条件调 hyper-util 的 Matcher::from_system()（reqwest-0.13.4/src/proxy.rs:517），\n" +
    "      但读注册表那段被 hyper-util 的 `client-proxy-system` feature 门控\n" +
    "      （hyper-util-0.1.20/src/client/proxy/matcher.rs:245 与 :663）。\n" +
    "      本仓从未显式要求过这个 feature —— 它是 `hyper-util = { features = [\"full\"] }`\n" +
    "      恰好把它带进来的。谁为瘦身把 full 收窄，更新功能就对一大批用户失效，\n" +
    "      而编译、测试、门全都不会报错。";

  const CARGO_TOML = "src-tauri/Cargo.toml";
  const CARGO_LOCK = "src-tauri/Cargo.lock";
  const problems = [];
  let checked = 0;

  if (!existsSync(CARGO_TOML) || !existsSync(CARGO_LOCK)) {
    problems.push(`      找不到 ${CARGO_TOML} 或 ${CARGO_LOCK} —— 解析器与仓库结构脱节`);
  } else {
    const toml = readFileSync(CARGO_TOML, "utf8");
    // 只认非注释行上的 hyper-util 依赖声明
    const line = toml
      .split("\n")
      .find((l) => /^\s*hyper-util\s*=/.test(l) && !l.trim().startsWith("#"));
    if (!line) {
      problems.push(
        "      Cargo.toml 里找不到 hyper-util 依赖声明 —— 要么被删了（那更新代理能力已经没了），\n" +
          "        要么写法变了导致本判据空转。两种都必须人工看一眼。"
      );
    } else {
      checked++;
      const feats = (line.match(/features\s*=\s*\[([^\]]*)\]/) || [, ""])[1];
      const has =
        /["']full["']/.test(feats) || /["']client-proxy-system["']/.test(feats);
      if (!has) {
        problems.push(
          `      ${CARGO_TOML} 的 hyper-util 既没有 "full" 也没有 "client-proxy-system"：\n` +
            `        ${line.trim()}\n` +
            '        → 收窄了 features 就必须显式写上 "client-proxy-system"。'
        );
      }
    }

    // 第二道：feature 是**按包**并集的，只有当 hyper-util 在依赖树里只有一个版本时,
    // 本仓 features=["full"] 才能影响到 reqwest 实际链接的那一份。
    // 出现第二个版本时上面那条会「仍然为绿而能力已失」—— 这是本判据唯一的盲区，故一并钉住。
    const lock = readFileSync(CARGO_LOCK, "utf8");
    const versions = [
      ...lock.matchAll(/^name = "hyper-util"\r?\nversion = "([^"]+)"/gm),
    ].map((m) => m[1]);
    if (versions.length === 0) {
      problems.push("      Cargo.lock 里没有 hyper-util —— 解析器坏了或依赖已移除");
    } else if (versions.length > 1) {
      problems.push(
        `      Cargo.lock 里有 ${versions.length} 个 hyper-util 版本（${versions.join(", ")}）。\n` +
          "        feature 是按包并集的，多版本时本仓的 features=[\"full\"] 可能落在\n" +
          "        reqwest 实际链接的那一份之外 → 上面那条判据会「绿着但能力已失」。\n" +
          "        请统一版本，或改为显式验证 reqwest 那一侧的 feature。"
      );
    } else {
      checked++;
    }
  }

  if (problems.length) {
    fail("updater-must-read-system-proxy", `${WHY}\n${problems.join("\n")}`);
  } else {
    pass(
      "updater-must-read-system-proxy",
      `hyper-util 带 client-proxy-system 且依赖树里仅一个版本（${checked}/2 项判据均命中）`
    );
  }
}

if (failed) {
  console.log(`\n❌ ${failed} 条硬规则未通过。这些判据没有基线可冻结 —— 只能修。`);
  process.exit(1);
}
console.log("\n✅ 全部硬规则通过");