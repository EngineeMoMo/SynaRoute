import { Button } from "@/components/ui/Button";
import { Badge } from "@/components/ui/Badge";
import type { ProxyState } from "@/types";
import { useStore } from "@/store";
import { api } from "@/lib/bridge";
import { useT } from "@/lib/useT";
import { Play, Square, Copy, ShieldCheck } from "lucide-react";
import { useState } from "react";

/** 顶部代理状态条：显示端点、运行状态，提供启停与一键写入工具配置（FR-008/FR-019） */
export function ProxyStatusBar({ proxy }: { proxy: ProxyState | null }) {
  const { startProxy, stopProxy, activeCategory, keys, loadCategory } = useStore();
  const t = useT();
  const [applyMsg, setApplyMsg] = useState<string | null>(null);

  const enabledCount = keys.filter((k) => k.enabled).length;
  const running = proxy?.status === "running";
  const endpoint = proxy?.port ? `http://127.0.0.1:${proxy.port}` : "—";

  const routeModeLabel =
    enabledCount <= 1 ? t("proxy.directMode") : t("proxy.failoverMode", { n: enabledCount });

  const handleApply = async () => {
    const msg = await api.applyToolConfig(activeCategory);
    setApplyMsg(msg);
    // apply_tool_config 后端会确保代理已启动，刷新状态条与列表使 UI 同步。
    await loadCategory(activeCategory);
    setTimeout(() => setApplyMsg(null), 4000);
  };

  const copyEndpoint = () => {
    if (proxy?.port) void navigator.clipboard?.writeText(endpoint);
  };

  return (
    <div className="flex items-center gap-3 border-b border-border bg-surface px-6 py-3">
      <div className="flex items-center gap-2">
        <span
          className={`inline-block h-2 w-2 rounded-full ${
            running ? "bg-success" : "bg-text-muted"
          }`}
        />
        <span className="text-sm font-medium text-text-primary">
          {running ? t("proxy.running") : t("proxy.stopped")}
        </span>
      </div>

      <Badge variant={enabledCount > 1 ? "primary" : "neutral"}>{routeModeLabel}</Badge>

      <div className="flex items-center gap-1.5 font-mono text-xs text-text-secondary">
        <span>{endpoint}</span>
        {proxy?.port && (
          <button
            onClick={copyEndpoint}
            className="rounded p-1 hover:bg-surface-hover"
            title={t("proxy.copyEndpoint")}
          >
            <Copy size={12} />
          </button>
        )}
      </div>

      <div className="ml-auto flex items-center gap-2">
        {applyMsg && (
          <span className="max-w-md truncate text-xs text-success" title={applyMsg}>
            {applyMsg}
          </span>
        )}
        <Button size="sm" variant="secondary" onClick={handleApply}>
          <ShieldCheck size={14} /> {t("proxy.applyConfig")}
        </Button>
        {running ? (
          <Button size="sm" variant="outline" onClick={() => void stopProxy()}>
            <Square size={14} /> {t("proxy.stop")}
          </Button>
        ) : (
          <Button size="sm" onClick={() => void startProxy()}>
            <Play size={14} /> {t("proxy.start")}
          </Button>
        )}
      </div>
    </div>
  );
}
