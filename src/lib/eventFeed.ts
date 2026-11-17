import type { EventLogEntry } from "@/types";

export interface EventCursor { epoch: string; revision: number }
export interface EventDelta {
  cursor: EventCursor;
  reset: boolean;
  entries: EventLogEntry[];
  removed: string[];
}

export function mergeEventDelta(previous: EventLogEntry[], delta: EventDelta): EventLogEntry[] {
  if (delta.reset) return delta.entries;
  if (delta.entries.length === 0 && delta.removed.length === 0) return previous;
  const updates = new Map(delta.entries.map((entry) => [entry.id, entry]));
  const removed = new Set(delta.removed);
  const next: EventLogEntry[] = [];
  for (const entry of previous) {
    if (removed.has(entry.id)) continue;
    next.push(updates.get(entry.id) ?? entry);
    updates.delete(entry.id);
  }
  for (const entry of updates.values()) next.push(entry);
  return next;
}
