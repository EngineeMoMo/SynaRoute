import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { FloatingWidget } from "./components/FloatingWidget";
import "./styles.css";

/**
 * 悬浮窗与主窗口共用这一份前端，靠查询串分流（后端建窗时用的是
 * `index.html?floating=1`，见 `floating.rs`）。
 *
 * 用 `search` 而不是 `hash`：hash 变化不会让 WebView 重新求值初始 URL，
 * 而这里需要在**首次加载**时就决定渲染哪棵树。
 */
const isFloating = new URLSearchParams(window.location.search).get("floating") === "1";

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>{isFloating ? <FloatingWidget /> : <App />}</React.StrictMode>
);
