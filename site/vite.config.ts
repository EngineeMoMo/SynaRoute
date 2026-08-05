import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import path from "node:path";

// 官网走自定义域名 synaroute.mofamilys.com，站点挂在域名根部，故 base 为 "/"。
// 若将来改回 GitHub Pages 默认域名（engineemomo.github.io/SynaRoute），
// 这里要同步改成 "/SynaRoute/"，并把 config/site.ts 的 url 一起改。
export default defineConfig({
  base: "/",
  plugins: [react()],
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
    },
  },
  build: {
    // 官网是公开静态站，产物走 CDN 缓存，用内容 hash 即可。
    // 刻意不抄应用侧 vite.config.ts 的 `-b${Date.now()}` 构建号方案 ——
    // 那是为了对抗 WebView2 按文件名命中旧缓存，网站不需要，反而会让每次构建
    // 全量失效 CDN 缓存。
    assetsInlineLimit: 4096,
  },
  server: {
    // 应用 dev server 固定占用 1420，官网另起端口避免两边同时开发时打架
    port: 1430,
    strictPort: true,
  },
});
