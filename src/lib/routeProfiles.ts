import type { CategoryType } from "@/types";

export interface ProfileRow {
  day: number;
  categoryId: CategoryType;
  keyId: string;
  requestedModel: string;
  realModel: string;
  streaming: boolean;
  attempts: number;
  successes: number;
  rateLimits: number;
  successLatencyMs: number;
  lastSeen: number;
}
export interface ProfileSnapshot {
  version: number;
  since: number;
  rows: ProfileRow[];
  dropped: number;
  readOnly: boolean;
}

/** UTC day windows; grouping never mixes aliases, actual models or stream modes. */
export function summarizeProfiles(snapshot: ProfileSnapshot, days: number, now = Date.now()): ProfileRow[] {
  const today = Math.floor(now / 86_400_000);
  const groups = new Map<string, ProfileRow>();
  for (const row of snapshot.rows) {
    if (row.day < today - days + 1 || row.day > today) continue;
    const id = JSON.stringify([row.categoryId, row.keyId, row.requestedModel, row.realModel, row.streaming]);
    const group = groups.get(id);
    if (!group) groups.set(id, { ...row });
    else {
      group.attempts += row.attempts;
      group.successes += row.successes;
      group.rateLimits += row.rateLimits;
      group.successLatencyMs += row.successLatencyMs;
      group.lastSeen = Math.max(group.lastSeen, row.lastSeen);
    }
  }
  return [...groups.values()].sort((a, b) => b.attempts - a.attempts || b.lastSeen - a.lastSeen);
}

/** A diagnostic hint, not an automatic ranking or a statistical quality claim. */
export function profileHint(row: ProfileRow): "sample" | "limits" | "failures" | "observed" {
  if (row.attempts < 20) return "sample";
  if (row.rateLimits >= 5 && row.rateLimits / row.attempts >= 0.2) return "limits";
  if (row.attempts - row.successes >= 5 && row.successes / row.attempts < 0.8) return "failures";
  return "observed";
}
