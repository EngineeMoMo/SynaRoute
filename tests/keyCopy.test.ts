import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { inspectableLines } from "../scripts/lib/rust-source.mjs";
import { makeKeyCopy } from "@/lib/keyCopy";
import type { ProviderKey } from "@/types";

/**
 * 「复制这条 Key 的配置」的规则。用户的诉求是「同一供应商有不同 key，新建太麻烦」——
 * 所以本判据的重点是**别少复制**（那会让用户以为设置跟过来了、实际没有），
 * 同时**别多复制**那几个会造成真实后果的字段。
 */

/** 一条**字段尽量填满**的 Key —— 逐字段对账那条用例靠它发现「漏复制」。 */
const FULL: ProviderKey = {
  id: "orig-uuid",
  categoryId: "claude-desktop",
  name: "百倍算力",
  vendor: "zhipu",
  baseUrl: "https://api.example.com",
  protocol: "anthropic",
  hasSecret: true,
  enabled: true,
  allowInAggregate: true,
  priority: 3,
  headersJson: '{"X-Title":"SynaRoute"}',
  params: { temperature: 0.7, timeoutMs: 60000 },
  models: [{ realName: "glm-5.3", source: "manual", contextWindow: 200000 }],
  mappings: [
    { id: "m1", expectedName: "claude-sonnet-5-3", realName: "glm-5.3", displayName: "GLM 5.3" },
  ],
  defaultModel: "glm-5.3",
  tierHaiku: "glm-4.5-air",
  tierSonnet: "glm-4.6",
  tierOpus: "deepseek-reasoner",
  tierFable: "gpt-5.6-sol",
  health: { status: "up", failCount: 2, breakerUntil: 9e12, latencyMs: 320 },
  balanceQuery: {
    enabled: true,
    template: "newapi",
    url: "{{baseUrl}}/v1/usage",
    method: "GET",
    auth: "bearer",
    timeoutSecs: 10,
    autoIntervalMin: 30,
  },
  costMultiplier: "0.3",
  icon: "zhipu",
};

/** 这四项**必须**被覆盖；其余一律沿用。改这张表前先读 `makeKeyCopy` 的文档。 */
const OVERRIDDEN = ["id", "name", "hasSecret", "health"] as const;

describe("复制 Key 的配置", () => {
  it("四项覆盖各有其后果，逐项钉住", () => {
    const c = makeKeyCopy(FULL, "（副本）");
    // id 留空：后端 upsert_key 按 id 认领，带原 id 会**直接覆盖掉原 Key**。
    expect(c.id).toBe("");
    // 密钥在加密库里、前端只有这个布尔值 —— 带 true 会让编辑器显示「已配置，留空则不修改」，
    // 用户不填就保存 → config 说有密钥、库里没有。
    expect(c.hasSecret).toBe(false);
    // 运行态不继承：否则副本一挂出来就带着**另一条 Key** 的熔断计数与探测结果。
    expect(c.health).toEqual({ status: "unknown", failCount: 0 });
    expect(c.name).toBe("百倍算力（副本）");
  });

  it("🔴 除那四项外**逐字段**沿用（这条是「别少复制」的机械化）", () => {
    const c = makeKeyCopy(FULL, "（副本）");
    const carried = (Object.keys(FULL) as (keyof ProviderKey)[]).filter(
      (f) => !(OVERRIDDEN as readonly string[]).includes(f),
    );
    // 逐字段比而不是整对象比：整对象比在失败时只说「不相等」，逐字段能指出**哪个**没跟过来。
    for (const f of carried) {
      expect(c[f], `字段 ${f} 没被复制 —— 用户会以为它跟过来了`).toEqual(FULL[f]);
    }
    // 正向断言：真的比过一堆字段，而不是 carried 恰好为空。
    expect(carried.length).toBeGreaterThan(15);
  });

  it("priority 刻意沿用 —— 撞车由后端 upsert_key 顶到队尾", () => {
    // 前端再猜一个数字只会和后端那条规则打架（它的注释原文：「判据放在**碰撞**而不是
    // 无条件重编号上」）。这里钉住「前端不动它」这个事实。
    expect(makeKeyCopy(FULL, "x").priority).toBe(3);
  });

  it("不改动原对象（副本进编辑器后用户的编辑不该回写原 Key）", () => {
    const before = JSON.stringify(FULL);
    makeKeyCopy(FULL, "（副本）");
    expect(JSON.stringify(FULL)).toBe(before);
  });

  it("没有备注名时也能复制（后缀单独成名，不产出空名字）", () => {
    expect(makeKeyCopy({ ...FULL, name: "" }, "（副本）").name).toBe("（副本）");
  });
});

/** 生产段（剥注释，含 JSX 的 `{/* … *\/}`）—— 判据说「代码里必须这么写」，就只能看代码。 */
function production(rel: string): string {
  const here = dirname(fileURLToPath(import.meta.url));
  const p = join(here, rel);
  return inspectableLines(readFileSync(p, "utf8"), p)
    .map((l: { text: string }) => l.text)
    .join("\n");
}

describe("复制功能的接线", () => {
  /**
   * 🔴 上面全是直接调 `makeKeyCopy` —— 把按钮从卡片上摘掉、或忘了把回调接下去，
   * 它们照样全绿，而那就是「功能做了、用户点不到」这个缺陷本身。本仓已 14 次撞同一类盲区。
   *
   * 四条覆盖整条链：卡片上的按钮 → 分类页透传 → App 调 `makeKeyCopy` → 编辑器按新建对待。
   */
  it("卡片上有一个绑到 onDuplicate 的按钮", () => {
    const card = production("../src/components/KeyCard.tsx");
    expect(card, "KeyCard 必须收这个 prop").toContain("onDuplicate: (k: ProviderKey) => void");
    expect(card, "并且真的绑到某个按钮的 onClick 上").toContain("onClick={() => onDuplicate(k)}");
  });

  it("分类页把 onDuplicateKey 透传给卡片", () => {
    // 少了这一环的表现是 TS 报错（prop 必填），故这条更像是防「日后改成可选」——
    // 一旦 onDuplicate 变成 optional，漏传就成了静默的。
    expect(production("../src/pages/CategoryPage.tsx")).toContain("onDuplicate={onDuplicateKey}");
  });

  it("App 用 makeKeyCopy 造草稿，而不是自己拼一份", () => {
    const app = production("../src/App.tsx");
    expect(app).toContain("makeKeyCopy(");
    expect(app, "分类页要拿到复制回调").toContain("onDuplicateKey={openDuplicate}");
  });

  it("🔴 抽屉的 key 必须带打开序号，否则连续复制两条不同 Key 不会重新挂载", () => {
    // 复制路径下 id 恒为空串 → 只用 `editingKey?.id ?? "new"` 时两次的 key 完全相同，
    // React 不重新挂载，而 KeyEditor 的字段全是 `useState(initial?.x)`（只在挂载时取一次）
    // → 抽屉标题换了、表单里还是上一条 Key 的 baseUrl。**静默给错内容。**
    const app = production("../src/App.tsx");
    expect(app).toMatch(/key=\{`\$\{editingKey\?\.id \?\? "new"\}-\$\{editorSeq\}`\}/);
    // 三个入口都要递增，漏一个就是那一条路径不重挂载。
    expect(app.split("setEditorSeq((s) => s + 1)").length - 1, "openAdd/openEdit/openDuplicate 各一次").toBe(3);
  });

  it("🔴 编辑器把「id 为空的草稿」当新建对待", () => {
    // `!initial` 会把复制态判成「编辑」：标题写错，且放过「拉模型前先填密钥」那道校验
    // （那时点拉取会拿一个空 id 去密钥库取，报出的是底层错误、不是就地可行动的提示）。
    expect(production("../src/components/KeyEditor.tsx")).toContain("const isNew = !initial?.id;");
  });
});
