import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { inspectableLines } from "../scripts/lib/rust-source.mjs";
import {
  adoptSuggestion,
  applyRealNameChange,
  rebuildMappingsForAllModels,
} from "@/components/ModelMappingSection";
import type { ModelMapping } from "@/types";

/**
 * 映射行的两条**用户能直接感知**的规则：对外名自动生成、一键修法不吃掉显示名。
 *
 * 两者的实现都刻意抽成了纯函数 —— 本仓没有 jsdom / testing-library，
 * 留在组件内部的箭头函数一律零覆盖，而这两条正是本轮改动里最容易悄悄坏掉的部分。
 */

const row = (over: Partial<ModelMapping> = {}): ModelMapping => ({
  id: "m1",
  expectedName: "",
  realName: "",
  displayName: undefined,
  ...over,
});

describe("对外名自动生成", () => {
  it("对外名为空时，选真实模型即乐观填上它", () => {
    const r = applyRealNameChange(row(), "glm-5.3", false);
    expect(r.row.realName).toBe("glm-5.3");
    expect(r.row.expectedName).toBe("glm-5.3");
    // CLI / Codex 分类没有合规约束 → 不需要再问后端要合规名，对外名==真实名最直观。
    expect(r.needsSuggestion).toBe(false);
  });

  it("桌面端分类还要再问后端要一个合规名", () => {
    const r = applyRealNameChange(row(), "glm-5.3", true);
    expect(r.row.expectedName).toBe("glm-5.3", "先乐观填上，异步再换成合规建议");
    expect(r.needsSuggestion).toBe(true);
  });

  it("🔴 对外名已有值时一个字节都不许动 —— 自动生成不能反过来吃掉用户的修改", () => {
    const mine = row({ expectedName: "claude-opus-4-8", realName: "glm-4.6" });
    const r = applyRealNameChange(mine, "deepseek-reasoner", true);
    expect(r.row.realName).toBe("deepseek-reasoner");
    expect(r.row.expectedName).toBe("claude-opus-4-8");
    expect(r.needsSuggestion).toBe(false, "已有值就不该再去要建议，否则异步回来会覆盖它");
  });

  it("清空真实模型不会顺手写一个空对外名", () => {
    const r = applyRealNameChange(row(), "   ", true);
    expect(r.row.expectedName).toBe("");
    expect(r.needsSuggestion).toBe(false);
  });

  it("显示名与本函数无关，原样带过去", () => {
    const r = applyRealNameChange(row({ displayName: "GLM 5.3（思考）" }), "glm-5.3", false);
    expect(r.row.displayName).toBe("GLM 5.3（思考）");
  });
});

describe("一键修法（给每个模型建一条映射）", () => {
  const models = [{ realName: "glm-4.6", source: "manual" as const }, { realName: "claude-opus-4-8", source: "manual" as const }];
  const issues = [{ name: "glm-4.6", suggestion: "claude-sonnet-4-6" }];

  it("每个模型都建一条，不合规的换成建议名、合规的保持原名", () => {
    const out = rebuildMappingsForAllModels(models, [], issues, 1000);
    // 🔴 必须是**每一个**：只给不合规的建，会让本来合规的那条从选择器里静默消失
    // （serviceable_models 的语义是「有任一完整映射，models 列表就被整份忽略」）。
    expect(out.map((m) => [m.realName, m.expectedName])).toEqual([
      ["glm-4.6", "claude-sonnet-4-6"],
      ["claude-opus-4-8", "claude-opus-4-8"],
    ]);
    expect(new Set(out.map((m) => m.id)).size).toBe(2, "同一毫秒批量生成也不许撞 id");
  });

  it("🔴 保留用户已填的显示名（按真实名认领旧行）", () => {
    const existing = [
      row({ id: "old1", realName: "glm-4.6", expectedName: "whatever", displayName: "GLM 4.6" }),
    ];
    const out = rebuildMappingsForAllModels(models, existing, issues, 1000);
    expect(out[0].displayName).toBe("GLM 4.6", "重建时丢掉它 = 点一下按钮，填好的显示名全没了");
    expect(out[1].displayName).toBeUndefined();
  });

  it("只有空白的旧显示名不算填过（不把空串搬进新行）", () => {
    const existing = [row({ id: "old1", realName: "glm-4.6", displayName: "   " })];
    expect(rebuildMappingsForAllModels(models, existing, issues, 1000)[0].displayName).toBeUndefined();
  });
});

describe("异步建议落地", () => {
  const rows = [
    row({ id: "a", realName: "glm-5.3", expectedName: "glm-5.3" }),
    row({ id: "b", realName: "gpt-5.6-sol", expectedName: "claude-opus-4-8" }),
  ];

  it("把乐观值换成合规建议", () => {
    const out = adoptSuggestion(rows, "a", "glm-5.3", "claude-sonnet-5-3");
    expect(out[0].expectedName).toBe("claude-sonnet-5-3");
    expect(out[1]).toBe(rows[1], "别的行连引用都不该动");
  });

  it("🔴 用户在这 250ms 里手改过对外名 → 不许覆盖", () => {
    // 点完模型顺手去改对外名是很自然的操作，无条件覆盖就是「刚打的字被吃掉」，
    // 而他不会知道是谁改的。
    const edited = [{ ...rows[0], expectedName: "我自己填的" }, rows[1]];
    expect(adoptSuggestion(edited, "a", "glm-5.3", "claude-sonnet-5-3")[0].expectedName).toBe("我自己填的");
  });

  it("那一行已经被删掉 → 什么都不发生（按 id 定位，不按索引）", () => {
    const out = adoptSuggestion([rows[1]], "a", "glm-5.3", "claude-sonnet-5-3");
    expect(out).toEqual([rows[1]]);
  });
});

describe("接线", () => {
  /**
   * 🔴 上面全是直接调纯函数 —— 把组件里的调用点改回内联实现，它们照样全绿，
   * 而那正是「规则写了两份」的开端。本仓已 14 次撞同一类盲区。
   */
  it("组件必须走这三个纯函数，而不是自己内联一份", () => {
    const here = dirname(fileURLToPath(import.meta.url));
    const p = join(here, "../src/components/ModelMappingSection.tsx");
    // 剥注释用本仓的**单一事实来源** `inspectableLines`（含 JSX 的 `{/* … */}`）——
    // 自己再写一份正则就是又一处会漂移的实现，而本仓已 5 次栽在「注释满足了断言」上。
    const code = inspectableLines(readFileSync(p, "utf8"), p)
      .map((l: { text: string }) => l.text)
      .join("\n");
    for (const fn of ["applyRealNameChange(", "rebuildMappingsForAllModels(", "adoptSuggestion("]) {
      // 2 = 定义 + 调用。少了调用那次就是内联实现回来了。
      expect(code.split(fn).length - 1, `${fn} 应恰好出现 2 次（定义 + 调用）`).toBe(2);
    }
  });
});
