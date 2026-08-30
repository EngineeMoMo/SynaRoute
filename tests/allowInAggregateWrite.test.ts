import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { inspectableLines } from "../scripts/lib/rust-source.mjs";

/**
 * 「允许大脑聚合使用」那个 checkbox **必须走专用 IPC，不许走整份 `upsertKey`**。
 *
 * ## 为什么这条缝需要一个判据
 *
 * Rust 侧的 `store/key_flags.rs` 有两条行为用例（只翻那一位 / Key 不存在报 NotFound），
 * 但它们都**直调函数**。把 `KeyCard.tsx` 那一行改回
 * `api.upsertKey({ ...k, allowInAggregate })`，那两条照样全绿 —— 而那正是缺陷本身：
 * `upsert_key` 是整份替换，只沿用库里的 `health` 与 `cached_balance` 两项运行态，
 * 于是卡片握着的旧快照会把后端刚探测到的余额端点（`balance_query.url`，
 * 由 `Store::set_balance_query_url` 写回）顶成旧值。
 *
 * 这是本仓第 12 次盯同一类接线盲区（前例：`mcp::handle_http` / `route_meta` /
 * `lan_guard` 的 peer / `log_rotate` 的写线程 / `custom_headers` / `model_choice::pick` /
 * `record_stream_end`…）：**单元覆盖了组件 ≠ 覆盖了调用它的那条线**，
 * 而漏掉接线的表现恰恰是静默的。
 *
 * 放在 `tests/` 而非 Rust 侧：判据要读 `.tsx`，跨语言这条缝 Rust 反查不到
 * （理由同 `reservedHeadersParity.test.ts` / `aggregateDisabledKeyHint.test.ts`）。
 *
 * ## 判据边界
 *
 * 只查**写这一位**的那条路径。`KeyEditor` 保存整个 Key 时**照样**走 `upsertKey`
 * （那本来就是整份替换的正当场合），本判据不碰它 —— 一个部分为真的判据必须把边界写明。
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

const CARD = "../src/components/KeyCard.tsx";
const BRIDGE = "../src/lib/bridge.ts";
const CMD = "set_key_allow_in_aggregate";

describe("allowInAggregate 的写入必须是单字段的", () => {
  it("KeyCard 里一处 upsertKey 都不许有", () => {
    const card = production(CARD);
    // 先确认判据还有目标：那个 checkbox 得在，否则「没有 upsertKey」是空洞的绿。
    expect(
      card,
      "找不到那个 checkbox —— 判据失去目标，先修判据（开关搬家了就把 CARD 改掉）",
    ).toContain("checked={!!k.allowInAggregate}");
    expect(
      card.includes("upsertKey"),
      "KeyCard 又走回整份 upsert 了：它会用卡片的旧快照顶掉后端刚写回的 balance_query.url。" +
        "理由全文见 src-tauri/src/key_flags.rs 的模块注释",
    ).toBe(false);
  });

  it("那一行调的是专用 IPC，且 bridge 把它接到了后端命令名上", () => {
    const card = production(CARD);
    expect(card, "checkbox 的 onChange 必须调 api.setKeyAllowInAggregate").toContain(
      "api.setKeyAllowInAggregate(",
    );

    // 前端方法名 → 后端命令名这一跳。名字拼错**没有编译错误**（`call` 收的是字符串），
    // 只在用户点到那个 checkbox 时炸。策略门 `invoke-command-must-exist` 查的是
    // 「这个字符串在 Rust 侧有 #[tauri::command]」，此处补的是「bridge 真的用了它」。
    const bridge = production(BRIDGE);
    expect(bridge, "bridge.ts 里没有 setKeyAllowInAggregate").toContain("setKeyAllowInAggregate:");
    const at = bridge.indexOf("setKeyAllowInAggregate:");
    expect(
      bridge.slice(at, at + 400),
      `setKeyAllowInAggregate 必须调后端命令 ${CMD}`,
    ).toContain(`"${CMD}"`);
  });

  it("失败必须弹 toast 并重载，不许静默吞掉", () => {
    // 勾选态由 `k.allowInAggregate` 驱动（受控组件），写失败又不提示的表现是
    // 「勾了一下、重载后自己弹回去」，而用户拿不到任何线索
    //（主口令锁定、落盘失败都会走到这里）。
    const card = production(CARD);
    const at = card.indexOf("api.setKeyAllowInAggregate(");
    expect(at, "找不到调用点 —— 判据失去目标").toBeGreaterThan(0);
    const around = card.slice(Math.max(0, at - 200), at + 400);
    expect(around, "写失败必须 showToast").toContain("showToast(");
    expect(around, "写成功必须 loadCategory 重载，否则卡片停在旧值").toContain("loadCategory(");
  });
});
