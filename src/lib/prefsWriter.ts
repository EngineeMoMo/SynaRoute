import { api } from "@/lib/bridge";
import { pickPrefs, type UserPrefs } from "@/lib/prefs";
import type { AppSettings } from "@/types";

let pending: Promise<void> = Promise.resolve();

export function persistPreferences(patch: Partial<UserPrefs>): Promise<AppSettings> {
  const changes = { ...patch };
  const operation = pending.then(async () => {
    const latest = await api.getSettings();
    await api.saveSettings(pickPrefs({ ...latest, ...changes }));
    return api.getSettings();
  });
  pending = operation.then(() => undefined, () => undefined);
  return operation;
}
