import { describe, it, expect } from "vitest";
// 用 Vite 的 `?raw` 读源码文本，而不是 node:fs —— src 侧的 tsconfig 是浏览器目标、没有
// node 类型，用 fs 会让 `tsc --noEmit` 报 TS2307（vitest 能跑但类型检查过不去）。
import i18nSource from "./i18n.ts?raw";
import { translate } from "@/lib/i18n";

/**
 * i18n 的两条结构性判据。**这两条浏览器里看不出来**（缺翻译只是显示成另一种语言或原始 key，
 * 不报错、不崩），而它们各自对应一个真实故障：
 *
 * 1. **zh/en 键必须对称** —— 缺键时 `translate` 会回退（en 缺 → 落 zh → 英文界面露中文；
 *    两边都缺 → 直接渲染出 `settings.foo` 这种原始 key）。历史上真出现过
 *    「healthCheckLabel 翻译了、时间还是中文」这类半汉半英。
 * 2. **JSX 里不得硬编码中文文案** —— i18n key 存在但**没接线**（label 直接写字面量）时，
 *    切语言毫无反应。实机暴露过：`Temperature` / `请求超时 (ms)` 两个 Field 的 label
 *    写死在 KeyEditor 里，i18n key 明明有却没用上，用户切英文这两项永远是原样。
 *
 * 用源码文本做判据而不是运行时快照：运行时只能覆盖当前渲染到的那几个组件，
 * 而漏翻译恰恰藏在没被点开的角落里。
 */

/** 按缩进两格的 `"key":` 抽取键名，并用 `const en` 作为两个字典的分界。 */
function dictKeys() {
  const src = i18nSource;
  const enStart = src.indexOf("const en");
  expect(enStart, "i18n.ts 里应有 `const en` 字典").toBeGreaterThan(0);
  const all = [...src.matchAll(/^ {2}"([^"]+)":/gm)].map((m) => ({ k: m[1], i: m.index ?? 0 }));
  return {
    zh: all.filter((x) => x.i < enStart).map((x) => x.k),
    en: all.filter((x) => x.i > enStart).map((x) => x.k),
  };
}

describe("i18n 结构判据", () => {
  it("zh 与 en 的键完全对称，且各自无重复键", () => {
    const { zh, en } = dictKeys();
    expect(zh.length, "zh 字典不应为空").toBeGreaterThan(100);

    const zhSet = new Set(zh);
    const enSet = new Set(en);
    expect(zh.filter((k) => !enSet.has(k)), "这些键 zh 有、en 缺（英文界面会露出中文）").toEqual([]);
    expect(en.filter((k) => !zhSet.has(k)), "这些键 en 有、zh 缺").toEqual([]);

    // 重复键：对象字面量里后者静默覆盖前者，改了前一处会「怎么改都不生效」
    const dup = (a: string[]) => [...new Set(a.filter((k, i) => a.indexOf(k) !== i))];
    expect(dup(zh), "zh 重复键").toEqual([]);
    expect(dup(en), "en 重复键").toEqual([]);
  });

  it("缺失/未知键有可预期的回退，不渲染空串", () => {
    // 已知键正常取词
    expect(translate("zh", "common.save")).toBeTruthy();
    expect(translate("en", "common.save")).toBeTruthy();
    // 未知键回退到 key 本身而非空串（空串会让按钮变成一个看不见的方块）
    expect(translate("en", "definitely.missing.key")).toBe("definitely.missing.key");
  });
});
