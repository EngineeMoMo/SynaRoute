import { useState } from "react";
import { api } from "@/lib/bridge";
import { useStore } from "@/store";
import { useT } from "@/lib/useT";
import { Button } from "@/components/ui/Button";
import { Badge } from "@/components/ui/Badge";
import type { ModelInfo, ModelMapping, ProviderKey, Protocol } from "@/types";
import { X, RefreshCw, Plus, Trash2, ArrowLeftRight, Eye, EyeOff } from "lucide-react";

interface KeyEditorProps {
  initial: ProviderKey | null; // null = 新增
  onClose: () => void;
}

/** 新增/编辑 Key 抽屉面板（FR-002/004/005/006） */
export function KeyEditor({ initial, onClose }: KeyEditorProps) {
  const { activeCategory, loadCategory, vendors } = useStore();
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
  const [models, setModels] = useState<ModelInfo[]>(initial?.models ?? []);
  const [mappings, setMappings] = useState<ModelMapping[]>(initial?.mappings ?? []);
  const [fetching, setFetching] = useState(false);
  const [error, setError] = useState<string | null>(null);
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
        params: { temperature, maxTokens },
        models,
        mappings,
        health: initial?.health ?? { status: "unknown", failCount: 0 },
      };
      const fetched = await api.fetchModelsDraft(draft, secret || undefined);
      if (fetched.length === 0) {
        setError(t("editor.errNoModels"));
      } else {
        setModels(fetched);
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

  const save = async () => {
    if (!name.trim()) return setError(t("editor.errNeedName"));
    if (!baseUrl.trim()) return setError(t("editor.errNeedBaseUrl2"));

    const key: ProviderKey = {
      id: initial?.id ?? `k_${Date.now()}`,
      categoryId: activeCategory,
      name: name.trim(),
      vendor,
      baseUrl: baseUrl.trim(),
      protocol,
      hasSecret: initial?.hasSecret || secret.length > 0,
      enabled: initial?.enabled ?? false,
      priority: initial?.priority ?? 999,
      params: { temperature, maxTokens },
      models,
      mappings: mappings.filter((m) => m.expectedName && m.realName),
      health: initial?.health ?? { status: "unknown", failCount: 0 },
    };

    setSaving(true);
    setError(null);
    try {
      await api.upsertKey(key);
      if (secret) await api.saveSecret(key.id, secret);
      await loadCategory(activeCategory);
      onClose(); // 仅在成功后关闭
    } catch (e) {
      // 失败时保留抽屉并显示错误，避免"报错后窗口卡住、列表不刷新"
      setError(t("editor.errSave", { err: String(e) }));
      await loadCategory(activeCategory); // 刷新以反映可能的部分写入
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="fixed inset-0 z-50 flex justify-end bg-black/30" onClick={onClose}>
      <div
        className="flex h-full w-[440px] flex-col bg-surface shadow-2xl"
        onClick={(e) => e.stopPropagation()}
      >
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
          <Field label={t("editor.name")}>
            <input className={inputCls} value={name} onChange={(e) => setName(e.target.value)} placeholder={t("editor.namePlaceholder")} />
          </Field>

          <div className="flex gap-3">
            <Field label={t("editor.vendor")} className="flex-1">
              <select className={inputCls} value={vendor} onChange={(e) => handleVendorChange(e.target.value)}>
                {vendors.map((v) => (
                  <option key={v.id} value={v.id}>{v.name}</option>
                ))}
                {vendorMissing && <option value={vendor}>{vendor}</option>}
              </select>
            </Field>
            <Field label={t("editor.protocol")} className="w-32">
              <select className={inputCls} value={protocol} onChange={(e) => setProtocol(e.target.value as Protocol)}>
                <option value="anthropic">Anthropic</option>
                <option value="openai">OpenAI</option>
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
            <div className="flex flex-wrap gap-1.5 rounded-control border border-border p-2">
              {models.length === 0 ? (
                <span className="text-xs text-text-muted">{t("editor.noModels")}</span>
              ) : (
                models.map((m) => (
                  <Badge key={m.realName} variant={m.source === "manual" ? "neutral" : "info"}>
                    {m.realName}
                    <button
                      onClick={() => setModels(models.filter((x) => x.realName !== m.realName))}
                      className="ml-1 rounded hover:text-danger"
                      title={t("common.remove")}
                    >
                      <X size={10} />
                    </button>
                  </Badge>
                ))
              )}
            </div>
          </div>

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
                  <input
                    className={`${inputCls} font-mono`}
                    placeholder="opus-4-7"
                    value={m.expectedName}
                    onChange={(e) => {
                      const next = [...mappings];
                      next[i] = { ...m, expectedName: e.target.value };
                      setMappings(next);
                    }}
                  />
                  <ArrowLeftRight size={14} className="shrink-0 text-text-muted" />
                  <input
                    className={`${inputCls} font-mono`}
                    placeholder="GLM5.1"
                    value={m.realName}
                    onChange={(e) => {
                      const next = [...mappings];
                      next[i] = { ...m, realName: e.target.value };
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

          {error && <div className="rounded-control bg-danger/10 px-3 py-2 text-xs text-danger">{error}</div>}
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
