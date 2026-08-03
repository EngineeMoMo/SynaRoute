import { useState } from "react";
import { api } from "@/lib/bridge";
import { useStore } from "@/store";
import { useT } from "@/lib/useT";
import { Button } from "@/components/ui/Button";
import { Combobox } from "@/components/ui/Combobox";
import { BrandIcon } from "@/components/BrandIcon";
import type { ModelInfo, ModelMapping, ProviderKey, Protocol } from "@/types";
import { X, RefreshCw, Plus, Trash2, ArrowRight, Eye, EyeOff, Zap, Gauge, Brain, Download } from "lucide-react";

interface KeyEditorProps {
  initial: ProviderKey | null; // null = 新增
  onClose: () => void;
}

/** 新增/编辑 Key 抽屉面板（FR-002/004/005/006） */
export function KeyEditor({ initial, onClose }: KeyEditorProps) {
  const activeCategory = useStore((s) => s.activeCategory);
  const loadCategory = useStore((s) => s.loadCategory);
  const vendors = useStore((s) => s.vendors);
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
  const [fetching, setFetching] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // 批量应用 Max Tokens 的状态与结果提示。成功走独立提示，不复用 error（那是红色告警样式）。
  const [applyingAll, setApplyingAll] = useState(false);
  const [applyAllMsg, setApplyAllMsg] = useState<string | null>(null);

  /**
   * 把当前输入框里的 Max Tokens 一次应用到本分类**全部** Key。
   *
   * 为什么要有：逐 Key 存是对的（各厂商上限不同），但漏改一个就会在故障转移落到它时按旧值
   * 截断回答 —— 表现为「同一个问题有时完整、有时被切断」，极难联想到是某个备用 Key 的参数。
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
        categoryId: activeCategory,
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

  // 当前厂商的内置预设模型（用于「一键导入」补全空列表）
  const presetModels = vendors.find((v) => v.id === vendor)?.presetModels ?? [];
  const importPresetModels = () => {
    const existing = new Set(models.map((m) => m.realName));
    const added: ModelInfo[] = presetModels
      .filter((p) => !existing.has(p.realName))
      .map((p) => ({ realName: p.realName, source: "manual", contextWindow: p.contextWindow }));
    if (added.length) setModels([...models, ...added]);
  };

  const save = async () => {
    if (!name.trim()) return setError(t("editor.errNeedName"));
    if (!baseUrl.trim()) return setError(t("editor.errNeedBaseUrl2"));

    const key: ProviderKey = {
      // 新建时留空，**由后端生成 uuid v4**（P3-5）。原先是 `k_${Date.now()}`：
      // id 被导入逻辑当作全局唯一标识做「同 id 即同一条 Key」的覆盖判据，而
      // 「两台机器照同一份教程配置」是真实场景，落在同一毫秒即撞号 → 跨机导入会把一条
      // 完全无关的本机 Key 静默覆盖成对方的配置。后端 upsert_key 会回填 id 并随返回值给回。
      id: initial?.id ?? "",
      categoryId: activeCategory,
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
    };

    setSaving(true);
    setError(null);
    try {
      // **必须用后端返回的 key**：新建时本地 id 是空串，真正的 uuid 由后端生成并回填。
      // 用本地那个空 id 去 saveSecret/checkHealth 会写到一条不存在的 Key 上（密钥成孤儿、
      // 探测无对象），而界面看起来一切正常——正是本项目最防的静默失效形态。
      const saved = await api.upsertKey(key);
      if (secret) await api.saveSecret(saved.id, secret);
      await loadCategory(activeCategory);
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
              <select className={inputCls} value={protocol} onChange={(e) => setProtocol(e.target.value as Protocol)}>
                <option value="anthropic">Anthropic</option>
                <option value="openai_chat">OpenAI Chat</option>
                <option value="openai_responses">OpenAI Responses</option>
              </select>
            </Field>
          </div>

          <Field label={t("editor.baseUrl")}>
            <input className={`${inputCls} font-mono`} value={baseUrl} onChange={(e) => setBaseUrl(e.target.value)} placeholder={t("editor.baseUrlPlaceholder")} />
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
            <Field label="Max Tokens" className="flex-1">
              <input type="number" className={inputCls} value={maxTokens} onChange={(e) => setMaxTokens(Number(e.target.value))} />
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

          {/* Max Tokens 批量应用：漏改一个 Key 会在故障转移落到它时按旧值截断回答，
              那种偶发性问题极难排查，故给一个「一次统一」的入口（含已停用的 Key）。 */}
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
                    <input
                      type="number"
                      className="h-7 w-24 rounded-control border border-border bg-bg px-2 text-xs text-text-primary placeholder:text-text-muted"
                      placeholder={t("editor.contextWindowPlaceholder")}
                      value={m.contextWindow ?? ""}
                      onChange={(e) => {
                        const val = e.target.value ? Number(e.target.value) : undefined;
                        setModels(models.map((x) => x.realName === m.realName ? { ...x, contextWindow: val } : x));
                      }}
                      title={t("editor.contextWindow")}
                    />
                    <button
                      onClick={() => setModels(models.filter((x) => x.realName !== m.realName))}
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
              {mappings.length === 0 && (
                <span className="text-xs text-text-muted">{t("editor.noMapping")}</span>
              )}
              {mappings.map((m, i) => (
                <div key={m.id} className="flex items-center gap-1.5">
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
                    className={`${inputCls} flex-1 font-mono`}
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
              ))}
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
