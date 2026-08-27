// 体积/计数**棘轮**：已冻结的数字只准降，不准升。
//
// 借鉴 OmniRoute 的 `scripts/check/check-file-size.mjs` + `check-quality-ratchet.mjs`
// （见 CLAUDE.md 里那份对照分析）。它解决的是本仓一个已经写在文档里的两难：
// docs/15 把「upstream/ 目录化」「service.rs 抽出」列为**刻意不做** —— 纯结构调整、
// 零用户可感收益，却是 diff 最大风险最高的。那个判断是对的，代价是 store.rs / proxy.rs
// 会继续长。棘轮不要求现在重构，它只保证**不再变坏**，并把每次顺手的小拆分永久锁住。
//
// ## 🔴 只数「生产段」，不数测试段
//
// Rust 的测试是**同文件内联**的（`#[cfg(test)] mod tests` 在文件尾）。若按整文件行数冻结，
// 「补一条回归测试」就会被这个门挡住 —— 那会把一个质量门变成质量的敌人，
// 而本仓的核心纪律恰恰是「每个缺陷都要留一条回归测试」。
// 故判据是：`.rs` 只数尾部 `#[cfg(test)] mod tests` **之前**的行；`.ts/.tsx` 排除 `*.test.ts`。
//
// ## 为什么还要「新文件上限」这一档
//
// 只冻结现有文件不够：那样下一个 5000 行的文件照样能出生（它不在 baseline 里，
// 于是没有任何约束）。新文件必须过 `newFileCapLines`。
//
// ## 更新（`--update`）
//
// 只向**下**棘轮：数字降了就锁到新值；降到 cap 以内就从 baseline 里删除条目。
// **绝不**自动抬高 —— 想抬高必须手改 JSON 并留下理由，而且会被
// `verify-ratchet-update.mjs` 在 CI 里拦住。OmniRoute 的实测教训（issue #8584）：
// 抬高上限是十秒钟的 JSON 编辑、是解红最快的路；降低要有人主动跑 `--update`。
// 结果他们「下个周期收紧」写了 31 次、兑现 1 次，复杂度上限从 1794 走到 2169 只降过一次。
// 那条路本仓不走。
import { readFileSync, writeFileSync, readdirSync, existsSync, statSync } from "node:fs";
import { join, sep } from "node:path";

const BASELINE = "config/quality/ratchet.json";
const UPDATE = process.argv.includes("--update");

const SCAN = [
  { dir: "src-tauri/src", ext: /\.rs$/ },
  { dir: "src", ext: /\.tsx?$/, skip: /\.test\.tsx?$/ },
];
const SKIP_DIRS = new Set(["node_modules", "target", "dist", "testdata"]);

// 「生产段 / 测试段」切分判据的实现与踩坑记录都在 lib/rust-source.mjs。
// 它同时被 check-forbidden.mjs 使用 —— 抄第二份必然漂移（已经发生过一次：
// 那边曾是只认裸 `mod tests` 的收窄版，会把测试段当成生产段去扫）。
// 仍然 re-export，因为已有调用方按 `check-ratchet.mjs` 的名字导入。
import { testModStartLine, prodLines } from "./lib/rust-source.mjs";
export { testModStartLine, prodLines };

function walk(dir, ext, skip, acc = []) {
  if (!existsSync(dir)) return acc;
  for (const e of readdirSync(dir, { withFileTypes: true })) {
    if (e.isDirectory()) {
      if (!SKIP_DIRS.has(e.name)) walk(join(dir, e.name), ext, skip, acc);
    } else if (ext.test(e.name) && !(skip && skip.test(e.name))) {
      acc.push(join(dir, e.name).split(sep).join("/"));
    }
  }
  return acc;
}

function measureFiles() {
  const out = {};
  for (const { dir, ext, skip } of SCAN) {
    for (const f of walk(dir, ext, skip)) {
      out[f] = prodLines(readFileSync(f, "utf8"));
    }
  }
  return out;
}

// ---------------------------------------------------------------------------
// 计数判据（非行数）。每条都是「本仓已经用文字记下、但没有机械判据」的规则。
// ---------------------------------------------------------------------------

/** 去掉行注释后再数，避免文档里提到某个模式时被计入。 */
function countInFile(file, re) {
  if (!existsSync(file)) return 0;
  return readFileSync(file, "utf8")
    .split("\n")
    .filter((l) => !l.trim().startsWith("//") && !l.trim().startsWith("///"))
    .filter((l) => re.test(l)).length;
}

const COUNTERS = {
  // docs/14 勘误 + docs/15 P1-2：裸 `self.persist()` 落盘失败即「内存领先磁盘」，
  // 该方向永不自愈。文档已确认现存的几处都是刻意例外（唯一落盘点本身 +
  // flush_health_if_dirty）。这个棘轮不要求清零，只保证**不再长回去**。
  bare_persist_calls: {
    measure: () => countInFile("src-tauri/src/store.rs", /\bself\.persist\(\)/),
    desc: "store.rs 里的裸 self.persist()（应走 mutate_and_persist；现存几处是文档记录的刻意例外）",
  },
};

function measureCounters() {
  const out = {};
  for (const [k, v] of Object.entries(COUNTERS)) out[k] = v.measure();
  return out;
}

// ---------------------------------------------------------------------------

if (!existsSync(BASELINE)) {
  if (!UPDATE) {
    console.error(`❌ 找不到 ${BASELINE}。首次生成：node scripts/check-ratchet.mjs --update`);
    process.exit(2);
  }
  const files = measureFiles();
  const cap = 900;
  writeFileSync(
    BASELINE,
    JSON.stringify(
      {
        $comment:
          "体积/计数棘轮基线。数字只准降。想抬高必须手改并写理由，CI 的 verify-ratchet-update 会拦。详见 scripts/check-ratchet.mjs",
        newFileCapLines: cap,
        frozen: Object.fromEntries(
          Object.entries(files)
            .filter(([, n]) => n > cap)
            .sort(([a], [b]) => a.localeCompare(b))
        ),
        counters: measureCounters(),
      },
      null,
      2
    ) + "\n"
  );
  console.log(`✅ 已生成 ${BASELINE}`);
  process.exit(0);
}

const baseline = JSON.parse(readFileSync(BASELINE, "utf8"));
const cap = baseline.newFileCapLines;
const frozen = baseline.frozen ?? {};
const files = measureFiles();
const counters = measureCounters();

const violations = [];
const improvements = [];
const redundant = [];

for (const [f, n] of Object.entries(files)) {
  if (f in frozen) {
    if (n > frozen[f]) {
      violations.push(
        `${f}: 生产段 ${n} 行 > 冻结值 ${frozen[f]}（只准降）\n` +
          `      → 抽个函数/子模块出去把它变小；实在必要就手改 ${BASELINE} 并写清理由`
      );
    } else if (n < frozen[f]) improvements.push([f, n]);
    else if (n <= cap) redundant.push(f);
  } else if (n > cap) {
    violations.push(
      `${f}: 生产段 ${n} 行 > 新文件上限 ${cap}（新文件不得超限）\n` +
        `      → 一出生就 ${n} 行意味着它注定长成下一个 5000 行文件`
    );
  }
}

for (const [k, spec] of Object.entries(baseline.counters ?? {})) {
  const now = counters[k];
  if (now === undefined) {
    console.log(`⚠️  基线里的计数判据 ${k} 已无对应度量（COUNTERS 里被删了？）`);
    continue;
  }
  if (now > spec) {
    violations.push(`计数 ${k}: ${now} > 冻结值 ${spec}（只准降）\n      → ${COUNTERS[k].desc}`);
  } else if (now < spec) improvements.push([`counter:${k}`, now]);
}

if (UPDATE) {
  if (violations.length) {
    console.error("❌ 有违规，拒绝更新基线（先修好再棘轮，否则等于自动放宽）");
    violations.forEach((v) => console.error("   ✗ " + v));
    process.exit(1);
  }
  let changed = false;
  for (const [f, n] of improvements) {
    if (f.startsWith("counter:")) {
      baseline.counters[f.slice(8)] = n;
      changed = true;
    } else if (n <= cap) {
      delete frozen[f]; // 已降到 cap 以内 → 退出 baseline
      changed = true;
    } else {
      frozen[f] = n;
      changed = true;
    }
  }
  // 没降但已经在 cap 以内的条目也要清掉。
  // OmniRoute 踩过这个坑（#8584）：这类条目原先只在「本轮降过」的分支里才被移除，
  // 于是一个恰好等于自身上限的条目会永远留在 baseline —— 最夸张的一条 19 行代码
  // 背着 2523 的上限（132×），把每一次已完成的拆分变成给下一个人的增长额度。
  for (const f of redundant) {
    delete frozen[f];
    changed = true;
  }
  if (changed) {
    baseline.frozen = Object.fromEntries(Object.entries(frozen).sort(([a], [b]) => a.localeCompare(b)));
    writeFileSync(BASELINE, JSON.stringify(baseline, null, 2) + "\n");
    console.log(
      `✅ 基线已向下棘轮：${improvements.length} 项变小，${redundant.length} 条冗余条目移除`
    );
  } else {
    console.log("✅ 无可棘轮项（当前值都等于基线）");
  }
  process.exit(0);
}

if (violations.length) {
  console.error(`❌ 棘轮：${violations.length} 项违规`);
  violations.forEach((v) => console.error("   ✗ " + v));
  process.exit(1);
}

const pending = improvements.length + redundant.length;
console.log(
  `✅ 棘轮：${Object.keys(frozen).length} 个冻结文件 + ${Object.keys(baseline.counters ?? {}).length} 项计数，` +
    `新文件上限 ${cap} 行（共查 ${Object.keys(files).length} 个文件）` +
    (pending ? `\n   💡 有 ${pending} 项已变好，跑 \`npm run ratchet:update\` 把它锁住` : "")
);
