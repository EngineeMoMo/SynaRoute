import { useCallback, useEffect, useRef, useState } from "react";
import { api } from "@/lib/bridge";
import { useBackendEvent, FALLBACK_POLL_MS } from "@/lib/useBackendEvents";
import { usePolling } from "@/lib/usePolling";
import { createOverviewRefresher } from "@/lib/routingOverview";
import type { RoutingPolicyView } from "@/lib/routingPolicies";

export function useRoutingPolicies() {
  const [data, setData] = useState<RoutingPolicyView | null>(null);
  const [failed, setFailed] = useState(false);
  const [loading, setLoading] = useState(false);
  const revision = useRef(-1);
  const refresher = useRef<ReturnType<typeof createOverviewRefresher<RoutingPolicyView>> | null>(null);
  const accept = useCallback((next: RoutingPolicyView) => {
    if (next.revision < revision.current) return;
    revision.current = next.revision;
    setData(next);
    setFailed(false);
  }, []);
  useEffect(() => {
    const current = createOverviewRefresher(() => api.routingPolicies(), (next, error) => {
      if (next) accept(next);
      else if (error) setFailed(true);
    }, setLoading, () => document.visibilityState === "visible");
    refresher.current = current;
    return () => { current.dispose(); refresher.current = null; };
  }, [accept]);
  const refresh = useCallback(() => refresher.current?.refresh(), []);
  useBackendEvent(["config", "settings", "proxy", "vault", "logs"], () => refresher.current?.schedule());
  usePolling(() => void refresh(), FALLBACK_POLL_MS);
  return { data, failed, loading, refresh, accept };
}
