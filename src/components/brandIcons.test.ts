import { describe, it, expect } from "vitest";
import {
  BRANDS,
  getBrand,
  isNearBlack,
  isPresetBrand,
  resolveBrand,
} from "@/components/brandIcons";
import { GENERATED_BRAND_ICONS } from "@/components/brandIcons.generated";

/**
 * 品牌注册表的结构性判据。这几条**界面上都看不出来**（错了只是图标不对或没有图标，
 * 不报错、不崩），而它们各自对应一个真实的失效方式。
 */

describe("品牌注册表", () => {
  /**
   * 生成的图标数据必须**全部被引用**，且被引用的 key 必须真的存在。
   *
   * 后半条是这里最要紧的：`brandIcons.ts` 里用 `p("qwen")` 取 path，取不到返回
   * `undefined` —— 于是 simple-icons 哪天把 slug 改了名，那个品牌会**静默**退回
   * 首字母色块。没有报错、没有警告，只是图标悄悄消失了。
   */
  it("生成的图标与注册表双向对齐（没有死数据，也没有取空的引用）", () => {
    const generated = new Set(GENERATED_BRAND_ICONS.map((g) => g.key));
    const withPath = BRANDS.filter((b) => b.path);

    // 每个 path 都必须非空字符串。undefined 会被 `b.path` 判假、静默退回字母块。
    for (const b of withPath) {
      expect(typeof b.path, `${b.key} 的 path 类型不对`).toBe("string");
      expect(b.path!.length, `${b.key} 的 path 是空串（上游 slug 改名了？）`).toBeGreaterThan(20);
    }

    // 生成的数据里不能有谁都不用的（死数据会进产物却永远画不出来）
    const referenced = new Set<string>();
    for (const g of GENERATED_BRAND_ICONS) {
      if (withPath.some((b) => b.path === g.path)) referenced.add(g.key);
    }
    const dead = [...generated].filter((k) => !referenced.has(k));
    expect(dead, "这些生成的图标没有任何品牌引用 —— 从 gen-brand-icons.mjs 的 WANT 里删掉").toEqual(
      [],
    );

    // 反向判据：解析到 0 个就说明上面的比较方式坏了
    expect(withPath.length, "应当有相当数量的品牌带 logo").toBeGreaterThan(10);
  });

  /**
   * 关键词不得**乱吃别家**的厂商名与模型名。
   *
   * 短关键词是这里唯一的危险源：加一个 `"yi"` 或 `"x"` 会让半个表都命中它。
   * 判据用一批真实的厂商名 / 模型名 / base_url 片段逐个跑 `resolveBrand`，
   * 断言认出来的那一家是对的。认错的表现是「图标是别家的」，
   * 用户会以为程序把 Key 搞混了。
   */
  it("关键词不与别家厂商冲突", () => {
    const cases: [string, string][] = [
      // [输入, 期望的品牌 key]
      ["claude-opus-5", "anthropic"],
      ["api.anthropic.com", "anthropic"],
      ["gpt-5.6-sol", "openai"],
      ["api.openai.com", "openai"],
      ["gpt-5-codex", "openai"],
      ["gemini-3-1-pro", "gemini"],
      ["generativelanguage.googleapis.com", "gemini"],
      ["grok-4.6", "xai"],
      ["api.x.ai", "xai"],
      ["mistral-large-latest", "mistral"],
      ["codestral-latest", "mistral"],
      ["llama-3.3-70b", "meta"],
      ["command-a-03-2025", "cohere"],
      ["sonar-pro", "perplexity"],
      ["deepseek-v4-pro", "deepseek"],
      ["api.deepseek.com", "deepseek"],
      ["glm-5.3", "zhipu"],
      ["open.bigmodel.cn", "zhipu"],
      ["kimi-k3", "moonshot"],
      ["api.moonshot.cn", "moonshot"],
      ["qwen3.8-max", "qwen"],
      ["dashscope.aliyuncs.com", "qwen"],
      ["doubao-seed-2", "doubao"],
      ["ark.cn-beijing.volces.com", "doubao"],
      ["ernie-5-turbo", "baidu"],
      ["qianfan.baidubce.com", "baidu"],
      ["hunyuan-turbos", "hunyuan"],
      ["minimax-m3", "minimax"],
      ["step-2-16k", "stepfun"],
      ["openrouter.ai", "openrouter"],
      ["api.siliconflow.cn", "siliconflow"],
      ["api.groq.com", "groq"],
      ["api.together.xyz", "together"],
      ["api.fireworks.ai", "fireworks"],
      ["api.novita.ai", "novita"],
      ["api.deepinfra.com", "deepinfra"],
      ["localhost:11434", "ollama"],
      ["localhost:1234", "lmstudio"],
    ];
    const wrong: string[] = [];
    for (const [input, expected] of cases) {
      const got = resolveBrand(input)?.key;
      if (got !== expected) wrong.push(`${input} → ${got ?? "(none)"}，应为 ${expected}`);
    }
    expect(wrong, "这些输入被认成了别家品牌（图标会显示成错的那一家）").toEqual([]);
  });

  /**
   * 深色模式必须能认出「近黑」品牌色。
   *
   * 上一版把 Kimi 写死成 `#000000` 且没有任何反色规则 —— 深色背景下那个图标
   * **完全看不见**，而深色是本应用的两个默认主题之一。
   */
  it("近黑判据认得出那几家纯黑品牌，也不会误伤正常颜色", () => {
    // 官方主色就是纯黑/近黑的
    expect(isNearBlack("#000000")).toBe(true);
    expect(isNearBlack("#191919"), "Anthropic 的 #191919，亮度 0.098").toBe(true);
    expect(isNearBlack("#111111"), "xAI，亮度 0.067").toBe(true);
    // 饱和的深蓝**不算**：亮度 0.286，白字压上去对比够，提亮反而失真。
    // 这一条同时钉住阈值不能被抬到 0.28 以上（那会贴着这个值、任何微调都翻边）。
    expect(isNearBlack("#0052D9"), "腾讯蓝，亮度 0.286").toBe(false);
    // 正常亮度的品牌色不得被误伤
    expect(isNearBlack("#4285F4"), "Google 蓝，亮度 0.493").toBe(false);
    expect(isNearBlack("#D4915D"), "Anthropic 暖橙，亮度 0.624").toBe(false);
    expect(isNearBlack("#FFD21E"), "Hugging Face 黄").toBe(false);
    expect(isNearBlack("#E73562"), "MiniMax 红").toBe(false);
    // 畸形输入不得抛异常（颜色是从数据里来的，脏值不该让图标渲染崩掉）
    expect(isNearBlack("")).toBe(false);
    expect(isNearBlack("not-a-color")).toBe(false);
  });

  it("预设键唯一、且 isPresetBrand / getBrand 与注册表一致", () => {
    const keys = BRANDS.map((b) => b.key);
    expect([...new Set(keys)].length, "预设键有重复").toBe(keys.length);
    for (const b of BRANDS) {
      expect(isPresetBrand(b.key), `${b.key} 应被认作预设键`).toBe(true);
      expect(getBrand(b.key)?.label).toBe(b.label);
    }
    // data-URL 与未知串都不能被当成预设键（否则会被塞进 <svg> 而不是 <img>）
    expect(isPresetBrand("data:image/png;base64,AAAA")).toBe(false);
    expect(isPresetBrand("totally-unknown")).toBe(false);
    expect(isPresetBrand(undefined)).toBe(false);
  });

  it("每个品牌都有中英文名与合法主色", () => {
    for (const b of BRANDS) {
      expect(b.label.trim(), `${b.key} 缺中文名`).not.toBe("");
      expect(b.labelEn.trim(), `${b.key} 缺英文名`).not.toBe("");
      expect(b.color, `${b.key} 的主色不是 #RRGGBB`).toMatch(/^#[0-9a-fA-F]{6}$/);
      expect(b.keywords.length, `${b.key} 没有匹配关键词`).toBeGreaterThan(0);
      /**
       * 关键词太短会乱吃别家名字（见上面那条冲突测试）。下限**按字符类分**：
       * - ASCII：至少 3 个字符。`yi`、`x`、`ai` 这种两字母词会命中半个表；
       * - CJK：至少 2 个字符。两个汉字已经是一个完整的词（「智谱」「豆包」「混元」），
       *   要求 3 个会把这些有用的中文关键词全部排除掉 —— 而中文名恰恰是国内厂商
       *   最常出现在 vendor 名里的形态。
       */
      for (const kw of b.keywords) {
        const cjk = /[一-鿿]/.test(kw);
        const min = cjk ? 2 : 3;
        expect(
          kw.length,
          `${b.key} 的关键词「${kw}」太短（${cjk ? "CJK 下限 2" : "ASCII 下限 3"}），会乱吃别家名字`,
        ).toBeGreaterThanOrEqual(min);
      }
    }
  });
});
