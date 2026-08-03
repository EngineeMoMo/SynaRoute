// SynaRoute 前端类型定义 —— 与 Rust 后端 IPC 契约对齐（架构文档 §8.2 / §9 ipc 模块）

import type { Lang } from "@/lib/i18n";
export type { Lang };

/** 三个目标工具分类 */
export type CategoryType = "claude-cli" | "claude-desktop" | "codex";

/** 上游协议 */
export type Protocol = "anthropic" | "openai_chat" | "openai_responses";

/** 协议的展示名（UI 徽标/卡片用）。 */
export function protocolLabel(p: Protocol): string {
  switch (p) {
    case "anthropic":
      return "Anthropic";
    case "openai_chat":
      return "OpenAI Chat";
    case "openai_responses":
      return "OpenAI Responses";
  }
}

/** 厂商预设模型条目（取自 cc-switch 的 modelCatalog）：上游不暴露模型接口时供一键导入 */
export interface PresetModel {
  realName: string;
  displayName?: string;
  contextWindow?: number;
}

/** 厂商预设（FR-002 扩展）：选中后自动填充 Key 的 baseUrl / protocol */
export interface Vendor {
  id: string; // slug，如 "anthropic"、"zhipu"；ProviderKey.vendor 存此值
  name: string; // 显示名
  defaultBaseUrl: string; // 选中后预填的 Base URL
  defaultProtocol: Protocol;
  builtin: boolean; // 内置项：只读，不可编辑/删除
  icon?: string; // 自定义图标（data-URL）；内置厂商为空，走品牌图标启发式
  presetModels?: PresetModel[]; // 内置预设模型清单：上游不暴露模型接口时供一键导入
}

/** 路由模式（由启用 Key 数量与聚合开关自动判定，FR-007） */
export type RoutingMode = "direct" | "failover" | "brain";

/** Key 健康状态 */
export type HealthStatus = "up" | "down" | "unknown" | "checking";

/** 聚合方式：A 压缩汇总 / B 全量上下文（CON-4） */
export type AggregateMode = "compressed" | "full";

/** 模型映射：期望名 → 真实名（FR-006） */
export interface ModelMapping {
  id: string;
  expectedName: string; // 工具请求的模型名，如 opus-4-7
  realName: string; // 厂商真实模型名，如 GLM5.1
}

/** 单条厂商 Key 配置（FR-002 / FR-005 / FR-006） */
export interface ProviderKey {
  id: string;
  categoryId: CategoryType;
  name: string; // 备注名
  vendor: string; // 厂商标识
  baseUrl: string;
  protocol: Protocol;
  hasSecret: boolean; // 是否已配置密钥（密钥本身不下发前端，NFR-006）
  enabled: boolean;
  priority: number; // 故障转移优先级，越小越优先（FR-010）
  headersJson?: string; // 自定义请求头（JSON 字符串）
  params: KeyParams;
  models: ModelInfo[];
  mappings: ModelMapping[];
  /** 默认兜底模型（可选）：故障转移到本 Key 时，请求模型既非映射期望名也非本 Key 真实模型名，则改用它（FR-006） */
  defaultModel?: string;
  /** 三档快捷映射（取自 cc-switch 的 haiku/sonnet/opus 语义）：Claude Code 按任务发家族模型名，
   *  配了对应档位即在运行时代理改写为上游真实名。与 mappings 并存，精确映射优先级更高。 */
  tierHaiku?: string;
  tierSonnet?: string;
  tierOpus?: string;
  health: HealthState;
}

/** Key 请求参数（FR-005） */
export interface KeyParams {
  temperature?: number;
  maxTokens?: number;
  topP?: number;
  timeoutMs?: number;
}

/** 模型信息（FR-004） */
export interface ModelInfo {
  realName: string;
  source: "fetched" | "manual";
  fetchedAt?: number;
  contextWindow?: number;
}

/** 健康状态（FR-011 / FR-012） */
export interface HealthState {
  status: HealthStatus;
  lastChecked?: number;
  latencyMs?: number;
  failCount: number;
  breakerUntil?: number; // 熔断结束时间戳；存在且未到则处于熔断中
}

/** 大脑聚合配置（FR-013 ~ FR-017） */
export interface BrainConfig {
  categoryId: CategoryType;
  enabled: boolean;
  aggregateMode: AggregateMode;
  concurrencyLimit: number; // 并发上限（默认 3-5）
  totalTimeoutMs: number; // 总超时（默认 60000）
  summarizerRef?: string; // 汇总模型：keyId::modelName，缺省复用决策者
  deciderRef?: string; // 决策者：keyId::modelName（必填才能开启）
  members: BrainMember[];
  workDir?: string; // 文件检索的工作目录
  maxContextTokens?: number; // 注入文件 token 上限（默认 50000）
  retrievalEnabled: boolean; // 是否启用文件检索
  autoFollowActive?: boolean; // 自动跟随最近活动项目（每次运行时取最新工作目录，忽略 workDir）
  /**
   * 是否给参与者一组只读检索工具（read_file / grep / list_dir / codegraph_query）。
   * 默认关：每轮工具调用都要重发完整历史，额度消耗显著高于单轮。
   */
  toolsEnabled?: boolean;
  /** 工具调用轮数上限，后端 clamp 到 2~12（默认 6） */
  maxToolRounds?: number;
  /** 工具历史字符预算：累计超过就把较早轮次结果压成占位；后端 clamp 到 8000~400000（默认 60000）；0=关闭裁剪 */
  toolCtxBudgetChars?: number;
  /** 单次工具结果字符上限：后端 clamp 到 1000~40000（默认 8000）；0=用默认 */
  toolResultCapChars?: number;
}

/** 聚合运行结果 */
export interface AggregateResult {
  resultType: "plan" | "applied" | "error";
  content: string;
  filesModified?: string[];
  /** Phase1 定下的工作目录，Phase2 执行时回传以避免 auto-follow 目录漂移 */
  workDir?: string;
}

/** 文件检索结果 */
export interface RetrievedFile {
  path: string;
  content: string;
  source: "grep" | "codegraph";
}

/** 最近使用过的工作目录（从各工具会话反推） */
export interface RecentWorkdir {
  path: string; // 绝对路径
  source: "claude-cli" | "codex" | "claude-desktop"; // 来源工具
  lastUsed: number; // 时间戳(ms)
  label?: string; // 展示用简短名（可选，通常为目录名）
}

/**
 * codegraph（可选的本地代码索引工具）可用状态。
 *
 * 四态各对应不同的用户动作，故必须区分「没装」与「装了但项目没索引」：
 * - notInstalled：给安装入口
 * - stranded：能调但不在 PATH（常见于 nvm 升级后旧版本目录残留），提示重装
 * - notIndexed：可执行就绪、项目缺 .codegraph/，给建索引按钮
 * - ready：可用，检索走符号级链路
 */
export type CodegraphState =
  | { state: "notInstalled" }
  | { state: "stranded"; path: string; version: string }
  | { state: "notIndexed"; path: string; version: string }
  | { state: "ready"; path: string; version: string; nodes?: number | null; edges?: number | null };

/** 聚合成员（FR-014） */
export interface BrainMember {
  id: string;
  keyId: string;
  modelName: string;
}

/** 代理运行状态（FR-019） */
export interface ProxyState {
  categoryId: CategoryType;
  port: number | null;
  status: "running" | "stopped" | "error";
  message?: string;
}

/** 目标工具配置只读预览（按分类三端分离，禁止混用字段语义） */
export interface ToolConfigFilePreview {
  path: string;
  exists: boolean;
  format: "json" | "toml" | "text" | string;
  content?: string | null;
}

export interface ToolConfigPreview {
  categoryId: CategoryType;
  /** 本分类写哪些文件、不写哪些 */
  summary: string;
  files: ToolConfigFilePreview[];
  /** 本分类是否已接入 MCP 大脑聚合（目标配置里已含 synaroute MCP 段），与全局开关无关 */
  mcpRegistered: boolean;
  /** 仅桌面端：接入已被其他工具（如 cc-switch 重写 _meta.json）接管时的说明；未被接管则不下发 */
  takeoverWarning?: string;
}

/** 检查更新结构化结果（与后端 UpdateCheckResult 对齐） */
export type UpdateCheckStatus = "available" | "up_to_date" | "error";

export interface UpdateCheckResult {
  status: UpdateCheckStatus;
  currentVersion: string;
  version?: string | null;
  notes?: string | null;
  error?: string | null;
}

/** 一次代理转发的完整链路快照，供「调用模型日志」在页面上展开调试 */
export interface RequestTrace {
  keyName: string; // 命中的 Key 名称
  vendor: string; // 命中的厂商标识
  protocol: Protocol; // 目标上游协议
  url: string; // 实际请求的上游完整 URL
  requestedModel: string; // 下游请求的模型名（映射前）
  realModel: string; // 映射后实际发往上游的模型名
  requestBody: string; // 发往上游的请求体（已映射/转换，不含鉴权头）
  responseBody: string; // 上游返回体（成功为内容，失败为错误体）
  status?: number; // 上游 HTTP 状态码（连接失败时缺省）
  latencyMs: number; // 本次耗时（毫秒）
  ok: boolean; // 是否成功（2xx）
}

/** 事件日志条目（FR-020，敏感信息已脱敏） */
/** 一次上游调用的 token 用量（两家协议字段名已在后端归一）。 */
export interface TokenUsage {
  /** 输入（prompt）token */
  input: number;
  /** 输出（completion）token */
  output: number;
  /** 命中缓存的输入 token（为 0 时后端不下发该字段） */
  cacheRead?: number;
}

export interface EventLogEntry {
  id: string;
  ts: number;
  categoryId: CategoryType;
  type: "route" | "failover" | "health" | "aggregate" | "error" | "request" | "mcp" | "config";
  keyId?: string;
  detail: string;
  /**
   * 连续同类事件被折叠的条数（1 = 未折叠，UI 不显示计数）。
   *
   * 高频转发下「同 Key、同模型、都成功」的记录只是延迟不同，逐条列出会瞬间刷屏。
   * 后端把**连续**的同类记录合成一条并计数；中间插进任何别的事件就重新起一条，
   * 时间线不会被压扁到看不出穿插关系。日志文件仍逐条完整写。
   */
  repeat?: number;
  /**
   * 本次调用的 token 用量（上游返回了才有）。
   *
   * 折叠条目上是**累计值**（N 次调用之和）——每次都真实烧了额度，
   * 只留最后一次会让总量看着远小于实际消耗。
   */
  usage?: TokenUsage;
  /**
   * 该条是否带链路快照（可展开看详情）。
   *
   * 列表接口**不返回 trace 正文**：日志页每 2s 全量轮询，而单条 trace 的请求/响应体各上限
   * 20000 字符，500 条满载约 19 MB —— 每 2s 搬一次会把界面拖卡，而正文只在用户展开某行时才看。
   * 故列表只带这个布尔位，展开时调 `api.getEventTrace(id)` 单取一条。
   */
  hasTrace?: boolean;
  /** 链路快照。仅 `getEventTrace` 单取时有；列表返回的条目此字段恒为 undefined */
  trace?: RequestTrace;
}

/** 主题偏好 */
export type ThemePref = "light" | "dark" | "system";

/** 厂商预设：在 Key 编辑器中选择后自动预填 baseUrl / protocol */
export interface Vendor {
  id: string;
  name: string;
  defaultBaseUrl: string;
  defaultProtocol: Protocol;
  builtin: boolean; // 内置项不可编辑/删除
}

/** 应用设置 */
export interface AppSettings {
  theme: ThemePref;
  language: Lang; // 界面语言（默认 zh）
  autoStart: boolean;
  masterPasswordEnabled: boolean; // 加密增强：主口令（默认关，DPAPI 免口令）。**只是 UI 镜像**——真实模式记在 secrets.enc 的 master 头部里，后端自管，走 enable/disableMasterPassword 专用命令，saveSettings 不得覆盖
  lanExposure: boolean; // 局域网暴露（默认关，NFR-007）
  requestLogEnabled: boolean; // 调用模型日志（默认关，开启后记录每次转发）
  logDownstreamRawEnabled?: boolean; // 诊断：request 日志额外记「下游原始请求体」（转换前，体量大，默认关，仅排障用）
  healthCheckIntervalSecs: number;
  /** 一次请求内故障转移的总时间预算（毫秒）。0 = 关闭该约束。默认 90000。 */
  failoverTotalBudgetMs: number;
  logDir?: string;
  mcpEnabled?: boolean; // 内置 MCP 服务器：开启即随应用启动
  mcpPort?: number; // MCP 服务器端口（默认 9527，占用时自动向上找空闲端口）
  upstreamRetryEnabled?: boolean; // 上游临时错误（502/503/504/429/连接失败）自动重试（默认开）
  healthProbeRealCompletion?: boolean; // 健康探测用真实补全请求（默认关，消耗少量额度）
  healthProbeTestMessages?: string[]; // 真实补全探测的测试消息全局列表：每次探测随机取一条，留空回退内置 "hi"
  aggregateTraceEnabled?: boolean; // 大脑聚合详细快照：开启后落盘成员答案/汇总/决策者入参出参（默认关，避免大 IO）
  activeModels?: Record<string, string>; // 各分类当前选定的对外模型名（key=分类）。借鉴 EchoBird：客户端菜单拉不到中转模型时，在应用内选、代理转发时覆盖。后端自管，走 setActiveModel 专用命令
  activeEfforts?: Record<string, string>; // 各分类默认推理强度（key=分类，值 minimal/low/medium/high/xhigh）。Codex 对自定义 provider 不发 reasoning.effort，故在此配置、转发时注入。后端自管，走 setActiveEffort 专用命令
  trayModelSwitchEnabled?: boolean; // 托盘快切 Codex 模型子菜单开关（默认开）。开启后右键托盘可直接切 Codex 当前对外模型，免打开主窗口
  proxyPorts?: Record<string, number>; // 各分类代理首选监听端口（key=分类）。粘滞固定端口：默认 CLI=47100/Codex=47101/Desktop=47102，改端口走 setProxyPort（重启代理+重写客户端 config）
}

/** MCP 服务器运行状态（供设置页展示连接指示灯与故障原因） */
export interface McpStatus {
  running: boolean;
  port?: number; // 实际绑定端口（可能与配置端口不同——占用时自动 fallback）
  lastError?: string; // 最近一次启动失败原因（成功时为空）
}

// ---- 从 cc-switch 导入历史 Key ----
//
// cc-switch（另一款 Key 切换工具）把各端供应商档存在 ~/.cc-switch/cc-switch.db。
// SynaRoute 只读它，映射成自己的 Key + 加密密钥。**导入不接入**：不改任何客户端配置。

/** 一条可导入（或明确不可导入）的候选 */
export interface CcSwitchCandidate {
  sourceId: string; // cc-switch providers.id，导入时按它回查
  appType: string; // cc-switch 原值：claude / claude-desktop / codex / gemini
  categoryId: CategoryType | null; // 映射到的分类；null = 无对应分类
  name: string;
  baseUrl: string;
  protocol: Protocol | null;
  defaultModel: string | null; // codex 的 config.toml 顶层 model
  isCurrent: boolean; // 该档在 cc-switch 里是否为当前生效项
  secretMasked: string; // 掩码（前6…后4 (长度)）；明文不出后端
  duplicateOf: string | null; // 与 SynaRoute 已有 Key 重复时给出对方名称
  skipReason: string | null; // 不可导入原因；为 null 才可导入
}

/** 扫描结果 */
export interface CcSwitchScanResult {
  dbPath: string; // 数据来源，供 UI 展示
  total: number; // 库里 providers 总数
  candidates: CcSwitchCandidate[];
}

/** 单条导入结局 */
export interface CcSwitchImportOutcome {
  sourceId: string;
  name: string;
  status: "imported" | "skipped" | "failed";
  detail: string;
  keyId: string | null;
}

/** 导入汇总 */
export interface CcSwitchImportReport {
  imported: number;
  skipped: number;
  failed: number;
  outcomes: CcSwitchImportOutcome[];
}

// ---- 配置导入 / 导出（FR-021，后端 src-tauri/src/portable.rs）----

/**
 * 导入模式（用户当场选）。
 * - merge：同 id 覆盖、新 id 新增、本机多出的保留（不删任何东西）
 * - replace：清空后导入，还原到导出那一刻（会删掉导出后新建的条目，故后端会先备份 config）
 */
export type ImportMode = "merge" | "replace";

/**
 * 导出结果。
 *
 * `undecryptable` 必须展示给用户：解不出的 Key 是被**跳过**而非让整次导出失败
 * （某个 Key 的密文可能因换过 Windows 账户而失效，不该因此丢掉整份配置的导出价值）。
 * 若不提示，用户会拿着「声称含密钥、实际少几条」的文件走，到新机器才发现。
 */
export interface ExportOutcome {
  path: string;
  /** 密钥解不出、已跳过的 Key 数（0 表示全部带上了） */
  undecryptable: number;
}

/** 导入前的只读预检：让用户在真正写盘之前看到会发生什么 */
/** 一处「id 相同但看起来根本不是同一条 Key」的导入冲突（见 ImportPreview.suspiciousConflicts）。 */
export interface SuspiciousConflict {
  id: string;
  /** 本机这条（将被覆盖的一方） */
  localName: string;
  localBaseUrl: string;
  /** 文件里这条（覆盖方） */
  incomingName: string;
  incomingBaseUrl: string;
}

export interface ImportPreview {
  formatVersion: number;
  /** 产出该文件的 SynaRoute 版本（仅供人看） */
  appVersion: string;
  exportedAt: string;
  /** 文件里有多少条 Key */
  keyCount: number;
  /** 其中与本机 id 重复的（merge 会覆盖它们） */
  conflictingKeys: number;
  /**
   * id 相同但名字与地址都不同的条目：极可能是历史时间戳 id 跨机碰撞，
   * 导入会把一条完全无关的本机 Key 覆盖成对方的配置。逐条显式警告，不混进计数。
   */
  suspiciousConflicts: SuspiciousConflict[];
  /** 本机独有、replace 模式会被删掉的 Key 数 */
  localOnlyKeys: number;
  vendorCount: number;
  brainCount: number;
  /** 文件是否含密钥段（含则导入时需要口令） */
  hasSecrets: boolean;
}

/** 导入结果报告 */
export interface ImportReport {
  mode: ImportMode;
  keysAdded: number;
  keysOverwritten: number;
  keysRemoved: number;
  vendorsImported: number;
  brainImported: number;
  secretsImported: number;
  /** replace 模式下导入前备份的 config 路径 */
  backupPath?: string;
  /** 非致命提示（如「需重新录入密钥」） */
  warnings: string[];
}

/**
 * 主口令增强模式的运行时状态（FR-018 可选增强）。
 *
 * `enabled` 的判据是**密钥库文件里有没有 master 头部**，不是 settings 里那个字段——
 * 后者只是 UI 镜像，启动时会按库对账。
 */
export interface MasterPasswordState {
  /** 是否处于主口令模式 */
  enabled: boolean;
  /** 是否锁着（主口令模式但本次进程还没解锁）。DPAPI 模式恒 false */
  locked: boolean;
}
