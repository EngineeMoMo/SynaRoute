import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { RESERVED_HEADERS, checkCustomHeaders } from "../src/lib/reservedHeaders";

/**
 * 「自定义请求头」保留字段清单的**跨语言不变量**（B1，docs/14 §21.1）。
 *
 * 清单有两份实现：
 * - Rust `custom_headers::is_reserved` —— 保存时真正拦下的、转发时真正过滤的
 * - TS `RESERVED_HEADERS` —— 编辑器里即时提示用的
 *
 * 两份分叉的表现都是**静默**的，且方向不同：
 * - 前端少一条 → 界面不提示，用户填进去、点保存才被拒，而提示语在别处
 * - 前端多一条 → 界面拦下了一个后端其实允许的头，用户以为不能用
 *
 * 编译器管不到跨语言这条缝，故用机械判据钉住。判据取自 **Rust 源码本身**，
 * 不是「我记得它长这样」—— 后端加一个新的代理自有头时这条必须变红。
 *
 * 放在 `tests/` 而非 `src/`：要用 node:fs 读 Rust 源码，理由同 mcpEndpointParity.test.ts。
 */

const here = dirname(fileURLToPath(import.meta.url));
const rustSrc = readFileSync(join(here, "../src-tauri/src/custom_headers.rs"), "utf8");

/** 从 Rust 的 `is_reserved` 里抽出 `matches!` 的字面量集合。 */
function rustReservedHeaders(): string[] {
  const fn = rustSrc.slice(rustSrc.indexOf("pub(crate) fn is_reserved"));
  const body = fn.slice(0, fn.indexOf("\n}"));
  const names = [...body.matchAll(/"([a-z0-9\-]+)"/g)].map((m) => m[1]);
  // 解析到 0 个就主动失败：函数改名或改写法时，一个恒绿的判据比没有判据更糟
  // （本仓 invoke-command-must-exist 那条门就是这么踩过的）。
  expect(names.length).toBeGreaterThan(5);
  return names;
}

describe("保留请求头清单的跨语言一致性", () => {
  it("前端 RESERVED_HEADERS 与 Rust is_reserved 是同一个集合", () => {
    const rust = [...rustReservedHeaders()].sort();
    const ts = [...RESERVED_HEADERS].sort();
    expect(ts).toEqual(rust);
  });

  it("鉴权类头必须在清单里（这是本功能存在的首要理由）", () => {
    // 放一个 authorization 过去会顶掉我们换上的真实 Key → 上游 401，
    // 而日志只显示「鉴权失败」，没人会想到是自定义头干的。
    for (const must of ["authorization", "x-api-key"]) {
      expect(RESERVED_HEADERS).toContain(must);
    }
  });

  it("清单里没有重复项", () => {
    expect(new Set(RESERVED_HEADERS).size).toBe(RESERVED_HEADERS.length);
  });
});

describe("KeyEditor 的接线", () => {
  // 🔴 上面那些用例全都直接调 checkCustomHeaders / 读 Rust 源码，
  // 于是**把字段从 KeyEditor 里摘掉、或忘了放进保存 payload，它们照样全绿** ——
  // 而那就是「界面能填、存不进去」这个缺陷本身，且表现是静默的（点保存没报错，重开是空的）。
  //
  // 这是本仓第 5 次撞同一类盲区（前四次：mcp handle_http、route_meta、lan_guard 的 peer、
  // log_rotate 的写线程）。教训见 docs/19「单元覆盖 ≠ 覆盖接线」。
  const editorSrc = readFileSync(join(here, "../src/components/KeyEditor.tsx"), "utf8");

  it("渲染了 CustomHeadersField 并把 state 双向接上", () => {
    expect(editorSrc).toContain("<CustomHeadersField");
    expect(editorSrc).toMatch(/value=\{headersJson\}/);
    expect(editorSrc).toMatch(/onChange=\{setHeadersJson\}/);
  });

  it("保存 payload 里带 headersJson，且空值落成 undefined 而不是空串", () => {
    // 空串会把 `headers_json: Some("")` 存进配置 —— 后端虽然容忍，
    // 但配置文件里凭空多一个空字段，排障时看着像「配过又清了」。
    expect(editorSrc).toMatch(/headersJson: headersJson\.trim\(\) \|\| undefined,/);
  });

  it("初始值从 initial?.headersJson 读回（否则编辑已有 Key 时会显示成空）", () => {
    expect(editorSrc).toMatch(/useState\(initial\?\.headersJson \?\? ""\)/);
  });
});

describe("checkCustomHeaders 的判据与后端对齐", () => {
  it("空与空白都算未设置", () => {
    for (const raw of ["", "   ", "\n"]) {
      expect(checkCustomHeaders(raw)).toEqual({ kind: "empty" });
    }
  });

  it("正常两条通过并给出条数", () => {
    const r = checkCustomHeaders('{"HTTP-Referer":"https://x.dev","X-Title":"App"}');
    expect(r).toEqual({ kind: "ok", count: 2 });
  });

  it("保留字段被指名报出（大小写不敏感）", () => {
    const r = checkCustomHeaders('{"Authorization":"Bearer x","X-Title":"ok"}');
    expect(r).toEqual({ kind: "reserved", names: ["authorization"] });
  });

  it("非对象、非法名字、含换行的值都判 invalid 且带原因", () => {
    for (const raw of ['["a"]', '{"X A":"v"}', '{"X-A":"a\\r\\nX-Evil: 1"}', "{oops"]) {
      const r = checkCustomHeaders(raw);
      expect(r.kind).toBe("invalid");
      expect((r as { reason: string }).reason.length).toBeGreaterThan(0);
    }
  });

  it("数字与布尔放过（用户写 {\"X-N\": 3} 是自然的）", () => {
    expect(checkCustomHeaders('{"X-N":3,"X-B":true}')).toEqual({ kind: "ok", count: 2 });
  });
});
