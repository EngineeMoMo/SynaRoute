import { api } from "@/lib/bridge";
import type { AppSettings, CategoryType, ProxyState } from "@/types";

export interface LanResult {
  settings: AppSettings | null;
  proxies: ProxyState[];
  restarted: number;
  error: string | null;
}

export function createLanWriter() {
  let pending = Promise.resolve();
  return (enabled: boolean, publish: (result: LanResult) => void): Promise<void> => {
    const operation = pending.then(async () => {
      let restarted = 0;
      const errors: string[] = [];
      try { restarted = await api.setLanExposure(enabled); }
      catch (error) { errors.push(String((error as Error)?.message ?? error)); }
      const categories: CategoryType[] = ["claude-cli", "claude-desktop", "codex"];
      const [settings, ...states] = await Promise.allSettled([
        api.getSettings(), ...categories.map((category) => api.getProxyState(category)),
      ]);
      for (const read of [settings, ...states]) {
        if (read.status === "rejected") errors.push(String(read.reason));
      }
      publish({
        settings: settings.status === "fulfilled" ? settings.value as AppSettings : null,
        proxies: states.flatMap((state) => state.status === "fulfilled" ? [state.value as ProxyState] : []),
        restarted, error: errors.length ? errors.join("; ") : null,
      });
    });
    pending = operation.catch(() => undefined);
    return operation;
  };
}

export const writeLanSettings = createLanWriter();
