import { useCallback, useEffect, useRef } from "react";
import { X, ChevronLeft, ChevronRight } from "lucide-react";
import { useT } from "@/hooks/useLang";

export interface LightboxImage {
  src: string;
  alt: string;
  width: number;
  height: number;
  caption?: string;
}

/**
 * 截图放大查看。
 *
 * 三个刻意的交互细节：
 * - 关闭遮罩用 `onMouseDown` 且判断 `target === currentTarget`，不用 `onClick`。
 *   否则在图上按下鼠标、拖到遮罩上松手会误触关闭（桌面应用里踩过同样的坑）。
 * - 打开时锁定 body 滚动，关闭后把焦点还给触发它的那个元素，键盘用户不会丢失位置。
 * - 左右方向键切图、Esc 关闭。
 */
export function ImageLightbox({
  images,
  index,
  onClose,
  onIndexChange,
}: {
  images: LightboxImage[];
  index: number | null;
  onClose: () => void;
  onIndexChange: (next: number) => void;
}) {
  const t = useT();
  const closeRef = useRef<HTMLButtonElement>(null);
  const restoreFocusRef = useRef<Element | null>(null);
  const open = index !== null;

  const go = useCallback(
    (delta: number) => {
      if (index === null || images.length === 0) return;
      onIndexChange((index + delta + images.length) % images.length);
    },
    [index, images.length, onIndexChange]
  );

  useEffect(() => {
    if (!open) return;

    restoreFocusRef.current = document.activeElement;
    closeRef.current?.focus();

    // 锁滚动：补上滚动条宽度，避免弹窗打开瞬间整页横向跳一下
    const scrollbar = window.innerWidth - document.documentElement.clientWidth;
    const prevOverflow = document.body.style.overflow;
    const prevPadding = document.body.style.paddingRight;
    document.body.style.overflow = "hidden";
    if (scrollbar > 0) document.body.style.paddingRight = `${scrollbar}px`;

    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
      else if (e.key === "ArrowLeft") go(-1);
      else if (e.key === "ArrowRight") go(1);
    };
    document.addEventListener("keydown", onKey);

    return () => {
      document.removeEventListener("keydown", onKey);
      document.body.style.overflow = prevOverflow;
      document.body.style.paddingRight = prevPadding;
      (restoreFocusRef.current as HTMLElement | null)?.focus?.();
    };
  }, [open, onClose, go]);

  if (index === null) return null;
  const image = images[index];
  if (!image) return null;

  return (
    <div
      role="dialog"
      aria-modal="true"
      aria-label={image.alt}
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
      className="fixed inset-0 z-[100] flex animate-fade-in items-center justify-center bg-black/80 p-4 backdrop-blur-sm sm:p-8"
    >
      <button
        ref={closeRef}
        type="button"
        onClick={onClose}
        aria-label={t("common.close")}
        className="absolute right-3 top-3 inline-flex h-11 w-11 items-center justify-center rounded-control text-white/80 transition-colors hover:bg-white/10 hover:text-white sm:right-5 sm:top-5"
      >
        <X size={22} aria-hidden="true" />
      </button>

      {images.length > 1 && (
        <>
          <button
            type="button"
            onClick={() => go(-1)}
            aria-label={t("screenshots.prev")}
            className="absolute left-2 top-1/2 inline-flex h-11 w-11 -translate-y-1/2 items-center justify-center rounded-full bg-black/40 text-white/80 transition-colors hover:bg-black/70 hover:text-white sm:left-5"
          >
            <ChevronLeft size={24} aria-hidden="true" />
          </button>
          <button
            type="button"
            onClick={() => go(1)}
            aria-label={t("screenshots.next")}
            className="absolute right-2 top-1/2 inline-flex h-11 w-11 -translate-y-1/2 items-center justify-center rounded-full bg-black/40 text-white/80 transition-colors hover:bg-black/70 hover:text-white sm:right-5"
          >
            <ChevronRight size={24} aria-hidden="true" />
          </button>
        </>
      )}

      {/* 弹窗内容在移动端不能超出视口：宽高各留出边距，图片自适应 */}
      <figure className="flex max-h-full w-full max-w-5xl flex-col items-center gap-3">
        <img
          src={image.src}
          alt={image.alt}
          width={image.width}
          height={image.height}
          className="max-h-[75vh] w-auto max-w-full rounded-control border border-white/10 object-contain shadow-2xl"
        />
        {image.caption && (
          <figcaption className="text-center text-sm text-white/70">{image.caption}</figcaption>
        )}
      </figure>
    </div>
  );
}
