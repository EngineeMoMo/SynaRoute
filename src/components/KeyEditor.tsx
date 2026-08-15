import { useEffect, useRef, useState } from "react";
import { api } from "@/lib/bridge";
import { useStore } from "@/store";
import { useT } from "@/lib/useT";
import { Button } from "@/components/ui/Button";
import { Combobox } from "@/components/ui/Combobox";
import { BrandIcon } from "@/components/BrandIcon";
import type {
  BalanceQuery,
  BalanceResult,
  DesktopModelNameIssue,
  ModelInfo,
  ModelMapping,
  ProviderKey,
  Protocol,
} from "@/types";
import {
  type TokenUnit,
  tokensFromAmount,
  preferredUnit,
  amountForUnit,
} from "@/lib/tokenUnit";
import { balanceFingerprint, formatBalanceAmount } from "@/lib/balance";
import { X, RefreshCw, Plus, Trash2, ArrowRight, Eye, EyeOff, Zap, Gauge, Brain, Download, AlertTriangle, Wallet, ChevronRight } from "lucide-react";

interface KeyEditorProps {
  initial: ProviderKey | null; // null = 新增
  onClose: () => void;
  /** 保存成功时回调（带后端回填 uuid 的那条 Key）。与 onClose 分开，因为 onClose 在取消时也会触发 */
  onSaved?: (saved: ProviderKey) => void;
}

/**
 * 协议下拉里的显示名。刻意提到模块作用域：它是常量，放组件里每次渲染都要重建一个对象。
 * 与下拉的三个 `<option>` 文案保持一致 —— 提示条说「看起来是 X 协议」，用户要能在
 * 下拉里找到同名的那一项。
 */
const PROTOCOL_LABEL: Record<Protocol, string> = {
  anthropic: "Anthropic",
  openai_chat: "OpenAI Chat",
  openai_responses: "OpenAI Responses",
};

/**
 * 余额查询的预设模板（对齐 cc-switch 界面上那排按钮）。
 *
 * 只预设 `url` / `method` / `auth` 三项 —— 取值路径刻意留空，走后端的候选字段链
 * 自动探测（`remaining` / `quota.remaining` / `balance` / `data.balance` …）。
 * 这样一个模板能覆盖同一类面板的多种字段命名，而不必为每家各写一条。
 *
 * `custom` 不在此表里：它表示「用户自己改过」，切到它时保留当前填的值不动。
 */
const BALANCE_TEMPLATES: Record<string, { url: string; method: string; auth: string }> = {
  // cc-switch 内置「通用模板」的**原文路径**（其用户手册 §2.5：`{{baseUrl}}/user/balance`）。
  // 此前这里写的是 `/v1/usage` —— 那是某个站的自定义脚本路径，被我误当成通用默认值，
  // 结果新用户一开开关就 404。
  generic: { url: "{{baseUrl}}/user/balance", method: "GET", auth: "bearer" },
  // NewAPI 系面板：认的是**面板登录态**（access token + 用户 id），不是转发用的 API Key
  newapi: { url: "{{baseUrl}}/api/user/self", method: "GET", auth: "access-token" },
  // DeepSeek：余额端点在**域名根**下，而它的 baseUrl 常带 `/anthropic` 后缀，
  // 故必须用 `{{origin}}`（剥掉路径）而非 `{{baseUrl}}` —— 实测后者 404、前者 200。
  deepseek: { url: "{{origin}}/user/balance", method: "GET", auth: "bearer" },
  // 官方 Anthropic
  official: { url: "{{baseUrl}}/v1/organizations/me", method: "GET", auth: "x-api-key" },
};

/** 模板按钮的顺序与文案 key（与 cc-switch 的排列一致：自定义在最前）。 */
const BALANCE_TEMPLATE_ORDER = ["custom", "generic", "newapi", "deepseek", "official"] as const;

/** 新建 Key 时的余额查询初值：默认**关闭**，但把通用模板填好，用户开开关即可用。 */
function defaultBalanceQuery(): BalanceQuery {
  return {
    enabled: false,
    template: "generic",
    ...BALANCE_TEMPLATES.generic,
    timeoutSecs: 10,
    autoIntervalMin: 0,
  };
}

/** 新增/编辑 Key 抽屉面板（FR-002/004/005/006） */
export function KeyEditor({ initial, onClose, onSaved }: KeyEditorProps) {
  const activeCategory = useStore((s) => s.activeCategory);
  const loadCategory = useStore((s) => s.loadCategory);
  const vendors = useStore((s) => s.vendors);
  const setBalanceResult = useStore((s) => s.setBalanceResult);
  const t = useT();
  const isNew = !initial;

  const [name, setName] = useState(initial?.name ?? "");
  const [vendor, setVendor] = useState(initial?.vendor ?? "custom");
  const [baseUrl, setBaseUrl] = useState(initial?.baseUrl ?? "");
  const [protocol, setProtocol] = useState<Protocol>(initial?.protocol ?? "anthropic");
  const [secret, setSecret] = useState("");
  const [showSecret, setShowSecret] = useState(false);
  const [revealing, setRevealing] = useState(false);
  const [temperature, setTemperature] = useState(initial?.params.temperature ?? 1.0);
  const [maxTokens, setMaxTokens] = useState(initial?.params.maxTokens ?? 8192);
  // 请求超时(ms):空 = 未设(后端默认 30000)。服务非流式转发,慢厂商可调大;
  // 健康探测/拉模型后端固定封顶 30s,大脑聚合用大脑页「总超时」——均不受此值放大影响。
  const [timeoutMs, setTimeoutMs] = useState<number | "">(initial?.params.timeoutMs ?? "");
  const [models, setModels] = useState<ModelInfo[]>(initial?.models ?? []);
  const [mappings, setMappings] = useState<ModelMapping[]>(initial?.mappings ?? []);
  const [defaultModel, setDefaultModel] = useState(initial?.defaultModel ?? "");
  const [tierHaiku, setTierHaiku] = useState(initial?.tierHaiku ?? "");
  const [tierSonnet, setTierSonnet] = useState(initial?.tierSonnet ?? "");
  const [tierOpus, setTierOpus] = useState(initial?.tierOpus ?? "");
  // ---- 余额查询（第④批）----
  // 整份配置放一个 state：字段间有联动（换模板要一次改 url/method/auth 三项），
  // 拆成 8 个 useState 会让「换模板」变成 8 次 setState、且容易漏改其中一项。
  const [balance, setBalance] = useState<BalanceQuery>(
    () => initial?.balanceQuery ?? defaultBalanceQuery(),
  );
  const [balanceOpen, setBalanceOpen] = useState(!!initial?.balanceQuery?.enabled);
  // 「测试查询」的结果。null = 还没测过；测过就把成功值或失败原因如实显示出来。
  const [balanceProbe, setBalanceProbe] = useState<BalanceResult | null>(null);
  const [probing, setProbing] = useState(false);
  // 计费倍率（如 "0.3" = 官方价三折）。存字符串，避免 0.1+0.2 那类浮点显示。
  const [costMultiplier, setCostMultiplier] = useState(initial?.costMultiplier ?? "");
  const [fetching, setFetching] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // 批量应用 Max Tokens 的状态与结果提示。成功走独立提示，不复用 error（那是红色告警样式）。
  const [applyingAll, setApplyingAll] = useState(false);
  const [applyAllMsg, setApplyAllMsg] = useState<string | null>(null);

  /**
   * 把当前输入框里的 Max Tokens 一次应用到本分类**全部** Key。
   *
   * ⚠️ 该值**当前不参与任何请求**（2026-08-15 定调）：代理转发透明不补上限；大脑聚合的
   * 输出预算按协议与模型上下文窗口自动算（见 upstream/budget.rs）。字段与本批量入口
   * 仅为兼容旧配置保留 —— 老配置里存着值，删字段要做迁移，收益不抵风险。
   *
   * 文案已如实标注「不生效」。**不要**因为「看起来没用」就悄悄把它接回某条请求路径：
   * 那正是本次要消除的东西（4096 默认值把长回答截断在一半，用户无从归因）。
   *
   * 用的是**当前输入框的值**（可能尚未保存到本 Key）：用户改完数字直接点批量是最自然的流程，
   * 要求先保存再批量反而绕。本 Key 自身的值仍由「保存」按钮落盘。
   */
  const applyMaxTokensToAll = async () => {
    if (!Number.isFinite(maxTokens) || maxTokens <= 0) {
      setError(t("editor.errMaxTokensZero"));
      return;
    }
    setApplyingAll(true);
    setApplyAllMsg(null);
    setError(null);
    try {
      const changed = await api.applyMaxTokensToCategory(activeCategory, maxTokens);
      // 如实区分「改了 N 条」与「本就都是这个值」——后者报成功会让人以为刚生效。
      setApplyAllMsg(
        changed > 0
          ? t("editor.maxTokensApplied", { n: String(changed) })
          : t("editor.maxTokensNoChange"),
      );
      await loadCategory(activeCategory);
    } catch (e) {
      setError(t("editor.errApplyAll", { err: String(e) }));
    } finally {
      setApplyingAll(false);
    }
  };
  const [saving, setSaving] = useState(false);
  const [invalidContextModels, setInvalidContextModels] = useState<Set<string>>(() => new Set());
  // 手动加模型输入框（不用原生 prompt()：WebView2 里行为不可靠）
  const [manualModel, setManualModel] = useState("");

  const draftId = initial?.id ?? `k_new`;

  // 选中厂商：仅当 baseUrl 为空、或仍等于上一个厂商的默认值时才自动预填，
  // 避免覆盖用户已手改的地址。协议同理。
  const handleVendorChange = (nextId: string) => {
    const prev = vendors.find((v) => v.id === vendor);
    const next = vendors.find((v) => v.id === nextId);
    setVendor(nextId);
    if (!next) return;
    const baseUrlUntouched = baseUrl.trim() === "" || baseUrl.trim() === (prev?.defaultBaseUrl ?? "");
    if (baseUrlUntouched && next.defaultBaseUrl) setBaseUrl(next.defaultBaseUrl);
    if (baseUrlUntouched) setProtocol(next.defaultProtocol);
  };

  /**
   * 从 baseUrl 推断上游协议（UX#3）。
   *
   * 为什么需要：协议选错是新手最容易踩、且**症状最难懂**的坑——填一个 OpenAI 兼容中转商
   * 却留着默认的 `anthropic`，结果每次转发都 400，而界面上一切看着都对。此前 vendor 下拉
   * 切换会带出默认协议，但**用户手贴 baseUrl 时没有任何推断**（自定义厂商是最常见情形）。
   *
   * **判据顺序刻意是「先路径、后域名」**：路径是协议本身的一部分（各家 SDK 硬编码），
   * 域名只是个名字。中转商域名里带 `claude` / `anthropic` 却提供 OpenAI 兼容接口
   * 极常见（`https://claude-relay.example.com/v1/chat/completions`），若先看域名就会
   * 把它判成 Anthropic —— 正好造出这个功能本要消灭的那种故障。故域名关键词只在
   * 路径给不出任何信号时才作为兜底。
   *
   * 返回 `null` = 认不出，此时**不动**用户当前的选择（宁可不猜，也不要猜错后悄悄改掉）。
   */
  const inferProtocol = (url: string): Protocol | null => {
    const u = url.trim().toLowerCase();
    if (!u) return null;
    // 第一优先：端点路径（协议的标志性特征，各家 SDK 里写死的那几条）
    if (u.includes("/messages")) return "anthropic";
    if (u.includes("/responses")) return "openai_responses";
    if (u.includes("/chat/completions") || u.includes("/completions")) return "openai_chat";
    // 兜底：路径无特征（多数中转商只给到 `https://x.com/v1`）时才看域名关键词
    if (u.includes("anthropic") || u.includes("claude")) return "anthropic";
    return null;
  };

  /**
   * baseUrl 失焦时按推断改协议。三重限制，避免「悄悄改掉用户的配置」：
   *
   * 1. `protocolTouched`——用户亲自动过协议下拉就绝不覆盖；
   * 2. `initial && baseUrl === initial.baseUrl`——编辑既有 Key 时，只要地址没被改过就不动。
   *    否则用户只是 Tab 键路过输入框，一个**正在正常工作**的 Key 的协议就被改了，
   *    而他毫无察觉——这正是本项目反复防的静默失效；
   * 3. 推断不出（`null`）时不动。
   *
   * 三条都不满足时仍会出提示条（见 `protocolHint`），只是不自动改。
   */
  const [protocolTouched, setProtocolTouched] = useState(false);
  const handleBaseUrlBlur = () => {
    if (protocolTouched) return;
    if (initial && baseUrl.trim() === (initial.baseUrl ?? "").trim()) return;
    const guess = inferProtocol(baseUrl);
    if (guess && guess !== protocol) setProtocol(guess);
  };
  // 推断结果与当前选择不一致时给一条**提示而非强改**（用户动过协议、或编辑既有 Key 的情况）。
  const protocolHint = (() => {
    const guess = inferProtocol(baseUrl);
    return guess && guess !== protocol ? guess : null;
  })();

  // 老 Key 的 vendor 值可能不在当前厂商列表里（自由文本历史值），补一个兜底 option 不丢显示
  const vendorMissing = vendor !== "" && !vendors.some((v) => v.id === vendor);

  const handleFetchModels = async () => {
    if (!baseUrl.trim()) {
      setError(t("editor.errNeedBaseUrl"));
      return;
    }
    // 新增 Key 未保存时无密钥、无真实 id：用编辑中的草稿直接探测
    if (isNew && !secret) {
      setError(t("editor.errNeedSecret"));
      return;
    }
    setFetching(true);
    setError(null);
    try {
      // 用当前编辑中的字段拼草稿 key，后端用其 baseUrl+protocol+secret 探测
      const draft: ProviderKey = {
        id: draftId,
        // 同 buildDraftKey：探测用的草稿也要跟随 Key 自己的分类，不跟随当前页。
        categoryId: initial?.categoryId ?? activeCategory,
        name: name.trim() || draftId,
        vendor,
        baseUrl: baseUrl.trim(),
        protocol,
        hasSecret: initial?.hasSecret || secret.length > 0,
        enabled: initial?.enabled ?? false,
        priority: initial?.priority ?? 999,
        params: { ...initial?.params, temperature, maxTokens, timeoutMs: typeof timeoutMs === "number" && timeoutMs >= 1000 ? timeoutMs : undefined },
        models,
        mappings,
        defaultModel: defaultModel.trim() || undefined,
        health: initial?.health ?? { status: "unknown", failCount: 0 },
      };
      const fetched = await api.fetchModelsDraft(draft, secret || undefined);
      if (fetched.length === 0) {
        setError(t("editor.errNoModels"));
      } else {
        const oldCtx = new Map(models.map((m) => [m.realName, m.contextWindow]));
        setModels(fetched.map((m) => ({ ...m, contextWindow: m.contextWindow ?? oldCtx.get(m.realName) })));
      }
    } catch (e) {
      setError(t("editor.errFetch", { err: String(e) }));
    } finally {
      setFetching(false);
    }
  };

  // 点"眼睛"：首次显示时若输入框为空且已配置密钥，从后端解密取回明文续填
  const toggleShowSecret = async () => {
    if (!showSecret && !secret && initial?.hasSecret && initial?.id) {
      setRevealing(true);
      try {
        const plain = await api.revealSecret(initial.id);
        if (plain) setSecret(plain);
      } catch (e) {
        setError(t("editor.errRevealSecret", { err: String(e) }));
      } finally {
        setRevealing(false);
      }
    }
    setShowSecret((v) => !v);
  };

  const addManualModel = () => {
    const nm = manualModel.trim();
    if (!nm) return;
    if (models.some((m) => m.realName === nm)) {
      setManualModel("");
      return;
    }
    setModels([...models, { realName: nm, source: "manual" }]);
    setManualModel("");
  };

  const addMapping = () =>
    setMappings([...mappings, { id: `m_${Date.now()}`, expectedName: "", realName: "" }]);

  /**
   * 切余额查询模板：套用该模板的 url/method/auth，其余字段（超时、间隔、覆盖项）保留。
   *
   * 切到 `custom` 时**什么都不改**，只改 template 标记 —— 用户选「自定义」的意图是
   * 「我要自己填」，此时清空或重置他刚填的内容是最招人烦的行为。
   */
  const applyBalanceTemplate = (tpl: string) => {
    setBalanceProbe(null); // 换了目标地址，上次的探测结果不再代表现在
    if (tpl === "custom") {
      setBalance((b) => ({ ...b, template: "custom" }));
      return;
    }
    const preset = BALANCE_TEMPLATES[tpl];
    if (!preset) return;
    setBalance((b) => ({ ...b, template: tpl, ...preset }));
  };

  /**
   * 测试查询：先保存再查。
   *
   * **为什么必须先保存**：后端 `query_key_balance` 按 keyId 从密钥库取密钥、
   * 从配置读 balanceQuery —— 都是已落盘的数据。若不保存就查，用户在表单里刚改的
   * URL 根本不会生效，测出来的是旧配置的结果，「改完一测还是错」会让人以为改动无效。
   *
   * **新建的 Key 也能直接测**（2026-08-14 修）：`upsert_key` 会回填后端生成的 uuid
   * 并随返回值给回，拿到它就能接着查 —— 不必要求用户先点一次「保存」再回来点「测试查询」。
   * 旧实现在 `initial?.id` 为空时直接拒绝，而那正是「新建 Key 时想验证余额配置」
   * 这个最需要它的场景（真机反馈：用户以为功能坏了）。
   *
   * 唯一仍需拦的是「连密钥都还没填」：余额查询要拿密钥去打上游，没密钥必然失败，
   * 与其让上游回一个 401 让用户猜，不如就地说清楚。
   */
  const probeBalance = async () => {
    // 没有可用密钥（既没有已落盘的、也没在表单里填）→ 就地说明，别去打一次注定 401 的上游。
    if (!secret && !initial?.hasSecret) {
      setBalanceProbe({
        ok: false,
        queriedAt: Date.now(),
        error: t("balance.probeNeedSecret"),
      });
      return;
    }
    setProbing(true);
    setBalanceProbe(null);
    try {
      // 先把当前表单落盘，否则测的是旧配置（见上方注释）。
      // **用返回值里的 id**：新建时 draft.id 是空串，真正的 uuid 由后端生成并回填。
      const draft = buildDraftKey();
      const saved = await api.upsertKey(draft);
      const keyId = saved.id;
      if (secret) await api.saveSecret(keyId, secret);
      // force=true 跳过缓存，确保「测试查询」总是查询上游最新值
      const result = await api.queryKeyBalance(keyId, true);
      setBalanceProbe(result);
      // 同一结果直接写进卡片的余额缓存。
      //
      // 指纹用**刚落盘的那条**算（`saved` 而非 `draft`/`initial`）：那才是产出本结果的配置，
      // 也正是卡片重新渲染后会算出的指纹 —— 两者一致，卡片才会判成缓存有效而不再发一次
      // 一模一样的请求。用 draft 算会在新建时差一个 id、指纹对不上，白发两个请求。
      setBalanceResult(keyId, balanceFingerprint(saved), result);
      // 新建的 Key 已经落盘了，把编辑器的「初始态」对齐过去。否则用户接着点「保存」
      // 会因为 draft.id 仍是空串而**再插一条**，表现为「测了一次余额就多出一条 Key」。
      onSaved?.(saved);
    } catch (e) {
      setBalanceProbe({
        ok: false,
        queriedAt: Date.now(),
        error: e instanceof Error ? e.message : String(e),
      });
    } finally {
      setProbing(false);
    }
  };

  // 当前厂商的内置预设模型（用于「一键导入」补全空列表）
  const presetModels = vendors.find((v) => v.id === vendor)?.presetModels ?? [];
  const importPresetModels = () => {
    const existing = new Set(models.map((m) => m.realName));
    const added: ModelInfo[] = presetModels
      .filter((p) => !existing.has(p.realName))
      .map((p) => ({ realName: p.realName, source: "manual", contextWindow: p.contextWindow }));
    if (added.length) setModels([...models, ...added]);
  };

  /**
   * 把当前表单拼成一个 ProviderKey 草稿。
   *
   * 抽出来是为了让「保存」与「即时校验」看的是**同一个对象**：桌面端模型名校验的输入源是
   * `serviceable_models()`，而它的结果取决于映射是否完整（有任一条完整映射，`models` 列表
   * 就被整份忽略、对外集合只由映射决定）。两边若各拼各的，就会出现「界面提示有问题、
   * 保存却成功」或反过来的自相矛盾。尤其是 `mappings.filter(...)` 那一条 ——
   * 少了它，没填完的映射行会混进校验集合，用户会收到一条保存时根本不存在的警告。
   */
  const buildDraftKey = (): ProviderKey => ({
    // 新建时留空，**由后端生成 uuid v4**（P3-5）。原先是 `k_${Date.now()}`：
    // id 被导入逻辑当作全局唯一标识做「同 id 即同一条 Key」的覆盖判据，而
    // 「两台机器照同一份教程配置」是真实场景，落在同一毫秒即撞号 → 跨机导入会把一条
    // 完全无关的本机 Key 静默覆盖成对方的配置。后端 upsert_key 会回填 id 并随返回值给回。
    id: initial?.id ?? "",
    // 编辑已有 Key 时用**它自己的分类**，不是当前页的分类。
    //
    // 编辑器 UI 根本不提供「改分类」，所以这里取 initial 才是语义正确的。
    // 原先写 activeCategory 一直没出事，只因为编辑器过去只能从当前分类页打开 ——
    // 那是个**载荷性的巧合**：一旦出现「编辑器已开着、再切到别的分类」的路径
    // （命令面板就能造出来），用户一保存就会把这条 Key 静默搬到另一个分类去，
    // 旧分类少一条、新分类多一条，且优先级顺序全乱。
    categoryId: initial?.categoryId ?? activeCategory,
    name: name.trim(),
    vendor,
    baseUrl: baseUrl.trim(),
    protocol,
    hasSecret: initial?.hasSecret || secret.length > 0,
    enabled: initial?.enabled ?? false,
    priority: initial?.priority ?? 999,
    params: { ...initial?.params, temperature, maxTokens, timeoutMs: typeof timeoutMs === "number" && timeoutMs >= 1000 ? timeoutMs : undefined },
    models,
    mappings: mappings.filter((m) => m.expectedName && m.realName),
    defaultModel: defaultModel.trim() || undefined,
    // 三档仅 Claude CLI/桌面端有意义；Codex 一律不落三档，避免 claude-*opus* 类名字被误改写路由
    // （即使该 Key 早前存过三档，此处也强制清空）。
    tierHaiku: activeCategory === "codex" ? undefined : tierHaiku.trim() || undefined,
    tierSonnet: activeCategory === "codex" ? undefined : tierSonnet.trim() || undefined,
    tierOpus: activeCategory === "codex" ? undefined : tierOpus.trim() || undefined,
    health: initial?.health ?? { status: "unknown", failCount: 0 },
    // 余额查询：从未配置过且仍是关闭态时不落这个字段，避免给每条 Key 的
    // config.json 都塞一段没用的默认配置（老 Key 保持原样、导出文件也更干净）。
    balanceQuery:
      balance.enabled || initial?.balanceQuery
        ? {
            ...balance,
            // 空白的覆盖项一律落 undefined 而非空串：后端把「空串」与「未设置」
            // 都当未设置处理，但空串会被序列化进文件，成为无意义的噪音。
            baseUrlOverride: balance.baseUrlOverride?.trim() || undefined,
            apiKeyRef: balance.apiKeyRef?.trim() || undefined,
            remainingPath: balance.remainingPath?.trim() || undefined,
          }
        : undefined,
    costMultiplier: costMultiplier.trim() || undefined,
  });

  /**
   * 桌面端对外模型名的**即时**体检（UX#4）。
   *
   * 为什么值得单开一条 IPC：对外名不合规会被 Claude 桌面端**静默过滤掉**，
   * 全被过滤则模型选择器为空、打开会话报 ModelsNotDiscoveredError ——
   * 这是本项目记录过的最难排查的症状之一。此前只在「保存」那一刻拦，
   * 用户可能已经填完整个表单（13 个字段）才被拒。
   *
   * **判据不在前端复刻**：那是 50+ 条厂商名子串加一套词边界匹配（逆向自桌面端 app.asar）。
   * 两份规则必然漂移，而漂移的两个方向都很糟 —— 轻则「界面说没问题、保存被拒」，
   * 重则「界面放行、桌面端静默过滤」。故调后端那份唯一事实。
   *
   * 三条约束：
   * 1. **250ms 防抖**：这个 effect 跟着打字走，不防抖会每键一发 IPC。
   * 2. **cancelled 标志**：慢的旧响应回来会盖掉新结果，表现为「明明改好了黄条还在」。
   * 3. **失败只清空、绝不阻断保存**：即时校验只是把反馈提前，真正的防线仍是后端保存拦截
   *    （无条件、且能覆盖历史 Key）。同理不因有问题就 disable 保存按钮 ——
   *    那样用户反而看不到后端那段带后果与修法的完整说明。
   */
  const [desktopIssues, setDesktopIssues] = useState<DesktopModelNameIssue[]>([]);
  useEffect(() => {
    if (activeCategory !== "claude-desktop") {
      setDesktopIssues([]);
      return;
    }
    let cancelled = false;
    const timer = setTimeout(() => {
      void api
        .checkDesktopModelNames(buildDraftKey())
        .then((r) => {
          if (!cancelled) setDesktopIssues(r.applicable ? r.issues : []);
        })
        .catch(() => {
          if (!cancelled) setDesktopIssues([]);
        });
    }, 250);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
    // deps 精确取 serviceable_models() 真正会读的那几项，别用整个 draft（每次渲染都是新对象）
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeCategory, models, mappings, tierHaiku, tierSonnet, tierOpus]);

  /** 是否已有「填完整」的映射行——决定 serviceable_models 走映射还是走 models 列表。 */
  const hasEffectiveMapping = mappings.some((m) => m.expectedName.trim() && m.realName.trim());

  /** 把某一行映射的对外名改成建议值。 */
  const applySuggestion = (rowId: string, suggestion: string) =>
    setMappings(mappings.map((m) => (m.id === rowId ? { ...m, expectedName: suggestion } : m)));

  /**
   * 一键修法：给 `models` 里的**每一个**模型都建一条映射。
   *
   * **必须是每一个，不能只给不合规的建** —— `serviceable_models()` 的语义是「只要存在任意一条
   * 完整映射，models 列表就被整份忽略」。若只给 glm-4.6 建映射，同列表里本来合规的
   * claude-opus-4-8 会直接从桌面端选择器里**消失**，用户「修好一个问题、丢了一个模型」，
   * 而且没有任何提示。合规的那些建 realName → realName 的恒等映射即可。
   * （model.rs 的 applying_report_suggestions_makes_key_saveable 用故障注入钉住了这条。）
   *
   * id 用 `m_${Date.now()}_${i}`：批量生成会落在同一毫秒，只用 Date.now() 会撞号 ——
   * React key 重复，且按 id 删除时会一次删掉多条。
   */
  const fixAllByAddingMappings = () => {
    const now = Date.now();
    setMappings(
      models.map((m, i) => ({
        id: `m_${now}_${i}`,
        expectedName:
          desktopIssues.find((x) => x.name === m.realName)?.suggestion ?? m.realName,
        realName: m.realName,
      })),
    );
  };

  const save = async () => {
    if (!name.trim()) return setError(t("editor.errNeedName"));
    if (!baseUrl.trim()) return setError(t("editor.errNeedBaseUrl2"));
    const currentModelNames = new Set(models.map((m) => m.realName));
    if ([...invalidContextModels].some((model) => currentModelNames.has(model))) {
      return setError(t("editor.errInvalidContextWindow"));
    }

    const key = buildDraftKey();

    setSaving(true);
    setError(null);
    try {
      // **必须用后端返回的 key**：新建时本地 id 是空串，真正的 uuid 由后端生成并回填。
      // 用本地那个空 id 去 saveSecret/checkHealth 会写到一条不存在的 Key 上（密钥成孤儿、
      // 探测无对象），而界面看起来一切正常——正是本项目最防的静默失效形态。
      const saved = await api.upsertKey(key);
      if (secret) await api.saveSecret(saved.id, secret);
      await loadCategory(activeCategory);
      // 保存成功的专用回调。**不能用 onClose 代替**：onClose 在「保存成功」与「点取消」
      // 两条路径上都会被调用，调用方无法区分。首启向导需要拿到后端回填了 uuid 的那条 Key
      // 才能接着自动启用它。App.tsx 现有用法不传该 prop，行为不变。
      onSaved?.(saved);
      onClose(); // 落盘即关闭：健康探测移出关键路径，避免等上游往返「像卡住」
      // Codex 分类的 Key 改动可能变更可服务模型集 → 托盘「Codex 模型」子菜单候选需同步重建
      // （托盘菜单静态构建，不自动跟数据变）。仅 Codex 需要，其余分类无此子菜单。
      if (activeCategory === "codex") {
        void api.rebuildTrayMenu().catch((e) => console.error("rebuildTrayMenu failed", e));
      }
      // 保存成功后后台跑一次可用性检查并回写 health（不阻塞关窗；探测失败仅标记 health=down）。
      // loadCategory 来自 store，本组件已卸载后仍可安全调用（更新的是全局状态，非本组件 state）。
      void (async () => {
        try {
          await api.checkHealth(saved.id);
          await loadCategory(activeCategory);
        } catch {
          // 探测异常忽略：Key 已保存，后台定时健康检查仍会兜底刷新
        }
      })();
    } catch (e) {
      // 失败时保留抽屉并显示错误，避免"报错后窗口卡住、列表不刷新"。
      // 后端校验消息（如桌面端对外模型名不合规）本身已含后果与修法，是多行文本，
      // 故不再套 "保存失败：{err}" 前缀——那会把首行挤在前缀后面、破坏排版。
      const raw = e instanceof Error ? e.message : String(e);
      setError(raw.includes("\n") ? raw : t("editor.errSave", { err: raw }));
      await loadCategory(activeCategory); // 刷新以反映可能的部分写入
      setSaving(false);
    }
  };

  return (
    <div
      className="fixed inset-0 z-50 flex justify-end bg-black/30"
      onMouseDown={(e) => {
        // 仅当在遮罩上真正按下并松开（纯点击外部）才关闭，避免框内拖选文字松手落到遮罩误关
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div className="flex h-full w-[440px] flex-col bg-surface shadow-2xl">

        {/* 头 */}
        <div className="flex items-center justify-between border-b border-border px-5 py-4">
          <h2 className="text-base font-semibold text-text-primary">
            {isNew ? t("editor.titleNew") : t("editor.titleEdit")}
          </h2>
          <button onClick={onClose} className="rounded p-1 text-text-muted hover:bg-surface-hover">
            <X size={18} />
          </button>
        </div>

        {/* 表单体 */}
        <div className="flex-1 space-y-4 overflow-y-auto px-5 py-4">
          {isNew && (
            <div className="rounded-control bg-info/10 px-3 py-2 text-[11px] leading-relaxed text-text-secondary">
              {t("editor.newKeyHint")}
            </div>
          )}
          <Field label={t("editor.name")}>
            <input className={inputCls} value={name} onChange={(e) => setName(e.target.value)} placeholder={t("editor.namePlaceholder")} />
          </Field>

          <div className="flex gap-3">
            <Field label={t("editor.vendor")} className="flex-1">
              <div className="flex items-center gap-2">
                <BrandIcon hint={vendor} fallbackLabel={vendor} iconUrl={vendors.find((v) => v.id === vendor)?.icon} size={28} />
                <select className={inputCls} value={vendor} onChange={(e) => handleVendorChange(e.target.value)}>
                  {vendors.map((v) => (
                    <option key={v.id} value={v.id}>{v.name}</option>
                  ))}
                  {vendorMissing && <option value={vendor}>{vendor}</option>}
                </select>
              </div>
            </Field>
            <Field label={t("editor.protocol")} className="w-48">
              <select
                className={inputCls}
                value={protocol}
                onChange={(e) => {
                  // 记住用户亲自动过：此后 baseUrl 推断只提示、不再覆盖他的选择。
                  setProtocolTouched(true);
                  setProtocol(e.target.value as Protocol);
                }}
              >
                <option value="anthropic">Anthropic</option>
                <option value="openai_chat">OpenAI Chat</option>
                <option value="openai_responses">OpenAI Responses</option>
              </select>
            </Field>
          </div>

          {/* 推断结果与当前选择不一致 → 提示 + 一键采纳。不强改，因为用户可能确实知道
              自己在做什么（例如中转商把 Anthropic 协议挂在 /v1 这种非标准路径下）。 */}
          {protocolHint && (
            <div className="flex items-center gap-2 rounded-control bg-warning/10 px-2.5 py-1.5 text-[11px] text-warning">
              <AlertTriangle size={12} className="shrink-0" />
              <span className="flex-1">
                {t("editor.protocolMismatch", { guess: PROTOCOL_LABEL[protocolHint] })}
              </span>
              <button
                type="button"
                onClick={() => setProtocol(protocolHint)}
                className="shrink-0 rounded border border-warning/40 px-1.5 py-0.5 font-medium hover:bg-warning/20"
              >
                {t("editor.protocolAdopt")}
              </button>
            </div>
          )}

          <Field label={t("editor.baseUrl")}>
            {/* 失焦时按 URL 推断协议（UX#3）：协议选错是新手最易踩、症状最难懂的坑
                （填 OpenAI 兼容中转商却留着默认 anthropic → 每次转发 400，界面看着全对）。
                只在用户没亲自动过协议下拉时才自动改，动过就只提示不覆盖。 */}
            <input
              className={`${inputCls} font-mono`}
              value={baseUrl}
              onChange={(e) => setBaseUrl(e.target.value)}
              onBlur={handleBaseUrlBlur}
              placeholder={t("editor.baseUrlPlaceholder")}
            />
          </Field>

          <Field label={initial?.hasSecret ? t("editor.apiKeyConfigured") : t("editor.apiKey")}>
            <div className="relative">
              <input
                type={showSecret ? "text" : "password"}
                className={`${inputCls} font-mono pr-9`}
                value={secret}
                onChange={(e) => setSecret(e.target.value)}
                placeholder={initial?.hasSecret ? "••••••••" : "sk-..."}
              />
              <button
                type="button"
                onClick={toggleShowSecret}
                disabled={revealing}
                title={showSecret ? t("editor.hideSecret") : t("editor.showSecret")}
                className="absolute right-2 top-1/2 -translate-y-1/2 rounded p-1 text-text-muted hover:text-text-secondary disabled:opacity-50"
              >
                {revealing ? (
                  <RefreshCw size={14} className="animate-spin" />
                ) : showSecret ? (
                  <EyeOff size={14} />
                ) : (
                  <Eye size={14} />
                )}
              </button>
            </div>
          </Field>

          {/* 参数 */}
          <div className="flex gap-3">
            <Field label="Temperature" className="flex-1">
              <input type="number" step={0.1} min={0} max={2} className={inputCls} value={temperature} onChange={(e) => setTemperature(Number(e.target.value))} />
            </Field>
            <Field label={t("editor.maxTokens")} className="flex-1">
              <input
                type="number"
                title={t("editor.maxTokensHint")}
                className={inputCls}
                value={maxTokens}
                onChange={(e) => setMaxTokens(Number(e.target.value))}
              />
            </Field>
            <Field label="请求超时 (ms)" className="flex-1">
              <input
                type="number"
                min={1000}
                step={1000}
                placeholder="30000"
                title="非流式转发的单请求超时,慢厂商可调大;留空=默认 30000。健康探测/拉模型固定≤30s;大脑聚合用大脑页「总超时」,均不受此值影响"
                className={inputCls}
                value={timeoutMs}
                onChange={(e) => setTimeoutMs(e.target.value === "" ? "" : Number(e.target.value))}
              />
            </Field>
          </div>

          {/* Max Tokens 批量应用：该值当前不参与任何请求，仅兼容旧配置（见
              applyMaxTokensToAll 的说明）。保留入口是为了让老配置能被统一清理/对齐。 */}
          <div className="flex items-center gap-2">
            <Button
              variant="outline"
              size="sm"
              onClick={applyMaxTokensToAll}
              disabled={applyingAll || !Number.isFinite(maxTokens) || maxTokens <= 0}
              title={t("editor.applyMaxTokensAllHint")}
            >
              {applyingAll ? (
                <RefreshCw size={13} className="animate-spin" />
              ) : (
                <Gauge size={13} />
              )}
              {t("editor.applyMaxTokensAll", { n: String(maxTokens) })}
            </Button>
            {applyAllMsg && <span className="text-xs text-success">{applyAllMsg}</span>}
          </div>

          {/* 模型拉取 */}
          <div>
            <div className="mb-1.5 flex items-center justify-between">
              <span className="text-xs font-medium text-text-secondary">{t("editor.modelList")}</span>
              <Button size="sm" variant="ghost" onClick={handleFetchModels} disabled={fetching}>
                <RefreshCw size={13} className={fetching ? "animate-spin" : ""} /> {t("editor.fetch")}
              </Button>
            </div>
            {/* 手动加模型：就地输入框 + 回车/按钮添加（不用原生 prompt） */}
            <div className="mb-1.5 flex gap-1.5">
              <input
                className={`${inputCls} font-mono`}
                placeholder={t("editor.modelManualPlaceholder")}
                value={manualModel}
                onChange={(e) => setManualModel(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") {
                    e.preventDefault();
                    addManualModel();
                  }
                }}
              />
              <Button size="sm" variant="secondary" onClick={addManualModel} disabled={!manualModel.trim()}>
                <Plus size={13} /> {t("common.add")}
              </Button>
            </div>
            {/* 上游不暴露 /v1/models 时，从当前厂商内置预设一键导入（参考 cc-switch 的 modelCatalog） */}
            {presetModels.length > 0 && (
              <div className="mb-1.5">
                <Button size="sm" variant="ghost" onClick={importPresetModels}>
                  <Download size={13} /> {t("editor.importPreset", { n: String(presetModels.length) })}
                </Button>
              </div>
            )}
            <div className="space-y-0.5 rounded-control border border-border p-2">
              {models.length === 0 ? (
                <span className="text-xs text-text-muted">{t("editor.noModels")}</span>
              ) : (
                models.map((m) => (
                  <div key={m.realName} className="flex items-center gap-2 rounded-control px-1 py-1 hover:bg-surface-hover">
                    <BrandIcon hint={vendor === "custom" ? m.realName : vendor} fallbackLabel={m.realName} size={20} />
                    <span className={`flex-1 truncate font-mono text-xs ${m.source === "manual" ? "text-text-secondary" : "text-text-primary"}`}>
                      {m.realName}
                    </span>
                    {/* 填了 ≥1M 窗口即会自动启用 1M 上下文（代理按落点模型补 anthropic-beta 头）。
                        给个可见徽标，避免又出现「配了但不知道生没生效」。 */}
                    {(m.contextWindow ?? 0) >= 1_000_000 && (
                      <span
                        className="shrink-0 rounded bg-accent/10 px-1.5 py-0.5 text-[10px] font-medium text-accent"
                        title={t("editor.oneMHint")}
                      >
                        1M
                      </span>
                    )}
                    <ContextWindowInput
                      value={m.contextWindow}
                      onChange={(val) =>
                        setModels(models.map((x) => x.realName === m.realName ? { ...x, contextWindow: val } : x))
                      }
                      onValidityChange={(valid) =>
                        setInvalidContextModels((prev) => {
                          const next = new Set(prev);
                          if (valid) next.delete(m.realName);
                          else next.add(m.realName);
                          return next;
                        })
                      }
                      title={t("editor.contextWindow")}
                      unitTitle={t("editor.contextWindowUnit")}
                      placeholder={t("editor.contextWindowPlaceholder")}
                    />
                    <button
                      onClick={() => {
                        setModels(models.filter((x) => x.realName !== m.realName));
                        setInvalidContextModels((prev) => {
                          const next = new Set(prev);
                          next.delete(m.realName);
                          return next;
                        });
                      }}
                      className="shrink-0 rounded p-1 text-text-muted hover:text-danger"
                      title={t("common.remove")}
                    >
                      <X size={12} />
                    </button>
                  </div>
                ))
              )}
            </div>
          </div>

          {/* 三档快捷映射（取自 cc-switch 的 haiku/sonnet/opus 语义，落到运行时代理）。
              仅 Claude CLI / 桌面端有意义：Claude Code 按任务发带 opus/sonnet/haiku 的模型名才触发档位改写。
              Codex 发 GPT 名匹配不到三档，且若 models 里有 claude-*opus* 之类名字反而会被误改写 → 故 Codex 隐藏。 */}
          {activeCategory !== "codex" && (
          <div>
            <div className="mb-1.5">
              <span className="text-xs font-medium text-text-secondary">{t("editor.tierTitle")}</span>
              <p className="mt-0.5 text-[11px] leading-relaxed text-text-muted">{t("editor.tierHint")}</p>
            </div>
            <div className="space-y-1.5">
              {([
                { icon: Zap, label: t("editor.tierHaiku"), value: tierHaiku, set: setTierHaiku, ph: "glm-4.5-air" },
                { icon: Gauge, label: t("editor.tierSonnet"), value: tierSonnet, set: setTierSonnet, ph: "glm-4.6" },
                { icon: Brain, label: t("editor.tierOpus"), value: tierOpus, set: setTierOpus, ph: "deepseek-reasoner" },
              ] as const).map((tier) => {
                const Icon = tier.icon;
                return (
                  <div key={tier.label} className="flex items-center gap-1.5">
                    <span className="flex w-24 shrink-0 items-center gap-1 text-xs text-text-secondary">
                      <Icon size={13} className="text-text-muted" /> {tier.label}
                    </span>
                    <ArrowRight size={14} className="shrink-0 text-text-muted" />
                    <div className="flex-1">
                      <Combobox
                        className={`${inputCls} font-mono`}
                        value={tier.value}
                        options={models.map((mm) => mm.realName)}
                        placeholder={tier.ph}
                        emptyHint={t("editor.comboNoModels")}
                        onChange={tier.set}
                      />
                    </div>
                  </div>
                );
              })}
            </div>
          </div>
          )}

          {/* 模型映射 */}
          <div>
            <div className="mb-1.5 flex items-center justify-between">
              <span className="text-xs font-medium text-text-secondary">{t("editor.mappingTitle")}</span>
              <Button size="sm" variant="ghost" onClick={addMapping}>
                <Plus size={13} /> {t("editor.addMapping")}
              </Button>
            </div>
            <div className="space-y-1.5">
              {/* 批量警告条（UX#4）：只在「还没配任何有效映射」时出——此时用户的对外名就是
                  models 里的真实名，一个都不合规，逐行提示会刷屏，给一键修法才是正解。
                  一旦有了映射，就转为逐行提示（下面那段），因为那时问题是具体某一行填错了。 */}
              {desktopIssues.length > 0 && !hasEffectiveMapping && (
                <div className="space-y-1.5 rounded-control border border-warning/40 bg-warning/10 px-2.5 py-2">
                  <div className="flex items-start gap-2 text-[11px] leading-relaxed text-warning">
                    <AlertTriangle size={12} className="mt-0.5 shrink-0" />
                    <span className="flex-1">
                      {t("editor.desktopNameBadBanner", { n: desktopIssues.length })}
                    </span>
                  </div>
                  <div className="text-[11px] leading-relaxed text-text-muted">
                    {t("editor.desktopNameFixAllHint")}
                    <br />
                    {t("editor.desktopNamePrefixUseless")}
                  </div>
                  <button
                    type="button"
                    onClick={fixAllByAddingMappings}
                    className="rounded border border-warning/40 px-2 py-0.5 text-[11px] font-medium text-warning hover:bg-warning/20"
                  >
                    {t("editor.desktopNameFixAll", { n: models.length })}
                  </button>
                </div>
              )}
              {mappings.length === 0 && (
                <span className="text-xs text-text-muted">{t("editor.noMapping")}</span>
              )}
              {mappings.map((m, i) => {
                // 逐行提示：只有「这一行的对外名」出现在体检结果里才提示。
                // realName 还空着的行不会进 serviceable_models，也就不会有 issue —— 刻意如此，
                // 否则用户填了一半就被警告、而保存其实会成功，属于假警报。
                const issue = desktopIssues.find((x) => x.name === m.expectedName.trim());
                return (
                  <div key={m.id} className="space-y-1">
                    <div className="flex items-center gap-1.5">
                      <div className="flex-1">
                        <Combobox
                          className={`${inputCls} font-mono`}
                          value={m.realName}
                          options={models.map((mm) => mm.realName)}
                          placeholder="GLM5.1"
                          emptyHint={t("editor.comboNoModels")}
                          onChange={(val) => {
                            const next = [...mappings];
                            next[i] = { ...m, realName: val };
                            setMappings(next);
                          }}
                        />
                      </div>
                      <ArrowRight size={14} className="shrink-0 text-text-muted" />
                      <input
                        className={`${inputCls} flex-1 font-mono ${issue ? "border-warning" : ""}`}
                        placeholder="opus-4-7"
                        value={m.expectedName}
                        onChange={(e) => {
                          const next = [...mappings];
                          next[i] = { ...m, expectedName: e.target.value };
                          setMappings(next);
                        }}
                      />
                      <button
                        onClick={() => setMappings(mappings.filter((x) => x.id !== m.id))}
                        className="shrink-0 rounded p-1.5 text-text-muted hover:bg-surface-hover hover:text-danger"
                      >
                        <Trash2 size={14} />
                      </button>
                    </div>
                    {issue && (
                      <div className="flex items-start gap-2 rounded-control bg-warning/10 px-2 py-1 text-[11px] leading-relaxed text-warning">
                        <AlertTriangle size={11} className="mt-0.5 shrink-0" />
                        <span className="flex-1">
                          {t("editor.desktopNameBadRow", { name: issue.name })}
                        </span>
                        <button
                          type="button"
                          onClick={() => applySuggestion(m.id, issue.suggestion)}
                          className="shrink-0 whitespace-nowrap rounded border border-warning/40 px-1.5 py-0.5 font-medium hover:bg-warning/20"
                        >
                          {t("editor.desktopNameFixTo", { name: issue.suggestion })}
                        </button>
                      </div>
                    )}
                  </div>
                );
              })}
            </div>
          </div>

          {/* 默认兜底模型（选填）：故障转移到本 Key 时，请求模型既无映射、本 Key 又不支持，则改用它。 */}
          <Field label={t("editor.defaultModel")}>
            <Combobox
              className={`${inputCls} font-mono`}
              value={defaultModel}
              options={models.map((m) => m.realName)}
              placeholder={t("editor.defaultModelPlaceholder")}
              emptyHint={t("editor.comboNoModels")}
              onChange={setDefaultModel}
            />
            <p className="mt-1 text-[11px] leading-relaxed text-text-muted">{t("editor.defaultModelHint")}</p>
          </Field>

          {/* ---- 余额查询与计费（第④批）----
              默认折叠：绝大多数用户不会配它，展开着会让本已很长的抽屉更难扫读。
              标题行常驻显示「已启用 / 未启用」，折叠状态下也能看出配没配。 */}
          <div className="rounded-control border border-border">
            <button
              type="button"
              onClick={() => setBalanceOpen((v) => !v)}
              className="flex w-full items-center gap-2 px-3 py-2 text-left"
            >
              <Wallet size={14} className="shrink-0 text-text-secondary" />
              <span className="flex-1 text-xs font-medium text-text-secondary">
                {t("balance.sectionTitle")}
              </span>
              {balance.enabled && (
                <span className="shrink-0 rounded-full bg-success/12 px-1.5 py-0.5 text-[10px] text-success">
                  {t("balance.on")}
                </span>
              )}
              <ChevronRight
                size={14}
                className={`shrink-0 text-text-muted transition-transform ${balanceOpen ? "rotate-90" : ""}`}
              />
            </button>

            {balanceOpen && (
              <div className="space-y-3 border-t border-border px-3 py-3">
                {/* 总开关 */}
                <label className="flex cursor-pointer items-start gap-2">
                  <input
                    type="checkbox"
                    checked={balance.enabled}
                    onChange={(e) => {
                      setBalance((b) => ({ ...b, enabled: e.target.checked }));
                      setBalanceProbe(null);
                    }}
                    className="mt-0.5"
                  />
                  <span className="text-xs text-text-primary">
                    {t("balance.enable")}
                    <span className="mt-0.5 block text-[11px] leading-relaxed text-text-muted">
                      {t("balance.enableHint")}
                    </span>
                  </span>
                </label>

                {balance.enabled && (
                  <>
                    {/* 预设模板 */}
                    <Field label={t("balance.template")}>
                      <div className="flex flex-wrap gap-1.5">
                        {BALANCE_TEMPLATE_ORDER.map((tpl) => (
                          <button
                            key={tpl}
                            type="button"
                            onClick={() => applyBalanceTemplate(tpl)}
                            className={`rounded-control border px-2 py-1 text-[11px] transition-colors ${
                              balance.template === tpl
                                ? "border-primary bg-primary/10 text-primary"
                                : "border-border text-text-secondary hover:bg-surface-hover"
                            }`}
                          >
                            {t(`balance.tpl.${tpl}`)}
                          </button>
                        ))}
                      </div>
                    </Field>

                    {/* 请求地址 */}
                    <Field label={t("balance.url")}>
                      <input
                        className={`${inputCls} font-mono`}
                        value={balance.url}
                        placeholder="{{baseUrl}}/v1/usage"
                        onChange={(e) => {
                          // 用户手改地址即视为自定义：模板高亮跟着走，
                          // 否则会出现「高亮在通用模板、地址却不是它」的错配。
                          setBalance((b) => ({ ...b, url: e.target.value, template: "custom" }));
                          setBalanceProbe(null);
                        }}
                      />
                      <p className="mt-1 text-[11px] leading-relaxed text-text-muted">
                        {t("balance.urlHint")}
                      </p>
                    </Field>

                    {/* 认证方式 + 超时。
                        「自动查询间隔」那一格已移除：字段本身留在数据结构里（后端
                        `BalanceQuery.auto_interval_min` 已定义），但**没有任何定时任务读它**
                        —— 摆一个填了不生效的输入框，正是本项目反复防的「静默失效开关」。
                        等定时查询真正落地再把它加回来。 */}
                    <div className="grid grid-cols-2 gap-2">
                      <Field label={t("balance.auth")}>
                        <select
                          className={inputCls}
                          value={balance.auth}
                          onChange={(e) => setBalance((b) => ({ ...b, auth: e.target.value }))}
                        >
                          <option value="bearer">Bearer</option>
                          <option value="x-api-key">x-api-key</option>
                          <option value="access-token">Access Token</option>
                          <option value="none">{t("balance.authNone")}</option>
                        </select>
                      </Field>
                      <Field label={t("balance.timeout")}>
                        <input
                          type="number"
                          min={1}
                          className={inputCls}
                          value={balance.timeoutSecs}
                          onChange={(e) =>
                            setBalance((b) => ({ ...b, timeoutSecs: Number(e.target.value) || 10 }))
                          }
                        />
                      </Field>
                    </div>

                    {/* Access Token / 用户 id：只在选了 access-token 认证时才出现。
                        NewAPI 类面板认的是面板登录态而非 API Key，两个都得填；
                        其它认证方式下这两格无意义，显示出来只会让人以为漏填了东西。 */}
                    {balance.auth === "access-token" && (
                      <div className="grid grid-cols-2 gap-2">
                        <Field label={t("balance.accessToken")}>
                          <input
                            className={`${inputCls} font-mono`}
                            value={balance.accessToken ?? ""}
                            onChange={(e) =>
                              setBalance((b) => ({ ...b, accessToken: e.target.value }))
                            }
                          />
                        </Field>
                        <Field label={t("balance.userId")}>
                          <input
                            className={`${inputCls} font-mono`}
                            value={balance.userId ?? ""}
                            onChange={(e) => setBalance((b) => ({ ...b, userId: e.target.value }))}
                          />
                        </Field>
                      </div>
                    )}
                    {balance.auth === "access-token" && (
                      <p className="text-[11px] leading-relaxed text-text-muted">
                        {t("balance.accessTokenHint")}
                      </p>
                    )}

                    {/* 取值路径（留空 = 自动探测） */}
                    <Field label={t("balance.remainingPath")}>
                      <input
                        className={`${inputCls} font-mono`}
                        value={balance.remainingPath ?? ""}
                        placeholder={t("balance.remainingPathPlaceholder")}
                        onChange={(e) =>
                          setBalance((b) => ({ ...b, remainingPath: e.target.value }))
                        }
                      />
                      <p className="mt-1 text-[11px] leading-relaxed text-text-muted">
                        {t("balance.remainingPathHint")}
                      </p>
                    </Field>

                    {/* 凭证覆盖：默认折叠在一行提示后，多数站点用不到 */}
                    <details className="rounded-control bg-surface-hover/40 px-2.5 py-2">
                      <summary className="cursor-pointer text-[11px] text-text-secondary">
                        {t("balance.overrideTitle")}
                      </summary>
                      <div className="mt-2 space-y-2">
                        <Field label={t("balance.baseUrlOverride")}>
                          <input
                            className={`${inputCls} font-mono`}
                            value={balance.baseUrlOverride ?? ""}
                            placeholder={t("balance.baseUrlOverridePlaceholder")}
                            onChange={(e) =>
                              setBalance((b) => ({ ...b, baseUrlOverride: e.target.value }))
                            }
                          />
                        </Field>
                        <p className="text-[11px] leading-relaxed text-text-muted">
                          {t("balance.overrideHint")}
                        </p>
                      </div>
                    </details>

                    {/* 测试查询 */}
                    <div className="flex items-center gap-2">
                      <Button
                        size="sm"
                        variant="secondary"
                        onClick={() => void probeBalance()}
                        disabled={probing}
                      >
                        <RefreshCw size={13} className={probing ? "animate-spin" : ""} />
                        {probing ? t("balance.probing") : t("balance.probe")}
                      </Button>
                    </div>

                    {/* 探测结果：成功显示数值，失败**如实显示原因**。
                        绝不把失败显示成「余额 0」——那会让用户以为额度真用光了。 */}
                    {balanceProbe && (
                      <div
                        className={`rounded-control px-3 py-2 text-xs leading-relaxed ${
                          balanceProbe.ok
                            ? "bg-success/10 text-success"
                            : "bg-danger/10 text-danger"
                        }`}
                      >
                        {balanceProbe.ok ? (
                          <>
                            {/* 与卡片同一个格式化函数：同一笔余额在两处显示成
                                `84.2` 和 `84.20` 两种写法会让人怀疑哪个是真的。
                                它也保证极小额不塌成 0（见 lib/balance.ts）。 */}
                            {t("balance.probeOk", {
                              amount:
                                balanceProbe.remaining != null
                                  ? formatBalanceAmount(balanceProbe.remaining)
                                  : "?",
                              unit: balanceProbe.unit ?? "USD",
                            })}
                            {/* 套餐名与总额/已用：上游给了才显示，不硬凑 */}
                            {balanceProbe.planName && (
                              <span className="ml-1 opacity-80">
                                · {balanceProbe.planName}
                              </span>
                            )}
                            {balanceProbe.total != null && (
                              <span className="ml-1 opacity-80">
                                · {t("balance.ofTotal", {
                                  total: formatBalanceAmount(balanceProbe.total),
                                  unit: balanceProbe.unit ?? "USD",
                                })}
                              </span>
                            )}
                            {balanceProbe.isValid === false && (
                              <span className="mt-1 block text-warning">
                                {balanceProbe.invalidMessage ?? t("balance.keyInactive")}
                              </span>
                            )}
                          </>
                        ) : (
                          balanceProbe.error
                        )}
                      </div>
                    )}
                  </>
                )}

                {/* 计费倍率：与余额查询无关，但同属「钱」这一类，放一起用户好找。
                    即使不开余额查询也能配（用量页靠它算金额）。 */}
                <Field label={t("balance.multiplier")}>
                  <input
                    className={`${inputCls} font-mono`}
                    value={costMultiplier}
                    placeholder="1.0"
                    onChange={(e) => setCostMultiplier(e.target.value)}
                  />
                  <p className="mt-1 text-[11px] leading-relaxed text-text-muted">
                    {t("balance.multiplierHint")}
                  </p>
                </Field>
              </div>
            )}
          </div>

          {/* 保存错误：后端的校验消息是多行的（如桌面端模型名不合规会附后果与修法），
              必须 whitespace-pre-line 保留换行，否则挤成一坨没人读得下去。 */}
          {error && (
            <div className="whitespace-pre-line rounded-control bg-danger/10 px-3 py-2 text-xs leading-relaxed text-danger">
              {error}
            </div>
          )}
        </div>

        {/* 底部操作 */}
        <div className="flex items-center justify-end gap-2 border-t border-border px-5 py-3">
          <Button variant="ghost" onClick={onClose} disabled={saving}>{t("common.cancel")}</Button>
          <Button onClick={save} disabled={saving}>{saving ? t("common.saving") : t("common.save")}</Button>
        </div>
      </div>
    </div>
  );
}

const inputCls =
  "h-9 w-full rounded-control border border-border bg-surface px-3 text-sm text-text-primary focus:outline-none focus:ring-2 focus:ring-ring";

function Field({
  label,
  children,
  className,
}: {
  label: string;
  children: React.ReactNode;
  className?: string;
}) {
  return (
    <div className={className}>
      <div className="mb-1 text-xs font-medium text-text-secondary">{label}</div>
      {children}
    </div>
  );
}

/** 单位换算与精确往返（`token`/`K`/`M`）抽到 [`@/lib/tokenUnit`] 便于单测（审计 A2-04）。 */

/**
 * 上下文窗口输入：数值框 + 单位下拉，替代早期「一个框里既能填纯数字又能填 200k/1M」的设计
 * ——那种写法要求用户记住语法，改成显式选单位后，数值框永远只接受数字，心智负担更低。
 *
 * 两个草稿状态（数值、单位）分离而不合并成一个字符串再解析：单位切换不该触发数值校验，
 * 数值输入也不该受单位状态影响，各自独立才不会出现「切单位时把已输入的数值吃掉」之类耦合。
 */
function ContextWindowInput({
  value,
  onChange,
  onValidityChange,
  title,
  unitTitle,
  placeholder,
}: {
  value: number | undefined;
  onChange: (v: number | undefined) => void;
  onValidityChange: (valid: boolean) => void;
  title: string;
  unitTitle: string;
  placeholder: string;
}) {
  const [amountDraft, setAmountDraft] = useState<string | null>(null);
  const [unit, setUnit] = useState<TokenUnit>(() => preferredUnit(value));
  const [bad, setBad] = useState(false);
  const nativeBadInput = useRef(false);

  // 单位是用户的显式选择，父组件回写 value 时不能擅自改档：
  // 用户填 `1.5` 选 M 后，value=1_500_000；若重新按「能整除的最大单位」拆分，
  // 会跳成 `1500 k`，数值虽相同但用户刚选的 M 被吃掉。按当前 unit 反算即可保持 `1.5 M`。
  const externalAmount = amountForUnit(value, unit);
  const shownAmount = amountDraft ?? externalAmount;

  const commitAmount = (rawAmount: string, rawUnit: TokenUnit) => {
    const trimmed = rawAmount.trim();
    if (trimmed === "") {
      setBad(false);
      setAmountDraft(null);
      onValidityChange(true);
      onChange(undefined);
      return;
    }
    // 只接受正数；十进制精确换算后必须是整数 token，不做静默四舍五入。
    const tokens = tokensFromAmount(trimmed, rawUnit);
    if (tokens === null) {
      setBad(true);
      onValidityChange(false);
      return;
    }
    setBad(false);
    setAmountDraft(null);
    onValidityChange(true);
    onChange(tokens);
  };

  return (
    <div
      className="flex shrink-0 items-center gap-1"
      onBlur={(e) => {
        // 焦点仍在「数值 + 单位」组合内部时不提交：点击单位下拉会先触发 input blur，
        // 若此时按旧单位提交，再处理 select change，就会短暂写入错误值。
        if (e.currentTarget.contains(e.relatedTarget as Node | null)) return;
        if (nativeBadInput.current) {
          setBad(true);
          onValidityChange(false);
          return;
        }
        commitAmount(amountDraft ?? externalAmount, unit);
      }}
    >
      <input
        type="number"
        inputMode="decimal"
        min="0"
        step="any"
        className={`h-7 w-16 rounded-control border bg-bg px-2 text-xs text-text-primary placeholder:text-text-muted ${
          bad ? "border-danger" : "border-border"
        }`}
        placeholder={placeholder}
        value={shownAmount}
        onChange={(e) => {
          // Chromium 对 `type=number` 的某些中间态（如单独一个 `-`）会给 value=""
          // 同时 validity.badInput=true。若只看 value，会把「输入未完成」误判成用户主动清空，
          // blur 后静默删掉原有 contextWindow。badInput 时保留上一个草稿并阻止 Save。
          if (e.currentTarget.validity.badInput) {
            nativeBadInput.current = true;
            setBad(true);
            onValidityChange(false);
            return;
          }
          nativeBadInput.current = false;
          const nextDraft = e.target.value;
          setAmountDraft(nextDraft);

          // 合法草稿立即同步父状态，Save 永远拿到最新值，不依赖 blur 与 click 的事件顺序。
          // 非法草稿保留在输入框中但不上报，且 validity=false 会阻止 Save。
          const trimmed = nextDraft.trim();
          const parsed = trimmed === "" ? undefined : tokensFromAmount(trimmed, unit);
          const valid = parsed !== null;
          onValidityChange(valid);
          if (valid) onChange(parsed);
          if (bad) setBad(false);
        }}
        onKeyDown={(e) => {
          if (e.key === "Enter") {
            e.preventDefault();
            commitAmount(amountDraft ?? externalAmount, unit);
          } else if (e.key === "Escape") {
            nativeBadInput.current = false;
            setAmountDraft(null); // 放弃本次编辑，回到外部值
            setBad(false);
            onValidityChange(true);
          }
        }}
        title={title}
      />
      <select
        className="h-7 rounded-control border border-border bg-bg px-1 text-xs text-text-primary"
        value={unit}
        onChange={(e) => {
          if (nativeBadInput.current) {
            setBad(true);
            onValidityChange(false);
            return;
          }
          const nextUnit = e.target.value as TokenUnit;
          // 数值 + 单位共同构成真实 token 数：输入 1 后选 M，就是 1,000,000；
          // 输入 200 后选 K，就是 200,000。单位选择不是纯显示格式切换。
          const amountNow = amountDraft ?? externalAmount;
          setUnit(nextUnit);
          if (amountNow.trim() !== "") {
            commitAmount(amountNow, nextUnit);
          }
        }}
        title={unitTitle}
      >
        <option value="token">tok</option>
        <option value="K">K</option>
        <option value="M">M</option>
      </select>
    </div>
  );
}
