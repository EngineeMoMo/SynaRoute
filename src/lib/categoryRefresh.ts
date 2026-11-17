import type { BrainConfig, CategoryType, ProviderKey, ProxyState } from "@/types";
import { api } from "./bridge";
import { createRefreshQueue } from "./refreshQueue";
import { reuseUnchanged } from "./reuseUnchanged";

interface CategoryState {
  activeCategory: CategoryType;
  keys: ProviderKey[];
  proxy: ProxyState | null;
  brain: BrainConfig | null;
  loading: boolean;
}

export function createCategoryRefresh(
  get: () => CategoryState,
  set: (update: Partial<CategoryState>) => void,
) {
  return createRefreshQueue(
    async (request: { category: CategoryType; full: boolean }) => {
      const results = await Promise.allSettled([
        api.listKeys(request.category),
        api.getProxyState(request.category),
        request.full ? api.getBrainConfig(request.category) : Promise.resolve(undefined),
      ]);
      for (const result of results) {
        if (result.status === "rejected") console.error("category refresh failed", result.reason);
      }
      return results;
    },
    ([keys, proxy, brain], request) => {
      const state = get();
      if (state.activeCategory !== request.category) return;
      set({
        keys: keys.status === "fulfilled" ? reuseUnchanged(state.keys, keys.value) : state.keys,
        proxy: proxy.status === "fulfilled" ? proxy.value : state.proxy,
        brain: brain.status === "fulfilled" && brain.value !== undefined ? brain.value : state.brain,
        ...(request.full ? { loading: false } : {}),
      });
    },
    (previous, next) => ({ ...next, full: next.full || (previous.category === next.category && previous.full) }),
  );
}
