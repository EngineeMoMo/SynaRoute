// 全局状态管理（Zustand）
import { create } from "zustand";
import type {
  AppSettings,
  BalanceResult,
  BrainConfig,
  CategoryType,
  EventLogEntry,
  OnboardingState,
  ProviderKey,
  ProxyState,
  ThemePref,
  UpdateCheckResult,
  Vendor,
} from "@/types";
import { translate, type Lang } from "@/lib/i18n";
import { api, isTauri } from "@/lib/bridge";
import { pickPrefs } from "@/lib/prefs";
import { reuseUnchanged } from "@/lib/reuseUnchanged";

/**
 * 一条余额缓存：查询结果 + **产出它的那份配置的指纹**。
 *
 * 指纹必须跟着结果一起存。只存结果的话，用户把查询地址从错的改成对的、保存，
 * 卡片上仍会显示上一次那条 404 —— 而那正是他刚修掉的东西，看着像「改了没生效」。
 */
export interface BalanceCacheEntry {
  result: BalanceResult;
  fingerprint: string;
}

/**
 * 余额缓存的新鲜期。超过它，卡片重新挂载时（切分类回来、重开窗口）会自动重查一次。
 *
 * 改为 5 分钟，与后端默认 TTL 对齐，避免「后端缓存过期 → 前端认为未过期 → 手动刷新不生效」。
 * 余额是「钱还剩多少」，看到几分钟前的值完全够用，
 * 而每次切分类都去发一轮请求会把中转站的计费接口打得很勤（部分站点对此限流）。
 * 想看当下值有手动刷新按钮，且卡片上永远标着「查询于 X 分钟前」，陈旧是可见的。
 */
export const BALANCE_TTL_MS = 5 * 60 * 1000;

/**
 * 只改 settings 里的一两个字段并落盘，**基线取磁盘最新值而非内存快照**。
 *
 * 为什么不能直接 `{...get().settings, theme}`：`store.settings` 只在 App 挂载时 `loadSettings()`
 * 写过一次，设置页此后改的字段（日志目录、健康探测间隔、局域网暴露…）只进它自己的
 * 局部 state，store 这份是**挂载那一刻的旧快照**。拿旧快照整份提交，会把用户刚改的字段全部
 * 顶回旧值。故先 `getSettings()` 拉磁盘权威值，再合并本次要改的字段。
 *
 * 提交前过一次 `pickPrefs` 白名单。后端 `save_settings` 的入参类型已经是 `UserPrefs`
 * （多余的键会被静默丢弃），这里再过一次是为了让「前端不该发 runtime 字段」在前端代码里
 * 也可读 —— 而且 `pickPrefs` 是逐字段列举的，日后后端加自管字段时，
 * 这里不会因为整店展开而把它带出去。
 *
 * 注意 `autoStart` **不再走这条路径**：它有系统副作用（写注册表），已改走专用命令
 * `api.setAutoStart`。那正是出过 P0 的地方 —— 切主题会把用户刚关掉的自启动重新装回系统。
 */
/**
 * persistOneSetting 的串行化链。
 *
 * 没有它时两次快速连续调用（切主题后立刻切语言，两个控件相邻，真实可发生）各自并发执行
 * getSettings → merge → saveSettings：第二次的 getSettings 读到的是**未含第一次 patch**
 * 的磁盘旧值，其 saveSettings 会把旧 theme 连同新 language 一起写回 —— UI 即时态正确
 * （theme/lang 单独维护），磁盘上先保存的那项却被静默顶回，重启后跳回旧值。
 * 正是本函数注释里最忌讳的「切 X 顶掉刚改的 Y」，只是发生在它自己的两次调用之间。
 * 链式串行让第二次的 getSettings 必然读到第一次已落盘的结果。
 */
let persistChain: Promise<void> = Promise.resolve();

async function persistOneSetting(
  patch: Partial<AppSettings>,
  get: () => AppState,
): Promise<void> {
  const run = async () => {
    try {
      const latest = await api.getSettings();
      const next = { ...latest, ...patch };
      await api.saveSettings(pickPrefs(next));
      // 落盘成功后同步 store 副本，避免它继续陈旧下去（下一次调用仍会重拉，这里只是保持一致）
      useStore.setState({ settings: next });
    } catch (e) {
      console.error("persistOneSetting failed", patch, e);
      get().showToast("error", String((e as Error)?.message ?? e));
    }
  };
  // run 自吞异常（catch 里已提示用户），链上不会累积 rejected 状态。
  persistChain = persistChain.then(run);
  return persistChain;
}

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
  /** 首启向导状态（UX#1）。null = 尚未拉取 */
  onboarding: OnboardingState | null;
  vendors: Vendor[];
  loading: boolean;

  /**
   * 余额查询结果缓存，按 keyId 索引（第④批）。
   *
   * **不并进 `keys` 里的 ProviderKey**：余额是「查询结果」而非 Key 的配置，
   * 混进去会被 5s 轮询的 `listKeys` 整份覆盖掉（后端不返回余额），
   * 表现为卡片上的余额每 5 秒闪一下就消失。
   *
   * 按 keyId 索引也顺带免疫了本项目栽过的那类「切分类串台」——
   * 在途的查询结果只会落到它自己那条 Key 上，不会写进新分类的某个位置。
   */
  balances: Record<string, BalanceCacheEntry>;
  /** 正在查询中的 keyId 集合（卡片据此转刷新图标，也用于去重并发请求） */
  balanceLoading: Record<string, boolean>;
  /**
   * 查一条 Key 的余额。
   *
   * `fingerprint` 由调用方用 `balanceFingerprint(key)` 算好传入：缓存命中判据是
   * 「指纹相同 且 未超新鲜度门槛」，指纹变了（用户改了查询地址等）就必须重查。
   * `force` = 用户手点刷新，跳过门槛但仍受并发去重约束。
   * `maxAgeMs` = 自定义新鲜度门槛（自动轮询用它传用户配的间隔，否则默认 5 分钟 TTL
   * 会把短间隔轮询全部拦掉 —— 表现为「设了自动查询不生效」）。
   */
  refreshBalance: (
    keyId: string,
    fingerprint: string,
    force?: boolean,
    maxAgeMs?: number,
  ) => Promise<void>;
  /** 编辑器「测试查询」的结果直接写回缓存，省掉卡片再发一次同样的请求 */
  setBalanceResult: (keyId: string, fingerprint: string, result: BalanceResult) => void;
  /** 清掉某 Key 的余额缓存，下次卡片渲染会重查。改密钥后调用它（指纹不含密钥、无法感知密钥变更）。 */
  clearBalance: (keyId: string) => void;

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
  loadOnboarding: () => Promise<void>;
  dismissOnboarding: () => Promise<void>;
  loadVendors: () => Promise<void>;
  toggleKey: (keyId: string, enabled: boolean) => Promise<void>;
  moveKey: (keyId: string, direction: "up" | "down") => Promise<void>;
  /**
   * 三个「切换后要弹成功 toast」的 action 返回 `boolean`：true = 无错。
   *
   * 为什么不靠 try/catch：这三个 action **内部**已经在失败时 `showToast("error", ...)` 了，
   * 调用方再包一层 catch 会弹两条；而 toast 是单值不是队列（后一条顶掉前一条），
   * 结果就是用户只看到后弹的那条。用返回值判断，才能做到「失败时只有错误提示、
   * 成功时才报成功」——否则会出现「明明失败了却提示已切换」。
   */
  setPrimaryKey: (keyId: string) => Promise<boolean>;
  deleteKey: (keyId: string) => Promise<void>;
  checkHealth: (keyId: string) => Promise<void>;
  startProxy: (opts?: { announce?: boolean }) => Promise<boolean>;
  stopProxy: () => Promise<void>;
  /** 切回官方：停止代理 + 还原配置，但保留 MCP 注册（大脑聚合继续可用）。 */
  switchToOfficial: () => Promise<void>;
  // 应用内「对外模型名」选择（借鉴 EchoBird）：客户端菜单拉不到中转模型时在应用内选，
  // 代理转发时覆盖客户端发来的模型名。走后端专用命令直写，即时生效、免重启客户端。
  setActiveModel: (category: CategoryType, model: string) => Promise<boolean>;
  setActiveEffort: (category: CategoryType, effort: string) => Promise<boolean>;
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
  onboarding: null,
  vendors: [],
  loading: false,

  balances: {},
  balanceLoading: {},

  async refreshBalance(keyId, fingerprint, force = false, maxAgeMs) {
    // 并发去重。**这一条不能省**：StrictMode 下 effect 会连跑两次，
    // 卡片挂载即发两个一模一样的请求。这里同步读同步写，第二次调用必然看到 true。
    if (get().balanceLoading[keyId]) return;

    if (!force) {
      const cached = get().balances[keyId];
      // 新鲜度门槛：调用方给了 `maxAgeMs` 就用它，否则用默认 TTL。
      //
      // **为什么必须可传**：自动轮询的间隔由用户配（可低至 1 分钟），而默认 TTL 是 5 分钟。
      // 若一律按 5 分钟判，用户把间隔设成 1 分钟时前 4 次轮询会全部被缓存拦掉 ——
      // 表现为「设了自动查询却不生效」，正是本项目最忌讳的静默失效。
      // 轮询也不能改用 `force: true`：那会连并发去重一起绕过。
      const freshFor = maxAgeMs && maxAgeMs > 0 ? maxAgeMs : BALANCE_TTL_MS;
      // 指纹相同 = 同一份配置查出来的；未超门槛 = 还算新鲜。两条都满足才复用。
      if (
        cached &&
        cached.fingerprint === fingerprint &&
        Date.now() - cached.result.queriedAt < freshFor
      ) {
        return;
      }
    }

    set((s) => ({ balanceLoading: { ...s.balanceLoading, [keyId]: true } }));
    try {
      // force 必须**透传给后端**：后端有自己的缓存（有效期 = autoIntervalMin，未配则 5 分钟，
      // 且失败结果也缓存）。只跳过前端 TTL 不透传的话，卡片「手动刷新」在后端缓存期内
      // 是空操作 —— 数值与时间戳纹丝不动，一次瞬时失败会被钉住整个缓存期，
      // 用户点多少次刷新都拿到同一条旧错误。
      const result = await api.queryKeyBalance(keyId, force);
      // **瞬时失败不入缓存**：后端并发去重命中时返回 `transient: true` 的伪失败
      // （压根没打上游、不代表任何结论）。写进缓存会把卡片钉在一条假错误上整个 TTL
      // （最长 5 分钟），而真正在跑的那次查询结果反被这条后到的伪失败盖掉 ——
      // 用户看到「余额查询正在进行中」这句话停留数分钟，点刷新也没用。
      // 直接丢弃即可：那次真查询自己会写缓存。
      if (result.transient) return;
      // 失败结果**也要落缓存**：后端把失败装在 `BalanceResult.error` 里正常返回
      // （不抛 Err），卡片要如实显示那句原因。丢掉它会让卡片停在「未查询」，
      // 用户既看不到余额也看不到为什么。
      set((s) => ({ balances: { ...s.balances, [keyId]: { result, fingerprint } } }));
    } catch (e) {
      // 走到这里是 IPC 本身炸了（后端 panic、命令不存在），不是查询失败。
      // 同样如实写进缓存，绝不静默——静默会让卡片永远停在「未查询」。
      console.error("queryKeyBalance failed", e);
      set((s) => ({
        balances: {
          ...s.balances,
          [keyId]: {
            result: {
              ok: false,
              queriedAt: Date.now(),
              error: String((e as Error)?.message ?? e),
            },
            fingerprint,
          },
        },
      }));
    } finally {
      // 用 delete 而不是置 false：这个表按 keyId 增长，删过的 Key 不该留残项。
      set((s) => {
        const next = { ...s.balanceLoading };
        delete next[keyId];
        return { balanceLoading: next };
      });
    }
  },

  setBalanceResult(keyId, fingerprint, result) {
    set((s) => ({ balances: { ...s.balances, [keyId]: { result, fingerprint } } }));
  },

  clearBalance(keyId) {
    set((s) => {
      if (!(keyId in s.balances)) return {};
      const next = { ...s.balances };
      delete next[keyId];
      return { balances: next };
    });
  },

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
        // 兜底文案要跟随语言：store 不是组件、拿不到 useT()，用 translate(get().lang, …)
        // （上游错误信息本身可能为空串/undefined，此时才用这句）。
        get().showToast("error", result.error || translate(get().lang, "settings.updateCheckFailed"));
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
    void persistOneSetting({ theme: t }, get);
  },

  lang: "zh",
  setLang: (l) => {
    set({ lang: l });
    void persistOneSetting({ language: l }, get);
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
      keys: keysR.status === "fulfilled" ? reuseUnchanged(s.keys, keysR.value) : s.keys,
      proxy: proxyR.status === "fulfilled" ? proxyR.value : s.proxy,
      brain: brainR.status === "fulfilled" ? brainR.value : s.brain,
      events: eventsR.status === "fulfilled" ? reuseUnchanged(s.events, eventsR.value) : s.events,
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
        // 逐条复用内容未变的旧对象：这是 5s 轮询，不做的话 KeyCard 的 memo 恒失效。
        keys: keysR.status === "fulfilled" ? reuseUnchanged(s.keys, keysR.value) : s.keys,
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
      // 2s 轮询：日志页最密的刷新源。逐条复用未变的旧事件对象，让 LogRow 的 memo 生效——
      // 事件是追加型的，新增一条时前面几百条内容都没动，不该跟着重渲染。
      set((s) => ({ events: reuseUnchanged(s.events, events) }));
    } catch (e) {
      console.error("refreshEvents failed", e);
    }
  },

  async loadSettings() {
    const settings = await api.getSettings();
    set({ settings, theme: settings.theme, lang: settings.language ?? "zh" });
    applyTheme(settings.theme);
  },

  /** 拉首启向导状态（UX#1）。失败不影响主流程——宁可不显示向导，也不要拿报错挡住启动。 */
  async loadOnboarding() {
    try {
      set({ onboarding: await api.getOnboardingState() });
    } catch (e) {
      console.error("getOnboardingState failed", e);
    }
  },

  /**
   * 关闭向导（完成或跳过）。
   *
   * 先乐观关闭再落盘，且**落盘失败不回滚**：把已经关掉的向导再弹回用户面前，
   * 比标记丢失糟糕得多。真丢了也只是下次启动再出现一次，而那时用户已经有 Key 了，
   * `shouldShow` 的 `totalKeys === 0` 条件会兜住。
   */
  async dismissOnboarding() {
    const cur = get().onboarding;
    if (cur) set({ onboarding: { ...cur, shouldShow: false, done: true } });
    try {
      await api.setOnboardingDone(true);
    } catch (e) {
      console.error("setOnboardingDone failed", e);
    }
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

  // 调整故障转移优先级：与相邻 Key 交换位置，并把整列重编号为连续 0,1,2…
  // （消除「全 999 同级」——只有优先级互不相同，故障转移才有确定的主/备顺序。）
  //
  // **重排本身在后端一次原子完成**（`move_key` 命令 → `Store::move_key`），
  // 前端只做乐观更新。旧实现是前端自己重编号、再对变化的每条并发 `upsertKey`：
  // ① 那把「换个顺序」变成 N 次整份 Key 覆盖，桌面端分类里若有一条 Key 的对外名不合规，
  //    `Promise.all` 中那条被拒、其余已落盘 → 优先级留下重复值/空洞，顺序半调整；
  //    而 toast 弹的是「某 Key 模型名不可服务」，与用户刚点的箭头毫不相干。
  // ② 提交的是页面快照，会把运行态（熔断计数/余额缓存）一起写回去。
  async moveKey(keyId, direction) {
    const cat = get().activeCategory;
    const ordered = [...get().keys]
      .filter((k) => k.categoryId === cat)
      .sort((a, b) => a.priority - b.priority);
    const idx = ordered.findIndex((k) => k.id === keyId);
    if (idx < 0) return;
    const swapWith = direction === "up" ? idx - 1 : idx + 1;
    if (swapWith < 0 || swapWith >= ordered.length) return; // 已在两端，无法再移

    // 乐观更新：先按交换后的顺序连续重编号刷新列表（后端用同一套规则，随后 reload 对齐）。
    [ordered[idx], ordered[swapWith]] = [ordered[swapWith], ordered[idx]];
    const byId = new Map(ordered.map((k, i) => [k.id, { ...k, priority: i }]));
    set({ keys: get().keys.map((k) => byId.get(k.id) ?? k) });
    try {
      await api.moveKey(cat, keyId, direction);
    } catch (e) {
      console.error("moveKey failed", e);
      get().showToast("error", String((e as Error)?.message ?? e));
    }
    await get().loadCategory(cat);
  },

  // 一键设为主：把目标 Key 提到 priority 0（其余保持原相对顺序顺延），免去连点上移箭头。
  //
  // 重排规则在**后端** Store::set_primary_key 里，这里只负责乐观更新 + 调命令。
  // 原先前端自己算重排再逐个 upsertKey：托盘也要这个功能（FR-022 要求「从托盘可完成
  // 主 Key 快切」），两处各写一份规则迟早漂移 —— 表现为「从托盘设的主」与「从界面设的主」
  // 结果不一致。故规则收归后端做单一事实来源。
  //
  // 乐观更新仍留在前端：让点击即时反映，失败时由 loadCategory 拉回后端权威值纠正。
  async setPrimaryKey(keyId) {
    const cat = get().activeCategory;
    const ordered = [...get().keys]
      .filter((k) => k.categoryId === cat)
      .sort((a, b) => a.priority - b.priority);
    const idx = ordered.findIndex((k) => k.id === keyId);
    // 已是主（或压根不存在）→ 不是失败，返回 true 让调用方照常给正反馈。
    if (idx <= 0) return true;
    const [target] = ordered.splice(idx, 1);
    ordered.unshift(target);
    const renumbered = ordered.map((k, i) => ({ ...k, priority: i }));
    const byId = new Map(renumbered.map((k) => [k.id, k]));
    set({ keys: get().keys.map((k) => byId.get(k.id) ?? k) });
    let ok = true;
    try {
      await api.setPrimaryKey(cat, keyId);
    } catch (e) {
      console.error("setPrimaryKey failed", e);
      get().showToast("error", String((e as Error)?.message ?? e));
      ok = false;
    }
    await get().loadCategory(cat);
    return ok;
  },

  async deleteKey(keyId) {
    try {
      await api.deleteKey(keyId);
    } catch (e) {
      console.error("deleteKey failed", e);
      get().showToast("error", String((e as Error)?.message ?? e));
      return;
    }
    // 顺带清掉这条 Key 的余额缓存：该表按 keyId 索引、只增不减，
    // 留着既是内存泄漏，又会在极端情况下（id 复用）把旧余额显示到新 Key 上。
    set((s) => {
      if (!(keyId in s.balances)) return {};
      const next = { ...s.balances };
      delete next[keyId];
      return { balances: next };
    });
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

  async startProxy(opts) {
    // announce：是否弹「已写入工具配置」对话框。首启向导要自己承接这一步的反馈
    // （第③步就在说明写了什么），再弹一个对话框会盖住向导。默认 true 保持既有行为。
    //
    // **必须复用本函数而不是在向导里重写一遍 start + apply**：语义得与界面按钮、托盘完全一致
    //（起 = 顺带写工具配置）。三处各写一份必然漂移，而漂移的表现是「托盘说已启动、
    // 客户端却没走代理」这类无头案。
    const announce = opts?.announce ?? true;
    const cat = get().activeCategory;
    // 启动失败（端口被占等）必须可见，不能静默吞掉——ProxyStatusBar 以 void 调用本函数。
    try {
      const proxy = await api.startProxy(cat);
      set({ proxy });
    } catch (e) {
      console.error("startProxy failed", e);
      get().showToast("error", String((e as Error)?.message ?? e));
      return false; // 代理没起来，不继续写工具配置
    }
    // 启动即写入目标工具配置，省去用户再手点一次「写入工具配置」。
    // 写入失败不回滚代理（代理已起来可用），仅弹提示告知。
    try {
      await api.applyToolConfig(cat);
      await get().loadCategory(cat);
      if (announce) set({ configAppliedCategory: cat });
    } catch (e) {
      console.error("applyToolConfig after start failed", e);
      get().showToast("error", String((e as Error)?.message ?? e));
    }
    // 代理已可用即算成功——写配置失败属于降级而非失败，与上面「不回滚」的语义一致。
    return true;
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

  async switchToOfficial() {
    const cat = get().activeCategory;
    try {
      await api.switchToOfficial(cat);
    } catch (e) {
      console.error("switchToOfficial failed", e);
      get().showToast("error", String((e as Error)?.message ?? e));
      return;
    }
    // 后端已原子地处理了 stop + restore + 重注册 MCP
    // 只需刷新本地状态
    try {
      const proxy = await api.getProxyState(cat);
      set({ proxy });
      if (get().configAppliedCategory === cat) set({ configAppliedCategory: null });
      await get().loadCategory(cat);
    } catch {
      // 状态刷新失败不阻断，下一次轮询会自愈
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
      return false;
    }
    return true;
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
      return false;
    }
    return true;
  },
}));

/** 应用主题到 <html> class（shadcn/ui 深色约定）+ 同步系统窗口装饰（标题栏）。 */
export function applyTheme(theme: ThemePref) {
  const root = document.documentElement;
  const isDark =
    theme === "dark" ||
    (theme === "system" &&
      window.matchMedia("(prefers-color-scheme: dark)").matches);
  root.classList.toggle("dark", isDark);
  // 标题栏由**系统**绘制，不在 WebView 里，故 CSS 变量管不到它——深色下顶部会留一条白条。
  // 必须显式告知窗口当前主题，Windows 才会把标题栏换成深色（Tauri v2 Window.setTheme）。
  // 浏览器预览模式没有 Tauri 注入，跳过；失败只告警不影响页面主题。
  void syncWindowTheme(isDark);
}

/** 把主题同步给系统窗口装饰（标题栏）。仅 Tauri 环境生效。 */
async function syncWindowTheme(isDark: boolean) {
  if (!isTauri()) return;
  try {
    const { getCurrentWindow } = await import("@tauri-apps/api/window");
    await getCurrentWindow().setTheme(isDark ? "dark" : "light");
  } catch (e) {
    console.warn("setTheme(window) failed", e);
  }
}
