import { useRef, useState, type Dispatch, type SetStateAction } from "react";
import { useStore } from "@/store";
import type { AppSettings } from "@/types";
import { writeLanSettings } from "./lanSettings";
import { useT } from "./useT";

export function useLanSettings(setSettings: Dispatch<SetStateAction<AppSettings | null>>) {
  const [busy, setBusy] = useState(false);
  const [unknown, setUnknown] = useState(false);
  const [status, setStatus] = useState<string | null>(null);
  const revision = useRef(0);
  const t = useT();
  const toggle = (enabled: boolean) => {
    const current = ++revision.current;
    setBusy(true);
    void writeLanSettings(enabled, (result) => {
      if (current !== revision.current) return;
      setUnknown(!result.settings);
      if (result.settings) {
        const actual = result.settings;
        setSettings((settings) => settings ? { ...settings, lanExposure: actual.lanExposure, proxyPorts: actual.proxyPorts } : actual);
        useStore.setState((state) => ({ settings: state.settings ? { ...state.settings, lanExposure: actual.lanExposure, proxyPorts: actual.proxyPorts } : actual }));
      }
      const proxy = result.proxies.find((state) => state.categoryId === useStore.getState().activeCategory);
      if (proxy) useStore.setState({ proxy });
      if (result.error) {
        const states = result.proxies.map((state) => `${state.categoryId}: ${t(state.status === "running" ? "settings.lanRunning" : "settings.lanStopped")}`).join(" · ");
        const message = `${t(result.settings ? "settings.lanPartial" : "settings.lanUnknown")} ${states} ${result.error}`;
        setStatus(message);
        useStore.getState().showToast("error", message);
      } else {
        setStatus(null);
        if (result.restarted > 0) useStore.getState().showToast("success", t("settings.lanRestarted", { n: result.restarted }));
      }
    }).finally(() => { if (current === revision.current) setBusy(false); });
  };
  return { toggle, busy, unknown, status };
}
