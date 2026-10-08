import { LayaLocalPanel } from "./LayaLocalPanel";
import { useEffect, useState } from "react";
import { api } from "@/lib/bridge";
import { defaultDecision, decisionPresets, type DecisionConfig, type DecisionProvider, type EffortApi } from "@/lib/decision";
import { useStore } from "@/store";
import { Button } from "@/components/ui/Button";
import type { BrainConfig, CategoryType, ProviderKey } from "@/types";

export function DecisionPanel({ category, brain, keys }: { category: CategoryType; brain: BrainConfig; keys: ProviderKey[] }) {
  const lang = useStore(s => s.lang);
  const text = (zh: string, en: string) => lang === "zh" ? zh : en;
  const [config, setConfig] = useState(defaultDecision().config);
  const [hasSecret, setHasSecret] = useState(false);
  const [secret, setSecret] = useState("");
  const [clearSecret, setClearSecret] = useState(false);
  const [loaded, setLoaded] = useState(false);
  const [busy, setBusy] = useState(false);
  const [status, setStatus] = useState("");
  useEffect(() => {
    let active = true;
    api.getDecisionConfig(category).then(view => {
      if (!active) return;
      setConfig({ ...view.config, ...(view.config.endpoint ? {} : decisionPresets[view.config.provider]) });
      setHasSecret(view.hasSecret); setLoaded(true);
    }).catch(e => { if (active) setStatus(String(e)); });
    return () => { active = false; };
  }, [category]);
  const update = (patch: Partial<DecisionConfig>) => { setConfig(c => ({ ...c, ...patch })); setStatus(""); };
  const refs = [...new Set([...brain.members.map(m => `${m.keyId}::${m.modelName}`), brain.deciderRef, brain.summarizerRef].filter((r): r is string => !!r))];
  const selectClass = "w-full rounded-control border border-border bg-surface px-3 py-2 text-sm text-text-primary focus-visible:ring-2 focus-visible:ring-ring";
  async function save() {
    setBusy(true); setStatus("");
    try {
      await api.saveDecisionConfig(category, config, secret || null, clearSecret);
      const view = await api.getDecisionConfig(category);
      setHasSecret(view.hasSecret); setSecret(""); setClearSecret(false);
      setStatus(text("已保存，下次会诊生效。", "Saved. Applies to the next consultation."));
    } catch (e) { setStatus(String(e)); } finally { setBusy(false); }
  }
  async function configureLocal(endpoint: string) {
    const next = { ...config, provider: "laya" as const, endpoint, model: "multilingual" };
    await api.saveDecisionConfig(category, next, null, true);
    setConfig(next); setHasSecret(false); setSecret(""); setClearSecret(false);
    setStatus(text("已保存本地服务配置；如需自动调整，请选择运行方式并授权模型。", "Local service saved. Choose a mode and authorize models to enable automatic adjustment."));
  }
  return <details className="rounded-card border border-border bg-surface p-4">
    <summary className="cursor-pointer text-sm font-medium text-text-primary">{text("动态思考 · 可选增强", "Dynamic reasoning · Optional")}</summary>
    <p className="mt-3 text-xs leading-relaxed text-text-secondary">{text("不开启也能正常会诊。开启后，每轮会将问题文本发送给所选判断服务（云端可能收费），判断低／中／高思考强度；不会发送检索文件或图片。失败时保留原配置。", "Consultations work without this. When enabled, each task's text is sent to the selected service (cloud calls may cost money) to choose low, medium or high reasoning effort. Retrieved files and images are excluded. Failures preserve original settings.")}</p>
    <fieldset disabled={!loaded || busy} className="mt-4 space-y-4">
      <div className="grid gap-4 sm:grid-cols-2">
        <label className="space-y-1 text-sm text-text-secondary"><span>{text("运行方式", "Mode")}</span><select aria-label={text("运行方式", "Mode")} className={selectClass} value={config.mode} onChange={e => update({ mode: e.target.value as DecisionConfig["mode"] })}>
          <option value="off">{text("关闭（默认）", "Off (default)")}</option><option value="suggest">{text("仅建议：记入运行日志", "Suggest: record in activity log")}</option><option value="auto">{text("自动调整已授权模型", "Adjust authorized models")}</option>
        </select></label>
        <label className="space-y-1 text-sm text-text-secondary"><span>{text("判断服务", "Decision service")}</span><select aria-label={text("判断服务", "Decision service")} className={selectClass} value={config.provider} onChange={e => {
          const provider = e.target.value as DecisionProvider;
          update({ provider, ...decisionPresets[provider] }); setSecret(""); setClearSecret(true);
        }}><option value="laya">Laya (laya-serve)</option><option value="jev">Jev (TypeSafe)</option><option value="openai">OpenAI-compatible</option></select></label>
      </div>
      <label className="block space-y-1 text-sm text-text-secondary"><span>{text("完整接口地址", "Full endpoint URL")}</span><input className={selectClass} value={config.endpoint} placeholder="https://…/v1/chat/completions" onChange={e => { update({ endpoint: e.target.value }); setClearSecret(true); }} /></label>
      <div className="grid gap-4 sm:grid-cols-2">
        <label className="space-y-1 text-sm text-text-secondary"><span>{text("判断模型", "Decision model")}</span><input className={selectClass} value={config.model} onChange={e => update({ model: e.target.value })} /></label>
        <label className="space-y-1 text-sm text-text-secondary"><span>API Key {hasSecret && !clearSecret ? text("（已加密保存）", "(stored encrypted)") : text("（本地可留空）", "(optional locally)")}</span><input type="password" autoComplete="new-password" className={selectClass} value={secret} onChange={e => setSecret(e.target.value)} placeholder={text("留空保留；更换地址需重新填写", "Blank keeps stored key; re-enter after changing URL")} /></label>
      </div>
      {hasSecret && <label className="flex items-center gap-2 text-xs text-text-secondary"><input type="checkbox" checked={clearSecret} onChange={e => setClearSecret(e.target.checked)} />{text("清除之前保存的判断服务密钥", "Clear the previously stored decision credential")}</label>}
      <div className="space-y-2">
        <p className="text-sm font-medium text-text-primary">{text("模型逐个授权", "Authorize each model")}</p>
        <p className="text-xs leading-relaxed text-text-secondary">{text("仅对确认支持 low / medium / high 的模型开启。Chat 模式发送 reasoning_effort；Claude adaptive 模式要求模型支持自适应思考。整轮工具调用保持同一强度。成员名单不变。", "Enable only for models confirmed to support low / medium / high. Chat sends reasoning_effort; Claude adaptive requires adaptive-thinking support. Effort stays fixed throughout tool turns. Team membership is unchanged.")}</p>
        {refs.length === 0 && <p className="text-xs text-text-muted">{text("先配置会诊成员和最终决策者。", "Configure consultation members and a decider first.")}</p>}
        {refs.map(ref => {
          const split = ref.indexOf("::"); const key = keys.find(k => k.id === ref.slice(0, split));
          return <label key={ref} className="flex flex-wrap items-center justify-between gap-2 text-sm text-text-secondary"><span className="min-w-0 break-all">{key?.name && <span className="text-text-muted">{key.name} · </span>}{ref.slice(split + 2)}</span><select aria-label={`${key?.name ?? ""} ${ref.slice(split + 2)} ${text("思考参数", "reasoning parameter")}`} className={selectClass + " sm:w-auto"} value={config.targets[ref] ?? ""} onChange={e => {
            const targets = { ...config.targets }; if (e.target.value) targets[ref] = e.target.value as EffortApi; else delete targets[ref]; update({ targets });
          }}><option value="">{text("保持原样", "Unchanged")}</option>{key?.protocol === "anthropic" ? <option value="adaptive">Claude adaptive</option> : <option value="openai">Chat reasoning_effort</option>}</select></label>;
        })}
      </div>
      {config.provider === "laya" && <LayaLocalPanel key={category} onConfigure={configureLocal} />}
      <Button size="sm" onClick={() => void save()} disabled={busy}>{busy ? text("保存中…", "Saving…") : text("保存动态思考设置", "Save reasoning settings")}</Button>
    </fieldset>
    {status && <p role="status" className="mt-3 text-xs text-text-secondary">{status}</p>}
  </details>;
}
