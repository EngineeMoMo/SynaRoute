// 全局状态管理（Zustand）
import { create } from "zustand";
import type {
  AppSettings,
  BrainConfig,
  CategoryType,
  EventLogEntry,
  ProviderKey,
  ProxyState,
  ThemePref,
  UpdateCheckResult,
  Vendor,
} from "@/types";
import type { Lang } from "@/lib/i18n";
import { api } from "@/lib/bridge";

interface AppState {
  // 当前选中分类
  activeCategory: CategoryType;
  setActiveCategory: (c: CategoryType) => void;

  // 数据
  keys: ProviderKey[];
  proxy: ProxyState | null;
  brain: BrainConfig | null;
  events: EventLogEntry[];
  settings: AppSettings | null;
  vendors: Vendor[];
  loading: boolean;

  // 在线更新（侧栏徽章 / 设置页共用）
  updateCheck: UpdateCheckResult | null;
  updateChecking: boolean;
  checkForUpdates: (opts?: { silent?: boolean }) => Promise<UpdateCheckResult | null>;
  clearUpdateAvailable: () => void;

  // 主题（副本，便于即时切换）
  theme: ThemePref;
  setTheme: (t: ThemePref) => void;

  // 界面语言（副本，便于即时切换）
  lang: Lang;
  setLang: (l: Lang) => void;

  // 全局轻提示（落盘失败等错误必须可见，禁止静默吞掉）
  toast: { kind: "error" | "success"; msg: string } | null;
  showToast: (kind: "error" | "success", msg: string) => void;
  clearToast: () => void;

  // 启动代理后自动写入配置的弹窗提示
  configAppliedCategory: CategoryType | null;
  clearConfigApplied: () => void;

  // 动作
  loadCategory: (c: CategoryType) => Promise<void>;
  refreshCategory: () => Promise<void>;
  refreshEvents: () => Promise<void>;
  loadSettings: () => Promise<void>;
  loadVendors: () => Promise<void>;
  toggleKey: (keyId: string, enabled: boolean) => Promise<void>;
  moveKey: (keyId: string, direction: "up" | "down") => Promise<void>;
  setPrimaryKey: (keyId: string) => Promise<void>;
  deleteKey: (keyId: string) => Promise<void>;
  checkHealth: (keyId: string) => Promise<void>;
  startProxy: () => Promise<void>;
  stopProxy: () => Promise<void>;
  // 应用内「对外模型名」选择（借鉴 EchoBird）：客户端菜单拉不到中转模型时在应用内选，
  // 代理转发时覆盖客户端发来的模型名。走后端专用命令直写，即时生效、免重启客户端。
  setActiveModel: (category: CategoryType, model: string) => Promise<void>;
  setActiveEffort: (category: CategoryType, effort: string) => Promise<void>;
}

export const useStore = create<AppState>((set, get) => ({
  activeCategory: "claude-cli",
  setActiveCategory: (c) => {
    set({ activeCategory: c });
    void get().loadCategory(c);
  },

  keys: [],
  proxy: null,
  brain: null,
  events: [],
  settings: null,
  vendors: [],
  loading: false,

  updateCheck: null,
  updateChecking: false,
  checkForUpdates: async (opts) => {
    const silent = opts?.silent ?? false;
    set({ updateChecking: true });
    try {
      const result = await api.checkForUpdates();
      set({ updateCheck: result });
      return result;
    } catch (e) {
      const result: UpdateCheckResult = {
        status: "error",
        currentVersion: "",
        error: String((e as Error)?.message ?? e),
      };
      set({ updateCheck: result });
      if (!silent) {
        get().showToast("error", result.error ?? "检查更新失败");
      }
      return result;
    } finally {
      set({ updateChecking: false });
    }
  },
  clearUpdateAvailable: () => {
    const cur = get().updateCheck;
    if (cur?.status === "available") {
      set({
        updateCheck: {
          ...cur,
          status: "up_to_date",
          version: null,
          notes: null,
        },
      });
    }
  },

  theme: "system",
  setTheme: (t) => {
    set({ theme: t });
    applyTheme(t);
    const s = get().settings;
    if (s) {
      api.saveSettings({ ...s, theme: t }).catch((e) => {
        console.error("saveSettings(theme) failed", e);
        get().showToast("error", String((e as Error)?.message ?? e));
      });
    }
  },

  lang: "zh",
  setLang: (l) => {
    set({ lang: l });
    const s = get().settings;
    if (s) {
      api.saveSettings({ ...s, language: l }).catch((e) => {
        console.error("saveSettings(language) failed", e);
        get().showToast("error", String((e as Error)?.message ?? e));
      });
    }
  },

  toast: null,
  showToast: (kind, msg) => set({ toast: { kind, msg } }),
  clearToast: () => set({ toast: null }),

  configAppliedCategory: null,
  clearConfigApplied: () => set({ configAppliedCategory: null }),

  async loadCategory(c) {
    set({ loading: true });
    // 容错：4 个 IPC 独立结算，单个失败不拖垮其余刷新（避免"操作后列表不更新"的假象）。
    const [keysR, proxyR, brainR, eventsR] = await Promise.allSettled([
      api.listKeys(c),
      api.getProxyState(c),
      api.getBrainConfig(c),
      api.listAllEvents(),
    ]);
    if (keysR.status === "rejected") console.error("listKeys failed", keysR.reason);
    if (proxyR.status === "rejected") console.error("getProxyState failed", proxyR.reason);
    if (brainR.status === "rejected") console.error("getBrainConfig failed", brainR.reason);
    if (eventsR.status === "rejected") console.error("listAllEvents failed", eventsR.reason);
    set((s) => ({
      keys: keysR.status === "fulfilled" ? keysR.value : s.keys,
      proxy: proxyR.status === "fulfilled" ? proxyR.value : s.proxy,
      brain: brainR.status === "fulfilled" ? brainR.value : s.brain,
      events: eventsR.status === "fulfilled" ? eventsR.value : s.events,
      loading: false,
    }));
  },

  async refreshCategory() {
    const c = get().activeCategory;
    try {
      const [keysR, proxyR] = await Promise.allSettled([
        api.listKeys(c),
        api.getProxyState(c),
      ]);
      set((s) => ({
        keys: keysR.status === "fulfilled" ? keysR.value : s.keys,
        proxy: proxyR.status === "fulfilled" ? proxyR.value : s.proxy,
      }));
    } catch (e) {
      console.error("refreshCategory failed", e);
    }
  },

  // 轻量刷新：仅拉事件日志（运行日志页 2s 轮询用），不动 keys/proxy/brain，避免整页重载闪烁。
  // 合并全部分类，切换活动分类时日志连续不裁剪；分类过滤交给前端页面按 categoryId 客户端筛选。
  async refreshEvents() {
    try {
      const events = await api.listAllEvents();
      set({ events });
    } catch (e) {
      console.error("refreshEvents failed", e);
    }
  },

  async loadSettings() {
    const settings = await api.getSettings();
    set({ settings, theme: settings.theme, lang: settings.language ?? "zh" });
    applyTheme(settings.theme);
  },

  async loadVendors() {
    try {
      set({ vendors: await api.listVendors() });
    } catch (e) {
      console.error("listVendors failed", e);
    }
  },

  async toggleKey(keyId, enabled) {
    // 乐观更新：立即翻转开关，让 UI 有即时反馈
    set({ keys: get().keys.map((k) => (k.id === keyId ? { ...k, enabled } : k)) });
    try {
      await api.toggleKey(keyId, enabled);
    } catch (e) {
      // 落盘失败必须可见：回滚开关状态 + 弹错误提示，禁止静默吞掉
      set({ keys: get().keys.map((k) => (k.id === keyId ? { ...k, enabled: !enabled } : k)) });
      console.error("toggleKey failed", e);
      get().showToast("error", String((e as Error)?.message ?? e));
      return;
    }
    await get().loadCategory(get().activeCategory);
  },

  // 调整故障转移优先级：在「同分类的全部 Key」按优先级排序后，与相邻 Key 交换位置。
  // 重排后把整列优先级重新赋为连续值 0,1,2…，一劳永逸消除「全 999 同级」——
  // 只有优先级互不相同，故障转移才有确定的主/备顺序（否则永远先打第一个 Key，触发限流）。
  async moveKey(keyId, direction) {
    const cat = get().activeCategory;
    const ordered = [...get().keys]
      .filter((k) => k.categoryId === cat)
      .sort((a, b) => a.priority - b.priority);
    const idx = ordered.findIndex((k) => k.id === keyId);
    if (idx < 0) return;
    const swapWith = direction === "up" ? idx - 1 : idx + 1;
    if (swapWith < 0 || swapWith >= ordered.length) return; // 已在两端，无法再移

    [ordered[idx], ordered[swapWith]] = [ordered[swapWith], ordered[idx]];
    // 规整为连续优先级 0,1,2…（消除全 999 同级），只对优先级真正变化的 Key 落盘。
    const renumbered = ordered.map((k, i) => ({ ...k, priority: i }));
    const toPersist = renumbered.filter((k) => {
      const orig = get().keys.find((o) => o.id === k.id);
      return orig && orig.priority !== k.priority;
    });
    // 乐观更新：立即用新优先级刷新列表。
    const byId = new Map(renumbered.map((k) => [k.id, k]));
    set({ keys: get().keys.map((k) => byId.get(k.id) ?? k) });
    try {
      await Promise.all(toPersist.map((k) => api.upsertKey(k)));
    } catch (e) {
      console.error("moveKey failed", e);
      get().showToast("error", String((e as Error)?.message ?? e));
    }
    await get().loadCategory(cat);
  },

  // 一键设为主：把目标 Key 提到 priority 0（其余保持原相对顺序顺延），
  // 免去连点上移箭头。与 moveKey 同样规整为连续优先级、只对变动的 Key 落盘。
  async setPrimaryKey(keyId) {
    const cat = get().activeCategory;
    const ordered = [...get().keys]
      .filter((k) => k.categoryId === cat)
      .sort((a, b) => a.priority - b.priority);
    const idx = ordered.findIndex((k) => k.id === keyId);
    if (idx <= 0) return; // 不存在或已是主，无需处理
    const [target] = ordered.splice(idx, 1);
    ordered.unshift(target);
    const renumbered = ordered.map((k, i) => ({ ...k, priority: i }));
    const toPersist = renumbered.filter((k) => {
      const orig = get().keys.find((o) => o.id === k.id);
      return orig && orig.priority !== k.priority;
    });
    const byId = new Map(renumbered.map((k) => [k.id, k]));
    set({ keys: get().keys.map((k) => byId.get(k.id) ?? k) });
    try {
      await Promise.all(toPersist.map((k) => api.upsertKey(k)));
    } catch (e) {
      console.error("setPrimaryKey failed", e);
      get().showToast("error", String((e as Error)?.message ?? e));
    }
    await get().loadCategory(cat);
  },

  async deleteKey(keyId) {
    try {
      await api.deleteKey(keyId);
    } catch (e) {
      console.error("deleteKey failed", e);
      get().showToast("error", String((e as Error)?.message ?? e));
      return;
    }
    await get().loadCategory(get().activeCategory);
  },

  async checkHealth(keyId) {
    // 乐观：先置为 checking
    set({
      keys: get().keys.map((k) =>
        k.id === keyId ? { ...k, health: { ...k.health, status: "checking" } } : k
      ),
    });
    try {
      await api.checkHealth(keyId);
    } catch (e) {
      console.error("checkHealth failed", e);
    }
    // 探测完只回写这一条 Key，不走 loadCategory（后者会 set loading=true 并重拉
    // keys/proxy/brain/events 四样，导致整列表闪烁）。拉最新 keys 后只替换目标那条，
    // 其余引用不变，不动 loading/proxy/brain/events。
    try {
      const latest = await api.listKeys(get().activeCategory);
      const updated = latest.find((k) => k.id === keyId);
      set({
        keys: get().keys.map((k) => (k.id === keyId && updated ? updated : k)),
      });
    } catch (e) {
      console.error("checkHealth refresh failed", e);
      // 拉取失败：至少把该 Key 从 checking 解除，避免永久卡在"检测中"
      set({
        keys: get().keys.map((k) =>
          k.id === keyId && k.health.status === "checking"
            ? { ...k, health: { ...k.health, status: "unknown" } }
            : k
        ),
      });
    }
  },

  async startProxy() {
    const cat = get().activeCategory;
    // 启动失败（端口被占等）必须可见，不能静默吞掉——ProxyStatusBar 以 void 调用本函数。
    try {
      const proxy = await api.startProxy(cat);
      set({ proxy });
    } catch (e) {
      console.error("startProxy failed", e);
      get().showToast("error", String((e as Error)?.message ?? e));
      return; // 代理没起来，不继续写工具配置
    }
    // 启动即写入目标工具配置，省去用户再手点一次「写入工具配置」。
    // 写入失败不回滚代理（代理已起来可用），仅弹提示告知。
    try {
      await api.applyToolConfig(cat);
      await get().loadCategory(cat);
      set({ configAppliedCategory: cat });
    } catch (e) {
      console.error("applyToolConfig after start failed", e);
      get().showToast("error", String((e as Error)?.message ?? e));
    }
  },

  async stopProxy() {
    const cat = get().activeCategory;
    const proxy = await api.stopProxy(cat);
    set({ proxy });
    // 停止即退出接入态：还原目标工具配置（Codex 会连同 auth.json 一起复原，
    // 用户官方 OAuth 登录立即恢复，无需手动拷备份或重新登录）。
    // 还原失败不阻断停止（代理已停），仅弹提示告知。
    try {
      await api.restoreToolConfig(cat);
      if (get().configAppliedCategory === cat) set({ configAppliedCategory: null });
      await get().loadCategory(cat);
    } catch (e) {
      console.error("restoreToolConfig after stop failed", e);
      get().showToast("error", String((e as Error)?.message ?? e));
    }
  },

  async setActiveModel(category, model) {
    // 乐观更新本地 settings.activeModels，让下拉即时反映选择
    const s = get().settings;
    if (s) {
      const next = { ...(s.activeModels ?? {}) };
      if (model.trim()) next[category] = model.trim();
      else delete next[category];
      set({ settings: { ...s, activeModels: next } });
    }
    try {
      await api.setActiveModel(category, model);
    } catch (e) {
      console.error("setActiveModel failed", e);
      get().showToast("error", String((e as Error)?.message ?? e));
      // 落盘失败：重新拉后端权威值回滚乐观更新，避免 UI 与实际不一致
      await get().loadSettings();
    }
  },

  async setActiveEffort(category, effort) {
    // 乐观更新本地 settings.activeEfforts，让下拉即时反映选择
    const s = get().settings;
    if (s) {
      const next = { ...(s.activeEfforts ?? {}) };
      if (effort.trim() && effort !== "off") next[category] = effort.trim();
      else delete next[category];
      set({ settings: { ...s, activeEfforts: next } });
    }
    try {
      await api.setActiveEffort(category, effort);
    } catch (e) {
      console.error("setActiveEffort failed", e);
      get().showToast("error", String((e as Error)?.message ?? e));
      await get().loadSettings();
    }
  },
}));

/** 应用主题到 <html> class（shadcn/ui 深色约定） */
export function applyTheme(theme: ThemePref) {
  const root = document.documentElement;
  const isDark =
    theme === "dark" ||
    (theme === "system" &&
      window.matchMedia("(prefers-color-scheme: dark)").matches);
  root.classList.toggle("dark", isDark);
}
