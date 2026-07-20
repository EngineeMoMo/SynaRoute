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
  deleteKey: (keyId: string) => Promise<void>;
  checkHealth: (keyId: string) => Promise<void>;
  startProxy: () => Promise<void>;
  stopProxy: () => Promise<void>;
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
      api.listEvents(c),
    ]);
    if (keysR.status === "rejected") console.error("listKeys failed", keysR.reason);
    if (proxyR.status === "rejected") console.error("getProxyState failed", proxyR.reason);
    if (brainR.status === "rejected") console.error("getBrainConfig failed", brainR.reason);
    if (eventsR.status === "rejected") console.error("listEvents failed", eventsR.reason);
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
  async refreshEvents() {
    const c = get().activeCategory;
    try {
      const events = await api.listEvents(c);
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
    } finally {
      // 无论探测成功、失败还是抛错，都刷新列表，避免 Key 永久卡在 "检测中"
      await get().loadCategory(get().activeCategory);
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
    const proxy = await api.stopProxy(get().activeCategory);
    set({ proxy });
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
