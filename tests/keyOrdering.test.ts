import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { dropAnchorAt, reorderIds, type CardBox } from "../src/lib/keyOrdering";

/**
 * Key 拖放排序的判据：**纯逻辑** + **接线**。
 *
 * 🔴 接线那半不可省，这是本仓第 23 次同一类盲区：Rust 侧 `key_order` 的 6 条行为用例
 * 全都**直接调函数**，于是「把手没接上」「store 里改回循环调 moveKey」「命令没进
 * generate_handler」这三种缺陷它们**照样全绿** —— 而那正是「拖了没反应」这个缺陷本身。
 *
 * 本仓无 jsdom，故命中判定抽成纯函数（`dropAnchorAt`）单测，其余用源码级判据。
 */

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const read = (...p: string[]) => readFileSync(join(ROOT, ...p), "utf8");

const KEY_CARD = read("src", "components", "KeyCard.tsx");
const CATEGORY_PAGE = read("src", "pages", "CategoryPage.tsx");
const USE_KEY_DRAG = read("src", "lib", "useKeyDrag.ts");
const STORE_TS = read("src", "store.ts");
const BRIDGE = read("src", "lib", "bridge.ts");
const LIB_RS = read("src-tauri", "src", "lib.rs");
const KEY_ORDER_RS = read("src-tauri", "src", "key_order.rs");
const MOCK = read("src", "lib", "mockData.ts");

/** 剥掉注释再扫：本仓已六次栽在「注释里的字面量满足了断言」上。 */
function code(src: string): string {
  return src
    .replace(/\/\*[\s\S]*?\*\//g, "")
    .split("\n")
    .filter((l) => !/^\s*(\/\/|\*)/.test(l))
    .join("\n");
}

/** 等高卡片，从 y=0 开始每张 100px（中线在 50 / 150 / 250…）。 */
function boxes(ids: string[]): CardBox[] {
  return ids.map((id, i) => ({ id, top: i * 100, bottom: i * 100 + 100 }));
}

describe("拖放命中判定（dropAnchorAt）", () => {
  const bs = boxes(["a", "b", "c"]);

  it("停在某张卡的上半 → 插到它之前；下半 → 插到下一张之前", () => {
    // 拖 c：停在 a 的上半（y=10）→ 锚点 a
    expect(dropAnchorAt(bs, "c", 10)).toBe("a");
    // 停在 a 的下半（y=90）→ 越过 a 的中线 → 锚点 b
    expect(dropAnchorAt(bs, "c", 90)).toBe("b");
  });

  it("低于所有卡片的中线 → 插到末尾（undefined）", () => {
    expect(dropAnchorAt(bs, "a", 260)).toBeUndefined();
    expect(dropAnchorAt(bs, "a", 9999)).toBeUndefined();
  });

  it("🔴 source 自己必须被排除，否则拖动看起来毫无反应", () => {
    // 拖 a、停在自己身上（y=10）：若不排除自己，锚点会算成 "a" ——
    // 后端把「插到自己之前」当 no-op，于是用户拖了半天什么都没发生。
    expect(dropAnchorAt(bs, "a", 10)).toBe("b");
    // 拖 b 停在 b 的上半：排除自己后锚点应是 c（越过了 b 的位置）
    expect(dropAnchorAt(bs, "b", 110)).toBe("c");
  });

  it("空列表 / 只有自己一张卡 → 末尾", () => {
    expect(dropAnchorAt([], "a", 50)).toBeUndefined();
    expect(dropAnchorAt(boxes(["a"]), "a", 50)).toBeUndefined();
  });
});

describe("乐观重排（reorderIds）", () => {
  const ids = ["a", "b", "c", "d"];

  it("末尾拖到首位 / 首位拖到末尾", () => {
    expect(reorderIds(ids, "d", "a")).toEqual(["d", "a", "b", "c"]);
    expect(reorderIds(ids, "a", undefined)).toEqual(["b", "c", "d", "a"]);
  });

  it("🔴 先摘除再定位锚点 —— 往下拖不能偏一格", () => {
    // 把 a 放到 c 之前：摘掉 a 后 c 在下标 1，结果 b,a,c,d。
    // 若「先定位再摘除」会算成下标 2 → b,c,a,d（偏一格，用户看到的插入线与落点不符）。
    expect(reorderIds(ids, "a", "c")).toEqual(["b", "a", "c", "d"]);
  });

  it("原地不动的三种情形都返回等价顺序", () => {
    expect(reorderIds(ids, "a", "a")).toEqual(ids); // 拖到自己身上
    expect(reorderIds(ids, "a", "b")).toEqual(ids); // 拖到原本的后继之前
    expect(reorderIds(ids, "d", undefined)).toEqual(ids); // 末尾那条再拖到末尾
  });

  it("不修改入参数组（调用方会把它当旧快照用）", () => {
    const input = [...ids];
    reorderIds(input, "d", "a");
    expect(input).toEqual(ids);
  });

  it("source 不在列表里 → 原样返回，不猜位置", () => {
    // 列表已被别的来源改过（切分类、磁盘自愈重载）时，凭陈旧快照猜一个位置比什么都不做糟。
    expect(reorderIds(ids, "zzz", "a")).toEqual(ids);
  });

  it("与后端语义一致：锚点为末尾之后 == 放末尾", () => {
    expect(reorderIds(ids, "b", undefined)).toEqual(["a", "c", "d", "b"]);
  });
});

describe("拖放接线（源码级判据）", () => {
  it("把手在 KeyCard 上，且只有把手能起拖（整卡不 draggable）", () => {
    const c = code(KEY_CARD);
    expect(c, "把手要挂 onPointerDown → 父级的 onDragHandleDown").toMatch(
      /onPointerDown=\{\(e\) => onDragHandleDown\(k\.id, e\)\}/,
    );
    // 🔴 整卡可拖会让卡片里的开关/编辑/复制/删除/余额刷新/可点地址全部变成「点一下就开始拖」。
    expect(c, "整张卡不许 draggable").not.toMatch(/draggable/);
    // HTML5 DnD 那条路已被明确放弃（WebView 里 ghost image / 自动滚动难控）。
    expect(c).not.toMatch(/dataTransfer|onDragStart|onDrop\b/);
  });

  it("🔴 上移/下移按钮必须保留 —— 拖放在键盘与辅助技术下不可达", () => {
    const c = code(KEY_CARD);
    expect(c, "上移按钮及其无障碍名").toContain('aria-label={t("key.moveUp")}');
    expect(c, "下移按钮及其无障碍名").toContain('aria-label={t("key.moveDown")}');
    expect(c, "两个按钮仍走 moveKey").toMatch(/moveKey\(k\.id, "up"\)/);
    expect(c, "两个按钮仍走 moveKey").toMatch(/moveKey\(k\.id, "down"\)/);
    expect(c, "把手也要有无障碍名").toContain('aria-label={t("key.dragHandle")}');
  });

  it("CategoryPage 把拖放状态接到每张卡上", () => {
    const c = code(CATEGORY_PAGE);
    expect(c, "本页要起拖放状态机").toMatch(/useKeyDrag\(/);
    expect(c, "把手回调要传下去").toContain("onDragHandleDown={drag.onHandleDown}");
    expect(c, "拖动中的卡要标出来").toContain("dragging={drag.draggingId === k.id}");
    expect(c, "插入线要标出来").toContain("dropBefore={drag.anchorId === k.id}");
    expect(c, "卡片 DOM 要登记，否则算不出命中").toMatch(/drag\.registerCard\(k\.id, el\)/);
    // 🔴 滚动容器必须接到 hook 上。不接的话长列表里「拖到视口外」就走不动，
    // 而那正是用户要这个功能的理由。
    expect(c, "scrollRef 必须挂在 overflow-y-auto 那个 div 上").toMatch(
      /ref=\{drag\.scrollRef\}/,
    );
  });

  it("🔴 一次拖放只提交一次，且不得循环调 moveKey", () => {
    const drag = code(USE_KEY_DRAG);
    // 提交点恰好一处（在 pointerup 里）。
    expect(drag.match(/reorderKey\(/g)?.length, "只应有一个提交点").toBe(1);
    // 🔴 循环调 moveKey = N 次落盘 + N 次客户端配置重写 + N 次托盘重建，
    // 中途失败留下半套顺序。那正是本功能要避免的。
    expect(drag, "拖放路径不许出现 moveKey").not.toMatch(/moveKey/);
    // pointermove 里不许提交（每帧一次 IPC）。
    expect(drag, "pointermove 只更新预览").not.toMatch(/onMove[\s\S]{0,400}reorderKey\(/);
    // 阈值判定必须在：没有它，「点一下把手」会被当成一次零距离拖放。
    expect(drag).toMatch(/DRAG_THRESHOLD_PX/);
  });

  it("取消路径齐全：Esc / pointercancel / 窗口失焦都不提交", () => {
    const drag = code(USE_KEY_DRAG);
    for (const ev of ["pointercancel", "blur", "keydown"]) {
      expect(drag, `要监听 ${ev}`).toContain(`addEventListener("${ev}"`);
      expect(drag, `要拆掉 ${ev} 监听（否则每次拖动泄漏一个）`).toContain(
        `removeEventListener("${ev}"`,
      );
    }
    expect(drag, "Esc 要能取消").toMatch(/Escape/);
    // 指针捕获：不设的话快速拖动时 pointerup 丢在别的元素上 → 卡在拖动态。
    expect(drag).toMatch(/setPointerCapture/);
    // ---- 边缘自动滚动 ----
    //
    // 不滚的话长列表「把末尾那条拖到首位」根本走不到（首位那张卡在视口外），
    // 而那正是用户要拖放的理由。
    //
    // ⚠️ 判据钉**行为**不钉常量名：第一版写 `toMatch(/EDGE_PX/)`，而注入时把
    // `const EDGE_PX` 改名成 `EDGE_PX_DISABLED` 它**照样绿**（子串仍然命中）。
    // 现在钉的是「真的写了 scrollTop」+「滚动过程里真的重算了锚点」。
    // ⚠️ `[^=]` 不可省：第一版写 `/\.scrollTop\s*=/`，而它被同一函数里的
    // `if (el.scrollTop === before)` 满足了（`===` 以 `=` 开头）——
    // 于是把真正那行赋值删掉，判据照样绿。注入实测抓到的。
    expect(drag, "必须真的滚容器（给 scrollTop 赋值），不是只判方向").toMatch(
      /\.scrollTop\s*=[^=]/,
    );
    expect(drag, "要按容器边缘判方向（拿容器的 rect 比对指针 Y）").toMatch(
      /getBoundingClientRect\(\)[\s\S]{0,400}?(?:r\.top|r\.bottom)/,
    );
    // 🔴 滚动时指针不动 → 不会再有 pointermove。若滚动循环里不重算锚点，
    // 插入线会停在滚动开始时那张卡上，松手落点与用户看到的线不一致。
    const tickAt = drag.search(/const tick = \(\) =>/);
    expect(tickAt, "找不到滚动循环").toBeGreaterThan(0);
    expect(
      drag.slice(tickAt, tickAt + 600),
      "滚动循环里必须重算锚点，否则插入线与落点不一致",
    ).toMatch(/dropAnchorAt/);
    expect(drag, "rAF 必须能停（漏停 = 列表自己一直滚）").toMatch(/cancelAnimationFrame/);
  });

  it("store → bridge → Rust 命令 → generate_handler 四段齐全", () => {
    expect(code(STORE_TS), "store 里要有 reorderKey action").toMatch(/async reorderKey\(/);
    expect(code(STORE_TS), "要调 bridge").toMatch(/api\.reorderKey\(/);
    expect(code(STORE_TS), "顺序变换必须复用共享纯函数（三处同语义）").toMatch(/reorderIds\(/);
    expect(code(BRIDGE), "bridge 要映射到 reorder_key").toMatch(/call<boolean>\("reorder_key"/);
    // 命令实现按本仓「命令跟着实现走」的做法在 key_order.rs（lib.rs 棘轮余量为 0）。
    expect(code(KEY_ORDER_RS), "Rust 侧要有这个命令").toMatch(/fn reorder_key\(/);
    // 🔴 反向判据：策略门 `invoke-command-must-exist` 只查「前端调的名字后端有定义」，
    // **不查命令有没有进 generate_handler** —— 漏注册只在用户真去拖那一下时才炸。
    const handler = LIB_RS.slice(LIB_RS.indexOf("generate_handler!"));
    expect(handler, "reorder_key 必须注册（全路径形态）").toMatch(
      /store::key_order::reorder_key,/,
    );
    // 每个排序命令在注册表里**各出现一次**：本轮发现 `move_key` 曾被注册两遍
    // （HEAD 上就有，两行完全一样）。判据按「命令名 + 逗号」数，容得下全路径写法。
    for (const cmd of ["set_primary_key", "move_key", "reorder_key"]) {
      const hits = handler.match(new RegExp(`(?:^|\\s|:)${cmd},`, "g"))?.length ?? 0;
      expect(hits, `${cmd} 在 generate_handler 里应恰好注册一次，实测 ${hits}`).toBe(1);
    }
  });

  it("🔴 拖放必须过 client_resync（首位换了 = 客户端默认模型换了）", () => {
    // 与 Rust 侧 `every_key_mutation_command_must_go_through_sync_after` 互补：
    // 那条扫全部入口，这条只盯拖放这一个，失败消息直接说清后果。
    const c = code(KEY_ORDER_RS);
    const at = c.indexOf("fn reorder_key(");
    expect(at, "找不到 reorder_key").toBeGreaterThan(0);
    const body = c.slice(at, at + 900);
    expect(body, "不过 sync_after 的表现是「界面说换了、客户端没换」").toMatch(
      /client_resync::sync_after\(/,
    );
    expect(body, "主 Key 可能易主 → 托盘勾选要跟上").toMatch(/rebuild_tray\(/);
  });

  it("mock 复刻同一套语义，浏览器预览里能真的拖动", () => {
    const c = code(MOCK);
    expect(c, "mock 要实现 reorderKey").toMatch(/async reorderKey\(/);
    // 与后端同一条纪律：摘掉 source 之后再定位锚点。
    expect(c).toMatch(/splice\(from, 1\)/);
    // 不实现的话预览下拖完一松手就弹回原位，看起来像拖放坏了。
    expect(c, "锚点缺失 = 放末尾").toMatch(/ordered\.push\(moved\)/);
  });

  it("后端三个入口共用同一套变换（不许拖放另写一份重排规则）", () => {
    const c = code(KEY_ORDER_RS);
    for (const fn of ["fn set_primary(", "fn move_one(", "fn reorder_before("]) {
      expect(c, `${fn} 应在 key_order 里`).toContain(fn);
    }
    // 三者都必须走同一个落盘函数：各写一份重编号逻辑必然漂移，
    // 而漂移的表现是「从拖放得到的顺序」与「从按钮得到的顺序」在同一意图上不一致。
    expect(c.match(/persist_contiguous\(/g)?.length, "三个入口 + 定义，至少 4 处").toBeGreaterThanOrEqual(4);
    // store.rs 里那两个公开方法必须是**委托**，不许留第二份实现。
    const storeRs = code(read("src-tauri", "src", "store.rs"));
    expect(storeRs).toMatch(/key_order::set_primary\(/);
    expect(storeRs).toMatch(/key_order::move_one\(/);
    expect(storeRs).toMatch(/key_order::reorder_before\(/);
  });
});
