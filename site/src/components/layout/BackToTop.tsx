import { useEffect, useState } from "react";
import { ArrowUp } from "lucide-react";
import { useT } from "@/hooks/useLang";
import { cn } from "@/lib/utils";
import { scrollToTop } from "@/lib/motion";

/** 滚过一屏后出现的返回顶部按钮 */
export function BackToTop() {
  const t = useT();
  const [show, setShow] = useState(false);

  useEffect(() => {
    const onScroll = () => setShow(window.scrollY > window.innerHeight);
    onScroll();
    window.addEventListener("scroll", onScroll, { passive: true });
    return () => window.removeEventListener("scroll", onScroll);
  }, []);

  return (
    <button
      type="button"
      aria-label={t("common.backToTop")}
      title={t("common.backToTop")}
      onClick={scrollToTop}
      className={cn(
        // <640px 缩一档并往角落收：44px 的圆钮固定在 bottom-5 right-5 时会压住
        // 移动端那些 `w-full` 主按钮的右端 —— 两者都可点，指头落在重叠处
        // 究竟触发哪一个取决于层级，不该让用户去猜。
        // 海拔用 raised —— 它是真的浮在页面之上的东西。
        "fixed bottom-4 right-3 z-40 inline-flex h-10 w-10 items-center justify-center rounded-full border border-border bg-surface text-text-secondary shadow-raised transition-all duration-250 hover:border-border-strong hover:text-text-primary sm:bottom-5 sm:right-5 sm:h-11 sm:w-11",
        show ? "translate-y-0 opacity-100" : "pointer-events-none translate-y-3 opacity-0"
      )}
    >
      <ArrowUp size={18} aria-hidden="true" />
    </button>
  );
}
