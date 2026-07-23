// Tauri IPC 桥接层
// 关键设计：检测运行环境。在 Tauri 中走真实 invoke；在纯浏览器（npm run dev 直接看页面）
// 中降级到 mock 数据，使前端可独立展示，不阻塞于 Rust 编译。

import type {
  AggregateResult,
  AppSettings,
  BrainConfig,
  CategoryType,
  EventLogEntry,
  McpStatus,
  ModelInfo,
  ProviderKey,
  ProxyState,
  RecentWorkdir,
  RetrievedFile,
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

  // 生成/写入目标工具接入配置（FR-008，会先备份，dev-hard-rules）
  applyToolConfig: (categoryId: CategoryType) =>
    call<string>("apply_tool_config", { categoryId }, () => mockBridge.applyToolConfig(categoryId)),

  // ---- 日志（FR-020） ----
  listEvents: (categoryId: CategoryType) =>
    call<EventLogEntry[]>("list_events", { categoryId }, () => mockBridge.listEvents(categoryId)),

  // ---- 设置 ----
  getSettings: () => call<AppSettings>("get_settings", undefined, () => mockBridge.getSettings()),

  saveSettings: (settings: AppSettings) =>
    call<void>("save_settings", { settings }, () => mockBridge.saveSettings(settings)),

  // ---- 版本与更新 ----
  getAppVersion: () => call<string>("get_app_version", undefined, () => "0.1.0"),

  checkForUpdates: () => call<string | null>("check_for_updates", undefined, async () => null),

  // ---- 文件目录选择 ----
  pickDirectory: () => call<string | null>("pick_directory", undefined, async () => null),

  getDefaultLogDir: () => call<string>("get_default_log_dir", undefined, async () => "C:\\AppData\\SynaRoute\\logs"),

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
};
