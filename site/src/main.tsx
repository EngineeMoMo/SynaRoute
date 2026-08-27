// **必须是第一个 import**：垫片要在任何依赖（marked 等）求值之前执行。
import "./lib/legacyPolyfills";
import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import "./styles.css";
import { siteConfig } from "@/config/site";

/**
 * 访问统计：只有同时配了脚本地址与站点 ID 才注入。
 * 未配置时**什么都不做、不报错**（需求模板第 17 节要求）。
 */
function mountAnalytics() {
  const { scriptUrl, websiteId } = siteConfig.analytics;
  if (!scriptUrl || !websiteId) return;
  const s = document.createElement("script");
  s.src = scriptUrl;
  s.defer = true;
  s.setAttribute("data-website-id", websiteId);
  document.head.appendChild(s);
}

mountAnalytics();

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>
);
