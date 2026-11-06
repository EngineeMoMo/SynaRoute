import { defaultRoutingPolicy, ROUTING_CATEGORIES, type CategoryPolicy, type RoutingPolicyView, type RoutingScene, type RoutingSyncResult, type ScenePreview, type SensitiveScan } from "./routingPolicies";
import { discoverableModels } from "./modelSets";
import type { CategoryType, ProviderKey } from "@/types";

// Browser-only fixtures. Never imported by the production bridge: loaded through mockData's alias.
export function createRoutingMock(readKeys: (category: CategoryType) => ProviderKey[]) {
  let revision = 0;
  const policies = new Map<CategoryType, CategoryPolicy>();
  const terms = new Map<CategoryType, string[]>();
  let scenes: RoutingScene[] = [];
  const selections = new Map<CategoryType, RoutingScene["routes"][number]>();
  const previews = new Map<string, { scene: RoutingScene; revision: number }>();
  const check = (expected: number) => { if (expected !== revision) throw new Error("Configuration changed; refresh first."); };
  const view = (): RoutingPolicyView => ({ revision, vaultLocked: false, scenes: structuredClone(scenes), categories: ROUTING_CATEGORIES.map((categoryId) => {
    const keys = readKeys(categoryId);
    return { categoryId, policy: structuredClone(policies.get(categoryId) ?? defaultRoutingPolicy()), keys: keys.map((k) => ({ id: k.id, name: k.name, enabled: k.enabled, models: k.models.map((m) => m.realName) })), models: discoverableModels(keys.filter((k) => k.enabled)), current: selections.get(categoryId) ?? { categoryId, primaryKeyId: keys.find((k) => k.enabled)?.id ?? null, model: null, effort: null }, issues: [], activity: null };
  }) });
  const sync = (categories: CategoryType[]): RoutingSyncResult[] => categories.map((categoryId) => ({ categoryId, status: "stopped", message: "Browser preview: no client files changed.", backupDir: null }));
  return {
    routingPolicies: view,
    saveRoutingPolicy(category: CategoryType, policy: CategoryPolicy, nextTerms: string[] | null, expected: number) {
      check(expected);
      const next = structuredClone(policy);
      if (nextTerms !== null) { terms.set(category, nextTerms); next.privacy.termCount = nextTerms.length; next.privacy.termsSecret = nextTerms.length ? "__routing_terms_preview" : null; }
      policies.set(category, next); revision++;
      return { view: view(), sync: sync([category]) };
    },
    saveRoutingScene(scene: RoutingScene, expected: number) {
      check(expected);
      const next = structuredClone({ ...scene, id: scene.id || crypto.randomUUID() });
      scenes = [...scenes.filter((s) => s.id !== next.id), next]; revision++;
      return view();
    },
    deleteRoutingScene(id: string, expected: number) { check(expected); scenes = scenes.filter((s) => s.id !== id); revision++; return view(); },
    previewRoutingScene(id: string): ScenePreview {
      const scene = scenes.find((s) => s.id === id);
      if (!scene) throw new Error("Scene not found");
      const token = crypto.randomUUID(); previews.set(token, { scene: structuredClone(scene), revision });
      return { token, name: scene.name, expiresInSeconds: 600, changes: scene.routes.map((after) => ({ before: view().categories.find((r) => r.categoryId === after.categoryId)!.current, after, client: sync([after.categoryId])[0], paths: [] })) };
    },
    applyRoutingScene(token: string) {
      const preview = previews.get(token); previews.delete(token);
      if (!preview || preview.revision !== revision) throw new Error("Preview expired; preview again.");
      let changed = false;
      for (const route of preview.scene.routes) { const before = view().categories.find((r) => r.categoryId === route.categoryId)!.current; const after = { ...route, primaryKeyId: route.primaryKeyId ?? before.primaryKeyId }; changed ||= JSON.stringify(before) !== JSON.stringify(after); selections.set(route.categoryId, after); }
      return { view: view(), sync: sync(preview.scene.routes.map((r) => r.categoryId)), changed, backupPath: null };
    },
    syncRoutingClients: sync,
    clearRoutingSessions: (_category: CategoryType) => undefined,
    previewSensitiveContent(category: CategoryType, text: string, config: { credentials: boolean; emails: boolean; phones: boolean }): SensitiveScan {
      const patterns: [string, RegExp][] = [];
      if (config.credentials) patterns.push(["api_key", /\bsk-(?:ant-|proj-)?[A-Za-z0-9_-]{20,}\b/g]);
      if (config.emails) patterns.push(["email", /[\w.+-]+@[\w.-]+\.[A-Za-z]{2,}/g]);
      if (config.phones) patterns.push(["phone", /\b1[3-9][0-9]{9}\b/g]);
      const counts: Record<string, number> = {};
      let redacted = text;
      for (const [kind, re] of patterns) redacted = redacted.replace(re, () => { counts[kind] = (counts[kind] ?? 0) + 1; return "[REDACTED]"; });
      for (const term of terms.get(category) ?? []) { const parts = redacted.split(term); if (parts.length > 1) { counts.custom = (counts.custom ?? 0) + parts.length - 1; redacted = parts.join("[REDACTED]"); } }
      return { counts, findings: [], redacted, limited: false };
    },
  };
}
