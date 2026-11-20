import { Activity, KeyRound, LockKeyhole, Radio, ShieldCheck } from "lucide-react";
import type { LucideIcon } from "lucide-react";
import { useId } from "react";
import { useT } from "@/lib/useT";
import { usePrefersReducedMotion } from "@/lib/usePrefersReducedMotion";
import type { CategoryOverview } from "@/lib/routingOverview";

const NODE_LAYOUT = [
  { x: 108, y: 116, path: "M 116 116 C 205 116, 270 108, 350 108" },
  { x: 380, y: 34, path: "M 380 42 C 380 64, 380 78, 380 96" },
  { x: 652, y: 116, path: "M 644 116 C 555 116, 490 108, 410 108" },
] as const;

function Metric({
  icon: Icon,
  value,
  label,
  tone = "default",
}: {
  icon: LucideIcon;
  value: string | number;
  label: string;
  tone?: "default" | "primary" | "warning";
}) {
  return (
    <div className="route-pulse-metric rounded-control border border-border/80 bg-surface/70 px-3.5 py-3">
      <div className="flex items-center gap-2 text-[11px] text-text-secondary">
        <Icon
          size={14}
          className={tone === "warning" ? "text-warning" : tone === "primary" ? "text-primary" : "text-text-secondary"}
          aria-hidden="true"
        />
        <span>{label}</span>
      </div>
      <div className="mt-1.5 text-xl font-semibold tracking-tight text-text-primary tabular-nums">{value}</div>
    </div>
  );
}

export function RoutePulse({
  categories,
  vaultLocked,
  refreshing = false,
}: {
  categories: CategoryOverview[];
  vaultLocked: boolean;
  refreshing?: boolean;
}) {
  const t = useT();
  const instanceId = useId().replace(/:/g, "");
  const reducedMotion = usePrefersReducedMotion();
  const centerGlowId = `route-pulse-center-glow-${instanceId}`;
  const filterId = `route-pulse-glow-${instanceId}`;
  const total = categories.length;
  const running = categories.filter((category) => category.running).length;
  const configured = categories.filter((category) => category.connection.state === "configured").length;
  const enabledKeys = categories.reduce((sum, category) => sum + category.enabledCount, 0);
  const observed = categories.filter((category) => category.latest !== null).length;
  const failed = categories.filter((category) => category.latest && !category.latest.ok).length;
  const live = running > 0 && !vaultLocked;
  const statusKey = refreshing
    ? "overview.signalRefreshing"
    : vaultLocked
      ? "overview.signalLocked"
      : failed > 0
        ? "overview.signalAttention"
        : live
          ? "overview.signalLive"
          : "overview.signalIdle";

  return (
    <section className="route-pulse-shell overflow-hidden rounded-card border border-border bg-surface shadow-card" data-testid="route-pulse">
      <div className="flex flex-wrap items-start justify-between gap-4 border-b border-border/80 px-5 py-4">
        <div className="min-w-0">
          <p className="sr-section-label">{t("overview.signalEyebrow")}</p>
          <h2 className="mt-1 text-base font-semibold tracking-tight text-text-primary">{t("overview.signalTitle")}</h2>
          <p className="mt-1 max-w-2xl text-xs leading-5 text-text-secondary">{t("overview.signalDescription")}</p>
        </div>
        <span
          className={`route-pulse-status inline-flex shrink-0 items-center gap-2 rounded-pill border px-3 py-1.5 text-xs font-medium ${
            live && !failed
              ? "border-primary/25 bg-primary/8 text-primary"
              : failed || vaultLocked
                ? "border-warning/30 bg-warning/8 text-warning"
                : "border-border bg-surface-hover text-text-secondary"
          }`}
          role="status"
          aria-live="polite"
        >
          <span
            className={`h-1.5 w-1.5 rounded-full ${
              live && !failed ? "bg-primary status-pulse" : failed || vaultLocked ? "bg-warning" : "bg-border-strong"
            }`}
            aria-hidden="true"
          />
          {t(statusKey)}
        </span>
      </div>

      <div className="grid gap-4 p-4 lg:grid-cols-[minmax(0,1.35fr)_minmax(16rem,0.65fr)]">
        <div className="route-pulse-visual relative min-h-[220px] overflow-hidden rounded-control border border-border/80 bg-surface-elevated" aria-hidden="true">
          <svg className="h-full min-h-[220px] w-full" viewBox="0 0 760 220" role="presentation">
            <defs>
              <radialGradient id={centerGlowId} cx="50%" cy="50%" r="50%">
                <stop offset="0%" stopColor="rgb(var(--primary) / 0.26)" />
                <stop offset="100%" stopColor="rgb(var(--primary) / 0)" />
              </radialGradient>
              <filter id={filterId} x="-80%" y="-80%" width="260%" height="260%">
                <feGaussianBlur stdDeviation="3" result="blur" />
                <feMerge>
                  <feMergeNode in="blur" />
                  <feMergeNode in="SourceGraphic" />
                </feMerge>
              </filter>
            </defs>

            <path className="route-pulse__grid" d="M 44 174 H 716 M 84 64 H 676" />
            <circle cx="380" cy="108" r="76" className="route-pulse__halo" />

            {categories.map((category, index) => {
              const layout = NODE_LAYOUT[index] ?? NODE_LAYOUT[NODE_LAYOUT.length - 1];
              const isConfigured = category.connection.state === "configured";
              const isLive = category.running && !vaultLocked;
              return (
                <g key={category.categoryId}>
                  <path d={layout.path} className={`route-pulse__path ${isConfigured ? "is-configured" : ""} ${isLive ? "is-live" : ""}`} />
                  {isLive && !reducedMotion && (
                    <circle r="3.2" className="route-pulse__packet" filter={`url(#${filterId})`}>
                      <animateMotion dur="2.8s" repeatCount="indefinite" path={layout.path} />
                    </circle>
                  )}
                  {isLive && <circle cx={layout.x} cy={layout.y} r="15" className="route-pulse__node-ring" />}
                  <circle cx={layout.x} cy={layout.y} r="7" className={`route-pulse__node ${isLive ? "is-live" : ""} ${isConfigured ? "is-configured" : ""}`} />
                  <text x={layout.x} y={layout.y + 31} textAnchor="middle" className="route-pulse__label">
                    {t(`nav.${category.categoryId}`)}
                  </text>
                </g>
              );
            })}

            <circle cx="380" cy="108" r="48" fill={`url(#${centerGlowId})`} />
            <circle cx="380" cy="108" r="24" className={`route-pulse__hub ${live ? "is-live" : ""}`} filter={live ? `url(#${filterId})` : undefined} />
            {live && <circle cx="380" cy="108" r="31" className="route-pulse__hub-ring" />}
            <text x="380" y="106" textAnchor="middle" className="route-pulse__hub-label">SR</text>
            <text x="380" y="121" textAnchor="middle" className="route-pulse__hub-caption">{t("overview.signalHub")}</text>
          </svg>
          <div className="pointer-events-none absolute inset-x-8 bottom-2 h-px bg-gradient-to-r from-transparent via-primary/20 to-transparent" />
        </div>

        <div className="grid grid-cols-2 gap-2.5 lg:grid-cols-1">
          <Metric icon={Radio} value={`${running}/${total}`} label={t("overview.signalListening")} tone={running > 0 ? "primary" : "default"} />
          <Metric icon={ShieldCheck} value={`${configured}/${total}`} label={t("overview.signalConfigured")} tone={configured === total && total > 0 ? "primary" : "default"} />
          <Metric icon={KeyRound} value={enabledKeys} label={t("overview.signalKeys")} tone={enabledKeys > 0 ? "primary" : "default"} />
          <Metric icon={vaultLocked ? LockKeyhole : Activity} value={observed} label={t("overview.signalObserved")} tone={failed > 0 || vaultLocked ? "warning" : "default"} />
        </div>
      </div>
    </section>
  );
}
