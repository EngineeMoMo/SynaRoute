import { Button } from "@/components/ui/Button";
import { useT } from "@/lib/useT";

interface Props {
  value?: string;
  reference?: string;
  className: string;
  onChange: (value: string) => void;
}

export function BalanceTokenInput({ value, reference, className, onChange }: Props) {
  const t = useT();
  return (
    <div className="flex items-center gap-1">
      <input
        className={className}
        type="password"
        autoComplete="new-password"
        aria-label={t("balance.accessToken")}
        value={value ?? ""}
        placeholder={reference && value === undefined ? t("common.saved") : undefined}
        onChange={(event) => onChange(event.target.value)}
      />
      {reference && value !== "" && (
        <Button type="button" variant="ghost" size="sm" onClick={() => onChange("")}>
          {t("common.remove")}
        </Button>
      )}
    </div>
  );
}
