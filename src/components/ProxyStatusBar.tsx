import { Button } from "@/components/ui/Button";
import type { ProxyState } from "@/types";
import { useStore } from "@/store";
import { useT } from "@/lib/useT";
import { Play, Square, Copy, Waypoints, ArrowRight, AlertTriangle } from "lucide-react";
import { ToolConfigPreviewButton } from "@/components/ToolConfigPreview";

/** 顶部代理状态条：显示端点、运行状态，提供启停（FR-008/FR-019） */
export function ProxyStatusBar({ proxy }: { proxy: ProxyState | null }) {
  // 细粒度订阅：整店解构（`useStore()`）会订阅**所有**字段，而 LogsPage 每 2s 就
  // `set({ events })` 一次 —— 那会让本组件（以及所有整店解构的组件）每 2s 全量重渲染，
  // 哪怕用户根本不在日志页。actions 是稳定引用，单独取不会引入额外渲染。
  const startProxy = useStore((s) => s.startProxy);
  const stopProxy = useStore((s) => s.stopProxy);
  const keys = useStore((s) => s.keys);
  const t = useT();

  const enabledKeys = keys.filter((k) => k.enabled);
  const enabledCount = enabledKeys.length;
  const running = proxy?.status === "running";
  const endpoint = proxy?.port ? `http://127.0.0.1:${proxy.port}` : "—";

  // 健康度聚合：此前状态条只看 enabled 数，全部 Key 探测为 down 时仍显示绿点「运行中/N」,
  // 用户必须点进列表才知道一个都用不了。数据本来就在 keys[].health.status，只是没被用。
  //
  // 口径说明：只把**明确 down** 计为不可用。unknown（未探测过）/checking 不算——
  // 探测与路由是刻意分离的（见 health::is_candidate：真实流量才是可用性裁判），
  // 把未探测算成故障会造成刚加完 Key 就报红的误导。
  const downCount = enabledKeys.filter((k) => k.health.status === "down").length;
  const allDown = enabledCount > 0 && downCount === enabledCount;
  const someDown = downCount > 0 && !allDown;

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
            allDown ? "bg-danger" : running ? "bg-success" : "bg-text-muted"
          }`}
        />
        <span className="text-sm font-medium text-text-primary">
          {running ? t("proxy.running") : t("proxy.stopped")}
        </span>
      </div>

      {/* 健康度告警：全部/部分 Key 明确不可用时，在状态条上直接可见，而不是只有点进列表才能发现 */}
      {allDown && (
        <span
          className="flex items-center gap-1 rounded-control bg-danger/10 px-1.5 py-1 text-xs font-medium text-danger"
          title={t("proxy.allKeysDown", { n: enabledCount })}
        >
          <AlertTriangle size={13} />
          {t("proxy.allKeysDown", { n: enabledCount })}
        </span>
      )}
      {someDown && (
        <span
          className="flex items-center gap-1 rounded-control bg-warning/10 px-1.5 py-1 text-xs font-medium text-warning"
          title={t("proxy.someKeysDown", { down: downCount, total: enabledCount })}
        >
          <AlertTriangle size={13} />
          {t("proxy.someKeysDown", { down: downCount, total: enabledCount })}
        </span>
      )}

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
