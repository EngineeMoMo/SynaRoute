import { useEffect, useRef } from "react";

/**
 * 元素进入视口时淡入。
 *
 * 用 IntersectionObserver 而不是引入动画库 —— 需要的只是「进视口加个类」。
 *
 * 两条刻意的行为：
 * - 只触发一次（`unobserve`），来回滚动不会反复闪动。
 * - 浏览器不支持 IntersectionObserver 时**直接显示**，而不是留在初始的透明状态。
 *   动画失效可以接受，内容看不见不行。
 */
export function useReveal<T extends HTMLElement = HTMLDivElement>(delayMs = 0) {
  const ref = useRef<T>(null);

  useEffect(() => {
    const el = ref.current;
    if (!el) return;

    if (typeof IntersectionObserver === "undefined") {
      el.classList.remove("reveal");
      return;
    }

    const observer = new IntersectionObserver(
      (entries) => {
        for (const entry of entries) {
          if (!entry.isIntersecting) continue;
          const target = entry.target as HTMLElement;
          target.style.animationDelay = `${delayMs}ms`;
          target.classList.add("reveal-in");
          observer.unobserve(target);
        }
      },
      // 元素露出约 1/8 就开始，等到完全进入才动会显得迟钝
      { threshold: 0.12, rootMargin: "0px 0px -40px 0px" }
    );

    observer.observe(el);
    return () => observer.disconnect();
  }, [delayMs]);

  return ref;
}
