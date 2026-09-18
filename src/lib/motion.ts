/**
 * 动效收口：视图过渡 + 「减少动态效果」偏好。
 *
 * # 为什么必须有这一层
 *
 * `styles.css` 里已有一整段 `@media (prefers-reduced-motion: reduce)`，但它
 * **管不到 `document.startViewTransition()`** —— 那是个 JS API，调了就会开始
 * 截屏、构建过渡树、播动画，CSS 只能把动画时长压到 0，改变不了「过渡确实发生了」
 * 这件事（期间整棵 DOM 被冻结、点击不响应）。
 *
 * 对开了这个设置的人来说，这不是审美问题：前庭功能敏感 / 晕动症用户会因为整屏
 * 位移与交叉淡化产生实际不适，这正是 WCAG 2.2 动画准则关切的范畴。
 *
 * # 为什么有一条硬规则盯着它
 *
 * `scripts/check-forbidden.mjs` 的 `view-transition-must-go-through-motion-helper`
 * 禁止本文件之外出现裸 `document.startViewTransition(`。理由与官网那条
 * `reduced-motion-must-go-through-motion-helper` 完全同构：「记得先查一下偏好」
 * 是每个调用点各自要记的纪律，而漏掉的表现是**静默的** —— 对不开该设置的人
 * （也就是开发者自己）毫无差别，自测永远看不出来。
 *
 * ⚠️ 官网那条门只扫 `site/`，应用侧此前没有任何防线。
 */

/**
 * 系统是否要求「减少动态效果」。
 *
 * **每次实时查，不缓存**：这个偏好可以在系统设置里随时改，用户改完不该需要
 * 重启应用。`matchMedia` 查一次极便宜。
 */
export function prefersReducedMotion(): boolean {
  return (
    typeof window !== "undefined" &&
    typeof window.matchMedia === "function" &&
    window.matchMedia("(prefers-reduced-motion: reduce)").matches
  );
}

/** 本运行环境是否支持视图过渡。WebView2 内核实测 Edge 153，支持；浏览器预览同。 */
function supportsViewTransition(): boolean {
  return typeof document !== "undefined" && typeof document.startViewTransition === "function";
}

/**
 * 把一次 DOM 变更包进视图过渡里。
 *
 * `mutate` 里放**同步的状态更新**（React 的 setState 在 React 18 下会被
 * `startViewTransition` 的回调同步 flush，这正是它能工作的前提）。
 *
 * 三种情况下直接同步调用 `mutate()`、完全不开过渡：
 * ① API 不存在（老内核）；
 * ② 用户要求减少动态效果；
 * ③ 已经有一场过渡在跑 —— 连点导航时第二次调用会中止第一场，表现是画面
 *    「闪回再重放」，比没有过渡更糟。
 *
 * 🔴 **任何情况下 `mutate` 都必须被调用一次**：它承载的是真实的业务状态变更
 * （切页、切分类），不是装饰。早期一版在不支持时直接 return，结果点导航没反应。
 */
let transitionInFlight = false;

export function startViewTransition(mutate: () => void): void {
  if (!supportsViewTransition() || prefersReducedMotion() || transitionInFlight) {
    mutate();
    return;
  }
  transitionInFlight = true;
  const transition = document.startViewTransition(() => {
    mutate();
  });
  /**
   * 🔴 **三个 promise 都要接住，不只 `finished`。**
   *
   * 过渡被中止时（文档不可见、同一帧内又起了一场、旧内容截图失败…）这三个各自
   * reject，没有 handler 的那个就是一条 unhandled rejection。
   *
   * 实测代价：只接 `finished` 的那一版在浏览器预览里刷出
   * `InvalidStateError: Transition was aborted because of invalid state` 红字。
   * 这类中止是**预期情形**（用户飞快连点、切到后台），不是缺陷 —— 但红字会被
   * 错误上报当成缺陷，更糟的是它会淹没真正该看见的报错。
   */
  transition.ready.catch(() => {});
  transition.updateCallbackDone.catch(() => {});
  void transition.finished.catch(() => {}).finally(() => {
    transitionInFlight = false;
  });
}
