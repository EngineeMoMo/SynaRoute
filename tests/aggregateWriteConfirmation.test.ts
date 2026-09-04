// 大脑聚合「落盘前必须先给用户看预览」这条链路的**跨语言接线判据**。
//
// 后端那两道防线都**依赖前端如实接线**，而漏掉的表现全是静默的：
//
// 1. **两步确认**：`aggregate_execute` 现在只出预览、不写盘，真正落盘是 `aggregate_write`。
//    前端要是把两者连着调（不给用户看清单的机会），后端一个字都察觉不到 ——
//    用户又回到「确认了计划文本，实际写了没见过的字节」那个状态。
// 2. **mtime 防线**：后端靠 `planStartedMs` 判断「目标文件在用户确认期间被改过吗」，
//    而那个值只能由前端从 Phase1 的返回里带过来。不回传 = `changed_since` 收到 0
//    = 整道防线跳过，Rust 侧所有用例照样全绿（它们直接传参数）。
//
// 这是本仓第 17 次盯同一类接线盲区（前几次：route_meta 的每个出口 / lan_guard 的 peer /
// log_rotate 的写线程 / custom_headers 的保存 payload / model_choice 的转发路径 /
// key_flags 的 checkbox / gate 的信号量 / brain_config 的落盘点）。
import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

const read = (p: string) => readFileSync(resolve(process.cwd(), p), "utf8");

/** 剥掉 `//` 行注释与 `/* *\/` 块注释 —— 判据说「代码里必须这么写」，就只能看代码。 */
function codeOnly(src: string): string {
  return src
    .replace(/\/\*[\s\S]*?\*\//g, "")
    .split(/\r?\n/)
    .map((l) => {
      const i = l.indexOf("//");
      return i >= 0 ? l.slice(0, i) : l;
    })
    .join("\n");
}

const bridge = codeOnly(read("src/lib/bridge.ts"));
const panel = codeOnly(read("src/components/BrainRunPanel.tsx"));
const writeRs = read("src-tauri/src/aggregate/write.rs");
const aggRs = read("src-tauri/src/aggregate.rs");
const libRs = read("src-tauri/src/lib.rs");

describe("落盘前的两步确认", () => {
  it("bridge 暴露的是「预览」与「落盘」两个动作，而不是一个「执行」", () => {
    expect(bridge).toContain("runAggregatePreview:");
    expect(bridge).toContain("runAggregateWrite:");
    // 旧名字对应「一点就写」的语义。留着它等于留了一条绕过预览的路。
    expect(bridge).not.toContain("runAggregateExecute");
  });

  it("两个 IPC 命令都在 Rust 侧注册了", () => {
    // 正向（前端调的名字有对应命令）由 check:forbidden 的 invoke-command-must-exist 管；
    // 这里管的是**反向**：命令写了但没进 generate_handler 时，只有用户点到才炸。
    //
    // 命令**定义在 aggregate.rs**（跟着实现走，同 key_flags / codex_catalog），
    // 注册在 lib.rs 里用全路径形式。判据分两处查 —— 这两个位置各自漏一个都会静默失效。
    for (const cmd of ["aggregate_execute", "aggregate_write"]) {
      expect(bridge, `${cmd} 应被 bridge 调用`).toContain(`"${cmd}"`);
      expect(aggRs, `${cmd} 应有 #[tauri::command] 定义`).toContain(`pub async fn ${cmd}(`);
      expect(libRs, `${cmd} 必须进 generate_handler`).toMatch(
        new RegExp(`^\\s+aggregate::${cmd},$`, "m"),
      );
    }
  });

  it("面板必须先拿到预览才可能落盘", () => {
    // 写入按钮只在 preview 阶段出现，且它的 handler 用的是**预览返回的原文**。
    expect(panel).toMatch(/phase === "preview"[\s\S]{0,400}runWrite\(\)/);
    expect(panel).toContain("preview.content");
    // 反面：runAggregateWrite 只能有一个调用点（在 runWrite 里）。
    expect(panel.match(/api\.runAggregateWrite\(/g)?.length ?? 0).toBe(1);
    // 而 runPlan 之后**不能**直接调它 —— 中间必须经过 runPreview。
    const planThenWrite = /runAggregatePlan\([\s\S]{0,600}?runAggregateWrite\(/.test(panel);
    expect(planThenWrite, "计划阶段之后不许直接落盘").toBe(false);
  });

  it("planStartedMs 必须一路回传，否则 mtime 防线静默失效", () => {
    // 后端那道防线的入口
    expect(writeRs).toContain("fn changed_since(");
    expect(writeRs).toContain("plan_started_ms");
    // 前端：从 Phase1 收下 → 存起来 → 两个后续阶段都带上
    expect(panel).toContain("res.planStartedMs");
    expect(panel).toContain("setPlanStartedMs");
    expect(panel.match(/planStartedMs,/g)?.length ?? 0).toBeGreaterThanOrEqual(2);
    // bridge 的两个签名都必须收它
    expect(bridge.match(/planStartedMs: number,/g)?.length ?? 0).toBe(2);
  });

  it("工作目录同样一路回传（Phase1 定下，后两阶段不许重新解析）", () => {
    // 空字符串哨兵：Phase1 明确「无工作目录」。丢掉它会让后端回退实时解析，
    // 也就是往一个用户从未指定过的目录写文件。
    expect(panel).toContain('res.workDir ?? ""');
    expect(bridge.match(/workDir: string \| undefined,/g)?.length ?? 0).toBe(2);
  });
});
