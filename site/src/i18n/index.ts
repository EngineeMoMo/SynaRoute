/**
 * 官网取词。
 *
 * 沿用桌面应用 `src/lib/i18n.ts` 的约定：扁平 key、zh 为基准语言、
 * en 缺词回退 zh、再回退 key 本身，保证永远不渲染空串。支持 {name} 占位插值。
 *
 * 与应用不同的是：官网的语言来自 URL 路径段（/zh/... 或 /en/...），
 * 不是应用里那份存进配置的偏好 —— 这样分享出去的链接自带语言，
 * 且搜索引擎能分别收录两个语言版本。
 */

import { zh } from "./zh";
import { en } from "./en";

export type Lang = "zh" | "en";

export const LANGS: { value: Lang; label: string }[] = [
  { value: "zh", label: "中文" },
  { value: "en", label: "English" },
];

export const DEFAULT_LANG: Lang = "zh";

export type Dict = Record<string, string>;

const dicts: Record<Lang, Dict> = { zh, en };

export function isLang(v: unknown): v is Lang {
  return v === "zh" || v === "en";
}

/** 取词：按当前语言查找，缺失回退 zh，再回退 key */
export function translate(lang: Lang, key: string, vars?: Record<string, string | number>): string {
  const raw = dicts[lang]?.[key] ?? dicts.zh[key] ?? key;
  if (!vars) return raw;
  return raw.replace(/\{(\w+)\}/g, (_, k: string) => (vars[k] != null ? String(vars[k]) : `{${k}}`));
}

export type TFunc = (key: string, vars?: Record<string, string | number>) => string;

/**
 * 首次访问 `/` 时按浏览器语言挑一个。
 * 只认前缀 `zh`，其余一律英文 —— 与其猜错不如给一个明确的默认。
 */
export function detectLang(): Lang {
  if (typeof navigator === "undefined") return DEFAULT_LANG;
  const langs = navigator.languages?.length ? navigator.languages : [navigator.language];
  for (const l of langs) {
    if (typeof l === "string" && l.toLowerCase().startsWith("zh")) return "zh";
  }
  return "en";
}
