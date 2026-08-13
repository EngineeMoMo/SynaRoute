import { useEffect, useRef, useState } from "react";
import { ChevronDown } from "lucide-react";
import { Tooltip } from "@/components/ui/Tooltip";

export interface StatusBarPickerOption {
  /** 提交给回调的值 */
  value: string;
  /** 显示文本 */
  label: string;
  /** 可选的第二行说明（如 Key 的健康状态） */
  hint?: string;
}

/**
 * 状态条上的紧凑下拉选择器（UX#6 / UX#8）。
 *
 * **为什么不复用 `ui/Combobox`**：那个组件是「文本输入 + 子串过滤 + allowCustom」，值即文本。
 * 主 Key 需要 `id → 显示名` 的映射，而 Key 名**允许重名**（KeyEditor 不校验唯一性），
 * 用字符串匹配会选中另一条 Key。且状态条空间很窄，不需要输入过滤。
 * 三个下拉共用本组件是为了视觉统一 —— 它们并排出现，样式一旦不一致会很显眼。
 *
 * 交互约定（与本仓既有弹层一致）：
 * - **遮罩关闭必须用 `onMouseDown` 且判 `target === currentTarget`**，不能用 `onClick={onClose}`。
 *   否则在菜单内按下鼠标、拖到菜单外松手时会误关（本项目踩过并记录的坑）。
 * - Esc 关闭、↑↓ 移动、Enter 选中：状态条是高频操作区，纯鼠标不够用。
 */
export function StatusBarPicker({
  label,
  value,
  options,
  onChange,
  emptyHint,
  title,
  disabled,
  placeholder,
}: {
  /** 下拉左侧的固定标签（如「主 Key」） */
  label: string;
  value: string;
  options: StatusBarPickerOption[];
  onChange: (v: string) => void;
  /** 选项为空时显示的话 */
  emptyHint: string;
  /**
   * 悬停提示，用来承载「切换即时生效」这类说明——状态条上没空间常驻显示。
   *
   * 走自研 `Tooltip` 而非原生 `title`：这里承载的是**长说明**（如推理强度那段两行文字），
   * 原生 title 有约 1s 延迟、无换行控制、字号由系统定，长文本会渲染成一坨看不下去，
   * 且 WebView2 里表现不一致。Tooltip 组件当初就是为替掉这些位置才建的。
   */
  title?: string;
  disabled?: boolean;
  /** value 为空时显示的占位文本 */
  placeholder?: string;
}) {
  const [open, setOpen] = useState(false);
  const [active, setActive] = useState(0);
  const boxRef = useRef<HTMLDivElement>(null);

  const current = options.find((o) => o.value === value);
  // 找不到当前值时**照常显示原始字符串**而不是回退成占位符：
  // 那种情况多半是「配置里存着一个已失效的名字」，显示出来用户才知道要重选，
  // 静默显示成「自动」会让他以为自己没配过。
  const display = current?.label ?? (value || placeholder || "—");

  useEffect(() => {
    if (!open) return;
    setActive(Math.max(0, options.findIndex((o) => o.value === value)));
  }, [open, options, value]);

  const pick = (v: string) => {
    setOpen(false);
    if (v !== value) onChange(v);
  };

  return (
    <div className="relative" ref={boxRef}>
      {/* 菜单展开时禁掉提示：否则提示框会浮在自己刚打开的选项列表上方挡住第一项。
          `content` 为空时 Tooltip 自身也不显示（见其 show()），故无 title 的调用方无副作用。 */}
      <Tooltip content={title} side="bottom" disabled={open}>
        <button
          type="button"
          disabled={disabled}
          onClick={() => setOpen((o) => !o)}
          className="flex max-w-[220px] items-center gap-1 rounded-control px-1.5 py-1 text-xs text-text-secondary hover:bg-surface-hover disabled:cursor-not-allowed disabled:opacity-50"
        >
          <span className="shrink-0 text-text-muted">{label}</span>
          <span className="truncate font-medium text-text-primary">{display}</span>
          <ChevronDown size={12} className="shrink-0 text-text-muted" />
        </button>
      </Tooltip>

      {open && (
        <>
          {/* 遮罩：onMouseDown + target===currentTarget，禁用 onClick={close}——
              菜单内按下、菜单外松手会误关（本仓既有约定）。 */}
          <div
            className="fixed inset-0 z-40"
            onMouseDown={(e) => {
              if (e.target === e.currentTarget) setOpen(false);
            }}
          />
          <div
            className="absolute left-0 top-full z-50 mt-1 max-h-72 min-w-[220px] overflow-auto rounded-control border border-border bg-surface py-1 shadow-card-hover"
            role="listbox"
            tabIndex={-1}
            onKeyDown={(e) => {
              if (e.key === "Escape") {
                e.preventDefault();
                setOpen(false);
              } else if (e.key === "ArrowDown") {
                e.preventDefault();
                setActive((i) => Math.min(options.length - 1, i + 1));
              } else if (e.key === "ArrowUp") {
                e.preventDefault();
                setActive((i) => Math.max(0, i - 1));
              } else if (e.key === "Enter") {
                e.preventDefault();
                const o = options[active];
                if (o) pick(o.value);
              }
            }}
            ref={(el) => el?.focus()}
          >
            {options.length === 0 && (
              <div className="px-3 py-1.5 text-xs text-text-muted">{emptyHint}</div>
            )}
            {options.map((o, i) => (
              <button
                key={o.value}
                type="button"
                role="option"
                aria-selected={o.value === value}
                onMouseEnter={() => setActive(i)}
                onClick={() => pick(o.value)}
                className={`block w-full px-3 py-1.5 text-left text-xs ${
                  i === active ? "bg-surface-hover" : ""
                } ${o.value === value ? "text-primary" : "text-text-primary"}`}
              >
                <div className="truncate font-medium">{o.label}</div>
                {o.hint && <div className="truncate text-[11px] text-text-muted">{o.hint}</div>}
              </button>
            ))}
          </div>
        </>
      )}
    </div>
  );
}
