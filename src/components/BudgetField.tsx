// 花费预算输入框。从 KeyEditor 抽出（那边冻结在棘轮上、余量为 0），
// 与 CostMultiplierField 对称 —— 同属「钱」这一类，一个形状、自带校验与说明行。

import { isValidBudget } from "@/lib/budget";

export function BudgetField({
  value,
  onChange,
  t,
}: {
  value: string;
  onChange: (v: string) => void;
  t: (k: string, p?: Record<string, string>) => string;
}) {
  const ok = isValidBudget(value);
  return (
    <div>
      <div className="mb-1 text-xs font-medium text-text-secondary">{t("budget.label")}</div>
      <input
        className={`h-9 w-full rounded-control border bg-surface px-3 font-mono text-sm text-text-primary focus:outline-none focus:ring-2 ${
          ok ? "border-border focus:ring-ring" : "border-danger focus:ring-danger"
        }`}
        value={value}
        placeholder="50"
        inputMode="decimal"
        onChange={(e) => onChange(e.target.value)}
      />
      {ok ? (
        <p className="mt-1 text-[11px] leading-relaxed text-text-muted">{t("budget.hint")}</p>
      ) : (
        // 后端对无效预算静默视同「未设」，所以这里必须说出来：否则用户填了 0/负数/文字
        // 以为设了上限，用量页却从不标红，而那正是他配这个字段要的东西。
        <p className="mt-1 text-[11px] leading-relaxed text-danger">{t("budget.invalid")}</p>
      )}
    </div>
  );
}
