import { Button } from "@/components/ui/Button";
import type { ProxyState } from "@/types";
import { useStore } from "@/store";
import { useT } from "@/lib/useT";
import { Play, Square, Copy, Waypoints, ArrowRight } from "lucide-react";
import { ToolConfigPreviewButton } from "@/components/ToolConfigPreview";

/** 顶部代理状态条：显示端点、运行状态，提供启停（FR-008/FR-019） */
export function ProxyStatusBar({ proxy }: { proxy: ProxyState | null }) {
  const { startProxy, stopProxy, keys } = useStore();
  const t = useT();

  const enabledCount = keys.filter((k) => k.enabled).length;
  const running = proxy?.status === "running";
  const endpoint = proxy?.port ? `http://127.0.0.1:${proxy.port}` : "—";

  const routeModeLabel =
    enabledCount <= 1 ? t("proxy.directMode") : t("proxy.failoverMode", { n: enabledCount });

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

      <span
        className={`flex items-center gap-1 rounded-control px-1.5 py-1 ${
          enabledCount > 1 ? "text-primary" : "text-text-muted"
        }`}
        title={routeModeLabel}
        aria-label={routeModeLabel}
      >
        {enabledCount > 1 ? <Waypoints size={15} /> : <ArrowRight size={15} />}
        <span className="text-xs font-medium tabular-nums">{enabledCount}</span>
      </span>

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
        <ToolConfigPreviewButton />
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
