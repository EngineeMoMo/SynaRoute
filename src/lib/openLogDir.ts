import { api } from "@/lib/bridge";

/**
 * 打开日志目录（UX#13）。
 *
 * 抽成助手是为了让**设置页按钮**与**命令面板**走同一条实现 —— 两份拷贝会漂移，
 * 而漂移在这里是隐形的：某一处忘了先建目录，就只在「一条日志都没写过」的新装环境下
 * 报「找不到路径」，日常自测永远碰不到。
 *
 * ## 为什么整件事收进后端一条命令
 *
 * 旧实现是「前端拿路径 → 动态 import shell 插件 → `open(dir)`」，那条路
 * **在生产里 100% 失败**：shell 插件对**来自 JS** 的 `open` 强制做 scope 正则校验，
 * 未配置时用默认正则 `^((mailto:\w+)|(tel:\w+)|(https?://\w+)).+`，Windows 路径
 * `C:\Users\…\logs` 匹配不上 → 直接 `Error::Validation`。于是按钮一按就报错，
 * 而关于页那几个 `https://` 外链却正常（匹配得上），把真因掩盖成了「随机不好使」。
 *
 * 后端 `open_log_dir` 用 Rust 端的 `shell.open(path, None)`，scope 传 `None` 即
 * **不做正则校验**，且顺带把「建目录 + 打开」并成一次 IPC（两步分开时中间失败会让
 * 前端拿着路径去开一个还不存在的目录 —— 日志目录是懒创建的）。
 * 也不必为了放行本地路径去放宽那条防「JS 任意打开文件」的正则。
 *
 * 返回实际打开的绝对路径，调用方可用于 toast 回显（MSIX 虚拟化下用户需要核对
 * 打开的是真实目录还是包内私有副本 —— CLAUDE.md 平行宇宙惨案的复发防线）。
 * 失败一律抛出，由调用方决定怎么提示（两处都是弹 error toast）。
 */
export async function openLogDir(): Promise<string> {
  return api.openLogDir();
}
