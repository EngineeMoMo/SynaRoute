import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { ActiveRequestFeed, requestBytes, requestDuration, type ActiveRequest } from "../src/lib/activeRequests";
const read = (path: string) => readFileSync(new URL(`../${path}`, import.meta.url), "utf8");
const deferred = <T>() => { let resolve!: (value: T) => void; let reject!: (error: unknown) => void; const promise = new Promise<T>((ok, no) => { resolve = ok; reject = no; }); return { promise, resolve, reject }; };

describe("active proxy requests", () => {
  it("formats counters without implying that missing data is zero", () => {
    expect(requestDuration(62_999)).toBe("1:02"); expect(requestDuration(-3)).toBe("0:00");
    expect(requestBytes(0)).toBe("0 B"); expect(requestBytes(1536)).toBe("1.5 KiB");
    expect(requestBytes(2 * 1024 * 1024)).toBe("2.0 MiB");
  });
  it("serializes polling and accepts an authoritative empty snapshot", async () => {
    const feed = new ActiveRequestFeed(); const pending = deferred<ActiveRequest[]>();
    const calls: ActiveRequest[][] = []; let reads = 0;
    const first = feed.refresh(() => { reads++; return pending.promise; }, (rows) => calls.push(rows), () => {});
    await feed.refresh(() => { reads++; return Promise.resolve([]); }, () => {}, () => {});
    expect(reads).toBe(1); expect(calls).toEqual([]);
    pending.resolve([]); await first; expect(calls).toEqual([[]]);
  });
  it("rejects stale success and failure after switching category or unmounting", async () => {
    const feed = new ActiveRequestFeed(); const old = deferred<ActiveRequest[]>();
    const applied: string[] = [];
    const first = feed.refresh(() => old.promise, () => applied.push("old"), () => applied.push("old error"));
    feed.invalidate();
    await feed.refresh(() => Promise.resolve([]), () => applied.push("new"), () => {});
    old.reject(new Error("old IPC")); await first;
    expect(applied).toEqual(["new"]);
    const stale = deferred<ActiveRequest[]>();
    const second = feed.refresh(() => stale.promise, () => applied.push("stale"), () => {});
    feed.invalidate(); stale.resolve([]); await second;
    expect(applied).toEqual(["new"]);
  });
  it("reports refresh errors rather than erasing rows and can retry", async () => {
    const feed = new ActiveRequestFeed(); const applied: string[] = [];
    await feed.refresh(() => Promise.reject("offline"), () => applied.push("empty"), (error) => applied.push(String(error)));
    await feed.refresh(() => Promise.resolve([]), () => applied.push("recovered"), () => {});
    expect(applied).toEqual(["offline", "recovered"]);
  });
  it("wires commands, category-scoped mounting, confirmation and stopping state", () => {
    const bridge = read("src/lib/bridge.ts"); const registered = read("src-tauri/src/lib.rs");
    for (const command of ["list_active_requests", "stop_active_request"]) {
      expect(bridge).toContain(`"${command}"`);
      expect(registered.slice(registered.indexOf("generate_handler!["))).toContain(`active_requests::${command}`);
    }
    const component = read("src/components/ActiveRequestsPanel.tsx");
    expect(read("src/pages/CategoryPage.tsx")).toContain("<ActiveRequestsPanel key={activeCategory} category={activeCategory}");
    expect(component).toContain("usePolling(() => void refresh(), 1000)");
    expect(component).toContain("feed.current.invalidate()");
    expect(component).toContain("api.listActiveRequests(category)");
    expect(component).toContain("<ConfirmDialog");
    expect(component).toContain("onClick={() => setConfirm(row)}");
    expect(component.match(/api\.stopActiveRequest\(/g)).toHaveLength(1);
    expect(component).toContain("api.stopActiveRequest(category, id)");
    expect(component).toContain('row.phase === "stopping" || requested.has(row.requestId)');
    expect(component).toContain("loaded && !error && rows.length === 0");
    expect(component).toContain("if (!alive.current) return");
  });
  it("keeps the snapshot privacy-safe and preserves the command parser's visibility support", () => {
    const rust = read("src-tauri/src/proxy/active_requests.rs");
    const snapshot = rust.split("pub(crate) struct Snapshot {")[1].split("\n}")[0];
    for (const forbidden of ["base_url", "secret", "body", "client_ip", "session"]) expect(snapshot).not.toContain(forbidden);
    // Exercise the actual gate parser, not a second copy of its regex.
    const source = read("scripts/check-forbidden.mjs");
    const line = source.split("\n").find((line) => line.includes("const m = lines[j].match("));
    expect(line).toBeTruthy();
    const expression = line!.slice(line!.indexOf("match(") + 6, line!.lastIndexOf(");"));
    const re = new Function(`return ${expression}`)() as RegExp;
    for (const visibility of ["", "pub ", "pub(crate) ", "pub(super) "]) expect(`${visibility}fn list_active_requests(`.match(re)?.[1]).toBe("list_active_requests");
  });
});
