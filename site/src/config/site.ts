/**
 * 站点级单一事实来源。
 *
 * 模板第 11 节要求「后续修改产品内容时，不需要修改页面结构组件」——
 * 所有产品名、链接、邮箱、版本兜底值都只在这里出现一次。
 */

const GITHUB_OWNER = "EngineeMoMo";
const GITHUB_REPO = "SynaRoute";

/**
 * 桌面应用的当前版本号，构建期由 vite 注入（见 site/vite.config.ts 的 `define`）。
 *
 * 事实来源是**应用自己的** `package.json`（仓库根目录那份），不是官网的
 * —— 官网的 version 恒为 `0.0.0`（它不发版）。
 */
const APP_VERSION: string = __APP_VERSION__;

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
   *
   * **从应用自己的 package.json 读**，不手写字面量。原来是手写的 `"v0.1.9"`，
   * 而实际版本已经到 0.1.33 —— 落后 24 个版本。GitHub 匿名 API 是 60 次/小时/IP，
   * 撞上限流时首屏、底部 CTA、下载卡三处会一起显示那个陈旧版本号，
   * 对一个主打「一直在修」的工具，访客看到的是一个半年前的版本。
   *
   * 「发版时顺手更新」这条纪律 24 次全没兑现 —— 那就说明它不该靠纪律。
   * 走构建期注入之后，官网版本号与应用版本号**结构上不可能分叉**。
   * （下载链接本来就不会坏：拿不到资产时回落 releases/latest 页面。坏的只是数字。）
   */
  fallbackVersion: `v${APP_VERSION}`,

  author: {
    name: GITHUB_OWNER,
    /** 作者主页统一用 GitHub 个人页 —— 这里能直接看到项目本身，比另开一个站更有用 */
    url: `https://github.com/${GITHUB_OWNER}`,
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
