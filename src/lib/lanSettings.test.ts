import { afterEach, expect, it, vi } from "vitest";
import { api } from "./bridge";
import { createLanWriter, type LanResult } from "./lanSettings";
import type { AppSettings } from "@/types";

afterEach(() => vi.restoreAllMocks());

it("shows authoritative LAN settings and each proxy after partial failure", async () => {
  vi.spyOn(api, "setLanExposure").mockRejectedValue(new Error("claude-cli restart failed"));
  vi.spyOn(api, "getSettings").mockResolvedValue({ lanExposure: true } as AppSettings);
  vi.spyOn(api, "getProxyState").mockImplementation(async (categoryId) => ({ categoryId, status: categoryId === "codex" ? "running" : "stopped", port: 12345 }));
  const publish = vi.fn();
  await createLanWriter()(true, publish);
  const result = publish.mock.calls[0][0] as LanResult;
  expect(result.settings?.lanExposure).toBe(true);
  expect(result.error).toContain("claude-cli restart failed");
  expect(result.proxies.find((state) => state.categoryId === "codex")?.status).toBe("running");
  expect(result.proxies.find((state) => state.categoryId === "claude-cli")?.status).toBe("stopped");
});

it("serializes changes including readback so old results cannot replace newer state", async () => {
  let release!: () => void;
  let saved = false;
  const order: string[] = [];
  vi.spyOn(api, "setLanExposure").mockImplementation(async (enabled) => {
    order.push(`set:${enabled}`);
    if (enabled) await new Promise<void>((resolve) => { release = resolve; });
    saved = enabled;
    return 1;
  });
  vi.spyOn(api, "getSettings").mockImplementation(async () => {
    order.push(`read:${saved}`);
    return { lanExposure: saved } as AppSettings;
  });
  vi.spyOn(api, "getProxyState").mockImplementation(async (categoryId) => ({ categoryId, status: "running", port: 12345 }));
  const writer = createLanWriter();
  const results: boolean[] = [];
  const publish = (result: LanResult) => { results.push(result.settings!.lanExposure); };
  const first = writer(true, publish);
  const second = writer(false, publish);
  await Promise.resolve();
  expect(order).toEqual(["set:true"]);
  release();
  await Promise.all([first, second]);
  expect(order).toEqual(["set:true", "read:true", "set:false", "read:false"]);
  expect(results).toEqual([true, false]);
});

it("reports unknown state when readback fails and recovers on the next write", async () => {
  vi.spyOn(api, "setLanExposure").mockRejectedValueOnce(new Error("partial failure")).mockResolvedValue(0);
  vi.spyOn(api, "getSettings").mockRejectedValueOnce(new Error("IPC unavailable")).mockResolvedValue({ lanExposure: false } as AppSettings);
  vi.spyOn(api, "getProxyState").mockImplementation(async (categoryId) => ({ categoryId, status: "stopped", port: null }));
  const publish = vi.fn();
  const writer = createLanWriter();
  await writer(true, publish);
  expect(publish.mock.calls[0][0].settings).toBeNull();
  expect(publish.mock.calls[0][0].error).toContain("IPC unavailable");
  await writer(false, publish);
  expect(publish.mock.calls[1][0].settings.lanExposure).toBe(false);
  expect(publish.mock.calls[1][0].error).toBeNull();
});
