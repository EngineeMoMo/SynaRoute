import { useEffect } from "react";
import { useStore } from "@/store";
import { AlertTriangle, CheckCircle2, X } from "lucide-react";

/**
 * 全局轻提示：渲染 store.toast。
 * 落盘失败等错误必须可见（禁止静默吞掉），错误提示不自动消失需手动关闭；
 * 成功提示 3s 后自动消失。
 */
export function Toast() {
  const toast = useStore((s) => s.toast);
  const clearToast = useStore((s) => s.clearToast);

  useEffect(() => {
    if (toast?.kind === "success") {
      const id = setTimeout(clearToast, 3000);
      return () => clearTimeout(id);
    }
  }, [toast, clearToast]);

  if (!toast) return null;

  const isError = toast.kind === "error";
  const Icon = isError ? AlertTriangle : CheckCircle2;

  return (
    <div className="pointer-events-none fixed bottom-5 left-1/2 z-[100] w-[min(460px,calc(100vw-2rem))] -translate-x-1/2">
      <div
        className={`pointer-events-auto flex items-start gap-3 rounded-card border bg-surface-overlay/95 px-4 py-3 text-sm shadow-elevated backdrop-blur-md ${
          isError ? "border-danger/35" : "border-route/30"
        }`}
        role={isError ? "alert" : "status"}
      >
        <span
          className={`mt-0.5 flex h-7 w-7 shrink-0 items-center justify-center rounded-control ${
            isError ? "bg-danger/12 text-danger" : "bg-route/12 text-route"
          }`}
        >
          <Icon size={16} />
        </span>
        <span className="min-w-0 flex-1 break-words leading-relaxed text-text-primary">{toast.msg}</span>
        <button
          onClick={clearToast}
          className="sr-icon-button h-7 w-7 shrink-0"
          aria-label="close"
        >
          <X size={14} />
        </button>
      </div>
    </div>
  );
}
