import { useEffect, useState } from "react";
import { api } from "@/lib/bridge";
import { useStore } from "@/store";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/Card";
import { Switch } from "@/components/ui/Switch";
import { Button } from "@/components/ui/Button";
import { Badge } from "@/components/ui/Badge";
import { HealthBadge } from "@/components/HealthBadge";
import { BrandIcon } from "@/components/BrandIcon";
import { useT } from "@/lib/useT";
import type { TFunc } from "@/lib/i18n";
import type { AggregateMode, BrainConfig, CategoryType, CodegraphState, ProviderKey } from "@/types";
import type { RecentWorkdir } from "@/types";
import { Brain, Info, Save, Plus, X, FolderOpen, Play, CheckCircle, History, Activity, ChevronDown, Wand2, Plug } from "lucide-react";

const CATEGORIES: { value: CategoryType; tKey: string }[] = [
  { value: "claude-cli", tKey: "nav.claude-cli" },
  { value: "claude-desktop", tKey: "nav.claude-desktop" },
  { value: "codex", tKey: "nav.codex" },
];

/** 大脑聚合配置页（FR-013 ~ FR-017）——按端分类独立配置 */
export function BrainPage() {
  const t = useT();
  const showToast = useStore((s) => s.showToast);
  const vendors = useStore((s) => s.vendors);
  const loadVendors = useStore((s) => s.loadVendors);
  // 本页独立维护分类，不跟随侧栏，使三端聚合配置互相隔离
  const [category, setCategory] = useState<CategoryType>("claude-cli");
  const [keys, setKeys] = useState<ProviderKey[]>([]);
  const [config, setConfig] = useState<BrainConfig | null>(null);
  const [saved, setSaved] = useState(false);

  const [checkingId, setCheckingId] = useState<string | null>(null);
  // 本分类是否已把 synaroute MCP 写进对应客户端配置（Codex config.toml / CLI ~/.claude.json）。
  // 与聚合配置分开：聚合配好只是「有内容」，还得接入客户端，客户端才能调 synaroute_ai。
  const [mcpRegistered, setMcpRegistered] = useState<boolean | null>(null);
  const [mcpBusy, setMcpBusy] = useState(false);

  // 自定义厂商图标查找（成员行的品牌图标用）
  const vendorIcon = (vid?: string) => vendors.find((v) => v.id === vid)?.icon;

  useEffect(() => {
    void loadVendors();
  }, [loadVendors]);

  useEffect(() => {
    setConfig(null);
    setSaved(false);
    setMcpRegistered(null);
    void Promise.all([api.getBrainConfig(category), api.listKeys(category), api.getToolConfigPreview(category)]).then(
      ([cfg, ks, preview]) => {
        setConfig(cfg);
        setKeys(ks);
        setMcpRegistered(preview.mcpRegistered);
      }
    );
  }, [category]);

  // 一键接入/断开：把本分类的 synaroute MCP 写入（或移出）对应客户端配置。
  // 免去用户绕到「配置预览」弹窗底部——配好聚合的地方直接接入。
  const toggleMcp = async () => {
    setMcpBusy(true);
    try {
      if (mcpRegistered) {
        await api.unregisterMcpForCategory(category);
        setMcpRegistered(false);
      } else {
        await api.registerMcpForCategory(category);
        setMcpRegistered(true);
      }
    } catch (e) {
      showToast("error", String((e as Error)?.message ?? e));
    } finally {
      setMcpBusy(false);
    }
  };

  // 对单个 Key 触发健康检测并就地刷新（本页维护独立 keys 状态，不走全局 store）
  const checkKeyHealth = async (keyId: string) => {
    setCheckingId(keyId);
    setKeys((ks) => ks.map((k) => (k.id === keyId ? { ...k, health: { ...k.health, status: "checking" } } : k)));
    try {
      await api.checkHealth(keyId);
    } finally {
      const fresh = await api.listKeys(category);
      setKeys(fresh);
      setCheckingId(null);
    }
  };

  const update = (patch: Partial<BrainConfig>) => {
    if (!config) return;
    setConfig({ ...config, ...patch });
    setSaved(false);
  };

  const save = async () => {
    if (!config) return;
    // 落盘失败必须可见（硬规则），否则用户以为已保存、重启后配置丢失。
    try {
      await api.saveBrainConfig(config);
      setSaved(true);
    } catch (e) {
      console.error("saveBrainConfig failed", e);
      showToast("error", String((e as Error)?.message ?? e));
    }
  };

  const memberChecked = (keyId: string, model: string) =>
    !!config?.members.some((m) => m.keyId === keyId && m.modelName === model);

  const addMember = (keyId: string, model: string) => {
    if (!config || !model.trim()) return;
    if (memberChecked(keyId, model)) return;
    update({
      members: [...config.members, { id: `bm_${keyId}_${model}`, keyId, modelName: model.trim() }],
    });
  };

  const removeMember = (keyId: string, model: string) => {
    if (!config) return;
    update({ members: config.members.filter((m) => !(m.keyId === keyId && m.modelName === model)) });
  };

  // 一键快速配置：把当前分类所有启用 Key 各取首个模型加为成员，并自动选第一个作决策者。
  // 解决空白配置无从下手的门槛——用户点一下即得可用配置，再按需微调。
  // 已有配置时不覆盖决策者/成员，只补齐缺的（幂等、不破坏用户已调的选择）。
  const quickFill = () => {
    if (!config) return;
    // 候选：启用且有已知模型的 Key（没拉到模型的没法自动选，跳过）。
    const usable = keys.filter((k) => k.enabled && k.models.length > 0);
    if (usable.length === 0) {
      showToast("error", t("brain.quickFillNoKeys"));
      return;
    }
    // 成员：每个可用 Key 首个模型，跳过已在成员里的（幂等）。
    const existing = new Set(config.members.map((m) => `${m.keyId}::${m.modelName}`));
    const added = [...config.members];
    for (const k of usable) {
      const model = k.models[0].realName;
      const ref = `${k.id}::${model}`;
      if (!existing.has(ref)) {
        added.push({ id: `bm_${k.id}_${model}`, keyId: k.id, modelName: model });
        existing.add(ref);
      }
    }
    // 决策者：未选则默认第一个可用 Key 的首个模型。
    const decider = config.deciderRef || `${usable[0].id}::${usable[0].models[0].realName}`;
    update({ members: added, deciderRef: decider, enabled: true });
    showToast("success", t("brain.quickFillDone", { n: added.length }));
  };

  // 就地改成员模型（无需删了重加）。避免与同 Key 下已有成员重复。
  const updateMemberModel = (memberId: string, newModel: string) => {
    if (!config || !newModel.trim()) return;
    const target = config.members.find((m) => m.id === memberId);
    if (!target) return;
    if (config.members.some((m) => m.id !== memberId && m.keyId === target.keyId && m.modelName === newModel)) return;
    update({
      members: config.members.map((m) => (m.id === memberId ? { ...m, modelName: newModel.trim() } : m)),
    });
  };

  if (!config) {
    return (
      <div className="h-full overflow-y-auto">
        <BrainHeader category={category} setCategory={setCategory} enabled={false} />
        <div className="p-6 text-sm text-text-muted">{t("common.loading")}</div>
      </div>
    );
  }

  return (
    <div className="h-full overflow-y-auto">
      <BrainHeader category={category} setCategory={setCategory} enabled={config.enabled} />

      <div className="space-y-4 p-6">
        {/* 总开关 */}
        <Card>
          <CardContent className="flex items-center justify-between pt-4">
            <div>
              <div className="text-sm font-medium text-text-primary">{t("brain.enableTitle")}</div>
              <div className="text-xs text-text-muted">{t("brain.enableDesc")}</div>
            </div>
            <Switch checked={config.enabled} onCheckedChange={(v) => update({ enabled: v })} />
          </CardContent>
        </Card>

        {/* 接入到客户端：配好聚合后一键把 synaroute MCP 写进对应客户端配置（本分类），
            免去绕到「配置预览」弹窗底部。这是「客户端能不能调 synaroute_ai」的关键一步。 */}
        <Card>
          <CardContent className="flex items-center justify-between pt-4">
            <div className="min-w-0 flex-1">
              <div className="flex items-center gap-2 text-sm font-medium text-text-primary">
                {t("brain.mcpConnectTitle")}
                {mcpRegistered != null && (
                  <Badge variant={mcpRegistered ? "success" : "neutral"}>
                    {mcpRegistered ? t("brain.mcpConnected") : t("brain.mcpNotConnected")}
                  </Badge>
                )}
              </div>
              <div className="mt-0.5 text-xs text-text-muted">
                {t("brain.mcpConnectDesc", { client: t(`nav.${category}`) })}
              </div>
            </div>
            <Button
              size="sm"
              variant={mcpRegistered ? "outline" : "primary"}
              disabled={mcpBusy || mcpRegistered == null}
              onClick={() => void toggleMcp()}
            >
              <Plug size={14} />
              {mcpBusy
                ? t("common.loading")
                : mcpRegistered
                  ? t("brain.mcpDisconnect")
                  : t("brain.mcpConnect")}
            </Button>
          </CardContent>
        </Card>

        {/* 成员选择（FR-014）——先选 Key 再二级选模型，不默认带入所有 Key */}
        <Card>
          <CardHeader className="flex flex-row items-center justify-between space-y-0">
            <CardTitle>{t("brain.membersTitle")}</CardTitle>
            {/* 一键快速配置：新用户免去逐个手选，点一下自动填成员 + 决策者 */}
            <Button size="sm" variant="secondary" onClick={quickFill} title={t("brain.quickFillHint")}>
              <Wand2 size={14} /> {t("brain.quickFill")}
            </Button>
          </CardHeader>
          <CardContent className="space-y-3">
            {keys.length === 0 ? (
              <div className="text-xs text-text-muted">{t("brain.noKeys", { category: t(`nav.${category}`) })}</div>
            ) : (
              <>
                {/* 已选成员列表 */}
                {config.members.length === 0 ? (
                  <div className="text-xs text-text-muted">{t("brain.noMembers")}</div>
                ) : (
                  <div className="space-y-1.5">
                    {config.members.map((m) => {
                      const k = keys.find((x) => x.id === m.keyId);
                      return (
                        <div
                          key={m.id}
                          className="flex items-center gap-2 rounded-control border border-border bg-surface px-2.5 py-2"
                        >
                          <BrandIcon hint={k?.vendor ?? m.modelName} fallbackLabel={k?.name} iconUrl={vendorIcon(k?.vendor)} size={20} />
                          <div className="min-w-0 flex-1">
                            <div className="truncate text-xs font-medium text-text-primary">{k?.name ?? m.keyId}</div>
                            {k && k.models.length > 0 ? (
                              // 就地改模型：下拉选该 Key 的模型（含当前值，若是手动加的也保底可见）
                              <select
                                value={m.modelName}
                                onChange={(e) => updateMemberModel(m.id, e.target.value)}
                                className="mt-0.5 max-w-full rounded-control border border-border bg-surface px-1.5 py-0.5 font-mono text-[11px] text-text-secondary focus:outline-none focus:ring-1 focus:ring-ring"
                              >
                                {!k.models.some((mm) => mm.realName === m.modelName) && (
                                  <option value={m.modelName}>{m.modelName}</option>
                                )}
                                {k.models.map((mm) => (
                                  <option key={mm.realName} value={mm.realName}>{mm.realName}</option>
                                ))}
                              </select>
                            ) : (
                              <div className="truncate font-mono text-[11px] text-text-muted">{m.modelName}</div>
                            )}
                          </div>
                          {k && <HealthBadge health={k.health} />}
                          <button
                            onClick={() => removeMember(m.keyId, m.modelName)}
                            className="shrink-0 rounded-control p-1 text-text-muted hover:text-danger"
                            title={t("common.remove")}
                          >
                            <X size={14} />
                          </button>
                        </div>
                      );
                    })}
                  </div>
                )}
                {/* 添加成员：先选 Key 再选模型 */}
                <MemberPicker
                  t={t}
                  keys={keys}
                  checkingId={checkingId}
                  onCheckHealth={checkKeyHealth}
                  isChecked={memberChecked}
                  onAdd={addMember}
                />
              </>
            )}
          </CardContent>
        </Card>

        {/* 决策者 + 汇总模型 */}
        <Card>
          <CardHeader>
            <CardTitle>{t("brain.decisionTitle")}</CardTitle>
          </CardHeader>
          <CardContent className="space-y-3">
            <KeyModelSelect
              label={t("brain.decider")}
              hint={t("brain.deciderHint")}
              t={t}
              keys={keys}
              value={config.deciderRef ?? ""}
              onChange={(v) => update({ deciderRef: v || undefined })}
            />
            <KeyModelSelect
              label={t("brain.summarizer")}
              hint={t("brain.summarizerHint")}
              t={t}
              keys={keys}
              value={config.summarizerRef ?? ""}
              emptyLabel={t("brain.summarizerReuse")}
              onChange={(v) => update({ summarizerRef: v || undefined })}
            />
          </CardContent>
        </Card>

        {/* 聚合方式 + 并发 + 超时（FR-017 / CON-4） */}
        <Card>
          <CardHeader>
            <CardTitle>{t("brain.strategyTitle")}</CardTitle>
          </CardHeader>
          <CardContent className="space-y-3">
            <div>
              <div className="mb-1.5 text-xs font-medium text-text-secondary">{t("brain.aggregateMode")}</div>
              <div className="flex gap-2">
                <ModeOption
                  active={config.aggregateMode === "compressed"}
                  title={t("brain.modeCompressedTitle")}
                  desc={t("brain.modeCompressedDesc")}
                  onClick={() => update({ aggregateMode: "compressed" as AggregateMode })}
                />
                <ModeOption
                  active={config.aggregateMode === "full"}
                  title={t("brain.modeFullTitle")}
                  desc={t("brain.modeFullDesc")}
                  onClick={() => update({ aggregateMode: "full" as AggregateMode })}
                />
              </div>
            </div>

            <div className="flex gap-4">
              <NumberField
                label={t("brain.concurrency")}
                value={config.concurrencyLimit}
                min={1}
                max={10}
                onChange={(v) => update({ concurrencyLimit: v })}
              />
              <NumberField
                label={t("brain.totalTimeout")}
                value={config.totalTimeoutMs}
                min={5000}
                max={1800000}
                step={5000}
                onChange={(v) => update({ totalTimeoutMs: v })}
              />
            </div>
            <p className="mt-2 text-xs text-text-secondary">{t("brain.totalTimeoutHint")}</p>
          </CardContent>
        </Card>

        {/* 文件检索配置 */}
        <Card>
          <CardHeader>
            <CardTitle>{t("brain.retrievalTitle")}</CardTitle>
          </CardHeader>
          <CardContent className="space-y-3">
            <div className="flex items-center justify-between">
              <div>
                <div className="text-sm font-medium text-text-primary">{t("brain.retrievalTitle")}</div>
                <div className="text-xs text-text-muted">{t("brain.retrievalDesc")}</div>
              </div>
              <Switch
                checked={config.retrievalEnabled}
                onCheckedChange={(v) => update({ retrievalEnabled: v })}
              />
            </div>
            {config.retrievalEnabled && (
              <>
                <div className="flex items-center justify-between border-t border-border pt-3">
                  <div>
                    <div className="text-sm font-medium text-text-primary">{t("brain.autoFollowTitle")}</div>
                    <div className="text-xs text-text-muted">{t("brain.autoFollowDesc")}</div>
                  </div>
                  <Switch
                    checked={config.autoFollowActive ?? false}
                    onCheckedChange={(v) => update({ autoFollowActive: v })}
                  />
                </div>

                {config.autoFollowActive ? (
                  <ActiveProjectDisplay t={t} />
                ) : (
                  <div>
                    <div className="mb-1 flex items-center justify-between">
                      <span className="text-xs font-medium text-text-secondary">{t("brain.workDir")}</span>
                      <RecentWorkdirsMenu t={t} onPick={(dir) => update({ workDir: dir })} />
                    </div>
                    <div className="flex gap-2">
                      <input
                        type="text"
                        className="flex-1 rounded-control border border-border bg-bg px-2.5 py-1.5 font-mono text-xs text-text-primary placeholder:text-text-muted"
                        value={config.workDir ?? ""}
                        placeholder={t("brain.workDirPlaceholder")}
                        onChange={(e) => update({ workDir: e.target.value || undefined })}
                      />
                      <Button
                        size="sm"
                        variant="secondary"
                        onClick={async () => {
                          const dir = await api.pickDirectory();
                          if (dir) update({ workDir: dir });
                        }}
                      >
                        <FolderOpen size={13} /> {t("settings.logBrowse")}
                      </Button>
                    </div>
                  </div>
                )}

                <NumberField
                  label={t("brain.maxContextTokens")}
                  value={config.maxContextTokens ?? 50000}
                  min={5000}
                  max={500000}
                  step={5000}
                  onChange={(v) => update({ maxContextTokens: v })}
                />

                <CodegraphPanel t={t} workDir={config.workDir} autoFollow={config.autoFollowActive ?? false} />
              </>
            )}
          </CardContent>
        </Card>

        <div className="flex items-center gap-2 rounded-control bg-info/8 px-3 py-2 text-xs text-info">
          <Info size={14} />
          {t("brain.textOnlyNote")}
        </div>

        <div className="flex items-center gap-3">
          <Button onClick={save}>
            <Save size={16} /> {t("brain.saveConfig")}
          </Button>
          {saved && <span className="text-xs text-success">{t("common.saved")}</span>}
          {config.members.length === 0 && (
            <span className="text-xs text-warning">{t("brain.noMembers")}</span>
          )}
          {!config.deciderRef && (
            <span className="text-xs text-warning">{t("brain.noDecider")}</span>
          )}
        </div>

        {/* 运行聚合面板 */}
        {config.enabled && config.deciderRef && (
          <AggregateRunner category={category} />
        )}
      </div>
    </div>
  );
}

/** 运行聚合交互面板：输入 prompt → 参与者思考 → 决策者输出计划 → 确认执行 */
function AggregateRunner({ category }: { category: CategoryType }) {
  const t = useT();
  const [prompt, setPrompt] = useState("");
  const [phase, setPhase] = useState<"idle" | "planning" | "planned" | "executing" | "done">("idle");
  const [plan, setPlan] = useState("");
  // Phase1 定下的工作目录，Phase2 原样回传（避免 auto-follow 期间目录漂移写错项目）
  const [planWorkDir, setPlanWorkDir] = useState<string | undefined>(undefined);
  const [result, setResult] = useState("");
  const [error, setError] = useState<string | null>(null);

  const runPlan = async () => {
    if (!prompt.trim()) return;
    setPhase("planning");
    setError(null);
    setPlan("");
    setResult("");
    try {
      const res = await api.runAggregatePlan(category, prompt);
      if (res.resultType === "plan") {
        setPlan(res.content);
        setPlanWorkDir(res.workDir);
        setPhase("planned");
      } else {
        setResult(res.content);
        setPhase("done");
      }
    } catch (e) {
      setError(String((e as Error)?.message ?? e));
      setPhase("idle");
    }
  };

  const runExecute = async () => {
    setPhase("executing");
    setError(null);
    try {
      const res = await api.runAggregateExecute(category, prompt, plan, planWorkDir);
      setResult(res.content);
      setPhase("done");
    } catch (e) {
      setError(String((e as Error)?.message ?? e));
      setPhase("planned");
    }
  };

  const reset = () => {
    setPhase("idle");
    setPlan("");
    setPlanWorkDir(undefined);
    setResult("");
    setError(null);
  };

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t("brain.runTitle")}</CardTitle>
      </CardHeader>
      <CardContent className="space-y-3">
        {/* Prompt 输入 */}
        <textarea
          className="w-full rounded-control border border-border bg-bg px-3 py-2 text-sm text-text-primary placeholder:text-text-muted focus:outline-none focus:ring-2 focus:ring-ring"
          rows={3}
          placeholder={t("brain.runPlaceholder")}
          value={prompt}
          onChange={(e) => setPrompt(e.target.value)}
          disabled={phase !== "idle"}
        />

        {/* 操作按钮 */}
        <div className="flex gap-2">
          {phase === "idle" && (
            <Button size="sm" onClick={() => void runPlan()} disabled={!prompt.trim()}>
              <Play size={14} /> {t("brain.runStart")}
            </Button>
          )}
          {phase === "planning" && (
            <span className="text-xs text-text-muted">{t("brain.runThinking")}</span>
          )}
          {phase === "planned" && (
            <>
              <Button size="sm" onClick={() => void runExecute()}>
                <CheckCircle size={14} /> {t("brain.runConfirm")}
              </Button>
              <Button size="sm" variant="outline" onClick={reset}>
                {t("common.cancel")}
              </Button>
            </>
          )}
          {phase === "executing" && (
            <span className="text-xs text-text-muted">{t("brain.runExecuting")}</span>
          )}
          {phase === "done" && (
            <Button size="sm" variant="secondary" onClick={reset}>
              {t("brain.runReset")}
            </Button>
          )}
        </div>

        {/* 错误 */}
        {error && <div className="text-xs text-danger">{error}</div>}

        {/* 计划展示 */}
        {plan && (
          <div>
            <div className="mb-1 text-xs font-medium text-text-secondary">{t("brain.runPlanTitle")}</div>
            <pre className="max-h-64 overflow-auto rounded-control border border-border bg-bg p-3 font-mono text-xs leading-relaxed text-text-primary">
              {plan}
            </pre>
          </div>
        )}

        {/* 执行结果 */}
        {result && phase === "done" && (
          <div>
            <div className="mb-1 text-xs font-medium text-success">{t("brain.runResultTitle")}</div>
            <pre className="max-h-64 overflow-auto rounded-control border border-success/30 bg-success/5 p-3 font-mono text-xs leading-relaxed text-text-primary">
              {result}
            </pre>
          </div>
        )}
      </CardContent>
    </Card>
  );
}

/** 页头：标题 + 分类切换 */
function BrainHeader({
  category,
  setCategory,
  enabled,
}: {
  category: CategoryType;
  setCategory: (c: CategoryType) => void;
  enabled: boolean;
}) {
  const t = useT();
  return (
    <div className="border-b border-border px-6 py-4">
      <div className="flex items-center gap-2">
        <Brain size={20} className="text-primary" />
        <h1 className="text-lg font-semibold text-text-primary">{t("brain.title")}</h1>
        <Badge variant={enabled ? "success" : "neutral"}>{enabled ? t("brain.enabled") : t("brain.disabled")}</Badge>
      </div>
      <p className="mt-1 text-xs text-text-muted">{t("brain.subtitle")}</p>
      {/* 端分类切换：三端聚合配置互相隔离 */}
      <div className="mt-3 flex gap-1.5">
        {CATEGORIES.map((c) => (
          <button
            key={c.value}
            onClick={() => setCategory(c.value)}
            className={`rounded-control border px-3 py-1 text-xs transition-colors ${
              category === c.value
                ? "border-primary bg-primary/12 text-primary"
                : "border-border text-text-secondary hover:bg-surface-hover"
            }`}
          >
            {t(c.tKey)}
          </button>
        ))}
      </div>
    </div>
  );
}

/** 单个 Key 的成员选择行：已拉取模型作可选 chip + 手动输入模型名参与 */
/** 成员添加器：先选 Key（可就地健康检测），再选该 Key 下的模型加入聚合 */
function MemberPicker({
  t,
  keys,
  checkingId,
  onCheckHealth,
  isChecked,
  onAdd,
}: {
  t: TFunc;
  keys: ProviderKey[];
  checkingId: string | null;
  onCheckHealth: (keyId: string) => void;
  isChecked: (keyId: string, model: string) => boolean;
  onAdd: (keyId: string, model: string) => void;
}) {
  const [selKey, setSelKey] = useState("");
  const [manual, setManual] = useState("");
  const k = keys.find((x) => x.id === selKey);

  return (
    <div className="space-y-2 rounded-control border border-dashed border-border p-2.5">
      <div className="text-xs font-medium text-text-secondary">{t("brain.addMember")}</div>
      {/* 第一级：选 Key */}
      <div className="flex items-center gap-2">
        <div className="relative flex-1">
          <select
            value={selKey}
            onChange={(e) => setSelKey(e.target.value)}
            className="h-9 w-full appearance-none rounded-control border border-border bg-surface pl-3 pr-8 text-sm text-text-primary focus:outline-none focus:ring-2 focus:ring-ring"
          >
            <option value="">{t("brain.pickKeyPlaceholder")}</option>
            {keys.map((key) => (
              <option key={key.id} value={key.id}>
                {key.name}
              </option>
            ))}
          </select>
          <ChevronDown size={14} className="pointer-events-none absolute right-2.5 top-1/2 -translate-y-1/2 text-text-muted" />
        </div>
        {k && (
          <>
            <HealthBadge health={k.health} />
            <Button
              size="sm"
              variant="secondary"
              disabled={checkingId === k.id}
              onClick={() => onCheckHealth(k.id)}
              title={t("brain.checkKeyHealth")}
            >
              <Activity size={13} className={checkingId === k.id ? "animate-pulse" : ""} />
              {t("brain.checkKeyHealth")}
            </Button>
          </>
        )}
      </div>

      {/* 第二级：选该 Key 下的模型 */}
      {k && (
        <>
          {k.models.length > 0 ? (
            <div className="flex flex-wrap gap-1.5">
              {k.models.map((m) => {
                const added = isChecked(k.id, m.realName);
                return (
                  <button
                    key={m.realName}
                    disabled={added}
                    onClick={() => onAdd(k.id, m.realName)}
                    className={`inline-flex items-center gap-1 rounded-control border px-2 py-1 text-xs transition-colors ${
                      added
                        ? "cursor-default border-border bg-surface-hover text-text-muted"
                        : "border-border text-text-secondary hover:border-primary hover:bg-primary/10 hover:text-primary"
                    }`}
                  >
                    {added ? <CheckCircle size={11} /> : <Plus size={11} />}
                    <span className="font-mono">{m.realName}</span>
                  </button>
                );
              })}
            </div>
          ) : (
            <div className="text-[11px] text-text-muted">{t("brain.keyNoModels")}</div>
          )}
          {/* 手动输入模型名（该 Key 未拉取模型时也能加入聚合） */}
          <div className="flex gap-1.5">
            <input
              className="h-8 flex-1 rounded-control border border-border bg-surface px-2 font-mono text-xs text-text-primary focus:outline-none focus:ring-2 focus:ring-ring"
              placeholder={t("brain.memberManualPlaceholder")}
              value={manual}
              onChange={(e) => setManual(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && manual.trim()) {
                  e.preventDefault();
                  onAdd(k.id, manual);
                  setManual("");
                }
              }}
            />
            <Button
              size="sm"
              variant="secondary"
              disabled={!manual.trim()}
              onClick={() => {
                onAdd(k.id, manual);
                setManual("");
              }}
            >
              <Plus size={13} /> {t("brain.join")}
            </Button>
          </div>
        </>
      )}
    </div>
  );
}

/** 决策者/汇总者二级选择：先选 Key，再选模型，产出 `keyId::model` 引用 */
function KeyModelSelect({
  label,
  hint,
  t,
  keys,
  value,
  emptyLabel,
  onChange,
}: {
  label: string;
  hint?: string;
  t: TFunc;
  keys: ProviderKey[];
  value: string;
  emptyLabel?: string;
  onChange: (v: string) => void;
}) {
  // value 形如 `keyId::model`；拆出当前选中的 Key 与模型
  const [selKeyId, selModel] = value ? splitRef(value) : ["", ""];
  const k = keys.find((x) => x.id === selKeyId);

  const setKey = (keyId: string) => {
    if (!keyId) return onChange("");
    // 切换 Key 后默认选中其第一个模型（若有），否则留空待选
    const first = keys.find((x) => x.id === keyId)?.models[0]?.realName;
    onChange(first ? `${keyId}::${first}` : "");
  };
  const setModel = (model: string) => {
    if (!selKeyId || !model) return;
    onChange(`${selKeyId}::${model}`);
  };

  return (
    <div>
      <div className="mb-1 text-xs font-medium text-text-secondary">{label}</div>
      <div className="flex gap-2">
        <div className="relative flex-1">
          <select
            value={selKeyId}
            onChange={(e) => setKey(e.target.value)}
            className="h-9 w-full appearance-none rounded-control border border-border bg-surface pl-3 pr-8 text-sm text-text-primary focus:outline-none focus:ring-2 focus:ring-ring"
          >
            <option value="">{emptyLabel ?? t("brain.pickKeyPlaceholder")}</option>
            {keys.map((key) => (
              <option key={key.id} value={key.id}>
                {key.name}
              </option>
            ))}
          </select>
          <ChevronDown size={14} className="pointer-events-none absolute right-2.5 top-1/2 -translate-y-1/2 text-text-muted" />
        </div>
        {k && (
          <div className="relative flex-1">
            <select
              value={selModel}
              onChange={(e) => setModel(e.target.value)}
              className="h-9 w-full appearance-none rounded-control border border-border bg-surface pl-3 pr-8 font-mono text-sm text-text-primary focus:outline-none focus:ring-2 focus:ring-ring"
            >
              {k.models.length === 0 && <option value="">{t("brain.keyNoModels")}</option>}
              {selModel && !k.models.some((m) => m.realName === selModel) && (
                <option value={selModel}>{selModel}</option>
              )}
              {k.models.map((m) => (
                <option key={m.realName} value={m.realName}>
                  {m.realName}
                </option>
              ))}
            </select>
            <ChevronDown size={14} className="pointer-events-none absolute right-2.5 top-1/2 -translate-y-1/2 text-text-muted" />
          </div>
        )}
      </div>
      {hint && <div className="mt-0.5 text-[11px] text-text-muted">{hint}</div>}
    </div>
  );
}

/** 拆分 `keyId::model` 引用（model 名本身不含 :: ） */
function splitRef(ref: string): [string, string] {
  const idx = ref.indexOf("::");
  if (idx < 0) return [ref, ""];
  return [ref.slice(0, idx), ref.slice(idx + 2)];
}

function ModeOption({
  active,
  title,
  desc,
  onClick,
}: {
  active: boolean;
  title: string;
  desc: string;
  onClick: () => void;
}) {
  return (
    <button
      onClick={onClick}
      className={`flex-1 rounded-control border p-3 text-left transition-colors ${
        active ? "border-primary bg-primary/8" : "border-border hover:bg-surface-hover"
      }`}
    >
      <div className={`text-sm font-medium ${active ? "text-primary" : "text-text-primary"}`}>
        {title}
      </div>
      <div className="mt-0.5 text-[11px] text-text-muted">{desc}</div>
    </button>
  );
}

/** 显示当前检测到的活跃项目（每 5s 刷新）。auto_follow 模式下取代 workDir 字段。 */
function ActiveProjectDisplay({ t }: { t: TFunc }) {
  const [active, setActive] = useState<RecentWorkdir | null>(null);

  useEffect(() => {
    const fetch = async () => {
      try {
        const list = await api.detectRecentWorkdirs();
        setActive(list[0] ?? null);
      } catch {
        setActive(null);
      }
    };
    void fetch();
    const id = setInterval(() => void fetch(), 5000);
    return () => clearInterval(id);
  }, []);

  const formatAge = (ts?: number) => {
    if (!ts) return "";
    const diff = Date.now() - ts;
    if (diff < 60_000) return t("brain.activeJustNow");
    if (diff < 3600_000) return t("brain.activeMinutesAgo", { n: Math.floor(diff / 60_000) });
    if (diff < 86400_000) return t("brain.activeHoursAgo", { n: Math.floor(diff / 3600_000) });
    return t("brain.activeDaysAgo", { n: Math.floor(diff / 86400_000) });
  };

  return (
    <div className="rounded-control border border-primary/30 bg-primary/8 p-2.5">
      <div className="flex items-center gap-2">
        <span className="relative flex h-2 w-2">
          <span className="absolute inline-flex h-full w-full animate-ping rounded-full bg-primary opacity-75" />
          <span className="relative inline-flex h-2 w-2 rounded-full bg-primary" />
        </span>
        <span className="text-xs font-medium text-primary">{t("brain.activeProject")}</span>
      </div>
      {active ? (
        <>
          <div className="mt-1 truncate font-mono text-xs text-text-primary" title={active.path}>
            {active.path}
          </div>
          <div className="mt-0.5 text-[11px] text-text-muted">
            {active.source} · {formatAge(active.lastUsed)}
          </div>
        </>
      ) : (
        <div className="mt-1 text-xs text-text-muted">{t("brain.noActiveProject")}</div>
      )}
    </div>
  );
}

/**
 * codegraph 可用状态面板 —— 装了就让检索走「符号 + 调用链」精确切片，没装则退化为按文件检索。
 *
 * 刻意**不做自动安装**：往用户机器装第三方 CLI 属于要明确确认的动作，这里只给出命令让用户自己跑，
 * 装完点「重新检测」。建索引是本地操作（在项目里生成 .codegraph/），故提供一键按钮。
 *
 * 自动跟随开启时工作目录由运行时决定（读会话历史），此处拿不到具体路径，
 * 故只判可执行是否就绪、不判项目索引——避免显示一个与实际检索目录无关的索引状态。
 */
function CodegraphPanel({
  t,
  workDir,
  autoFollow,
}: {
  t: TFunc;
  workDir?: string;
  autoFollow: boolean;
}) {
  const [state, setState] = useState<CodegraphState | null>(null);
  const [checking, setChecking] = useState(false);
  const [building, setBuilding] = useState(false);
  const [msg, setMsg] = useState<{ kind: "ok" | "err"; text: string } | null>(null);

  // 自动跟随时不传目录：索引状态无从判定（运行时才知道用哪个项目）
  const effectiveDir = autoFollow ? undefined : workDir?.trim() || undefined;

  const check = async () => {
    setChecking(true);
    try {
      setState(await api.detectCodegraph(effectiveDir));
    } catch (e) {
      setMsg({ kind: "err", text: String((e as Error)?.message ?? e) });
    } finally {
      setChecking(false);
    }
  };

  // 目录变化时重新检测（换项目后索引状态会变）
  useEffect(() => {
    void check();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [effectiveDir]);

  const build = async () => {
    if (!effectiveDir) return;
    setBuilding(true);
    setMsg(null);
    try {
      const summary = await api.codegraphInit(effectiveDir);
      setMsg({ kind: "ok", text: summary });
      await check();
    } catch (e) {
      setMsg({ kind: "err", text: String((e as Error)?.message ?? e) });
    } finally {
      setBuilding(false);
    }
  };

  const s = state?.state;
  const desc =
    s === "ready"
      ? t("brain.cgDescReady")
      : s === "notIndexed"
        ? t("brain.cgDescNotIndexed")
        : s === "stranded"
          ? t("brain.cgDescStranded")
          : t("brain.cgDescNotInstalled");

  return (
    <div className="space-y-2 border-t border-border pt-3">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <div className="flex items-center gap-2">
            <span className="text-sm font-medium text-text-primary">{t("brain.cgTitle")}</span>
            {s === "ready" && (
              <Badge variant="success">
                <CheckCircle size={10} /> {state && "version" in state ? `v${state.version}` : ""}
              </Badge>
            )}
            {s === "notIndexed" && <Badge variant="warning">{t("brain.cgBadgeNotIndexed")}</Badge>}
            {s === "stranded" && <Badge variant="warning">PATH</Badge>}
            {s === "notInstalled" && <Badge variant="neutral">{t("brain.cgBadgeNotInstalled")}</Badge>}
          </div>
          <div className="mt-1 text-xs text-text-muted">{desc}</div>
        </div>
        <Button size="sm" variant="secondary" onClick={() => void check()} disabled={checking}>
          <Activity size={13} /> {t("brain.cgRecheck")}
        </Button>
      </div>

      {/* 未安装 / 孤岛：给命令让用户自己装，不代劳 */}
      {(s === "notInstalled" || s === "stranded") && (
        <div className="space-y-1">
          <div className="text-xs text-text-secondary">{t("brain.cgInstallHint")}</div>
          <code className="block select-all rounded-control border border-border bg-surface px-2.5 py-1.5 font-mono text-xs text-text-primary">
            npm i -g @colbymchenry/codegraph
          </code>
        </div>
      )}

      {/* 已安装但项目未索引：一键建索引（纯本地） */}
      {s === "notIndexed" && (
        <div className="flex items-center gap-2">
          <Button size="sm" onClick={() => void build()} disabled={building || !effectiveDir}>
            <Wand2 size={13} /> {building ? t("brain.cgBuilding") : t("brain.cgBuildIndex")}
          </Button>
          {!effectiveDir && (
            <span className="text-xs text-warning">{t("brain.cgNeedWorkDir")}</span>
          )}
        </div>
      )}

      {msg && (
        <div className={`text-xs ${msg.kind === "ok" ? "text-success" : "text-danger"}`}>
          {msg.text}
        </div>
      )}

      <div className="flex items-center gap-1.5 text-xs text-text-muted">
        <Info size={12} /> {t("brain.cgLocalNote")}
      </div>
    </div>
  );
}

function RecentWorkdirsMenu({ onPick, t }: { onPick: (path: string) => void; t: TFunc }) {
  const [open, setOpen] = useState(false);
  const [items, setItems] = useState<RecentWorkdir[]>([]);
  const [loading, setLoading] = useState(false);

  const load = async () => {
    setLoading(true);
    try {
      const list = await api.detectRecentWorkdirs();
      setItems(list);
    } catch {
      setItems([]);
    } finally {
      setLoading(false);
    }
  };

  const toggle = () => {
    if (!open) void load();
    setOpen(!open);
  };

  const formatTime = (ts?: number) => {
    if (!ts) return "";
    const d = new Date(ts);
    const now = Date.now();
    const diff = now - ts;
    if (diff < 3600_000) return `${Math.floor(diff / 60_000)} 分钟前`;
    if (diff < 86400_000) return `${Math.floor(diff / 3600_000)} 小时前`;
    if (diff < 7 * 86400_000) return `${Math.floor(diff / 86400_000)} 天前`;
    return d.toLocaleDateString();
  };

  return (
    <div className="relative">
      <Button size="sm" variant="secondary" onClick={toggle} title={t("brain.recentWorkdirs")}>
        <History size={13} />
      </Button>
      {open && (
        <>
          <div className="fixed inset-0 z-40" onClick={() => setOpen(false)} />
          <div className="absolute right-0 top-full z-50 mt-1 max-h-80 w-[420px] overflow-y-auto rounded-control border border-border bg-surface shadow-lg">
            <div className="border-b border-border px-3 py-2 text-xs font-medium text-text-secondary">
              {t("brain.recentWorkdirs")}
            </div>
            {loading ? (
              <div className="px-3 py-3 text-xs text-text-muted">{t("common.loading")}</div>
            ) : items.length === 0 ? (
              <div className="px-3 py-3 text-xs text-text-muted">{t("brain.noRecentWorkdirs")}</div>
            ) : (
              items.map((it, i) => (
                <button
                  key={`${it.path}-${i}`}
                  onClick={() => {
                    onPick(it.path);
                    setOpen(false);
                  }}
                  className="flex w-full items-start gap-2 border-b border-border px-3 py-2 text-left hover:bg-surface-hover"
                >
                  <span className="mt-0.5 rounded bg-primary/10 px-1.5 py-0.5 text-[10px] text-primary">
                    {it.source}
                  </span>
                  <div className="min-w-0 flex-1">
                    <div className="truncate font-mono text-xs text-text-primary">{it.path}</div>
                    {it.lastUsed && (
                      <div className="text-[11px] text-text-muted">{formatTime(it.lastUsed)}</div>
                    )}
                  </div>
                </button>
              ))
            )}
          </div>
        </>
      )}
    </div>
  );
}

function NumberField({
  label,
  value,
  min,
  max,
  step = 1,
  onChange,
}: {
  label: string;
  value: number;
  min?: number;
  max?: number;
  step?: number;
  onChange: (v: number) => void;
}) {
  return (
    <div className="flex-1">
      <div className="mb-1 text-xs font-medium text-text-secondary">{label}</div>
      <input
        type="number"
        value={value}
        min={min}
        max={max}
        step={step}
        onChange={(e) => onChange(Number(e.target.value))}
        className="h-9 w-full rounded-control border border-border bg-surface px-3 text-sm text-text-primary focus:outline-none focus:ring-2 focus:ring-ring"
      />
    </div>
  );
}
