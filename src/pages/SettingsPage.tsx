import { useEffect, useState } from "react";
import { useStore } from "@/store";
import { api } from "@/lib/bridge";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/Card";
import { Switch } from "@/components/ui/Switch";
import { Button } from "@/components/ui/Button";
import { useT } from "@/lib/useT";
import { LANGS } from "@/lib/i18n";
import type { AppSettings, ThemePref } from "@/types";
import { Sun, Moon, Monitor, ShieldCheck, KeyRound, Languages, ScrollText, type LucideIcon } from "lucide-react";

/** 设置页（主题 / 语言 / 自启 / 加密方式 / 局域网暴露） */
export function SettingsPage() {
  const { theme, setTheme, lang, setLang } = useStore();
  const t = useT();
  const [settings, setSettings] = useState<AppSettings | null>(null);

  useEffect(() => {
    void api.getSettings().then(setSettings);
  }, []);

  const update = (patch: Partial<AppSettings>) => {
    if (!settings) return;
    const next = { ...settings, ...patch };
    setSettings(next);
    void api.saveSettings(next);
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
              checked={settings?.masterPasswordEnabled ?? false}
              onChange={(v) => update({ masterPasswordEnabled: v })}
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

        {/* 调试 / 可观测 */}
        <Card>
          <CardHeader>
            <CardTitle>{t("settings.debug")}</CardTitle>
          </CardHeader>
          <CardContent>
            <ToggleRow
              icon={ScrollText}
              title={t("settings.reqLogTitle")}
              desc={t("settings.reqLogDesc")}
              checked={settings?.requestLogEnabled ?? false}
              onChange={(v) => update({ requestLogEnabled: v })}
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

        {/* 配置导入导出 */}
        <Card>
          <CardHeader>
            <CardTitle>{t("settings.backup")}</CardTitle>
          </CardHeader>
          <CardContent className="flex gap-2">
            <Button variant="secondary" size="sm">{t("settings.export")}</Button>
            <Button variant="secondary" size="sm">{t("settings.import")}</Button>
          </CardContent>
        </Card>
      </div>
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
}: {
  icon?: LucideIcon;
  title: string;
  desc: string;
  checked: boolean;
  onChange: (v: boolean) => void;
  danger?: boolean;
}) {
  return (
    <div className="flex items-start justify-between gap-4">
      <div className="flex gap-2.5">
        {Icon && <Icon size={16} className={`mt-0.5 shrink-0 ${danger ? "text-danger" : "text-text-secondary"}`} />}
        <div>
          <div className="text-sm font-medium text-text-primary">{title}</div>
          <div className="text-xs text-text-muted">{desc}</div>
        </div>
      </div>
      <Switch checked={checked} onCheckedChange={onChange} />
    </div>
  );
}
