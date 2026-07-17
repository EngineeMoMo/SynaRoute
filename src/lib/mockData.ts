// 浏览器环境下的 mock 数据层
// 目的：让前端脱离 Rust 后端也能独立展示与交互（npm run dev 直接看页面）。
// 数据取自需求文档 UC-002 示例场景（厂商1/2/3 + GLM 映射），使界面一打开即有真实感。
// Tauri 环境不会走到这里（见 bridge.ts 的 isTauri 判定）。
// 所有结构严格对齐 src/types.ts 的 IPC 契约。

import type {
  AppSettings,
  BrainConfig,
  CategoryType,
  EventLogEntry,
  HealthStatus,
  ModelInfo,
  ProviderKey,
  ProxyState,
  Vendor,
} from "@/types";

const now = Date.now();

/** 便捷构造模型列表 */
const models = (names: string[], ctxWindow?: number): ModelInfo[] =>
  names.map((n) => ({ realName: n, source: "fetched" as const, fetchedAt: now - 3600_000, contextWindow: ctxWindow }));

// 内存态，仅本次会话有效
const store: Record<CategoryType, ProviderKey[]> = {
  "claude-cli": [
    {
      id: "k1",
      categoryId: "claude-cli",
      name: "厂商1（官方直连）",
      vendor: "anthropic",
      baseUrl: "https://api.anthropic.com",
      protocol: "anthropic",
      hasSecret: true,
      enabled: true,
      priority: 0,
      params: { temperature: 1.0, maxTokens: 8192 },
      models: models(["opus-4-6", "opus-4-7", "opus-4-8"], 200000),
      mappings: [],
      health: { status: "up", lastChecked: now - 20_000, latencyMs: 320, failCount: 0 },
    },
    {
      id: "k2",
      categoryId: "claude-cli",
      name: "厂商2（备用中转）",
      vendor: "custom",
      baseUrl: "https://relay.example.com",
      protocol: "anthropic",
      hasSecret: true,
      enabled: true,
      priority: 1,
      params: { temperature: 1.0, maxTokens: 8192 },
      models: models(["opus-4-6", "opus-4-7", "opus-4-8", "fable-5"], 200000),
      mappings: [],
      health: { status: "up", lastChecked: now - 22_000, latencyMs: 540, failCount: 0 },
    },
    {
      id: "k3",
      categoryId: "claude-cli",
      name: "厂商3（GLM 映射）",
      vendor: "glm",
      baseUrl: "https://open.bigmodel.cn/api/paas/v4",
      protocol: "openai",
      hasSecret: true,
      enabled: true,
      priority: 2,
      params: { temperature: 1.0, maxTokens: 8192 },
      models: models(["GLM5.1", "GLM5.2"]),
      mappings: [
        { id: "m1", expectedName: "opus-4-7", realName: "GLM5.1" },
        { id: "m2", expectedName: "opus-4-8", realName: "GLM5.2" },
      ],
      health: { status: "unknown", failCount: 0 },
    },
  ],
  "claude-desktop": [],
  codex: [
    {
      id: "c1",
      categoryId: "codex",
      name: "OpenAI 官方",
      vendor: "openai",
      baseUrl: "https://api.openai.com/v1",
      protocol: "openai",
      hasSecret: true,
      enabled: true,
      priority: 0,
      params: { temperature: 1.0, maxTokens: 8192 },
      models: models(["gpt-5.6", "gpt-5.4"]),
      mappings: [],
      health: { status: "up", lastChecked: now - 15_000, latencyMs: 280, failCount: 0 },
    },
  ],
};

const brainConfigs: Record<CategoryType, BrainConfig> = {
  "claude-cli": {
    categoryId: "claude-cli",
    enabled: false,
    aggregateMode: "compressed",
    concurrencyLimit: 3,
    totalTimeoutMs: 60000,
    summarizerRef: undefined, // 缺省复用决策者
    deciderRef: "k2::opus-4-8",
    members: [
      { id: "bm1", keyId: "k1", modelName: "opus-4-8" },
      { id: "bm2", keyId: "k2", modelName: "opus-4-7" },
      { id: "bm3", keyId: "k3", modelName: "GLM5.2" },
    ],
  },
  "claude-desktop": {
    categoryId: "claude-desktop",
    enabled: false,
    aggregateMode: "compressed",
    concurrencyLimit: 3,
    totalTimeoutMs: 60000,
    members: [],
  },
  codex: {
    categoryId: "codex",
    enabled: false,
    aggregateMode: "compressed",
    concurrencyLimit: 3,
    totalTimeoutMs: 60000,
    members: [],
  },
};

const proxyStates: Record<CategoryType, ProxyState> = {
  "claude-cli": { categoryId: "claude-cli", status: "running", port: 8788 },
  "claude-desktop": { categoryId: "claude-desktop", status: "stopped", port: null },
  codex: { categoryId: "codex", status: "running", port: 8789 },
};

const events: EventLogEntry[] = [
  {
    id: "e1",
    ts: now - 60000,
    categoryId: "claude-cli",
    type: "failover",
    keyId: "k1",
    detail: "厂商1 超时（>30s），切换到厂商2",
  },
  {
    id: "e2",
    ts: now - 59000,
    categoryId: "claude-cli",
    type: "route",
    keyId: "k2",
    detail: "厂商2 成功返回 opus-4-7",
  },
  {
    id: "e3",
    ts: now - 30000,
    categoryId: "claude-cli",
    type: "health",
    keyId: "k3",
    detail: "厂商3 健康检查：状态未知（尚未探测）",
  },
  {
    id: "e4",
    ts: now - 15000,
    categoryId: "claude-cli",
    type: "request",
    keyId: "k2",
    detail: "厂商2 · claude-opus-4 → glm-4.6 · 1240ms",
    trace: {
      keyName: "厂商2",
      vendor: "zhipu",
      protocol: "openai",
      url: "https://open.bigmodel.cn/api/paas/v4/chat/completions",
      requestedModel: "claude-opus-4",
      realModel: "glm-4.6",
      requestBody: JSON.stringify(
        {
          model: "glm-4.6",
          messages: [
            { role: "user", content: "用一句话解释什么是代理服务器。" },
          ],
          max_tokens: 1024,
          temperature: 0.7,
        },
        null,
        2,
      ),
      responseBody: JSON.stringify(
        {
          id: "chatcmpl-mock-abc",
          model: "glm-4.6",
          choices: [
            {
              index: 0,
              message: {
                role: "assistant",
                content: "代理服务器是位于客户端与目标服务器之间的中转站，代客户端转发请求并返回响应。",
              },
              finish_reason: "stop",
            },
          ],
          usage: { prompt_tokens: 21, completion_tokens: 34, total_tokens: 55 },
        },
        null,
        2,
      ),
      status: 200,
      latencyMs: 1240,
      ok: true,
    },
  },
  {
    id: "e5",
    ts: now - 8000,
    categoryId: "claude-cli",
    type: "request",
    keyId: "k1",
    detail: "厂商1 · claude-opus-4 → claude-opus-4 · 320ms · 失败 HTTP 401",
    trace: {
      keyName: "厂商1",
      vendor: "anthropic",
      protocol: "anthropic",
      url: "https://api.anthropic.com/v1/messages",
      requestedModel: "claude-opus-4",
      realModel: "claude-opus-4",
      requestBody: JSON.stringify(
        {
          model: "claude-opus-4",
          messages: [{ role: "user", content: "用一句话解释什么是代理服务器。" }],
          max_tokens: 1024,
        },
        null,
        2,
      ),
      responseBody: JSON.stringify(
        {
          type: "error",
          error: { type: "authentication_error", message: "invalid x-api-key" },
        },
        null,
        2,
      ),
      status: 401,
      latencyMs: 320,
      ok: false,
    },
  },
];

let settings: AppSettings = {
  theme: "system",
  language: "zh",
  autoStart: false,
  masterPasswordEnabled: false,
  lanExposure: false,
  requestLogEnabled: false,
  healthCheckIntervalSecs: 60,
};

// 内置厂商种子（与后端 model.rs Vendor::builtin_seed 保持一致）
let vendors: Vendor[] = [
  { id: "anthropic", name: "Anthropic", defaultBaseUrl: "https://api.anthropic.com", defaultProtocol: "anthropic", builtin: true },
  { id: "openai", name: "OpenAI", defaultBaseUrl: "https://api.openai.com/v1", defaultProtocol: "openai", builtin: true },
  { id: "zhipu", name: "智谱 GLM", defaultBaseUrl: "https://open.bigmodel.cn/api/paas/v4", defaultProtocol: "openai", builtin: true },
  { id: "deepseek", name: "DeepSeek", defaultBaseUrl: "https://api.deepseek.com", defaultProtocol: "openai", builtin: true },
  { id: "moonshot", name: "月之暗面 Kimi", defaultBaseUrl: "https://api.moonshot.cn/v1", defaultProtocol: "openai", builtin: true },
  { id: "custom", name: "自定义", defaultBaseUrl: "", defaultProtocol: "anthropic", builtin: true },
];

const delay = (ms = 200) => new Promise((r) => setTimeout(r, ms));
const clone = <T>(v: T): T => JSON.parse(JSON.stringify(v));

export const mockBridge = {
  async listKeys(categoryId: CategoryType) {
    await delay();
    return clone(store[categoryId] ?? []);
  },
  async upsertKey(key: ProviderKey) {
    await delay();
    const list = store[key.categoryId];
    const idx = list.findIndex((k) => k.id === key.id);
    if (idx >= 0) list[idx] = clone(key);
    else list.push({ ...clone(key), id: key.id || `k_${list.length + 1}` });
    return clone(key);
  },
  async deleteKey(keyId: string) {
    await delay();
    for (const cat of Object.keys(store) as CategoryType[]) {
      store[cat] = store[cat].filter((k) => k.id !== keyId);
    }
  },
  async saveSecret(_keyId: string, _secret: string) {
    await delay();
    // mock：不真正存储明文
  },
  async toggleKey(keyId: string, enabled: boolean) {
    await delay();
    for (const cat of Object.keys(store) as CategoryType[]) {
      const k = store[cat].find((x) => x.id === keyId);
      if (k) k.enabled = enabled;
    }
  },
  async fetchModels(keyId: string): Promise<ModelInfo[]> {
    await delay(600);
    for (const cat of Object.keys(store) as CategoryType[]) {
      const k = store[cat].find((x) => x.id === keyId);
      if (k) return clone(k.models);
    }
    return [];
  },
  async checkHealth(keyId: string) {
    await delay(500);
    const statuses: HealthStatus[] = ["up", "up", "down"];
    for (const cat of Object.keys(store) as CategoryType[]) {
      const k = store[cat].find((x) => x.id === keyId);
      if (k) {
        k.health.status = statuses[Math.floor(Math.random() * statuses.length)];
        k.health.lastChecked = Date.now();
        k.health.latencyMs = 200 + Math.floor(Math.random() * 600);
      }
    }
  },
  async getBrainConfig(categoryId: CategoryType) {
    await delay();
    return clone(brainConfigs[categoryId]);
  },
  async saveBrainConfig(config: BrainConfig) {
    await delay();
    brainConfigs[config.categoryId] = clone(config);
  },
  async getProxyState(categoryId: CategoryType) {
    await delay();
    return clone(proxyStates[categoryId]);
  },
  async startProxy(categoryId: CategoryType) {
    await delay();
    proxyStates[categoryId].status = "running";
    if (!proxyStates[categoryId].port) proxyStates[categoryId].port = 8790;
    return clone(proxyStates[categoryId]);
  },
  async stopProxy(categoryId: CategoryType) {
    await delay();
    proxyStates[categoryId].status = "stopped";
    return clone(proxyStates[categoryId]);
  },
  async applyToolConfig(categoryId: CategoryType) {
    await delay();
    const p = proxyStates[categoryId];
    return `已（模拟）写入 ${categoryId} 接入配置，端点 http://127.0.0.1:${p.port ?? 8790}（真实环境会先备份原配置再原子写入）`;
  },
  async listEvents(categoryId: CategoryType) {
    await delay();
    return clone(events.filter((e) => e.categoryId === categoryId));
  },
  async getSettings() {
    await delay();
    return clone(settings);
  },
  async saveSettings(next: AppSettings) {
    await delay();
    settings = clone(next);
  },
  async listVendors(): Promise<Vendor[]> {
    await delay();
    return clone(vendors);
  },
  async upsertVendor(vendor: Vendor): Promise<Vendor> {
    await delay();
    const idx = vendors.findIndex((v) => v.id === vendor.id);
    const incoming = { ...clone(vendor), builtin: false };
    if (idx >= 0) {
      if (vendors[idx].builtin) throw new Error("内置厂商不可修改");
      vendors[idx] = incoming;
    } else {
      vendors.push(incoming);
    }
    return clone(vendor);
  },
  async deleteVendor(vendorId: string) {
    await delay();
    const v = vendors.find((x) => x.id === vendorId);
    if (v?.builtin) throw new Error("内置厂商不可删除");
    vendors = vendors.filter((x) => x.id !== vendorId);
  },
};
