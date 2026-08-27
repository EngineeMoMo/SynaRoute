/// <reference types="vite/client" />

/**
 * 桌面应用的版本号，由 `vite.config.ts` 的 `define` 在构建期注入。
 *
 * 声明在这里而不是 `any`：类型缺失时 `siteConfig.fallbackVersion` 会静默变成
 * `v[object Object]` 之类 —— 而它出现在首屏，出错了却不报错。
 */
declare const __APP_VERSION__: string;
