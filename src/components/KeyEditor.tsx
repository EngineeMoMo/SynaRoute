import { useEffect, useRef, useState } from "react";
import { api } from "@/lib/bridge";
import { useStore } from "@/store";
import { useT } from "@/lib/useT";
import { Button } from "@/components/ui/Button";
import { Combobox } from "@/components/ui/Combobox";
import { BrandIcon } from "@/components/BrandIcon";
import { BrandPickerDialog, BrandPickerTrigger } from "@/components/BrandPickerDialog";
import { SaveErrorDialog } from "@/components/SaveErrorDialog";
import { MAX_COST_MULTIPLIER, isValidCostMultiplier } from "@/lib/costMultiplier";
import { CostMultiplierField } from "@/components/CostMultiplierField";
import { CustomHeadersField } from "@/components/CustomHeadersField";
import { ModelMappingSection, type TierValues } from "@/components/ModelMappingSection";
import type {
  BalanceQuery,
  BalanceResult,
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
import { X, RefreshCw, Plus, Eye, EyeOff, Download, AlertTriangle, Wallet, ChevronRight } from "lucide-react";

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
  // 中转站（NewAPI 的 OpenAI 兼容计费层）：`GET /v1/dashboard/billing/subscription`，
  // 认的是**转发用的 API Key**，不需要另去面板拿 access token —— 故它比下面的 newapi
  // 模板好用得多，绝大多数中转站首选这条。
  //
  // **实测来源**（2026-08-16，sotamodel.net）：
  //   /user/balance                        → 200 但 Content-Type: text/html（是网页，不是 API）
  //   /api/user/self                       → 200 JSON，但报 "invalid access token"（要面板登录态）
  //   /v1/dashboard/billing/subscription   → 401 JSON `{"error":{…,"type":"new_api_error"}}`
  //                                          ← 只有这条认 API Key
  // 用户此前选 generic 一直失败，正是因为缺这个模板。返回里的余额字段是
  // `hard_limit_usd`，已补进后端候选链（见 balance.rs REMAINING_CANDIDATES 末尾）。
  relay: { url: "{{origin}}/v1/dashboard/billing/subscription", method: "GET", auth: "bearer" },
  // NewAPI 系面板：认的是**面板登录态**（access token + 用户 id），不是转发用的 API Key。
  // 只在 relay 模板取不到值时才需要它（要用户去面板复制 access token 与用户 id）。
  newapi: { url: "{{baseUrl}}/api/user/self", method: "GET", auth: "access-token" },
  // DeepSeek：余额端点在**域名根**下，而它的 baseUrl 常带 `/anthropic` 后缀，
  // 故必须用 `{{origin}}`（剥掉路径）而非 `{{baseUrl}}` —— 实测后者 404、前者 200。
  deepseek: { url: "{{origin}}/user/balance", method: "GET", auth: "bearer" },
  // 官方 Anthropic
  official: { url: "{{baseUrl}}/v1/organizations/me", method: "GET", auth: "x-api-key" },
};

/** 模板按钮的顺序与文案 key。「自动」在最前：它是默认值，也是绝大多数用户唯一需要的一项。 */
const BALANCE_TEMPLATE_ORDER = ["auto", "custom", "relay", "generic", "newapi", "deepseek", "official"] as const;

/**
 * 新建 Key 时的余额查询初值：默认**关闭**，但用「自动」模板（url/auth 留空）。
 *
 * 为什么初值不再是 generic：generic 写死 `{{baseUrl}}/user/balance`，而中转站普遍
 * 在那个路径上返回网页而非 API（实测 sotamodel）—— 新用户一开开关就失败，而他无从
 * 判断自己的站该选哪个模板。留空则由后端 `detect_balance_endpoint` 按域名自动识别
 * （对齐 cc-switch 的 detect_provider），认不出时兜底到中转站命中率最高的那条端点。
 */
/**
 * base_url 是否为可用的 http(s) 地址。
 *
 * 用 `URL` 构造器解析（Tauri webview 里可用），并**必须**是 http/https 协议：
 * - `api.foo.com`（缺协议头）→ `new URL` 抛错 → false；
 * - `foo`（纯文本）→ 抛错 → false；
 * - `ftp://x` / `file://x` → 能解析但协议不对 → false（转发只走 http(s)）；
 * - `https://api.example.com` / 带 `/v1` 路径 → true。
 *
 * 不校验域名可达性（那要联网、且健康探测会做）；这里只挡「一眼就不可能转发成功」的格式。
 */
function isValidHttpUrl(raw: string): boolean {
  try {
    const u = new URL(raw.trim());
    return u.protocol === "http:" || u.protocol === "https:";
  } catch {
    return false;
  }
}


function defaultBalanceQuery(): BalanceQuery {
  return {
    enabled: false,
    template: "auto",
    // url/auth 刻意留空 —— 后端见空即自动识别。填了值就等于替用户做了选择，
    // 而这正是「一直不行」的根因（选错模板且用户不知道该选哪个）。
    url: "",
    method: "GET",
    auth: "",
    timeoutSecs: 10,
    autoIntervalMin: 0,
  };
}

/** 新增/编辑 Key 抽屉面板（FR-002/004/005/006） */
export function KeyEditor({ initial, onClose, onSaved }: KeyEditorProps) {
  const activeCategory = useStore((s) => s.activeCategory);
  const loadCategory = useStore((s) => s.loadCategory);
  const storeCheckHealth = useStore((s) => s.checkHealth);
  const vendors = useStore((s) => s.vendors);
  const setBalanceResult = useStore((s) => s.setBalanceResult);
  const clearBalance = useStore((s) => s.clearBalance);
  const t = useT();
  /**
   * 这条 Key 还没落盘 —— 决定标题、新建提示条、以及「拉模型前必须先填密钥」那道校验。
   * 🔴 判据是 `!initial?.id` 而**不是** `!initial`，理由与代价见 `lib/keyCopy.ts`（复制路径）。
   */
  const isNew = !initial?.id;

  /**
   * 本次编辑会话中**已落盘的真实 id**。
   *
   * 挂载时取 initial.id（编辑已有 Key）或空串（新建）；`probeBalance` 先保存拿到后端
   * 回填的 uuid 后更新它。`buildDraftKey` 读它而**不是** `initial?.id` ——
   * 这样无论调用方是否通过 onSaved 回填 initial prop（App.tsx 回填了，
   * OnboardingWizard 只推进步骤不回填），后续「保存」都走 update 而不是再 insert 一条。
   *
   * 真机复现过的事故形态（2026-08-16）：首启向导里「测试查询 → 保存」插出两条相同 Key，
   * 因为向导的 initial 恒为 null，第二次 upsert 仍带空 id。修调用方只能修一处，
   * 在组件内保活 id 才能覆盖所有现在与将来的调用方。
   */
  const [persistedId, setPersistedId] = useState(initial?.id ?? "");

  const [name, setName] = useState(initial?.name ?? "");
  const [vendor, setVendor] = useState(initial?.vendor ?? "custom");
  const [baseUrl, setBaseUrl] = useState(initial?.baseUrl ?? "");
  const [protocol, setProtocol] = useState<Protocol>(initial?.protocol ?? "anthropic");
  const [secret, setSecret] = useState("");
  const [showSecret, setShowSecret] = useState(false);
  const [revealing, setRevealing] = useState(false);
  // 空 = 不发送该字段(同下方 timeoutMs)。刻意**不预填 1.0**:那会让每条 Key 都带一个用户没配过的采样参数,而同协议直通到 Responses 上游时它原样发出即 400(见 proxy.rs 的 `skip_sampling`)。
  const [temperature, setTemperature] = useState<number | "">(initial?.params.temperature ?? "");
  // 请求超时(ms):空 = 未设(后端默认 30000),服务非流式转发、慢厂商可调大;健康探测/拉模型后端固定封顶 30s,大脑聚合用大脑页「总超时」——均不受此值放大影响。
  const [timeoutMs, setTimeoutMs] = useState<number | "">(initial?.params.timeoutMs ?? "");
  const [models, setModels] = useState<ModelInfo[]>(initial?.models ?? []);
  const [mappings, setMappings] = useState<ModelMapping[]>(initial?.mappings ?? []);
  const [defaultModel, setDefaultModel] = useState(initial?.defaultModel ?? "");
  // 四档快捷映射放一个对象：它们总是一起读、一起落盘，四个 useState 只会让
  // 「加第五档」变成四处改动（而 mythos 迟早会有人问）。字段名与 ProviderKey 的 tier* 对应。
  const [tiers, setTiers] = useState<TierValues>({
    haiku: initial?.tierHaiku ?? "",
    sonnet: initial?.tierSonnet ?? "",
    opus: initial?.tierOpus ?? "",
    fable: initial?.tierFable ?? "",
  });
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
  const [headersJson, setHeadersJson] = useState(initial?.headersJson ?? "");
  /** 这条 Key 的图标覆盖（预设键或 data-URL）；undefined = 跟着厂商走。 */
  const [icon, setIcon] = useState<string | undefined>(initial?.icon);
  const [iconPickerOpen, setIconPickerOpen] = useState(false);
  const [fetching, setFetching] = useState(false);
  const [error, setError] = useState<string | null>(null);
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
  /** 当前所选厂商记录（历史自由文本值找不到时为 undefined）。图标兜底取它的 `icon`。 */
  const selectedVendor = vendors.find((v) => v.id === vendor);

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
        enabled: initial?.enabled ?? true,
        priority: initial?.priority ?? 999,
        params: { ...initial?.params, temperature: typeof temperature === "number" ? temperature : undefined, timeoutMs: typeof timeoutMs === "number" && timeoutMs >= 1000 ? timeoutMs : undefined },
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
    // 「自动」= 清空 url/auth，让后端按 base_url 域名自动识别端点
    // （对齐 cc-switch 的 detect_provider）。清空是关键：后端见空才走自动识别，
    // 留着旧模板的值等于继续用那个可能选错的端点。
    if (tpl === "auto") {
      setBalance((b) => ({ ...b, template: "auto", url: "", auth: "", method: "GET" }));
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
    // 「测试查询」会**先把表单落盘**（见下方注释），故校验必须与「保存」**逐条一致**：
    // 不拦的话，名称/地址还没填就点测试 → 一条无名无地址、无法转发的 Key 已被插入，
    // 用户没点过保存却「多了一条空 Key」，且关抽屉也不会回收它。
    // 「逐条一致」不是修辞：下面三道（名称、地址非空、地址格式）与 `save` 的前三道一一对应，
    // 少任何一道就等于开了一条绕过校验的落盘后门。
    if (!name.trim()) {
      setBalanceProbe({ ok: false, queriedAt: Date.now(), error: t("editor.errNeedName") });
      return;
    }
    if (!baseUrl.trim()) {
      setBalanceProbe({ ok: false, queriedAt: Date.now(), error: t("editor.errNeedBaseUrl2") });
      return;
    }
    // 与 `save` 完全同一道校验，不只是判空。
    //
    // 上面那句「与保存同一口径」此前是**假的**：`save` 拦 `isValidHttpUrl`，这里只拦空串。
    // 于是 `api.foo.com`（缺协议头）、`ftp://x`、`foo` 这类**保存会被拒**的地址，
    // 从「测试查询」这条路径照样落盘 —— 而它同样调 `upsert_key`，落的是同一张表。
    // 后果不是「测试失败」这么轻：用户没点保存却多了一条 Key，且这条 Key 每次转发都因
    // URL 无法解析而失败，回到编辑器点保存时才第一次看到「地址格式无效」，
    // 完全对不上「这条 Key 是怎么进来的」。
    //
    // 校验放在 upsert 之前而不是之后：一旦落盘就已经造成了脏数据，事后报错也收不回来。
    if (!isValidHttpUrl(baseUrl)) {
      setBalanceProbe({ ok: false, queriedAt: Date.now(), error: t("editor.errInvalidBaseUrl") });
      return;
    }
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
      // 保活落盘 id：此后 buildDraftKey 带真实 uuid，再点「保存」走 update 而非再 insert。
      // 这一行就是 persistedId 机制的写入点，漏掉它 = 首启向导「测一次多一条 Key」复现。
      setPersistedId(saved.id);
      const keyId = saved.id;
      if (secret) await api.saveSecret(keyId, secret);
      // force=true 跳过缓存，确保「测试查询」总是查询上游最新值
      const result = await api.queryKeyBalance(keyId, true);
      setBalanceProbe(result);
      // 同一结果直接写进卡片的余额缓存 —— **仅当成功时**。
      //
      // 为什么要判 result.ok：probeBalance 直调 api.queryKeyBalance，绕过了 store.refreshBalance
      // 的 balanceLoading 去重门。若此刻自动轮询恰好在途，后端 in-flight 哨兵会返回一条
      // 带 `transient: true` 的**伪失败**（压根没打上游）；无条件写缓存会把它盖到卡片上，
      // 甚至可能覆盖轮询随后落盘的真实成功值。
      // 判 `ok` 已经涵盖了这种情况（伪失败的 ok 必为 false），store 那侧另有 `transient` 显式门
      // 作为第二道；这里保持「只有成功才入共享缓存」这条更强的约束不变。
      // 伪失败只在编辑器内经 setBalanceProbe 展示即可。
      if (result.ok) {
        // 指纹用**刚落盘的那条**算（`saved` 而非 `draft`/`initial`）：那才是产出本结果的配置，
        // 也正是卡片重新渲染后会算出的指纹 —— 两者一致，卡片才会判成缓存有效而不再发一次
        // 一模一样的请求。用 draft 算会在新建时差一个 id、指纹对不上，白发两个请求。
        setBalanceResult(keyId, balanceFingerprint(saved), result);
      }
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
    //
    // 读 `persistedId` 而非 `initial?.id`：probeBalance 先保存后会把后端 uuid 写进
    // persistedId，此后再点「保存」走 update；读 initial 会在「调用方不回填 initial」时
    // 二次 insert（首启向导真机复现过），见 persistedId 的声明注释。
    id: persistedId,
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
    // ⚠️ 只沿用旧值，**不因表单里填了密钥就预置 true**：save/probeBalance 都是先 upsert
    // 再 saveSecret，若这里带 true 而 saveSecret 随后失败（主口令锁定态、secrets.enc 写盘
    // 失败），config 里就永久记着「已配密钥」而密钥库里没有 —— UI 显示已配置、转发却报
    // 「未配置密钥」，且无对账路径修正。后端 save_secret 的设计就是「写库成功后才置位」
    // （service.rs），这里预置 true 等于从载荷侧绕过那道防线。
    hasSecret: initial?.hasSecret ?? false,
    // 新建默认**启用**：与 cc-switch 导入同口径。原先默认 false，而首启向导第②步只看
    // 「已有 N 条 Key」就放行 → 第③步接入后 enabled_keys_sorted 为空、每个请求都没有候选；
    // 桌面端更糟，报「需要至少一个可服务模型」，把用户引向「没配模型」这个完全错误的方向。
    enabled: initial?.enabled ?? true,
    allowInAggregate: initial?.allowInAggregate, // 必须透传：入口在 Key 卡片上，漏了则保存一次就被静默清掉（有 parity 判据钉住）
    priority: initial?.priority ?? 999,
    params: { ...initial?.params, temperature: typeof temperature === "number" ? temperature : undefined, timeoutMs: typeof timeoutMs === "number" && timeoutMs >= 1000 ? timeoutMs : undefined },
    models,
    mappings: mappings.filter((m) => m.expectedName && m.realName),
    defaultModel: defaultModel.trim() || undefined,
    // 档位仅 Claude CLI/桌面端有意义；Codex 一律不落，避免 claude-*opus* 类名字被误改写路由
    // （即使该 Key 早前存过档位，此处也强制清空）。
    tierHaiku: activeCategory === "codex" ? undefined : tiers.haiku.trim() || undefined,
    tierSonnet: activeCategory === "codex" ? undefined : tiers.sonnet.trim() || undefined,
    tierOpus: activeCategory === "codex" ? undefined : tiers.opus.trim() || undefined,
    tierFable: activeCategory === "codex" ? undefined : tiers.fable.trim() || undefined,
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
    headersJson: headersJson.trim() || undefined,
    icon,
  });

  /**
   * 桌面端对外模型名的**即时**体检（UX#4）—— 提供给 `ModelMappingSection`。
   *
   * 为什么值得单开一条 IPC：对外名不合规会被 Claude 桌面端**静默过滤掉**，全被过滤则模型
   * 选择器为空、打开会话报 ModelsNotDiscoveredError（本项目记录过的最难排查的症状之一）。
   * 此前只在「保存」那一刻拦，用户可能已经填完整个表单（13 个字段）才被拒。
   * 判据**不在前端复刻**（理由与代价见 `bridge.ts` 那条命令的文档）。
   *
   * 🔴 **入参是 mappings 而不是无参**：调用方有两种用法 —— 常规体检传当前值，
   * 「求一个合规对外名」时传一份把 `expectedName` 置成 `realName` 的探测副本。
   * 而 draft 必须由这里拼（体检的输入源是 `serviceable_models()`，与保存拦截同一个集合；
   * 两边各拼一份就会出现「界面说没问题、保存却被拒」的自相矛盾）。
   *
   * 防抖、cancelled 竞态、以及「失败只清空、绝不阻断保存」都在 `ModelMappingSection` 里 ——
   * 那三条约束服务的是那一区的显示，跟着它走。
   */
  const probeDesktopNames = (mappings: ModelMapping[]) =>
    api.checkDesktopModelNames({ ...buildDraftKey(), mappings });

  const save = async () => {
    if (!name.trim()) return setError(t("editor.errNeedName"));
    if (!baseUrl.trim()) return setError(t("editor.errNeedBaseUrl2"));
    // base_url 基本格式校验：必须是可解析、且 http(s) 协议的 URL。缺协议头（api.foo.com）或
    // 纯文本（foo）此前会静默落盘，之后每次转发都因 URL 无法解析而失败，用户只在客户端看到
    // 底层报文、就地无可行动提示。就地拦下并给出示例，是最省事的修复入口。
    if (!isValidHttpUrl(baseUrl)) return setError(t("editor.errInvalidBaseUrl"));
    const currentModelNames = new Set(models.map((m) => m.realName));
    // 校验集里现在**只有上下文窗口**一类 tag（裸模型名）——「最大单次输出」输入框已移除，
    // 那一项由后端自动定。原先还有 `模型名::maxout` 后缀的一类，随输入框一并去掉。
    const invalidWindow = [...invalidContextModels].some((tag) => currentModelNames.has(tag));
    if (invalidWindow) return setError(t("editor.errInvalidContextWindow"));
    // 计费倍率与上面两个数值字段同一口径：拦在保存处。
    // 只在编辑器里标红是不够的 —— 抽屉一关那条提示就没了，而这条 Key 会带着一个
    // 后端只会静默退回 1.0 的废值一直存在，用量页金额差几倍且没有任何地方提示过。
    if (!isValidCostMultiplier(costMultiplier)) {
      return setError(t("balance.multiplierInvalid", { max: String(MAX_COST_MULTIPLIER) }));
    }

    const key = buildDraftKey();

    setSaving(true);
    setError(null);
    try {
      // **必须用后端返回的 key**：新建时本地 id 是空串，真正的 uuid 由后端生成并回填。
      // 用本地那个空 id 去 saveSecret/checkHealth 会写到一条不存在的 Key 上（密钥成孤儿、
      // 探测无对象），而界面看起来一切正常——正是本项目最防的静默失效形态。
      const saved = await api.upsertKey(key);
      // 保活落盘 id：若后续 saveSecret/loadCategory 抛错走 catch（编辑器不关闭），
      // 用户重试「保存」必须走 update —— 否则每点一次就多 insert 一条。
      setPersistedId(saved.id);
      if (secret) {
        await api.saveSecret(saved.id, secret);
        // 写了新密钥 → 作废该 Key 的余额缓存。余额指纹不含密钥（前端只有 hasSecret，
        // 拿不到明文），仅改密钥时指纹不变，卡片会继续复用旧的 401/陈旧余额最长 5 分钟，
        // 看着像「换了密钥没生效」。清缓存后卡片下次渲染会用新密钥重查。（本轮审查确认）
        clearBalance(saved.id);
      }
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
      // **必须走 store.checkHealth 而非自己 await loadCategory(activeCategory)**：后者的
      // activeCategory 是本组件挂载期冻结的旧值，健康探测可能耗时数秒，期间用户切了分类，
      // 探测返回后 loadCategory(旧分类) 会把当前分类页整表覆盖成另一分类的 Key（切分类串台）。
      // store.checkHealth 用 get().activeCategory（实时）且只替换目标那一条 Key，正为此设计。
      void storeCheckHealth(saved.id);
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
      <div className="flex h-full w-[min(560px,100vw)] flex-col bg-surface shadow-2xl">

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
                <BrandIcon
                  hint={vendor}
                  fallbackLabel={name || vendor}
                  iconUrl={icon ?? selectedVendor?.icon}
                  size={28}
                />
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

          {/* 图标预设**自成一行、占满宽度**。
              放在「厂商 / 协议」那一行的左列里过不去：那一行是 `flex gap-3`，左列 `flex-1`、
              右列是固定 `w-48` 的协议下拉。触发器被框在左列宽度内，而右列下方没东西可填
              → 触发器右边空出一块（真机报障「厂商后面没有自适应」）。
              挪出来之后触发器的 `w-full` 才真的等于抽屉宽度，那块空白随之消失。

              图标写的是**这条 Key 自己的 `icon`**，不写厂商。原设计写厂商，被真机否掉了：
              绝大多数中转站 Key 选的厂商就是内置的「自定义」，而内置厂商只读 →
              界面显示「图标不可修改」，而这恰恰是最需要选图标的场景（真机原话：
              「自定义时是需要能选预设图标，你设置的不能更改 不对」）。
              另一层理由：「自定义」下会挂很多条指向不同站点的 Key，把图标记在那个共享厂商上，
              改一条会连带改掉其余全部。故任何厂商下都可选，选中值只作用于当前这条 Key。

              挑选器走**弹窗**而不是内联展开：内联那版把整行撑到 ~200px 高，
              32 个品牌的目录（搜索 + 分组 + 滚动）与旁边的单选下拉不是同一个量级。
              详见 BrandPickerDialog。 */}
          <Field label={t("editor.iconPresetLabel")}>
            <BrandPickerTrigger
              value={icon}
              vendorHint={vendor}
              fallbackLabel={name || vendor}
              onOpen={() => setIconPickerOpen(true)}
            />
          </Field>

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
            <Field label={t("editor.temperature")} className="flex-1">
              <input type="number" step={0.1} min={0} max={2} placeholder="1.0" className={inputCls} value={temperature} onChange={(e) => setTemperature(e.target.value === "" ? "" : Number(e.target.value))} />
            </Field>
            <Field label={t("editor.reqTimeout")} className="flex-1">
              <input
                type="number"
                min={1000}
                step={1000}
                placeholder="30000"
                title={t("editor.reqTimeoutHint")}
                className={inputCls}
                value={timeoutMs}
                onChange={(e) => setTimeoutMs(e.target.value === "" ? "" : Number(e.target.value))}
              />
            </Field>
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
                        className="shrink-0 rounded bg-primary/10 px-1.5 py-0.5 text-[10px] font-medium text-primary"
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
                    {/* 这里**刻意没有「最大单次输出」输入框**（2026-08-22 按用户要求移除）。
                        它由后端 `resolve_max_output` 自动定：内置能力表（Claude 全族 + GLM /
                        DeepSeek / Kimi / Qwen / GPT / Grok …）→ Claude 按世代单调兜底 →
                        非 Claude 按家族兜底行 → 全局 8192。用户不需要知道这些数字。

                        为什么连「自动 128K」这种占位提示也不留：那仍然是一个**看起来要填**的
                        输入框，而它对用户不产出任何决策 —— 填错只会更糟（填大了上游 400、
                        填小了长回答被截断）。少一个能填错的地方，比多一句解释更有用。

                        **数据字段 `ModelInfo.maxOutputTokens` 保留不动**：
                        ① 已经填过值的老配置继续生效（`resolve_max_output` 仍优先用户值），
                           删字段等于替用户抹掉他调过的配置；
                        ② 万一某个私有模型名的自动值不对，导入/编辑配置文件仍是条出路。
                        即「不再展示」而非「不再支持」。 */}
                    <button
                      onClick={() => {
                        setModels(models.filter((x) => x.realName !== m.realName));
                        setInvalidContextModels((prev) => {
                          const next = new Set(prev);
                          // 删掉这一行的校验残项，否则会留下一个指向已删除模型的标记 ——
                          // 虽然 save 的比对会因模型已不在列表而放过它，但那是靠巧合，不该依赖。
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

          {/* 档位快捷映射（四档）+ 精确映射（真实名 → 对外名 · 显示名）。
              整块抽成 ModelMappingSection —— 本文件冻结在棘轮上、余量为 0，而这一区本轮要长。
              分类传 activeCategory 而不是 initial?.categoryId：与下面 buildDraftKey 里
              `activeCategory === "codex"` 那三行的口径保持一致，别让「显示与落盘」各按一套判。 */}
          <ModelMappingSection
            category={activeCategory}
            models={models}
            mappings={mappings}
            setMappings={setMappings}
            tiers={tiers}
            onTierChange={(which, v) => setTiers((p) => ({ ...p, [which]: v }))}
            probe={probeDesktopNames}
          />

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
                        // 留空 = 自动识别，故 placeholder 要说明这件事，
                        // 而不是给一个看起来「应该填这个」的示例路径。
                        placeholder={t("balance.urlAutoPlaceholder")}
                        onChange={(e) => {
                          // 用户手改地址即视为自定义：模板高亮跟着走，
                          // 否则会出现「高亮在通用模板、地址却不是它」的错配。
                          // 清空回退到 auto —— 与后端「见空即自动识别」一致。
                          const next = e.target.value;
                          setBalance((b) => ({
                            ...b,
                            url: next,
                            template: next.trim() === "" ? "auto" : "custom",
                          }));
                          setBalanceProbe(null);
                        }}
                      />
                      <p className="mt-1 text-[11px] leading-relaxed text-text-muted">
                        {t("balance.urlHint")}
                      </p>
                    </Field>

                    {/* 请求方法 + 认证方式 + 超时。
                        `method` 此前**没有 UI 入口**：后端 `balance.rs` 明确支持 GET/POST
                        （其余方法报「不支持的请求方法」），而 5 个预设模板全是 GET ——
                        于是需要 POST 的余额端点在界面上根本配不出来，用户只能看着
                        「自定义」模板却改不了方法。 */}
                    <div className="grid grid-cols-3 gap-2">
                      <Field label={t("balance.method")}>
                        <select
                          className={inputCls}
                          value={balance.method || "GET"}
                          onChange={(e) => {
                            // method 是模板定义字段（BALANCE_TEMPLATES 的三元组之一）：
                            // 手改即视为自定义、并清掉旧探测结果 —— 与 URL 输入框同一原则，
                            // 否则高亮停在「通用」而实际已是 POST，旧的成功结果框还挂着，
                            // 看起来像改后的配置已验证通过（实际验证的是改前的）。
                            setBalance((b) => ({ ...b, method: e.target.value, template: "custom" }));
                            setBalanceProbe(null);
                          }}
                        >
                          {/* 只列后端真正支持的两个：多列一个就是「填了报错」 */}
                          <option value="GET">GET</option>
                          <option value="POST">POST</option>
                        </select>
                      </Field>
                      <Field label={t("balance.auth")}>
                        <select
                          className={inputCls}
                          // auto 态下 balance.auth 是空串（后端据此走自动识别）。
                          // 空串映射到「自动」选项，否则 <select> 会渲染成空白/落到第一项，
                          // 让用户误以为认证方式是 Bearer（本轮把默认 auth 改空后引入的 bug）。
                          value={balance.auth || "auto"}
                          onChange={(e) => {
                            const v = e.target.value;
                            // 选「自动」= 清空 auth 回到自动识别；其余值是用户显式指定 → custom。
                            if (v === "auto") {
                              setBalance((b) => ({ ...b, auth: "", template: b.url.trim() === "" ? "auto" : "custom" }));
                            } else {
                              setBalance((b) => ({ ...b, auth: v, template: "custom" }));
                            }
                            setBalanceProbe(null);
                          }}
                        >
                          <option value="auto">{t("balance.authAuto")}</option>
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

                    {/* 自动查询间隔。这个输入框曾被删掉（当时没有任何定时任务读该字段，
                        摆着就是静默失效开关）；现在 KeyCard 的 usePolling 已按它起表，
                        必须加回来 —— 否则轮询功能反过来变成**不可达功能**：代码都在，
                        初值恒为 0（关闭），用户没有任何入口能打开它。 */}
                    <Field label={t("balance.interval")}>
                      <input
                        type="number"
                        min={0}
                        className={inputCls}
                        value={balance.autoIntervalMin}
                        onChange={(e) =>
                          setBalance((b) => ({
                            ...b,
                            autoIntervalMin: Math.max(0, Math.floor(Number(e.target.value) || 0)),
                          }))
                        }
                      />
                      <p className="mt-1 text-[11px] leading-relaxed text-text-muted">
                        {t("balance.intervalHint")}
                      </p>
                    </Field>
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
                        onChange={(e) => {
                          // 取值路径是探测取值验证的**对象本身**：改了它，旧探测结果就不再
                          // 代表现在的配置，必须清掉 —— 否则旧的成功框像在给改后的路径背书。
                          // （它不是模板定义字段，不置 custom。）
                          setBalance((b) => ({ ...b, remainingPath: e.target.value }));
                          setBalanceProbe(null);
                        }}
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
                <CostMultiplierField value={costMultiplier} onChange={setCostMultiplier} t={t} />
                <CustomHeadersField value={headersJson} onChange={setHeadersJson} t={t} />
              </div>
            )}
          </div>

        </div>

        {/* 底部操作 */}
        <div className="flex items-center justify-end gap-2 border-t border-border px-5 py-3">
          <Button variant="ghost" onClick={onClose} disabled={saving}>{t("common.cancel")}</Button>
          <Button onClick={save} disabled={saving}>{saving ? t("common.saving") : t("common.save")}</Button>
        </div>
      </div>

      {/* 保存错误走弹窗，不再贴在可滚动表单体的末尾（在视口外 = 「点了没反应」）。
          判据与实现见 SaveErrorDialog。 */}
      <SaveErrorDialog error={error} onClose={() => setError(null)} />

      <BrandPickerDialog
        open={iconPickerOpen}
        value={icon}
        onChange={setIcon}
        onClose={() => setIconPickerOpen(false)}
      />
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

  // 卸载时上报「有效」，清掉可能残留的校验失败标记。
  //
  // 立这条时的场景是「最大输出输入框仅 Anthropic 协议渲染」——那个框已按用户要求移除，
  // 但这条清理**仍然必要且更通用**：任何条件渲染（模型行被删、列表被拉取结果替换）都会让
  // 一个处于非法态的输入框卸载，而标记留在父级 → Save 被一句指向屏幕上已不存在的控件的
  // 报错挡住，成了无法自助解除的死胡同。故保留。
  // 用 ref 持最新回调、空依赖：**不能**把 onValidityChange 放进依赖数组 —— 它每次渲染
  // 都是新闭包，会让 cleanup 每次渲染都跑一遍、把非法态标记误清，反而放行非法值。
  const onValidityChangeRef = useRef(onValidityChange);
  onValidityChangeRef.current = onValidityChange;
  useEffect(() => () => onValidityChangeRef.current(true), []);

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
        className={`h-7 w-16 rounded-control border bg-background px-2 text-xs text-text-primary placeholder:text-text-muted ${
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
        className="h-7 rounded-control border border-border bg-background px-1 text-xs text-text-primary"
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
