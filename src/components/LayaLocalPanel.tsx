import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Button } from "@/components/ui/Button";
import { useStore } from "@/store";

type Status = { supported: boolean; phase: string; endpoint: string; logs: string[]; error: string | null; errorCode?: string | null; elapsedSeconds?: number };
const activePhases = ["checking", "python", "dependencies", "model", "running", "degraded"];
export function LayaLocalPanel({ onConfigure }: { onConfigure: (endpoint: string) => Promise<void> }) {
  const zh = useStore(s => s.lang) === "zh";
  const text = (cn: string, en: string) => zh ? cn : en;
  const [state, setState] = useState<Status | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const operation = useRef(0);
  const acting = useRef(false);
  const configure = useRef(onConfigure);
  configure.current = onConfigure;
  const configureWhenReady = useRef(false);
  const alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    let cancelled = false;
    let timer = 0;
    async function poll() {
      if (!("__TAURI_INTERNALS__" in window)) return;
      const generation = operation.current;
      try {
        const next = await invoke<Status>("laya_local_status");
        if (cancelled || generation !== operation.current || acting.current) return;
        setState(next);
        if (next.phase === "running" && configureWhenReady.current) {
          configureWhenReady.current = false;
          await configure.current(next.endpoint);
          if (!cancelled) setNotice(zh ? "服务已就绪，本地接口已保存。" : "Service is ready. Local endpoint saved.");
        }
      } catch (e) { if (!cancelled) setError(String(e)); }
      finally { if (!cancelled) timer = window.setTimeout(() => void poll(), 1500); }
    }
    void poll();
    return () => { cancelled = true; alive.current = false; window.clearTimeout(timer); };
  }, [zh]);
  const working = !!state && activePhases.includes(state.phase);
  const phases: Record<string, string> = {
    idle: text("尚未启动", "Not started"), stopped: text("已停止", "Stopped"),
    checking: text("检查本地环境…", "Checking environment…"), python: text("安装 Python…", "Installing Python…"),
    dependencies: text("安装并校验依赖…", "Installing and validating dependencies…"), model: text("下载或加载模型…", "Downloading or loading model…"),
    running: text("运行中", "Running"), degraded: text("服务暂时无响应，正在检查…", "Service is unresponsive; checking…"), failed: text("未能完成", "Unable to complete"),
  };
  const remedies: Record<string, string> = {
    disk: text("磁盘空间不足。安装目录、临时目录和模型缓存所在磁盘请预留至少 6 GB，然后重试。", "Insufficient storage. Reserve at least 6 GB on the installation, temporary and model-cache drives, then retry."),
    permission: text("目录不可写。请检查安装和缓存目录的权限，以及安全软件是否拦截。", "Directory access denied. Check installation/cache permissions and security software."),
    network: text("下载连接失败。请检查网络、系统代理及证书，恢复后重试；不要关闭证书验证。", "Download connection failed. Check connectivity, system proxy and certificates, then retry. Keep certificate verification enabled."),
    python: text("Python 安装失败。确认 Windows 应用安装程序可用，或安装 Python 3.12 后重试。", "Python setup failed. Check Windows App Installer, or install Python 3.12 and retry."),
    environment: text("依赖校验失败。点击“修复环境并运行”创建新的环境；原环境会保留。", "Dependency validation failed. Repair and run to create a new environment; the previous one is retained."),
    locked: text("另一安装任务或服务占用该环境，请先停止它，再重试。", "Another installer or service owns this environment. Stop it before retrying."),
    port: text("启动时端口被其他程序抢占。重试会重新选择可用端口。", "Another process claimed the port. Retry to select an available port."),
    memory: text("内存或虚拟内存不足。关闭其他高占用程序，检查系统虚拟内存后重试。", "Insufficient memory or page-file capacity. Free memory and check system virtual memory before retrying."),
    timeout: text("等待超过当前阶段时限，任务已停止。检查网络和资源后重试；已完成的环境与模型缓存会保留。", "This stage exceeded its time limit and was stopped. Check connectivity and resources, then retry; validated environments and model cache are retained."),
    interrupted: text("上次任务意外中断。可以重试，程序会重新检查已有环境。", "The previous task was interrupted. Retry to recheck the existing environment."),
    setup: text("无法启动安装程序。查看记录，检查目录权限或安全软件拦截后重试。", "Could not launch setup. Check details, directory permissions and security software, then retry."),
    process: text("安装或服务异常退出。查看记录定位原因；依赖损坏可选择修复环境。", "Installer or service exited unexpectedly. Check the log; use repair for damaged dependencies."),
  };
  async function action(stop = false, repair = false) {
    if (acting.current) return;
    acting.current = true; operation.current++; setBusy(true); setError(""); setNotice("");
    configureWhenReady.current = !stop;
    try {
      const next = await invoke<Status>(stop ? "laya_local_stop" : "laya_local_start", stop ? {} : { repair });
      if (!alive.current) return;
      setState(next);
      if (!stop && next.phase === "running") {
        configureWhenReady.current = false;
        await configure.current(next.endpoint);
        if (alive.current) setNotice(text("服务已就绪，本地接口已保存。", "Service is ready. Local endpoint saved."));
      }
    } catch (e) { configureWhenReady.current = false; if (alive.current) setError(String(e)); }
    finally { acting.current = false; if (alive.current) setBusy(false); }
  }
  async function copyDiagnostics() {
    try {
      await navigator.clipboard.writeText(JSON.stringify({ phase: state?.phase, errorCode: state?.errorCode, error: state?.error, elapsedSeconds: state?.elapsedSeconds, logs: state?.logs }, null, 2));
      setNotice(text("诊断记录已复制。", "Diagnostics copied."));
    } catch { setError(text("无法复制，请展开运行记录手动复制。", "Copy failed. Expand the log and copy it manually.")); }
  }
  return <section className="space-y-3 rounded-control border border-border p-3" aria-busy={busy}>
    <div className="flex flex-wrap items-center justify-between gap-2">
      <h3 className="text-sm font-medium text-text-primary">{text("Laya 本地服务", "Local Laya service")}</h3>
      <span role="status" className="text-xs text-text-secondary">{state ? phases[state.phase] ?? state.phase : text("等待连接桌面服务…", "Waiting for desktop service…")}</span>
    </div>
    <p className="text-xs leading-relaxed text-text-secondary">{text("先检查已有环境，可用则直接启动。首次安装需要联网，并建议预留至少 6 GB 磁盘空间。服务就绪后自动保存本地接口；运行方式保持你的选择。退出 SynaRoute 时服务停止。", "Existing environments are checked and reused. First setup needs internet access and at least 6 GB of recommended free disk space. The local endpoint is saved once ready; your selected mode is preserved. The service stops when SynaRoute exits.")}</p>
    {working && state?.phase !== "running" && <p className="text-xs text-text-muted">{text(`已用时 ${state?.elapsedSeconds ?? 0} 秒。环境检查最多 2 分钟，Python 安装 15 分钟，依赖安装 30 分钟，模型加载 20 分钟；可随时取消。`, `Elapsed: ${state?.elapsedSeconds ?? 0}s. Limits: environment check 2 min, Python 15 min, dependencies 30 min, model loading 20 min. Cancel at any time.`)}</p>}
    {state && !state.supported ? <p className="text-xs text-text-muted">{text("一键部署目前支持 Windows，其他系统可手动连接 Laya 服务。", "One-click setup currently supports Windows; other systems can connect to Laya manually.")}</p> : <div className="flex flex-wrap gap-2">
      <Button size="sm" disabled={!state || busy || working} onClick={() => void action()}>{state?.phase === "failed" ? text("重试", "Retry") : text("一键安装并运行", "Install and run")}</Button>
      {working && <Button size="sm" variant="secondary" disabled={busy} onClick={() => void action(true)}>{state?.phase === "running" ? text("停止", "Stop") : text("取消", "Cancel")}</Button>}
      {!working && !!state?.supported && <Button size="sm" variant="secondary" disabled={busy} onClick={() => void action(false, true)}>{text("修复环境并运行", "Repair and run")}</Button>}
      {state?.phase === "running" && <Button size="sm" variant="secondary" disabled={busy} onClick={() => void action()}>{text("使用此本地服务", "Use this local service")}</Button>}
    </div>}
    {!working && !!state?.supported && <p className="text-xs text-text-muted">{text("修复会重新下载依赖并创建独立环境，保留原环境和模型缓存，需要额外磁盘空间。", "Repair downloads dependencies into a separate environment, retaining the previous environment and model cache; additional disk space is required.")}</p>}
    {(error || state?.error) && <div role="alert" className="space-y-1 break-words text-xs text-danger"><p>{error || remedies[state?.errorCode ?? ""] || state?.error}</p>{state?.error && <details><summary className="cursor-pointer">{text("错误详情", "Error details")}</summary><p className="mt-1">{state.error}</p></details>}</div>}
    {notice && <p role="status" className="text-xs text-text-secondary">{notice}</p>}
    {!!state?.logs.length && <details><summary className="cursor-pointer text-xs text-text-secondary">{text("安装与运行记录", "Installation and service log")}</summary><pre className="mt-2 max-h-48 overflow-auto whitespace-pre-wrap break-all text-xs text-text-muted">{state.logs.join("\n")}</pre><Button size="sm" variant="secondary" onClick={() => void copyDiagnostics()}>{text("复制诊断记录", "Copy diagnostics")}</Button></details>}
  </section>;
}
