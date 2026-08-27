// 计费倍率输入框。从 KeyEditor 抽出（那边冻结在棘轮上、余量为 0），
// 与同属「钱」这一类的 `CustomHeadersField` 对称 —— 两个字段一个形状，都自带校验与说明行。

import { MAX_COST_MULTIPLIER, isValidCostMultiplier } from "@/lib/costMultiplier";

export function CostMultiplierField({
  value,
  onChange,
  t,
}: {
  value: string;
  onChange: (v: string) => void;
  t: (k: string, p?: Record<string, string>) => string;
}) {
  const ok = isValidCostMultiplier(value);
  return (
    <div>
      <div className="mb-1 text-xs font-medium text-text-secondary">{t("balance.multiplier")}</div>
      <input
        className={`h-9 w-full rounded-control border bg-surface px-3 font-mono text-sm text-text-primary focus:outline-none focus:ring-2 ${
          ok ? "border-border focus:ring-ring" : "border-danger focus:ring-danger"
        }`}
        value={value}
        placeholder="1.0"
        onChange={(e) => onChange(e.target.value)}
      />
      {ok ? (
        <p className="mt-1 text-[11px] leading-relaxed text-text-muted">{t("balance.multiplierHint")}</p>
      ) : (
        // 后端对非法值静默退回 1.0（不让笔误把金额算成 0），所以这里必须说出来：
        // 否则用户填了「三折」「30%」以为生效了，用量页却按原价算，金额差 3 倍而无人告知。
        <p className="mt-1 text-[11px] leading-relaxed text-danger">
          {t("balance.multiplierInvalid", { max: String(MAX_COST_MULTIPLIER) })}
        </p>
      )}
    </div>
  );
}
