import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

/**
 * 用量页「实时性」与「零用量新 Key」两条链的判据。
 *
 * 两者都是用户 2026-09-09 实报的：「用量统计也不是实时，新加的 key 都看不见」。
 * 成因是**两个独立缺陷叠加**，判据也必须分开钉住，否则修了一个会让人以为全好了：
 *
 * 1. **实时性**：后端在收到 usage 的同一次调用内就更新了内存累计，`logs`/`config`
 *    推送也一直在发 —— 只是本页从没订阅过（裸 `setInterval(30s)`）。这是**接线缺失**。
 * 2. **新 Key 不出现**：后端行集合曾是 `usage_totals` 的投影，而正常采集刻意不建零桶，
 *    于是没跑过请求的 Key 压根没有行 —— **手动刷新也没用**。这是**行集合语义**。
 *
 * 🔴 本仓无 jsdom，组件行为测不到；而 Rust 侧的行为用例直接调 `rows()`，
 * 「页面没订阅事件」「占位行被算进无价横幅」这类缺陷它们照样全绿。故用源码级判据。
 */

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const read = (...p: string[]) => readFileSync(join(ROOT, ...p), "utf8");

const USAGE_PAGE = read("src", "pages", "UsagePage.tsx");
const USAGE_COST_RS = read("src-tauri", "src", "usage_cost.rs");
const TYPES_TS = read("src", "types.ts");
const I18N_USAGE = read("src", "lib", "i18n.usage.ts");
const MOCK_USAGE = read("src", "lib", "mockData.usage.ts");

/** 剥掉注释再扫（本仓已六次栽在「注释里的字面量满足了断言」上）。 */
function code(src: string): string {
  return src
    .replace(/\/\*[\s\S]*?\*\//g, "")
    .split("\n")
    .filter((l) => !/^\s*(\/\/|\*)/.test(l))
    .join("\n");
}

describe("用量页的刷新接线", () => {
  const page = code(USAGE_PAGE);

  it("🔴 必须订阅 logs 与 config 两个主题", () => {
    const m = page.match(/useBackendEvent\(\[([^\]]*)\]/);
    expect(m, "本页必须订阅后端推送，否则数据早就新了、界面还是旧的").not.toBeNull();
    const topics = m![1];
    // `logs`：一次转发记完账就发（流式在流末补记时也发）→ 用量数字跟着涨。
    expect(topics, "缺 logs：转发完成后用量最长 30s 才更新").toContain('"logs"');
    // `config`：Key 增删改名/改倍率会发 → **新加的 Key 也在这时出现**。
    expect(topics, "缺 config：新加的 Key 要等兜底轮询才出现").toContain('"config"');
  });

  it("🔴 轮询降级为兜底，且必须是 visibility-aware 的 usePolling", () => {
    expect(page, "要用共享的 usePolling").toMatch(/usePolling\(/);
    expect(page, "周期取共享常量，不许再写 30_000 字面量").toMatch(/FALLBACK_POLL_MS/);
    // 裸 setInterval 在最小化到托盘后仍在跑，而那是本应用的常态使用姿势。
    expect(page, "不许再用裸 setInterval").not.toMatch(/setInterval/);
  });

  it("🔴 在途请求要有代际号，旧结果不许覆盖新结果", () => {
    // 事件推送与兜底轮询可能几乎同时触发两轮 load，先发的后到就会写回更旧的快照
    // —— 表现是「刚发的请求，用量数字反而退回去了」。
    expect(page, "要有代际 ref").toMatch(/genRef/);
    expect(page, "每轮开始时自增").toMatch(/\+\+genRef\.current/);
    expect(page, "提交前要比对代际").toMatch(/gen !== genRef\.current/);
  });
});

describe("零用量新 Key 的呈现", () => {
  it("Rust / TS / mock 三处都有 hasRecordedUsage 这一位", () => {
    expect(code(USAGE_COST_RS), "Rust 侧字段").toMatch(/has_recorded_usage: bool/);
    expect(code(TYPES_TS), "TS 侧字段").toMatch(/hasRecordedUsage\?: boolean/);
    // mock 不铺这个分支，浏览器预览里就看不到它的样式与文案（必然做漏）。
    expect(code(MOCK_USAGE), "mock 要造一条零用量行").toMatch(/hasRecordedUsage: false/);
  });

  it("🔴 行集合是并集：配置里的 Key 也要产出行", () => {
    const rs = code(USAGE_COST_RS);
    // 并集的实现形态：历史桶先入表，再为活着的 Key 补占位（不覆盖已有桶）。
    expect(rs, "要有补占位那一步").toMatch(/or_insert_with/);
    expect(rs, "占位行的 usage 是全零").toMatch(/TokenUsage::default\(\)/);
  });

  it("🔴 金额显示「尚无用量」而不是 $0，且给出条件句说明", () => {
    const page = code(USAGE_PAGE);
    expect(page, "要按这一位分派").toMatch(/!row\.hasRecordedUsage/);
    expect(page, "要用专门的文案").toMatch(/usage\.noUsageYet/);
    // 两条文案各自要有 zh/en。
    for (const key of ['"usage.noUsageYet"', '"usage.noUsageYetHint"']) {
      const hits = I18N_USAGE.split(key).length - 1;
      expect(hits, `${key} 应各有 zh/en 两条，实测 ${hits}`).toBe(2);
    }
    // 🔴 成因有三种（尚未使用 / 流仍在进行 / 上游不回 usage），我们**分辨不出**，
    // 故文案必须是条件句 —— 断言它把三种都列出来了，而不是挑一种说成结论。
    const hint = I18N_USAGE.slice(I18N_USAGE.indexOf('"usage.noUsageYetHint"'));
    const zhHint = hint.slice(0, hint.indexOf("\n  \""));
    expect(zhHint, "要提到「尚未使用」这一种可能").toMatch(/尚未被使用|尚未使用/);
    expect(zhHint, "要提到流式仍在进行").toMatch(/流/);
    expect(zhHint, "要提到上游可能不回 usage").toMatch(/不返回|不回/);
  });

  it("🔴 占位行不计入「未计入金额的条数」，也不进无价横幅", () => {
    const page = code(USAGE_PAGE);
    // 那个角标在说「有 N 行的钱没算进这个总数」，而占位行压根没有消耗可算。
    // 把它们算进去，用户会去横幅里找原因 —— 而横幅里一条也没有（后端不给它们成因）。
    expect(page, "汇总时要跳过占位行").toMatch(/if \(!r\.hasRecordedUsage\) continue;/);
    // 后端那一半：占位行不许带 unpricedReason，也不许算出金额。
    //
    // ⚠️ 判据钉**性质**不钉写法：第一版写死 `if !has_recorded_usage {\s*None`，
    // 而为消掉 clippy 的 `if_same_then_else` 把两支合并成
    // `if !has_recorded_usage || cost_nano.is_some()` 之后它当场变红 —— 那是**假红**，
    // 代价是下一个人把这个正确的改进撤回去。本仓已为「钉写法」栽过六次。
    const rs = code(USAGE_COST_RS);
    const reasonAt = rs.indexOf("let unpriced_reason =");
    expect(reasonAt, "找不到成因判定").toBeGreaterThan(0);
    expect(
      rs.slice(reasonAt, reasonAt + 200),
      "成因判定必须以 has_recorded_usage 为前提，否则占位行会被报成「算不出金额」",
    ).toMatch(/has_recorded_usage/);
    // 金额也要清掉：`estimate_cost` 对全零 usage 会算出 Some(0)，显示成 $0.0000。
    const costAt = rs.indexOf("let cost_nano = if has_recorded_usage");
    expect(costAt, "占位行的 cost_nano 必须被清成 None，否则界面显示 $0.0000").toBeGreaterThan(0);
  });

  it("空态文案不再说「有转发流量后才会出现」", () => {
    // 配了 Key 之后表格就不是空的（它们以「尚无用量」的形态出现），旧文案会让用户
    // 以为得先跑一次请求才能看到自己的 Key —— 而他报的正是「新加的 key 看不见」。
    const zh = I18N_USAGE.slice(0, I18N_USAGE.indexOf("export const usageEn"));
    const empty = zh.slice(zh.indexOf('"usage.empty"'));
    expect(empty.slice(0, empty.indexOf("\n")), "空态口径要改成「没配 Key 且无历史」").not.toMatch(
      /有转发流量/,
    );
  });
});
