import { useEffect, useState } from "react";
import { api } from "@/lib/bridge";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/Card";
import { Switch } from "@/components/ui/Switch";
import { Button } from "@/components/ui/Button";
import { Badge } from "@/components/ui/Badge";
import { useT } from "@/lib/useT";
import type { TFunc } from "@/lib/i18n";
import type { AggregateMode, BrainConfig, BrainMember, CategoryType, ProviderKey } from "@/types";
import { Brain, Info, Save, Plus, X } from "lucide-react";

const CATEGORIES: { value: CategoryType; tKey: string }[] = [
  { value: "claude-cli", tKey: "nav.claude-cli" },
  { value: "claude-desktop", tKey: "nav.claude-desktop" },
  { value: "codex", tKey: "nav.codex" },
];

/** 大脑聚合配置页（FR-013 ~ FR-017）——按端分类独立配置 */
export function BrainPage() {
  const t = useT();
  // 本页独立维护分类，不跟随侧栏，使三端聚合配置互相隔离
  const [category, setCategory] = useState<CategoryType>("claude-cli");
  const [keys, setKeys] = useState<ProviderKey[]>([]);
  const [config, setConfig] = useState<BrainConfig | null>(null);
  const [saved, setSaved] = useState(false);

  useEffect(() => {
    setConfig(null);
    setSaved(false);
    void Promise.all([api.getBrainConfig(category), api.listKeys(category)]).then(
      ([cfg, ks]) => {
        setConfig(cfg);
        setKeys(ks);
      }
    );
  }, [category]);

  const update = (patch: Partial<BrainConfig>) => {
    if (!config) return;
    setConfig({ ...config, ...patch });
    setSaved(false);
  };

  const save = async () => {
    if (!config) return;
    await api.saveBrainConfig(config);
    setSaved(true);
  };

  // 可选的 Key+模型引用列表（仅有模型的 Key 可作决策者/汇总者）
  const refOptions = keys.flatMap((k) =>
    k.models.map((m) => ({ value: `${k.id}::${m.realName}`, label: `${k.name} · ${m.realName}` }))
  );

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

  const toggleMember = (keyId: string, model: string) => {
    if (memberChecked(keyId, model)) removeMember(keyId, model);
    else addMember(keyId, model);
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

        {/* 成员选择（FR-014）——逐个 Key 自选参与的模型 */}
        <Card>
          <CardHeader>
            <CardTitle>{t("brain.membersTitle")}</CardTitle>
          </CardHeader>
          <CardContent className="space-y-3">
            {keys.length === 0 && (
              <div className="text-xs text-text-muted">{t("brain.noKeys", { category: t(`nav.${category}`) })}</div>
            )}
            {keys.map((k) => (
              <KeyMemberRow
                key={k.id}
                k={k}
                t={t}
                members={config.members.filter((m) => m.keyId === k.id)}
                memberChecked={(model) => memberChecked(k.id, model)}
                onToggle={(model) => toggleMember(k.id, model)}
                onAddManual={(model) => addMember(k.id, model)}
                onRemove={(model) => removeMember(k.id, model)}
              />
            ))}
          </CardContent>
        </Card>

        {/* 决策者 + 汇总模型 */}
        <Card>
          <CardHeader>
            <CardTitle>{t("brain.decisionTitle")}</CardTitle>
          </CardHeader>
          <CardContent className="space-y-3">
            <LabeledSelect
              label={t("brain.decider")}
              hint={t("brain.deciderHint")}
              value={config.deciderRef ?? ""}
              options={refOptions}
              onChange={(v) => update({ deciderRef: v || undefined })}
            />
            <LabeledSelect
              label={t("brain.summarizer")}
              hint={t("brain.summarizerHint")}
              value={config.summarizerRef ?? ""}
              options={[{ value: "", label: t("brain.summarizerReuse") }, ...refOptions]}
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
                max={300000}
                step={5000}
                onChange={(v) => update({ totalTimeoutMs: v })}
              />
            </div>
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
      </div>
    </div>
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
function KeyMemberRow({
  k,
  t,
  members,
  memberChecked,
  onToggle,
  onAddManual,
  onRemove,
}: {
  k: ProviderKey;
  t: TFunc;
  members: BrainMember[];
  memberChecked: (model: string) => boolean;
  onToggle: (model: string) => void;
  onAddManual: (model: string) => void;
  onRemove: (model: string) => void;
}) {
  const [manual, setManual] = useState("");
  // 已选但不在拉取列表里的成员（手动添加的），单独显示为可移除 chip
  const manualMembers = members.filter((m) => !k.models.some((md) => md.realName === m.modelName));

  return (
    <div className="rounded-control border border-border p-2.5">
      <div className="mb-1.5 text-xs font-medium text-text-secondary">{k.name}</div>
      <div className="flex flex-wrap items-center gap-1.5">
        {k.models.map((m) => (
          <button
            key={m.realName}
            onClick={() => onToggle(m.realName)}
            className={`rounded-control border px-2 py-1 text-xs transition-colors ${
              memberChecked(m.realName)
                ? "border-primary bg-primary/12 text-primary"
                : "border-border text-text-secondary hover:bg-surface-hover"
            }`}
          >
            {m.realName}
          </button>
        ))}
        {manualMembers.map((m) => (
          <span
            key={m.modelName}
            className="inline-flex items-center gap-1 rounded-control border border-primary bg-primary/12 px-2 py-1 text-xs text-primary"
          >
            {m.modelName}
            <button onClick={() => onRemove(m.modelName)} title={t("common.remove")}>
              <X size={11} />
            </button>
          </span>
        ))}
      </div>
      {/* 手动输入模型名（该 Key 未拉取模型时也能加入聚合） */}
      <div className="mt-1.5 flex gap-1.5">
        <input
          className="h-8 flex-1 rounded-control border border-border bg-surface px-2 font-mono text-xs text-text-primary focus:outline-none focus:ring-2 focus:ring-ring"
          placeholder={t("brain.memberManualPlaceholder")}
          value={manual}
          onChange={(e) => setManual(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              onAddManual(manual);
              setManual("");
            }
          }}
        />
        <Button
          size="sm"
          variant="secondary"
          disabled={!manual.trim()}
          onClick={() => {
            onAddManual(manual);
            setManual("");
          }}
        >
          <Plus size={13} /> {t("brain.join")}
        </Button>
      </div>
    </div>
  );
}

function LabeledSelect({
  label,
  hint,
  value,
  options,
  onChange,
}: {
  label: string;
  hint?: string;
  value: string;
  options: { value: string; label: string }[];
  onChange: (v: string) => void;
}) {
  const t = useT();
  return (
    <div>
      <div className="mb-1 text-xs font-medium text-text-secondary">{label}</div>
      <select
        value={value}
        onChange={(e) => onChange(e.target.value)}
        className="h-9 w-full rounded-control border border-border bg-surface px-3 text-sm text-text-primary focus:outline-none focus:ring-2 focus:ring-ring"
      >
        <option value="">{t("brain.selectPlaceholder")}</option>
        {options.map((o) => (
          <option key={o.value} value={o.value}>
            {o.label}
          </option>
        ))}
      </select>
      {hint && <div className="mt-0.5 text-[11px] text-text-muted">{hint}</div>}
    </div>
  );
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
