// 棘轮**防作弊验证器**：拦住「悄悄抬高上限」这条解红最快的路。
//
// ## 为什么必须有这个东西
//
// 棘轮只有一半是自动的，而且是错的那一半：
// - **抬高**一个数字 = 改一行 JSON，十秒钟，是让红 CI 变绿最快的办法；
// - **降低**一个数字 = 有人得主动跑 `--update` 并提交。
//
// OmniRoute 实测过这个不对称的代价（他们的 issue #8584，见 CLAUDE.md 里的对照分析）：
// 18 个已冻结文件早就远低于上限（最夸张的一条 19 行代码背着 2523 的上限，132×）；
// 复杂度上限从 1794 一路走到 2169，全程只降过一次（−1）；
// 「下个周期收紧」在提交记录里写了 31 次、兑现 1 次。
// 一个活得比它所约束的代码更久的上限，会把每一次已完成的拆分变成给下一个人的增长额度。
//
// 他们的结论值得原样抄下来：**能抬高上限的机器人比没有机器人更糟**。
//
// ## 判据
//
// 对比工作区的 baseline 与 `git HEAD` 里的那一份，只允许这些变化：
//   - `frozen` 里某个数字**变小**，或整条**被删除**；
//   - `counters` 里某个数字**变小**；
//   - `newFileCapLines` **变小**。
//
// 任何抬高（含新增 frozen 条目、抬高 cap）都必须在 `raises` 里配一条**写明理由**的记录，
// 否则本脚本失败。理由会留在 diff 里，是可审计的 —— 这就是全部目的：
// 抬高不是不可能，而是**有成本、留痕迹**。
import { readFileSync, existsSync } from "node:fs";
import { execFileSync } from "node:child_process";

const BASELINE = "config/quality/ratchet.json";
const MIN_WHY_LEN = 20;

// `--before <path>`：拿指定文件当「改动前」的基线，而不是 git HEAD。
//
// 存在的理由是**可测性**。第一版只读 git HEAD，于是在基线尚未入库时它走
// 「首次引入 → 放行」分支 —— 我给它做故障注入时**四条注入全部被放过**，
// 而当时看起来是「验证器有 bug」。一个只能在特定 git 状态下才起作用的门，
// 等于一个平时无法验证的门。
const beforeIdx = process.argv.indexOf("--before");
const BEFORE_FILE = beforeIdx >= 0 ? process.argv[beforeIdx + 1] : null;

if (!existsSync(BASELINE)) {
  console.error(`❌ 找不到 ${BASELINE}`);
  process.exit(2);
}

let head;
if (BEFORE_FILE) {
  if (!existsSync(BEFORE_FILE)) {
    console.error(`❌ --before 指定的文件不存在：${BEFORE_FILE}`);
    process.exit(2);
  }
  head = JSON.parse(readFileSync(BEFORE_FILE, "utf8"));
} else {
  try {
    head = JSON.parse(
      execFileSync("git", ["show", `HEAD:${BASELINE}`], {
        encoding: "utf8",
        stdio: ["ignore", "pipe", "pipe"],
      })
    );
  } catch {
    // HEAD 里还没有这个文件（首次引入棘轮的那个提交）→ 无从对比，放行。
    // ⚠️ 这条分支意味着「本次运行什么都没验」。刻意打印得显眼一点，
    // 免得它在 CI 日志里被当成一次成功的检查。
    console.log(
      "⚠️  HEAD 里尚无棘轮基线（首次引入棘轮的提交）→ 无从对比，本次**什么都没验**。\n" +
        "    下一个提交起本检查才真正生效。要现在验，用 --before <改动前的基线文件>。"
    );
    process.exit(0);
  }
}
const now = JSON.parse(readFileSync(BASELINE, "utf8"));

const problems = [];
const ok = [];

/** 这次抬高有没有配一条合格的理由？ */
function hasJustification(key, to) {
  const r = (now.raises ?? {})[key];
  if (!r) return `缺少 raises["${key}"] 理由记录`;
  if (r.to !== to) return `raises["${key}"].to = ${r.to}，与新值 ${to} 不符`;
  if (typeof r.why !== "string" || r.why.trim().length < MIN_WHY_LEN)
    return `raises["${key}"].why 太短（至少 ${MIN_WHY_LEN} 字）—— 理由要能让半年后的自己看懂`;
  return null;
}

function checkNumber(kind, key, before, after) {
  if (after === undefined) {
    ok.push(`${kind} ${key}: ${before} → 已移除`);
    return;
  }
  if (before === undefined) {
    // 新增条目 = 一个新的大文件被冻结进来。它绕过了 newFileCapLines，必须写理由。
    const why = hasJustification(key, after);
    if (why) problems.push(`${kind} ${key}: 新增冻结条目（${after}）—— ${why}`);
    else ok.push(`${kind} ${key}: 新增 ${after}（已附理由）`);
    return;
  }
  if (after < before) {
    ok.push(`${kind} ${key}: ${before} → ${after}（变小 ✓）`);
  } else if (after > before) {
    const why = hasJustification(key, after);
    if (why) problems.push(`${kind} ${key}: ${before} → ${after}（**抬高**）—— ${why}`);
    else ok.push(`${kind} ${key}: ${before} → ${after}（抬高，已附理由）`);
  }
}

// cap 只准降（抬高 cap 是一次性放宽所有新文件，影响面最大）
checkNumber("newFileCapLines", "newFileCapLines", head.newFileCapLines, now.newFileCapLines);

const hf = head.frozen ?? {};
const nf = now.frozen ?? {};
for (const k of new Set([...Object.keys(hf), ...Object.keys(nf)])) {
  checkNumber("frozen", k, hf[k], nf[k]);
}

const hc = head.counters ?? {};
const nc = now.counters ?? {};
for (const k of new Set([...Object.keys(hc), ...Object.keys(nc)])) {
  if (nc[k] === undefined && hc[k] !== undefined) {
    // 删掉一整条计数判据 = 把一条规则悄悄废掉。这比抬高数字更严重。
    problems.push(
      `counters ${k}: 整条判据被删除 —— 废掉一条规则必须显式说明，不能靠删 JSON 悄悄进行`
    );
    continue;
  }
  checkNumber("counters", k, hc[k], nc[k]);
}

if (ok.length) {
  console.log("变化：");
  ok.forEach((l) => console.log("   • " + l));
}
if (problems.length) {
  console.error(`\n❌ 棘轮基线被放宽了 ${problems.length} 处，且没有合格的理由记录：`);
  problems.forEach((p) => console.error("   ✗ " + p));
  console.error(
    `\n想抬高就在 ${BASELINE} 里加：\n` +
      `  "raises": { "<同名 key>": { "from": <旧值>, "to": <新值>, "why": "为什么必须抬高（≥${MIN_WHY_LEN} 字）", "when": "YYYY-MM-DD" } }\n` +
      `理由会留在 diff 里 —— 这就是全部目的：抬高不是不可能，而是有成本、留痕迹。`
  );
  process.exit(1);
}
console.log(`✅ 棘轮基线只往好的方向动了（${ok.length} 处变化）`);
