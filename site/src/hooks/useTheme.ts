import { useCallback, useEffect, useState } from "react";

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
 * 深浅主题。
 *
 * 主题类加在 <html> 上而不是 <body>，因为 Tailwind 的 darkMode:["class"]
 * 默认从祖先找 .dark，加在 html 上能覆盖到 portal 渲染出去的弹窗。
 *
 * 首屏闪白的问题在 index.html 里用一小段内联脚本先行处理（在 React 挂载前
 * 就把类加上），这里只负责后续的切换与持久化。
 */
export function useTheme() {
  const [theme, setTheme] = useState<Theme>(readInitialTheme);

  useEffect(() => {
    const root = document.documentElement;
    root.classList.toggle("dark", theme === "dark");
    root.style.colorScheme = theme;
    window.localStorage.setItem(STORAGE_KEY, theme);
  }, [theme]);

  // 用户没有显式选择过时，跟随系统的后续变化
  useEffect(() => {
    if (window.localStorage.getItem(STORAGE_KEY)) return;
    const mq = window.matchMedia("(prefers-color-scheme: dark)");
    const onChange = (e: MediaQueryListEvent) => setTheme(e.matches ? "dark" : "light");
    mq.addEventListener("change", onChange);
    return () => mq.removeEventListener("change", onChange);
  }, []);

  const toggle = useCallback(() => setTheme((t) => (t === "dark" ? "light" : "dark")), []);

  return { theme, setTheme, toggle };
}
