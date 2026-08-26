import { useEffect, useRef } from "react";
import { X } from "lucide-react";
import { BrandIcon, BrandPresetPicker, brandLabel } from "@/components/BrandIcon";
import { useT } from "@/lib/useT";

/**
 * 品牌图标挑选器的**弹窗**形态。
 *
 * # 为什么从内联改成弹窗
 *
 * 内联那版嵌在「厂商 / 协议」那一行的左列里，而右列是个固定 `w-48` 的协议下拉。
 * 品牌网格把整行撑到 ~200px 高，右列却只有一个 40px 的下拉可填 ——
 * 于是协议下拉右下方留出一大块空白（真机截图里那个红框）。
 *
 * 这不是「调调间距」能修好的：**32 个品牌的目录本质上不是一个表单字段**，
 * 它需要搜索 + 分组 + 滚动，占的空间量级与旁边的单选下拉差两个数量级。
 * 塞进同一行必然要么挤瘦它（搜索框只剩 300px）、要么撑高整行留白。
 * 弹窗把它移出表单流：那一行回到正常高度，空白随之消失，而目录反倒拿到全宽。
 *
 * # 交互判据
 *
 * - 触发器**显示当前选择**（图标 + 名字），不是一个只写「选择图标」的哑按钮 ——
 *   否则用户看不出自己选过什么，得点开才知道；
 * - 选中即关闭：挑图标是个一次性动作，留着弹窗等用户再点一次关是多余的一步；
 * - Esc 关闭 + 打开时焦点进搜索框（键盘可用）；
 * - 「跟随厂商自动匹配」是显式的一项，而不是靠「再点一次已选中的」这个隐藏手势
 *   —— 那个手势内联版就有，但没人猜得到。
 */
export function BrandPickerDialog({
  open,
  value,
  onChange,
  onClose,
}: {
  open: boolean;
  value?: string;
  onChange: (next: string | undefined) => void;
  onClose: () => void;
}) {
  const t = useT();
  const boxRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        onClose();
      }
    };
    // 捕获阶段：KeyEditor 外层也监听 Esc（关整个抽屉），不拦住会一次关两层。
    window.addEventListener("keydown", onKey, true);
    // 打开就把焦点送进搜索框，键盘用户不用先 Tab 一圈。
    boxRef.current?.querySelector<HTMLInputElement>("input[type=text]")?.focus();
    return () => window.removeEventListener("keydown", onKey, true);
  }, [open, onClose]);

  if (!open) return null;

  return (
    <div
      className="fixed inset-0 z-[60] flex items-center justify-center bg-black/40 p-4"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div
        ref={boxRef}
        role="dialog"
        aria-modal="true"
        aria-label={t("brandPicker.title")}
        className="flex max-h-[min(560px,90vh)] w-[min(520px,100%)] flex-col overflow-hidden rounded-card border border-border bg-surface shadow-2xl"
      >
        <div className="flex items-center justify-between border-b border-border px-4 py-3">
          <h3 className="text-sm font-semibold text-text-primary">{t("brandPicker.title")}</h3>
          <button
            type="button"
            onClick={onClose}
            aria-label={t("common.close")}
            className="rounded p-1 text-text-muted hover:bg-surface-hover hover:text-text-primary"
          >
            <X size={16} aria-hidden="true" />
          </button>
        </div>

        <div className="min-h-0 flex-1 overflow-hidden px-4 py-3">
          {/* 弹窗里不再限高到 224px：目录自己吃满剩余高度，一屏能看到的品牌多一倍 */}
          <BrandPresetPicker
            value={value}
            listClassName="max-h-[min(380px,60vh)]"
            onChange={(next) => {
              onChange(next);
              onClose();
            }}
          />
        </div>

        <div className="flex items-center justify-between gap-3 border-t border-border px-4 py-3">
          <p className="text-[11px] leading-relaxed text-text-muted">
            {t("editor.iconPresetHint")}
          </p>
          <button
            type="button"
            onClick={() => {
              onChange(undefined);
              onClose();
            }}
            className="shrink-0 rounded-control border border-border px-2.5 py-1.5 text-xs text-text-secondary hover:bg-surface-hover"
          >
            {t("brandPicker.auto")}
          </button>
        </div>
      </div>
    </div>
  );
}

/**
 * 打开挑选器的触发按钮。**显示当前选择**，而不是一个哑的「选择图标」。
 *
 * `vendorHint` 用于「未显式选择」时的回显：那种情况下图标是按厂商名启发式猜的，
 * 按钮上要照实显示猜出来的那个 + 「自动」二字，否则用户分不清
 * 「我选了这个」与「程序替我猜的」——而这两者在他改厂商时的行为完全不同。
 */
export function BrandPickerTrigger({
  value,
  vendorHint,
  fallbackLabel,
  onOpen,
  disabled,
}: {
  value?: string;
  vendorHint?: string;
  fallbackLabel?: string;
  onOpen: () => void;
  disabled?: boolean;
}) {
  const t = useT();
  const explicit = brandLabel(value);
  return (
    <button
      type="button"
      onClick={onOpen}
      disabled={disabled}
      className="flex w-full items-center gap-2 rounded-control border border-border bg-surface px-2 py-1.5 text-left text-xs text-text-secondary transition-colors hover:border-primary/50 hover:bg-surface-hover disabled:cursor-not-allowed disabled:opacity-50"
    >
      <BrandIcon
        hint={vendorHint}
        fallbackLabel={fallbackLabel ?? vendorHint}
        iconUrl={value}
        size={20}
      />
      <span className="min-w-0 flex-1 truncate">
        {explicit ?? t("brandPicker.autoCurrent")}
      </span>
      <span className="shrink-0 text-[11px] text-text-muted">{t("brandPicker.change")}</span>
    </button>
  );
}
