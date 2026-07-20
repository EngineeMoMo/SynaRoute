import { useEffect, useMemo, useRef, useState } from "react";
import { ChevronDown, Check } from "lucide-react";

interface ComboboxProps {
  value: string;
  options: string[];
  onChange: (v: string) => void;
  placeholder?: string;
  /** 允许输入不在候选里的自定义值（默认允许） */
  allowCustom?: boolean;
  className?: string;
  /** 无候选时的提示 */
  emptyHint?: string;
}

/**
 * 可搜索下拉：点箭头/聚焦即展开全部候选；输入时按子串过滤但始终保留已输入文本；
 * 允许自定义值。用于「默认兜底模型」「映射真实名」等——datalist 选中后无法再列全部候选，故自建。
 */
export function Combobox({
  value,
  options,
  onChange,
  placeholder,
  allowCustom = true,
  className = "",
  emptyHint,
}: ComboboxProps) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const rootRef = useRef<HTMLDivElement>(null);

  // 展开时以当前值为初始查询；过滤只在用户实际输入后生效
  const [typing, setTyping] = useState(false);
  const filtered = useMemo(() => {
    if (!typing || !query.trim()) return options;
    const q = query.trim().toLowerCase();
    return options.filter((o) => o.toLowerCase().includes(q));
  }, [options, query, typing]);

  // 点击外部关闭
  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      if (rootRef.current && !rootRef.current.contains(e.target as Node)) {
        setOpen(false);
        setTyping(false);
      }
    };
    document.addEventListener("mousedown", onDown);
    return () => document.removeEventListener("mousedown", onDown);
  }, [open]);

  const commit = (v: string) => {
    onChange(v);
    setOpen(false);
    setTyping(false);
    setQuery("");
  };

  const shownValue = open && typing ? query : value;

  return (
    <div ref={rootRef} className="relative">
      <div className="relative">
        <input
          className={`${className} pr-8`}
          value={shownValue}
          placeholder={placeholder}
          onChange={(e) => {
            setTyping(true);
            setQuery(e.target.value);
            setOpen(true);
            if (allowCustom) onChange(e.target.value);
          }}
          onFocus={() => {
            setOpen(true);
            setTyping(false);
          }}
        />
        <button
          type="button"
          tabIndex={-1}
          onClick={() => {
            setOpen((v) => !v);
            setTyping(false);
          }}
          className="absolute right-1.5 top-1/2 -translate-y-1/2 rounded p-1 text-text-muted hover:text-text-secondary"
        >
          <ChevronDown size={14} className={open ? "rotate-180 transition-transform" : "transition-transform"} />
        </button>
      </div>

      {open && (
        <div className="absolute z-20 mt-1 max-h-48 w-full overflow-y-auto rounded-control border border-border bg-surface py-1 shadow-lg">
          {filtered.length === 0 ? (
            <div className="px-3 py-2 text-xs text-text-muted">{emptyHint ?? "无匹配项"}</div>
          ) : (
            filtered.map((o) => (
              <button
                key={o}
                type="button"
                onClick={() => commit(o)}
                className="flex w-full items-center gap-2 px-3 py-1.5 text-left font-mono text-xs text-text-primary hover:bg-surface-hover"
              >
                <Check size={12} className={o === value ? "text-primary" : "invisible"} />
                <span className="truncate">{o}</span>
              </button>
            ))
          )}
        </div>
      )}
    </div>
  );
}
