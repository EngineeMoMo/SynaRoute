import { useCallback, useEffect, useState } from "react";
import { api } from "@/lib/bridge";
import { useStore } from "@/store";
import { useT } from "@/lib/useT";
import type { ToolConfigPreview as Preview } from "@/types";
import { Button } from "@/components/ui/Button";
import { FileCode2, RefreshCw, X, Copy, FolderOpen, AlertTriangle } from "lucide-react";

/**
 * 目标工具配置只读预览。
 * 三端严格分离：Claude CLI = settings.json；Codex = config.toml+auth.json；桌面 = claude_desktop_config。
 * 不做自由编辑（阶段 4 未做）；token 已在后端脱敏。
 */
export function ToolConfigPreviewPanel({ open, onClose }: { open: boolean; onClose: () => void }) {
  const { activeCategory } = useStore();
  const t = useT();
  const [data, setData] = useState<Preview | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);

  const load = useCallback(async () => {
    setLoading(true);
    setErr(null);
    try {
      const p = await api.getToolConfigPreview(activeCategory);
      setData(p);
    } catch (e) {
      setErr(e instanceof Error ? e.message : String(e));
      setData(null);
    } finally {
      setLoading(false);
    }
  }, [activeCategory]);

  useEffect(() => {
    if (open) void load();
  }, [open, load]);

  if (!open) return null;

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/40 p-4"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div className="flex max-h-[85vh] w-full max-w-2xl flex-col overflow-hidden rounded-card border border-border bg-surface shadow-lg">
        <div className="flex items-center gap-2 border-b border-border px-4 py-3">
          <FileCode2 size={16} className="text-primary" />
          <h2 className="flex-1 text-sm font-semibold text-text-primary">{t("toolConfig.title")}</h2>
          <Button size="sm" variant="ghost" onClick={() => void load()} disabled={loading} title={t("common.refresh")}>
            <RefreshCw size={14} className={loading ? "animate-spin" : ""} />
          </Button>
          <button type="button" onClick={onClose} className="rounded p-1 hover:bg-surface-hover" aria-label={t("common.close")}>
            <X size={16} />
          </button>
        </div>

        <div className="flex-1 space-y-3 overflow-y-auto px-4 py-3">
          {err && <p className="text-xs text-danger">{err}</p>}
          {loading && !data && <p className="text-xs text-text-muted">{t("common.loading")}</p>}
          {data && (
            <>
              <p className="text-xs leading-relaxed text-text-secondary">{data.summary}</p>
              {/* 接入被其他工具接管（cc-switch 重写 _meta.json）：档还在、UI 也显示已接入，
                  但桌面端实际走的是别人那一档。这是「接入了但不生效」这类无头案的唯一线索，
                  必须醒目且给出可操作的恢复方式。 */}
              {data.takeoverWarning && (
                <div className="flex items-start gap-2 rounded-control border border-warning/30 bg-warning/8 px-3 py-2">
                  <AlertTriangle size={14} className="mt-0.5 shrink-0 text-warning" />
                  <p className="text-[11px] leading-relaxed text-warning">{data.takeoverWarning}</p>
                </div>
              )}
              {data.files.map((f) => (
                <div key={f.path} className="rounded-control border border-border bg-surface-elevated">
                  <div className="flex items-center gap-2 border-b border-border px-3 py-2">
                    <span className="min-w-0 flex-1 truncate font-mono text-[11px] text-text-secondary" title={f.path}>
                      {f.path}
                    </span>
                    <span
                      className={`shrink-0 rounded px-1.5 py-0.5 text-[10px] ${
                        f.exists ? "bg-success/15 text-success" : "bg-warning/15 text-warning"
                      }`}
                    >
                      {f.exists ? t("toolConfig.exists") : t("toolConfig.missing")}
                    </span>
                    <span className="shrink-0 rounded bg-surface-hover px-1.5 py-0.5 text-[10px] text-text-muted">
                      {f.format}
                    </span>
                    {f.exists && f.content && (
                      <button
                        type="button"
                        className="rounded p-1 hover:bg-surface-hover"
                        title={t("toolConfig.copy")}
                        onClick={() => void navigator.clipboard?.writeText(f.content ?? "")}
                      >
                        <Copy size={12} />
                      </button>
                    )}
                  </div>
                  <pre className="max-h-64 overflow-auto p-3 font-mono text-[11px] leading-relaxed text-text-primary">
                    {f.exists ? f.content || t("toolConfig.empty") : t("toolConfig.fileMissingHint")}
                  </pre>
                </div>
              ))}
            </>
          )}
        </div>

        <div className="flex items-center justify-end gap-2 border-t border-border px-4 py-3">
          <Button size="sm" variant="outline" onClick={onClose}>
            {t("common.close")}
          </Button>
        </div>
      </div>
    </div>
  );
}

/** 代理条上的「查看工具配置」入口 */
export function ToolConfigPreviewButton() {
  const [open, setOpen] = useState(false);
  const t = useT();
  return (
    <>
      <Button size="sm" variant="outline" onClick={() => setOpen(true)} title={t("toolConfig.title")}>
        <FolderOpen size={14} /> {t("toolConfig.button")}
      </Button>
      <ToolConfigPreviewPanel open={open} onClose={() => setOpen(false)} />
    </>
  );
}
