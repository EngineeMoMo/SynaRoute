/**
 * 品牌注册表：预设键 → 显示名、主色、图标、匹配关键词。
 *
 * 从 `BrandIcon.tsx` 拆出来的：那边只管**画**，这边管**是什么**。
 * 拆开的实际收益是加品牌时只动一个数组，而渲染逻辑（深色模式反色、首字母兜底、
 * data-URL 分支）一行都不用碰。
 *
 * # 图标从哪来
 *
 * 19 个品牌的 path 来自 `brandIcons.generated.ts`（simple-icons，CC0-1.0，
 * 由 `scripts/gen-brand-icons.mjs` 抽出，运行时零依赖）。
 *
 * 剩下的品牌 simple-icons 里**没有**（OpenAI 被上游移除了，xAI / 智谱 / 腾讯混元 /
 * Groq / Together / Cohere / 阶跃 / 硅基流动 / 火山豆包 / 讯飞 都没有收录）。
 * 处置分两类：
 * - **OpenAI** 用本仓原有的手绘花瓣环 path —— 它确实认得出来，且这是最常用的一个；
 * - 其余走**首字母色块**兜底。这是刻意的：宁可显示一个干净的字母块，
 *   也不画一个「看起来像但其实不是」的假 logo（本仓上一版的智谱「Z」字、
 *   Qwen「双线」、Mistral「网格」正是那种，用户直接反馈了「不像」）。
 *   参照：cc-switch 给中转站也完全没有图标（实测它 20 条 provider 里只有 3 条有）。
 *
 * # 深色模式必须反色，否则等于没有图标
 *
 * 好几家的官方主色是纯黑或近黑（Moonshot/Kimi/Ollama/LM Studio 是 `#000000`、
 * Anthropic 是 `#191919`）。本仓上一版把 moonshot 写死成 `#000000`，
 * 深色背景下那个图标**完全看不见** —— 而深色是本应用的默认主题之一。
 * 判据收在 [`isNearBlack`]，由 `BrandIcon` 在渲染时按当前主题决定用不用它。
 */

import { GENERATED_BRAND_ICONS } from "@/components/brandIcons.generated";

/** 品牌分组，供挑选器分段展示（30+ 个平铺成一排是找不到东西的）。 */
export type BrandGroup = "official" | "china" | "gateway" | "local";

export interface Brand {
  key: string;
  /** 中文显示名。用户是在「我这个 Key 是哪家的」这个语境下选的，故用中文常用叫法。 */
  label: string;
  /** 英文显示名（英文界面用）。 */
  labelEn: string;
  group: BrandGroup;
  /** 官方主色（#RRGGBB）。近黑的会在深色模式下被反色，见 `isNearBlack`。 */
  color: string;
  /** 24x24 viewBox 的单条 path。没有就走首字母色块兜底。 */
  path?: string;
  /**
   * 自动匹配用的关键词（小写子串）。`resolveBrand` 拿 vendor id / 厂商名 / 模型名去撞它。
   *
   * ⚠️ **短关键词会乱吃别家名字**。`yi`、`x`、`step` 这类两三个字母的词
   * 绝不能进这张表 —— 有一条测试（`brandKeywordsDoNotCollide`）按真实厂商名与模型名
   * 逐个校验，加了会撞的词会直接变红。
   */
  keywords: string[];
}

const generated = new Map(GENERATED_BRAND_ICONS.map((g) => [g.key, g]));
const p = (key: string) => generated.get(key)?.path;
const hex = (key: string, fallback: string) => generated.get(key)?.hex ?? fallback;

/**
 * OpenAI 的花瓣环。simple-icons 已移除该图标（上游决定），故沿用本仓原有的手绘 path
 * —— 它是这批手绘里唯一确实认得出来的一个，且 OpenAI 是最常被选到的品牌。
 */
const OPENAI_PATH =
  "M21.55 10.02a5.42 5.42 0 0 0-.47-4.45 5.5 5.5 0 0 0-5.92-2.63A5.42 5.42 0 0 0 11.07 1a5.5 5.5 0 0 0-5.24 3.8A5.42 5.42 0 0 0 2.2 7.43a5.5 5.5 0 0 0 .68 6.44 5.42 5.42 0 0 0 .47 4.45 5.5 5.5 0 0 0 5.92 2.63A5.42 5.42 0 0 0 12.93 23a5.5 5.5 0 0 0 5.24-3.8 5.42 5.42 0 0 0 3.63-2.63 5.5 5.5 0 0 0-.68-6.44 5.4 5.4 0 0 0-.57-.11Zm-8.1 11.3a4.07 4.07 0 0 1-2.62-.95l.13-.07 4.35-2.51a.71.71 0 0 0 .36-.62v-6.13l1.84 1.06v5.07a4.1 4.1 0 0 1-4.06 4.15Zm-8.73-3.73a4.07 4.07 0 0 1-.49-2.74l.13.08 4.35 2.51a.71.71 0 0 0 .72 0l5.31-3.06v2.12l-4.4 2.54a4.1 4.1 0 0 1-5.6-1.5ZM3.57 8.14a4.07 4.07 0 0 1 2.13-1.79v5.16a.71.71 0 0 0 .36.62l5.31 3.06-1.84 1.06-4.35-2.51a4.1 4.1 0 0 1-1.62-5.6Zm15.11 3.51-5.31-3.07 1.84-1.06 4.35 2.51a4.1 4.1 0 0 1-.63 7.39v-5.16a.71.71 0 0 0-.25-.61Zm1.83-2.75-.13-.08-4.35-2.51a.71.71 0 0 0-.72 0L10.4 9.37V7.25l4.4-2.54a4.1 4.1 0 0 1 6.09 4.25Zm-11.56 3.8-1.84-1.06V6.57a4.1 4.1 0 0 1 6.72-3.15l-.13.07L9.32 5.99a.71.71 0 0 0-.36.62v5.09Zm1-2.15L12.24 9.4l2.5 1.44v2.88l-2.5 1.44-2.5-1.44v-2.87Z";

/**
 * 全部预设品牌。
 *
 * 顺序即挑选器里的顺序：**同组内按「用户最可能选到」排**，不按字母。
 * 官方原厂在前、国内原厂其次、聚合中转再次、本机推理最后 ——
 * 与新建 Key 时的真实分布一致。
 */
export const BRANDS: Brand[] = [
  // ---------------- 国际原厂 ----------------
  {
    key: "anthropic",
    label: "Anthropic / Claude",
    labelEn: "Anthropic / Claude",
    group: "official",
    // simple-icons 给的是 #191919（近黑）→ 深色模式会反色。
    // 浅色下仍用品牌自己的暖橙 #D4915D：那是 cc-switch 本机库里的实测值
    // （2026-08-22 读 ~/.cc-switch/cc-switch.db 的 providers.icon_color），
    // 对齐它能让从 cc-switch 导入过来的档在两个程序里看起来是同一个东西。
    color: "#D4915D",
    path: p("anthropic"),
    keywords: ["anthropic", "claude"],
  },
  {
    key: "openai",
    label: "OpenAI / GPT",
    labelEn: "OpenAI / GPT",
    group: "official",
    color: "#00A67E", // cc-switch 实测值（不是纯黑）
    path: OPENAI_PATH,
    keywords: ["openai", "gpt", "chatgpt", "o1-", "o3-", "o4-", "codex"],
  },
  {
    key: "gemini",
    label: "Google Gemini",
    labelEn: "Google Gemini",
    group: "official",
    color: "#4285F4", // cc-switch 实测值（Google 蓝）
    path: p("gemini"),
    keywords: ["gemini", "google", "vertex", "generativelanguage"],
  },
  {
    key: "xai",
    label: "xAI Grok",
    labelEn: "xAI Grok",
    group: "official",
    color: "#111111",
    // simple-icons 没有 xAI。**不画假 logo**，走首字母兜底。
    keywords: ["xai", "grok", "x.ai"],
  },
  {
    key: "mistral",
    label: "Mistral",
    labelEn: "Mistral",
    group: "official",
    color: hex("mistral", "#FA520F"),
    path: p("mistral"),
    keywords: ["mistral", "mixtral", "codestral", "ministral"],
  },
  {
    key: "meta",
    label: "Meta Llama",
    labelEn: "Meta Llama",
    group: "official",
    color: hex("meta", "#0866FF"),
    path: p("meta"),
    keywords: ["meta", "llama"],
  },
  {
    key: "cohere",
    label: "Cohere",
    labelEn: "Cohere",
    group: "official",
    color: "#39594D",
    keywords: ["cohere", "command-r", "command-a"],
  },
  {
    key: "perplexity",
    label: "Perplexity",
    labelEn: "Perplexity",
    group: "official",
    color: hex("perplexity", "#1FB8CD"),
    path: p("perplexity"),
    keywords: ["perplexity", "sonar"],
  },

  // ---------------- 国内原厂 ----------------
  {
    key: "deepseek",
    label: "DeepSeek 深度求索",
    labelEn: "DeepSeek",
    group: "china",
    color: hex("deepseek", "#4D6BFE"),
    path: p("deepseek"),
    keywords: ["deepseek"],
  },
  {
    key: "zhipu",
    label: "智谱 GLM",
    labelEn: "Zhipu GLM",
    group: "china",
    color: "#3859FF",
    // simple-icons 没有智谱。上一版那个手绘「Z」字被用户点名不像，故改走首字母兜底。
    keywords: ["zhipu", "glm", "bigmodel", "智谱", "z.ai"],
  },
  {
    key: "moonshot",
    label: "月之暗面 Kimi",
    labelEn: "Moonshot / Kimi",
    group: "china",
    // 官方主色是纯黑 → `isNearBlack` 会让它在深色模式下反色。
    // 上一版写死 #000000 而没有反色规则，深色下那个图标完全看不见。
    color: hex("kimi", "#000000"),
    path: p("kimi"),
    keywords: ["moonshot", "kimi", "月之暗面"],
  },
  {
    key: "qwen",
    label: "通义千问 Qwen",
    labelEn: "Alibaba Qwen",
    group: "china",
    color: hex("qwen", "#615CED"),
    path: p("qwen"),
    keywords: ["qwen", "通义", "千问", "dashscope", "aliyun", "alibaba", "bailian"],
  },
  {
    key: "doubao",
    label: "字节豆包 / 火山方舟",
    labelEn: "ByteDance Doubao",
    group: "china",
    color: hex("bytedance", "#3C8CFF"),
    path: p("bytedance"),
    keywords: ["doubao", "豆包", "volces", "volcengine", "ark", "bytedance", "seed-"],
  },
  {
    key: "baidu",
    label: "百度文心 / 千帆",
    labelEn: "Baidu ERNIE",
    group: "china",
    color: hex("baidu", "#2932E1"),
    path: p("baidu"),
    keywords: ["baidu", "百度", "ernie", "文心", "qianfan", "wenxin"],
  },
  {
    key: "hunyuan",
    label: "腾讯混元",
    labelEn: "Tencent Hunyuan",
    group: "china",
    color: "#0052D9",
    keywords: ["hunyuan", "混元", "tencent", "腾讯"],
  },
  {
    key: "minimax",
    label: "MiniMax",
    labelEn: "MiniMax",
    group: "china",
    color: hex("minimax", "#E73562"),
    path: p("minimax"),
    keywords: ["minimax", "abab"],
  },
  {
    key: "stepfun",
    label: "阶跃星辰 StepFun",
    labelEn: "StepFun",
    group: "china",
    color: "#005CFF",
    keywords: ["stepfun", "阶跃", "step-1", "step-2"],
  },
  {
    key: "spark",
    label: "讯飞星火",
    labelEn: "iFlytek Spark",
    group: "china",
    color: "#0052D9",
    keywords: ["iflytek", "讯飞", "spark", "xf-yun"],
  },
  {
    key: "baichuan",
    label: "百川 Baichuan",
    labelEn: "Baichuan",
    group: "china",
    color: "#FF6933",
    keywords: ["baichuan", "百川"],
  },

  // ---------------- 聚合 / 中转 / 托管 ----------------
  {
    key: "openrouter",
    label: "OpenRouter",
    labelEn: "OpenRouter",
    group: "gateway",
    color: hex("openrouter", "#94A3B8"),
    path: p("openrouter"),
    keywords: ["openrouter"],
  },
  {
    key: "siliconflow",
    label: "硅基流动 SiliconFlow",
    labelEn: "SiliconFlow",
    group: "gateway",
    color: "#7C3AED",
    keywords: ["siliconflow", "硅基", "siliconcloud"],
  },
  {
    key: "groq",
    label: "Groq",
    labelEn: "Groq",
    group: "gateway",
    color: "#F55036",
    keywords: ["groq"],
  },
  {
    key: "together",
    label: "Together AI",
    labelEn: "Together AI",
    group: "gateway",
    color: "#0F6FFF",
    keywords: ["together"],
  },
  {
    key: "fireworks",
    label: "Fireworks AI",
    labelEn: "Fireworks AI",
    group: "gateway",
    color: "#5019C5",
    keywords: ["fireworks"],
  },
  {
    key: "novita",
    label: "Novita AI",
    labelEn: "Novita AI",
    group: "gateway",
    color: "#23D57C",
    keywords: ["novita"],
  },
  {
    key: "deepinfra",
    label: "DeepInfra",
    labelEn: "DeepInfra",
    group: "gateway",
    color: "#3B82F6",
    keywords: ["deepinfra"],
  },
  {
    key: "huggingface",
    label: "Hugging Face",
    labelEn: "Hugging Face",
    group: "gateway",
    color: hex("huggingface", "#FFD21E"),
    path: p("huggingface"),
    keywords: ["huggingface", "hf.co"],
  },
  {
    key: "azure",
    label: "Azure OpenAI",
    labelEn: "Azure OpenAI",
    group: "gateway",
    color: "#0078D4",
    keywords: ["azure", "openai.azure.com", "cognitiveservices"],
  },
  {
    key: "bedrock",
    label: "AWS Bedrock",
    labelEn: "AWS Bedrock",
    group: "gateway",
    color: "#FF9900",
    keywords: ["bedrock", "amazonaws"],
  },

  // ---------------- 本机推理 ----------------
  {
    key: "ollama",
    label: "Ollama（本机）",
    labelEn: "Ollama (local)",
    group: "local",
    // 官方主色纯黑 → 深色模式反色
    color: hex("ollama", "#000000"),
    path: p("ollama"),
    keywords: ["ollama", "11434"],
  },
  {
    key: "lmstudio",
    label: "LM Studio（本机）",
    labelEn: "LM Studio (local)",
    group: "local",
    color: hex("lmstudio", "#000000"),
    path: p("lmstudio"),
    keywords: ["lmstudio", "lm-studio", "1234"],
  },
  {
    key: "vllm",
    label: "vLLM（自建）",
    labelEn: "vLLM (self-hosted)",
    group: "local",
    color: hex("vllm", "#30A2FF"),
    path: p("vllm"),
    keywords: ["vllm"],
  },
];

const BY_KEY = new Map(BRANDS.map((b) => [b.key, b]));

/** 某个字符串是否是内置预设键（而非用户上传的 data-URL）。 */
export function isPresetBrand(v: string | undefined): boolean {
  return !!v && BY_KEY.has(v);
}

export function getBrand(key: string | undefined): Brand | undefined {
  return key ? BY_KEY.get(key) : undefined;
}

/**
 * 按 vendor id、厂商名或模型名启发式推断品牌。
 *
 * **最长关键词优先**，不是「表里谁先谁赢」。理由与单价表那张同源：
 * 靠顺序决定语义时，排错了不报错、只是悄悄认成另一家 ——
 * 而这里认错的表现是「图标是别家的」，用户会以为程序把 Key 搞混了。
 */
export function resolveBrand(hint: string | undefined): Brand | undefined {
  if (!hint) return undefined;
  const s = hint.toLowerCase();
  let best: Brand | undefined;
  let bestLen = 0;
  for (const b of BRANDS) {
    for (const kw of b.keywords) {
      if (s.includes(kw) && kw.length > bestLen) {
        best = b;
        bestLen = kw.length;
      }
    }
  }
  return best;
}

/**
 * 这个颜色在深色背景上是不是「等于看不见」。
 *
 * 好几家的官方主色是纯黑或近黑（Kimi / Ollama / LM Studio `#000000`、
 * Anthropic `#191919`、xAI `#111111`）。用感知亮度而不是三通道均值 ——
 * 人眼对绿最敏感、对蓝最不敏感，等权重会把深蓝判成「够亮」。
 *
 * 阈值 **0.2** 的取法（实算过，不是拍的）：
 * ```text
 * #000000 → 0        ← 要提亮
 * #111111 → 0.067    ← 要提亮
 * #191919 → 0.098    ← 要提亮
 * #0052D9 → 0.286    ← 不提亮（饱和蓝，白字压上去对比够）
 * #4285F4 → 0.493    ← 不提亮
 * #D4915D → 0.624    ← 不提亮
 * ```
 * 0.2 落在 0.098 与 0.286 之间，两侧都有近 3 倍余量。刻意不取 0.28 ——
 * 那会贴着 `#0052D9` 的 0.286，任何一家品牌色微调都可能把它翻到另一边。
 */
export function isNearBlack(hexColor: string): boolean {
  const m = /^#?([0-9a-f]{6})$/i.exec(hexColor.trim());
  if (!m) return false;
  const n = parseInt(m[1], 16);
  const r = ((n >> 16) & 255) / 255;
  const g = ((n >> 8) & 255) / 255;
  const b = (n & 255) / 255;
  return 0.299 * r + 0.587 * g + 0.114 * b < 0.2;
}
