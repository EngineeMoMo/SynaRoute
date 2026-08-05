import type { ReactNode } from "react";
import { cn } from "@/lib/utils";
import { useReveal } from "@/hooks/useReveal";

/** 页面主容器：统一最大宽度与左右安全边距 */
export function Container({ className, children }: { className?: string; children: ReactNode }) {
  return <div className={cn("mx-auto w-full max-w-content px-5 sm:px-6 lg:px-8", className)}>{children}</div>;
}

/**
 * 首页区块外壳。
 *
 * `id` 用于顶栏锚点跳转；上下间距统一在这里给，避免各区块各写一套导致节奏不齐。
 * 刻意不给每个区块单独背景色（模板第 7.4 节），只在需要区分时由调用方传 className。
 */
export function Section({
  id,
  className,
  children,
}: {
  id?: string;
  className?: string;
  children: ReactNode;
}) {
  return (
    <section id={id} className={cn("scroll-mt-20 py-16 sm:py-20 lg:py-28", className)}>
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
      <h2 className="text-3xl font-bold tracking-tight text-text-primary sm:text-section-title">{title}</h2>
      {subtitle && <p className="mt-4 text-base leading-relaxed text-text-secondary">{subtitle}</p>}
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
