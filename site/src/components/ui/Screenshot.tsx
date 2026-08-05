import { useState } from "react";
import type { ImgHTMLAttributes } from "react";
import { ImageOff } from "lucide-react";
import { cn } from "@/lib/utils";
import { useT } from "@/hooks/useLang";

/**
 * 首屏主图的加载优先级提示。
 *
 * 必须写**全小写**的 `fetchpriority`：React 18 既不认识 camelCase 的 `fetchPriority`
 * 也不会转换它（那是 React 19 才加的支持），传进去只会在控制台留一条
 * 「React does not recognize the prop」并把属性丢掉。全小写则被当作普通 DOM 属性
 * 原样透传，浏览器照常识别。
 */
const HIGH_PRIORITY = { fetchpriority: "high" } as ImgHTMLAttributes<HTMLImageElement>;

/**
 * 产品截图，带缺图占位。
 *
 * 为什么需要它：截图是构建后才放进 public/ 的资源，缺了或路径写错时
 * `<img>` 会渲染成一个破图图标 —— 官网上出现破图比出现「占位」难看得多，
 * 也让人怀疑软件本身。这里在 onError 时切成一个明确标注的占位块。
 *
 * 需求模板第 15 节的要求也是这个口径：没有图片时用清晰的占位组件，
 * 且占位必须**标明是占位**，不能伪造产品界面。
 */
export function Screenshot({
  src,
  alt,
  width,
  height,
  className,
  eager = false,
}: {
  src: string;
  alt: string;
  width: number;
  height: number;
  className?: string;
  /** 首屏主图用 true：不懒加载并提高优先级，否则最大内容绘制会明显变慢 */
  eager?: boolean;
}) {
  const t = useT();
  const [failed, setFailed] = useState(false);

  if (failed) {
    return (
      <div
        role="img"
        aria-label={alt}
        // 用宽高比占位而不是固定高度，保证与真图一致、切换时不产生布局跳动
        style={{ aspectRatio: `${width} / ${height}` }}
        className={cn(
          "flex w-full flex-col items-center justify-center gap-2 bg-surface-hover text-text-muted",
          className
        )}
      >
        <ImageOff size={26} aria-hidden="true" />
        <span className="px-4 text-center text-xs">{t("screenshots.placeholder")}</span>
      </div>
    );
  }

  return (
    <img
      src={src}
      alt={alt}
      width={width}
      height={height}
      loading={eager ? "eager" : "lazy"}
      {...(eager ? HIGH_PRIORITY : {})}
      decoding="async"
      onError={() => setFailed(true)}
      className={cn("block h-auto w-full", className)}
    />
  );
}
