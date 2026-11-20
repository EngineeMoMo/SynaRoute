import type { ReactNode } from "react";
import { Waypoints } from "lucide-react";

export function RouteEmptyState({
  title,
  description,
  action,
}: {
  title: string;
  description: string;
  action: ReactNode;
}) {
  return (
    <div className="route-empty-state flex flex-col items-center justify-center rounded-card border border-dashed border-border-strong bg-surface px-6 py-10 text-center">
      <svg className="route-empty-visual mb-3 h-28 w-full max-w-[240px]" viewBox="0 0 240 112" aria-hidden="true">
        <path className="route-empty__track" d="M 48 78 V 68 Q 48 62 54 62 H 186 Q 192 62 192 68 V 78 M 120 48 V 78" />
        <rect x="100" y="8" width="40" height="40" rx="10" className="route-empty__hub" />
        <Waypoints x={110} y={18} width={20} height={20} strokeWidth={1.75} className="text-primary" />
        {[28, 100, 172].map((position) => (
          <g key={position}>
            <rect x={position} y="78" width="40" height="24" rx="6" className="route-empty__node" />
            <path d={`M ${position + 12} 88 H ${position + 28} M ${position + 12} 93 H ${position + 22}`} className="route-empty__track" />
          </g>
        ))}
      </svg>
      <h2 className="mt-2 text-sm font-semibold text-text-primary">{title}</h2>
      <p className="mt-1.5 max-w-sm text-xs leading-5 text-text-secondary">{description}</p>
      <div className="mt-5">{action}</div>
    </div>
  );
}
