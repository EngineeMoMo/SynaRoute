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

/**
 * 悬浮球要的是「圆」，而窗口永远是方的 —— 圆是靠**三层同时透明**换来的：
 * 窗口 `transparent(true)`（floating.rs）、body 透明（本行打的类）、
 * 组件外层不给背景（FloatingWidget）。任意一层不透明，屏幕上就是个白方块套着球。
 *
 * 在 render **之前**打这个类：晚一步就会在首帧闪一下方形白底。
 */
if (isFloating) {
  document.documentElement.classList.add("floating-mode");
}

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>{isFloating ? <FloatingWidget /> : <App />}</React.StrictMode>
);
