import { type ClassValue, clsx } from "clsx";
import { twMerge } from "tailwind-merge";

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

/** 相对时间格式化（用于"上次检查/拉取"展示） */
export function formatRelativeTime(ts: number | null | undefined): string {
  if (!ts) return "从未";
  const diff = Date.now() - ts;
  const sec = Math.floor(diff / 1000);
  if (sec < 60) return `${sec} 秒前`;
  const min = Math.floor(sec / 60);
  if (min < 60) return `${min} 分钟前`;
  const hour = Math.floor(min / 60);
  if (hour < 24) return `${hour} 小时前`;
  const day = Math.floor(hour / 24);
  return `${day} 天前`;
}
