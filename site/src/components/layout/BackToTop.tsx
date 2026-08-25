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
        "fixed bottom-5 right-5 z-40 inline-flex h-11 w-11 items-center justify-center rounded-full border border-border bg-surface text-text-secondary shadow-card-hover transition-all duration-250 hover:text-text-primary",
        show ? "translate-y-0 opacity-100" : "pointer-events-none translate-y-3 opacity-0"
      )}
    >
      <ArrowUp size={18} aria-hidden="true" />
    </button>
  );
}
