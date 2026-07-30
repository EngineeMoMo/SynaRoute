import { useEffect, useState } from "react";
import { api } from "@/lib/bridge";
import { Button } from "@/components/ui/Button";
import { Badge } from "@/components/ui/Badge";
import { AlertTriangle, Database, Loader2, X } from "lucide-react";
import type { CcSwitchCandidate, CcSwitchImportReport, CcSwitchScanResult } from "@/types";

/** 分类 → 中文名（与侧栏口径一致） */
const CATEGORY_LABEL: Record<string, string> = {
  "claude-cli": "Claude CLI",
  "claude-desktop": "Claude 桌面端",
  codex: "Codex",
};

/**
 * 从 cc-switch 导入历史 Key。
 *
 * 三条产品约定（与后端一致，勿擅改）：
 * 1. **只读** cc-switch 的库，绝不改动/删除它的数据 —— 用户可能还在用它；
 * 2. **导入后不接入** —— 只把 Key 存进 SynaRoute，不动任何客户端配置；
 * 3. **默认不勾选** 已存在/不可导入项，且必须逐条显式勾选，不做「一键全导」。
 */
export function CcSwitchImportDialog({ onClose, onImported }: {
  onClose: () => void;
  onImported: () => void;
}) {
  const [scan, setScan] = useState<CcSwitchScanResult | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [picked, setPicked] = useState<Set<string>>(new Set());
  const [importing, setImporting] = useState(false);
  const [report, setReport] = useState<CcSwitchImportReport | null>(null);

  useEffect(() => {
    let alive = true;
    (async () => {
      try {
        const r = await api.scanCcswitch();
        if (!alive) return;
        setScan(r);
        // 只预勾选「确实可导入」的项：已重复 / 官方档 / 不支持端一律不预选。
        setPicked(new Set(r.candidates.filter((c) => !c.skipReason).map((c) => c.sourceId)));
      } catch (e) {
        if (alive) setError(String(e));
      } finally {
        if (alive) setLoading(false);
      }
    })();
    return () => {
      alive = false;
    };
  }, []);

  const importable = (scan?.candidates ?? []).filter((c) => !c.skipReason);
  const blocked = (scan?.candidates ?? []).filter((c) => c.skipReason);

  const toggle = (id: string) =>
    setPicked((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });

  async function doImport() {
    setImporting(true);
    try {
      const r = await api.importFromCcswitch([...picked]);
      setReport(r);
      if (r.imported > 0) onImported();
    } catch (e) {
      setError(String(e));
    } finally {
      setImporting(false);
    }
  }

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/40 p-4">
      <div className="flex max-h-[85vh] w-full max-w-3xl flex-col rounded-card border border-border bg-surface shadow-xl">
        <header className="flex items-center justify-between border-b border-border px-5 py-4">
          <div className="flex items-center gap-2">
            <Database size={18} className="text-text-secondary" />
            <h2 className="text-base font-semibold text-text-primary">从 cc-switch 导入 Key</h2>
          </div>
          <button
            onClick={onClose}
            className="rounded p-1 text-text-secondary hover:bg-surface-hover"
            aria-label="关闭"
          >
            <X size={18} />
          </button>
        </header>

        <div className="flex-1 overflow-y-auto px-5 py-4">
          {loading && (
            <div className="flex items-center gap-2 py-8 text-sm text-text-secondary">
              <Loader2 size={16} className="animate-spin" /> 正在读取 cc-switch 配置库…
            </div>
          )}

          {error && (
            <div className="flex items-start gap-2 rounded border border-danger/40 bg-danger/10 p-3 text-sm text-text-primary">
              <AlertTriangle size={16} className="mt-0.5 shrink-0 text-danger" />
              <div className="break-all">{error}</div>
            </div>
          )}

          {report && (
            <div className="mb-4 rounded border border-border bg-surface-hover p-3 text-sm">
              <div className="font-medium text-text-primary">
                导入完成：成功 {report.imported} · 跳过 {report.skipped} · 失败 {report.failed}
              </div>
              <ul className="mt-2 space-y-1 text-xs text-text-secondary">
                {report.outcomes.map((o) => (
                  <li key={o.sourceId}>
                    <span className="text-text-primary">{o.name || o.sourceId}</span> — {o.detail}
                  </li>
                ))}
              </ul>
              <p className="mt-2 text-xs text-text-secondary">
                已导入的 Key 尚<strong>未接入</strong>任何客户端配置。需要接入时在对应分类里点「接入」。
              </p>
            </div>
          )}

          {scan && !report && (
            <>
              <p className="mb-3 break-all text-xs text-text-secondary">
                数据来源：{scan.dbPath}（只读，不会修改 cc-switch 的任何数据）
              </p>

              {importable.length === 0 && (
                <p className="py-4 text-sm text-text-secondary">没有可导入的档。</p>
              )}

              {importable.map((c) => (
                <CandidateRow
                  key={c.sourceId}
                  c={c}
                  checked={picked.has(c.sourceId)}
                  onToggle={() => toggle(c.sourceId)}
                />
              ))}

              {blocked.length > 0 && (
                <details className="mt-4">
                  <summary className="cursor-pointer text-sm text-text-secondary">
                    不可导入 {blocked.length} 条（官方登录档 / 已存在 / 暂不支持的端）
                  </summary>
                  <div className="mt-2 space-y-2">
                    {blocked.map((c) => (
                      <div
                        key={c.sourceId}
                        className="rounded border border-border/60 px-3 py-2 text-xs text-text-secondary"
                      >
                        <span className="text-text-primary">{c.name}</span>
                        <span className="ml-2 opacity-70">[{c.appType}]</span>
                        <div className="mt-0.5">{c.skipReason}</div>
                      </div>
                    ))}
                  </div>
                </details>
              )}
            </>
          )}
        </div>

        <footer className="flex items-center justify-between border-t border-border px-5 py-3">
          <span className="text-xs text-text-secondary">
            {report ? "可关闭本窗口" : `已选 ${picked.size} / 可导入 ${importable.length}`}
          </span>
          <div className="flex gap-2">
            <Button variant="secondary" onClick={onClose}>
              {report ? "关闭" : "取消"}
            </Button>
            {!report && (
              <Button onClick={doImport} disabled={picked.size === 0 || importing}>
                {importing ? "导入中…" : `导入选中 ${picked.size} 条`}
              </Button>
            )}
          </div>
        </footer>
      </div>
    </div>
  );
}

function CandidateRow({ c, checked, onToggle }: {
  c: CcSwitchCandidate;
  checked: boolean;
  onToggle: () => void;
}) {
  return (
    <label className="mb-2 flex cursor-pointer items-start gap-3 rounded border border-border px-3 py-2 hover:bg-surface-hover">
      <input type="checkbox" checked={checked} onChange={onToggle} className="mt-1" />
      <div className="min-w-0 flex-1">
        <div className="flex flex-wrap items-center gap-2">
          <span className="font-medium text-text-primary">{c.name}</span>
          <Badge variant="neutral">{CATEGORY_LABEL[c.categoryId ?? ""] ?? c.appType}</Badge>
          {c.protocol && <Badge variant="neutral">{c.protocol}</Badge>}
          {c.isCurrent && <Badge variant="success">cc-switch 当前生效</Badge>}
        </div>
        <div className="mt-1 break-all text-xs text-text-secondary">
          {c.baseUrl}
          {c.defaultModel && <> · 默认模型 {c.defaultModel}</>}
        </div>
        <div className="mt-0.5 font-mono text-xs text-text-secondary">{c.secretMasked}</div>
      </div>
    </label>
  );
}
