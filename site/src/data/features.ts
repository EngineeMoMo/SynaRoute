import type { LucideIcon } from "lucide-react";
import {
  Shuffle,
  Layers,
  ShieldCheck,
  ArrowLeftRight,
  Split,
  Replace,
  KeyRound,
  PlugZap,
  ScrollText,
  PackageOpen,
  PanelBottom,
  Stethoscope,
  ShieldOff,
  TrendingUp,
  Wallet,
  Network,
  SlidersHorizontal,
} from "lucide-react";

/**
 * 首页的优势与功能数据。
 *
 * 只存 i18n key，不存文案 —— 保证中英两版结构完全一致，也避免文案散落在组件里。
 */

export interface Benefit {
  id: string;
  icon: LucideIcon;
  /** 取词前缀，实际 key 为 `${i18nPrefix}.name` / `.desc` / `.more` */
  i18nPrefix: string;
}

export const benefits: Benefit[] = [
  { id: "failover", icon: Shuffle, i18nPrefix: "benefits.failover" },
  { id: "threeClients", icon: Layers, i18nPrefix: "benefits.threeClients" },
  { id: "local", icon: ShieldCheck, i18nPrefix: "benefits.local" },
  { id: "protocol", icon: ArrowLeftRight, i18nPrefix: "benefits.protocol" },
];

export interface Feature {
  id: string;
  icon: LucideIcon;
  /** 取词前缀，实际 key 为 `${i18nPrefix}.name` / `.short` / `.desc` */
  i18nPrefix: string;
  /**
   * 在 Bento 网格里占据的宽度。
   *
   * 🔴 **`third` 的数量必须是 6 的倍数**，`half` 必须是偶数。
   *
   * 为什么是 6 而不是 3：small 网格是 `sm:grid-cols-2 lg:grid-cols-3`，
   * 两档都要排满，所以数量得同时被 2 和 3 整除。
   *
   * 这条约束原先写的是「六个 third」并只说了三列那一档 —— 0.1.30~0.1.33 加了
   * 四条 third（变成 10 条）之后它就破了，而**注释不是判据**，没有任何东西报错：
   * 桌面端最后一行只剩「余额查询」一张孤卡、右侧空掉约 770×255px，
   * 看起来像是漏排了。本轮补到 12 条并把约束写准。
   *
   * ⚠️ 只在 `lg` 那一档才看得出来（<640px 单列、640~1023px 两列都排得满），
   * 所以自测时窗口不拉到 1024 以上是发现不了的。
   *
   * 大脑聚合不在这个列表里 —— 它有独立的 `BrainSpotlight` 区块，别再往这儿加一份。
   */
  span: "half" | "third";
}

export const features: Feature[] = [
  { id: "failover", icon: Split, i18nPrefix: "features.failover", span: "half" },
  { id: "protocol", icon: ArrowLeftRight, i18nPrefix: "features.protocol", span: "half" },
  { id: "mapping", icon: Replace, i18nPrefix: "features.mapping", span: "third" },
  { id: "secret", icon: KeyRound, i18nPrefix: "features.secret", span: "third" },
  { id: "apply", icon: PlugZap, i18nPrefix: "features.apply", span: "third" },
  { id: "logs", icon: ScrollText, i18nPrefix: "features.logs", span: "third" },
  { id: "portable", icon: PackageOpen, i18nPrefix: "features.portable", span: "third" },
  { id: "tray", icon: PanelBottom, i18nPrefix: "features.tray", span: "third" },
  // 0.1.30~0.1.33 新增的四条。放在末尾而不是插到前面：前几条回答「为什么要装它」，
  // 这四条回答「装了之后还有这些」——顺序本身就是一层信息。
  { id: "diag", icon: Stethoscope, i18nPrefix: "features.diag", span: "third" },
  { id: "resilience", icon: ShieldOff, i18nPrefix: "features.resilience", span: "third" },
  { id: "usage", icon: TrendingUp, i18nPrefix: "features.usage", span: "third" },
  { id: "balance", icon: Wallet, i18nPrefix: "features.balance", span: "third" },
  // 0.1.41~0.1.42 的两条，官网此前一个字都没提。补上它们同时把 third 凑回 12
  //（6 的倍数，见上面 span 的说明）。
  { id: "lan", icon: Network, i18nPrefix: "features.lan", span: "third" },
  { id: "codexModels", icon: SlidersHorizontal, i18nPrefix: "features.codexModels", span: "third" },
];

export interface Step {
  id: string;
  i18nPrefix: string;
}

export const steps: Step[] = [
  { id: "s1", i18nPrefix: "steps.s1" },
  { id: "s2", i18nPrefix: "steps.s2" },
  { id: "s3", i18nPrefix: "steps.s3" },
  { id: "s4", i18nPrefix: "steps.s4" },
];
