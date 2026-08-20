/**
 * 用系统默认浏览器打开一个 http(s) 链接。
 *
 * **必须走 shell 插件**，不能用 `window.open` 或 `<a target="_blank">`：WebView 里那些会
 * 试图在应用自己的窗口内导航（或被直接拦掉），用户会看到主界面被网页顶替、且回不去。
 *
 * 与「打开本地目录」刻意分成两条路（后者见 `lib/openLogDir.ts`，走后端命令）：
 * shell 插件对**来自 JS** 的 `open` 强制做 scope 正则校验，默认正则
 * `^((mailto:\w+)|(tel:\w+)|(https?://\w+)).+` 只放行 mailto / tel / http(s)。
 * 外链天然匹配得上，所以前端直接调没问题；而 Windows 本地路径（`C:\…`）匹配不上，
 * 必须由 Rust 端调（那边 scope 传 None、不校验）。
 * 把两者混成一个 helper 只会让「为什么这个能开那个不能开」重新变成谜题。
 *
 * 只接受 http/https：其余 scheme（file:、自定义协议）交给这里等于把
 * 「用默认程序打开任意东西」的能力暴露给页面逻辑，收益为零。
 */
export async function openExternalUrl(url: string): Promise<void> {
  const trimmed = url.trim();
  if (!/^https?:\/\//i.test(trimmed)) {
    throw new Error(`拒绝打开非 http(s) 链接：${trimmed || "(空)"}`);
  }
  const { open } = await import("@tauri-apps/plugin-shell");
  await open(trimmed);
}
