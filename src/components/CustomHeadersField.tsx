// 「自定义请求头」输入框（B1，docs/14 §21.1）。
//
// 抽成独立组件而不是写在 KeyEditor 里：`KeyEditor.tsx` 冻结在棘轮上、余量为 0，
// 而目录化/大拆分是 docs/15 P2 刻意未做的。同 `BrandPresetPicker` / `McpAddressList` 的做法。
//
// 判据复用 `checkCustomHeaders`（与 Rust `custom_headers` 由
// `tests/reservedHeadersParity.test.ts` 跨语言对账）—— 界面即时提示的与保存时真正拦下的
// 必须是同一批头名字，两边各自 filter 是这类功能最典型的漂移源。

import { checkCustomHeaders } from "@/lib/reservedHeaders";

export function CustomHeadersField({
  value,
  onChange,
  t,
}: {
  value: string;
  onChange: (v: string) => void;
  t: (k: string, p?: Record<string, string>) => string;
}) {
  const check = checkCustomHeaders(value);
  const bad = check.kind === "invalid" || check.kind === "reserved";

  return (
    <div>
      <div className="mb-1 text-xs font-medium text-text-secondary">{t("customHeaders.label")}</div>
      <textarea
        rows={3}
        spellCheck={false}
        className={`w-full rounded-control border bg-surface px-3 py-2 font-mono text-xs leading-relaxed text-text-primary focus:outline-none focus:ring-2 ${
          bad ? "border-danger focus:ring-danger" : "border-border focus:ring-ring"
        }`}
        value={value}
        placeholder={t("customHeaders.placeholder")}
        onChange={(e) => onChange(e.target.value)}
      />
      {/* 状态行必须**说出原因**：后端保存时会拒，若这里只把边框变红，
          用户不知道该改什么，只能反复试。保留字段那条要指名是哪几个。 */}
      {check.kind === "invalid" && (
        <p className="mt-1 text-[11px] leading-relaxed text-danger">
          {t("customHeaders.invalid", { err: check.reason })}
        </p>
      )}
      {check.kind === "reserved" && (
        <p className="mt-1 text-[11px] leading-relaxed text-danger">
          {t("customHeaders.reserved", { names: check.names.join("、") })}
        </p>
      )}
      {!bad && (
        <p className="mt-1 text-[11px] leading-relaxed text-text-muted">
          {check.kind === "ok"
            ? t("customHeaders.ok", { n: String(check.count) })
            : t("customHeaders.empty")}
          {" · "}
          {t("customHeaders.desc")}
        </p>
      )}
    </div>
  );
}
