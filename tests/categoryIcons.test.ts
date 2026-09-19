import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { CATEGORY_ICONS } from "@/lib/categoryIcons";

/**
 * 分类图标的**唯一事实来源**判据（2026-09-18 用户实报「三个分类图标还是同一个」）。
 *
 * 真实成因不是选错图标，是这份映射此前散在四处、而分类**页头**压根没接任何一份 ——
 * `CategoryPage` 三页统一写死 `Waypoints`（SynaRoute 的 logo）。收进 `categoryIcons.ts`
 * 之后，判据要保证：① 三个分类都有图标且**两两不同**（相同就等于没区分）；
 * ② 四个入口都从这一份取，任何一处再抄一份私有映射就变红。
 *
 * 🔴 尤其钉住页头：它是这次真正坏掉的那处。断言它按 `activeCategory` 取图标、
 * 且**不再**把 `Waypoints` 直接传给 PageHeader —— 后者正是缺陷本体。
 */

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const read = (...p: string[]) => readFileSync(join(ROOT, ...p), "utf8");
const code = (src: string) =>
  src
    .replace(/\/\*[\s\S]*?\*\//g, "")
    .split("\n")
    .filter((l) => !/^\s*(\/\/|\*)/.test(l))
    .join("\n");

describe("分类图标单一来源", () => {
  it("三个分类都有图标，且两两不同", () => {
    const icons = [
      CATEGORY_ICONS["claude-cli"],
      CATEGORY_ICONS["claude-desktop"],
      CATEGORY_ICONS.codex,
    ];
    expect(icons.every(Boolean), "每个分类都要有图标").toBe(true);
    expect(new Set(icons).size, "三个图标必须两两不同，否则等于没区分").toBe(3);
  });

  it("🔴 页头按 activeCategory 取图标，不再写死 logo", () => {
    const page = code(read("src", "pages", "CategoryPage.tsx"));
    expect(page, "页头图标必须来自单一来源、按当前分类取").toContain(
      "icon={CATEGORY_ICONS[activeCategory]}",
    );
    // Waypoints 是 SynaRoute 自己的 logo 字形 —— 页头三页共用它正是本次缺陷本体。
    expect(page, "页头不该再把 logo 字形直接当分类图标").not.toContain("Waypoints");
  });

  it("四个入口都从单一来源取，无人再抄私有映射", () => {
    for (const f of [
      ["src", "components", "Sidebar.tsx"],
      ["src", "components", "CommandPalette.tsx"],
      ["src", "components", "OnboardingWizard.tsx"],
      ["src", "pages", "CategoryPage.tsx"],
    ]) {
      expect(read(...f), `${f.join("/")} 应从 categoryIcons 取`).toContain(
        "CATEGORY_ICONS",
      );
    }
  });

  it("旧的分类图标字形不再散落在各入口里（否则又是一份会漂移的私有映射）", () => {
    // MonitorSmartphone / Code2 是被替换掉的旧字形；它们若还出现在这些文件里，
    // 说明有人没走单一来源、又抄了一份。Terminal 不查：它可能被别处正当使用。
    for (const f of [
      ["src", "components", "Sidebar.tsx"],
      ["src", "components", "CommandPalette.tsx"],
      ["src", "components", "OnboardingWizard.tsx"],
    ]) {
      const src = code(read(...f));
      expect(src, `${f.join("/")} 不该再引旧的分类字形`).not.toMatch(
        /MonitorSmartphone|Code2/,
      );
    }
  });
});
