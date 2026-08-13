import { usePolling } from "@/lib/usePolling";
import { useEffect, useState } from "react";
import { useStore } from "@/store";
import { api } from "@/lib/bridge";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/Card";
import { Switch } from "@/components/ui/Switch";
import { Button } from "@/components/ui/Button";
import { useT } from "@/lib/useT";
import { LANGS } from "@/lib/i18n";
import { openLogDir } from "@/lib/openLogDir";
import { pickPrefs } from "@/lib/prefs";
import type {
  AppSettings,
  McpStatus,
  ThemePref,
  CategoryType,
  ExportOutcome,
  ImportMode,
  ImportPreview,
  ImportReport,
  MasterPasswordState,
} from "@/types";
import {
  // 别名 SettingsIcon：与本页组件名 SettingsPage 不冲突，但直接叫 Settings 容易看混
  Settings as SettingsIcon,
  Sun, Moon, Monitor, ShieldCheck, KeyRound, Languages, ScrollText,
  Activity, RefreshCw, FolderOpen, Info, Plug, BookOpen, Copy, Check, X, Brain, MousePointerClick, MessageSquareDot, Timer,
  Download, Upload, AlertTriangle, type LucideIcon,
} from "lucide-react";

/** 设置页（主题 / 语言 / 自启 / 加密方式 / 局域网暴露 / 版本更新 / 日志路径） */
export function SettingsPage() {
  // 细粒度订阅（勿改回整店解构）：见 ProxyStatusBar 同处注释。
  const theme = useStore((s) => s.theme);
  const setTheme = useStore((s) => s.setTheme);
  const lang = useStore((s) => s.lang);
  const setLang = useStore((s) => s.setLang);
  const showToast = useStore((s) => s.showToast);
  const activeCategory = useStore((s) => s.activeCategory);
  const refreshCategory = useStore((s) => s.refreshCategory);
  const loadCategory = useStore((s) => s.loadCategory);
  const loadSettings = useStore((s) => s.loadSettings);
  const t = useT();
  const [settings, setSettings] = useState<AppSettings | null>(null);
  const [version, setVersion] = useState<string>("");
  const [updateMsg, setUpdateMsg] = useState<string | null>(null);
  const [installing, setInstalling] = useState(false);
  const updateCheck = useStore((s) => s.updateCheck);
  const updateChecking = useStore((s) => s.updateChecking);
  const checkForUpdates = useStore((s) => s.checkForUpdates);
  const [defaultLogDir, setDefaultLogDir] = useState<string>("");
  const [mcp, setMcp] = useState<McpStatus | null>(null);
  const [showWizard, setShowWizard] = useState(false);
  // 配置导入导出（FR-021）
  const [exportOpen, setExportOpen] = useState(false);
  const [importState, setImportState] = useState<{ path: string; preview: ImportPreview } | null>(null);
  // 主口令增强模式（FR-018 可选增强）。master 是后端真实状态（判据是密钥库里有无 master
  // 头部），不是 settings.masterPasswordEnabled 那个镜像字段。
  const [master, setMaster] = useState<MasterPasswordState | null>(null);
  const [masterDialog, setMasterDialog] = useState<"enable" | "disable" | "unlock" | "change" | null>(null);
  // 孤儿密钥（UX#19 / P2-3）：密钥库里有、配置里已无对应 Key 的残留。
  // `null` = 还没查过（不显示这一行）；`0` = 查过且干净（也不显示，没必要占位置）。
  const [orphanCount, setOrphanCount] = useState<number | null>(null);
  const [pruning, setPruning] = useState(false);
  const [pruneConfirm, setPruneConfirm] = useState(false);

  /** 重新拉主口令状态。任何切换/解锁后都要调，UI 才不会显示陈旧态。 */
  const reloadMaster = async () => {
    try {
      setMaster(await api.getMasterPasswordState());
    } catch (e) {
      console.error("getMasterPasswordState failed", e);
    }
  };

  /**
   * 重查孤儿密钥条数。
   *
   * 锁定态下后端一律返回 0（`Store::count_orphan_secrets` 主动跳过）——那不是「确实没有」，
   * 而是「读不到所以不敢下结论」。故 UI 这一行只在**已解锁**时才有意义，
   * 否则会给出「已清理干净」的错误暗示。
   */
  const reloadOrphans = async () => {
    try {
      setOrphanCount(await api.countOrphanSecrets());
    } catch (e) {
      console.error("countOrphanSecrets failed", e);
    }
  };

  /**
   * 执行清理。**破坏性且不可逆**，故先要二次确认。
   *
   * 后端在删之前会整库备份（备份失败即放弃，见 `service::prune_orphan_secrets`），
   * 这里把「已备份」写进成功提示——用户需要知道还有退路，否则不敢点这个按钮。
   */
  const handlePrune = async () => {
    setPruning(true);
    try {
      const n = await api.pruneOrphanSecrets();
      showToast("success", t("settings.orphanPruned", { n }));
      setPruneConfirm(false);
      await reloadOrphans();
    } catch (e) {
      showToast("error", String((e as Error)?.message ?? e));
    } finally {
      setPruning(false);
    }
  };

  /** 立即锁定：不需要口令，故不走对话框。 */
  const lockNow = async () => {
    try {
      await api.lockMasterPassword();
      showToast("success", t("master.locked"));
      await reloadMaster();
    } catch (e) {
      showToast("error", String((e as Error)?.message ?? e));
    }
  };

  /** 选文件 + 校验 + 预检（不改任何配置）。校验不过时直接弹错，不进确认弹窗。 */
  const startImport = async () => {
    try {
      const picked = await api.pickAndPreviewImport();
      if (!picked) return; // 用户取消
      const [path, preview] = picked;
      setImportState({ path, preview });
    } catch (e) {
      // 版本不符 / 校验和不匹配 / 不是导出文件，都在这里——后端消息已含行动指引。
      showToast("error", String((e as Error)?.message ?? e));
    }
  };

  /** 导入成功后刷新全局状态：Key 列表、设置、当前分类都变了。 */
  const reloadAfterImport = async () => {
    try {
      // 走 store 的 loadSettings 而不是只 setSettings 局部：它会一并同步 store.settings /
      // theme / lang 并 applyTheme。只刷局部的话，导入进来的主题不生效，且 store 里那份
      // 陈旧快照会在下次切主题时把刚导入的设置顶回去（见 store.ts persistOneSetting 注释）。
      await loadSettings();
      setSettings(useStore.getState().settings);
      await loadCategory(activeCategory);
      await refreshCategory();
      await reloadMaster(); // 导入含密钥段时锁定态可能变
      // Replace 导入会移除本机独有的 Key，其密钥由后端一并清理；Merge 不会。
      // 无论哪种模式都重查一次：这是孤儿数最可能变化的时刻。
      await reloadOrphans();
    } catch (e) {
      console.error("reload after import failed", e);
      showToast("error", String((e as Error)?.message ?? e));
    }
  };

  useEffect(() => {
    void api.getSettings().then(setSettings);
    void api.getAppVersion().then(setVersion);
    void api.getDefaultLogDir().then(setDefaultLogDir);
    void reloadMaster();
    void reloadOrphans();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // 解锁后重查孤儿数：锁定态下后端返回的 0 是「读不到」而非「没有」（见 reloadOrphans）。
  // 不重查会让刚解锁的用户看不到本该提示的清理入口，直到他离开再回到本页。
  const vaultLocked = master?.locked ?? false;
  useEffect(() => {
    if (!vaultLocked) void reloadOrphans();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [vaultLocked]);

  // MCP 运行状态轮询：开启时每 3s 刷新一次，展示运行中/端口/故障原因。
  const mcpEnabled = !!settings?.mcpEnabled;
  useEffect(() => {
    if (!mcpEnabled) setMcp(null);
  }, [mcpEnabled]);
  // `enabled` 跟随 MCP 开关；窗口不可见时停表（见 usePolling）。
  //
  // **刻意保留 3s、不改成 30s 兜底**（UX#5 的例外）：MCP 是独立的服务进程，后端没有它的
  // 状态变更信号可推；而这个数字恰恰是用户刚点完开关后**盯着看**的东西
  //（「启动中」→「运行中」，或启动失败的原因）。拉长到 30s 会把一个即时反馈变成
  // 「点了没反应」。它只在 MCP 开着时才跑，代价可控。
  usePolling(
    () => {
      void api.mcpStatus().then((s) => setMcp(s)).catch(() => {
        // 状态读不到不清空已有显示：一次 IPC 抖动不该让界面闪成「未运行」
      });
    },
    3000,
    mcpEnabled,
  );

  const update = (patch: Partial<AppSettings>) => {
    if (!settings) return;
    const prev = settings;
    // 乐观更新：先用本页当前快照 + patch 立刻反映到 UI，不等 IPC 往返。
    setSettings({ ...settings, ...patch });
    // 落盘前**重新拉一次磁盘最新值**再合并 patch —— 不能用本页挂载时的旧 `settings` 当基线。
    //
    // 本页的 `settings` 只在挂载时 `getSettings()` 拉过一次。若用户先切了主题/语言
    // （store.ts 的 setTheme/setLang 走 persistOneSetting，各自都会重拉磁盘最新值再落盘，
    // 绕过本页状态），本页这份快照就带着**旧的 theme/language**。此时再点本页任意一个开关，
    // 若直接拿本页旧快照 `{...settings, ...patch}` 提交，就会把刚落盘的新主题/语言顶回旧值——
    // 用户会看到「明明选了深色，点了个不相关的开关后又变回浅色」，且没有任何报错。
    // 与 persistOneSetting 同一套写法：先拉最新、再合并、再落盘，避免用旧快照覆盖新值。
    void api
      .getSettings()
      .then((latest) => {
        const merged = { ...latest, ...patch };
        return api.saveSettings(pickPrefs(merged)).then(() => merged);
      })
      .then((merged) => {
        // 用合并后的值同步两份状态：本页快照（继续给用户看）与全局 store
        // （store.settings 是切主题/语言时的落盘基线之一，留着旧值会反过来把这次的改动顶回去）。
        setSettings(merged);
        useStore.setState({ settings: merged });
      })
      .catch((e) => {
        console.error("saveSettings failed", e);
        setSettings(prev);
        showToast("error", String((e as Error)?.message ?? e));
      });
  };

  /**
   * 开机自启动走**专用命令**，不走 update()。
   *
   * 它伴随系统副作用（写注册表 Run 键），而 update() 提交的是本页局部 state 的整份快照 ——
   * 那正是出过 P0 的地方：切主题/切语言会把用户刚关掉的自启动重新装回系统。
   * 现在 `autoStart` 根本不在 `saveSettings` 的入参类型里，物理上走不到那条路。
   *
   * 失败必须回滚开关：UI 显示已开、系统实际没开是这类设置最坑的形态
   * （用户以为开成了，重启后什么也没发生）。
   */
  const handleToggleAutoStart = (v: boolean) => {
    if (!settings) return;
    const prev = settings;
    setSettings({ ...settings, autoStart: v });
    void api
      .setAutoStart(v)
      .then(() => {
        useStore.setState({ settings: { ...prev, autoStart: v } });
      })
      .catch((e) => {
        console.error("setAutoStart failed", e);
        setSettings(prev);
        showToast("error", String((e as Error)?.message ?? e));
      });
  };

  /**
   * 悬浮窗开关。与自启动同样走**专用命令**，不走 update()。
   *
   * 理由完全相同：它带窗口副作用（建/销毁一个 WebView），而 update() 提交的是本页
   * 局部 state 的整份快照 —— 那正是出过 P0 的地方（切主题/语言会把用户刚关掉的
   * 开关重新打开）。`floatingWidgetEnabled` 也不在 `saveSettings` 的入参类型里，
   * 物理上走不到那条路。
   *
   * 失败必须回滚开关：UI 显示已开、实际没开是这类设置最坑的形态。
   */
  const handleToggleFloating = (v: boolean) => {
    if (!settings) return;
    const prev = settings;
    setSettings({ ...settings, floatingWidgetEnabled: v });
    void api
      .setFloatingWidget(v)
      .then(() => {
        useStore.setState({ settings: { ...prev, floatingWidgetEnabled: v } });
      })
      .catch((e) => {
        console.error("setFloatingWidget failed", e);
        setSettings(prev);
        showToast("error", String((e as Error)?.message ?? e));
      });
  };

  /**
   * 悬浮球置顶开关。同上走专用命令、失败回滚。
   *
   * 单独开这个开关是因为原先置顶是**写死**的（`always_on_top(true)`），
   * 于是用别的软件时悬浮球一直盖在最上面挡着。置顶对「瞥一眼状态」有用，
   * 但该由用户自己选，默认关。
   */
  const handleToggleFloatingPinned = (v: boolean) => {
    if (!settings) return;
    const prev = settings;
    setSettings({ ...settings, floatingWidgetAlwaysOnTop: v });
    void api
      .setFloatingPinned(v)
      .then(() => {
        useStore.setState({ settings: { ...prev, floatingWidgetAlwaysOnTop: v } });
      })
      .catch((e) => {
        console.error("setFloatingPinned failed", e);
        setSettings(prev);
        showToast("error", String((e as Error)?.message ?? e));
      });
  };

  // MCP 开关/端口走专用命令：携带当前活跃分类，后端据此自动注册 synaroute 到对应工具客户端
  // （~/.claude.json 或 ~/.codex/config.toml），并在端口漂移时重写。不走通用 saveSettings，
  // 因为那拿不到 activeCategory。
  const applyMcp = (enabled: boolean, port: number) => {
    if (!settings) return;
    setSettings({ ...settings, mcpEnabled: enabled, mcpPort: port });
    void api
      .setMcpEnabled(activeCategory, enabled, port)
      .then((s) => setMcp(s))
      .catch((e) => {
        console.error("setMcpEnabled failed", e);
        showToast("error", String((e as Error)?.message ?? e));
      });
  };

  const handleCheckUpdate = async () => {
    setUpdateMsg(null);
    const result = await checkForUpdates({ silent: true });
    if (!result) return;
    if (result.status === "available") {
      setUpdateMsg(t("settings.updateAvailable", { version: result.version ?? "?" }));
    } else if (result.status === "up_to_date") {
      setUpdateMsg(t("settings.upToDate"));
    } else {
      setUpdateMsg(t("settings.updateError", { err: result.error ?? "unknown" }));
    }
  };

  const handleInstallUpdate = async () => {
    setInstalling(true);
    setUpdateMsg(null);
    try {
      const msg = await api.installUpdate();
      setUpdateMsg(msg);
      showToast("success", msg);
    } catch (e) {
      const err = String((e as Error)?.message ?? e);
      setUpdateMsg(t("settings.updateInstallError", { err }));
      showToast("error", err);
    } finally {
      setInstalling(false);
    }
  };

  const handlePickLogDir = async () => {
    const dir = await api.pickDirectory();
    if (dir) update({ logDir: dir });
  };

  // **实际生效**的日志目录：用户配了就是用户的，否则是默认目录。
  // 直接推导而非另开一个 IPC + state：这样改完目录立刻同步，不会出现「显示的还是旧路径」。
  // 显示它是必需的——MSIX 虚拟化下「按钮打开的目录」可能是包内私有副本，
  // 用户得能核对自己看的是哪一份（CLAUDE.md 平行宇宙惨案的复发防线）。
  const effectiveLogDir = settings?.logDir?.trim() || defaultLogDir;

  /**
   * 打开日志目录（UX#13）。实现在 `lib/openLogDir`，与命令面板共用一条 ——
   * 两份拷贝会漂移，而漂移在这里是隐形的（详见该文件注释）。
   */
  const handleOpenLogDir = async () => {
    try {
      await openLogDir();
    } catch (e) {
      showToast("error", String((e as Error)?.message ?? e));
    }
  };

  /** 导出诊断报告（UX#12）。用户取消保存对话框时后端返回 null，不提示错误。 */
  const handleExportDiagnostics = async () => {
    try {
      const path = await api.exportDiagnostics();
      if (path) showToast("success", t("settings.diagSaved", { path }));
    } catch (e) {
      showToast("error", String((e as Error)?.message ?? e));
    }
  };

  // MCP 接入地址：优先用实际绑定端口（占用时会 fallback），否则用配置端口。
  const mcpPort = mcp?.port ?? settings?.mcpPort ?? 9527;
  const mcpAddress = `http://127.0.0.1:${mcpPort}/mcp`;
  const [copied, setCopied] = useState(false);
  const handleCopyAddr = () => {
    void navigator.clipboard?.writeText(mcpAddress).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 1400);
    });
  };

  // MCP 重启：先停后起、强制重绑端口。用于端口变更/冲突排障/强制重连。
  // 大脑聚合参数是每次调用实时读的，改了保存即生效，不需要走这里。
  const [restarting, setRestarting] = useState(false);
  const handleRestartMcp = () => {
    setRestarting(true);
    void api
      .restartMcp()
      .then((s) => {
        setMcp(s);
        showToast("success", t("settings.mcpRestarted", { port: String(s.port ?? mcpPort) }));
      })
      .catch((e) => {
        console.error("restartMcp failed", e);
        showToast("error", String((e as Error)?.message ?? e));
      })
      .finally(() => setRestarting(false));
  };

  // 各分类代理端口的草稿输入（受控），失焦/回车时提交 setProxyPort（落盘+重启代理+重写客户端配置）。
  const [portDrafts, setPortDrafts] = useState<Record<string, string>>({});
  const [savingPort, setSavingPort] = useState<string | null>(null);
  const PROXY_CATS: { id: CategoryType; def: number }[] = [
    { id: "claude-cli", def: 47100 },
    { id: "codex", def: 47101 },
    { id: "claude-desktop", def: 47102 },
  ];
  const commitProxyPort = (cat: CategoryType, raw: string, def: number) => {
    const port = Number(raw);
    if (!Number.isInteger(port) || port < 1024 || port > 65535) {
      showToast("error", t("settings.proxyPortInvalid"));
      setPortDrafts((d) => ({ ...d, [cat]: String(settings?.proxyPorts?.[cat] ?? def) }));
      return;
    }
    if (port === (settings?.proxyPorts?.[cat] ?? def)) return; // 未变，跳过
    setSavingPort(cat);
    void api
      .setProxyPort(cat, port)
      .then((st) => {
        const bound = st.port ?? port;
        setSettings((s) => (s ? { ...s, proxyPorts: { ...(s.proxyPorts ?? {}), [cat]: bound } } : s));
        setPortDrafts((d) => ({ ...d, [cat]: String(bound) }));
        // 文案按**实际运行态**分流：后端对未运行的分类只落盘、不启动、不碰客户端配置
        // （service::set_proxy_port 的 was_running=false 分支），此时回报 status="stopped"。
        // 若不分流、一律说「已重写客户端配置，客户端需重启生效」，用户会去重启 claude/codex
        // 期待生效——而配置一个字节没动、代理也没跑，重启毫无效果，且他不知道还得回来点「启动」。
        // 这正是后端注释极力避免的「界面说 A、实际 B」，不能被这句 toast 重新引入。
        const running = st.status === "running";
        showToast(
          "success",
          running
            ? t("settings.proxyPortSaved", { port: String(bound) })
            : t("settings.proxyPortSavedStopped", { port: String(bound) }),
        );
      })
      .catch((e) => {
        console.error("setProxyPort failed", e);
        showToast("error", String((e as Error)?.message ?? e));
      })
      .finally(() => setSavingPort(null));
  };

  const themeOptions: { value: ThemePref; tKey: string; icon: LucideIcon }[] = [
    { value: "light", tKey: "settings.theme.light", icon: Sun },
    { value: "dark", tKey: "settings.theme.dark", icon: Moon },
    { value: "system", tKey: "settings.theme.system", icon: Monitor },
  ];

  return (
    <div className="h-full overflow-y-auto">
      <div className="border-b border-border px-6 py-4">
        {/* 标题配图标（UI-1）：与 BrainPage / LogsPage 同一形制。 */}
        <div className="flex items-center gap-2">
          <SettingsIcon size={20} className="text-primary" />
          <h1 className="text-lg font-semibold text-text-primary">{t("settings.title")}</h1>
        </div>
      </div>

      <div className="space-y-4 p-6">
        {/* 版本与更新 */}
        <Card>
          <CardHeader>
            <CardTitle>{t("settings.about")}</CardTitle>
          </CardHeader>
          <CardContent>
            <div className="flex items-start justify-between gap-4">
              <div className="flex min-w-0 flex-1 gap-2.5">
                <Info size={16} className="mt-0.5 shrink-0 text-text-secondary" />
                <div className="min-w-0">
                  <div className="flex flex-wrap items-center gap-2 text-sm font-medium text-text-primary">
                    <span>SynaRoute v{version}</span>
                    {updateCheck?.status === "available" && (
                      <span className="rounded-full bg-primary/15 px-2 py-0.5 text-[11px] font-medium text-primary">
                        {t("settings.updateAvailable", { version: updateCheck.version ?? "?" })}
                      </span>
                    )}
                  </div>
                  {(updateMsg || updateCheck?.error) && (
                    <div
                      className={`mt-1 whitespace-pre-wrap text-xs ${
                        updateCheck?.status === "error" ? "text-danger" : "text-text-muted"
                      }`}
                    >
                      {updateMsg ?? updateCheck?.error}
                    </div>
                  )}
                  {updateCheck?.status === "available" && updateCheck.notes && (
                    <div className="mt-1 max-h-24 overflow-y-auto whitespace-pre-wrap text-[11px] text-text-muted">
                      {updateCheck.notes}
                    </div>
                  )}
                </div>
              </div>
              <div className="flex shrink-0 flex-col gap-1.5 sm:flex-row">
                <Button
                  size="sm"
                  variant="secondary"
                  onClick={() => void handleCheckUpdate()}
                  disabled={updateChecking || installing}
                >
                  <RefreshCw size={14} className={updateChecking ? "animate-spin" : ""} />
                  {t("settings.checkUpdate")}
                </Button>
                {updateCheck?.status === "available" && (
                  <Button
                    size="sm"
                    onClick={() => void handleInstallUpdate()}
                    disabled={installing || updateChecking}
                  >
                    <RefreshCw size={14} className={installing ? "animate-spin" : ""} />
                    {t("settings.installUpdate")}
                  </Button>
                )}
              </div>
            </div>
          </CardContent>
        </Card>
        {/* 主题 */}
        <Card>
          <CardHeader>
            <CardTitle>{t("settings.appearance")}</CardTitle>
          </CardHeader>
          <CardContent>
            <div className="flex gap-2">
              {themeOptions.map((o) => {
                const Icon = o.icon;
                const active = theme === o.value;
                return (
                  <button
                    key={o.value}
                    onClick={() => setTheme(o.value)}
                    className={`flex flex-1 flex-col items-center gap-1.5 rounded-control border p-3 transition-colors ${
                      active ? "border-primary bg-primary/8 text-primary" : "border-border text-text-secondary hover:bg-surface-hover"
                    }`}
                  >
                    <Icon size={18} />
                    <span className="text-xs">{t(o.tKey)}</span>
                  </button>
                );
              })}
            </div>
          </CardContent>
        </Card>

        {/* 语言 */}
        <Card>
          <CardHeader>
            <CardTitle>{t("settings.language")}</CardTitle>
          </CardHeader>
          <CardContent>
            <div className="flex items-start justify-between gap-4">
              <div className="flex gap-2.5">
                <Languages size={16} className="mt-0.5 shrink-0 text-text-secondary" />
                <div className="text-xs text-text-muted">{t("settings.languageDesc")}</div>
              </div>
              <div className="flex gap-2">
                {LANGS.map((l) => {
                  const active = lang === l.value;
                  return (
                    <button
                      key={l.value}
                      onClick={() => setLang(l.value)}
                      className={`rounded-control border px-3 py-1.5 text-xs transition-colors ${
                        active ? "border-primary bg-primary/12 text-primary" : "border-border text-text-secondary hover:bg-surface-hover"
                      }`}
                    >
                      {l.label}
                    </button>
                  );
                })}
              </div>
            </div>
          </CardContent>
        </Card>

        {/* 安全 / 加密 */}
        <Card>
          <CardHeader>
            <CardTitle>{t("settings.security")}</CardTitle>
          </CardHeader>
          <CardContent className="space-y-3">
            {/* 主口令增强（FR-018 可选增强）。开关不直接落盘——启用/关闭都要整库迁移密钥，
                必须走对话框收口令。checked 取自后端真实状态（密钥库里有无 master 头部），
                不看 settings 那个镜像字段。 */}
            <ToggleRow
              icon={ShieldCheck}
              title={t("settings.masterPwTitle")}
              desc={t("settings.masterPwDesc")}
              checked={master?.enabled ?? false}
              onChange={(v) => setMasterDialog(v ? "enable" : "disable")}
            />
            {master?.enabled && (
              <div className="ml-[26px] flex flex-wrap items-center gap-2">
                {master.locked ? (
                  <>
                    <span className="rounded-control bg-warning/12 px-2 py-0.5 text-[11px] text-warning">
                      ● {t("master.lockedBadge")}
                    </span>
                    <Button size="sm" variant="secondary" onClick={() => setMasterDialog("unlock")}>
                      {t("master.unlockAction")}
                    </Button>
                  </>
                ) : (
                  <Button size="sm" variant="outline" onClick={() => void lockNow()}>
                    {t("master.lockNow")}
                  </Button>
                )}
                <Button size="sm" variant="outline" onClick={() => setMasterDialog("change")}>
                  {t("master.changeAction")}
                </Button>
              </div>
            )}
            <ToggleRow
              icon={KeyRound}
              title={t("settings.lanTitle")}
              desc={t("settings.lanDesc")}
              checked={settings?.lanExposure ?? false}
              onChange={(v) => update({ lanExposure: v })}
              danger
            />

            {/* 孤儿密钥清理（UX#19 / P2-3）。
                **只在有孤儿且已解锁时才出现**：没有残留就不该占位置；锁定态下后端返回的 0
                是「读不到所以不敢下结论」，此时显示这一行会给出「已清理干净」的错误暗示。

                刻意不做启动时自动清理：删密钥不可逆，而残留孤儿本身是无害的（只占空间），
                不值得用「自动删」去换那点整洁——一旦删错，用户没有任何补救机会。 */}
            {!vaultLocked && orphanCount !== null && orphanCount > 0 && (
              <div className="rounded-control border border-warning/40 bg-warning/8 p-2.5">
                <div className="flex items-start gap-2">
                  <AlertTriangle size={14} className="mt-0.5 shrink-0 text-warning" />
                  <div className="min-w-0 flex-1">
                    <div className="text-sm font-medium text-text-primary">
                      {t("settings.orphanTitle", { n: orphanCount })}
                    </div>
                    <div className="mt-0.5 text-xs leading-relaxed text-text-muted">
                      {t("settings.orphanDesc")}
                    </div>
                    {pruneConfirm ? (
                      <div className="mt-2 space-y-1.5">
                        {/* 确认文案必须说清「不可逆」与「已备份」两件事：
                            只说不可逆用户不敢点，只说已备份用户会以为随手可撤。 */}
                        <div className="text-xs font-medium text-danger">
                          {t("settings.orphanConfirm", { n: orphanCount })}
                        </div>
                        <div className="flex gap-2">
                          <Button size="sm" variant="danger" disabled={pruning} onClick={() => void handlePrune()}>
                            {pruning ? t("settings.orphanPruning") : t("settings.orphanConfirmYes")}
                          </Button>
                          <Button size="sm" variant="outline" disabled={pruning} onClick={() => setPruneConfirm(false)}>
                            {t("common.cancel")}
                          </Button>
                        </div>
                      </div>
                    ) : (
                      <Button size="sm" variant="outline" className="mt-2" onClick={() => setPruneConfirm(true)}>
                        {t("settings.orphanPrune")}
                      </Button>
                    )}
                  </div>
                </div>
              </div>
            )}
          </CardContent>
        </Card>

        {/* MCP 服务器 */}
        <Card>
          <CardHeader>
            <CardTitle>{t("settings.mcpTitle")}</CardTitle>
          </CardHeader>
          <CardContent className="space-y-3">
            <ToggleRow
              icon={Plug}
              title={t("settings.mcpEnable")}
              desc={t("settings.mcpEnableDesc")}
              checked={settings?.mcpEnabled ?? false}
              onChange={(v) => applyMcp(v, settings?.mcpPort ?? 9527)}
            />
            {settings?.mcpEnabled && (
              <>
                {/* 端口 */}
                <div className="flex items-start justify-between gap-4">
                  <div className="flex gap-2.5">
                    <Plug size={16} className="mt-0.5 shrink-0 text-text-secondary" />
                    <div>
                      <div className="text-sm font-medium text-text-primary">{t("settings.mcpPort")}</div>
                      <div className="text-xs text-text-muted">{t("settings.mcpPortDesc")}</div>
                    </div>
                  </div>
                  <input
                    type="number"
                    min={1024}
                    max={65535}
                    className="w-24 shrink-0 rounded-control border border-border bg-bg px-2.5 py-1.5 text-xs text-text-primary"
                    defaultValue={settings?.mcpPort ?? 9527}
                    onBlur={(e) => applyMcp(true, Number(e.target.value) || 9527)}
                  />
                </div>

                {/* 服务地址 + 复制 */}
                <div className="flex items-start justify-between gap-4">
                  <div className="flex gap-2.5">
                    <Activity size={16} className={`mt-0.5 shrink-0 ${mcp?.running ? "text-success" : "text-text-muted"}`} />
                    <div className="min-w-0">
                      <div className="text-sm font-medium text-text-primary">
                        {t("settings.mcpAddress")}
                        <span className={`ml-2 text-xs ${mcp?.running ? "text-success" : "text-text-muted"}`}>
                          {mcp?.running ? `● ${t("settings.mcpRunning")}` : `○ ${t("settings.mcpStopped")}`}
                        </span>
                      </div>
                      <div className="mt-1 break-all font-mono text-xs text-text-muted">{mcpAddress}</div>
                      {mcp?.running && mcp.port != null && mcp.port !== (settings?.mcpPort ?? 9527) && (
                        <div className="mt-1 text-xs text-warning">
                          {t("settings.mcpPortFallback", { port: mcp.port })}
                        </div>
                      )}
                      {mcp?.lastError && (
                        <div className="mt-1 text-xs text-danger">{mcp.lastError}</div>
                      )}
                    </div>
                  </div>
                  <Button size="sm" variant="secondary" onClick={handleCopyAddr}>
                    {copied ? <Check size={14} /> : <Copy size={14} />}
                    {copied ? t("settings.mcpCopied") : t("settings.mcpCopy")}
                  </Button>
                </div>

                <div className="flex flex-wrap gap-2">
                  <Button size="sm" variant="outline" onClick={() => setShowWizard(true)}>
                    <BookOpen size={14} /> {t("settings.mcpWizard")}
                  </Button>
                  <Button size="sm" variant="outline" onClick={handleRestartMcp} disabled={restarting}>
                    <RefreshCw size={14} className={restarting ? "animate-spin" : ""} /> {t("settings.mcpRestart")}
                  </Button>
                </div>
                <div className="flex items-start gap-1.5 text-xs text-text-muted">
                  <Info size={13} className="mt-0.5 shrink-0" />
                  <span>{t("settings.mcpRestartHint")}</span>
                </div>
              </>
            )}
          </CardContent>
        </Card>

        {/* 代理端口（每分类固定端口，可配；改后自动重启代理并重写客户端配置） */}
        <Card>
          <CardHeader>
            <CardTitle>{t("settings.proxyPortTitle")}</CardTitle>
          </CardHeader>
          <CardContent className="space-y-3">
            <div className="flex items-start gap-1.5 text-xs text-text-muted">
              <Info size={13} className="mt-0.5 shrink-0" />
              <span>{t("settings.proxyPortDesc")}</span>
            </div>
            {PROXY_CATS.map(({ id, def }) => {
              const current = settings?.proxyPorts?.[id] ?? def;
              const draft = portDrafts[id] ?? String(current);
              return (
                <div key={id} className="flex items-center justify-between gap-4">
                  <div className="flex gap-2.5">
                    <Plug size={16} className="mt-0.5 shrink-0 text-text-secondary" />
                    <div>
                      <div className="text-sm font-medium text-text-primary">{t(`nav.${id}`)}</div>
                      <div className="font-mono text-xs text-text-muted">http://127.0.0.1:{current}</div>
                    </div>
                  </div>
                  <input
                    type="number"
                    min={1024}
                    max={65535}
                    className="w-24 shrink-0 rounded-control border border-border bg-surface px-2.5 py-1.5 text-xs text-text-primary outline-none focus:border-primary disabled:opacity-50"
                    value={draft}
                    disabled={savingPort === id}
                    onChange={(e) => setPortDrafts((d) => ({ ...d, [id]: e.target.value }))}
                    onBlur={(e) => commitProxyPort(id, e.target.value, def)}
                    onKeyDown={(e) => {
                      if (e.key === "Enter") (e.target as HTMLInputElement).blur();
                    }}
                  />
                </div>
              );
            })}
          </CardContent>
        </Card>

        {/* 调试 / 可观测 */}
        <Card>
          <CardHeader>
            <CardTitle>{t("settings.debug")}</CardTitle>
          </CardHeader>
          <CardContent className="space-y-3">
            <ToggleRow
              icon={ScrollText}
              title={t("settings.reqLogTitle")}
              desc={t("settings.reqLogDesc")}
              cost={t("settings.reqLogCost")}
              checked={settings?.requestLogEnabled ?? false}
              onChange={(v) => update({ requestLogEnabled: v })}
            />
            <ToggleRow
              icon={ScrollText}
              title={t("settings.rawLogTitle")}
              desc={t("settings.rawLogDesc")}
              checked={settings?.logDownstreamRawEnabled ?? false}
              onChange={(v) => update({ logDownstreamRawEnabled: v })}
            />
            <div className="flex items-start justify-between gap-4">
              <div className="flex gap-2.5">
                <Activity size={16} className="mt-0.5 shrink-0 text-text-secondary" />
                <div>
                  <div className="text-sm font-medium text-text-primary">{t("settings.healthTitle")}</div>
                  <div className="text-xs text-text-muted">{t("settings.healthDesc")}</div>
                </div>
              </div>
              <select
                className="shrink-0 rounded-control border border-border bg-surface px-2.5 py-1.5 text-xs text-text-primary"
                value={settings?.healthCheckIntervalSecs ?? 60}
                onChange={(e) => update({ healthCheckIntervalSecs: Number(e.target.value) })}
              >
                {[0, 30, 60, 120, 300].map((s) => (
                  <option key={s} value={s}>
                    {t(`settings.health.${s}`)}
                  </option>
                ))}
              </select>
            </div>
            {/* 故障转移总预算（P1-1）：没有它时最坏耗时 = 候选数 × per-Key 超时
                （6 Key × 30s = 180s），而客户端早已超时重发，代理侧仍在逐个打上游烧额度。 */}
            <div className="flex items-start justify-between gap-4">
              <div className="flex gap-2.5">
                <Timer size={16} className="mt-0.5 shrink-0 text-text-secondary" />
                <div>
                  <div className="text-sm font-medium text-text-primary">
                    {t("settings.failoverBudgetTitle")}
                  </div>
                  <div className="text-xs text-text-muted">
                    {t("settings.failoverBudgetDesc")}
                  </div>
                </div>
              </div>
              <select
                className="shrink-0 rounded-control border border-border bg-surface px-2.5 py-1.5 text-xs text-text-primary"
                value={settings?.failoverTotalBudgetMs ?? 90000}
                onChange={(e) => update({ failoverTotalBudgetMs: Number(e.target.value) })}
              >
                {[0, 30000, 60000, 90000, 180000].map((ms) => (
                  <option key={ms} value={ms}>
                    {t(`settings.failoverBudget.${ms}`)}
                  </option>
                ))}
              </select>
            </div>
            <ToggleRow
              // MessageSquareDot：会话气泡 + 探测点，同时表达「发一条真实消息」与「在探测」。
              // 不用 Activity —— 上面「定时健康检查间隔」已占用它（同为健康检查族，图标相同会让
              // 两项在视觉上黏成一条，看不出这项才是「用真实请求」的开关）。
              icon={MessageSquareDot}
              title={t("settings.realProbeTitle")}
              desc={t("settings.realProbeDesc")}
              checked={settings?.healthProbeRealCompletion ?? false}
              onChange={(v) => update({ healthProbeRealCompletion: v })}
            />
            {settings?.healthProbeRealCompletion && (
              <div className="ml-8 space-y-1.5">
                <div className="text-sm font-medium text-text-primary">{t("settings.probeMsgTitle")}</div>
                <div className="text-xs text-text-muted">{t("settings.probeMsgDesc")}</div>
                <textarea
                  className="min-h-[80px] w-full resize-y rounded-control border border-border bg-surface px-2.5 py-1.5 font-mono text-xs text-text-primary placeholder:text-text-muted"
                  placeholder={t("settings.probeMsgPlaceholder")}
                  value={(settings?.healthProbeTestMessages ?? []).join("\n")}
                  onChange={(e) =>
                    update({ healthProbeTestMessages: e.target.value.split("\n") })
                  }
                />
              </div>
            )}
            <ToggleRow
              icon={Brain}
              title={t("settings.aggTraceTitle")}
              desc={t("settings.aggTraceDesc")}
              checked={settings?.aggregateTraceEnabled ?? false}
              onChange={(v) => update({ aggregateTraceEnabled: v })}
            />
            <ToggleRow
              icon={MousePointerClick}
              title={t("settings.trayModelTitle")}
              desc={t("settings.trayModelDesc")}
              checked={settings?.trayModelSwitchEnabled ?? true}
              onChange={(v) => {
                update({ trayModelSwitchEnabled: v });
                // 托盘菜单静态构建：开关改动后须主动重建，否则子菜单不随开关增减。
                void api.rebuildTrayMenu().catch((e) => console.error("rebuildTrayMenu failed", e));
              }}
            />
          </CardContent>
        </Card>

        {/* 启动 */}
        <Card>
          <CardHeader>
            <CardTitle>{t("settings.startup")}</CardTitle>
          </CardHeader>
          <CardContent className="space-y-3">
            <ToggleRow
              title={t("settings.autoStartTitle")}
              desc={t("settings.autoStartDesc")}
              checked={settings?.autoStart ?? false}
              onChange={handleToggleAutoStart}
            />
            {/* 悬浮窗（第⑥批）。desc 里必须写明「最小化到托盘后才出现」——
                否则用户开了开关却什么都没发生，会当成 bug 反复点。 */}
            <ToggleRow
              title={t("settings.floatingTitle")}
              desc={t("settings.floatingDesc")}
              checked={settings?.floatingWidgetEnabled ?? false}
              onChange={handleToggleFloating}
            />
            {/* 置顶子开关：只在悬浮球开着时才有意义，故关着时不显示 ——
                摆一个开了也看不到效果的开关，正是本项目反复防的那种困惑。 */}
            {settings?.floatingWidgetEnabled && (
              <ToggleRow
                title={t("settings.floatingPinTitle")}
                desc={t("settings.floatingPinDesc")}
                checked={settings?.floatingWidgetAlwaysOnTop ?? false}
                onChange={handleToggleFloatingPinned}
              />
            )}
          </CardContent>
        </Card>

        {/* 日志文件 */}
        <Card>
          <CardHeader>
            <CardTitle>{t("settings.logTitle")}</CardTitle>
          </CardHeader>
          <CardContent>
            <div className="flex gap-2.5">
              <FolderOpen size={16} className="mt-0.5 shrink-0 text-text-secondary" />
              <div className="flex-1">
                <div className="text-xs text-text-muted">{t("settings.logDesc")}</div>
                <div className="mt-2 flex items-center gap-2">
                  <input
                    type="text"
                    className="flex-1 rounded-control border border-border bg-bg px-2.5 py-1.5 font-mono text-xs text-text-primary placeholder:text-text-muted"
                    value={settings?.logDir ?? ""}
                    placeholder={defaultLogDir || t("settings.logDirDefault")}
                    onChange={(e) => update({ logDir: e.target.value || undefined })}
                  />
                  <Button size="sm" variant="secondary" onClick={() => void handlePickLogDir()}>
                    {t("settings.logBrowse")}
                  </Button>
                  {settings?.logDir && (
                    <Button size="sm" variant="outline" onClick={() => update({ logDir: undefined })}>
                      {t("settings.logReset")}
                    </Button>
                  )}
                </div>

                {/* 打开日志目录 + 诊断报告（UX#12/#13）。
                    实际路径全文常驻显示：MSIX 虚拟化下「按钮打开的目录」可能是包内私有副本，
                    用户必须能核对自己看的是哪一份（CLAUDE.md 平行宇宙惨案的复发防线）。 */}
                <div className="mt-2.5 flex flex-wrap items-center gap-2">
                  <Button size="sm" variant="outline" onClick={() => void handleOpenLogDir()}>
                    <FolderOpen size={13} /> {t("settings.logOpenDir")}
                  </Button>
                  <Button size="sm" variant="outline" onClick={() => void handleExportDiagnostics()}>
                    <ScrollText size={13} /> {t("settings.diagExport")}
                  </Button>
                </div>
                <div className="mt-1.5 break-all font-mono text-[11px] text-text-muted">
                  {effectiveLogDir || "—"}
                </div>
                <div className="mt-1 text-[11px] leading-relaxed text-text-muted">
                  {t("settings.diagDesc")}
                  {/* 「不含密钥」这句单独加粗：它决定用户敢不敢把报告发出去。
                      不能写成 Markdown `**…**`——本页没有 Markdown 渲染器，会显示成字面星号。 */}
                  <span className="font-semibold text-text-secondary">
                    {" "}
                    {t("settings.diagDescNoSecrets")}
                  </span>
                  {t("settings.diagDescTail")}
                </div>
              </div>
            </div>
          </CardContent>
        </Card>

        {/* 配置导入导出（FR-021）。密钥段走用户口令加密而非 DPAPI 密文——后者绑当前
            Windows 账户、换机解不出，照搬等于导出一份用不了的文件。 */}
        <Card>
          <CardHeader>
            <CardTitle>{t("settings.backup")}</CardTitle>
          </CardHeader>
          <CardContent className="space-y-2">
            <div className="flex gap-2">
              <Button variant="secondary" size="sm" onClick={() => setExportOpen(true)}>
                {t("settings.export")}
              </Button>
              <Button variant="secondary" size="sm" onClick={() => void startImport()}>
                {t("settings.import")}
              </Button>
            </div>
            <div className="text-xs text-text-muted">{t("settings.backupHint")}</div>
          </CardContent>
        </Card>
      </div>

      {exportOpen && <ExportDialog onClose={() => setExportOpen(false)} t={t} />}
      {importState && (
        <ImportDialog
          path={importState.path}
          preview={importState.preview}
          onClose={() => setImportState(null)}
          onDone={() => {
            setImportState(null);
            // 导入会整体改动 Key/厂商/大脑配置，必须让整个应用重新拉一遍，
            // 否则用户看到的还是导入前的列表（且下次保存会用旧快照顶回）。
            void reloadAfterImport();
          }}
          t={t}
        />
      )}

      {showWizard && (
        <McpWizard mcpAddress={mcpAddress} onClose={() => setShowWizard(false)} t={t} />
      )}

      {masterDialog && (
        <MasterPasswordDialog
          kind={masterDialog}
          onClose={() => setMasterDialog(null)}
          onDone={async () => {
            setMasterDialog(null);
            await reloadMaster();
            // 迁移会改 settings 里的镜像字段（后端在库切换成功后写），重拉一遍免得显示陈旧。
            try {
              setSettings(await api.getSettings());
            } catch (e) {
              console.error("reload settings after master switch failed", e);
            }
          }}
          t={t}
        />
      )}
    </div>
  );
}

/** MCP 接入向导弹窗：分步展示 Codex / Claude Code 的接入命令与可选钩子。 */
function McpWizard({
  mcpAddress,
  onClose,
  t,
}: {
  mcpAddress: string;
  onClose: () => void;
  t: (key: string, vars?: Record<string, string | number>) => string;
}) {
  const codexCmd = `codex mcp add --transport http synaroute ${mcpAddress}`;
  const codexHook = "遇到复杂的代码审查、架构设计、疑难排查任务时，优先调用 synaroute_ai 工具，获取多个模型的综合分析后再动手。";
  const claudeCmd = `claude mcp add --transport http synaroute ${mcpAddress}`;
  const claudeHook = `{
  "hooks": {
    "UserPromptSubmit": [
      { "matcher": "review|审查|重构", "hooks": [{ "type": "prompt", "prompt": "优先调用 synaroute_ai 做多模型分析" }] }
    ]
  }
}`;
  const verifyCmd = "claude mcp list";

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/50 p-6"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div className="max-h-[85vh] w-full max-w-xl overflow-y-auto rounded-card border border-border bg-surface p-5 shadow-xl">
        <div className="mb-3 flex items-center justify-between">
          <h2 className="text-base font-semibold text-text-primary">{t("settings.mcpWizardTitle")}</h2>
          <button onClick={onClose} className="rounded p-1 text-text-muted hover:bg-surface-hover hover:text-text-primary">
            <X size={16} />
          </button>
        </div>
        <p className="mb-4 text-xs text-text-muted">{t("settings.mcpWizardIntro")}</p>

        <WizardStep n={1} title={t("settings.mcpWizardCodex")}>
          <div className="mb-1 text-xs text-text-secondary">{t("settings.mcpWizardCodexStep")}</div>
          <WizardCode text={codexCmd} />
          <div className="mb-1 mt-2 text-xs text-text-secondary">{t("settings.mcpWizardCodexHook")}</div>
          <WizardCode text={codexHook} />
        </WizardStep>

        <WizardStep n={2} title={t("settings.mcpWizardClaude")}>
          <div className="mb-1 text-xs text-text-secondary">{t("settings.mcpWizardClaudeStep")}</div>
          <WizardCode text={claudeCmd} />
          <div className="mb-1 mt-2 text-xs text-text-secondary">{t("settings.mcpWizardClaudeHook")}</div>
          <WizardCode text={claudeHook} />
        </WizardStep>

        <WizardStep n={3} title={t("settings.mcpWizardVerify")}>
          <div className="mb-1 text-xs text-text-secondary">{t("settings.mcpWizardVerifyStep")}</div>
          <WizardCode text={verifyCmd} />
        </WizardStep>

        <WizardStep n={4} title={t("settings.mcpWizardUse")}>
          <div className="text-xs text-text-secondary">{t("settings.mcpWizardUseStep")}</div>
        </WizardStep>

        <div className="mt-4 flex justify-end">
          <Button size="sm" variant="secondary" onClick={onClose}>{t("settings.mcpWizardClose")}</Button>
        </div>
      </div>
    </div>
  );
}

function WizardStep({ n, title, children }: { n: number; title: string; children: React.ReactNode }) {
  return (
    <div className="mb-4">
      <div className="mb-1.5 flex items-center gap-2">
        <span className="flex h-5 w-5 items-center justify-center rounded-full bg-primary/15 text-[11px] font-semibold text-primary">{n}</span>
        <span className="text-sm font-medium text-text-primary">{title}</span>
      </div>
      <div className="pl-7">{children}</div>
    </div>
  );
}

function WizardCode({ text }: { text: string }) {
  const [copied, setCopied] = useState(false);
  const copy = () => {
    void navigator.clipboard?.writeText(text).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 1200);
    });
  };
  return (
    <div className="relative">
      <pre className="overflow-x-auto rounded-control border border-border bg-bg p-2.5 pr-9 font-mono text-[11px] leading-relaxed text-text-primary whitespace-pre-wrap break-all">
        {text}
      </pre>
      <button
        onClick={copy}
        className="absolute right-1.5 top-1.5 rounded p-1 text-text-muted hover:bg-surface-hover hover:text-text-primary"
        title="copy"
      >
        {copied ? <Check size={12} /> : <Copy size={12} />}
      </button>
    </div>
  );
}

function ToggleRow({
  icon: Icon,
  title,
  desc,
  cost,
  checked,
  onChange,
  danger,
  disabled,
  badge,
}: {
  icon?: LucideIcon;
  title: string;
  desc: string;
  /**
   * 开启这个开关要付出的代价（UX#15）。
   *
   * 与 `desc` 分开是因为两者用途不同：`desc` 讲「它是什么」，`cost` 讲「开了会怎样」。
   * **常驻显示**（不是只在开着时才显示）——关着的时候用户正是要靠这句判断该不该开；
   * 但**开着时转成 warning 色**，因为那时这句话的作用变成了「排障完记得关掉」。
   */
  cost?: string;
  checked: boolean;
  onChange: (v: boolean) => void;
  danger?: boolean;
  disabled?: boolean;
  badge?: string;
}) {
  return (
    <div className={`flex items-start justify-between gap-4 ${disabled ? "opacity-60" : ""}`}>
      <div className="flex gap-2.5">
        {Icon && <Icon size={16} className={`mt-0.5 shrink-0 ${danger ? "text-danger" : "text-text-secondary"}`} />}
        <div>
          <div className="flex items-center gap-2 text-sm font-medium text-text-primary">
            {title}
            {badge && (
              <span className="rounded-full border border-border px-1.5 py-0.5 text-[10px] font-normal text-text-muted">
                {badge}
              </span>
            )}
          </div>
          <div className="text-xs text-text-muted">{desc}</div>
          {cost && (
            <div
              className={`mt-1 text-[11px] leading-relaxed ${checked ? "text-warning" : "text-text-muted"}`}
            >
              {cost}
            </div>
          )}
        </div>
      </div>
      <Switch checked={checked} onCheckedChange={onChange} disabled={disabled} />
    </div>
  );
}

/** 导出配置弹窗：先选「是否含密钥」，含则要求设口令并二次确认。 */
function ExportDialog({
  onClose,
  t,
}: {
  onClose: () => void;
  t: (k: string, vars?: Record<string, string | number>) => string;
}) {
  const showToast = useStore((s) => s.showToast);
  const [includeSecrets, setIncludeSecrets] = useState(false);
  const [pw, setPw] = useState("");
  const [pw2, setPw2] = useState("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  // 只在「有 Key 的密钥解不出被跳过」时置位。这种情况**不能只弹 toast**——toast 会自己消失，
  // 用户很可能带着一份缺条目的文件离开源机器，到新机器才发现。故留在弹窗里等用户确认。
  const [skipped, setSkipped] = useState<ExportOutcome | null>(null);

  // 口令校验只在勾了「含密钥」时生效。8 位下限是个务实底线：这层的真实保护来自
  // Argon2id 的派生成本，但太短的口令依然挡不住字典攻击。
  const pwProblem = !includeSecrets
    ? null
    : pw.length < 8
      ? t("backup.passwordTooShort")
      : pw !== pw2
        ? t("backup.passwordMismatch")
        : null;

  const run = async () => {
    if (pwProblem) return setErr(pwProblem);
    setBusy(true);
    setErr(null);
    try {
      const out = await api.exportConfig(includeSecrets ? pw : undefined);
      if (!out) return onClose(); // 用户取消选文件，不算失败
      if (out.undecryptable > 0) {
        // 文件已经写成功了，只是少了几条密钥。停在弹窗上把这事说清楚。
        setSkipped(out);
        setBusy(false);
        return;
      }
      showToast("success", t("backup.exportDone", { path: out.path }));
      onClose();
    } catch (e) {
      setErr(String((e as Error)?.message ?? e));
      setBusy(false);
    }
  };

  // 导出成功但有条目被跳过：单独一屏，必须用户点掉才走。
  if (skipped) {
    return (
      <ModalShell title={t("backup.exportTitle")} icon={Download} onClose={onClose}>
        <div className="break-all rounded-control bg-surface-elevated px-3 py-2 font-mono text-[11px] text-text-secondary">
          {skipped.path}
        </div>
        <div className="flex items-start gap-2 rounded-control border border-warning/30 bg-warning/8 px-3 py-2">
          <AlertTriangle size={13} className="mt-0.5 shrink-0 text-warning" />
          <span className="text-[11px] leading-relaxed text-warning">
            {t("backup.exportSkipped", { n: skipped.undecryptable })}
          </span>
        </div>
        <div className="flex justify-end pt-1">
          <Button size="sm" onClick={onClose}>
            {t("common.close")}
          </Button>
        </div>
      </ModalShell>
    );
  }

  return (
    <ModalShell title={t("backup.exportTitle")} icon={Download} onClose={onClose} busy={busy}>
      <label className="flex cursor-pointer items-start gap-2.5">
        <input
          type="checkbox"
          className="mt-0.5"
          checked={includeSecrets}
          onChange={(e) => setIncludeSecrets(e.target.checked)}
        />
        <div>
          <div className="text-sm text-text-primary">{t("backup.includeSecrets")}</div>
          <div className="mt-0.5 text-[11px] leading-relaxed text-text-muted">
            {t("backup.includeSecretsDesc")}
          </div>
        </div>
      </label>

      {includeSecrets ? (
        <div className="space-y-2">
          <input
            type="password"
            className={pwInputCls}
            placeholder={t("backup.exportPassword")}
            value={pw}
            onChange={(e) => setPw(e.target.value)}
            autoFocus
          />
          <input
            type="password"
            className={pwInputCls}
            placeholder={t("backup.exportPasswordAgain")}
            value={pw2}
            onChange={(e) => setPw2(e.target.value)}
          />
          <div className="flex items-start gap-2 rounded-control border border-warning/30 bg-warning/8 px-3 py-2">
            <AlertTriangle size={13} className="mt-0.5 shrink-0 text-warning" />
            <span className="text-[11px] leading-relaxed text-warning">
              {t("backup.passwordLostWarning")}
            </span>
          </div>
        </div>
      ) : (
        <div className="rounded-control bg-info/10 px-3 py-2 text-[11px] leading-relaxed text-text-secondary">
          {t("backup.noSecretsHint")}
        </div>
      )}

      {/* 口令校验提示常驻渲染：只把它塞进 disabled 判据的话，用户只看到一个灰掉的按钮，
          界面上一个字都不说为什么（「至少 8 位」这个约束也无从得知）。开始输入后才显示，
          避免刚打开弹窗就报红。 */}
      {includeSecrets && pwProblem && (pw.length > 0 || pw2.length > 0) && (
        <div className="text-[11px] leading-relaxed text-warning">{pwProblem}</div>
      )}

      {err && (
        <div className="whitespace-pre-line rounded-control bg-danger/10 px-3 py-2 text-xs leading-relaxed text-danger">
          {err}
        </div>
      )}

      <div className="flex justify-end gap-2 pt-1">
        <Button variant="ghost" size="sm" onClick={onClose} disabled={busy}>
          {t("common.cancel")}
        </Button>
        <Button size="sm" onClick={() => void run()} disabled={busy || !!pwProblem}>
          {busy ? t("backup.exporting") : t("backup.exportAction")}
        </Button>
      </div>
    </ModalShell>
  );
}

/** 导入配置弹窗：展示预检结果 → 选模式 →（若含密钥）输口令 → 执行。 */
function ImportDialog({
  path,
  preview,
  onClose,
  onDone,
  t,
}: {
  path: string;
  preview: ImportPreview;
  onClose: () => void;
  onDone: () => void;
  t: (k: string, vars?: Record<string, string | number>) => string;
}) {
  const showToast = useStore((s) => s.showToast);
  const [mode, setMode] = useState<ImportMode>("merge");
  const [pw, setPw] = useState("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [report, setReport] = useState<ImportReport | null>(null);

  const run = async () => {
    setBusy(true);
    setErr(null);
    try {
      const r = await api.applyImportConfig(path, mode, preview.hasSecrets ? pw : undefined);
      setReport(r);
      showToast(
        "success",
        t("backup.importDone", {
          added: r.keysAdded,
          overwritten: r.keysOverwritten,
          removed: r.keysRemoved,
          secrets: r.secretsImported,
        }),
      );
    } catch (e) {
      // 口令错 / 文件损坏都在这里。后端已保证「失败时本机配置一点没动」。
      setErr(String((e as Error)?.message ?? e));
    } finally {
      setBusy(false);
    }
  };

  // 导入已完成：只展示结果与警告，按「完成」才刷新全局状态并关闭。
  if (report) {
    return (
      <ModalShell title={t("backup.importTitle")} icon={Upload} onClose={onDone}>
        <div className="rounded-control bg-success/10 px-3 py-2 text-xs leading-relaxed text-success">
          {t("backup.importDone", {
            added: report.keysAdded,
            overwritten: report.keysOverwritten,
            removed: report.keysRemoved,
            secrets: report.secretsImported,
          })}
        </div>
        {/* 清理掉的旧密钥要明确报告（P2-3）：密钥是敏感材料，删了几条不该无声发生。 */}
        {report.secretsPruned > 0 && (
          <div className="rounded-control bg-surface-elevated px-3 py-2 text-xs leading-relaxed text-text-secondary">
            {t("backup.secretsPruned", { n: report.secretsPruned })}
          </div>
        )}
        {report.backupPath && (
          <div className="space-y-1.5 rounded-control bg-surface-elevated px-3 py-2">
            <div className="break-all font-mono text-[11px] text-text-secondary">
              {t("backup.backupSaved", { path: report.backupPath })}
            </div>
            {/* 备份路径原先只是显示出来，用户不知道拿它能干什么（docs/15 UX #20）。 */}
            <div className="text-[11px] leading-relaxed text-text-muted">
              {t("backup.backupRestoreHint")}
            </div>
            {/* 删过旧密钥时，密钥库的备份必须并列给出并单独说明怎么用：
                secrets.enc 不是能被「导入」的格式，只能手动拷回去。
                少了这一句，用户按上面那行导回配置，会得到「Key 都回来了、密钥没了」的半截状态。 */}
            {report.secretsBackupPath && (
              <>
                <div className="break-all font-mono text-[11px] text-text-secondary">
                  {t("backup.secretsBackupSaved", { path: report.secretsBackupPath })}
                </div>
                <div className="text-[11px] leading-relaxed text-text-muted">
                  {t("backup.secretsBackupRestoreHint")}
                </div>
              </>
            )}
          </div>
        )}
        {report.warnings.length > 0 && (
          <div className="space-y-1 rounded-control border border-warning/30 bg-warning/8 px-3 py-2">
            {report.warnings.map((w, i) => (
              <div key={i} className="flex items-start gap-2">
                <AlertTriangle size={13} className="mt-0.5 shrink-0 text-warning" />
                <span className="text-[11px] leading-relaxed text-warning">{w}</span>
              </div>
            ))}
          </div>
        )}
        <div className="flex justify-end pt-1">
          <Button size="sm" onClick={onDone}>
            {t("common.close")}
          </Button>
        </div>
      </ModalShell>
    );
  }

  return (
    <ModalShell title={t("backup.importTitle")} icon={Upload} onClose={onClose} busy={busy}>
      <div className="space-y-1 rounded-control bg-surface-elevated px-3 py-2">
        <div className="break-all font-mono text-[11px] text-text-secondary">{path}</div>
        <div className="text-[11px] text-text-muted">
          {t("backup.fileInfo", { version: preview.appVersion, time: preview.exportedAt })}
        </div>
        <div className="text-xs text-text-primary">
          {t("backup.summary", {
            keys: preview.keyCount,
            vendors: preview.vendorCount,
            brain: preview.brainCount,
          })}
        </div>
        <div className="text-[11px] text-text-muted">
          {preview.hasSecrets ? t("backup.hasSecrets") : t("backup.noSecrets")}
        </div>
      </div>

      <div className="space-y-2">
        <div className="text-xs font-medium text-text-primary">{t("backup.modeTitle")}</div>
        <ModeOption
          checked={mode === "merge"}
          onSelect={() => setMode("merge")}
          title={t("backup.modeMerge")}
          desc={t("backup.modeMergeDesc")}
          note={
            preview.conflictingKeys > 0
              ? t("backup.conflictNote", { n: preview.conflictingKeys })
              : undefined
          }
        />
        <ModeOption
          checked={mode === "replace"}
          onSelect={() => setMode("replace")}
          title={t("backup.modeReplace")}
          desc={t("backup.modeReplaceDesc")}
          note={
            preview.localOnlyKeys > 0
              ? t("backup.removeNote", { n: preview.localOnlyKeys })
              : undefined
          }
          danger
        />
      </div>

      {/* 可疑冲突（P3-5）：id 相同但名字与地址都不同 → 极可能是历史时间戳 id 跨机碰撞，
          导入会把一条完全无关的本机 Key 换成对方的配置。必须逐条显示，
          而不是混进 conflictingKeys 那个计数里——后者与「同一条 Key 的正常更新」无法区分。 */}
      {preview.suspiciousConflicts.length > 0 && (
        <div className="space-y-1.5 rounded-control border border-danger/40 bg-danger/5 p-2.5">
          <div className="flex items-center gap-1.5 text-xs font-medium text-danger">
            <AlertTriangle size={13} className="shrink-0" />
            {t("backup.suspiciousTitle", { n: preview.suspiciousConflicts.length })}
          </div>
          <div className="text-[11px] leading-relaxed text-text-secondary">
            {t("backup.suspiciousDesc")}
          </div>
          <ul className="space-y-1">
            {preview.suspiciousConflicts.map((c) => (
              <li key={c.id} className="font-mono text-[11px] text-text-secondary">
                <span className="text-danger">{c.localName}</span>
                <span className="text-text-muted"> ({c.localBaseUrl}) → </span>
                <span className="text-text-primary">{c.incomingName}</span>
                <span className="text-text-muted"> ({c.incomingBaseUrl})</span>
              </li>
            ))}
          </ul>
        </div>
      )}

      {preview.hasSecrets && (
        <input
          type="password"
          className={pwInputCls}
          placeholder={t("backup.importPassword")}
          value={pw}
          onChange={(e) => setPw(e.target.value)}
          autoFocus
        />
      )}

      {err && (
        <div className="whitespace-pre-line rounded-control bg-danger/10 px-3 py-2 text-xs leading-relaxed text-danger">
          {err}
        </div>
      )}

      <div className="flex justify-end gap-2 pt-1">
        <Button variant="ghost" size="sm" onClick={onClose} disabled={busy}>
          {t("common.cancel")}
        </Button>
        <Button
          size="sm"
          onClick={() => void run()}
          disabled={busy || (preview.hasSecrets && pw.length === 0)}
        >
          {busy ? t("backup.importing") : t("backup.importAction")}
        </Button>
      </div>
    </ModalShell>
  );
}

/** 导入模式单选项。`danger` 用于「替换」这种破坏性选项，选中后描边变警示色。 */
function ModeOption({
  checked,
  onSelect,
  title,
  desc,
  note,
  danger,
}: {
  checked: boolean;
  onSelect: () => void;
  title: string;
  desc: string;
  note?: string;
  danger?: boolean;
}) {
  const border = checked
    ? danger
      ? "border-warning/50 bg-warning/8"
      : "border-primary/50 bg-primary/8"
    : "border-border";
  return (
    <label className={`flex cursor-pointer items-start gap-2.5 rounded-control border ${border} px-3 py-2`}>
      <input type="radio" className="mt-0.5" checked={checked} onChange={onSelect} />
      <div className="min-w-0">
        <div className="text-sm text-text-primary">{title}</div>
        <div className="mt-0.5 text-[11px] leading-relaxed text-text-muted">{desc}</div>
        {note && (
          <div className={`mt-1 text-[11px] ${danger ? "text-warning" : "text-text-secondary"}`}>
            {note}
          </div>
        )}
      </div>
    </label>
  );
}

/** 弹窗外壳：与项目既有弹窗一致——遮罩用 onMouseDown + target===currentTarget 判定，
 *  避免框内拖选文字松手落到遮罩上误关（见 memory: modal-overlay-close-pattern）。
 *
 *  `busy` 期间遮罩与 X 都不响应：这两条路径的结果（导出跳过条数、迁移失败原因）只存在于
 *  弹窗组件的局部 state，组件一卸载 setState 就是 no-op，React 不报错也不提示 ——
 *  用户会带着一份缺条目的导出文件走，或以为主口令启用成功了。取消按钮本来就 disabled，
 *  遮罩与 X 必须同口径，否则「禁用」形同虚设。 */
function ModalShell({
  title,
  icon: Icon,
  onClose,
  busy = false,
  children,
}: {
  title: string;
  icon: LucideIcon;
  onClose: () => void;
  /** 进行中：禁止通过遮罩/X 关闭，防止结果信息随组件卸载丢失 */
  busy?: boolean;
  children: React.ReactNode;
}) {
  const close = () => {
    if (!busy) onClose();
  };
  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/40 p-4"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) close();
      }}
    >
      <div className="flex max-h-[85vh] w-full max-w-lg flex-col overflow-hidden rounded-card border border-border bg-surface shadow-lg">
        <div className="flex items-center gap-2 border-b border-border px-4 py-3">
          <Icon size={16} className="text-primary" />
          <h2 className="flex-1 text-sm font-semibold text-text-primary">{title}</h2>
          <button
            type="button"
            onClick={close}
            disabled={busy}
            className="rounded p-1 hover:bg-surface-hover disabled:cursor-not-allowed disabled:opacity-40"
          >
            <X size={16} />
          </button>
        </div>
        <div className="flex-1 space-y-3 overflow-y-auto px-4 py-3">{children}</div>
      </div>
    </div>
  );
}

const pwInputCls =
  "h-9 w-full rounded-control border border-border bg-surface px-3 text-sm text-text-primary placeholder:text-text-muted focus:outline-none focus:ring-2 focus:ring-ring";

/**
 * 主口令增强模式的四合一对话框（FR-018 可选增强）。
 *
 * 四种用途共用一个组件，因为它们的结构完全一致（收 1~2 个口令 → 调一个命令 → 报结果），
 * 分成四个组件只会把同样的错误处理、忙碌态、回车提交抄四遍。
 *
 * | kind | 收什么 | 调什么 |
 * |---|---|---|
 * | `enable` | 新口令 ×2 | `enableMasterPassword` |
 * | `disable` | 当前口令 | `disableMasterPassword` |
 * | `unlock` | 当前口令 | `unlockMasterPassword` |
 * | `change` | 旧口令 + 新口令 ×2 | `changeMasterPassword` |
 *
 * 失败**不关闭弹窗**：口令错是最常见的失败，关掉会让用户从头点起。
 */
function MasterPasswordDialog({
  kind,
  onClose,
  onDone,
  t,
}: {
  kind: "enable" | "disable" | "unlock" | "change";
  onClose: () => void;
  onDone: () => void;
  t: (k: string, vars?: Record<string, string | number>) => string;
}) {
  const showToast = useStore((s) => s.showToast);
  const [oldPw, setOldPw] = useState("");
  const [pw, setPw] = useState("");
  const [pw2, setPw2] = useState("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  // 需要「设新口令」的两种：要二次确认 + 长度下限。
  // 8 位下限是务实底线——真正的保护来自 Argon2id 的派生成本，但太短仍挡不住字典攻击。
  const settingNew = kind === "enable" || kind === "change";
  const pwProblem = !settingNew
    ? null
    : pw.length < 8
      ? t("master.passwordTooShort")
      : pw !== pw2
        ? t("master.passwordMismatch")
        : null;
  // 提交按钮的可用判据：各 kind 需要的字段都填了、且新口令校验通过。
  const canSubmit =
    !busy &&
    !pwProblem &&
    (kind === "change" ? oldPw.length > 0 && pw.length > 0 : pw.length > 0);

  const titleKey = {
    enable: "master.enableTitle",
    disable: "master.disableTitle",
    unlock: "master.unlockTitle",
    change: "master.changeTitle",
  }[kind];
  const descKey = {
    enable: "master.enableDesc",
    disable: "master.disableDesc",
    unlock: "master.unlockDesc",
    change: "master.changeDesc",
  }[kind];
  const actionKey = {
    enable: "master.enableAction",
    disable: "master.disableAction",
    unlock: "master.unlockAction",
    change: "master.changeAction",
  }[kind];
  const busyKey = {
    enable: "master.enabling",
    disable: "master.disabling",
    unlock: "master.unlocking",
    change: "master.changing",
  }[kind];

  const run = async () => {
    if (pwProblem) return setErr(pwProblem);
    setBusy(true);
    setErr(null);
    try {
      if (kind === "unlock") {
        await api.unlockMasterPassword(pw);
        showToast("success", t("master.unlockAction"));
      } else if (kind === "enable") {
        const n = await api.enableMasterPassword(pw);
        showToast("success", t("master.enabled", { count: n }));
      } else if (kind === "disable") {
        const n = await api.disableMasterPassword(pw);
        showToast("success", t("master.disabled", { count: n }));
      } else {
        const n = await api.changeMasterPassword(oldPw, pw);
        showToast("success", t("master.changed", { count: n }));
      }
      onDone();
    } catch (e) {
      // 不关弹窗：口令错是最常见的失败，关掉等于让用户从头点一遍。
      setErr(String((e as Error)?.message ?? e));
      setBusy(false);
    }
  };

  // 回车提交：这几个弹窗都是纯口令输入，用户手不会离开键盘。
  const onKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "Enter" && canSubmit) void run();
  };

  return (
    <ModalShell title={t(titleKey)} icon={ShieldCheck} onClose={onClose} busy={busy}>
      <div className="rounded-control bg-info/10 px-3 py-2 text-[11px] leading-relaxed text-text-secondary">
        {t(descKey)}
      </div>

      {kind === "change" && (
        <input
          type="password"
          className={pwInputCls}
          placeholder={t("master.oldPassword")}
          value={oldPw}
          onChange={(e) => setOldPw(e.target.value)}
          onKeyDown={onKeyDown}
          autoFocus
        />
      )}

      <input
        type="password"
        className={pwInputCls}
        placeholder={t(settingNew ? "master.newPassword" : "master.password")}
        value={pw}
        onChange={(e) => setPw(e.target.value)}
        onKeyDown={onKeyDown}
        autoFocus={kind !== "change"}
      />

      {settingNew && (
        <input
          type="password"
          className={pwInputCls}
          placeholder={t("master.newPasswordAgain")}
          value={pw2}
          onChange={(e) => setPw2(e.target.value)}
          onKeyDown={onKeyDown}
        />
      )}

      {/* 启用是不可逆风险最高的一步：忘记口令等于永久失去全部密钥，必须显式警告。 */}
      {kind === "enable" && (
        <div className="flex items-start gap-2 rounded-control border border-warning/30 bg-warning/8 px-3 py-2">
          <AlertTriangle size={13} className="mt-0.5 shrink-0 text-warning" />
          <span className="text-[11px] leading-relaxed text-warning">
            {t("master.enableWarning")}
          </span>
        </div>
      )}

      {/* 口令校验提示常驻渲染，理由同导出弹窗：只塞进 disabled 判据的话，
          用户只看到一个灰掉的按钮而界面不说原因。开始输入后才显示。 */}
      {pwProblem && (pw.length > 0 || pw2.length > 0) && (
        <div className="text-[11px] leading-relaxed text-warning">{pwProblem}</div>
      )}

      {err && (
        <div className="whitespace-pre-line rounded-control bg-danger/10 px-3 py-2 text-xs leading-relaxed text-danger">
          {err}
        </div>
      )}

      <div className="flex justify-end gap-2 pt-1">
        <Button variant="ghost" size="sm" onClick={onClose} disabled={busy}>
          {t("common.cancel")}
        </Button>
        <Button size="sm" onClick={() => void run()} disabled={!canSubmit}>
          {busy ? t(busyKey) : t(actionKey)}
        </Button>
      </div>
    </ModalShell>
  );
}
