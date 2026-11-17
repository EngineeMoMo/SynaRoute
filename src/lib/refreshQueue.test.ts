import { describe, expect, it, vi } from "vitest";
import { createRefreshQueue } from "./refreshQueue";

function deferred<Value>() {
  let resolve!: (value: Value) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<Value>((accept, fail) => { resolve = accept; reject = fail; });
  return { promise, resolve, reject };
}

describe("refresh queue", () => {
  it("coalesces pending work and rejects snapshots invalidated during a read", async () => {
    const older = deferred<string>();
    const read = vi.fn().mockReturnValueOnce(older.promise).mockResolvedValue("new");
    const apply = vi.fn();
    const refresh = createRefreshQueue(read, apply);
    const completion = refresh("old");
    await Promise.resolve();
    for (let index = 0; index < 20; index++) void refresh("latest");
    expect(read).toHaveBeenCalledTimes(1);
    older.resolve("stale");
    await completion;
    expect(read).toHaveBeenCalledTimes(2);
    expect(read).toHaveBeenLastCalledWith("latest");
    expect(apply).toHaveBeenCalledExactlyOnceWith("new", "latest");
  });

  it("recovers after failures and still performs pending invalidations", async () => {
    const older = deferred<string>();
    const read = vi.fn().mockReturnValueOnce(older.promise).mockResolvedValue("fresh");
    const apply = vi.fn();
    const refresh = createRefreshQueue(read, apply);
    const completion = refresh(true);
    await Promise.resolve();
    void refresh(true);
    older.reject(new Error("obsolete read"));
    await completion;
    expect(apply).toHaveBeenCalledExactlyOnceWith("fresh", true);
    read.mockRejectedValueOnce(new Error("offline"));
    await expect(refresh(true)).rejects.toThrow("offline");
    await refresh(true);
    expect(apply).toHaveBeenCalledTimes(2);
  });
});
