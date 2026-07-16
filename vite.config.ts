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
