import { useMemo } from "react";

/**
 * 品牌图标：按 vendor id / 名称 / 模型名匹配内置 SVG logo。
 * 未知厂商回退为名称首字母的配色圆形占位（参考 ccswitch 风格）。
 *
 * 图标为简化单色路径，通过 currentColor 上色，随品牌配色变化。
 */

type BrandKey =
  | "anthropic"
  | "openai"
  | "deepseek"
  | "zhipu"
  | "moonshot"
  | "gemini"
  | "qwen"
  | "mistral";

/**
 * 可供用户**显式挑选**的内置品牌清单（厂商管理页的图标选择器用）。
 *
 * ## 为什么需要「显式挑选」，而不是只靠自动匹配
 *
 * `resolveBrand` 按 vendor id / 名称 / 模型名做启发式匹配，覆盖了大多数情况。但中转站
 * 的名字千奇百怪（「小明API」「林夕公益站」「百倍」），猜不中时只能退化成首字母色块 ——
 * 用户明知这是个 Claude 中转，却没有任何地方能告诉程序。cc-switch 给了这个入口，我们没有。
 *
 * ## 与 cc-switch 的存储形态对齐（从其本机库取证，非推测）
 *
 * 读 `~/.cc-switch/cc-switch.db` 的 `providers` 表可见：`icon` 列存的是**预设键字符串**
 * （`anthropic`、`openai`），配套 `icon_color` 存品牌色（`#D4915D`、`#00A67E`），
 * **不是** data-URL。故我们沿用同一模型：`Vendor.icon` 既可放 data-URL（自定义上传），
 * 也可放这里的预设键 —— 字段类型无需改动（本就是 `Option<String>`），
 * 靠 `isPresetBrand` 在渲染时区分两者。
 *
 * 显示名用中文常用叫法而非厂商英文全称：用户是在「我这个 Key 是哪家的」这个语境下选的。
 */
export const PRESET_BRANDS: { key: BrandKey; label: string }[] = [
  { key: "anthropic", label: "Anthropic / Claude" },
  { key: "openai", label: "OpenAI / GPT" },
  { key: "deepseek", label: "DeepSeek 深度求索" },
  { key: "zhipu", label: "智谱 GLM" },
  { key: "moonshot", label: "月之暗面 Kimi" },
  { key: "qwen", label: "通义千问 Qwen" },
  { key: "gemini", label: "Google Gemini" },
  { key: "mistral", label: "Mistral" },
];

/** 某个字符串是否是内置预设键（而非用户上传的 data-URL）。 */
export function isPresetBrand(v: string | undefined): v is BrandKey {
  return !!v && PRESET_BRANDS.some((b) => b.key === v);
}

/** 每个品牌的主色（用于图标底色 tint） */
const BRAND_COLOR: Record<BrandKey, string> = {
  anthropic: "#D97757",
  openai: "#000000",
  deepseek: "#4D6BFE",
  zhipu: "#3859FF",
  moonshot: "#000000",
  gemini: "#1C69FF",
  qwen: "#615CED",
  mistral: "#FA520F",
};

/** 内置品牌 SVG（24x24 viewBox，路径用 fill=currentColor 上色） */
const BRAND_SVG: Record<BrandKey, JSX.Element> = {
  // Anthropic —— 星芒标记
  anthropic: (
    <path
      fill="currentColor"
      d="M13.83 4h-2.9L5.3 20h2.98l1.15-3.16h5.99L16.57 20h2.98L13.83 4Zm-3.5 10.2 2.05-5.63 2.05 5.63h-4.1Z"
    />
  ),
  // OpenAI —— 花瓣环
  openai: (
    <path
      fill="currentColor"
      d="M21.55 10.02a5.42 5.42 0 0 0-.47-4.45 5.5 5.5 0 0 0-5.92-2.63A5.42 5.42 0 0 0 11.07 1a5.5 5.5 0 0 0-5.24 3.8A5.42 5.42 0 0 0 2.2 7.43a5.5 5.5 0 0 0 .68 6.44 5.42 5.42 0 0 0 .47 4.45 5.5 5.5 0 0 0 5.92 2.63A5.42 5.42 0 0 0 12.93 23a5.5 5.5 0 0 0 5.24-3.8 5.42 5.42 0 0 0 3.63-2.63 5.5 5.5 0 0 0-.68-6.44 5.4 5.4 0 0 0-.57-.11Zm-8.1 11.3a4.07 4.07 0 0 1-2.62-.95l.13-.07 4.35-2.51a.71.71 0 0 0 .36-.62v-6.13l1.84 1.06v5.07a4.1 4.1 0 0 1-4.06 4.15Zm-8.73-3.73a4.07 4.07 0 0 1-.49-2.74l.13.08 4.35 2.51a.71.71 0 0 0 .72 0l5.31-3.06v2.12l-4.4 2.54a4.1 4.1 0 0 1-5.6-1.5ZM3.57 8.14a4.07 4.07 0 0 1 2.13-1.79v5.16a.71.71 0 0 0 .36.62l5.31 3.06-1.84 1.06-4.35-2.51a4.1 4.1 0 0 1-1.62-5.6Zm15.11 3.51-5.31-3.07 1.84-1.06 4.35 2.51a4.1 4.1 0 0 1-.63 7.39v-5.16a.71.71 0 0 0-.25-.61Zm1.83-2.75-.13-.08-4.35-2.51a.71.71 0 0 0-.72 0L10.4 9.37V7.25l4.4-2.54a4.1 4.1 0 0 1 6.09 4.25Zm-11.56 3.8-1.84-1.06V6.57a4.1 4.1 0 0 1 6.72-3.15l-.13.07L9.32 5.99a.71.71 0 0 0-.36.62v5.09Zm1-2.15L12.24 9.4l2.5 1.44v2.88l-2.5 1.44-2.5-1.44v-2.87Z"
    />
  ),
  // DeepSeek —— 鲸鱼曲线（简化）
  deepseek: (
    <path
      fill="currentColor"
      d="M22 5.5c-.5-.25-.72.23-1.02.47-.1.08-.2.19-.28.29-.72.77-1.56 1.28-2.66 1.22-1.6-.1-2.97.4-4.18 1.63-.26-1.5-1.11-2.4-2.4-2.98a15.6 15.6 0 0 1-1.75-.98c-.44-.3-.53-.63-.28-1.08.06-.11.13-.22.18-.33.13-.3.13-.56-.07-.72-.2-.16-.44-.09-.66.03-.9.5-1.24 1.35-1.28 2.33-.02.53.03 1.06.06 1.58.06 1.24.62 2.17 1.65 2.87.28.19.31.38.22.66-.16.55-.36 1.09-.53 1.64-.11.35-.27.42-.63.28a5.5 5.5 0 0 1-2.63-2.16c-.55-.84-1.05-1.7-1.75-2.42-.17-.17-.34-.36-.55-.48-.44-.26-.83-.07-.91.44-.06.36.07.68.24.99.38.66.83 1.27 1.28 1.88.34.46.28.65-.24.9l-.1.05c-.5.24-.55.53-.19.94.68.78 1.53 1.32 2.53 1.62 1.15.35 2.32.42 3.5.16.55-.12.55-.12.7.44l.03.11c.29 1.05.86 1.94 1.85 2.5.44.24.9.35 1.4.32.24-.01.4-.14.4-.4 0-.13-.02-.27-.05-.4-.1-.5-.32-.95-.7-1.3-.14-.14-.3-.28-.3-.5 0-.15.1-.28.25-.34.4-.15.8-.27 1.2-.42 1.94-.72 3.24-2.05 3.66-4.13.28-1.36.15-2.7-.32-4.02-.06-.17-.1-.35-.02-.53.28-.62.28-1.24.05-1.87-.06-.16-.06-.35.02-.51.15-.3.28-.61.27-.97 0-.28-.15-.4-.42-.28Z"
    />
  ),
  // 智谱 GLM —— Z 字形
  zhipu: (
    <path fill="currentColor" d="M5 5h14v2.4L9.2 18.6H19V21H5v-2.4L14.8 7.4H5V5Z" />
  ),
  // Kimi / Moonshot —— 月牙
  moonshot: (
    <path
      fill="currentColor"
      d="M13.5 3a9 9 0 1 0 7.5 14 7 7 0 0 1-7.5-11 7 7 0 0 1 3.1-2.6A9 9 0 0 0 13.5 3Z"
    />
  ),
  // Gemini —— 四角星
  gemini: (
    <path
      fill="currentColor"
      d="M12 2c.4 4.7 3.3 7.6 8 8-4.7.4-7.6 3.3-8 8-.4-4.7-3.3-7.6-8-8 4.7-.4 7.6-3.3 8-8Z"
    />
  ),
  // Qwen —— 双线
  qwen: (
    <path
      fill="currentColor"
      d="M7 4h3l4.5 8L19 4h-3l-3 5.3L10 4H7Zm0 16h3l-1.5-2.6L7 20Zm7-8 4.5 8h-3L11 12h3Z"
    />
  ),
  // Mistral —— 网格
  mistral: (
    <path
      fill="currentColor"
      d="M4 4h4v4H4V4Zm12 0h4v4h-4V4ZM4 10h4v4H4v-4Zm6 0h4v4h-4v-4Zm6 0h4v4h-4v-4ZM4 16h4v4H4v-4Zm12 0h4v4h-4v-4Z"
    />
  ),
};

/** 按 vendor id、厂商名或模型名启发式推断品牌 */
function resolveBrand(hint: string | undefined): BrandKey | null {
  if (!hint) return null;
  const s = hint.toLowerCase();
  const match: [BrandKey, string[]][] = [
    ["anthropic", ["anthropic", "claude"]],
    ["openai", ["openai", "gpt", "o1", "o3", "o4", "chatgpt"]],
    ["deepseek", ["deepseek"]],
    ["zhipu", ["zhipu", "glm", "bigmodel", "智谱"]],
    ["moonshot", ["moonshot", "kimi", "月之暗面"]],
    ["gemini", ["gemini", "google"]],
    ["qwen", ["qwen", "通义", "千问", "dashscope", "aliyun"]],
    ["mistral", ["mistral", "mixtral"]],
  ];
  for (const [brand, keys] of match) {
    if (keys.some((k) => s.includes(k))) return brand;
  }
  return null;
}

/** 名称首字母（英文取首字母，中文取首字） */
function initial(label: string): string {
  const trimmed = label.trim();
  if (!trimmed) return "?";
  return trimmed[0].toUpperCase();
}

/** 稳定地为未知品牌挑一个占位底色 */
const FALLBACK_COLORS = ["#6366F1", "#0EA5E9", "#10B981", "#F59E0B", "#EF4444", "#8B5CF6", "#EC4899"];
function fallbackColor(label: string): string {
  let h = 0;
  for (let i = 0; i < label.length; i++) h = (h * 31 + label.charCodeAt(i)) >>> 0;
  return FALLBACK_COLORS[h % FALLBACK_COLORS.length];
}

interface BrandIconProps {
  /** vendor id、厂商名或模型名，任一即可用于推断品牌 */
  hint?: string;
  /** 无法推断品牌时，用于生成首字母占位的显示名 */
  fallbackLabel?: string;
  /**
   * 用户**显式指定**的图标，两种形态（与 cc-switch 的 `icon` 列同一模型，见 `PRESET_BRANDS`）：
   * - **预设键**（`"anthropic"`、`"zhipu"`…）→ 渲染内置品牌 SVG，等于「我就要用这家的图标」；
   * - **data-URL**（`data:image/png;base64,…`）→ 渲染上传的自定义图片。
   *
   * 两者都**优先于**启发式匹配 —— 显式选择必须胜过程序的猜测，否则用户会发现「选了没用」。
   */
  iconUrl?: string;
  size?: number;
  className?: string;
}

export function BrandIcon({ hint, fallbackLabel, iconUrl, size = 18, className }: BrandIconProps) {
  // 显式预设键最高优先：用户挑了就按他挑的画，不再让启发式插手。
  const explicitBrand = isPresetBrand(iconUrl) ? iconUrl : null;
  const brand = useMemo(
    () => explicitBrand ?? resolveBrand(hint) ?? resolveBrand(fallbackLabel),
    [explicitBrand, hint, fallbackLabel],
  );

  // 自定义上传图标（data-URL）：仅当它**不是**预设键时才走 <img>。
  // 预设键不是 URL，塞进 src 会被浏览器当相对路径去请求、渲染成碎图标；
  // 它已在上面并进 `brand`，由下面的内置 SVG 分支渲染。
  if (iconUrl && !explicitBrand) {
    return (
      <span
        className={`inline-flex shrink-0 items-center justify-center overflow-hidden rounded-full bg-surface-hover ${className ?? ""}`}
        style={{ width: size, height: size }}
        title={hint ?? fallbackLabel}
      >
        <img src={iconUrl} alt="" width={size} height={size} className="h-full w-full object-cover" />
      </span>
    );
  }

  if (brand) {
    return (
      <span
        className={`inline-flex shrink-0 items-center justify-center rounded-full ${className ?? ""}`}
        style={{
          width: size,
          height: size,
          color: BRAND_COLOR[brand],
          background: `${BRAND_COLOR[brand]}1a`, // 10% alpha 底色
        }}
        title={hint ?? fallbackLabel}
      >
        <svg width={size * 0.68} height={size * 0.68} viewBox="0 0 24 24" aria-hidden="true">
          {BRAND_SVG[brand]}
        </svg>
      </span>
    );
  }

  // 未知品牌：首字母配色圆形
  const label = fallbackLabel ?? hint ?? "?";
  const color = fallbackColor(label);
  return (
    <span
      className={`inline-flex shrink-0 items-center justify-center rounded-full font-semibold text-white ${className ?? ""}`}
      style={{ width: size, height: size, background: color, fontSize: size * 0.5 }}
      title={label}
    >
      {initial(label)}
    </span>
  );
}
