import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { inspectableLines } from "../scripts/lib/rust-source.mjs";

/**
 * 「Key 已禁用，无法作为决策者/汇总者」那句报错**必须指对地方**。
 *
 * 本仓的纪律是「指错方向的提示比没有提示更糟」（`lan_guard` 的 401 文案就为此把判据从
 * 「提到令牌」升级成「**指对地方**」）。这条错误里那句出路是唯一告诉用户
 * 「允许大脑聚合使用」这个开关在哪的地方 —— 而那个开关**只在分类页的 Key 卡片上**
 * （`KeyCard.tsx`，且只在该 Key 已禁用时才渲染），**不在 Key 编辑器里**。
 *
 * 第一版文案写的就是「在 Key 编辑器里打开」，照着去找是找不到的：
 * 用户会把编辑器每个页签翻一遍，然后得出「这个开关不存在」的结论。
 *
 * 判据刻意做成**双向**的：开关搬到哪个界面，文案就必须跟着说哪个界面。
 * 单向判据（只查「别写编辑器」）在开关真的搬进编辑器那天会变成假警。
 *
 * 放在 `tests/` 而非 Rust 侧：判据要同时读 Rust 文案与两个 `.tsx`，
 * 而跨语言这条缝编译器管不到，理由同 `reservedHeadersParity.test.ts`。
 */

const here = dirname(fileURLToPath(import.meta.url));
const read = (rel: string) => readFileSync(join(here, rel), "utf8");

/** 只看生产段的非注释行 —— 本仓已 5 次栽在「注释里的字面量满足了断言」上。 */
function production(rel: string): string {
  const src = read(rel);
  return inspectableLines(src, rel)
    .map((l: { text: string }) => l.text)
    .join("\n");
}

/** 从 `aggregate.rs` 的生产段里抽出那句报错的全文（跨行字符串已拼回一行）。 */
function disabledKeyHint(): string {
  const prod = production("../src-tauri/src/aggregate.rs");
  const anchor = "无法作为决策者/汇总者调用";
  const at = prod.indexOf(anchor);
  // 解析不到就主动失败：文案改写或函数搬家时，一个恒绿的判据比没有判据更糟
  // （本仓 `invoke-command-must-exist` 那条门就是这么踩过的）。
  expect(at, "没在 aggregate.rs 的生产段里找到那句报错 —— 判据失去目标，先修判据").toBeGreaterThan(0);
  const tail = prod.slice(at);
  const end = tail.indexOf('",');
  expect(end, "那句报错没有正常收尾，抽取逻辑该跟着改").toBeGreaterThan(0);
  // Rust 的续行转义 `\` + 前导空白拼成一行，便于按词判定
  return tail.slice(0, end).replace(/\\\s*\n\s*/g, "").replace(/\s+/g, " ");
}

/** 这个开关的 UI 宿主：文件 → 文案里必须出现的词。 */
const HOSTS: { rel: string; word: string; label: string }[] = [
  { rel: "../src/components/KeyCard.tsx", word: "卡片", label: "分类页的 Key 卡片" },
  { rel: "../src/components/KeyEditor.tsx", word: "编辑器", label: "Key 编辑器" },
];

/**
 * 这个文件里**真的渲染了一个控件**吗？
 *
 * 🔴 判据刻意不是「文件里提到了 allowInAggregate」—— 那个维度是错的：
 * `KeyEditor` 的保存草稿里有一行纯透传（`allowInAggregate: initial?.allowInAggregate,`，
 * 见 [tests/providerKeyDraftParity.test.ts] 那条判据的来由），它**必须**在，
 * 但编辑器里并没有那个勾选框。按「提到就算宿主」判会得出「文案该写编辑器」，
 * 于是这条判据会**催着人把用户送进空房间** —— 正是它自己写来防的那件事。
 *
 * 所以要求同一行上既有字段、又有控件绑定的形态。
 */
function rendersAControl(rel: string): boolean {
  const CONTROL = ["checked=", "onCheckedChange", "<input", "<Switch", "ToggleRow"];
  return production(rel)
    .split("\n")
    .some((l) => l.includes("allowInAggregate") && CONTROL.some((c) => l.includes(c)));
}

describe("「Key 已禁用」报错必须指向真正能改那个开关的界面", () => {
  it("文案提到的界面 = 真正渲染那个开关的界面（双向）", () => {
    const hint = disabledKeyHint();
    const hosts = HOSTS.filter((h) => rendersAControl(h.rel));

    expect(
      hosts.length,
      "没有任何界面渲染 allowInAggregate —— 那这个开关是个死字段（后端读、前端无入口）",
    ).toBeGreaterThan(0);

    for (const h of hosts) {
      expect(hint, `开关在${h.label}里，文案必须提到「${h.word}」`).toContain(h.word);
    }
    for (const h of HOSTS.filter((x) => !hosts.includes(x))) {
      expect(hint, `开关不在${h.label}里，文案不许提到「${h.word}」，那是把人送去空房间`).not.toContain(
        h.word,
      );
    }
  });

  it("文案里必须出现开关的原文标签（否则用户按字搜不到那个开关）", () => {
    const hint = disabledKeyHint();
    const i18n = read("../src/lib/i18n.ts");
    // 后端错误消息里嵌的是中文原文标签，用户要拿它到界面上按字找。
    // 标签改了却忘了改这句 → 用户搜不到，与指错地方等价。
    //
    // zh/en 对称性**不在这里查** —— `src/lib/i18n.test.ts` 已有那道门
    //（含跨分片重复与「分片没被展开」两条）。两处各写一份必然漂移。
    const zhLabel = /"key\.allowInAggregate":\s*"([^"]+)"/.exec(i18n)?.[1];
    expect(zhLabel, "i18n 里找不到 key.allowInAggregate").toBeTruthy();
    expect(hint).toContain(zhLabel!);
  });

  it("三条「Key 被禁用」的用户可见文案口径一致，没有一条只说「重新启用」", () => {
    /**
     * 三条路径各有自己的文案，而它们**只有一条**在这一轮被改过：
     * - `call_ref`：决策者/汇总者被禁用（返回给客户端的 Err）
     * - `gather_members`：成员被禁用（落进日志页「大脑聚合」组的事件）
     * - `mcp.rs`：聚合结果 Markdown 里那句「N 个成员因所属 Key 已停用而跳过」
     *
     * 🔴 只说「重新启用」是**指去做一个已知有害的操作**：重新启用会让这条 Key 回到故障
     * 转移池，把当初禁用它的那个 404 带回主链路 —— 而用户手里有一个代价为零的选项
     * （勾那个 checkbox）。所以判据是：凡是给出路的文案，都必须同时提到那个勾选框。
     */
    const sites: { rel: string; anchor: string; what: string }[] = [
      { rel: "../src-tauri/src/aggregate.rs", anchor: "参与者已禁用，跳过", what: "成员被跳过的事件" },
      { rel: "../src-tauri/src/mcp.rs", anchor: "个成员因所属 Key 已停用而跳过", what: "聚合结果里的注" },
    ];
    for (const s of sites) {
      const prod = production(s.rel);
      const at = prod.indexOf(s.anchor);
      expect(at, `找不到「${s.what}」的文案 —— 判据失去目标，先修判据`).toBeGreaterThan(0);
      const line = prod.slice(at, prod.indexOf("\n", at));
      expect(line, `${s.what}：提到「重新启用」就必须同时给出勾选框那条出路`).toContain("卡片");
    }
    // 第三条（call_ref 的报错）由本文件第一条 it 用同一套 HOSTS 判据盯着
    expect(disabledKeyHint()).toContain("卡片");
  });

  it("文案声称「只在已禁用时显示」，那就必须真的只在已禁用时渲染", () => {
    const hint = disabledKeyHint();
    if (!hint.includes("已禁用时显示")) return; // 文案没这么承诺就不必校验
    const card = production("../src/components/KeyCard.tsx");
    // 锚点必须是**那个 checkbox 本身**，不是「文件里第一次出现这个字段」——
    // 后者会命中别的用途（例如余额自动查询的口径 `k.enabled || !!k.allowInAggregate`），
    // 于是判据要么误报、要么恒真。第一版就是这么坏的。
    const box = card.indexOf("checked={!!k.allowInAggregate}");
    expect(box, "找不到那个 checkbox —— 判据失去目标，先修判据").toBeGreaterThan(0);
    // 它整段被 `{!k.enabled && (...)}` 包着。去掉这道门，启用状态下也会冒出一个开关，
    // 而那时它对聚合毫无影响（启用的 Key 本来就能参与）—— 文案随之变成假话。
    // 取「紧邻上方那个 <Switch>（启用开关）到 checkbox 之间」这一段，门必须在其中。
    const from = card.lastIndexOf("<Switch", box);
    expect(from, "找不到启用开关 <Switch> —— 判据失去参照物").toBeGreaterThan(0);
    expect(
      card.slice(from, box),
      "KeyCard 里那个 checkbox 必须被 `{!k.enabled && …}` 门住，否则文案在撒谎",
    ).toContain("!k.enabled &&");
  });
});
