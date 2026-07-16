import { useEffect } from "react";
import { useStore } from "@/store";
import { AlertTriangle, CheckCircle2, X } from "lucide-react";

/**
 * 全局轻提示：渲染 store.toast。
 * 落盘失败等错误必须可见（禁止静默吞掉），错误提示不自动消失需手动关闭；
 * 成功提示 3s 后自动消失。
 */
export function Toast() {
  const { toast, clearToast } = useStore();

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
    <div className="pointer-events-none fixed bottom-4 left-1/2 z-[100] -translate-x-1/2">
      <div
        className={`pointer-events-auto flex max-w-lg items-start gap-2.5 rounded-control px-4 py-3 text-sm shadow-lg ${
          isError
            ? "bg-danger text-white"
            : "bg-success text-white"
        }`}
        role={isError ? "alert" : "status"}
      >
        <Icon size={16} className="mt-0.5 shrink-0" />
        <span className="min-w-0 break-words">{toast.msg}</span>
        <button
          onClick={clearToast}
          className="ml-1 shrink-0 rounded p-0.5 opacity-80 hover:opacity-100"
          aria-label="close"
        >
          <X size={14} />
        </button>
      </div>
    </div>
  );
}
