import { describe, expect, it } from "vitest";
import { consultationPrompt, consultationTemplates, parseConsultationReport } from "../src/lib/consultation";
import { translate } from "../src/lib/i18n";

const report = "## 推荐结论\n先检查重试。\n## 依据与共识\n顾问 A 引用了日志。\n## 分歧与待确认\n顾问 B 认为连接池也是因素。\n## 验证与下一步\n先关联请求 ID。";

describe("consultation report presentation", () => {
  it("keeps evidence and minority opinions in their own sections", () => {
    const sections = parseConsultationReport(report)!;
    expect(sections.map((s) => s.kind)).toEqual(["conclusion", "evidence", "disagreements", "verification"]);
    expect(sections[2].text).toBe("顾问 B 认为连接池也是因素。");
  });
  it("supports English output independently of UI language", () => {
    expect(parseConsultationReport("## Recommendation\nA\n## Evidence and agreement\nB\n## Disagreements and unknowns\nC\n## Verification and next steps\nD")?.length).toBe(4);
  });
  it("never mistakes fenced code for report headings", () => {
    const withCode = report.replace("先检查重试。", "````md\n## 分歧与待确认\n```\n````");
    expect(parseConsultationReport(withCode)?.[0].text).toContain("## 分歧与待确认");
  });
  it("preserves additional content inside sections", () => {
    const withExtra = `${report}\n## 附录\n不要遗漏此证据。`;
    expect(parseConsultationReport(withExtra)?.[3].text).toContain("不要遗漏此证据。");
  });
  it.each([
    "legacy plain answer", report.replace("## 依据与共识", "## Evidence"),
    `${report}\n## 推荐结论\n重复`, `降级警告\n${report}`,
    report.replace("先检查重试。", ""), `${report}\n~~~js\nnot closed`,
  ])("falls back to original text when the report is ambiguous: %s", (text) => {
    expect(parseConsultationReport(text)).toBeNull();
  });
});

describe("consultation templates", () => {
  it("keeps free discussion unchanged and preserves the user's input in every template", () => {
    for (const lang of ["zh", "en"] as const) {
      const t = (key: string) => translate(lang, key);
      expect(consultationPrompt("  原始需求\n不要改代码  ", "general", t)).toBe("原始需求\n不要改代码");
      for (const template of consultationTemplates) {
        const prompt = consultationPrompt("原始需求\n不要改代码", template, t);
        expect(prompt.endsWith("原始需求\n不要改代码")).toBe(true);
        expect(prompt).not.toContain("brain.template.");
      }
    }
  });
});
