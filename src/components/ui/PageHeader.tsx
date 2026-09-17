import type { ReactNode } from "react";
import type { LucideIcon } from "lucide-react";

export function PageHeader({
  icon: Icon,
  title,
  description,
  actions,
}: {
  icon?: LucideIcon;
  title: string;
  description?: ReactNode;
  actions?: ReactNode;
}) {
  return (
    <header className="flex shrink-0 items-start justify-between gap-4 border-b border-border/80 px-6 py-4">
      <div className="min-w-0">
        <div className="flex items-center gap-2.5">
          {Icon && (
            <span className="flex h-8 w-8 shrink-0 items-center justify-center rounded-control border border-route/20 bg-route/12 text-route">
              <Icon size={17} aria-hidden="true" />
            </span>
          )}
          <h1 className="truncate text-lg font-semibold tracking-[-0.01em] text-text-primary">{title}</h1>
        </div>
        {description && <div className="mt-1.5 max-w-2xl text-xs leading-relaxed text-text-secondary">{description}</div>}
      </div>
      {actions && <div className="flex shrink-0 items-center gap-2">{actions}</div>}
    </header>
  );
}
