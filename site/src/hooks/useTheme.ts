import { useCallback, useEffect, useSyncExternalStore } from "react";

export type Theme = "light" | "dark";

const STORAGE_KEY = "synaroute-site-theme";

/** 读用户此前的选择；没有选择过就跟随系统 */
function readInitialTheme(): Theme {
  if (typeof window === "undefined") return "light";
  const saved = window.localStorage.getItem(STORAGE_KEY);
  if (saved === "light" || saved === "dark") return saved;
  return window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
}

/**
 * 主题状态存在模块级、用 useSyncExternalStore 订阅，**不是** useState。
 *
 * 原因是踩过的坑：useState 版本每个调用点各有一份独立 state。顶栏的切换按钮翻了自己那份，
 * 副作用把 `<html class="dark">` 加上了，于是页面变暗；但 Hero / Screenshots /
 * BrainSpotlight 三处仍拿着 `"light"`，截图还是浅色版 —— 深色站配一堆亮白截图。
 * 换成单一外部源后，所有调用点读同一个值。
 */
let current: Theme = readInitialTheme();
const listeners = new Set<() => void>();

function subscribe(cb: () => void) {
  listeners.add(cb);
  return () => listeners.delete(cb);
}

function getSnapshot(): Theme {
  return current;
}

function setThemeGlobal(next: Theme) {
  if (next === current) return;
  current = next;
  listeners.forEach((cb) => cb());
}

/** 把主题落到 DOM 与 localStorage。写在组件外，切换与初始化共用一份逻辑。 */
function applyTheme(theme: Theme, persist: boolean) {
  const root = document.documentElement;
  root.classList.toggle("dark", theme === "dark");
  root.style.colorScheme = theme;
  if (persist) window.localStorage.setItem(STORAGE_KEY, theme);
}

/**
 * 深浅主题。
 *
 * 主题类加在 <html> 上而不是 <body>，因为 Tailwind 的 darkMode:["class"]
 * 默认从祖先找 .dark，加在 html 上能覆盖到 portal 渲染出去的弹窗。
 *
 * 首屏闪白的问题在 index.html 里用一小段内联脚本先行处理（在 React 挂载前
 * 就把类加上），这里只负责后续的切换与持久化。
 */
export function useTheme() {
  const theme = useSyncExternalStore(subscribe, getSnapshot, () => "light" as Theme);

  // 只在用户主动切换时写 localStorage：挂载时就写会把「跟随系统」变成显式选择，
  // 之后系统切深色页面也不再跟着变了。
  useEffect(() => {
    applyTheme(theme, window.localStorage.getItem(STORAGE_KEY) !== null);
  }, [theme]);

  // 用户没有显式选择过时，跟随系统的后续变化
  useEffect(() => {
    if (window.localStorage.getItem(STORAGE_KEY)) return;
    const mq = window.matchMedia("(prefers-color-scheme: dark)");
    const onChange = (e: MediaQueryListEvent) => setThemeGlobal(e.matches ? "dark" : "light");
    mq.addEventListener("change", onChange);
    return () => mq.removeEventListener("change", onChange);
  }, []);

  const setTheme = useCallback((next: Theme) => {
    window.localStorage.setItem(STORAGE_KEY, next);
    setThemeGlobal(next);
  }, []);

  const toggle = useCallback(() => {
    const next: Theme = current === "dark" ? "light" : "dark";
    window.localStorage.setItem(STORAGE_KEY, next);
    setThemeGlobal(next);
  }, []);

  return { theme, setTheme, toggle };
}
