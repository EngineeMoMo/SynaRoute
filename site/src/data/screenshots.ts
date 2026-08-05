/**
 * 产品截图。
 *
 * 图片取自软件的浏览器预览模式（`launch.json` 的 `synaroute-web`），
 * 界面真实、数据是 mock 里的示例数据（relay.example.com 之类），
 * **不含任何真实密钥或真实厂商地址**。采集步骤见 site/scripts/capture-shots.md。
 *
 * 每张图存明暗两版，随站点主题切换 —— 深色站点配浅色截图会很突兀。
 */

export interface Screenshot {
  id: string;
  /** 取词前缀：`${i18nPrefix}.title` / `.desc`，alt 文本由 title + desc 合成 */
  i18nPrefix: string;
  /** 浅色主题下的图，相对 public/ */
  light: string;
  /** 深色主题下的图 */
  dark: string;
  /** 原始像素尺寸，写进 <img width/height> 以避免加载时的布局偏移 */
  width: number;
  height: number;
}

/** 截图统一采集分辨率，改这里就要重拍全部 */
export const SHOT_WIDTH = 1440;
export const SHOT_HEIGHT = 900;

export const screenshots: Screenshot[] = [
  {
    id: "category",
    i18nPrefix: "screenshots.category",
    light: "/screenshots/category-light.png",
    dark: "/screenshots/category-dark.png",
    width: SHOT_WIDTH,
    height: SHOT_HEIGHT,
  },
  {
    id: "brain",
    i18nPrefix: "screenshots.brain",
    light: "/screenshots/brain-light.png",
    dark: "/screenshots/brain-dark.png",
    width: SHOT_WIDTH,
    height: SHOT_HEIGHT,
  },
  {
    id: "logs",
    i18nPrefix: "screenshots.logs",
    light: "/screenshots/logs-light.png",
    dark: "/screenshots/logs-dark.png",
    width: SHOT_WIDTH,
    height: SHOT_HEIGHT,
  },
  {
    id: "settings",
    i18nPrefix: "screenshots.settings",
    light: "/screenshots/settings-light.png",
    dark: "/screenshots/settings-dark.png",
    width: SHOT_WIDTH,
    height: SHOT_HEIGHT,
  },
  {
    id: "vendors",
    i18nPrefix: "screenshots.vendors",
    light: "/screenshots/vendors-light.png",
    dark: "/screenshots/vendors-dark.png",
    width: SHOT_WIDTH,
    height: SHOT_HEIGHT,
  },
];

/** Hero 区的主图，单独取用（就是 Key 管理那张） */
export const heroScreenshot = screenshots[0];
