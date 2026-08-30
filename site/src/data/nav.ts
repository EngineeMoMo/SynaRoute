/**
 * 导航菜单。
 *
 * `hash` 项是首页内的锚点（平滑滚动），`path` 项是独立路由。
 * 在非首页时点击锚点项，会先回到首页再滚动 —— 由 Header 处理。
 */

export interface NavItem {
  id: string;
  labelKey: string;
  /** 首页内锚点 id（不带 #） */
  hash?: string;
  /** 相对语言前缀的路径，如 "download" → /zh/download */
  path?: string;
}

export const navItems: NavItem[] = [
  // 大脑聚合排在功能之前：它是唯一「别处没有」的能力，顶栏得能直接跳过去
  { id: "brain", labelKey: "nav.brain", hash: "brain" },
  { id: "features", labelKey: "nav.features", hash: "features" },
  { id: "screenshots", labelKey: "nav.screenshots", hash: "screenshots" },
  // ⚠️ 这里**刻意没有「下载」**：顶栏右侧已经有一个指向同一个 /download 的
  // 按钮，两者同 key、同目标、在同一条 64px 里相距约 580px —— 同一个词出现两次
  // 只会让人怀疑它们不是一回事。下载入口保留在右侧按钮与页脚，不进导航列表。
  { id: "docs", labelKey: "nav.docs", path: "docs" },
  { id: "changelog", labelKey: "nav.changelog", path: "changelog" },
];

/** 页脚分组链接 */
export const footerNav: { titleKey: string; items: NavItem[] }[] = [
  {
    titleKey: "footer.product",
    items: [
      { id: "brain", labelKey: "nav.brain", hash: "brain" },
      { id: "features", labelKey: "nav.features", hash: "features" },
      { id: "screenshots", labelKey: "nav.screenshots", hash: "screenshots" },
      { id: "download", labelKey: "nav.download", path: "download" },
    ],
  },
  {
    titleKey: "footer.resources",
    items: [
      { id: "docs", labelKey: "nav.docs", path: "docs" },
      { id: "changelog", labelKey: "nav.changelog", path: "changelog" },
    ],
  },
  {
    titleKey: "footer.legal",
    items: [
      { id: "privacy", labelKey: "footer.privacy", path: "privacy" },
      { id: "terms", labelKey: "footer.terms", path: "terms" },
    ],
  },
];
