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
  { id: "features", labelKey: "nav.features", hash: "features" },
  { id: "screenshots", labelKey: "nav.screenshots", hash: "screenshots" },
  { id: "download", labelKey: "nav.download", path: "download" },
  { id: "docs", labelKey: "nav.docs", path: "docs" },
  { id: "changelog", labelKey: "nav.changelog", path: "changelog" },
];

/** 页脚分组链接 */
export const footerNav: { titleKey: string; items: NavItem[] }[] = [
  {
    titleKey: "footer.product",
    items: [
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
