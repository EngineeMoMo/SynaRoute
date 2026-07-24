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
export interface EventLogEntry {
  id: string;
  ts: number;
  categoryId: CategoryType;
  type: "route" | "failover" | "health" | "aggregate" | "error" | "request" | "mcp" | "config";
  keyId?: string;
  detail: string;
  trace?: RequestTrace; // request / mcp 类型带完整链路快照
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
  masterPasswordEnabled: boolean; // 加密增强：主口令（默认关，DPAPI 免口令）
  lanExposure: boolean; // 局域网暴露（默认关，NFR-007）
  requestLogEnabled: boolean; // 调用模型日志（默认关，开启后记录每次转发）
  healthCheckIntervalSecs: number;
  logDir?: string;
  mcpEnabled?: boolean; // 内置 MCP 服务器：开启即随应用启动
  mcpPort?: number; // MCP 服务器端口（默认 9527，占用时自动向上找空闲端口）
  upstreamRetryEnabled?: boolean; // 上游临时错误（502/503/504/429/连接失败）自动重试（默认开）
  healthProbeRealCompletion?: boolean; // 健康探测用真实补全请求（默认关，消耗少量额度）
  aggregateTraceEnabled?: boolean; // 大脑聚合详细快照：开启后落盘成员答案/汇总/决策者入参出参（默认关，避免大 IO）
}

/** MCP 服务器运行状态（供设置页展示连接指示灯与故障原因） */
export interface McpStatus {
  running: boolean;
  port?: number; // 实际绑定端口（可能与配置端口不同——占用时自动 fallback）
  lastError?: string; // 最近一次启动失败原因（成功时为空）
}
