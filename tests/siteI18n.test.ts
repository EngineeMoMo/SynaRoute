import { describe, expect, it } from "vitest";
import { checkTranslations } from "../site/scripts/check-i18n.mjs";

describe("site translation references", () => {
  it("rejects a step prefix absent from both languages", () => {
    const sources = { "features.ts": 'const steps = [{ i18nPrefix: "steps.s4" }];' };
    expect(checkTranslations(sources, { zh: {}, en: {} })).toEqual(expect.arrayContaining([
      "zh: missing steps.s4.title", "en: missing steps.s4.desc",
    ]));
  });
  it("rejects literal references and one-language gaps", () => {
    const sources = { "Page.tsx": 't("heading"); t("missing");' };
    expect(checkTranslations(sources, { zh: { heading: "标题" }, en: {} })).toContain("en: missing heading");
    expect(checkTranslations(sources, { zh: {}, en: {} })).toContain("zh: missing missing");
  });
  it("accepts complete step dictionaries and ignores commented references", () => {
    const sources = { "Page.tsx": '// t("not-a-reference")\nconst steps = [{ i18nPrefix: "steps.s1" }];' };
    const dictionary = { "steps.s1.title": "Start", "steps.s1.desc": "Install" };
    expect(checkTranslations(sources, { zh: dictionary, en: dictionary })).toEqual([]);
  });
});
