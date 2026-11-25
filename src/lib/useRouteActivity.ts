import { useCallback, useEffect, useRef, useState } from "react";
import { api } from "@/lib/bridge";
import { createOverviewRefresher } from "@/lib/routingOverview";
import { FALLBACK_POLL_MS, useBackendEvent } from "@/lib/useBackendEvents";
import { usePolling } from "@/lib/usePolling";
import type { CategoryType } from "@/types";

export function useRouteActivity() {
  const [activeRoutes, setActiveRoutes] = useState<ReadonlySet<CategoryType>>(() => new Set());
  const refreshRef = useRef<(() => Promise<void>) | null>(null);

  useEffect(() => {
    const idleTimers: ReturnType<typeof setTimeout>[] = [];
    const clearIdleTimers = () => idleTimers.splice(0).forEach((timer) => clearTimeout(timer));
    const refresher = createOverviewRefresher(
      api.routeActivity,
      (next) => {
        clearIdleTimers();
        const routes = next ?? [];
        setActiveRoutes(new Set(routes.map(({ categoryId }) => categoryId)));
        for (const { categoryId, idleInMs } of routes) {
          if (idleInMs === null) continue;
          idleTimers.push(setTimeout(() => setActiveRoutes((previous) => {
            const remaining = new Set(previous);
            remaining.delete(categoryId);
            return remaining;
          }), idleInMs));
        }
      },
      () => {},
      () => document.visibilityState === "visible",
    );
    refreshRef.current = refresher.refresh;
    return () => {
      refreshRef.current = null;
      refresher.dispose();
      clearIdleTimers();
    };
  }, []);

  const refresh = useCallback(() => void refreshRef.current?.(), []);
  useBackendEvent(["route-activity"], refresh);
  usePolling(refresh, FALLBACK_POLL_MS);
  return activeRoutes;
}
