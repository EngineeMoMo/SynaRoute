import type { CategoryType } from "@/types";

export type RequestPhase = "routing" | "queued" | "waiting" | "receiving" | "stopping";
export interface ActiveRequest {
  requestId: string;
  categoryId: CategoryType;
  streaming: boolean;
  phase: RequestPhase;
  requestedModel: string;
  realModel: string | null;
  keyId: string | null;
  keyName: string | null;
  attempts: number;
  elapsedMs: number;
  attemptMs: number | null;
  bytesReceived: number;
  lastReadMs: number | null;
}
export type RequestStopResult = "requested" | "alreadyStopping" | "finished";

/** Serial snapshots; changing category/unmount invalidates all pending IPC results.
 * A failed refresh must never be presented as an authoritative empty list. */
export class ActiveRequestFeed {
  private generation = 0;
  private busy = false;
  invalidate() { this.generation += 1; this.busy = false; }
  async refresh(
    read: () => Promise<ActiveRequest[]>,
    apply: (rows: ActiveRequest[]) => void,
    fail: (error: unknown) => void,
  ) {
    if (this.busy) return;
    this.busy = true;
    const generation = this.generation;
    try {
      const rows = await read();
      if (generation === this.generation) apply(rows);
    } catch (error) {
      if (generation === this.generation) fail(error);
    } finally {
      if (generation === this.generation) this.busy = false;
    }
  }
}
export function requestDuration(ms: number): string {
  const seconds = Math.max(0, Math.floor(ms / 1000));
  return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, "0")}`;
}
export function requestBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KiB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MiB`;
}
