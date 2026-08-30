// 浏览器环境下的 mock 数据层
// 目的：让前端脱离 Rust 后端也能独立展示与交互（npm run dev 直接看页面）。
// 数据取自需求文档 UC-002 示例场景（厂商1/2/3 + GLM 映射），使界面一打开即有真实感。
// Tauri 环境不会走到这里（见 bridge.ts 的 isTauri 判定）。
// 所有结构严格对齐 src/types.ts 的 IPC 契约。

import type {
  AppSettings,
  BrainConfig,
  CategoryType,
  DailyUsageBucket,
  EventLogEntry,
  HealthStatus,
  MasterPasswordState,
  ModelInfo,
  ProviderKey,
  ProxyState,
  TokenUsageByKey,
  UsageCostRow,
  Vendor,
} from "@/types";
import type { UserPrefs } from "@/lib/prefs";
import { mockUsageWithCost } from "./mockData.usage";
import { MOCK_VENDORS } from "./mockData.vendors";
import { mockEvents } from "./mockData.events";

const now = Date.now();

/** 主口令模式的浏览器预览态：默认关（与真实默认一致），启用后记住口令用于校验。 */
let masterMock: MasterPasswordState = { enabled: false, locked: false };
let masterPw = "";

/**
 * 孤儿密钥条数的浏览器预览态。**刻意给非零初值**：这一行 UI 只在 `> 0` 时出现，
 * 若 mock 恒返回 0，浏览器模式下这块界面永远不可达 —— 那 mock 就失去了意义
 * （改样式、验文案都得先装到真机上）。清理后归零，让「清理→提示→行消失」整条链可走通。
 */
let orphanMock = 2;

/** 便捷构造模型列表 */
const models = (names: string[], ctxWindow?: number): ModelInfo[] =>
  names.map((n) => ({ realName: n, source: "fetched" as const, fetchedAt: now - 3600_000, contextWindow: ctxWindow }));

/**
 * 开着余额查询的默认配置（浏览器预览用）。
 *
 * `enabled: true` 是刻意的：Key 卡片上的余额行只在开关打开时才渲染，
 * 若 mock 里全是关闭态，那一行在浏览器模式下**永远不可达** —— 改样式、验文案
 * 都得先打包装到真机上。与 `orphanMock` 给非零初值同一个道理。
 */
const mockBalanceQuery = () => ({
  enabled: true,
  template: "generic",
  url: "{{baseUrl}}/user/balance",
  method: "GET",
  auth: "bearer",
  timeoutSecs: 10,
  autoIntervalMin: 0,
});

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
      params: { temperature: 1.0 },
      models: models(["opus-4-6", "opus-4-7", "opus-4-8"], 200000),
      mappings: [],
      health: { status: "up", lastChecked: now - 20_000, latencyMs: 320, failCount: 0 },
      // k1~k4 各开余额查询，配合 `queryBalance` 的 4 个分桶，在预览里一次铺开
      // 全部展示状态：有额度/有总额 → 查询失败 → 超时 → 上游报账号不可用。
      // 卡片上这四种长得完全不同（尤其「失败」绝不能显示成余额 0），并排才看得出差异。
      balanceQuery: mockBalanceQuery(),
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
      params: { temperature: 1.0 },
      models: models(["opus-4-6", "opus-4-7", "opus-4-8", "fable-5"], 200000),
      mappings: [],
      health: { status: "up", lastChecked: now - 22_000, latencyMs: 540, failCount: 0 },
      balanceQuery: mockBalanceQuery(),
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
      params: { temperature: 1.0 },
      models: models(["GLM5.1", "GLM5.2"]),
      mappings: [
        { id: "m1", expectedName: "opus-4-7", realName: "GLM5.1" },
        { id: "m2", expectedName: "opus-4-8", realName: "GLM5.2" },
      ],
      health: { status: "unknown", failCount: 0 },
      balanceQuery: mockBalanceQuery(),
    },
    // 以下两条专供验证「陈旧失败结论」与「新鲜失败结论」的徽标差异（2026-08-02 加）：
    // 真机上见过一条**禁用** Key 卡片显示「探测不可达 · 10 天前」，而那家上游早已恢复、
    // 真实转发也成功 —— 定时探测只扫启用的 Key，禁用期间状态永久冻结在很久以前那次失败上。
    // 两条并排放，才能在预览里一眼看出「已过期」与「不可达」是两种不同措辞。
    {
      id: "k4",
      categoryId: "claude-cli",
      name: "厂商4（禁用·状态是 10 天前的）",
      vendor: "custom",
      baseUrl: "https://stale.example.com",
      protocol: "anthropic",
      hasSecret: true,
      enabled: false, // 禁用 → 不参与定时探测 → 状态永久冻结
      priority: 3,
      params: { temperature: 1.0 },
      models: models(["claude-opus-4-8"]),
      mappings: [],
      health: {
        status: "down",
        lastChecked: now - 10 * 24 * 60 * 60 * 1000, // 10 天前
        latencyMs: 342,
        failCount: 0,
      },
      balanceQuery: mockBalanceQuery(),
    },
    {
      id: "k5",
      categoryId: "claude-cli",
      name: "厂商5（刚探测过·确实不可达）",
      vendor: "custom",
      baseUrl: "https://fresh-down.example.com",
      protocol: "anthropic",
      hasSecret: true,
      enabled: true,
      priority: 4,
      params: { temperature: 1.0 },
      models: models(["claude-opus-4-8"]),
      mappings: [],
      health: { status: "down", lastChecked: now - 30_000, latencyMs: 1251, failCount: 1 },
    },
    {
      id: "k6",
      categoryId: "claude-cli",
      name: "厂商6（熔断中）",
      vendor: "custom",
      baseUrl: "https://tripped.example.com",
      protocol: "anthropic",
      hasSecret: true,
      enabled: true,
      priority: 5,
      params: { temperature: 1.0 },
      models: models(["claude-opus-4-8"]),
      mappings: [],
      // 熔断中：breakerUntil 在 40 秒后到期 → 常驻警告条应显示这条 Key
      health: { status: "up", lastChecked: now - 5_000, latencyMs: 90, failCount: 3, breakerUntil: now + 40_000 },
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
      params: { temperature: 1.0 },
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

const events: EventLogEntry[] = mockEvents(now);

let settings: AppSettings = {
  theme: "system",
  language: "zh",
  autoStart: false,
  masterPasswordEnabled: false,
  lanExposure: false,
  requestLogEnabled: false,
  aggregateTraceEnabled: false,
  healthCheckIntervalSecs: 60,
  failoverTotalBudgetMs: 90000,
  mcpEnabled: false,
  mcpPort: 9527,
};

// 内置厂商种子。清单本身在 mockData.vendors.ts（与 Rust 那份有跨语言判据盯着，
// 见该文件顶部说明）。这里只保留一个可变副本，供 upsert/delete 改动。
//
// ⚠️ 刻意用 `structuredClone` 而不是下面那个 `clone` 助手：`clone` 是 `const` 箭头函数、
// 在本行之后才定义，`const` 没有提升，模块初始化时调它会直接 TDZ 报错
// （表现是整个 mock 层 import 失败 → 演示模式整页白屏）。
let vendors: Vendor[] = structuredClone(MOCK_VENDORS);

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
    // 与后端一致：id 为空时在此生成并**随返回值回传**（P3-5）。
    // 原先只在 push 时补 id 却 `return clone(key)`（仍是空 id），mock 模式下调用方拿不到
    // 真实 id，后续 saveSecret/checkHealth 会打到一条不存在的 Key 上。
    const withId: ProviderKey =
      key.id.trim() === ""
        ? { ...clone(key), id: crypto.randomUUID() }
        : clone(key);
    const idx = list.findIndex((k) => k.id === withId.id);
    if (idx >= 0) list[idx] = clone(withId);
    else list.push(clone(withId));
    return clone(withId);
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

  // 孤儿密钥（UX#19）。非零初值与「清理后归零」的用意见 orphanMock 声明处。
  async countOrphanSecrets() {
    await delay();
    return orphanMock;
  },
  async pruneOrphanSecrets() {
    await delay();
    const n = orphanMock;
    orphanMock = 0;
    return n;
  },
  async toggleKey(keyId: string, enabled: boolean) {
    await delay();
    for (const cat of Object.keys(store) as CategoryType[]) {
      const k = store[cat].find((x) => x.id === keyId);
      if (k) k.enabled = enabled;
    }
  },
  // 与后端 store::key_flags 同口径：只翻这一位，别的字段一个都不碰。
  async setKeyAllowInAggregate(keyId: string, allow: boolean) {
    await delay();
    for (const cat of Object.keys(store) as CategoryType[]) {
      const k = store[cat].find((x) => x.id === keyId);
      if (k) k.allowInAggregate = allow;
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

  // 与后端 Store::move_key 同规则：与相邻项交换后整列重编号为连续 0,1,2…
  // 已在两端则返回 false（不改动）。
  async moveKey(categoryId: CategoryType, keyId: string, direction: "up" | "down") {
    await delay();
    const list = store[categoryId] ?? [];
    const ordered = [...list].sort((a, b) => a.priority - b.priority);
    const idx = ordered.findIndex((k) => k.id === keyId);
    if (idx < 0) return false;
    const swapWith = direction === "up" ? idx - 1 : idx + 1;
    if (swapWith < 0 || swapWith >= ordered.length) return false;
    [ordered[idx], ordered[swapWith]] = [ordered[swapWith], ordered[idx]];
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
  async tokenUsage() {
    await delay();
    // 与真实后端同口径：按分类×Key 聚合。mock 里各 Key 的 usage 取自 events 里的用量；
    // 无用量事件不产生分组。
    const map = new Map<string, TokenUsageByKey>();
    for (const e of events) {
      if (!e.usage) continue;
      const key = `${e.categoryId}/${e.keyId ?? ""}`;
      const cur = map.get(key) ?? {
        categoryId: e.categoryId,
        keyId: e.keyId ?? "",
        usage: { input: 0, output: 0, cacheRead: 0 },
      };
      cur.usage.input += e.usage.input;
      cur.usage.output += e.usage.output;
      cur.usage.cacheRead = (cur.usage.cacheRead ?? 0) + (e.usage.cacheRead ?? 0);
      map.set(key, cur);
    }
    return clone([...map.values()]);
  },
  async usageSince() {
    await delay();
    // mock 没有真实的落盘文件，用「7 天前」代表一份已经攒了一阵的累计，
    // 好让浏览器预览也能看出这行不是「本次启动时间」。
    return Date.now() - 7 * 24 * 60 * 60 * 1000;
  },

  /**
   * 按日分桶用量的 mock：造近 7 天、逐日递增的曲线。
   *
   * 刻意让数字有起伏而不是一条直线：趋势图的排序、峰值高亮、空日留白
   * 这些在等值数据上都看不出错。第 3 天故意留空，用来验证「那天没用量」
   * 的显示（真实数据里周末常常就是空的）。
   */
  async dailyUsage() {
    await delay();
    const buckets: DailyUsageBucket[] = [];
    const perDay = [1_200_000, 340_000, 0, 890_000, 1_750_000, 620_000, 2_100_000];
    for (let i = 0; i < perDay.length; i++) {
      const d = new Date(Date.now() - i * 86_400_000);
      const date = d.toISOString().slice(0, 10);
      if (perDay[i] === 0) {
        buckets.push({ date, entries: [] });
        continue;
      }
      buckets.push({
        date,
        entries: [
          {
            categoryId: "claude-cli",
            keyId: store["claude-cli"][0]?.id ?? "k1",
            usage: {
              input: perDay[i],
              output: Math.round(perDay[i] * 0.28),
              cacheRead: Math.round(perDay[i] * 1.4),
              cacheCreation: Math.round(perDay[i] * 0.11),
            },
          },
        ],
      });
    }
    // 与后端同口径：最新在前
    return buckets;
  },

  /**
   * 用量 + 成本估算行。实现搬到了 `mockData.usage.ts`（本文件在棘轮上余量为 0），
   * 那边同时铺开了「算不出金额」的**四种成因**——它们各有不同的界面文案与指路，
   * 不在预览里各出现一次就必然做漏（这正是本文件存在的理由）。
   */
  async usageWithCost(): Promise<UsageCostRow[]> {
    await delay();
    return mockUsageWithCost(store, await this.tokenUsage());
  },

  /**
   * 余额查询的 mock。
   *
   * 按 keyId 的哈希分出三种形态：**成功、未配置、查询失败**。
   * 三种都要能在浏览器预览里看到 —— 失败态的文案与布局最容易做漏，
   * 而它恰恰是真实使用中最常出现的（站点没这接口、路径填错）。
   */
  /**
   * 按 keyId 稳定分桶，铺开卡片余额行的**全部四种**展示状态。
   *
   * 分桶（而不是随机）是为了让同一条 Key 每次刷新都落在同一状态上，
   * 改样式时不会因为随机跳变而误判「刚才那个样式没生效」。
   * k1~k4 恰好落在 0~3 四个桶里（已实测），四张卡片并排即可一次看全。
   */
  async queryBalance(keyId: string) {
    await delay();
    const bucket = keyId.split("").reduce((a, c) => a + c.charCodeAt(0), 0) % 4;
    // 桶 1：查询失败（字段找不到）。卡片必须显示这句原因，**且绝不显示成余额 0**。
    if (bucket === 1) {
      return {
        ok: false,
        queriedAt: Date.now(),
        error: "上游返回里找不到余额字段（可在「取值路径」手填，如 data.balance）",
      };
    }
    // 桶 2：超时
    if (bucket === 2) {
      return { ok: false, queriedAt: Date.now(), error: "查询超时（10s）" };
    }
    // 桶 3：**有余额、但上游说这个号不可用**。这两件事要分开显示（warning 而非 danger）：
    // 「查询坏了」去改配置，「号不能用了」去充钱或换号，处理方式完全不同。
    if (bucket === 3) {
      return {
        ok: true,
        remaining: 12.5,
        unit: "USD",
        isValid: false,
        invalidMessage: "该密钥已被上游限流（触发日额度上限）",
        queriedAt: Date.now() - 3 * 60_000,
      };
    }
    // 桶 0：正常，且带总额 + 套餐名 → 卡片上会多出「共 N」与「已用 X%」
    return {
      ok: true,
      remaining: 84.2,
      total: 200,
      unit: "CNY",
      planName: "月付套餐",
      isValid: true,
      queriedAt: Date.now(),
    };
  },
  async getEventTrace(eventId: string) {
    await delay();
    return clone(events.find((e) => e.id === eventId)?.trace ?? null);
  },
  async getSettings() {
    await delay();
    return clone(settings);
  },
  /**
   * 收 UserPrefs 并**浅合并**，不是整份替换。
   *
   * 整份替换会把 runtime 字段（autoStart / mcpPort / activeModels…）抹成 undefined，
   * 与真机行为背离 —— 而本项目已经吃过「预览验证通过、真机不对」的亏（见验收纪律）。
   */
  async saveSettings(next: UserPrefs) {
    await delay();
    Object.assign(settings, next);
  },
  async setAutoStart(enabled: boolean) {
    await delay();
    settings.autoStart = enabled;
  },
  async setLanExposure(enabled: boolean): Promise<number> {
    await delay();
    settings.lanExposure = enabled;
    // 真实后端返回「因此重启了几个代理」；预览里没有真代理，恒 0。
    return 0;
  },
  /** 预览用假令牌：形状与真实一致（64 位十六进制）但**固定值** —— 每次刷新都换会让演示走不通。 */
  async getLanToken(): Promise<string | null> {
    await delay();
    return settings.lanExposure ? "0".repeat(32) + "f".repeat(32) : null;
  },
  /** 预览里没有真的 `~/.codex/config.toml`，恒 null（= 状态条不显示那一格）。 */
  async getCodexConfigModel(): Promise<string | null> {
    await delay();
    return null;
  },
  async regenerateLanToken(): Promise<string> {
    await delay();
    settings.lanExposure = true;
    return crypto.randomUUID().replace(/-/g, "") + crypto.randomUUID().replace(/-/g, "");
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
