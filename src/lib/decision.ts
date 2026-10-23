export type DecisionProvider = "laya" | "jev" | "openai";
export type EffortApi = "openai" | "adaptive";
export interface DecisionConfig {
  mode: "off" | "suggest" | "auto";
  provider: DecisionProvider;
  endpoint: string;
  model: string;
  targets: Record<string, EffortApi>;
}
export interface DecisionView { config: DecisionConfig; hasSecret: boolean }
/** Mirror of `decision/laya_local.rs::Status` (camelCase). */
export interface LayaLocalStatus {
  supported: boolean; phase: string; endpoint: string; logs: string[]; error: string | null;
  errorCode?: string | null; elapsedSeconds?: number; installed?: boolean; autostart?: boolean;
  mirror?: boolean; critical?: boolean; readyEndpoint?: string | null; servedEndpoints?: string[];
}
/** True only for this machine; an empty endpoint counts as not configured. Mirrors the backend's loopback rule. */
export function isLoopbackEndpoint(endpoint: string): boolean {
  try {
    const host = new URL(endpoint).hostname.toLowerCase().replace(/^\[|\]$/g, "");
    return host === "localhost" || host.endsWith(".localhost") || host === "::1" || /^127\.\d+\.\d+\.\d+$/.test(host);
  } catch { return false; }
}
export const decisionPresets = {
  laya: { endpoint: "http://127.0.0.1:8000/v1/systemone", model: "multilingual" },
  jev: { endpoint: "https://api.typesafe.ai/v1/systemone", model: "jev-latest" },
  openai: { endpoint: "", model: "" },
};
export function defaultDecision(): DecisionView {
  return { config: { mode: "off", provider: "laya", ...decisionPresets.laya, targets: {} }, hasSecret: false };
}
