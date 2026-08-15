import { type ClassValue, clsx } from "clsx";
import { twMerge } from "tailwind-merge";
import type { TFunc } from "@/lib/i18n";

/** 合并 Tailwind class，处理冲突（shadcn/ui 标准工具函数） */
export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}

/** 掩码显示密钥：保留前后各若干位，中间用 • 替代 */
export function maskSecret(secret: string, visible = 4): string {
  if (!secret) return "";
  if (secret.length <= visible * 2) return "•".repeat(8);
  return `${secret.slice(0, visible)}${"•".repeat(8)}${secret.slice(-visible)}`;
}

/**
 * 相对时间格式化（用于「上次检查/拉取」展示）。
 *
 * `t` 是**必填**的，这是刻意的：这个函数原先直接返回中文串（`${min} 分钟前`），
 * 而调用方还把结果套进 `t("key.healthCheckLabel", { time })` —— 外层翻译了、内层没翻，
 * 英文界面下显示「Checked 3 分钟前」。做成必填参数后，漏改的调用点会在**编译期**报错，
 * 而不是等到有人切到英文才发现。别为了省事加默认值。
 */
export function formatRelativeTime(ts: number | null | undefined, t: TFunc): string {
  if (!ts) return t("time.never");
  const diff = Date.now() - ts;
  const sec = Math.max(0, Math.floor(diff / 1000));
  if (sec < 60) return t("time.secondsAgo", { n: sec });
  const min = Math.floor(sec / 60);
  if (min < 60) return t("time.minutesAgo", { n: min });
  const hour = Math.floor(min / 60);
  if (hour < 24) return t("time.hoursAgo", { n: hour });
  const day = Math.floor(hour / 24);
  return t("time.daysAgo", { n: day });
}
