import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { api, isTauri } from "@/lib/bridge";
import { useStore } from "@/store";
import type { CategoryType } from "@/types";
import { Button } from "@/components/ui/Button";

interface NativeSnapshot {
  id: string; models: { id: string; efforts: string[] }[];
  status: string; output: string; decision: string;
  approval: { ticket: string; request: unknown } | null;
}

export function NativeEffortPanel({ category, workDir, autoFollowActive = false }: { category: CategoryType; workDir?: string; autoFollowActive?: boolean }) {
  const lang = useStore(s => s.lang);
  const t = (zh: string, en: string) => lang === "zh" ? zh : en;
  const runtime = category === "codex" ? "codex" : "claude";
  const [detecting, setDetecting] = useState(true);
  const [manual, setManual] = useState(false);
  const [refresh, setRefresh] = useState(0);
  const [executable, setExecutable] = useState("");
  const [cwd, setCwd] = useState("");
  const [view, setView] = useState<NativeSnapshot | null>(null);
  const [model, setModel] = useState("");
  const [effort, setEffort] = useState("");
  const [auto, setAuto] = useState(false);
  const [prompt, setPrompt] = useState("");
  const [busy, setBusy] = useState(false);
  const [started, setStarted] = useState(false);
  const [error, setError] = useState("");
  const sessionId = useRef<string | null>(null);
  const generation = useRef(0);
  useEffect(() => {
    if (view) return;
    let active = true;
    setDetecting(true); setError(""); setExecutable(""); setCwd("");
    if (!isTauri()) { setDetecting(false); setManual(true); return; }
    void invoke<{ executable: string | null; cwd: string | null }>("native_defaults", { category, workDir: workDir ?? null, autoFollowActive }).then(result => {
      if (!active) return;
      setExecutable(result.executable ?? ""); setCwd(result.cwd ?? "");
      setManual(!result.executable || !result.cwd);
    }).catch(e => { if (active) { setError(String(e)); setManual(true); } })
      .finally(() => { if (active) setDetecting(false); });
    return () => { active = false; };
  }, [category, workDir, autoFollowActive, refresh, view?.id]);
  useEffect(() => () => {
    generation.current++;
    if (sessionId.current) void invoke("native_close", { id: sessionId.current }).catch(() => {});
  }, []);
  useEffect(() => {
    if (!view?.id || !busy) return;
    const id = view.id;
    const timer = window.setInterval(() => {
      void invoke<NativeSnapshot>("native_snapshot", { id }).then(next => {
        if (sessionId.current === id) setView(next);
      }).catch(() => {});
    }, 400);
    return () => window.clearInterval(timer);
  }, [view?.id, busy]);
  const cls = "w-full rounded-control border border-border bg-surface px-3 py-2 text-sm text-text-primary focus-visible:ring-2 focus-visible:ring-ring";
  async function open() {
    if (!isTauri()) { setError(t("请在桌面应用中使用原生会话。", "Native sessions require the desktop app.")); return; }
    const version = ++generation.current;
    setBusy(true); setError("");
    try {
      const next = await invoke<NativeSnapshot>("native_open", { runtime, executable, cwd });
      if (generation.current !== version) { await invoke("native_close", { id: next.id }); return; }
      sessionId.current = next.id; setView(next); setModel(next.models[0].id);
      setEffort(next.models[0].efforts[0]); setStarted(false);
    } catch (e) { if (generation.current === version) setError(String(e)); }
    finally { if (generation.current === version) setBusy(false); }
  }
  async function close() {
    const id = sessionId.current;
    generation.current++; sessionId.current = null;
    setView(null); setBusy(false); setStarted(false);
    if (id) await invoke("native_close", { id }).catch(e => setError(String(e)));
  }
  async function send() {
    if (!view) return;
    const id = view.id;
    setBusy(true); setStarted(true); setError("");
    try {
      const next = await invoke<NativeSnapshot>("native_turn", { request: { id, category, model, effort, prompt, auto } });
      if (sessionId.current === id) { setView(next); setPrompt(""); }
    } catch (e) {
      if (sessionId.current === id) { setError(String(e)); setView(v => v ? { ...v, status: "closed", approval: null } : v); }
    } finally { if (sessionId.current === id) setBusy(false); }
  }
  async function approve(allow: boolean) {
    if (!view?.approval) return;
    try { await invoke("native_approve", { id: view.id, ticket: view.approval.ticket, allow }); }
    catch (e) { setError(String(e)); }
  }
  const efforts = view?.models.find(m => m.id === model)?.efforts ?? [];
  return <details className="rounded-card border border-border bg-surface p-4">
    <summary className="cursor-pointer text-sm font-medium text-text-primary">{t("原生自动思考 · 预览", "Native auto effort · Preview")}</summary>
    <p className="mt-3 text-xs leading-relaxed text-text-secondary">{t(
      "在这里启动独立的 Codex / Claude Code 会话，使用已有客户端的登录与配置。每次提问前选择客户端原生档位，模型保持不变；不会控制已经打开的官方窗口。工具可在工作目录内操作，需要审批时会显示请求。",
      "Start a separate Codex / Claude Code session using your CLI login and settings. Select native effort before each turn while keeping the model fixed. Existing client windows are not controlled. Tools can operate in the workspace; permission requests appear here.")}</p>
    {!view ? <fieldset disabled={busy || detecting} className="mt-4 space-y-3">
      <p className="text-sm text-text-primary">{t("跟随当前分类：", "Current category: ")}{runtime === "codex" ? "Codex" : "Claude Code"}</p>
      {category === "claude-desktop" && <p className="text-xs text-text-secondary">{t("此分类使用 Claude Code 启动独立原生会话，需要本机安装 Claude Code。", "This category uses Claude Code for independent native sessions; Claude Code must be installed.")}</p>}
      <div role="status" className="space-y-1 break-all text-xs text-text-secondary">
        {detecting ? t("正在自动检测客户端和工作目录…", "Detecting client and workspace…") : <>
          <p>{executable ? `${t("客户端：", "Executable: ")}${executable}` : t("未找到客户端，请在下方补充可执行文件路径。", "Client not found. Specify its executable below.")}</p>
          <p>{cwd ? `${t("工作目录：", "Workspace: ")}${cwd}` : t("当前分类尚无可用工作目录，请选择项目文件夹。", "No workspace configured for this category. Choose a project folder.")}</p>
        </>}
      </div>
      <div className="flex flex-wrap gap-2"><Button variant="secondary" onClick={() => setRefresh(n => n + 1)}>{t("重新检测", "Detect again")}</Button><Button variant="ghost" aria-expanded={manual} onClick={() => setManual(v => !v)}>{t("手动调整", "Manual settings")}</Button></div>
      {manual && <div className="space-y-3">
      <label className="block text-sm text-text-secondary">{t("客户端可执行文件（绝对路径，Windows 需 .exe）", "Client executable (absolute path; .exe on Windows)")}<input className={cls} value={executable} onChange={e => setExecutable(e.target.value)} spellCheck={false} /></label>
      <label className="block text-sm text-text-secondary">{t("工作目录（绝对路径）", "Workspace (absolute path)")}<input className={cls} value={cwd} onChange={e => setCwd(e.target.value)} spellCheck={false} /></label>
      <Button variant="secondary" onClick={() => void api.pickDirectory().then(dir => { if (dir) setCwd(dir); }).catch(e => setError(String(e)))}>{t("选择项目文件夹", "Choose project folder")}</Button>
      </div>}
      <Button disabled={!executable.trim() || !cwd.trim() || busy || detecting} onClick={() => void open()}>{busy ? t("正在读取原生目录…", "Reading native catalog…") : t("启动独立会话", "Start session")}</Button>
    </fieldset> : <div className="mt-4 space-y-3">
      <div className="grid gap-3 sm:grid-cols-2">
        <label className="text-sm text-text-secondary">{t("原生模型（开始后固定）", "Native model (fixed after first turn)")}<select className={cls} disabled={busy || started} value={model} onChange={e => { setModel(e.target.value); setEffort(view.models.find(m => m.id === e.target.value)!.efforts[0]); }}>{view.models.map(m => <option key={m.id}>{m.id}</option>)}</select></label>
        <label className="text-sm text-text-secondary">{t("手动 / 回退档位", "Manual / fallback effort")}<select className={cls} disabled={busy} value={effort} onChange={e => setEffort(e.target.value)}>{efforts.map(e => <option key={e}>{e}</option>)}</select></label>
      </div>
      <label className="flex items-center gap-2 text-sm text-text-primary"><input type="checkbox" checked={auto} disabled={busy} onChange={e => setAuto(e.target.checked)} />{t("自动选择原生档位", "Choose native effort automatically")}</label>
      <p className="text-xs text-text-secondary">{t("自动模式使用上方已保存的判断服务，把本次问题文本发送给它（云端可能收费）。Jev、Laya 或兼容接口均可；未配置、超时或不确定时使用回退档位。关闭后无需判断服务。当前支持每轮开始前切换，不支持生成过程中切换。", "Auto mode sends the current task text to the saved judge above (cloud calls may cost money). Jev, Laya and compatible endpoints are optional; failures preserve fallback effort. Switching occurs before each turn, not during generation.")}</p>
      <label className="block text-sm text-text-secondary">{t("任务", "Task")}<textarea className={cls} rows={4} value={prompt} disabled={busy || view.status === "closed"} onChange={e => setPrompt(e.target.value)} /></label>
      <div className="flex flex-wrap gap-2"><Button disabled={busy || !prompt.trim() || view.status === "closed"} onClick={() => void send()}>{busy ? t("正在执行…", "Running…") : t("发送到原生客户端", "Send to native client")}</Button><Button variant="secondary" onClick={() => void close()}>{busy ? t("停止并关闭", "Stop and close") : t("关闭会话", "Close session")}</Button></div>
      {view.approval && <div className="rounded-control border border-border p-3 space-y-2"><p className="text-sm font-medium">{t("客户端请求工具授权", "Client requests tool permission")}</p><pre className="max-h-64 overflow-auto whitespace-pre-wrap break-all text-xs">{JSON.stringify(view.approval.request, null, 2)}</pre><div className="flex gap-2"><Button onClick={() => void approve(false)} variant="secondary">{t("拒绝", "Deny")}</Button><Button onClick={() => void approve(true)}>{t("允许本次", "Allow once")}</Button></div></div>}
      {view.decision && <p role="status" className="text-xs text-text-secondary">{view.decision}</p>}
      {view.output && <pre className="max-h-96 overflow-auto whitespace-pre-wrap break-words rounded-control bg-surface-elevated p-3 text-sm text-text-primary">{view.output}</pre>}
    </div>}
    {error && <p role="alert" className="mt-3 text-sm text-danger">{error}</p>}
  </details>;
}
