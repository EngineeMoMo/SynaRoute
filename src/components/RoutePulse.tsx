import { Activity, KeyRound, LockKeyhole, Radio, ShieldCheck, Waypoints } from "lucide-react";
import type { LucideIcon } from "lucide-react";
import type { CSSProperties } from "react";
import { CATEGORY_ICONS } from "@/lib/categoryIcons";
import { useT } from "@/lib/useT";
import type { CategoryOverview } from "@/lib/routingOverview";
import type { CategoryType } from "@/types";

function connectorPath(index: number, total: number) {
  const position = ((index + 0.5) / total) * 1000;
  if (Math.abs(position - 500) < 16) return `M ${position} 48 L 500 16`;
  const direction = position < 500 ? 1 : -1;
  return `M ${position} 48 V 32 Q ${position} 24 ${position + direction * 8} 24 H ${500 - direction * 8} Q 500 24 500 16`;
}

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
    <div className="route-pulse-metric">
      <dt className="flex items-center gap-2 text-xs leading-5 text-text-secondary">
        <Icon
          size={14}
          className={`shrink-0 ${tone === "warning" ? "text-warning" : tone === "primary" ? "text-primary" : "text-text-secondary"}`}
          aria-hidden="true"
        />
        <span>{label}</span>
      </dt>
      <dd className="mt-1.5 text-xl font-semibold tracking-tight text-text-primary tabular-nums">{value}</dd>
    </div>
  );
}

export function RoutePulse({
  categories,
  vaultLocked,
  activeRoutes,
  refreshing = false,
}: {
  categories: CategoryOverview[];
  vaultLocked: boolean;
  activeRoutes: ReadonlySet<CategoryType>;
  refreshing?: boolean;
}) {
  const t = useT();
  const total = categories.length;
  const running = categories.filter((category) => category.running).length;
  const configured = categories.filter((category) => category.connection.state === "configured").length;
  const enabledKeys = categories.reduce((sum, category) => sum + category.enabledCount, 0);
  const observed = categories.filter((category) => category.latest !== null).length;
  const failed = categories.filter((category) => category.latest && !category.latest.ok).length;
  const live = running > 0 && !vaultLocked;
  const routes = categories.map((category, index) => {
    const isLive = category.running && !vaultLocked;
    return {
      category,
      isLive,
      isActive: isLive && activeRoutes.has(category.categoryId),
      isConfigured: category.connection.state === "configured",
      path: connectorPath(index, total),
    };
  });
  const lastActiveIndex = routes.reduce((last, route, index) => route.isActive ? index : last, -1);
  const hasActivity = lastActiveIndex >= 0;
  const segments = [
    ...routes.map(({ category, path, isConfigured, isActive }) => ({ id: category.categoryId, path, isConfigured, isActive })),
    { id: "hub", path: "M 500 16 V 0", isConfigured: configured > 0, isActive: hasActivity },
  ];
  const statusKey = refreshing
    ? "overview.signalRefreshing"
    : vaultLocked
      ? "overview.signalLocked"
      : failed > 0
        ? "overview.signalAttention"
        : hasActivity
          ? "overview.signalTraffic"
          : live ? "overview.signalLive" : "overview.signalIdle";

  return (
    <section className="route-pulse-shell overflow-hidden rounded-card border border-border bg-surface shadow-card" data-testid="route-pulse" aria-label={t("overview.signalTitle")}>
      <div className="flex flex-wrap items-start justify-between gap-4 border-b border-border/80 px-5 py-4">
        <div className="min-w-0">
          <p className="sr-section-label">{t("overview.signalEyebrow")}</p>
          <h2 className="mt-1 text-base font-semibold tracking-tight text-text-primary">{t("overview.signalTitle")}</h2>
          <p className="mt-1 max-w-2xl text-xs leading-5 text-text-secondary">{t("overview.signalDescription")}</p>
        </div>
        <span
          className={`route-pulse-status inline-flex shrink-0 items-center gap-2 rounded-pill border px-3 py-1.5 text-xs font-medium ${
            live && !failed
              ? "border-primary/25 bg-primary/8 text-text-primary"
              : failed || vaultLocked
                ? "border-warning/30 bg-warning/8 text-text-primary"
                : "border-border bg-surface-hover text-text-secondary"
          }`}
          role="status"
          aria-live="polite"
        >
          <span
            className={`h-1.5 w-1.5 rounded-full ${
              live && !failed ? "bg-primary" : failed || vaultLocked ? "bg-warning" : "bg-text-secondary"
            }`}
            aria-hidden="true"
          />
          {t(statusKey)}
        </span>
      </div>

      <div className="route-pulse-visual">
        <div className="route-pulse-diagram" style={{ "--route-count": Math.max(total, 1) } as CSSProperties}>
          <div className={`route-pulse-hub${live ? " is-live" : ""}${hasActivity ? " is-active" : ""}`}>
            <span className={`route-pulse-icon route-pulse-icon--hub${hasActivity ? " status-pulse" : ""}`} aria-hidden="true"><Waypoints size={22} strokeWidth={1.75} /></span>
            <div className="route-pulse-copy">
              <h3 className="route-pulse-name">{t("overview.signalHub")}</h3>
              <p className="route-pulse-detail">{t("overview.signalHubHint")}</p>
            </div>
          </div>
          {total > 0 && <svg className="route-pulse-connectors" viewBox="0 0 1000 48" preserveAspectRatio="none" aria-hidden="true">
            {segments.map(({ id, path, isConfigured, isActive }) => (
              <path key={id} d={path} vectorEffect="non-scaling-stroke" className={`route-pulse__path${isConfigured ? " is-configured" : ""}${isActive ? " is-active" : ""}`} />
            ))}
            {segments.filter(({ isActive }) => isActive).map(({ id, path }) => (
              <path key={`flow-${id}`} d={path} vectorEffect="non-scaling-stroke" className="route-pulse__path route-pulse__flow" />
            ))}
          </svg>}
          <ul className="route-pulse-clients">
            {routes.map(({ category, isLive, isActive }, index) => {
              const Icon = CATEGORY_ICONS[category.categoryId];
              const isLocked = category.running && vaultLocked;
              return (
                <li key={category.categoryId} className={`route-pulse-client${isActive ? " is-active" : ""}`}>
                  {index <= lastActiveIndex && <span className={`route-pulse-spine-flow${index === lastActiveIndex ? " is-end" : ""}`} aria-hidden="true" />}
                  <span className={`route-pulse-icon${isActive ? " status-pulse" : ""}`} aria-hidden="true"><Icon size={19} strokeWidth={1.75} /></span>
                  <div className="route-pulse-copy">
                    <h3 className="route-pulse-name">{t(`nav.${category.categoryId}`)}</h3>
                    <p className="route-pulse-detail flex items-center gap-1.5">
                      <span className={`route-pulse-dot ${isLocked ? "is-locked" : isLive ? "is-live" : ""}`} aria-hidden="true" />
                      <span>{t(isLocked ? "overview.signalLocked" : isActive ? "overview.signalTraffic" : isLive ? "overview.running" : "overview.stopped")}</span>
                    </p>
                  </div>
                </li>
              );
            })}
          </ul>
        </div>
      </div>

      <dl className="route-pulse-metrics">
        <Metric icon={Radio} value={`${running}/${total}`} label={t("overview.signalListening")} tone={running > 0 ? "primary" : "default"} />
        <Metric icon={ShieldCheck} value={`${configured}/${total}`} label={t("overview.signalConfigured")} tone={configured === total && total > 0 ? "primary" : "default"} />
        <Metric icon={KeyRound} value={enabledKeys} label={t("overview.signalKeys")} tone={enabledKeys > 0 ? "primary" : "default"} />
        <Metric icon={vaultLocked ? LockKeyhole : Activity} value={observed} label={t("overview.signalObserved")} tone={failed > 0 || vaultLocked ? "warning" : "default"} />
      </dl>
    </section>
  );
}
