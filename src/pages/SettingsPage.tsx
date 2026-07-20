import { useEffect, useState } from "react";
import { useStore } from "@/store";
import { api } from "@/lib/bridge";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/Card";
import { Switch } from "@/components/ui/Switch";
import { Button } from "@/components/ui/Button";
import { useT } from "@/lib/useT";
import { LANGS } from "@/lib/i18n";
import type { AppSettings, McpStatus, ThemePref } from "@/types";
import {
  Sun, Moon, Monitor, ShieldCheck, KeyRound, Languages, ScrollText,
  Activity, RefreshCw, FolderOpen, Info, Plug, BookOpen, Copy, Check, X, type LucideIcon,
} from "lucide-react";

/** 设置页（主题 / 语言 / 自启 / 加密方式 / 局域网暴露 / 版本更新 / 日志路径） */
export function SettingsPage() {
  const { theme, setTheme, lang, setLang, showToast, activeCategory } = useStore();
  const t = useT();
  const [settings, setSettings] = useState<AppSettings | null>(null);
  const [version, setVersion] = useState<string>("");
  const [updateMsg, setUpdateMsg] = useState<string | null>(null);
  const [checking, setChecking] = useState(false);
  const [defaultLogDir, setDefaultLogDir] = useState<string>("");
  const [mcp, setMcp] = useState<McpStatus | null>(null);
  const [showWizard, setShowWizard] = useState(false);

  useEffect(() => {
    void api.getSettings().then(setSettings);
    void api.getAppVersion().then(setVersion);
    void api.getDefaultLogDir().then(setDefaultLogDir);
  }, []);

  // MCP 运行状态轮询：开启时每 3s 刷新一次，展示运行中/端口/故障原因。
  useEffect(() => {
    if (!settings?.mcpEnabled) {
      setMcp(null);
      return;
    }
    let alive = true;
    const tick = () => void api.mcpStatus().then((s) => { if (alive) setMcp(s); });
    tick();
    const id = setInterval(tick, 3000);
    return () => { alive = false; clearInterval(id); };
  }, [settings?.mcpEnabled]);

  const update = (patch: Partial<AppSettings>) => {
    if (!settings) return;
    const next = { ...settings, ...patch };
    setSettings(next);
    // 落盘失败必须可见（项目硬规则：禁止静默吞错）。乐观更新 UI，写盘失败时弹提示。
    void api.saveSettings(next).catch((e) => {
      console.error("saveSettings failed", e);
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
    setChecking(true);
    setUpdateMsg(null);
    try {
      const newVer = await api.checkForUpdates();
      setUpdateMsg(newVer ? t("settings.updateAvailable", { version: newVer }) : t("settings.upToDate"));
    } catch (e) {
      setUpdateMsg(t("settings.updateError", { err: String((e as Error)?.message ?? e) }));
    } finally {
      setChecking(false);
    }
  };

  const handlePickLogDir = async () => {
    const dir = await api.pickDirectory();
    if (dir) update({ logDir: dir });
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

  const themeOptions: { value: ThemePref; tKey: string; icon: LucideIcon }[] = [
    { value: "light", tKey: "settings.theme.light", icon: Sun },
    { value: "dark", tKey: "settings.theme.dark", icon: Moon },
    { value: "system", tKey: "settings.theme.system", icon: Monitor },
  ];

  return (
    <div className="h-full overflow-y-auto">
      <div className="border-b border-border px-6 py-4">
        <h1 className="text-lg font-semibold text-text-primary">{t("settings.title")}</h1>
      </div>

      <div className="max-w-2xl space-y-4 p-6">
        {/* 版本与更新 */}
        <Card>
          <CardHeader>
            <CardTitle>{t("settings.about")}</CardTitle>
          </CardHeader>
          <CardContent>
            <div className="flex items-center justify-between gap-4">
              <div className="flex gap-2.5">
                <Info size={16} className="mt-0.5 shrink-0 text-text-secondary" />
                <div>
                  <div className="text-sm font-medium text-text-primary">
                    SynaRoute v{version}
                  </div>
                  {updateMsg && (
                    <div className="mt-1 text-xs text-text-muted">{updateMsg}</div>
                  )}
                </div>
              </div>
              <Button size="sm" variant="secondary" onClick={() => void handleCheckUpdate()} disabled={checking}>
                <RefreshCw size={14} className={checking ? "animate-spin" : ""} />
                {t("settings.checkUpdate")}
              </Button>
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
            <ToggleRow
              icon={ShieldCheck}
              title={t("settings.masterPwTitle")}
              desc={t("settings.masterPwDesc")}
              checked={false}
              onChange={() => {}}
              disabled
            />
            <ToggleRow
              icon={KeyRound}
              title={t("settings.lanTitle")}
              desc={t("settings.lanDesc")}
              checked={settings?.lanExposure ?? false}
              onChange={(v) => update({ lanExposure: v })}
              danger
            />
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

                <Button size="sm" variant="outline" onClick={() => setShowWizard(true)}>
                  <BookOpen size={14} /> {t("settings.mcpWizard")}
                </Button>
              </>
            )}
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
              checked={settings?.requestLogEnabled ?? false}
              onChange={(v) => update({ requestLogEnabled: v })}
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
                {[30, 60, 120, 300].map((s) => (
                  <option key={s} value={s}>
                    {t(`settings.health.${s}`)}
                  </option>
                ))}
              </select>
            </div>
            <ToggleRow
              icon={Activity}
              title={t("settings.realProbeTitle")}
              desc={t("settings.realProbeDesc")}
              checked={settings?.healthProbeRealCompletion ?? false}
              onChange={(v) => update({ healthProbeRealCompletion: v })}
            />
          </CardContent>
        </Card>

        {/* 启动 */}
        <Card>
          <CardHeader>
            <CardTitle>{t("settings.startup")}</CardTitle>
          </CardHeader>
          <CardContent>
            <ToggleRow
              title={t("settings.autoStartTitle")}
              desc={t("settings.autoStartDesc")}
              checked={settings?.autoStart ?? false}
              onChange={(v) => update({ autoStart: v })}
            />
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
              </div>
            </div>
          </CardContent>
        </Card>

        {/* 配置导入导出 */}
        <Card>
          <CardHeader>
            <CardTitle>{t("settings.backup")}</CardTitle>
          </CardHeader>
          <CardContent className="space-y-2">
            <div className="flex gap-2">
              <Button variant="secondary" size="sm" disabled>{t("settings.export")}</Button>
              <Button variant="secondary" size="sm" disabled>{t("settings.import")}</Button>
            </div>
            <div className="text-xs text-text-muted">{t("settings.backupDeveloping")}</div>
          </CardContent>
        </Card>
      </div>

      {showWizard && (
        <McpWizard mcpAddress={mcpAddress} onClose={() => setShowWizard(false)} t={t} />
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
  checked,
  onChange,
  danger,
  disabled,
  badge,
}: {
  icon?: LucideIcon;
  title: string;
  desc: string;
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
        </div>
      </div>
      <Switch checked={checked} onCheckedChange={onChange} disabled={disabled} />
    </div>
  );
}
