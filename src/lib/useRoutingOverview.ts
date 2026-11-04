import { useCallback, useEffect, useRef, useState } from "react";
import { api } from "@/lib/bridge";
import { useBackendEvent, FALLBACK_POLL_MS } from "@/lib/useBackendEvents";
import { usePolling } from "@/lib/usePolling";
import { createOverviewRefresher, type RoutingOverview } from "@/lib/routingOverview";

export function useRoutingOverview() {
  const [data, setData] = useState<RoutingOverview | null>(null);
  const [failed, setFailed] = useState(false);
  const [loading, setLoading] = useState(false);
  const refresher = useRef<ReturnType<typeof createOverviewRefresher> | null>(null);

  useEffect(() => {
    const current = createOverviewRefresher(
      () => api.routingOverview(),
      (next, error) => {
        if (next !== null) setData(next);
        setFailed(error);
      },
      setLoading,
      () => document.visibilityState === "visible",
    );
    refresher.current = current;
    return () => {
      current.dispose();
      refresher.current = null;
    };
  }, []);

  const refresh = useCallback(() => refresher.current?.refresh(), []);
  useBackendEvent(["config", "settings", "health", "proxy", "vault", "logs"], () => refresher.current?.schedule());
  usePolling(() => void refresh(), FALLBACK_POLL_MS);
  return { data, failed, loading, refresh };
}
