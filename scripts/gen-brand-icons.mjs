// 一次性生成脚本：从 simple-icons（CC0-1.0）抽出本项目需要的品牌 SVG path，
// 生成 src/components/brandIcons.generated.ts。
//
// 为什么是「抽出来生成」而不是加依赖：
// - simple-icons 解包 16 MB / 3453 个图标，本项目只要 20 个；
// - @lobehub/icons 虽然是专做 AI 厂商图标的，但它依赖 antd-style、
//   peerDependencies 要 antd@^6 + react@^19 —— 本项目是 React 18，
//   为几个图标引入一整套 UI 库不成比例。
// 生成一份数据文件之后运行时零依赖、产物只多这 20 段 path。
//
// 用法（临时装一次 simple-icons 再删掉）：
//   npm i -D simple-icons@16 && node scripts/gen-brand-icons.mjs && npm rm simple-icons
import { writeFileSync } from "node:fs";
import * as si from "simple-icons";

/**
 * 要抽的品牌：`[本项目的预设键, simple-icons 的 slug]`。
 *
 * 不在这张表里的厂商（OpenAI / xAI / 智谱 / 腾讯混元 / Groq / Together / Cohere /
 * 阶跃 / 硅基流动 / 火山豆包 / 讯飞 …）simple-icons 里**没有**，
 * 它们走首字母色块兜底 —— 那是刻意的：宁可显示一个干净的字母块，
 * 也不画一个「看起来像但其实不是」的假 logo。
 * 参照：cc-switch 给中转站也完全没有图标（实测它 20 条 provider 里只有 3 条有）。
 */
const WANT = [
  ["anthropic", "anthropic"],
  ["deepseek", "deepseek"],
  ["kimi", "kimi"],
  ["qwen", "qwen"],
  ["mistral", "mistralai"],
  ["ollama", "ollama"],
  ["gemini", "googlegemini"],
  ["meta", "metaai"],
  ["perplexity", "perplexity"],
  ["openrouter", "openrouter"],
  ["baidu", "baidu"],
  ["minimax", "minimax"],
  ["huggingface", "huggingface"],
  ["bytedance", "bytedance"],
  ["lmstudio", "lmstudio"],
  ["vllm", "vllm"],
];

const bySlug = new Map();
for (const k of Object.keys(si)) {
  const ic = si[k];
  if (ic && ic.slug) bySlug.set(ic.slug, ic);
}

const rows = [];
const missing = [];
for (const [key, slug] of WANT) {
  const ic = bySlug.get(slug);
  if (!ic) {
    missing.push(`${key} → ${slug}`);
    continue;
  }
  rows.push({ key, title: ic.title, hex: `#${ic.hex}`, path: ic.path });
}
if (missing.length) {
  throw new Error(`simple-icons 里找不到这些 slug（上游改名了？）：${missing.join(", ")}`);
}

const header = `// 本文件由 scripts/gen-brand-icons.mjs **生成**，不要手改。
//
// 图标数据来自 simple-icons（https://simpleicons.org），许可证 **CC0-1.0**
// （公共领域，无署名要求；这里注明只为让下一个人知道从哪儿更新）。
// 各品牌标识的商标权归各自所有者；这里的用法是「在客户端里标识该厂商」，
// 属于指称性使用。
//
// 重新生成：
//   npm i -D simple-icons@16 && node scripts/gen-brand-icons.mjs && npm rm simple-icons
//
// viewBox 一律 24x24，path 用 fill="currentColor" 上色。
// \`hex\` 是该品牌的官方主色 —— 注意其中若干是**纯黑或近黑**
// （Moonshot/Kimi/Ollama/LM Studio 是 #000000、Anthropic 是 #191919），
// 深色模式下必须反转，否则等于看不见。那条规则在 brandIcons.tsx 里，别在这儿处理。

export interface GeneratedBrandIcon {
  /** 本项目的预设键 */
  key: string;
  /** simple-icons 里的品牌名（供排查用） */
  title: string;
  /** 官方主色（#RRGGBB） */
  hex: string;
  /** 24x24 viewBox 的单条 path */
  path: string;
}

export const GENERATED_BRAND_ICONS: GeneratedBrandIcon[] = ${JSON.stringify(rows, null, 2)};
`;

writeFileSync("src/components/brandIcons.generated.ts", header);
console.log(`已生成 ${rows.length} 个品牌图标 → src/components/brandIcons.generated.ts`);
