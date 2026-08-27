import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import path from "node:path";
import { readFileSync } from "node:fs";

/**
 * 桌面应用的版本号，从**仓库根目录**的 package.json 读（官网自己那份恒为 0.0.0）。
 *
 * 注入而不是在官网里手写：原先 `siteConfig.fallbackVersion` 是手写的 `"v0.1.9"`，
 * 而应用已经到 0.1.33 —— 落后 24 个版本，「发版时顺手改」这条纪律 24 次全没兑现。
 * GitHub API 一限流，访客就会在首屏看到那个半年前的版本号。
 */
const APP_VERSION: string = JSON.parse(
  readFileSync(path.resolve(__dirname, "../package.json"), "utf8"),
).version;

// 官网走自定义域名 synaroute.mofamilys.com，站点挂在域名根部，故 base 为 "/"。
// 若将来改回 GitHub Pages 默认域名（engineemomo.github.io/SynaRoute），
// 这里要同步改成 "/SynaRoute/"，并把 config/site.ts 的 url 一起改。
export default defineConfig({
  base: "/",
  define: {
    // 官网侧的版本号兜底值。见上面 APP_VERSION 的注释：结构上与应用版本号不可能分叉。
    __APP_VERSION__: JSON.stringify(APP_VERSION),
  },
  plugins: [react()],
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
    },
  },
  build: {
    /**
     * 显式支持下限，而不是靠 Vite 的默认值（`'modules'` = 含 safari14）。
     *
     * 为什么要写出来：默认值随 Vite 版本变，而「支持到哪一档」是产品决定，不该由
     * 依赖升级悄悄改掉。写成 safari15 是因为 iOS 15 仍有可观占比（老 iPad 停在那）。
     *
     * ⚠️ **target 只降语法，esbuild 从不补 API**。`Array.prototype.at` 这类缺口
     * 必须靠 `src/lib/legacyPolyfills.ts` 显式垫 —— 把 target 调低不会给你一个 `at`。
     * 这一条踩过：iOS 15.0~15.3 上 marked 的 `.at()` 让文档页整站白屏。
     */
    target: ["es2020", "safari15", "chrome87", "firefox78", "edge88"],
    // 官网是公开静态站，产物走 CDN 缓存，用内容 hash 即可。
    // 刻意不抄应用侧 vite.config.ts 的 `-b${Date.now()}` 构建号方案 ——
    // 那是为了对抗 WebView2 按文件名命中旧缓存，网站不需要，反而会让每次构建
    // 全量失效 CDN 缓存。
    assetsInlineLimit: 4096,
    rollupOptions: {
      output: {
        /**
         * 把 markdown 渲染器（marked + dompurify）拆出首屏关键路径。
         *
         * 它只有文档页 / 更新日志 / 条款页用得到，而首页压根不需要 ——
         * 打进同一个 chunk 等于让每个只看首页的移动端访客白下载一份 markdown 引擎。
         */
        manualChunks(id) {
          if (id.includes("node_modules/marked") || id.includes("node_modules/dompurify")) {
            return "markdown";
          }
          return undefined;
        },
      },
    },
  },
  server: {
    // 应用 dev server 固定占用 1420，官网另起端口避免两边同时开发时打架
    port: 1430,
    strictPort: true,
  },
});
