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
export const decisionPresets = {
  laya: { endpoint: "http://127.0.0.1:8000/v1/systemone", model: "multilingual" },
  jev: { endpoint: "https://api.typesafe.ai/v1/systemone", model: "jev-latest" },
  openai: { endpoint: "", model: "" },
};
export function defaultDecision(): DecisionView {
  return { config: { mode: "off", provider: "laya", ...decisionPresets.laya, targets: {} }, hasSecret: false };
}
