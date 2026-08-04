// Tauri IPC 桥接层
// 关键设计：检测运行环境。在 Tauri 中走真实 invoke；在纯浏览器（npm run dev 直接看页面）
// 中降级到 mock 数据，使前端可独立展示，不阻塞于 Rust 编译。

import type {
  AggregateResult,
  AppSettings,
  BrainConfig,
  CategoryType,
  CcSwitchImportReport,
  CcSwitchScanResult,
  CodegraphState,
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
  UpdateCheckResult,
  Vendor,
} from "@/types";
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
 */
async function call<T>(cmd: string, args: Record<string, unknown> | undefined, mock: () => Promise<T> | T): Promise<T> {
  if (isTauri()) {
    return tauriInvoke<T>(cmd, args);
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

  // 把 Max Tokens 一次应用到该分类下全部 Key（含已停用的：否则日后重新启用又带回旧值）。
  // 返回实际改动条数 —— 全都已是该值时为 0，前端据此如实提示，不谎报「已保存」。
  applyMaxTokensToCategory: (categoryId: CategoryType, maxTokens: number) =>
    call<number>("apply_max_tokens_to_category", { categoryId, maxTokens }, () =>
      mockBridge.applyMaxTokensToCategory(categoryId, maxTokens),
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

  saveSettings: (settings: AppSettings) =>
    call<void>("save_settings", { settings }, () => mockBridge.saveSettings(settings)),

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

  /** 确保日志目录存在并返回绝对路径；打开动作由调用方走 shell 插件（UX#13）。 */
  prepareLogDir: () =>
    call<string>("prepare_log_dir", undefined, async () => "C:\\AppData\\SynaRoute\\logs"),

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
