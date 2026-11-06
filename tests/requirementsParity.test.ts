import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const requirements = readFileSync("docs/01-需求规格说明书.md", "utf8").replace(/\r\n/g, "\n");
const development = readFileSync("docs/19-开发文档.md", "utf8").replace(/\r\n/g, "\n");
const definitions = [...requirements.matchAll(/^#### FR-(\d{3}[a-z]?)\s+(.+)$/gm)]
  .map((match) => [match[1], match[2].trim()]);
const section = development.match(/^## 四、功能需求实现状态[^\n]*\n([\s\S]*?)(?=^## 五、)/m)?.[1] ?? "";
const rows = [...section.matchAll(/^\|\s*(?:FR-)?(\d{3}[a-z]?)\s*\|\s*([^|]+)\|/gm)]
  .map((match) => [match[1], match[2].trim()]);

describe("需求编号与实现表对账", () => {
  it("两份清单均完整且没有重复编号", () => {
    expect(definitions.length).toBeGreaterThanOrEqual(30);
    expect(rows.length).toBeGreaterThanOrEqual(30);
    for (const list of [definitions, rows]) {
      expect(new Set(list.map(([id]) => id)).size).toBe(list.length);
    }
  });

  it("实现表的编号与标题逐条使用需求原文", () => {
    expect(rows).toEqual(definitions);
  });
});
