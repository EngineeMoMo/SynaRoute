import { afterEach, describe, expect, it, vi } from "vitest";
import { api } from "../src/lib/bridge";
import { persistPreferences } from "../src/lib/prefsWriter";
import { useStore } from "../src/store";
import type { AppSettings } from "../src/types";

afterEach(() => vi.restoreAllMocks());

function settings() {
  return {
    requestLogEnabled: false,
    healthProbeRealCompletion: false,
    language: "zh",
    theme: "system",
    autoStart: true,
  } as AppSettings;
}

describe("shared preference writes", () => {
  it("serializes delayed settings-page and language writes without losing fields", async () => {
    let stored = settings();
    let release = () => {};
    const blocked = new Promise<void>((resolve) => { release = resolve; });
    const read = vi.spyOn(api, "getSettings").mockImplementation(async () => ({ ...stored }));
    const save = vi.spyOn(api, "saveSettings").mockImplementation(async (prefs) => {
      await blocked;
      stored = { ...stored, ...prefs };
    });
    const first = persistPreferences({ requestLogEnabled: true });
    useStore.getState().setLang("en");
    const last = persistPreferences({ healthProbeRealCompletion: true });
    await vi.waitFor(() => expect(save).toHaveBeenCalledTimes(1));
    expect(read).toHaveBeenCalledTimes(1);
    release();
    await Promise.all([first, last]);
    expect(stored).toMatchObject({
      requestLogEnabled: true,
      healthProbeRealCompletion: true,
      language: "en",
      autoStart: true,
    });
    expect(save).toHaveBeenCalledTimes(3);
    expect(save.mock.calls.every(([prefs]) => !("autoStart" in prefs))).toBe(true);
  });

  it("does not poison the queue or replay a failed patch", async () => {
    let stored = settings();
    vi.spyOn(api, "getSettings").mockImplementation(async () => ({ ...stored }));
    vi.spyOn(api, "saveSettings")
      .mockRejectedValueOnce(new Error("fixture failure"))
      .mockImplementation(async (prefs) => { stored = { ...stored, ...prefs }; });
    const failed = persistPreferences({ requestLogEnabled: true });
    const successful = persistPreferences({ healthProbeRealCompletion: true });
    await expect(failed).rejects.toThrow("fixture failure");
    await successful;
    expect(stored.requestLogEnabled).toBe(false);
    expect(stored.healthProbeRealCompletion).toBe(true);
  });

  it("applies the most recently queued value for the same setting", async () => {
    let stored = settings();
    vi.spyOn(api, "getSettings").mockImplementation(async () => ({ ...stored }));
    vi.spyOn(api, "saveSettings").mockImplementation(async (prefs) => { stored = { ...stored, ...prefs }; });
    await Promise.all([
      persistPreferences({ requestLogEnabled: true }),
      persistPreferences({ requestLogEnabled: false }),
    ]);
    expect(stored.requestLogEnabled).toBe(false);
  });
});
