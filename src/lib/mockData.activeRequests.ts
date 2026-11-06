import type { ActiveRequest, RequestStopResult } from "./activeRequests";
import type { CategoryType } from "@/types";

/** Dev-only fixture, opt-in with ?activeRequests=1. Vite replaces mockData in production. */
export function createActiveRequestsMock() {
  const enabled = typeof window !== "undefined" && new URLSearchParams(window.location.search).has("activeRequests");
  const stopped = new Set<string>();
  const started = Date.now();
  return {
    listActiveRequests(categoryId: CategoryType): ActiveRequest[] {
      if (!enabled) return [];
      const elapsed = Date.now() - started;
      return (["receiving", "queued"] as const).map((phase, index): ActiveRequest => ({
        requestId: `preview-${categoryId}-${index}`, categoryId, phase, streaming: index === 0,
        requestedModel: "claude-opus-5", realModel: index === 0 ? "glm-5" : "claude-opus-5",
        keyId: "preview-key", keyName: "Preview Key", attempts: index + 1,
        elapsedMs: 42_000 + elapsed, attemptMs: 12_000 + elapsed,
        bytesReceived: index === 0 ? 8192 : 0, lastReadMs: index === 0 ? 1200 : null,
      })).filter((row) => !stopped.has(row.requestId));
    },
    stopActiveRequest(categoryId: CategoryType, requestId: string): RequestStopResult {
      if (!this.listActiveRequests(categoryId).some((row) => row.requestId === requestId)) return "finished";
      stopped.add(requestId);
      return "requested";
    },
  };
}
