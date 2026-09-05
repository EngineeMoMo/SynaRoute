import { useCallback, useEffect, useMemo, useState } from "react";
import { api } from "@/lib/bridge";
import { useT } from "@/lib/useT";
import type { CodexSessionList, CodexSessionRow } from "@/types";
import { Badge } from "@/components/ui/Badge";
import { AlertTriangle, Download, RefreshCw, Trash2 } from "lucide-react";

/**
 * Codex 会话管理页。
 *
 * 这个页面的主要理由是**「会话记的 provider」这一列** —— 每条 thread 自带一个 provider
 * 身份（rollout 首行的 `session_meta.model_provider`），它会覆盖 `config.toml` 的根
 * `model_provider`。于是从官方登录切到 SynaRoute 之后，旧对话仍然打 `api.openai.com`、
 * 拿我们写的占位凭据换回 401，而新建对话完全正常。用户在这里能一眼看出是哪些会话。
 *
 * 接入（点「启动」）时后端已经会自动把它们改过来（见 `tools/codex_sessions.rs`），
 * 所以这一页多数时候应该是「全部一致」。它存在的价值是：**当自动同步没做完时给出解释**
 * —— 比如 Codex 当时正开着、文件被独占。
 *
 * 删除是本应用唯一会主动删用户对话记录的地方，故必须走确认框，且文案明说不可逆。
 */
export function CodexSessionsPage() {
  const t = useT();
  const [data, setData] = useState<CodexSessionList | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [picked, setPicked] = useState<Set<string>>(new Set());
  const [confirming, setConfirming] = useState(false);
  const [note, setNote] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const load = useCallback(async () => {
    try {
      const next = await api.listCodexSessions();
      // 后端返回的形状做一次校验再入 state：本页多处直接解引用 row 字段，而 render 抛异常
      // 会让 React 卸载整棵树 → 整窗口白屏（用量页真机反馈过这种事故）。
      setData({
        ...next,
        rows: Array.isArray(next.rows) ? next.rows.filter(isValidRow) : [],
      });
      setError(null);
    } catch (e) {
      setError(String(e));
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  // 选中项按 relPath 记，而不是按下标 —— 刷新后顺序可能变（新会话插到最前面），
  // 按下标记会让「删除选中」删错行。
  const toggle = (relPath: string) => {
    setPicked((prev) => {
      const next = new Set(prev);
      if (!next.delete(relPath)) next.add(relPath);
      return next;
    });
  };

  const mismatched = useMemo(() => {
    if (!data || !data.currentProvider) return [];
    return data.rows.filter((r) => r.provider !== data.currentProvider);
  }, [data]);

  const doDelete = async () => {
    setBusy(true);
    setConfirming(false);
    try {
      setNote(await api.deleteCodexSessions([...picked]));
      setPicked(new Set());
    } catch (e) {
      // 部分成功时后端返回 Err 并列出没删掉的那些 —— 原样展示，不要压成一句「失败」。
      setNote(String(e));
    } finally {
      setBusy(false);
      await load();
    }
  };

  const doExport = async (row: CodexSessionRow) => {
    try {
      // 后端直接落盘并返回完整路径 —— 不走 blob 下载（那依赖 WebView2 的下载行为，
      // 失败形态是「点了什么都不发生」）。
      setNote(`${t("sessions.exported")} ${await api.exportCodexSessionMarkdown(row.relPath)}`);
    } catch (e) {
      setNote(`${t("sessions.exportFailed")}: ${e}`);
    }
  };

  return (
    <div className="space-y-4">
      <header className="flex items-start justify-between gap-4">
        <div>
          <h1 className="text-xl font-semibold text-text">{t("sessions.title")}</h1>
          <p className="mt-1 max-w-3xl text-sm text-text-muted">{t("sessions.subtitle")}</p>
        </div>
        <button
          type="button"
          onClick={() => void load()}
          className="inline-flex shrink-0 items-center gap-1.5 rounded-md border border-border px-3 py-1.5 text-sm text-text hover:bg-surface-hover"
        >
          <RefreshCw className="h-4 w-4" />
          {t("sessions.refresh")}
        </button>
      </header>

      {data?.currentProvider && (
        <p className="text-sm text-text-muted">
          {t("sessions.current")}
          <code className="ml-1 rounded bg-surface-hover px-1.5 py-0.5 text-text">
            {data.currentProvider}
          </code>
        </p>
      )}

      {mismatched.length > 0 && (
        <div className="flex gap-2 rounded-md border border-warning/40 bg-warning/8 p-3 text-sm text-text">
          <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0 text-warning" />
          <span>{t("sessions.mismatchHint", { n: mismatched.length })}</span>
        </div>
      )}

      {(data?.unreadable || 0) > 0 && (
        <p className="text-sm text-text-muted">
          {t("sessions.unreadable", { n: data?.unreadable ?? 0 })}
        </p>
      )}
      {(data?.pathRejected || 0) > 0 && (
        <p className="text-sm text-text-muted">
          {t("sessions.pathRejected", { n: data?.pathRejected ?? 0 })}
        </p>
      )}

      {note && <p className="rounded-md bg-surface-hover p-3 text-sm text-text">{note}</p>}
      {error && (
        <p className="rounded-md border border-danger/40 bg-danger/8 p-3 text-sm text-text">
          {t("sessions.loadFailed")}: {error}
        </p>
      )}

      {picked.size > 0 && (
        <div className="flex items-center gap-3 rounded-md border border-border bg-surface-hover p-3">
          <span className="text-sm text-text">
            {t("sessions.selected", { n: picked.size })}
          </span>
          <button
            type="button"
            disabled={busy}
            onClick={() => setConfirming(true)}
            className="inline-flex items-center gap-1.5 rounded-md bg-danger px-3 py-1.5 text-sm text-white disabled:opacity-50"
          >
            <Trash2 className="h-4 w-4" />
            {t("sessions.deleteSelected")}
          </button>
        </div>
      )}

      {confirming && (
        <div className="rounded-md border border-danger/50 bg-danger/8 p-4">
          <p className="font-medium text-text">{t("sessions.confirmTitle")}</p>
          <p className="mt-1 text-sm text-text-muted">
            {t("sessions.confirmBody", { n: picked.size })}
          </p>
          <div className="mt-3 flex gap-2">
            <button
              type="button"
              onClick={() => void doDelete()}
              className="rounded-md bg-danger px-3 py-1.5 text-sm text-white"
            >
              {t("sessions.confirmOk")}
            </button>
            <button
              type="button"
              onClick={() => setConfirming(false)}
              className="rounded-md border border-border px-3 py-1.5 text-sm text-text"
            >
              {t("sessions.cancel")}
            </button>
          </div>
        </div>
      )}

      {data === null ? (
        <p className="text-sm text-text-muted">{t("sessions.loading")}</p>
      ) : data.rows.length === 0 ? (
        <p className="text-sm text-text-muted">{t("sessions.empty")}</p>
      ) : (
        <div className="overflow-x-auto rounded-md border border-border">
          <table className="w-full text-sm">
            <thead className="bg-surface-hover/60 text-left text-text-muted">
              <tr>
                <th className="w-8 px-3 py-2" />
                <th className="px-3 py-2">{t("sessions.colTime")}</th>
                <th className="px-3 py-2">{t("sessions.colProvider")}</th>
                <th className="px-3 py-2">{t("sessions.colCwd")}</th>
                <th className="px-3 py-2 text-right">{t("sessions.colSize")}</th>
                <th className="px-3 py-2 text-right">{t("sessions.colOps")}</th>
              </tr>
            </thead>
            <tbody>
              {data.rows.map((r) => {
                const bad = !!data.currentProvider && r.provider !== data.currentProvider;
                return (
                  <tr key={r.relPath} className="border-t border-border">
                    <td className="px-3 py-2">
                      <input
                        type="checkbox"
                        aria-label={r.relPath}
                        checked={picked.has(r.relPath)}
                        onChange={() => toggle(r.relPath)}
                      />
                    </td>
                    <td className="whitespace-nowrap px-3 py-2 text-text">
                      {formatTime(r.timestamp)}
                      {r.archived && (
                        <Badge variant="neutral" className="ml-2">
                          {t("sessions.archived")}
                        </Badge>
                      )}
                    </td>
                    <td className="px-3 py-2">
                      <code className={bad ? "text-danger" : "text-text"}>
                        {r.provider || "—"}
                      </code>
                      {bad && (
                        <Badge variant="danger" className="ml-2">
                          {t("sessions.mismatch")}
                        </Badge>
                      )}
                    </td>
                    <td className="max-w-[22rem] truncate px-3 py-2 text-text-muted" title={r.cwd}>
                      {r.cwd || "—"}
                    </td>
                    <td className="whitespace-nowrap px-3 py-2 text-right text-text-muted">
                      {formatBytes(r.bytes)}
                    </td>
                    <td className="whitespace-nowrap px-3 py-2 text-right">
                      <button
                        type="button"
                        onClick={() => void doExport(r)}
                        title={t("sessions.export")}
                        className="inline-flex items-center gap-1 rounded-md border border-border px-2 py-1 text-xs text-text hover:bg-surface-hover"
                      >
                        <Download className="h-3.5 w-3.5" />
                        {t("sessions.export")}
                      </button>
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}

/** 后端返回的行必须有这几个字段才敢渲染（见 `load` 里那段注释）。 */
function isValidRow(r: unknown): r is CodexSessionRow {
  const o = r as Partial<CodexSessionRow> | null;
  return !!o && typeof o.relPath === "string" && typeof o.provider === "string";
}

function formatTime(iso: string): string {
  if (!iso) return "—";
  const d = new Date(iso);
  return Number.isNaN(d.getTime()) ? iso : d.toLocaleString();
}

function formatBytes(n: number): string {
  if (!Number.isFinite(n) || n <= 0) return "—";
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}
