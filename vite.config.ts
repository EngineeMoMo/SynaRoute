import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import path from "node:path";

// Tauri 开发时前端固定跑在 1420 端口，供 Rust 侧 devUrl 引用
const host = process.env.TAURI_DEV_HOST;

export default defineConfig({
  plugins: [react()],
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
    },
  },
  // 防止 vite 屏蔽 Rust 侧错误
  clearScreen: false,
  build: {
    rollupOptions: {
      output: {
        // 每次构建注入唯一构建号,使产物 chunk 文件名每次都不同——从物理上杜绝
        // WebView2/浏览器按文件名命中旧缓存导致「重开加载旧界面(Key 数陈旧)」。
        // 浏览器缓存按 URL 命中,文件名每次唯一则用户机器缓存里绝无此名,必加载最新前端。
        // 配合 tauri.conf.json 的 incognito + --disable-http-cache 为三重保险。
        entryFileNames: `assets/[name]-[hash]-b${Date.now()}.js`,
        chunkFileNames: `assets/[name]-[hash]-b${Date.now()}.js`,
        assetFileNames: `assets/[name]-[hash]-b${Date.now()}.[ext]`,
      },
    },
  },
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      // Tauri 后端目录交给 cargo 监听，vite 忽略
      ignored: ["**/src-tauri/**"],
    },
  },
});
