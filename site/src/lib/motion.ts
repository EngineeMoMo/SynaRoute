/**
 * 尊重系统的「减少动态效果」设置（`prefers-reduced-motion: reduce`）。
 *
 * # 为什么需要这一层
 *
 * `styles.css` 里已有一条 `@media (prefers-reduced-motion: reduce) { html { scroll-behavior: auto } }`，
 * 但它**管不到 JS**：`scrollIntoView({ behavior: "smooth" })` 与
 * `scrollTo({ behavior: "smooth" })` 里的 `behavior` 是**显式参数**，
 * 优先级高于 CSS 的 `scroll-behavior`。于是那道 CSS 兜底被四处调用点逐个盖掉：
 * 顶栏锚点、页脚锚点、返回顶部、路由 hash 跳转。
 *
 * 后果对特定人群是实打实的：开这个设置的多是前庭功能敏感 / 晕动症用户，
 * 而首页高约 14000px —— 点一次页脚锚点就是一万像素的平滑滚动动画，
 * 正是该设置要消除的那类动效。
 *
 * # 为什么每次都查、不缓存
 *
 * 这个偏好可以在系统里随时改，用户改完不该需要刷新页面。`matchMedia` 查一次极便宜。
 */
export function prefersReducedMotion(): boolean {
  return (
    typeof window !== "undefined" &&
    typeof window.matchMedia === "function" &&
    window.matchMedia("(prefers-reduced-motion: reduce)").matches
  );
}

/**
 * 该用哪种滚动行为。全站**所有** JS 滚动都要经过它，不许直接写
 * `behavior: "smooth"` —— 有一条策略门（`scripts/check-reduced-motion.mjs`）盯着这件事，
 * 因为「记得查一下偏好」是四个调用点各自要记的纪律，而漏掉的表现是静默的
 * （对不开这个设置的人毫无差别，所以自测时永远看不出来）。
 */
export function scrollBehavior(): ScrollBehavior {
  return prefersReducedMotion() ? "auto" : "smooth";
}

/** 滚到某个锚点元素。 */
export function scrollToId(id: string) {
  document.getElementById(id)?.scrollIntoView({ behavior: scrollBehavior(), block: "start" });
}

/** 滚回页首。 */
export function scrollToTop() {
  window.scrollTo({ top: 0, behavior: scrollBehavior() });
}
