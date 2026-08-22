import type {
  AggregateResult,
  AppSettings,
  OnboardingState,
  FirstRequestProbe,
  BalanceResult,
  BrainConfig,
  CategoryType,
  CcSwitchImportReport,
  CcSwitchScanResult,
  CodegraphState,
  DailyUsageBucket,
  DesktopModelNameReport,
  EventLogEntry,
  ExportOutcome,
  ImportMode,
  ImportPreview,
  ImportReport,
  MasterPasswordState,
  McpStatus,
  ModelInfo,
  ProviderKey,
  ProxyState,
  RecentWorkdir,
  RequestTrace,
  RetrievedFile,
  ToolConfigPreview,
  TokenUsageByKey,
  UsageCostRow,
  UpdateCheckResult,
  Vendor,
} from "@/types";
import type { UserPrefs } from "@/lib/prefs";
import { mockBridge } from "./mockData";

/** 是否运行在 Tauri 环境内 */
export function isTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

// 动态导入 @tauri-apps/api，避免在纯浏览器环境因缺少注入而报错
async function tauriInvoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<T>(cmd, args);
}

/**
 * 统一调用入口：Tauri 环境走真实后端命令，浏览器环境走 mock。
 *
 * `mock` 参数是一个**惰性 thunk**：`() => mockBridge.x()` 或内联假实现。
 * mockData.ts 在生产构建被 vite alias 换成「全方法抛错」的 Proxy 桩，但本文件里还有
 * 30 余处**内联** thunk（revealSecret 返回伪造密钥、exportConfig 返回不存在的假路径…）
 * 不经过 mockBridge —— 生产环境 `isTauri()` 若误判（Tauri 注入失效），一半 API 抛错、
 * 另一半静默返回捏造数据，比全炸更难排查（导出假成功在备份场景等于数据丢失）。
 * 故守卫收进 call() 本体：**非 DEV 构建走到 mock 分支一律抛错**，覆盖所有 thunk。
 * `import.meta.env.DEV` 是编译期常量，浏览器预览（vite dev）不受影响。
 */
async function call<T>(cmd: string, args: Record<string, unknown> | undefined, mock: () => Promise<T> | T): Promise<T> {
  if (isTauri()) {
    return tauriInvoke<T>(cmd, args);
  }
  if (!import.meta.env.DEV) {
    throw new Error(
      `[SynaRoute] 运行环境检测失败（cmd=${cmd}）：本应走 Tauri 后端，却落到了浏览器预览分支。请重启应用；若持续出现请反馈此错误。`,
    );
  }
  return mock();
}

// ---- 配置管理 ----

export const api = {
  // 列出某分类下的 Key
  listKeys: (categoryId: CategoryType) =>
    call<ProviderKey[]>("list_keys", { categoryId }, () => mockBridge.listKeys(categoryId)),

  // 新增/更新 Key（secret 单独走 saveSecret，不随对象下发）
  upsertKey: (key: ProviderKey) =>
    call<ProviderKey>("upsert_key", { key }, () => mockBridge.upsertKey(key)),

  /**
   * 桌面端对外模型名即时体检（UX#4）。跟着打字走，故后端那条命令刻意是纯函数、零锁。
   *
   * **mock 刻意返回「不适用」而不复刻判据**：在前端造第二份规则正是本功能要消灭的漂移源
   * （判据是 50+ 条厂商名子串 + 词边界匹配，逆向自桌面端 app.asar）。
   * 代价是浏览器预览模式下不显示该提示 —— 可以接受，因为它本来就只对桌面端分类有意义。
   */
  checkDesktopModelNames: (key: ProviderKey) =>
    call<DesktopModelNameReport>("check_desktop_model_names", { key }, async () => ({
      applicable: false,
      total: 0,
      issues: [],
    })),

  // 删除 Key
  deleteKey: (keyId: string) =>
    call<void>("delete_key", { keyId }, () => mockBridge.deleteKey(keyId)),

  // 保存密钥（加密存储，NFR-006）
  saveSecret: (keyId: string, secret: string) =>
    call<void>("save_secret", { keyId, secret }, () => mockBridge.saveSecret(keyId, secret)),

  // 按需揭示已存明文密钥（编辑器"眼睛"查看/续用）
  revealSecret: (keyId: string) =>
    call<string | null>("reveal_secret", { keyId }, async () => `sk-mock-${keyId}`),

  // 孤儿密钥条数（密钥库里有、配置里已无对应 Key 的残留）。只读，不删。
  //
  // 刻意做成「先看数、再由用户点确认」两步：删密钥不可逆，而残留孤儿本身无害（只占空间），
  // 不值得用「启动时自动清理」去换那点整洁 —— 一旦自动删错，用户没有任何补救机会。
  countOrphanSecrets: () =>
    call<number>("count_orphan_secrets", undefined, () => mockBridge.countOrphanSecrets()),

  // 清理孤儿密钥。**破坏性操作**：后端会先备份整个密钥库，备份失败即放弃清理。
  // 返回实际清理条数。主口令锁定态下后端直接返回 0（读不到内容时「没这条」的结论不可信）。
  pruneOrphanSecrets: () =>
    call<number>("prune_orphan_secrets", undefined, () => mockBridge.pruneOrphanSecrets()),

  // 切换启用状态
  toggleKey: (keyId: string, enabled: boolean) =>
    call<void>("toggle_key", { keyId, enabled }, () => mockBridge.toggleKey(keyId, enabled)),

  // 设为该分类的主 Key（优先级 0，其余顺延）。重排规则在后端 Store::set_primary_key，
  // 因为托盘快切（FR-022）与界面共用同一套规则，前端不再自己算。
  // 返回是否真的改了（已是主则 false）。
  setPrimaryKey: (categoryId: CategoryType, keyId: string) =>
    call<boolean>("set_primary_key", { categoryId, keyId }, () =>
      mockBridge.setPrimaryKey(categoryId, keyId),
    ),

  // 上移/下移优先级。与 setPrimaryKey 同理收归后端：只动 priority、原子重编号。
  // 旧实现是前端重编号后对每条变化的 Key 各发一次 upsertKey，两个真实后果 ——
  // ① 其中一条被桌面端对外名校验拒绝时，其余已落盘 → 优先级留下重复值/空洞；
  // ② 弹出的错误是那条校验失败，与「调整顺序」毫不相干，读起来像排序功能坏了。
  // 返回是否真的改了（已在两端则 false）。
  moveKey: (categoryId: CategoryType, keyId: string, direction: "up" | "down") =>
    call<boolean>("move_key", { categoryId, keyId, direction }, () =>
      mockBridge.moveKey(categoryId, keyId, direction),
    ),

  // ---- 从 cc-switch 导入历史 Key（只读对方库；导入后不接入）----
  scanCcswitch: () =>
    call<CcSwitchScanResult>("scan_ccswitch", undefined, () => mockBridge.scanCcswitch()),
  importFromCcswitch: (sourceIds: string[]) =>
    call<CcSwitchImportReport>("import_from_ccswitch", { sourceIds }, () =>
      mockBridge.importFromCcswitch(sourceIds),
    ),

  // 拉取模型列表（FR-004）——已保存 Key 按 id 探测
  fetchModels: (keyId: string) =>
    call<ModelInfo[]>("fetch_models", { keyId }, () => mockBridge.fetchModels(keyId)),

  // 用编辑中的草稿探测模型（新增 Key 尚未保存、无真实 id 时用此）
  fetchModelsDraft: (key: ProviderKey, secret?: string) =>
    call<ModelInfo[]>("fetch_models_draft", { key, secret }, () => mockBridge.fetchModels(key.id)),

  // 健康检查（FR-011）
  checkHealth: (keyId: string) =>
    call<void>("check_health", { keyId }, () => mockBridge.checkHealth(keyId)),

  // ---- 大脑聚合 ----
  getBrainConfig: (categoryId: CategoryType) =>
    call<BrainConfig>("get_brain_config", { categoryId }, () => mockBridge.getBrainConfig(categoryId)),

  saveBrainConfig: (config: BrainConfig) =>
    call<void>("save_brain_config", { config }, () => mockBridge.saveBrainConfig(config)),

  // ---- 代理生命周期（FR-019） ----
  getProxyState: (categoryId: CategoryType) =>
    call<ProxyState>("get_proxy_state", { categoryId }, () => mockBridge.getProxyState(categoryId)),

  startProxy: (categoryId: CategoryType) =>
    call<ProxyState>("start_proxy", { categoryId }, () => mockBridge.startProxy(categoryId)),

  stopProxy: (categoryId: CategoryType) =>
    call<ProxyState>("stop_proxy", { categoryId }, () => mockBridge.stopProxy(categoryId)),

  // 设置某分类代理的首选端口（粘滞固定）：持久化 → 重启代理绑新端口 → 重写客户端 config
  setProxyPort: (categoryId: CategoryType, port: number) =>
    call<ProxyState>("set_proxy_port", { categoryId, port }, () => mockBridge.getProxyState(categoryId)),

  // 生成/写入目标工具接入配置（FR-008，会先备份，dev-hard-rules）
  applyToolConfig: (categoryId: CategoryType) =>
    call<string>("apply_tool_config", { categoryId }, () => mockBridge.applyToolConfig(categoryId)),

  // 从 .synaroute.bak 还原目标工具配置（停止代理时调用，恢复接入前状态）。
  // Codex 会一并还原 auth.json，让被占位符覆盖的官方 OAuth 登录自动回来。
  restoreToolConfig: (categoryId: CategoryType) =>
    call<string>("restore_tool_config", { categoryId }, async () => ""),

  /**
   * 切回官方：停止该分类代理并还原客户端配置，**同时保留 MCP 注册**。
   *
   * 与 stopProxy + restoreToolConfig 两步走的区别：
   * Codex 的代理设置和 MCP 在同一份 config.toml 里，整文件还原会把 mcp_servers 一起抹掉。
   * 这里后端在还原后会自动把 MCP 重注册一次（幂等：CLI/桌面端内容未变则跳过写盘）。
   * 用于「想用官方 API 聊天，但还想用大脑聚合 MCP 工具」的场景。
   */
  switchToOfficial: (categoryId: CategoryType) =>
    call<void>("switch_to_official", { categoryId }, async () => {}),

  /** 只读预览：三端路径/内容不同（CLI settings / Codex toml+auth / 桌面 config），token 已脱敏 */
  getToolConfigPreview: (categoryId: CategoryType) =>
    call<ToolConfigPreview>(
      "get_tool_config_preview",
      { categoryId },
      () => mockBridge.getToolConfigPreview(categoryId),
    ),

  // ---- 日志（FR-020） ----
  listEvents: (categoryId: CategoryType) =>
    call<EventLogEntry[]>("list_events", { categoryId }, () => mockBridge.listEvents(categoryId)),

  /** 合并全部分类的事件日志（不按分类过滤），供运行日志页连续展示；每条自带 categoryId 标签。 */
  listAllEvents: () =>
    call<EventLogEntry[]>("list_all_events", undefined, () => mockBridge.listAllEvents()),

  /** 按「分类 × Key」聚合的 token 用量（用量统计面板）。 */
  getTokenUsage: () =>
    call<TokenUsageByKey[]>("get_token_usage", undefined, () => mockBridge.tokenUsage()),

  /**
   * 按日分桶的用量（最近 90 天，最新在前），供「今日 / 本周 / 近 7 日趋势」。
   *
   * 不含尚未落盘的增量（最多落后 60s）——「今日」应把本接口的历史与
   * `getTokenUsage` 的实时总量配合使用，否则刚发的请求要等一分钟才出现。
   */
  getDailyUsage: () =>
    call<DailyUsageBucket[]>("get_daily_usage", undefined, () => mockBridge.dailyUsage()),

  /**
   * 一批模型的「最大单次输出」内置默认值（模型名 → token 数）。
   *
   * Key 编辑器拿它当占位提示（`自动 128K`），让用户看出这一项**不用填**。
   *
   * **不在前端抄一张表**：那必然与后端漂移，而漂移的形态是「界面说 128K、实际发 8192」，
   * 用户照界面判断却拿到被截断的回答。这里调的是转发时用的同一个函数。
   */
  defaultMaxOutputs: (models: string[]) =>
    call<Record<string, number>>("default_max_outputs", { models }, () =>
      // mock 环境（浏览器预览）没有后端：返回空表 → 界面退回静态占位，不报错。
      Promise.resolve({} as Record<string, number>),
    ),

  /**
   * 按「分类 × Key」聚合的用量 + 成本估算。
   *
   * 成本按 Key 估算（累加器不含模型维度），代表模型取该 Key 的兜底模型或首个模型。
   * `costNano` 为 null 表示没有可用单价 —— 界面应显示「—」而非 0，
   * 否则用户会以为这条 Key 没花钱。
   */
  getUsageWithCost: () =>
    call<UsageCostRow[]>("get_usage_with_cost", undefined, () => mockBridge.usageWithCost()),

  /**
   * 查询某个 Key 的上游余额。
   *
   * **失败不抛异常**：查不到时返回 `{ok: false, error: "原因"}`，
   * 调用方直接把 `error` 显示在卡片上。余额查不到是常态（站点没这接口、
   * 路径填错、网络抖动），用异常表达会让一次抖动炸掉整个面板。
   *
   * @param keyId - Key ID
   * @param force - 为 true 时跳过缓存检查，强制查询上游（编辑器「测试查询」按钮使用）
   */
  queryKeyBalance: (keyId: string, force?: boolean) =>
    call<BalanceResult>("query_key_balance", { keyId, force }, () => mockBridge.queryBalance(keyId)),

  /**
   * 上面那份累计量的**起算时刻**（epoch ms）。
   *
   * 累加器跨重启保留，所以「从什么时候开始算」必须由后端给出：前端拿不到（既不是本次挂载时间，
   * 也不是本次启动时间）。首次落盘前后端也会给出本次进程的起点，不会返回 null。
   */
  getUsageSince: () =>
    call<number>("get_usage_since", undefined, () => mockBridge.usageSince()),

  /**
   * 最近一次「够新」的转发失败（UX#11 的分类页常驻提示用）。
   *
   * **为什么必须走后端而不在前端从 `events` 里挑**：分类页的 5s 轮询走
   * `refreshCategory`，它只拉 keys + proxy，**不拉 events**（events 只在
   * `loadCategory` 即挂载/切分类时拉一次）。若在前端从 store 的 `events` 里筛，
   * 拿到的是挂载那一刻的快照，转发失败后不会更新——正是本项目反复防的静默失效。
   * 而把 `listAllEvents` 加进 5s 轮询又会把 P1-6 刚消掉的「每轮搬运 500 条」重新引回来。
   * 故让后端只回**一条**已剥离 trace 的事件，每轮开销恒定。
   *
   * `freshMs` 之外的旧失败返回 null——陈旧失败不该一直挂着让用户以为现在还坏着。
   */
  recentFailure: (categoryId: CategoryType, freshMs: number) =>
    call<EventLogEntry | null>("recent_failure", { categoryId, freshMs }, async () => null),

  /**
   * 按事件 id 取链路快照（用户展开某行日志时才调）。
   *
   * 列表接口不返回 trace 正文——每 2s 全量轮询 × 单条最坏 40000 字符会把界面拖卡。
   * 返回 null = 该条已被内存日志上限挤出。
   */
  getEventTrace: (eventId: string) =>
    call<RequestTrace | null>("get_event_trace", { eventId }, () =>
      mockBridge.getEventTrace(eventId),
    ),

  // ---- 设置 ----
  getSettings: () => call<AppSettings>("get_settings", undefined, () => mockBridge.getSettings()),

  /**
   * 保存**用户偏好**。入参是白名单类型 `UserPrefs` —— 后端自管字段（粘滞端口、
   * MCP 注册记录、已选模型、密钥库模式镜像、开机自启动…）在类型上就不存在。
   * 后端另有同名白名单兜底，多余的键会被静默丢弃。
   */
  saveSettings: (prefs: UserPrefs) =>
    call<void>("save_settings", { settings: prefs }, () => mockBridge.saveSettings(prefs)),

  /**
   * 开机自启动开关。**专用命令，不走 saveSettings**：它伴随系统副作用（写注册表 Run 键），
   * 而批量保存路径的入参是前端挂载时的旧快照 —— 那正是出过 P0 的地方
   * （切主题把用户刚关掉的自启动重新装回系统）。
   */
  setAutoStart: (enabled: boolean) =>
    call<void>("set_auto_start", { enabled }, () => mockBridge.setAutoStart(enabled)),

  /**
   * 「允许局域网访问代理」开关。**专用命令，不走 saveSettings**（同 setAutoStart 的理由）：
   * 它有系统副作用 —— 绑定地址 127.0.0.1 ↔ 0.0.0.0 在 `ProxyManager::start` 里一次定死，
   * 只落盘不重建监听时，关掉开关后端口仍对整个局域网敞开（界面说「已关闭」而实际没关）。
   * 后端会顺带重启正在运行的代理，返回受影响的分类数。
   */
  setLanExposure: (enabled: boolean) =>
    call<number>("set_lan_exposure", { enabled }, () => mockBridge.setLanExposure(enabled)),

  // 悬浮窗的三条命令（setFloatingWidget / setFloatingPinned / setFloatingExpanded）
  // 已随该功能于 2026-08-15 一并删除，后端也不再注册它们。见 docs/14 第十八节。

  /** 显示并聚焦主窗口（从托盘恢复）。 */
  showMainWindow: () => call<void>("show_main_window_cmd", {}, async () => {}),

  // ---- 首启向导（UX#1）----
  // 浏览器预览刻意返回 shouldShow=true，好让预览模式能验证向导布局
  // （与 UpdateBanner 不加 isTauri 守卫同一惯例）。
  getOnboardingState: () =>
    call<OnboardingState>("get_onboarding_state", undefined, async () => ({
      shouldShow: true,
      done: false,
      totalKeys: 0,
      ccswitchAvailable: true,
    })),
  setOnboardingDone: (done: boolean) =>
    call<void>("set_onboarding_done", { done }, async () => {}),
  firstRequestSince: (categoryId: CategoryType, sinceMs: number) =>
    call<FirstRequestProbe>("first_request_since", { categoryId, sinceMs }, async () => ({
      routed: false,
      failed: false,
    })),

  // 设置某分类当前选定的「对外模型名」（应用内模型下拉专用，后端自管字段）。
  // 空串清除该分类选择，回到透传客户端发来的模型名。每请求实时读取，改选即时生效。
  setActiveModel: (categoryId: CategoryType, model: string) =>
    call<void>("set_active_model", { categoryId, model }, async () => {}),

  // 设置某分类「默认推理强度」（方案 A，Codex 专用）。Codex 对自定义 provider 不发
  // reasoning.effort，故在此配置、转发时补进请求体。取值 off/minimal/low/medium/high/xhigh。
  setActiveEffort: (categoryId: CategoryType, effort: string) =>
    call<void>("set_active_effort", { categoryId, effort }, async () => {}),

  // 重建托盘菜单：改了 Key 模型列表 / 切了 active_model / 改了托盘开关后调用，
  // 让托盘 Codex 模型子菜单候选与勾选态跟最新数据一致（托盘菜单不自动跟数据变）。
  rebuildTrayMenu: () =>
    call<void>("rebuild_tray_menu", undefined, async () => {}),

  // ---- 版本与更新 ----
  getAppVersion: () => call<string>("get_app_version", undefined, () => "0.2.0"),

  checkForUpdates: () =>
    call<UpdateCheckResult>("check_for_updates", undefined, async () => ({
      status: "up_to_date",
      currentVersion: "0.2.0",
      version: null,
      notes: null,
      error: null,
    })),

  installUpdate: () =>
    call<string>("install_update", undefined, async () => "mock: no install in browser"),

  // ---- 文件目录选择 ----
  pickDirectory: () => call<string | null>("pick_directory", undefined, async () => null),

  getDefaultLogDir: () => call<string>("get_default_log_dir", undefined, async () => "C:\\AppData\\SynaRoute\\logs"),

  /**
   * 当前**实际生效**的日志目录（用户配了就是用户的）。
   * 与 getDefaultLogDir 的区别很重要：显示给用户看的必须是真正在写的那个。
   */
  getEffectiveLogDir: () =>
    call<string>("get_effective_log_dir", undefined, async () => "C:\\AppData\\SynaRoute\\logs"),

  /** 确保日志目录存在并返回绝对路径（只建不开；打开请用 `openLogDir`）。 */
  prepareLogDir: () =>
    call<string>("prepare_log_dir", undefined, async () => "C:\\AppData\\SynaRoute\\logs"),

  /**
   * 建目录**并**在系统文件管理器里打开，返回实际打开的绝对路径（UX#13）。
   *
   * 打开动作必须由**后端**执行：shell 插件对来自 JS 的 `open` 强制做 scope 正则校验
   * （默认只放行 mailto/tel/http(s)），Windows 本地路径匹配不上 → 前端调必失败。
   * 详见 `lib/openLogDir.ts` 与后端 `open_log_dir` 的注释。
   */
  openLogDir: () =>
    call<string>("open_log_dir", undefined, async () => "C:\\AppData\\SynaRoute\\logs"),

  /**
   * 导出诊断报告（UX#12）。返回保存路径，用户取消时返回 null。
   * 内容已脱敏且不含对话正文——后端在报告头部显式列出了包含/不包含什么。
   */
  exportDiagnostics: () =>
    call<string | null>("export_diagnostics", undefined, async () => null),

  // ---- 配置导入 / 导出（FR-021）----
  // 密钥段用**用户口令**加密（Argon2id + AES-GCM），而非照搬 DPAPI 密文——后者绑当前
  // Windows 账户、换机解不出，照搬等于导出一份用不了的文件。password 为空即不含密钥。

  /** 导出配置到用户选定文件。返回路径 + 跳过条数；用户取消时 null。 */
  exportConfig: (password?: string) =>
    call<ExportOutcome | null>("export_config", { password }, async () => ({
      path: "C:\\mock\\synaroute-config.json",
      undecryptable: 0,
    })),

  /** 选文件并只做校验+预检（不改任何配置）。返回 [路径, 预检]；用户取消时 null。 */
  pickAndPreviewImport: () =>
    call<[string, ImportPreview] | null>("pick_and_preview_import", undefined, async () => null),

  /** 执行导入。path 来自 pickAndPreviewImport。 */
  applyImportConfig: (path: string, mode: ImportMode, password?: string) =>
    call<ImportReport>("apply_import_config", { path, mode, password }, async () => ({
      mode,
      keysAdded: 0,
      keysOverwritten: 0,
      keysRemoved: 0,
      vendorsImported: 0,
      brainImported: 0,
      secretsImported: 0,
      secretsPruned: 0,
      warnings: ["浏览器预览模式：未实际导入"],
    })),

  // ---- 主口令增强模式（FR-018 可选增强）----
  // 默认 DPAPI（免口令、绑当前 Windows 账户）。启用后由 Argon2id 从用户口令派生密钥
  // 加密整个密钥库，每次启动需解锁一次。**真实模式的事实来源是密钥库文件本身**，
  // settings.masterPasswordEnabled 只是镜像。

  /** 读当前状态（是否启用 / 是否锁着）。 */
  getMasterPasswordState: () =>
    call<MasterPasswordState>("get_master_password_state", undefined, () =>
      mockBridge.getMasterPasswordState(),
    ),

  /** 用主口令解锁本次进程。解锁前取不到任何密钥（转发会报「需解锁」）。 */
  unlockMasterPassword: (password: string) =>
    call<void>("unlock_master_password", { password }, () =>
      mockBridge.unlockMasterPassword(password),
    ),

  /** 立即上锁（清掉进程内常驻的库密钥）。 */
  lockMasterPassword: () =>
    call<void>("lock_master_password", undefined, () => mockBridge.lockMasterPassword()),

  /** 启用主口令：把库里全部 DPAPI 密文改用口令加密。返回迁移条数。 */
  enableMasterPassword: (password: string) =>
    call<number>("enable_master_password", { password }, () =>
      mockBridge.enableMasterPassword(password),
    ),

  /** 关闭主口令：改回 DPAPI。需输入当前主口令确认。返回迁移条数。 */
  disableMasterPassword: (password: string) =>
    call<number>("disable_master_password", { password }, () =>
      mockBridge.disableMasterPassword(password),
    ),

  /** 修改主口令（验证旧口令 + 全库用新口令重新封装）。返回重新封装条数。 */
  changeMasterPassword: (oldPassword: string, newPassword: string) =>
    call<number>("change_master_password", { oldPassword, newPassword }, () =>
      mockBridge.changeMasterPassword(oldPassword, newPassword),
    ),

  // ---- 厂商预设 ----
  listVendors: () => call<Vendor[]>("list_vendors", undefined, () => mockBridge.listVendors()),

  upsertVendor: (vendor: Vendor) =>
    call<Vendor>("upsert_vendor", { vendor }, () => mockBridge.upsertVendor(vendor)),

  deleteVendor: (vendorId: string) =>
    call<void>("delete_vendor", { vendorId }, () => mockBridge.deleteVendor(vendorId)),

  // ---- 大脑聚合 V2（文件检索 + 两阶段决策） ----
  runAggregatePlan: (categoryId: CategoryType, prompt: string) =>
    call<AggregateResult>("aggregate_plan", { categoryId, prompt }, async () => ({
      resultType: "plan" as const,
      content: "Mock: 修改 src/main.ts 第 10 行...",
    })),

  runAggregateExecute: (
    categoryId: CategoryType,
    prompt: string,
    confirmedPlan: string,
    workDir?: string,
  ) =>
    call<AggregateResult>(
      "aggregate_execute",
      { categoryId, prompt, confirmedPlan, workDir },
      async () => ({
        resultType: "applied" as const,
        content: "已修改 src/main.ts",
        filesModified: ["src/main.ts"],
      }),
    ),

  retrieveFiles: (workDir: string, query: string) =>
    call<RetrievedFile[]>("retrieve_files", { workDir, query }, async () => []),

  // ---- 最近工作目录（从 Claude CLI / Codex 会话中检测） ----
  detectRecentWorkdirs: () =>
    call<RecentWorkdir[]>("detect_recent_workdirs", undefined, async () => []),

  // ---- codegraph（可选本地代码索引工具） ----
  // workDir 为空则只判可执行是否就绪，不判项目索引。
  detectCodegraph: (workDir?: string) =>
    call<CodegraphState>("detect_codegraph", { workDir }, async () => ({
      state: "notInstalled" as const,
    })),

  // 为指定项目建索引（codegraph init）。大仓库可能耗时分钟级，调用方需给 loading 态。
  codegraphInit: (workDir: string) =>
    call<string>("codegraph_init", { workDir }, async () => "浏览器预览模式：未实际建索引"),

  // ---- MCP 服务器 ----
  mcpStatus: () =>
    call<McpStatus>("mcp_status", undefined, async () => ({ running: false })),

  // 启用/停用 MCP，并自动注册到「当前活跃分类」对应工具的客户端配置（重启客户端即用）。
  // 返回启动后的实际状态（含可能因占用而 fallback 的真实端口）。
  setMcpEnabled: (categoryId: CategoryType, enabled: boolean, port: number) =>
    call<McpStatus>(
      "set_mcp_enabled",
      { categoryId, enabled, port },
      async () => ({ running: enabled, port }),
    ),

  // 手动重启 MCP 服务：先停后起，强制重新绑定端口。用于改端口后立即重绑、端口冲突排障、
  // 或客户端连不上时强制重连。大脑聚合参数改后保存即生效，不需要走这里。
  restartMcp: () =>
    call<McpStatus>("restart_mcp", undefined, async () => ({ running: true })),

  // 单分类接入大脑聚合 MCP：只给该分类写客户端配置（Codex=config.toml / CLI=~/.claude.json），
  // 不影响其它分类。服务未跑则先启动。可让 CLI 与 Codex 各自独立接入。
  registerMcpForCategory: (categoryId: CategoryType) =>
    call<McpStatus>(
      "register_mcp_for_category",
      { categoryId },
      async () => ({ running: true }),
    ),

  // 单分类断开：只从该分类客户端配置移除 synaroute，不停服务、不动其它分类。
  unregisterMcpForCategory: (categoryId: CategoryType) =>
    call<McpStatus>(
      "unregister_mcp_for_category",
      { categoryId },
      async () => ({ running: true }),
    ),
};
