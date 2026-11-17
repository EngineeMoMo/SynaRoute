import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useStore } from "./store";
import { api } from "./lib/bridge";
import type { ProviderKey } from "./types";

beforeEach(() => {
  useStore.setState({ activeCategory: "codex", keys: [], loading: false });
  vi.spyOn(api, "getProxyState").mockResolvedValue({ categoryId: "codex", status: "stopped", port: null });
  vi.spyOn(api, "getBrainConfig").mockResolvedValue({ categoryId: "codex" } as Awaited<ReturnType<typeof api.getBrainConfig>>);
  vi.spyOn(api, "listEventDelta").mockResolvedValue({ cursor: { epoch: "test", revision: 0 }, reset: true, entries: [], removed: [] });
});
afterEach(() => vi.restoreAllMocks());

describe("category refresh ordering", () => {
  it("a full load and event refresh cannot overwrite a newer key mutation", async () => {
    let resolveOld!: (keys: ProviderKey[]) => void;
    const key = { id: "key", enabled: false } as ProviderKey;
    const reads = vi.spyOn(api, "listKeys")
      .mockImplementationOnce(() => new Promise((resolve) => { resolveOld = resolve; }))
      .mockResolvedValue([key]);
    const first = useStore.getState().loadCategory("codex");
    await Promise.resolve();
    const latest = useStore.getState().refreshCategory();
    resolveOld([{ ...key, enabled: true }]);
    await Promise.all([first, latest]);
    expect(reads).toHaveBeenCalledTimes(2);
    expect(useStore.getState().keys[0].enabled).toBe(false);
    expect(useStore.getState().loading).toBe(false);
    expect(api.getBrainConfig).toHaveBeenCalledTimes(2);
  });

  it("switching A to B to A invalidates the first A snapshot", async () => {
    let resolveOld!: (keys: ProviderKey[]) => void;
    const key = { id: "fresh", enabled: true } as ProviderKey;
    const reads = vi.spyOn(api, "listKeys")
      .mockImplementationOnce(() => new Promise((resolve) => { resolveOld = resolve; }))
      .mockResolvedValue([key]);
    const first = useStore.getState().loadCategory("codex");
    await Promise.resolve();
    useStore.getState().setActiveCategory("claude-cli");
    useStore.getState().setActiveCategory("codex");
    resolveOld([{ ...key, id: "stale" }]);
    await first;
    expect(reads).toHaveBeenCalledTimes(2);
    expect(useStore.getState().keys[0].id).toBe("fresh");
    expect(useStore.getState().loading).toBe(false);
  });
});
