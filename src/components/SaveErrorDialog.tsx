import { useEffect } from "react";
import { AlertTriangle } from "lucide-react";
import { Button } from "@/components/ui/Button";
import { useT } from "@/lib/useT";
import { DialogBody, DialogFooter, DialogFrame, DialogHeader } from "@/components/ui/DialogFrame";

/**
 * 保存失败的提示**弹窗**。
 *
 * # 为什么不能是贴在表单末尾的那条 banner
 *
 * 原先它在 KeyEditor 可滚动表单体的**最后**，而那个抽屉的表单很长（模型列表 + 映射 +
 * 余额查询 + 计费全展开时轻易过两屏）。用户在中段点保存，错误落在视口外 ——
 * 界面表现为**「点了保存什么都没发生」**（真机报障原话：「提示在页面底部一般看不到」）。
 *
 * 这条消息不是可选的补充说明，而是「这次操作失败了」的唯一告知，且后端的校验消息常带
 * 「为什么不合规 + 会导致什么 + 怎么改」三段（如桌面端模型名不合规那条），
 * 需要用户真的读完再回去改。故必须是模态的。
 *
 * `whitespace-pre-line` 不可省：那些多行消息挤成一坨没人读得下去。
 */
export function SaveErrorDialog({
  error,
  onClose,
}: {
  error: string | null;
  onClose: () => void;
}) {
  const t = useT();

  useEffect(() => {
    if (!error) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        // 捕获阶段 + 阻断：外层抽屉也监听 Esc，不拦住会连表单一起关掉、
        // 用户刚填的内容全丢 —— 而他只是想关掉这条提示。
        e.stopPropagation();
        onClose();
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [error, onClose]);

  if (!error) return null;

  return (
    <DialogFrame
      size="sm"
      role="alertdialog"
      ariaLabel={t("editor.saveFailedTitle")}
      overlayClassName="z-[60]"
      className="max-h-[min(420px,85vh)] w-[min(460px,100%)] border-danger/35"
      onBackdropMouseDown={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <DialogHeader>
        <div className="flex items-center gap-2">
          <span className="flex h-8 w-8 items-center justify-center rounded-control bg-danger/12 text-danger">
            <AlertTriangle size={16} aria-hidden="true" />
          </span>
          <h3 className="text-sm font-semibold text-text-primary">
            {t("editor.saveFailedTitle")}
          </h3>
        </div>
      </DialogHeader>
      <DialogBody>
        <p className="whitespace-pre-line text-xs leading-relaxed text-text-secondary">
          {error}
        </p>
      </DialogBody>
      <DialogFooter>
        <Button onClick={onClose} autoFocus>
          {t("common.ok")}
        </Button>
      </DialogFooter>
    </DialogFrame>
  );
}
