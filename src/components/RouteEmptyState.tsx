import type { ReactNode } from "react";
import { useId } from "react";

export function RouteEmptyState({
  title,
  description,
  action,
}: {
  title: string;
  description: string;
  action: ReactNode;
}) {
  const instanceId = useId().replace(/:/g, "");
  const lineId = `route-empty-line-${instanceId}`;
  const glowId = `route-empty-glow-${instanceId}`;
  return (
    <div className="route-empty-state flex flex-col items-center justify-center rounded-card border border-dashed border-border-strong bg-surface/70 px-6 py-14 text-center">
      <svg className="route-empty-visual h-32 w-full max-w-[360px]" viewBox="0 0 360 128" aria-hidden="true">
        <defs>
          <linearGradient id={lineId} x1="0" x2="1">
            <stop offset="0" stopColor="rgb(var(--border-strong) / 0.2)" />
            <stop offset="0.5" stopColor="rgb(var(--primary) / 0.5)" />
            <stop offset="1" stopColor="rgb(var(--border-strong) / 0.2)" />
          </linearGradient>
          <radialGradient id={glowId}>
            <stop offset="0" stopColor="rgb(var(--primary) / 0.2)" />
            <stop offset="1" stopColor="rgb(var(--primary) / 0)" />
          </radialGradient>
        </defs>
        <path className="route-empty__track" style={{ stroke: `url(#${lineId})` }} d="M 32 84 C 92 84, 108 34, 180 64 S 270 102, 328 42" />
        <path className="route-empty__track route-empty__track--secondary" style={{ stroke: `url(#${lineId})` }} d="M 32 42 C 90 42, 116 94, 180 64 S 270 30, 328 84" />
        <circle cx="180" cy="64" r="42" fill={`url(#${glowId})`} />
        <circle cx="32" cy="84" r="6" className="route-empty__node" />
        <circle cx="32" cy="42" r="6" className="route-empty__node" />
        <circle cx="328" cy="42" r="6" className="route-empty__node" />
        <circle cx="328" cy="84" r="6" className="route-empty__node" />
        <circle cx="180" cy="64" r="11" className="route-empty__hub" />
        <circle cx="180" cy="64" r="22" className="route-empty__ring" />
        <circle cx="180" cy="64" r="4" className="route-empty__core" />
      </svg>
      <h2 className="mt-2 text-sm font-semibold text-text-primary">{title}</h2>
      <p className="mt-1.5 max-w-sm text-xs leading-5 text-text-secondary">{description}</p>
      <div className="mt-5">{action}</div>
    </div>
  );
}
