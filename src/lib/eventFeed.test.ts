import { describe, expect, it } from "vitest";
import { mergeEventDelta, type EventDelta } from "./eventFeed";
import type { EventLogEntry } from "@/types";

const row = (id: string): EventLogEntry => ({ id, ts: 1, categoryId: "codex", type: "route", detail: id });
const delta = (patch: Partial<EventDelta>): EventDelta => ({ cursor: { epoch: "test", revision: 2 }, reset: false, entries: [], removed: [], ...patch });

describe("incremental event feed", () => {
  it("preserves unchanged objects while applying collapse, usage, append and eviction", () => {
    const previous = [row("expired"), row("folded"), row("unchanged")];
    const changed = { ...previous[1], repeat: 3, usage: { input: 5, output: 2, cacheRead: 0 } };
    const result = mergeEventDelta(previous, delta({ entries: [changed, row("new")], removed: ["expired"] }));
    expect(result.map((entry) => entry.id)).toEqual(["folded", "unchanged", "new"]);
    expect(result[0].repeat).toBe(3);
    expect(result[0].usage?.input).toBe(5);
    expect(result[1]).toBe(previous[2]);
  });
  it("keeps an unchanged array and resets after restart or a stale cursor", () => {
    const previous = [row("old")];
    expect(mergeEventDelta(previous, delta({}))).toBe(previous);
    expect(mergeEventDelta(previous, delta({ reset: true, entries: [row("new")] })).map((entry) => entry.id)).toEqual(["new"]);
  });
});
