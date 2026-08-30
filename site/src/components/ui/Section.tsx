import type { ReactNode } from "react";
import { cn } from "@/lib/utils";
import { useReveal } from "@/hooks/useReveal";

/** 页面主容器：统一最大宽度与左右安全边距 */
export function Container({ className, children }: { className?: string; children: ReactNode }) {
  return <div className={cn("mx-auto w-full max-w-content px-5 sm:px-6 lg:px-8", className)}>{children}</div>;
}

/**
 * 首页区块的上下留白档位。
 *
 * 🔴 为什么需要三档：原先 `Section` 只有一组 `py-16 sm:py-20 lg:py-28`，于是
 * **8 个区块交界处的空白全是 224px**（112 + 112，实测）。留白本来是用来表达
 * 「这里换话题了」的，全站一个值等于这个维度压根没被使用 —— 再叠上网格里的
 * 孤卡与卡片底部空洞，那些空白就从「呼吸」被读成「没做完」。
 *
 * 三档只按一个判据选：**下一段是不是换了话题**。
 * - `loose`（112）：真正的话题边界。目前只有 BrainSpotlight 用 —— 它同时是
 *   全页唯一换底色的区块，两个信号叠在一起。
 * - `normal`（96）：默认。
 * - `tight`（80）：与上一段是同一件事的延续（「界面一览」紧接功能、
 *   「四步开始使用」紧接下载）。
 *
 * 加区块时请显式想一遍该给哪一档，别一律留默认 —— 那会让这三档退化回一档。
 */
const RHYTHM = {
  tight: "py-10 sm:py-14 lg:py-20",
  normal: "py-12 sm:py-16 lg:py-24",
  loose: "py-16 sm:py-20 lg:py-28",
} as const;

export type Rhythm = keyof typeof RHYTHM;

/**
 * 首页区块外壳。
 *
 * `id` 用于顶栏锚点跳转；上下间距由 `rhythm` 给（见上面 RHYTHM 的说明），
 * 避免各区块各写一套导致节奏不齐。
 * 刻意不给每个区块单独背景色（模板第 7.4 节），只在需要区分时由调用方传 className。
 */
export function Section({
  id,
  className,
  rhythm = "normal",
  children,
}: {
  id?: string;
  className?: string;
  rhythm?: Rhythm;
  children: ReactNode;
}) {
  return (
    <section id={id} className={cn("scroll-mt-20", RHYTHM[rhythm], className)}>
      <Container>{children}</Container>
    </section>
  );
}

/** 区块标题 + 副标题，居中排布 */
export function SectionTitle({
  title,
  subtitle,
  className,
}: {
  title: string;
  subtitle?: string;
  className?: string;
}) {
  const ref = useReveal<HTMLDivElement>();
  return (
    <div ref={ref} className={cn("reveal mx-auto max-w-2xl text-center", className)}>
      {/* text-balance：中文标题不加它会在桌面端把最后两三个字挤成孤零零一行
          （Hero 一直有，其余区块此前都没有）。 */}
      <h2 className="text-balance text-3xl font-bold tracking-tight text-text-primary sm:text-section-title">
        {title}
      </h2>
      {subtitle && (
        <p className="mt-4 text-balance text-base leading-relaxed text-text-secondary">{subtitle}</p>
      )}
    </div>
  );
}

/** 包一层就有进入视口淡入效果；`delay` 用于同组元素的错峰 */
export function Reveal({
  delay = 0,
  className,
  children,
}: {
  delay?: number;
  className?: string;
  children: ReactNode;
}) {
  const ref = useReveal<HTMLDivElement>(delay);
  return (
    <div ref={ref} className={cn("reveal", className)}>
      {children}
    </div>
  );
}
