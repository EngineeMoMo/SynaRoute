import type { AppSettings, CategoryType, ProviderKey, ProxyState } from "@/types";
import type { RoutingOverview } from "./routingOverview";

const observedAt = Date.now();
export function mockRoutingOverview(keys: Record<CategoryType, ProviderKey[]>, proxies: Record<CategoryType, ProxyState>, settings: AppSettings): RoutingOverview {
  const categories: CategoryType[] = ["claude-cli", "claude-desktop", "codex"];
  return {
    capturedAt: Date.now(),
    vaultLocked: false,
    categories: categories.map((categoryId, index) => {
      const all = keys[categoryId];
      const enabled = all.filter((k) => k.enabled).sort((a, b) => a.priority - b.priority);
      const key = enabled[1] ?? enabled[0];
      return {
        categoryId,
        running: proxies[categoryId].status === "running",
        endpoint: `http://127.0.0.1:${proxies[categoryId].port ?? settings.proxyPorts?.[categoryId] ?? ({ "claude-cli": 47100, codex: 47101, "claude-desktop": 47102 }[categoryId])}`,
        keyCount: all.length,
        enabledCount: enabled.length,
        primaryKey: enabled[0]?.name ?? null,
        configuredModel: settings.activeModels?.[categoryId] || null,
        configuredEffort: settings.activeEfforts?.[categoryId] || null,
        connection: {
          state: index === 1 ? "not_configured" : "configured",
          reason: index === 1 ? "desktop_not_selected" : "matches",
          clientModel: categoryId === "codex" ? "gpt-5" : categoryId === "claude-cli" ? "claude-sonnet-4-5" : null,
        },
        latest: key && index !== 1 ? {
          categoryId,
          keyId: key.id,
          keyName: key.name,
          requestedModel: categoryId === "codex" ? "gpt-5" : "claude-sonnet-4-5",
          realModel: key.models[0]?.realName ?? "glm-4.6",
          streaming: true,
          ok: categoryId !== "codex",
          status: categoryId === "codex" ? 429 : 200,
          latencyMs: categoryId === "codex" ? 840 : 1230,
          observedAt,
        } : null,
      };
    }),
  };
}
