/**
 * 支持平台矩阵。
 *
 * ⚠️ 这是「macOS 版做出来之后只改数据、不改组件」的关键：Hero 的平台图标、
 * 首页下载区、下载页、页脚四处全部读这份数据渲染。将来 Mac 端发布，
 * 只需把 `status` 改成 `"available"`，四处同时生效。
 *
 * `status: "coming-soon"` 的平台**不会渲染出任何 href** —— 模板第 6.6 节
 * 明确要求「下载地址未填写时，按钮显示『即将推出』，不得链接到无效地址」。
 */

export type PlatformId = "windows" | "macos" | "linux";
export type PlatformStatus = "available" | "coming-soon";

export interface Platform {
  id: PlatformId;
  /** i18n key 后缀，取词为 `platform.<id>.name` 等 */
  status: PlatformStatus;
  /**
   * 最低系统版本，直接展示。
   * 刻意不走 i18n：「Windows 10 (1809) / Windows 11」两种语言下写法完全一致，
   * 进字典只会多一处要同步的地方。
   */
  minOS: string;
  /**
   * 从 GitHub Release 资产列表里挑出本平台安装包的匹配规则。
   * 拿不到（API 失败 / 该版本没有此资产）时回落到 releases/latest 页面。
   */
  assetPattern: RegExp;
  /** `navigator.userAgent` 命中该正则即认为访客在用此平台 */
  uaPattern: RegExp;
}

export const platforms: Platform[] = [
  {
    id: "windows",
    status: "available",
    // Tauri 2 的 WebView2 依赖决定了下限：Win10 1809 起系统自带 WebView2 Runtime
    minOS: "Windows 10 (1809) / Windows 11",
    assetPattern: /_x64-setup\.exe$/i,
    uaPattern: /windows/i,
  },
  {
    id: "macos",
    // 尚未构建 macOS 版本。做出来之后改这一行即可，无需改任何组件。
    status: "coming-soon",
    minOS: "macOS 12+",
    assetPattern: /\.dmg$/i,
    uaPattern: /mac os x|macintosh/i,
  },
];

/** 当前是否已有任一平台可下载（全都 coming-soon 时下载区要换一套说法） */
export const hasAnyDownload = platforms.some((p) => p.status === "available");

/**
 * 猜访客所在平台，用于「推荐下载」。
 *
 * 猜错的代价必须为零：调用方只拿它决定**高亮哪一个**，所有平台的手动入口
 * 始终并列可见（模板第 16 节要求）。SSR/无 navigator 环境返回 null。
 */
export function detectPlatform(ua = typeof navigator === "undefined" ? "" : navigator.userAgent): PlatformId | null {
  for (const p of platforms) {
    if (p.uaPattern.test(ua)) return p.id;
  }
  return null;
}
