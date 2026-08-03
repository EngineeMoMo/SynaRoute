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
  MasterPasswordState,
  ModelInfo,
  ProviderKey,
  ProxyState,
  Vendor,
} from "@/types";

const now = Date.now();

/** 主口令模式的浏览器预览态：默认关（与真实默认一致），启用后记住口令用于校验。 */
let masterMock: MasterPasswordState = { enabled: false, locked: false };
let masterPw = "";

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
      protocol: "openai_chat",
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
      protocol: "openai_responses",
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
    summarizerRef: undefined,
    deciderRef: "k2::opus-4-8",
    members: [
      { id: "bm1", keyId: "k1", modelName: "opus-4-8" },
      { id: "bm2", keyId: "k2", modelName: "opus-4-7" },
      { id: "bm3", keyId: "k3", modelName: "GLM5.2" },
    ],
    retrievalEnabled: false,
    toolsEnabled: false,
    maxToolRounds: 6,
  },
  "claude-desktop": {
    categoryId: "claude-desktop",
    enabled: false,
    aggregateMode: "compressed",
    concurrencyLimit: 3,
    totalTimeoutMs: 60000,
    members: [],
    retrievalEnabled: false,
    toolsEnabled: false,
    maxToolRounds: 6,
  },
  codex: {
    categoryId: "codex",
    enabled: false,
    aggregateMode: "compressed",
    concurrencyLimit: 3,
    totalTimeoutMs: 60000,
    members: [],
    retrievalEnabled: false,
    toolsEnabled: false,
    maxToolRounds: 6,
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
    // 折叠计数：验证「×N」徽标的渲染（真实后端对连续同类成功记录会这样合并）
    repeat: 7,
    // 用量在折叠条目上是**累计值**（7 次之和）。这里刻意给一个上万的输入量，
    // 用来验证 ↑/↓ 徽标的 k 缩写与 tooltip 渲染。
    usage: { input: 128400, output: 3120, cacheRead: 96000 },
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
    id: "e3b",
    ts: now - 28000,
    categoryId: "claude-cli",
    type: "config",
    detail: "已注册 MCP 到 Claude：C:\\Users\\me\\.claude.json（http://127.0.0.1:9527/mcp），重启客户端生效",
  },
  {
    id: "e3c",
    ts: now - 20000,
    categoryId: "claude-cli",
    type: "mcp",
    detail: "synaroute_ai · C:\\proj\\demo · 3个参与者 · 5个文件 · 8200ms",
    trace: {
      keyName: "synaroute_ai",
      vendor: "mcp",
      protocol: "anthropic",
      url: "-",
      requestedModel: "-",
      realModel: "-",
      requestBody: "分析当前项目的登录模块有哪些安全隐患",
      responseBody: "## 🧠 SynaRoute 多模型聚合分析\n\n1. 密码明文比对……\n2. 缺少速率限制……",
      latencyMs: 8200,
      ok: true,
    },
  },
  {
    id: "e4",
    ts: now - 15000,
    categoryId: "claude-cli",
    type: "request",
    keyId: "k2",
    detail: "厂商2 · claude-opus-4 → glm-4.6 · 1240ms",
    // 小额用量：验证 ↑/↓ 徽标在不足 1 万时不走 k 缩写（直接显示原数）。
    usage: { input: 21, output: 34 },
    trace: {
      keyName: "厂商2",
      vendor: "zhipu",
      protocol: "openai_chat",
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
  aggregateTraceEnabled: false,
  healthCheckIntervalSecs: 60,
  mcpEnabled: false,
  mcpPort: 9527,
};

// 内置厂商种子（与后端 model.rs Vendor::builtin_seed 保持一致）
let vendors: Vendor[] = [
  { id: "anthropic", name: "Anthropic", defaultBaseUrl: "https://api.anthropic.com", defaultProtocol: "anthropic", builtin: true, presetModels: [
    { realName: "claude-opus-4-5", displayName: "Claude Opus 4.5", contextWindow: 200_000 },
    { realName: "claude-sonnet-4-5", displayName: "Claude Sonnet 4.5", contextWindow: 200_000 },
    { realName: "claude-haiku-4-5", displayName: "Claude Haiku 4.5", contextWindow: 200_000 },
  ] },
  { id: "openai", name: "OpenAI", defaultBaseUrl: "https://api.openai.com/v1", defaultProtocol: "openai_responses", builtin: true, presetModels: [
    { realName: "gpt-5.5", displayName: "GPT-5.5", contextWindow: 400_000 },
    { realName: "gpt-5", displayName: "GPT-5", contextWindow: 400_000 },
    { realName: "gpt-4o", displayName: "GPT-4o", contextWindow: 128_000 },
  ] },
  { id: "zhipu", name: "智谱 GLM", defaultBaseUrl: "https://open.bigmodel.cn/api/paas/v4", defaultProtocol: "openai_chat", builtin: true, presetModels: [
    { realName: "glm-4.6", displayName: "GLM-4.6", contextWindow: 200_000 },
    { realName: "glm-4.5", displayName: "GLM-4.5", contextWindow: 128_000 },
    { realName: "glm-4.5-air", displayName: "GLM-4.5-Air", contextWindow: 128_000 },
  ] },
  { id: "deepseek", name: "DeepSeek", defaultBaseUrl: "https://api.deepseek.com", defaultProtocol: "openai_chat", builtin: true, presetModels: [
    { realName: "deepseek-chat", displayName: "DeepSeek Chat", contextWindow: 128_000 },
    { realName: "deepseek-reasoner", displayName: "DeepSeek Reasoner", contextWindow: 128_000 },
  ] },
  { id: "moonshot", name: "月之暗面 Kimi", defaultBaseUrl: "https://api.moonshot.cn/v1", defaultProtocol: "openai_chat", builtin: true, presetModels: [
    { realName: "kimi-k2-0905-preview", displayName: "Kimi K2", contextWindow: 256_000 },
    { realName: "moonshot-v1-128k", displayName: "Moonshot v1 128k", contextWindow: 128_000 },
  ] },
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

  // 与后端 Store::set_primary_key 同规则：目标提到队首，整列重编号为连续 0,1,2…
  // 浏览器预览态也要能看到「设为主」的效果，否则 npm run dev 下点了没反应像坏了。
  async setPrimaryKey(categoryId: CategoryType, keyId: string) {
    await delay();
    const list = store[categoryId] ?? [];
    if (!list.some((k) => k.id === keyId)) return false;
    const ordered = [...list].sort((a, b) => a.priority - b.priority);
    const idx = ordered.findIndex((k) => k.id === keyId);
    const [target] = ordered.splice(idx, 1);
    ordered.unshift(target);
    let changed = false;
    ordered.forEach((k, i) => {
      const live = list.find((x) => x.id === k.id);
      if (live && live.priority !== i) {
        live.priority = i;
        changed = true;
      }
    });
    return changed;
  },

  // 批量设置 Max Tokens：与后端同语义 —— 含已停用的 Key、幂等（无改动返 0）。
  async applyMaxTokensToCategory(categoryId: CategoryType, maxTokens: number) {
    await delay();
    if (maxTokens <= 0) throw new Error("Max Tokens 不能为 0（上游会直接拒绝请求）");
    const list = store[categoryId] ?? [];
    let changed = 0;
    for (const k of list) {
      if (k.params.maxTokens !== maxTokens) {
        k.params = { ...k.params, maxTokens };
        changed++;
      }
    }
    return changed;
  },

  // 浏览器预览态：给出一份覆盖各种分支的假候选（可导入 / 重复 / 官方档 / 不支持端），
  // 让 UI 的每种状态都能在 npm run dev 下被看到。
  async scanCcswitch() {
    await delay();
    return {
      dbPath: "C:\\Users\\demo\\.cc-switch\\cc-switch.db",
      total: 4,
      candidates: [
        {
          sourceId: "demo-claude",
          appType: "claude",
          categoryId: "claude-cli" as CategoryType,
          name: "Sub2API",
          baseUrl: "https://sub.example.com",
          protocol: "anthropic" as const,
          defaultModel: null,
          isCurrent: true,
          secretMasked: "sk-abc…1111 (48)",
          duplicateOf: null,
          skipReason: null,
        },
        {
          sourceId: "demo-codex",
          appType: "codex",
          categoryId: "codex" as CategoryType,
          name: "公益站",
          baseUrl: "https://muyuan.example/v1",
          protocol: "openai_responses" as const,
          defaultModel: "gpt-5.6-sol",
          isCurrent: false,
          secretMasked: "sk-def…3333 (48)",
          duplicateOf: null,
          skipReason: null,
        },
        {
          sourceId: "demo-dup",
          appType: "claude-desktop",
          name: "百倍（已存在）",
          categoryId: "claude-desktop" as CategoryType,
          baseUrl: "https://sub.example.com",
          protocol: "anthropic" as const,
          defaultModel: null,
          isCurrent: false,
          secretMasked: "sk-abc…1111 (48)",
          duplicateOf: "百倍",
          skipReason: "SynaRoute 里已有同站点同密钥的 Key",
        },
        {
          sourceId: "demo-official",
          appType: "codex",
          categoryId: "codex" as CategoryType,
          name: "OpenAI Official",
          baseUrl: "",
          protocol: null,
          defaultModel: null,
          isCurrent: false,
          secretMasked: "",
          duplicateOf: null,
          skipReason: "ChatGPT 官方登录档（只有 OAuth token，无 API Key）",
        },
      ],
    };
  },
  async importFromCcswitch(sourceIds: string[]) {
    await delay();
    return {
      imported: sourceIds.length,
      skipped: 0,
      failed: 0,
      outcomes: sourceIds.map((id) => ({
        sourceId: id,
        name: id,
        status: "imported" as const,
        detail: "已导入（预览态模拟）",
        keyId: `k_mock_${id}`,
      })),
    };
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
  async getToolConfigPreview(categoryId: CategoryType) {
    await delay();
    const p = proxyStates[categoryId];
    const port = p.port ?? 8790;
    if (categoryId === "codex") {
      return {
        categoryId,
        summary:
          "Codex：config.toml + auth.json。写入 model_provider=synaroute 与 OPENAI_API_KEY 占位。不写 ANTHROPIC_*。",
        files: [
          {
            path: "~/.codex/config.toml",
            exists: true,
            format: "toml",
            content: `model_provider = "synaroute"\nmodel = "gpt-5"\n\n[model_providers.synaroute]\nbase_url = "http://127.0.0.1:${port}/v1"\nwire_api = "responses"\nrequires_openai_auth = true\n`,
          },
          {
            path: "~/.codex/auth.json",
            exists: true,
            format: "json",
            content: '{\n  "OPENAI_API_KEY": "***"\n}\n',
          },
        ],
        mcpRegistered: false,
      };
    }
    if (categoryId === "claude-desktop") {
      return {
        categoryId,
        summary:
          "Claude 桌面端（3p 部署模式）：两个 claude_desktop_config.json 写 deploymentMode=3p，Claude-3p/configLibrary 里写 gateway 档（inferenceGatewayBaseUrl 指向本机代理 + 占位 key + bearer + 模型清单）并登记进 _meta。凭据预填齐即跳过 get-started。与 cc-switch 用独立档共存。不写 CLI 的 settings.json。",
        files: [
          {
            path: "%LOCALAPPDATA%/Claude/claude_desktop_config.json",
            exists: true,
            format: "json",
            content: `{\n  "deploymentMode": "3p"\n}\n`,
          },
          {
            path: "%LOCALAPPDATA%/Claude-3p/claude_desktop_config.json",
            exists: true,
            format: "json",
            content: `{\n  "deploymentMode": "3p",\n  "preferences": { "…": "（用户既有偏好，原样保留）" }\n}\n`,
          },
          {
            path: "%LOCALAPPDATA%/Claude-3p/configLibrary/00000000-0000-4000-8000-000053796e61.json",
            exists: true,
            format: "json",
            content: `{\n  "inferenceProvider": "gateway",\n  "inferenceGatewayBaseUrl": "http://127.0.0.1:${port}",\n  "inferenceGatewayApiKey": "***",\n  "inferenceGatewayAuthScheme": "bearer",\n  "disableDeploymentModeChooser": true,\n  "coworkEgressAllowedHosts": ["*"],\n  "inferenceModels": [{ "name": "claude-opus-4-8", "supports1m": true }]\n}\n`,
          },
          {
            path: "%LOCALAPPDATA%/Claude-3p/configLibrary/_meta.json",
            exists: true,
            format: "json",
            content: `{\n  "appliedId": "00000000-0000-4000-8000-000053796e61",\n  "entries": [{ "id": "00000000-0000-4000-8000-000053796e61", "name": "SynaRoute" }]\n}\n`,
          },
        ],
        mcpRegistered: false,
      };
    }
    // claude-cli
    return {
      categoryId,
      summary:
        "Claude CLI：settings.json。写入 BASE_URL / AUTH_TOKEN(占位) / 发现开关 / ANTHROPIC_MODEL / 顶层 model；不写三档 DEFAULT_*。",
      files: [
        {
          path: "~/.claude/settings.json",
          exists: true,
          format: "json",
          content: `{\n  "env": {\n    "ANTHROPIC_BASE_URL": "http://127.0.0.1:${port}",\n    "ANTHROPIC_AUTH_TOKEN": "***",\n    "ANTHROPIC_MODEL": "claude-opus-4-7",\n    "CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY": "1"\n  },\n  "model": "claude-opus-4-7"\n}\n`,
        },
      ],
      mcpRegistered: false,
    };
  },
  async listEvents(categoryId: CategoryType) {
    await delay();
    return clone(events.filter((e) => e.categoryId === categoryId));
  },
  async listAllEvents() {
    await delay();
    // 与真实后端同口径：列表剥掉 trace 正文，只留 hasTrace 布尔位。
    return clone(events).map((e) => ({ ...e, hasTrace: !!e.trace, trace: undefined }));
  },
  async getEventTrace(eventId: string) {
    await delay();
    return clone(events.find((e) => e.id === eventId)?.trace ?? null);
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

  // ---- 主口令增强模式（浏览器预览态）----
  // 只模拟状态机与口令校验，不做真加密。目的是让设置页的三个对话框
  // （启用 / 解锁 / 关闭）在 npm run dev 下都能走完整流程被看到。
  async getMasterPasswordState(): Promise<MasterPasswordState> {
    await delay();
    return { ...masterMock };
  },
  async unlockMasterPassword(password: string) {
    await delay();
    if (!masterMock.enabled) throw new Error("当前不是主口令模式，无需解锁");
    if (password !== masterPw) throw new Error("主口令错误。请确认后重试（口令区分大小写）。");
    masterMock.locked = false;
  },
  async lockMasterPassword() {
    await delay();
    if (masterMock.enabled) masterMock.locked = true;
  },
  async enableMasterPassword(password: string) {
    await delay();
    if (masterMock.enabled) throw new Error("已处于主口令模式，无需重复启用");
    if (!password) throw new Error("主口令不能为空");
    masterPw = password;
    masterMock = { enabled: true, locked: false };
    // 迁移条数 = 当前三个分类的 Key 总数（贴近真实反馈）
    return (Object.keys(store) as CategoryType[]).reduce((n, c) => n + store[c].length, 0);
  },
  async disableMasterPassword(password: string) {
    await delay();
    if (!masterMock.enabled) throw new Error("当前不是主口令模式，无需关闭");
    if (password !== masterPw) throw new Error("主口令错误。请确认后重试（口令区分大小写）。");
    masterMock = { enabled: false, locked: false };
    masterPw = "";
    return (Object.keys(store) as CategoryType[]).reduce((n, c) => n + store[c].length, 0);
  },
  async changeMasterPassword(oldPassword: string, newPassword: string) {
    await delay();
    if (!masterMock.enabled) throw new Error("当前不是主口令模式，无法修改主口令");
    if (oldPassword !== masterPw) throw new Error("主口令错误。请确认后重试（口令区分大小写）。");
    if (!newPassword) throw new Error("新主口令不能为空");
    masterPw = newPassword;
    masterMock.locked = false;
    return (Object.keys(store) as CategoryType[]).reduce((n, c) => n + store[c].length, 0);
  },
};
