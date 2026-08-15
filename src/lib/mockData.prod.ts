/**
 * 生产构建时替换 `mockData.ts` 的空桩（由 vite.config.ts 的 alias 在 `build` 模式下切换）。
 *
 * ## 为什么需要它
 *
 * `bridge.ts` 顶层 `import { mockBridge } from "./mockData"`，而 mockData 里有整套演示
 * 数据（假 Key、假余额、假 cc-switch 库路径…）。顶层静态 import 无法被 tree-shake，
 * 于是那些演示数据会**原样打进生产 bundle**（实测 index chunk 里能 grep 到
 * `cc-switch.db` 这类字符串）。虽然 Tauri 环境下 `isTauri()` 恒为真、mock 分支永不执行，
 * 但把假数据发给用户仍然不对：既多占体积，也让「产物里为什么有演示 Key」这类疑问
 * 无从解释。
 *
 * ## 为什么用 alias 而不是改调用点
 *
 * `bridge.ts` 有 60+ 个 `call(cmd, args, () => mockBridge.x(...))` 调用点。把它们改成
 * 动态 import 需要动每一处签名，收益（省 15KB）远小于风险（改错一处就是生产环境
 * 某个功能静默走进 mock）。alias 方案零侵入：类型由同名导出保证，编译期就能发现漏项。
 *
 * ## 这里的每个方法都必须抛错而不是返回假值
 *
 * 生产环境走到这里只有一种可能：`isTauri()` 判据失效（Tauri 注入变了）。那时**必须
 * 立刻炸**，让用户看到明确报错去反馈；返回空数组会让界面显示「一条 Key 都没有」，
 * 用户以为配置丢了 —— 那是本项目最忌讳的静默失效。
 */

const NOT_AVAILABLE = "[SynaRoute] 运行环境检测失败：本应走 Tauri 后端，却落到了浏览器预览分支。请重启应用；若持续出现请反馈此错误。";

function unavailable(): never {
  throw new Error(NOT_AVAILABLE);
}

/**
 * 用 Proxy 兜住所有方法：无需逐个列出 60+ 个方法名（列表会与 mockData.ts 漂移，
 * 漏一个就是生产环境访问 undefined 报 `x is not a function`，比明确报错更难查）。
 */
export const mockBridge = new Proxy({} as Record<string, () => never>, {
  get() {
    return unavailable;
  },
});
