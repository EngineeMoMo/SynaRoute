import { useEffect, useState } from "react";
import { api } from "@/lib/bridge";
import { Button } from "@/components/ui/Button";
import { Badge } from "@/components/ui/Badge";
import { AlertTriangle, Database, Loader2, X } from "lucide-react";
import { useT } from "@/lib/useT";
import type { TFunc } from "@/lib/i18n";
import type { CcSwitchCandidate, CcSwitchImportReport, CcSwitchScanResult } from "@/types";

/**
 * 分类 → 显示名。**走 `nav.*` 词条，不在本文件里写字面量**。
 *
 * 原先这里硬编码着「Claude 桌面端」之类的中文，于是英文界面下候选行的分类徽标是中文、
 * 而侧栏同一个东西是英文。复用 `nav.*` 也顺带保证两处永远一致（日后改名不会漏掉这里）。
 *
 * 认不出的分类 id 原样回显，而不是显示空白 —— 那样用户至少知道是哪个 id 没被识别。
 */
function categoryLabel(t: TFunc, id: string): string {
  const label = t(`nav.${id}`);
  return label === `nav.${id}` ? id : label;
}

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
  const t = useT();

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
            <h2 className="text-base font-semibold text-text-primary">{t("ccswitch.title")}</h2>
          </div>
          <button
            onClick={onClose}
            className="rounded p-1 text-text-secondary hover:bg-surface-hover"
            aria-label={t("common.close")}
          >
            <X size={18} />
          </button>
        </header>

        <div className="flex-1 overflow-y-auto px-5 py-4">
          {loading && (
            <div className="flex items-center gap-2 py-8 text-sm text-text-secondary">
              <Loader2 size={16} className="animate-spin" /> {t("ccswitch.scanning")}
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
                {t("ccswitch.reportSummary", {
                  ok: report.imported,
                  skipped: report.skipped,
                  failed: report.failed,
                })}
              </div>
              <ul className="mt-2 space-y-1 text-xs text-text-secondary">
                {report.outcomes.map((o) => (
                  <li key={o.sourceId}>
                    <span className="text-text-primary">{o.name || o.sourceId}</span> — {o.detail}
                  </li>
                ))}
              </ul>
              {/* 原文用 <strong> 强调「未接入」。改走词条后不能再夹 JSX 标签，
                  故把强调交给整句语气（词条里写明「不会自动生效」），
                  否则两种语言各要一套 JSX 拼装，日后必然漂移。 */}
              <p className="mt-2 text-xs text-text-secondary">{t("ccswitch.notAppliedNote")}</p>
            </div>
          )}

          {scan && !report && (
            <>
              <p className="mb-3 break-all text-xs text-text-secondary">
                {t("ccswitch.source", { path: scan.dbPath })}
              </p>

              {importable.length === 0 && (
                <p className="py-4 text-sm text-text-secondary">{t("ccswitch.nothingToImport")}</p>
              )}

              {importable.map((c) => (
                <CandidateRow
                  key={c.sourceId}
                  c={c}
                  t={t}
                  checked={picked.has(c.sourceId)}
                  onToggle={() => toggle(c.sourceId)}
                />
              ))}

              {blocked.length > 0 && (
                <details className="mt-4">
                  <summary className="cursor-pointer text-sm text-text-secondary">
                    {t("ccswitch.blockedSummary", { n: blocked.length })}
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
            {report
              ? t("ccswitch.canClose")
              : t("ccswitch.pickedCount", { n: picked.size, total: importable.length })}
          </span>
          <div className="flex gap-2">
            <Button variant="secondary" onClick={onClose}>
              {report ? t("common.close") : t("common.cancel")}
            </Button>
            {!report && (
              <Button onClick={doImport} disabled={picked.size === 0 || importing}>
                {importing ? t("ccswitch.importing") : t("ccswitch.importN", { n: picked.size })}
              </Button>
            )}
          </div>
        </footer>
      </div>
    </div>
  );
}

function CandidateRow({ c, t, checked, onToggle }: {
  c: CcSwitchCandidate;
  /** 从父组件传入而非各自 `useT()`：本组件按候选条数渲染 N 份，没必要每份都建一次 */
  t: TFunc;
  checked: boolean;
  onToggle: () => void;
}) {
  return (
    <label className="mb-2 flex cursor-pointer items-start gap-3 rounded border border-border px-3 py-2 hover:bg-surface-hover">
      <input type="checkbox" checked={checked} onChange={onToggle} className="mt-1" />
      <div className="min-w-0 flex-1">
        <div className="flex flex-wrap items-center gap-2">
          <span className="font-medium text-text-primary">{c.name}</span>
          {/* 认不出分类时退回 appType（后端原样给的串），不显示空徽标 */}
          <Badge variant="neutral">
            {c.categoryId ? categoryLabel(t, c.categoryId) : c.appType}
          </Badge>
          {c.protocol && <Badge variant="neutral">{c.protocol}</Badge>}
          {c.isCurrent && <Badge variant="success">{t("ccswitch.isCurrent")}</Badge>}
        </div>
        <div className="mt-1 break-all text-xs text-text-secondary">
          {c.baseUrl}
          {c.defaultModel && <> · {t("ccswitch.defaultModel", { name: c.defaultModel })}</>}
        </div>
        <div className="mt-0.5 font-mono text-xs text-text-secondary">{c.secretMasked}</div>
      </div>
    </label>
  );
}
