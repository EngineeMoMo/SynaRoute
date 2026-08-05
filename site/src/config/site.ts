/**
 * 站点级单一事实来源。
 *
 * 模板第 11 节要求「后续修改产品内容时，不需要修改页面结构组件」——
 * 所有产品名、链接、邮箱、版本兜底值都只在这里出现一次。
 */

const GITHUB_OWNER = "EngineeMoMo";
const GITHUB_REPO = "SynaRoute";

export const siteConfig = {
  name: "SynaRoute",

  /** 站点自身地址。换域名时改这里 + vite.config.ts 的 base + public/CNAME 三处。 */
  url: "https://synaroute.mofamilys.com",

  github: {
    owner: GITHUB_OWNER,
    repo: GITHUB_REPO,
    url: `https://github.com/${GITHUB_OWNER}/${GITHUB_REPO}`,
    releases: `https://github.com/${GITHUB_OWNER}/${GITHUB_REPO}/releases`,
    latestRelease: `https://github.com/${GITHUB_OWNER}/${GITHUB_REPO}/releases/latest`,
    issues: `https://github.com/${GITHUB_OWNER}/${GITHUB_REPO}/issues`,
    /** 匿名可调用，限流 60 次/小时/IP，故调用处必须有兜底。 */
    apiLatestRelease: `https://api.github.com/repos/${GITHUB_OWNER}/${GITHUB_REPO}/releases/latest`,
    apiReleases: `https://api.github.com/repos/${GITHUB_OWNER}/${GITHUB_REPO}/releases?per_page=20`,
  },

  /**
   * GitHub API 不可用时（限流 / 断网 / 接口变更）下载区显示的版本号。
   * 发新版后可顺手更新，但**不更新也不会导致坏链** —— 兜底链接指向 releases/latest 页面。
   */
  fallbackVersion: "v0.1.9",

  author: {
    name: "EngineeMoMo",
    url: `https://github.com/${GITHUB_OWNER}`,
    site: "https://www.mofamilys.com",
    email: "mhm292117@163.com",
  },

  /** Open Graph 分享图，放在 public/ 下。 */
  ogImage: "/og.png",

  /**
   * 访问统计：默认关闭。
   *
   * `scriptUrl` 与 `websiteId` 同时非空才注入脚本；任一为空则完全不加载，
   * 也不报错（模板第 17 节要求「未配置统计 ID 时不得报错」）。
   */
  analytics: {
    scriptUrl: "",
    websiteId: "",
  },
} as const;

export type SiteConfig = typeof siteConfig;
