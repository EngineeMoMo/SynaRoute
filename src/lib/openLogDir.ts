import { api } from "@/lib/bridge";

/**
 * 打开日志目录（UX#13）。
 *
 * 抽成助手是为了让**设置页按钮**与**命令面板**走同一条实现 —— 两份拷贝会漂移，
 * 而漂移在这里是隐形的：某一处忘了先建目录，就只在「一条日志都没写过」的新装环境下
 * 报「找不到路径」，日常自测永远碰不到。
 *
 * 两步的判据：
 * 1. 后端 `prepare_log_dir` 先确保目录存在。日志目录是**懒创建**的，一条日志都没写过时
 *    它并不存在，直接把路径交给资源管理器会报「找不到路径」。
 * 2. 前端走 shell 插件打开，与关于页开外链同一做法（WebView 里 `window.open` 不可靠）。
 *
 * 失败一律抛出，由调用方决定怎么提示（两处都是弹 error toast）。
 * 浏览器预览模式下 `import("@tauri-apps/plugin-shell")` 会抛错，属预期行为、不是回归。
 */
export async function openLogDir(): Promise<void> {
  const dir = await api.prepareLogDir();
  const { open } = await import("@tauri-apps/plugin-shell");
  await open(dir);
}
