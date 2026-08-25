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
  /**
   * 同平台的**其它包格式 / 架构**，与主下载并列展示。
   *
   * 为什么需要它：一次发布里 macOS 有 aarch64 与 x64 两个 dmg、Linux 有
   * AppImage / deb / rpm 三种。只给一个就必然有人拿错 —— 而拿错架构的 dmg
   * 在 Mac 上报的是「已损坏，无法打开」，与真正的损坏一模一样，用户诊断不出来。
   *
   * **刻意不靠 UA 判架构**：Safari 在 Apple 芯片上的 UA 依然自称
   * `Intel Mac OS X`，用 UA 判必错。并列给出、让用户自己选是唯一可靠的做法。
   */
  extraDownloads?: { labelSuffix: string; assetPattern: RegExp }[];
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
    status: "available",
    minOS: "macOS 11+（Apple 芯片 / Intel）",
    // ⚠️ **必须锚定架构**：一次发布里有两个 dmg（`_aarch64.dmg` 与 `_x64.dmg`），
    // 裸 `/\.dmg$/i` 会命中「资产列表里碰巧排在前面的那一个」——
    // Intel Mac 用户拿到 arm64 包会直接打不开，而报错是「已损坏，无法打开」，
    // 与真正的损坏毫无区别，用户几乎不可能自己诊断到「架构拿错了」。
    //
    // 这里默认给 Apple 芯片（现役 Mac 的绝大多数）；Intel 那份由下面的
    // `extraDownloads` 并列给出，不靠 UA 猜 —— Safari 的 UA 在 Apple 芯片上
    // 依然自称 `Intel Mac OS X`，UA 判架构必错。
    assetPattern: /_aarch64\.dmg$/i,
    uaPattern: /mac os x|macintosh/i,
    extraDownloads: [{ labelSuffix: "Intel", assetPattern: /_x64\.dmg$/i }],
  },
  {
    id: "linux",
    status: "available",
    minOS: "glibc 2.31+（Ubuntu 20.04+ / Debian 11+ 等）",
    // AppImage 免安装、发行版无关，作为默认；deb / rpm 并列给出。
    assetPattern: /_amd64\.AppImage$/i,
    uaPattern: /linux/i,
    extraDownloads: [
      { labelSuffix: "deb", assetPattern: /_amd64\.deb$/i },
      { labelSuffix: "rpm", assetPattern: /\.x86_64\.rpm$/i },
    ],
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
