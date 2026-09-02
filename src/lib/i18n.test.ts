import { describe, it, expect } from "vitest";
// 用 Vite 的 `?raw` 读源码文本，而不是 node:fs —— src 侧的 tsconfig 是浏览器目标、没有
// node 类型，用 fs 会让 `tsc --noEmit` 报 TS2307（vitest 能跑但类型检查过不去）。
import i18nSource from "./i18n.ts?raw";
import i18nUsageSource from "./i18n.usage.ts?raw";
import i18nBrandPickerSource from "./i18n.brandPicker.ts?raw";
import i18nVendorSource from "./i18n.vendor.ts?raw";
import i18nFieldsSource from "./i18n.fields.ts?raw";
import i18nMappingSource from "./i18n.mapping.ts?raw";
import { translate } from "@/lib/i18n";
import { usageEn, usageZh } from "@/lib/i18n.usage";
import { brandPickerEn, brandPickerZh } from "@/lib/i18n.brandPicker";
import { vendorEn, vendorZh } from "@/lib/i18n.vendor";
import { fieldsEn, fieldsZh } from "@/lib/i18n.fields";
import { mappingEn, mappingZh } from "@/lib/i18n.mapping";

/**
 * i18n 的结构性判据。**这些浏览器里看不出来**（缺翻译只是显示成另一种语言或原始 key，
 * 不报错、不崩），而它们各自对应一个真实故障：
 *
 * 1. **zh/en 键必须对称** —— 缺键时 `translate` 会回退（en 缺 → 落 zh → 英文界面露中文；
 *    两边都缺 → 直接渲染出 `settings.foo` 这种原始 key）。历史上真出现过
 *    「healthCheckLabel 翻译了、时间还是中文」这类半汉半英。
 * 2. **拆出去的词典分片必须真的被展开进主词典**（见下面第二个 it）。
 *
 * 🔴 **还有一条纪律，本文件此前声称在管、实际没有**：「JSX 里不得硬编码中文文案」。
 * 那条对应的真实故障是 i18n key 存在但**没接线**（label 直接写字面量），切语言毫无反应
 * —— 实机暴露过：`Temperature` / `请求超时 (ms)` 两个 Field 的 label 写死在 KeyEditor 里。
 * 但**全仓没有任何机械判据在查它**（策略门那六条也不含），今天它靠 review。
 * 原文档头把它列为「两条结构性判据」之一，那是过度承诺 —— 读到「有判据」的人就不会再自己查。
 * 要补的话是扫 `.tsx` 生产段里 JSX 文本节点 / `label=` / `title=` 的 CJK 字面量，
 * 得先摸清白名单规模，不适合紧挨着一次未做的真机验证做。
 *
 * 用源码文本做判据而不是运行时快照：运行时只能覆盖当前渲染到的那几个组件，
 * 而漏翻译恰恰藏在没被点开的角落里。
 *
 * ⚠️ **词典拆分时必须把新文件加进 `SOURCES`**。这条判据本身有过一次静默缩水：
 * `usage.*` 那 32 条词条被拆到 `i18n.usage.ts` 之后，本测试仍然只读 `i18n.ts` ——
 * 它照样全绿，但那 32 条已经不在保护范围内了。下面的 `dict chunk 数量`
 * 断言就是为了让「拆了文件却没接进来」这件事变红。
 */

/** 参与对称性校验的全部词典源文件。拆新文件时**必须**加进来。 */
const SOURCES: { name: string; src: string }[] = [
  { name: "i18n.ts", src: i18nSource },
  { name: "i18n.usage.ts", src: i18nUsageSource },
  { name: "i18n.brandPicker.ts", src: i18nBrandPickerSource },
  { name: "i18n.vendor.ts", src: i18nVendorSource },
  { name: "i18n.fields.ts", src: i18nFieldsSource },
  { name: "i18n.mapping.ts", src: i18nMappingSource },
];

/** 各分片导出的运行时字典，用于校验「分片真的被展开进主词典了」。 */
const CHUNKS: { name: string; zh: Record<string, string>; en: Record<string, string> }[] = [
  { name: "usage", zh: usageZh, en: usageEn },
  { name: "brandPicker", zh: brandPickerZh, en: brandPickerEn },
  { name: "vendor", zh: vendorZh, en: vendorEn },
  { name: "fields", zh: fieldsZh, en: fieldsEn },
  { name: "mapping", zh: mappingZh, en: mappingEn },
];

/**
 * 按缩进两格的 `"key":` 抽取键名，并用 en 字典的起始位置作为两个字典的分界。
 *
 * 两种文件形态都要认：`i18n.ts` 里是 `const en: Dict = {`，
 * 拆出去的分片里是 `export const <name>En: Dict = {`。
 */
function dictKeysOf(name: string, src: string) {
  const enStart = src.search(/^export const \w*En: Dict|^const en/m);
  expect(enStart, `${name} 里应有 en 字典`).toBeGreaterThan(0);
  const all = [...src.matchAll(/^ {2}"([^"]+)":/gm)].map((m) => ({ k: m[1], i: m.index ?? 0 }));
  return {
    zh: all.filter((x) => x.i < enStart).map((x) => x.k),
    en: all.filter((x) => x.i > enStart).map((x) => x.k),
  };
}

function dictKeys() {
  const zh: string[] = [];
  const en: string[] = [];
  for (const s of SOURCES) {
    const k = dictKeysOf(s.name, s.src);
    zh.push(...k.zh);
    en.push(...k.en);
  }
  return { zh, en };
}

describe("i18n 结构判据", () => {
  it("zh 与 en 的键完全对称，且各自无重复键", () => {
    const { zh, en } = dictKeys();
    expect(zh.length, "zh 字典不应为空").toBeGreaterThan(100);

    const zhSet = new Set(zh);
    const enSet = new Set(en);
    expect(zh.filter((k) => !enSet.has(k)), "这些键 zh 有、en 缺（英文界面会露出中文）").toEqual([]);
    expect(en.filter((k) => !zhSet.has(k)), "这些键 en 有、zh 缺").toEqual([]);

    // 重复键：对象字面量里后者静默覆盖前者，改了前一处会「怎么改都不生效」。
    // 跨文件重复同样要抓：分片里重写一条 i18n.ts 已有的 key，展开顺序决定谁生效，
    // 而那个顺序在 i18n.ts 里是隐式的。
    const dup = (a: string[]) => [...new Set(a.filter((k, i) => a.indexOf(k) !== i))];
    expect(dup(zh), "zh 重复键（含跨分片重复）").toEqual([]);
    expect(dup(en), "en 重复键（含跨分片重复）").toEqual([]);
  });

  /**
   * 拆出去的分片必须真的被 `i18n.ts` 展开进主词典。
   *
   * 只校验分片自身的对称性是不够的：一个**没被 import、没被展开**的分片同样能通过
   * 对称性检查，而界面上那一整页词条会全部回退成原始 key。
   */
  it("拆出的词典分片都已接进主词典", () => {
    for (const chunk of CHUNKS) {
      for (const key of Object.keys(chunk.zh)) {
        expect(translate("zh", key), `${chunk.name}: ${key} 未被展开进 zh 主词典`).not.toBe(key);
      }
      for (const key of Object.keys(chunk.en)) {
        expect(translate("en", key), `${chunk.name}: ${key} 未被展开进 en 主词典`).not.toBe(key);
      }
      // 反向判据：空分片会让上面两个循环空转、测试恒绿
      expect(Object.keys(chunk.zh).length, `${chunk.name} 分片不应为空`).toBeGreaterThan(3);
    }
    // 每个源文件分片都要有一份对应的运行时校验（否则「拆了文件却没接进来」照旧漏掉）
    expect(CHUNKS.length, "CHUNKS 应覆盖 SOURCES 里除 i18n.ts 之外的每个分片").toBe(
      SOURCES.length - 1,
    );
  });

  it("缺失/未知键有可预期的回退，不渲染空串", () => {
    // 已知键正常取词
    expect(translate("zh", "common.save")).toBeTruthy();
    expect(translate("en", "common.save")).toBeTruthy();
    // 未知键回退到 key 本身而非空串（空串会让按钮变成一个看不见的方块）
    expect(translate("en", "definitely.missing.key")).toBe("definitely.missing.key");
  });
});
