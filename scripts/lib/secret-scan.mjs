// **仓库内密钥材料扫描器**：判据是「解码后的内容特征」，不是文件名。
//
// ## 它防的是哪次事故
//
// 2026-08-14 `GITHUB_SECRETS_SETUP.md` 把 Tauri 更新签名私钥（`DE14C6EC68286277`）
// 连同它的解密口令一起提交进了**公开**仓库，直到 2026-08-23 才被发现。那把钥恰好在
// 08-16 被换掉，所以只有 v0.1.23 那一批客户端受影响 —— 纯属运气，不是防线起了作用。
//
// 当时是有一道门的（`npm run audit:release`），它没抓到，原因值得写下来：
//
//   1. 它按**文件名**判（`\.key$|\.pem$`）。密钥贴在 `.md` 正文里，文件名毫无异样。
//   2. 就算改成内容 grep `untrusted comment: rsign …`，**照样抓不到** ——
//      `TAURI_SIGNING_PRIVATE_KEY` 的形态是「整份密钥文件再 base64 一层」，
//      那句注释在文件里根本不以明文出现，第 15 行看上去只是一串普通 base64。
//
// 所以本扫描器的核心判据是：**把长 base64 串解出来再匹配**（最多解两层，因为密钥文件
// 自身的第二行又是一层 base64）。这正是 CLAUDE.md 反复强调的那件事 ——
// 「判据存在」不等于「判对了维度」。
//
// ## 边界（别把它当成比实际更强的东西）
//
// 只扫**工作区里被 git 跟踪的文件**。它不扫 git 历史、不扫已经推出去的 fork。
// 也就是说：删掉泄露文件能让这道门变绿，但**那不等于泄露被修复了** ——
// 已经公开的密钥必须当作永久失效来处置（换钥或接受风险），门只保证「不再新增」。
import { readFileSync, statSync, existsSync } from "node:fs";
import { execFileSync, execSync } from "node:child_process";
import { join, dirname } from "node:path";

// ---------------------------------------------------------------------------
// 判据常量
// ---------------------------------------------------------------------------

// 🔴 签名串**刻意拼接**而不是写成整串字面量。
//
// 本文件自身也被 git 跟踪、也会被自己扫到。写成整串会让扫描器每次都命中自己，
// 于是必然有人把它加进白名单 —— 而白名单就是下一次泄露的藏身处。
// 拼接之后本文件里不存在任何完整签名串，无需任何自我豁免。
const UC = "untrusted comment: ";
const KEY_SIGNATURES = [
  ["rsign 私钥（口令加密形态）", UC + "rsign encrypted " + "secret key"],
  ["rsign 私钥（明文形态）", UC + "rsign " + "secret key"],
  ["minisign 私钥", UC + "minisign " + "secret key"],
];

const PEM_RE = /-----BEGIN (?:[A-Z ]+ )?PRIVATE KEY-----/;

// minisign/rsign 私钥主体（密钥文件第二行）的 base64 固定前缀。公钥也是 `RWR` 开头，
// 但第 4 个字符起就不同（本仓公钥是 `RWRv…`），故不误伤。
//
// **只在够长的 base64 串里认它**：以 8 字符字面量提到这个前缀是正常的
// （`audit-release-bundle.mjs` 提到两次，本文件提到一次）。长度下限统一由
// `B64_MIN_RUN` 表达 —— 刻意只留一个常量：曾经写成「run 下限 60 + 前缀下限 200」两个数，
// 结果是「把前缀下限从 200 改成 8」注入进去**测试仍然全绿**（60 那道先挡住了），
// 也就是那个 200 既是死数、又制造了 60~199 长度的盲区。一个判据一个旋钮。
const SK_B64_PREFIX = "RWRTY0Iy";
const B64_MIN_RUN = 60;

// 只认**签名凭据**这一小类标签，刻意不泛化成任何 `password`：
// 本应用自己有「主口令」功能，i18n 与 Rust 里有大量 password/口令 字样，
// 泛化必然淹没在假警里 —— 而一道会喊狼来了的门等于没有门。
const CRED_LABEL_RE =
  /\b(TAURI_SIGNING_PRIVATE_KEY_PASSWORD|TAURI_KEY_PASSWORD|SIGNING_KEY_PASSWORD|GPG_PASSPHRASE|GPG_PASSWORD|MINISIGN_PASSWORD)\b/;

// 多行「前向窗口」只对**文档 / 配置**格式生效；源码只认同行形态。
//
// 🔴 这条限制是被自己咬出来的，值得写清楚。本次泄露的形态是 markdown
// （标签 → 空行 → `**Value**:` → 围栏 → 值，隔了 4 行），所以必须有前向窗口。
// 但在**源码**里，标签几乎总是以「模式定义 / 环境变量引用 / 测试夹具」的身份出现，
// 紧随其后的任意一行都是代码，于是前向窗口把这些当成了「口令值」：
//
//   本文件的 CRED_LABEL_RE 那一行  → 把下面 BINARY_EXT 的正则字面量当成口令
//   secretScan.test.ts 的夹具数组 → 把 `].join("\n");`、`expect(...)` 当成口令
//
// 而这个假警**直到这两个文件被 git 跟踪的那一刻才会出现** —— 扫描器只扫 tracked
// 文件（见 [`trackedFiles`]），它们还是 untracked 时它看不见自己，于是整个开发期这道门
// 都是绿的，提交那一刻才红。**「门是绿的」不等于「门查过了它该查的东西」。**
//
// 为什么不加文件白名单：本文件开头那段已经写明「白名单就是下一次泄露的藏身处」。
// 按格式收窄判据不产生藏身处 —— 任何 `.md` / `.yml` / `.env` 里的泄露照旧被抓。
//
// 源码里仍保留**同行**形态（`GPG_PASSPHRASE=xxx`），那才是硬编码口令的真实写法；
// 而「标签在注释里、值在下一行」这种形态旧实现本来也抓不到
// （`const pw = "x";` 带空格，过不了 `looksLikeRealSecret`），故没有新增盲区。
const SOURCE_EXT =
  /\.(m?[jt]sx?|c[jt]s|rs|py|go|java|kt|rb|php|c|cc|cpp|h|hpp|cs|swift|sh|bash|ps1|bat|cmd)$/i;

const BINARY_EXT =
  /\.(png|jpe?g|gif|webp|ico|icns|pdf|docx?|xlsx?|pptx?|zip|gz|tgz|7z|rar|exe|dll|msi|so|dylib|woff2?|ttf|otf|eot|mp4|mp3|wav|db|sqlite3?|bin|wasm|lock)$/i;

// `.gpg` / `.enc` 刻意跳过：它们**本来就该是密文**，扫内容毫无意义。
// 它们的真实风险是「口令也在仓库里」，那由 `crossCheckGpg()` 单独验。
const CIPHERTEXT_EXT = /\.(gpg|asc|enc)$/i;

const MAX_BYTES = 2 * 1024 * 1024;

// ---------------------------------------------------------------------------
// base64 提取与解码
// ---------------------------------------------------------------------------

/**
 * 取出文件里所有「够长的 base64 串」。两种形态都要覆盖：
 * - 单行内嵌的长串（本次泄露就是这形态：md 的一行 348 字符）
 * - 连续多行组成的块（密钥文件原样贴进来、PEM 正文）
 */
export function extractBase64Runs(text) {
  const runs = new Set();

  // 形态一：行内长串
  const RUN_RE = new RegExp(`[A-Za-z0-9+/=]{${B64_MIN_RUN},}`, "g");
  for (const m of text.matchAll(RUN_RE)) runs.add(m[0]);

  // 形态二：连续 base64 行拼成块
  let acc = [];
  const flush = () => {
    if (acc.length > 1) {
      const joined = acc.join("");
      if (joined.length >= B64_MIN_RUN) runs.add(joined);
    }
    acc = [];
  };
  for (const line of text.split("\n")) {
    const t = line.trim();
    if (t.length >= 20 && /^[A-Za-z0-9+/=]+$/.test(t)) acc.push(t);
    else flush();
  }
  flush();

  return [...runs];
}

/**
 * 对一个 base64 串解码并匹配密钥特征，最多解两层。
 *
 * 为什么要两层：本仓 `TAURI_SIGNING_PRIVATE_KEY` 的形态是 base64(整份密钥文件)，
 * 而那份文件的第二行**又是**一层 base64。`secrets/*.gpg` 解出来的明文也是这个套娃形态。
 * 只解一层能抓到本次事故，解两层才能抓到「只把密钥主体那一行贴出来」的变体。
 */
export function decodedKeyHits(run) {
  let cur = run;
  for (let depth = 0; depth < 2; depth++) {
    let dec;
    try {
      dec = Buffer.from(cur, "base64").toString("latin1");
    } catch {
      return [];
    }
    if (!dec) return [];

    const hits = [];
    for (const [label, sig] of KEY_SIGNATURES) if (dec.includes(sig)) hits.push(label);
    if (PEM_RE.test(dec)) hits.push("PEM 私钥");
    if (hits.length) return hits;

    // 解出来的还是纯 base64 → 再套一层试试
    const t = dec.trim();
    if (t.length < B64_MIN_RUN || !/^[A-Za-z0-9+/=\s]+$/.test(t)) return [];
    cur = t.replace(/\s+/g, "");
  }
  return [];
}

// ---------------------------------------------------------------------------
// 口令值判定
// ---------------------------------------------------------------------------

const LABEL_LIKE_RE = /^[A-Z][A-Z0-9_]{5,}$/;

/**
 * 判断一段文本是否像「真的凭据值」。返回规范化后的值，或 null。
 *
 * 判据刻意收紧到「不含空格的可打印 ASCII」：中文说明、YAML 片段、散文都带空格或中文字符，
 * 一律排除。这样假警面接近零，而 `mhm292117.` 这类真口令照样命中。
 */
export function looksLikeRealSecret(raw) {
  const s = String(raw).trim().replace(/^[`"'“”]+/, "").replace(/[`"'“”]+$/, "").trim();
  if (!/^[\x21-\x7E]{4,200}$/.test(s)) return null;
  if (/\$\{\{|\$\(|\$[A-Za-z_]|%[A-Za-z_]+%|<[^>]*>/.test(s)) return null; // 变量引用 / 尖括号占位
  if (LABEL_LIKE_RE.test(s)) return null; // 值就是标签名本身（markdown 的 **Name**: `FOO` 行）
  if (CRED_LABEL_RE.test(s)) return null;
  if (/^[-*_<>{}[\]().…x*#/\\|=+~]+$/i.test(s)) return null; // 纯占位符号
  if (/your|placeholder|example|示例|占位|填写|待填|xxx|\*{3,}/i.test(s)) return null;
  if (/^(?:null|none|empty|n\/a|na|todo|tbd|password|passphrase)$/i.test(s)) return null;
  return s;
}

const SCAFFOLD_RE = /^(?:#{1,6}\s|\*\*|>|[-*+]\s|\||```|~~~|\s*$)/;

/**
 * 找出「签名凭据标签 + 紧随其后的真实值」这一形态。
 *
 * 前向窗口是必需的：本次泄露的形态是 markdown 的
 *   `**Name**: \`TAURI_SIGNING_PRIVATE_KEY_PASSWORD\`` → 空行 → `**Value**:` → ``` → 值
 * 标签和值隔了 4 行。同行形态（`FOO=bar`、`FOO: bar`）也要覆盖。
 *
 * `forwardWindow=false`（源码文件）时只认同行形态，理由见 [`SOURCE_EXT`]。
 */
export function findCredentialValues(text, { forwardWindow = true } = {}) {
  const lines = text.split("\n");
  const out = [];
  let fence = false;

  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    if (/^\s*(?:```|~~~)/.test(line)) fence = !fence;
    if (!CRED_LABEL_RE.test(line)) continue;

    // 同行：分隔符之后的部分
    const sameLine = line.replace(CRED_LABEL_RE, "").match(/^\s*[:=]\s*(.+)$/);
    const candidates = [];
    if (sameLine) candidates.push({ n: i + 1, v: sameLine[1] });

    // 前向窗口：跳过 markdown 脚手架行，遇标题或下一个标签就停
    let inFence = fence;
    for (let j = i + 1; forwardWindow && j < Math.min(i + 10, lines.length); j++) {
      const t = lines[j];
      if (/^\s*(?:```|~~~)/.test(t)) {
        inFence = !inFence;
        continue;
      }
      if (/^\s*#{1,6}\s/.test(t)) break;
      if (CRED_LABEL_RE.test(t)) break;
      if (SCAFFOLD_RE.test(t)) continue;
      // 🔴 关键的假警防线：围栏外只接受「不含冒号的裸值行」。
      // 没有它，YAML/TOML 里紧跟标签的任意一行（`projectPath: .`）都会被当成值。
      if (!inFence && t.includes(":")) continue;
      candidates.push({ n: j + 1, v: t });
    }

    for (const c of candidates) {
      const v = looksLikeRealSecret(c.v);
      if (v) {
        out.push({ line: c.n, labelLine: i + 1, value: v });
        break;
      }
    }
  }
  return out;
}

// ---------------------------------------------------------------------------
// 文件扫描
// ---------------------------------------------------------------------------

export function trackedFiles(cwd = process.cwd()) {
  return execSync("git ls-files", { encoding: "utf8", cwd, maxBuffer: 64 * 1024 * 1024 })
    .split("\n")
    .map((s) => s.trim())
    .filter(Boolean);
}

/**
 * 扫一组文件，返回 { findings, stats }。
 *
 * findings 里每条都是「必须为 0」的策略级问题：
 *   - kind=key        仓库里有私钥材料
 *   - kind=passphrase 仓库里有签名凭据口令
 */
export function scanFiles(files, { cwd = process.cwd() } = {}) {
  const findings = [];
  const stats = { scanned: 0, skippedBinary: 0, skippedLarge: 0, b64Runs: 0, ciphertext: [] };

  for (const rel of files) {
    if (CIPHERTEXT_EXT.test(rel)) {
      stats.ciphertext.push(rel);
      continue;
    }
    if (BINARY_EXT.test(rel)) {
      stats.skippedBinary++;
      continue;
    }
    const abs = join(cwd, rel);
    if (!existsSync(abs)) continue;
    let size;
    try {
      size = statSync(abs).size;
    } catch {
      continue;
    }
    if (size > MAX_BYTES) {
      stats.skippedLarge++;
      continue;
    }

    let text;
    try {
      text = readFileSync(abs, "utf8");
    } catch {
      continue;
    }
    stats.scanned++;

    // 1) 明文签名串
    for (const [label, sig] of KEY_SIGNATURES) {
      if (text.includes(sig)) findings.push({ kind: "key", file: rel, what: label + "（明文）" });
    }
    if (PEM_RE.test(text)) findings.push({ kind: "key", file: rel, what: "PEM 私钥（明文）" });

    // 2) base64 包装的签名串 ← 本次事故的实际形态
    const runs = extractBase64Runs(text);
    stats.b64Runs += runs.length;
    const seen = new Set();
    for (const run of runs) {
      for (const label of decodedKeyHits(run)) {
        if (seen.has(label)) continue;
        seen.add(label);
        findings.push({ kind: "key", file: rel, what: `${label}（base64 包装，解码后命中）` });
      }
      if (run.length >= B64_MIN_RUN && run.startsWith(SK_B64_PREFIX) && !seen.has("body")) {
        seen.add("body");
        findings.push({ kind: "key", file: rel, what: "minisign 私钥主体（裸 base64）" });
      }
    }

    // 3) 签名凭据口令
    for (const c of findCredentialValues(text, { forwardWindow: !SOURCE_EXT.test(rel) })) {
      findings.push({
        kind: "passphrase",
        file: rel,
        line: c.line,
        value: c.value,
        what: `签名凭据口令明文（标签在第 ${c.labelLine} 行）`,
      });
    }
  }

  return { findings, stats };
}

// ---------------------------------------------------------------------------
// GPG 交叉验证
// ---------------------------------------------------------------------------

/**
 * 找 gpg。Git for Windows 自带它，但只把 `cmd\` 写进 PATH、不写 `usr\bin`
 * （见 secrets/README.md 里记的同一个坑），故从 `git --exec-path` 反推 Git 根目录。
 */
export function findGpg() {
  const tryRun = (bin) => {
    try {
      execFileSync(bin, ["--version"], { stdio: "ignore" });
      return bin;
    } catch {
      return null;
    }
  };
  const direct = tryRun("gpg");
  if (direct) return direct;
  try {
    let d = execSync("git --exec-path", { encoding: "utf8" }).trim();
    for (let i = 0; i < 5 && d && d !== dirname(d); i++) {
      const cand = join(d, "usr", "bin", "gpg.exe");
      if (existsSync(cand) && tryRun(cand)) return cand;
      d = dirname(d);
    }
  } catch {
    /* git 不在 PATH，放弃 */
  }
  return null;
}

/**
 * 拿仓库里出现过的口令去试解仓库里的密文 —— 这次泄露正是这么破的：
 * `secrets/synaroute.key.gpg` 的口令写在另一个 tracked 的 `.md` 里，
 * 于是「密文可以公开」这个设计前提被自己作废了。
 *
 * 没有候选口令时**无需 gpg 也算通过**（没东西可试）；有候选却找不到 gpg 时判失败 ——
 * 一个「无法验证真实风险」的门不该悄悄放行。
 */
export function crossCheckGpg(ciphertextFiles, passphrases, { cwd = process.cwd() } = {}) {
  if (!ciphertextFiles.length || !passphrases.length) {
    return { findings: [], checked: 0, gpg: null };
  }
  const gpg = findGpg();
  if (!gpg) {
    return {
      findings: [
        {
          kind: "unverifiable",
          what:
            `仓库里有 ${ciphertextFiles.length} 份密文和 ${passphrases.length} 个候选口令，` +
            "但找不到 gpg，无法验证口令是否能解开它们。装 Git for Windows 或 GnuPG 后重跑。",
        },
      ],
      checked: 0,
      gpg: null,
    };
  }

  const findings = [];
  let checked = 0;
  for (const f of ciphertextFiles) {
    for (const pw of passphrases) {
      checked++;
      try {
        const out = execFileSync(
          gpg,
          ["--batch", "--yes", "--quiet", "--pinentry-mode", "loopback", "--passphrase-fd", "0", "-d", f],
          { cwd, input: pw, maxBuffer: 8 * 1024 * 1024, stdio: ["pipe", "pipe", "ignore"] },
        );
        if (out && out.length > 0) {
          findings.push({
            kind: "passphrase",
            file: f,
            what: "仓库内的口令能解开这份密文 —— 「密文可公开」的前提已失效",
          });
          break;
        }
      } catch {
        /* 解不开 = 正常 */
      }
    }
  }
  return { findings, checked, gpg };
}

/** 一次性跑完全部检查。供 check-forbidden.mjs 与 audit-release-bundle.mjs 共用。 */
export function scanRepo({ cwd = process.cwd() } = {}) {
  const files = trackedFiles(cwd);
  const { findings, stats } = scanFiles(files, { cwd });
  const passphrases = [...new Set(findings.filter((f) => f.kind === "passphrase" && f.value).map((f) => f.value))];
  const cross = crossCheckGpg(stats.ciphertext, passphrases, { cwd });
  return {
    findings: [...findings, ...cross.findings],
    stats: { ...stats, tracked: files.length, gpgChecks: cross.checked, gpg: cross.gpg },
  };
}
