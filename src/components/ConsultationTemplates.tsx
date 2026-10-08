import { consultationTemplates, type ConsultationTemplate } from "@/lib/consultation";
import { useT } from "@/lib/useT";

export function ConsultationTemplates({ value, onChange, disabled }: {
  value: ConsultationTemplate; onChange: (value: ConsultationTemplate) => void; disabled: boolean;
}) {
  const t = useT();
  return <fieldset disabled={disabled} className="space-y-2">
    <legend className="mb-2 text-xs font-medium text-text-secondary">{t("brain.templateTitle")}</legend>
    <div className="flex flex-wrap gap-2">
      {consultationTemplates.map((template) => <label key={template} className={`flex cursor-pointer items-center gap-2 rounded-control border px-3 py-2 text-sm focus-within:ring-2 focus-within:ring-ring ${disabled ? "cursor-default opacity-60" : "hover:border-border-strong"} ${value === template ? "border-primary bg-primary/5 text-text-primary" : "border-border text-text-secondary"}`}>
        <input type="radio" name="consultation-template" value={template} checked={value === template} onChange={() => onChange(template)} className="accent-primary" />
        {t(`brain.template.${template}.title`)}
      </label>)}
    </div>
    <p className="text-xs leading-relaxed text-text-secondary">{t(`brain.template.${value}.description`)}</p>
  </fieldset>;
}
