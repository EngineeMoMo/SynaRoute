import type { CategoryType } from "@/types";

export type AffinityMode = "off" | "turn" | "session";
export type PrivacyMode = "off" | "warn" | "block";
export interface PrivacyConfig {
  mode: PrivacyMode;
  credentials: boolean;
  emails: boolean;
  phones: boolean;
  termsSecret: string | null;
  termCount: number;
}
export interface IntentRule { id: string; intent: string; model: string }
export interface RouteGroup {
  id: string;
  name: string;
  enabled: boolean;
  defaultModel: string;
  judge: { keyId: string; model: string } | null;
  rules: IntentRule[];
}
export interface CategoryPolicy {
  affinity: AffinityMode;
  privacy: PrivacyConfig;
  groups: RouteGroup[];
}
export interface SceneRoute {
  categoryId: CategoryType;
  primaryKeyId: string | null;
  model: string | null;
  effort: string | null;
}
export interface RoutingScene { id: string; name: string; routes: SceneRoute[] }
export interface RoutingActivity {
  categoryId: CategoryType | null;
  observedAt: number;
  identity: string;
  selection: string;
  selectedModel: string | null;
  keptKey: string | null;
  privacy: string;
  findings: Record<string, number>;
  blocked: number;
  checked: number;
}
export interface RoutingKeyOption { id: string; name: string; enabled: boolean; models: string[] }
export interface RoutingCategoryView {
  categoryId: CategoryType;
  policy: CategoryPolicy;
  keys: RoutingKeyOption[];
  models: string[];
  current: SceneRoute;
  issues: string[];
  activity: RoutingActivity | null;
}
export interface RoutingPolicyView {
  revision: number;
  categories: RoutingCategoryView[];
  scenes: RoutingScene[];
  vaultLocked: boolean;
}
export interface RoutingSyncResult {
  categoryId: CategoryType;
  status: string;
  message: string;
  backupDir: string | null;
}
export interface RoutingSaveResult { view: RoutingPolicyView; sync: RoutingSyncResult[] }
export interface ScenePreview {
  token: string;
  name: string;
  expiresInSeconds: number;
  changes: { before: SceneRoute; after: SceneRoute; client: RoutingSyncResult; paths: string[] }[];
}
export interface SceneApplied extends RoutingSaveResult { changed: boolean; backupPath: string | null }
export interface SensitiveScan {
  counts: Record<string, number>;
  findings: { kind: string; start: number; end: number }[];
  redacted: string;
  limited: boolean;
}

export const ROUTING_CATEGORIES: CategoryType[] = ["claude-cli", "claude-desktop", "codex"];
export const ROUTING_EFFORTS = ["minimal", "low", "medium", "high", "xhigh", "max", "ultra"] as const;
export function defaultRoutingPolicy(): CategoryPolicy {
  return { affinity: "off", privacy: { mode: "off", credentials: true, emails: false, phones: false, termsSecret: null, termCount: 0 }, groups: [] };
}
export function groupModelId(group: RouteGroup): string { return `claude-synaroute-auto-${group.id}`; }
export function routingChoices(row: RoutingCategoryView): { value: string; label: string }[] {
  return [
    ...row.models.map((model) => ({ value: model, label: model })),
    ...row.policy.groups.filter((group) => group.enabled).map((group) => ({ value: groupModelId(group), label: `${group.name} · Auto` })),
  ];
}
export function sceneMatches(scene: RoutingScene, rows: RoutingCategoryView[]): boolean {
  return scene.routes.length > 0 && scene.routes.every((route) => {
    const current = rows.find((row) => row.categoryId === route.categoryId)?.current;
    return current !== undefined && (route.primaryKeyId === null || route.primaryKeyId === current.primaryKeyId)
      && (route.model || null) === (current.model || null) && (route.effort || null) === (current.effort || null);
  });
}
export function samePolicy(a: CategoryPolicy, b: CategoryPolicy): boolean { return JSON.stringify(a) === JSON.stringify(b); }
export function parseSensitiveTerms(text: string): string[] { return [...new Set(text.split(/\r?\n/).map((line) => line.trim()).filter(Boolean))]; }
export function reorderRule(rules: IntentRule[], index: number, step: -1 | 1): IntentRule[] {
  const target = index + step;
  if (target < 0 || target >= rules.length || index < 0 || index >= rules.length) return rules;
  const next = [...rules];
  [next[index], next[target]] = [next[target], next[index]];
  return next;
}
export function activityKey(kind: "identity" | "selection" | "privacy", value: string): string {
  const choices = {
    identity: ["explicit", "native", "unidentified"],
    selection: ["manual", "default", "identity_missing", "session_unavailable", "turn_reused", "opaque_history", "turn_not_seen", "no_text", "judge_unavailable", "judge_cooldown", "judge_timeout", "judge_busy", "judge_transport", "judge_locked", "judge_http", "judge_oversized", "invalid_answer", "cached", "classified", "target_unavailable", "no_match"],
    privacy: ["off", "checked", "partial", "warned", "blocked", "scan_failed"],
  };
  return `routing.${kind}.${choices[kind].includes(value) ? value : "unknown"}`;
}
