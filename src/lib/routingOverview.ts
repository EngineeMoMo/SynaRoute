import type { CategoryType } from "@/types";

export type ConnectionState = "configured" | "not_configured" | "needs_attention" | "unknown";
export interface ConnectionCheck {
  state: ConnectionState;
  reason: string;
  clientModel: string | null;
}
export interface LatestAttempt {
  categoryId: CategoryType;
  keyId: string;
  keyName: string;
  requestedModel: string;
  realModel: string;
  streaming: boolean;
  ok: boolean;
  status: number | null;
  latencyMs: number;
  observedAt: number;
}
export interface CategoryOverview {
  categoryId: CategoryType;
  running: boolean;
  endpoint: string;
  keyCount: number;
  enabledCount: number;
  primaryKey: string | null;
  configuredModel: string | null;
  configuredEffort: string | null;
  connection: ConnectionCheck;
  latest: LatestAttempt | null;
}
export interface RoutingOverview {
  capturedAt: number;
  vaultLocked: boolean;
  categories: CategoryOverview[];
}

export function createOverviewRefresher(
  read: () => Promise<RoutingOverview>,
  update: (data: RoutingOverview | null, failed: boolean) => void,
  loading: (value: boolean) => void,
  visible: () => boolean,
) {
  let active = true;
  let inFlight = false;
  let queued = false;
  let timer: ReturnType<typeof setTimeout> | undefined;

  function schedule() {
    if (!active || !visible() || timer !== undefined) return;
    timer = setTimeout(() => {
      timer = undefined;
      void refresh();
    }, 500);
  }

  async function refresh() {
    if (!active || !visible()) return;
    if (inFlight) {
      queued = true;
      return;
    }
    clearTimeout(timer);
    timer = undefined;
    inFlight = true;
    loading(true);
    try {
      const next = await read();
      if (active) update(next, false);
    } catch {
      if (active) update(null, true);
    } finally {
      inFlight = false;
      if (active) {
        loading(false);
        if (queued) {
          queued = false;
          schedule();
        }
      }
    }
  }

  return {
    refresh,
    schedule,
    dispose() {
      active = false;
      queued = false;
      clearTimeout(timer);
      timer = undefined;
    },
  };
}

export function attemptLabel(attempt: LatestAttempt): "failed" | "streamEstablished" | "responseReceived" {
  if (!attempt.ok) return "failed";
  return attempt.streaming ? "streamEstablished" : "responseReceived";
}

export function connectionReason(reason: string): string {
  const known = ["matches", "missing", "not_pointing", "endpoint_mismatch", "credentials_missing", "profile_override", "legacy_profile", "catalog_missing", "placeholder_armed", "provider_missing", "provider_mismatch", "desktop_not_selected", "deployment_mode", "profile_missing", "unreadable", "invalid_json", "invalid_toml", "path_unavailable"];
  return `overview.reason.${known.includes(reason) ? reason : "unknown"}`;
}
