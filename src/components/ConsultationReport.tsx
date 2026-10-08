import { useState } from "react";
import { Copy, Check } from "lucide-react";
import { Button } from "./ui/Button";
import { useT } from "@/lib/useT";
import { parseConsultationReport } from "@/lib/consultation";

export function ConsultationReport({ content }: { content: string }) {
  const t = useT();
  const [copied, setCopied] = useState(false);
  const [error, setError] = useState(false);
  const sections = parseConsultationReport(content);
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(content);
      setCopied(true);
      setError(false);
    } catch { setError(true); }
  };
  return (
    <section className="space-y-3" aria-label={t("brain.runPlanTitle")}>
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h3 className="text-sm font-semibold text-text-primary">{t("brain.runPlanTitle")}</h3>
        <Button size="sm" variant="outline" onClick={() => void copy()}>
          {copied ? <Check size={14} /> : <Copy size={14} />}
          {t(copied ? "brain.reportCopied" : "brain.reportCopy")}
        </Button>
      </div>
      <p className="text-xs leading-relaxed text-text-secondary">{t("brain.reportCaution")}</p>
      {error && <p role="alert" className="text-xs text-danger">{t("brain.reportCopyFailed")}</p>}
      {sections ? (
        <div className="divide-y divide-border rounded-control border border-border px-4">
          {sections.map((section) => (
            <section key={section.kind} className="py-4">
              <h4 className="mb-2 text-sm font-semibold text-text-primary">{t(`brain.report.${section.kind}`)}</h4>
              <div className="whitespace-pre-wrap break-words text-sm leading-relaxed text-text-secondary">{section.text}</div>
            </section>
          ))}
        </div>
      ) : <pre className="max-h-96 overflow-auto whitespace-pre-wrap break-words rounded-control border border-border p-4 font-sans text-sm leading-relaxed text-text-primary">{content}</pre>}
      {sections && <details>
        <summary className="cursor-pointer text-xs text-text-secondary">{t("brain.runDeciderOutput")}</summary>
        <pre className="mt-2 max-h-64 overflow-auto whitespace-pre-wrap break-words text-xs text-text-secondary">{content}</pre>
      </details>}
    </section>
  );
}
