/**
 * 老浏览器 API 垫片。**必须在任何应用代码之前执行**（`main.tsx` 里第一个 import）。
 *
 * # 为什么需要它：一次实测复现的整站白屏
 *
 * `marked`（Markdown 渲染器，文档页 / 更新日志 / 隐私 / 用户协议四类页面都用它）
 * 内部用了 15 处 `Array.prototype.at`。而：
 *
 * - `Array.prototype.at` 是 **Safari / iOS 15.4+** 才有的；
 * - Vite 的 build target 只控制**语法**降级，esbuild **从不补 API**
 *   —— 把 target 调到 safari14 也不会给你一个 `at` 出来。
 *
 * 于是 iOS 15.0~15.3 的用户打开任意文档页 → `TypeError: n.at is not a function`
 * → React 18 对未捕获的渲染异常会**卸载整个 root**。实测（注入
 * `delete Array.prototype.at` 复现）：`#root` 的 childElementCount 变 0、
 * 连顶栏和页脚一起消失，而且**此后连首页也回不来**（root 已经空了，
 * 客户端路由切过去也没有东西再挂上）。用户看到的是一整片纯白。
 *
 * # 为什么用垫片而不是换掉 marked / 预渲染
 *
 * 垫片是三行、零依赖、零构建配置改动，而且顺手覆盖 marked 未来用到的其它 `at` 调用。
 * 换渲染器或改成构建期预渲染 HTML 都是更大的改动，收益只是省掉这三行。
 *
 * # 与 ErrorBoundary 的分工
 *
 * 这里修的是**这一个已知病因**；`ErrorBoundary` 挡的是**任何未来的渲染异常**
 * （让它只毁掉一个区块而不是整站）。两层都要，缺一不可：
 * 只有垫片 → 下一个 API 缺口照样白屏；只有 ErrorBoundary → 用户看到的是错误页而不是文档。
 *
 * # 判据
 *
 * `scripts/check-legacy-api.mjs` 扫构建产物里的 `.at(` / `.findLast(` / `structuredClone`
 * 之类，出现了但这里没垫就报错 —— 靠人记「加依赖时想想 Safari 15」是记不住的。
 */

/*
 * 实现上的两处细节：
 *
 * - `lib` 是 ES2020（见 tsconfig.json），故 TS 的类型里**压根没有** `at`。
 *   这不是要改 lib 的理由 —— 恰恰相反，lib 停在 ES2020 是一道有用的防线：
 *   它会让「源码里直接写 `.at()`」变成编译错误（依赖里的用法它管不到，那正是本文件存在的原因）。
 *   所以这里用 `as unknown as Record<string, unknown>` 绕过类型，而不是抬高 lib。
 * - 用 `Object.defineProperty` 而不是直接赋值：直接赋值会让它变成可枚举属性，
 *   `for...in` 遍历数组时会多出一个 `at`（老代码里这种遍历不算罕见）。
 */
type Patchable = Record<string, unknown>;

const arrayProto = Array.prototype as unknown as Patchable;
// Array.prototype.at —— Safari/iOS 15.4+，Chrome 92+
if (typeof arrayProto.at !== "function") {
  Object.defineProperty(arrayProto, "at", {
    configurable: true,
    writable: true,
    value: function at(this: unknown[], index: number) {
      const len = this.length;
      // 规范是 ToIntegerOrInfinity：`at(1.7)` 取 1、`at(NaN)` 取 0、`at(-1)` 取末位
      let i = Math.trunc(index) || 0;
      if (i < 0) i += len;
      return i < 0 || i >= len ? undefined : this[i];
    },
  });
}

const stringProto = String.prototype as unknown as Patchable;
// String.prototype.at —— 同一批（Safari/iOS 15.4+）。marked 目前只用数组版，
// 但两者总是成对缺失，垫上它的成本是几行、漏掉它的成本又是一次整站白屏。
if (typeof stringProto.at !== "function") {
  Object.defineProperty(stringProto, "at", {
    configurable: true,
    writable: true,
    value: function at(this: string, index: number) {
      const len = this.length;
      let i = Math.trunc(index) || 0;
      if (i < 0) i += len;
      return i < 0 || i >= len ? undefined : this[i];
    },
  });
}
