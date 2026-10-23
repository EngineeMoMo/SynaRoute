import { useEffect, useRef, useState } from "react";
import { api } from "@/lib/bridge";
import type { LayaLocalStatus as Status } from "@/lib/decision";
import { Button } from "@/components/ui/Button";
import { ConfirmDialog } from "@/components/ui/ConfirmDialog";
import { ToggleRow } from "@/components/ToggleRow";
import { useStore } from "@/store";

const activePhases = ["checking", "python", "dependencies", "model", "running", "degraded", "stopping"];
type Pending = { kind: "start" | "repair" | "launch" | "use" };
/** `replaces`: saving the local endpoint would overwrite a remote decision service or clear a stored key. */
export function LayaLocalPanel({ onConfigure, replaces }: { onConfigure: (endpoint: string) => Promise<void>; replaces: boolean }) {
  const zh = useStore(s => s.lang) === "zh";
  const text = (cn: string, en: string) => zh ? cn : en;
  const [state, setState] = useState<Status | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const [pending, setPending] = useState<Pending | null>(null);
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
      const generation = operation.current;
      try {
        const next = await api.layaLocalStatus();
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
    stopping: text("等待 Python 安装完成后停止…", "Stopping after the Python installer finishes…"),
  };
  const remedies: Record<string, string> = {
    disk: text("检测到可用空间不足。请查看错误详情中的磁盘、剩余空间和本次需要的空间，释放空间后重试。", "Insufficient free space. Check the drive, available space and required space in the error details, then free space and retry."),
    permission: text("目录不可写。请检查安装和缓存目录的权限，以及安全软件是否拦截。", "Directory access denied. Check installation/cache permissions and security software."),
    network: text("下载连接失败。请检查网络、系统代理及证书，恢复后重试；不要关闭证书验证。无法访问 Hugging Face 时可开启下方的镜像下载。", "Download connection failed. Check connectivity, system proxy and certificates, then retry. Keep certificate verification enabled. If Hugging Face is unreachable, turn on mirror download below."),
    python: text("Python 自动安装未完成。点击“重试”可重新下载官网安装包，核对 SHA-256 与发布者签名后安装；具体原因见错误详情。", "Automatic Python setup did not complete. Retry to download the official installer, check its SHA-256 and publisher signature, and install again. See error details for the cause."),
    platform: text("当前系统无法运行本地服务所需的 x64 组件（Arm 设备需要 Windows 11）。可改用其他设备上运行的 Laya 服务，并在上方填写其接口。", "This system cannot run the x64 components the local service needs (Arm devices require Windows 11). Use a Laya service on another device and enter its endpoint above."),
    integrity: text("模型文件与已验证的 SHA-256 不一致，已拒绝加载。请删除模型缓存文件夹（默认是用户目录下 .cache\\huggingface\\hub\\models--convaiinnovations--laya）后重试；若开启了镜像下载，可先关闭它。", "A model file does not match its verified SHA-256 and was not loaded. Delete the model cache folder (by default .cache\\huggingface\\hub\\models--convaiinnovations--laya in your user folder) and retry; if mirror download is on, consider turning it off."),
    environment: text("依赖校验失败。点击“修复环境并运行”创建新的环境；原环境会保留。", "Dependency validation failed. Repair and run to create a new environment; the previous one is retained."),
    locked: text("另一安装任务或服务占用该环境，请先停止它，再重试。", "Another installer or service owns this environment. Stop it before retrying."),
    port: text("启动时端口被其他程序抢占。重试会重新选择可用端口。", "Another process claimed the port. Retry to select an available port."),
    memory: text("内存或虚拟内存不足。关闭其他高占用程序，检查系统虚拟内存后重试。", "Insufficient memory or page-file capacity. Free memory and check system virtual memory before retrying."),
    timeout: text("等待超过当前阶段时限，任务已停止。检查网络和资源后重试；已完成的环境与模型缓存会保留。", "This stage exceeded its time limit and was stopped. Check connectivity and resources, then retry; validated environments and model cache are retained."),
    interrupted: text("上次任务意外中断。可以重试，程序会重新检查已有环境。", "The previous task was interrupted. Retry to recheck the existing environment."),
    setup: text("无法启动安装程序。查看记录，检查目录权限或安全软件拦截后重试。", "Could not launch setup. Check details, directory permissions and security software, then retry."),
    process: text("安装或服务异常退出。查看记录定位原因；依赖损坏可选择修复环境。", "Installer or service exited unexpectedly. Check the log; use repair for damaged dependencies."),
    install: text("本地环境需要安装或修复，尚未下载任何内容。点击按钮并确认后再安装。", "The local environment needs setup; nothing was downloaded. Click the button and confirm to install."),
  };
  // "use" only reads status: if the service died since the last poll, starting it from that button
  // would download and install without the confirmation dialog. "launch" skips the dialog for an
  // installed environment, so it always sends noInstall and the script refuses instead of installing.
  async function action(kind: "start" | "launch" | "repair" | "stop" | "use") {
    if (acting.current) return;
    acting.current = true; operation.current++; setBusy(true); setError(""); setNotice("");
    configureWhenReady.current = kind === "start" || kind === "launch" || kind === "repair";
    try {
      const next = kind === "stop" ? await api.layaLocalStop()
        : kind === "use" ? await api.layaLocalStatus()
        : await api.layaLocalStart({ repair: kind === "repair", noInstall: kind === "launch" });
      if (!alive.current) return;
      setState(next);
      if (kind === "stop" && next.phase === "stopping") {
        setNotice(text("Python 正在安装，安装完成后会自动停止。", "Python is installing; the service stops as soon as the installer finishes."));
      } else if (kind !== "stop" && next.phase === "running") {
        configureWhenReady.current = false;
        await configure.current(next.endpoint);
        if (alive.current) setNotice(text("服务已就绪，本地接口已保存。", "Service is ready. Local endpoint saved."));
      } else if (kind === "use") {
        setError(text("本地服务已不在运行，请重新启动。", "The local service is no longer running. Start it again."));
      }
    } catch (e) { configureWhenReady.current = false; if (alive.current) setError(String(e)); }
    finally { acting.current = false; if (alive.current) setBusy(false); }
  }
  // A preference for the next launch only: it never starts or stops the service now.
  async function setAutostart(enabled: boolean) {
    if (acting.current) return;
    acting.current = true; operation.current++; setBusy(true); setError(""); setNotice("");
    try {
      const next = await api.layaLocalSetAutostart(enabled);
      if (!alive.current) return;
      setState(next);
      setNotice(enabled ? text("已开启：下次启动 SynaRoute 时自动启动本地服务。", "On: the local service starts with SynaRoute next time.") : text("已关闭自动启动。", "Automatic start is off."));
    } catch (e) { if (alive.current) setError(String(e)); }
    finally { acting.current = false; if (alive.current) setBusy(false); }
  }
  // Applies to the next model download only; never starts, stops or re-downloads anything now.
  async function setMirror(enabled: boolean) {
    if (acting.current) return;
    acting.current = true; operation.current++; setBusy(true); setError(""); setNotice("");
    try {
      const next = await api.layaLocalSetMirror(enabled);
      if (!alive.current) return;
      setState(next);
      setNotice(enabled ? text("已开启：之后的模型下载经 hf-mirror.com 进行。", "On: later model downloads go through hf-mirror.com.") : text("已关闭镜像下载。", "Mirror download is off."));
    } catch (e) { if (alive.current) setError(String(e)); }
    finally { acting.current = false; if (alive.current) setBusy(false); }
  }
  async function copyDiagnostics() {
    try {
      await navigator.clipboard.writeText(JSON.stringify({ phase: state?.phase, errorCode: state?.errorCode, error: state?.error, elapsedSeconds: state?.elapsedSeconds, logs: state?.logs }, null, 2));
      setNotice(text("诊断记录已复制。", "Diagnostics copied."));
    } catch { setError(text("无法复制，请展开运行记录手动复制。", "Copy failed. Expand the log and copy it manually.")); }
  }
  // Anything that may download, or overwrite a remote decision service / stored key, goes through the
  // confirmation dialog. Only an installed environment with nothing to replace starts directly.
  function request(kind: Pending["kind"]) {
    if ((kind === "launch" || kind === "use") && !replaces) { void action(kind); return; }
    setPending({ kind });
  }
  const canLaunch = !!state?.installed && state.errorCode !== "install";
  const setupText = text(`将先检查并复用已有环境。缺少 Python 时，将从 python.org 下载约 26 MB 的 Python 3.12 安装包，核对 SHA-256 与发布者签名后安装到当前用户（会出现在系统「应用和功能」中，需在那里单独卸载）；随后按锁定的版本安装 CPU PyTorch 与 Laya（每个文件核对 SHA-256），并从${state?.mirror ? "镜像 hf-mirror.com" : " Hugging Face "}下载模型（核对 SHA-256）。需要联网，首次安装约占 3.5 GB 磁盘空间，可在运行记录中查看进度并取消。修复会创建新环境并保留原环境。`, `Existing environments will be checked and reused. If Python is missing, a roughly 26 MB Python 3.12 installer will be downloaded from python.org, its SHA-256 and publisher signature checked, and installed for the current user (it appears in Windows Apps & features and is removed there separately). CPU PyTorch and Laya are then installed at locked versions (every file's SHA-256 checked), and the model is downloaded from ${state?.mirror ? "the hf-mirror.com mirror" : "Hugging Face"} (SHA-256 checked). Internet access is needed; first setup takes about 3.5 GB of disk. You can view progress and cancel. Repair creates a new environment and retains the previous one.`);
  const replaceText = text("完成后会把本分类的判断服务改为本机 Laya，替换当前填写的接口，并清除已保存的判断服务密钥。", "When done, this category's decision service becomes the local Laya: the current endpoint is replaced and the stored decision credential is cleared.");
  const setupKind = pending?.kind === "start" || pending?.kind === "repair";
  return <section className="space-y-3 rounded-control border border-border p-3" aria-busy={busy}>
    {pending && <ConfirmDialog
      tone={replaces ? "danger" : "primary"}
      title={pending.kind === "repair" ? text("修复本地服务环境", "Repair local service") : setupKind ? text("安装并启动本地服务", "Set up local service") : text("替换当前判断服务", "Replace the current decision service")}
      description={<>{setupKind && <span className="block">{setupText}</span>}{replaces && <span className="mt-2 block font-medium text-text-primary">{replaceText}</span>}<span className="mt-2 block">{text("是否继续？", "Continue?")}</span></>}
      confirmLabel={setupKind ? text("确认，自动安装并运行", "Confirm, install and run") : text("确认替换", "Replace")}
      cancelLabel={text("取消", "Cancel")}
      onCancel={() => setPending(null)}
      onConfirm={() => { const kind = pending.kind; setPending(null); void action(kind); }}
    />}
    <div className="flex flex-wrap items-center justify-between gap-2">
      <h3 className="text-sm font-medium text-text-primary">{text("Laya 本地服务", "Local Laya service")}</h3>
      <span role="status" className="text-xs text-text-secondary">{state ? phases[state.phase] ?? state.phase : text("等待连接桌面服务…", "Waiting for desktop service…")}</span>
    </div>
    <p className="text-xs leading-relaxed text-text-secondary">{text("先检查已有环境，可用则直接启动。首次安装需要联网，约占 3.5 GB 磁盘空间。服务就绪后自动保存本地接口；运行方式保持你的选择。退出 SynaRoute 时服务停止。", "Existing environments are checked and reused. First setup needs internet access and takes about 3.5 GB of disk. The local endpoint is saved once ready; your selected mode is preserved. The service stops when SynaRoute exits.")}</p>
    <p className="text-xs leading-relaxed text-text-muted">{text("判断方式：Laya 只判断「这个任务是否需要深入推理」，判定需要时把思考强度提高到 high，从不降低。在 36 条中英任务的留出测试中，需要深入推理的 12 条里提高了 4 条，其余 24 条里误提高 1 条；未提高时保持原有设置。约 1000 token 以上的输入会截断，此时保持原有设置。运行时常驻约 1.6 GB 内存。建议先用「仅建议」观察运行日志。", "How it judges: Laya only decides whether a task needs deep reasoning; when it does, effort is raised to high, never lowered. On a held-out set of 36 Chinese and English tasks it raised 4 of the 12 that needed deep reasoning and wrongly raised 1 of the other 24; otherwise the existing setting is kept. Inputs above about 1,000 tokens are truncated, and the existing setting is kept. While running it keeps about 1.6 GB of memory. Start with Suggest and check the activity log.")}</p>
    {working && state?.phase !== "running" && state?.phase !== "stopping" && <p className="text-xs text-text-muted">{text(`已用时 ${state?.elapsedSeconds ?? 0} 秒。环境检查最多 2 分钟，Python 安装 15 分钟，依赖安装 30 分钟，模型加载 20 分钟；可随时取消。`, `Elapsed: ${state?.elapsedSeconds ?? 0}s. Limits: environment check 2 min, Python 15 min, dependencies 30 min, model loading 20 min. Cancel at any time.`)}</p>}
    {state?.phase === "stopping" && <p className="text-xs text-text-muted">{text("中途打断 Python 安装程序可能留下装了一半的 Python，因此会等它结束后再停止（最多 10 分钟，超时则强制停止）。", "Interrupting the Python installer can leave a half-installed Python, so the stop waits for it to finish (up to 10 minutes, then it is forced).")}</p>}
    {state && !state.supported ? <p className="text-xs text-text-muted">{text("一键部署目前支持 Windows，其他系统可手动连接 Laya 服务。", "One-click setup currently supports Windows; other systems can connect to Laya manually.")}</p> : <div className="flex flex-wrap gap-2">
      <Button size="sm" disabled={!state || busy || working} onClick={() => request(canLaunch ? "launch" : "start")}>{state?.phase === "failed" ? text("重试", "Retry") : canLaunch ? text("启动本地服务", "Start local service") : text("一键安装并运行", "Install and run")}</Button>
      {working && <Button size="sm" variant="secondary" disabled={busy || state?.phase === "stopping"} onClick={() => void action("stop")}>{state?.phase === "running" ? text("停止", "Stop") : text("取消", "Cancel")}</Button>}
      {!working && !!state?.supported && <Button size="sm" variant="secondary" disabled={busy} onClick={() => request("repair")}>{text("修复环境并运行", "Repair and run")}</Button>}
      {state?.phase === "running" && <Button size="sm" variant="secondary" disabled={busy} onClick={() => request("use")}>{text("使用此本地服务", "Use this local service")}</Button>}
    </div>}
    {!working && !!state?.supported && <p className="text-xs text-text-muted">{text("修复会重新下载依赖并创建独立环境（约 1.5 GB），保留原环境和模型缓存。", "Repair downloads dependencies into a separate environment (about 1.5 GB), retaining the previous environment and model cache.")}</p>}
    {!!state?.supported && <ToggleRow
      title={text("随 SynaRoute 自动启动本地服务", "Start the local service with SynaRoute")}
      desc={text("默认关闭。开启后，每次启动 SynaRoute 时自动启动已安装的本地服务，并使用上次运行成功的端口；环境未安装或该端口被占用时不会启动，也不会下载或安装任何内容。本机设置，不随配置导出。", "Off by default. When on, each SynaRoute launch starts the installed local service on the port it last ran on. It does not start if the environment is not installed or that port is taken, and never downloads or installs anything. Machine-local; not included in configuration export.")}
      cost={text("开启后每次启动都会常驻约 1.6 GB 内存。", "When on, about 1.6 GB of memory stays in use after every launch.")}
      checked={!!state?.autostart}
      disabled={busy}
      onChange={(v) => void setAutostart(v)}
    />}
    {!!state?.supported && <ToggleRow
      title={text("通过镜像 hf-mirror.com 下载模型", "Download the model through hf-mirror.com")}
      desc={text("默认关闭。无法访问 Hugging Face 时可开启：之后的模型下载改从镜像站进行，下载的文件仍逐个核对已验证的 SHA-256 后才加载；Python 与依赖的下载来源不变，已下载的模型不会重新下载。本机设置，不随配置导出。", "Off by default. Turn on when Hugging Face is unreachable: later model downloads come from the mirror, and each downloaded file is still checked against its verified SHA-256 before loading. Python and dependency sources do not change, and an already downloaded model is not downloaded again. Machine-local; not included in configuration export.")}
      cost={text("hf-mirror.com 是第三方站点，会看到模型下载请求。", "hf-mirror.com is a third-party site and sees the model download requests.")}
      checked={!!state?.mirror}
      disabled={busy}
      onChange={(v) => void setMirror(v)}
    />}
    {(error || state?.error) && <div role="alert" className="space-y-1 break-words text-xs text-danger"><p>{error || remedies[state?.errorCode ?? ""] || state?.error}</p>{state?.error && <details><summary className="cursor-pointer">{text("错误详情", "Error details")}</summary><p className="mt-1">{state.error}</p></details>}</div>}
    {notice && <p role="status" className="text-xs text-text-secondary">{notice}</p>}
    {!!state?.logs.length && <details><summary className="cursor-pointer text-xs text-text-secondary">{text("安装与运行记录", "Installation and service log")}</summary><pre className="mt-2 max-h-48 overflow-auto whitespace-pre-wrap break-all text-xs text-text-muted">{state.logs.join("\n")}</pre><Button size="sm" variant="secondary" onClick={() => void copyDiagnostics()}>{text("复制诊断记录", "Copy diagnostics")}</Button></details>}
  </section>;
}
