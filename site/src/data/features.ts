import type { LucideIcon } from "lucide-react";
import {
  Shuffle,
  Layers,
  ShieldCheck,
  ArrowLeftRight,
  Split,
  Replace,
  Brain,
  KeyRound,
  PlugZap,
  ScrollText,
  PackageOpen,
  PanelBottom,
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
   * 头两个功能是最核心的卖点，给整行/半行的大格子；其余走三列小格子，
   * 避免「八个一模一样的卡片」那种没有信息层级的排布。
   */
  span: "wide" | "half" | "third";
}

export const features: Feature[] = [
  { id: "failover", icon: Split, i18nPrefix: "features.failover", span: "half" },
  { id: "brain", icon: Brain, i18nPrefix: "features.brain", span: "half" },
  { id: "mapping", icon: Replace, i18nPrefix: "features.mapping", span: "third" },
  { id: "protocol", icon: ArrowLeftRight, i18nPrefix: "features.protocol", span: "third" },
  { id: "secret", icon: KeyRound, i18nPrefix: "features.secret", span: "third" },
  { id: "apply", icon: PlugZap, i18nPrefix: "features.apply", span: "third" },
  { id: "logs", icon: ScrollText, i18nPrefix: "features.logs", span: "third" },
  { id: "portable", icon: PackageOpen, i18nPrefix: "features.portable", span: "third" },
  { id: "tray", icon: PanelBottom, i18nPrefix: "features.tray", span: "third" },
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
