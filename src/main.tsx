import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import "./styles.css";

/**
 * 单一入口：这份前端只服务主窗口。
 *
 * 历史：这里曾按 `?floating=1` 分流出一棵 `FloatingWidget` 树（桌面悬浮球用的独立
 * Tauri 窗口共用同一份 index.html）。悬浮窗功能已于 2026-08-15 整体删除
 * —— 真机实测它的子 WebView 从未初始化、用户彻底看不见，而后端却打出「已显示」。
 * 详见 docs/14 第十八节。**不要**在这里重新加查询串分流：那条路已被证明不可靠，
 * 要做桌面浮层需要重新立项并先解决 WebView 初始化问题。
 */
ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>
);
