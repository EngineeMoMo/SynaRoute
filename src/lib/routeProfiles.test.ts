import { describe, expect, it } from "vitest";
import { profileHint, summarizeProfiles, type ProfileRow, type ProfileSnapshot } from "./routeProfiles";

const day = 20_000;
const base: ProfileRow = { day, categoryId: "codex", keyId: "k", requestedModel: "alias", realModel: "model", streaming: true, attempts: 10, successes: 8, rateLimits: 2, successLatencyMs: 800, lastSeen: day * 86_400_000 };
const snapshot = (rows: ProfileRow[]): ProfileSnapshot => ({ version: 1, since: 0, rows, dropped: 0, readOnly: false });

describe("passive route profiles", () => {
  it("sums UTC day buckets but never mixes requested models, actual models, categories or modes", () => {
    const rows = [base, { ...base, day: day - 1 }, { ...base, streaming: false }, { ...base, realModel: "different" }, { ...base, requestedModel: "different" }, { ...base, categoryId: "claude-cli" as const }];
    const result = summarizeProfiles(snapshot(rows), 7, day * 86_400_000);
    expect(result).toHaveLength(5);
    expect(result[0]).toMatchObject({ attempts: 20, successes: 16, rateLimits: 4, successLatencyMs: 1600 });
    expect(base.attempts).toBe(10);
  });
  it("excludes expired and future buckets, including at the UTC boundary", () => {
    const s = snapshot([base, { ...base, day: day - 6 }, { ...base, day: day - 7 }, { ...base, day: day + 1 }]);
    expect(summarizeProfiles(s, 7, day * 86_400_000)[0].attempts).toBe(20);
    expect(summarizeProfiles(s, 1, day * 86_400_000)[0].attempts).toBe(10);
  });
  it("does not recommend from tiny samples or infer answer quality", () => {
    expect(profileHint({ ...base, attempts: 19, successes: 0, rateLimits: 19 })).toBe("sample");
    expect(profileHint({ ...base, attempts: 25, successes: 20, rateLimits: 5 })).toBe("limits");
    expect(profileHint({ ...base, attempts: 25, successes: 18, rateLimits: 0 })).toBe("failures");
    expect(profileHint({ ...base, attempts: 25, successes: 25, rateLimits: 0 })).toBe("observed");
  });
});
