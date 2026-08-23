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

/** 尾部 `#[cfg(test)] mod tests {` 的起始行号（1-based）；没有则 null。与 check-ratchet.mjs 同源判据。 */
function testModStartLine(src) {
  const lines = src.split("\n");
  for (let i = lines.length - 1; i >= 0; i--) {
    if (!/^\s{0,4}mod tests\b/.test(lines[i])) continue;
    for (let j = i - 1; j >= 0 && j >= i - 8; j--) {
      const t = lines[j].trim();
      if (t === "" || t.startsWith("//")) continue;
      if (/^#\[cfg\(test\)\]$/.test(t)) return j + 1;
      break;
    }
  }
  return null;
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

if (failed) {
  console.log(`\n❌ ${failed} 条硬规则未通过。这些判据没有基线可冻结 —— 只能修。`);
  process.exit(1);
}
console.log("\n✅ 全部硬规则通过");
