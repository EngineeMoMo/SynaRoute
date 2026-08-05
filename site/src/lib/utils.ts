import { type ClassValue, clsx } from "clsx";
import { twMerge } from "tailwind-merge";

/** 合并 Tailwind class，处理冲突（与桌面应用同一个工具函数） */
export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}

/** 字节数转可读大小，用于展示安装包体积 */
export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return "";
  const mb = bytes / 1024 / 1024;
  if (mb >= 1) return `${mb.toFixed(1)} MB`;
  return `${Math.round(bytes / 1024)} KB`;
}

/** ISO 时间 → 按语言本地化的日期（只到天，发布日期不需要时分） */
export function formatDate(iso: string, lang: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "";
  return d.toLocaleDateString(lang === "zh" ? "zh-CN" : "en-US", {
    year: "numeric",
    month: "long",
    day: "numeric",
  });
}

/** 外链统一属性：模板第 19 节要求所有外链带 noopener noreferrer */
export const externalLinkProps = {
  target: "_blank",
  rel: "noopener noreferrer",
} as const;
