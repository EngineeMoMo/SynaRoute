import { useCallback, useEffect, useMemo, useState } from "react";
import { api } from "@/lib/bridge";
import { useT } from "@/lib/useT";
import type {
  CodexProviderTargetList,
  CodexSessionIndexAudit,
  CodexSessionList,
  CodexSessionRow,
} from "@/types";
import { ToggleRow } from "@/components/ToggleRow";
import { AlertTriangle, RefreshCw, Trash2, Wand2 } from "lucide-react";
import { SessionTable } from "@/components/SessionTable";

/**
 * Codex 会话管理页。
 *
 * 这个页面的主要理由是**「会话记的 provider」这一列** —— 每条 thread 自带一个 provider
 * 身份（rollout 首行的 `session_meta.model_provider`），它会覆盖 `config.toml` 的根
 * `model_provider`。于是从官方登录切到 SynaRoute 之后，旧对话仍然打 `api.openai.com`、
 * 拿我们写的占位凭据换回 401，而新建对话完全正常。用户在这里能一眼看出是哪些会话。
 *
 * 接入（点「启动」）时后端已经会自动把它们改过来，所以这一页多数时候应该是「全部一致」。
 * 它存在的价值有两条：**当自动同步没做完时给出解释**（Codex 当时正开着、文件被独占），
 * 以及**给出补做的入口** —— 此前唯一的补救是「先停止再启动」整个代理，为了改几行首行
 * 元数据去重启整条转发链路，代价与目的完全不成比例。
 *
 * 删除会先把 rollout 备份到应用数据目录（保留 30 天），但仍然走确认框：有备份不等于
 * 可以随手删。
 */
export function CodexSessionsPage() {
  const t = useT();
  const [data, setData] = useState<CodexSessionList | null>(null);
  const [targets, setTargets] = useState<CodexProviderTargetList | null>(null);
  const [audit, setAudit] = useState<CodexSessionIndexAudit | null>(null);
  const [target, setTarget] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [picked, setPicked] = useState<Set<string>>(new Set());
  const [confirming, setConfirming] = useState(false);
  const [note, setNote] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [syncing, setSyncing] = useState(false);
  const [confirmingSync, setConfirmingSync] = useState(false);
  const [query, setQuery] = useState("");
  const [onlyBad, setOnlyBad] = useState(false);
  const [onlyGone, setOnlyGone] = useState(false);

  const load = useCallback(async () => {
    try {
      const [next, tl, ia] = await Promise.all([
        api.listCodexSessions(),
        api.listCodexProviderTargets(),
        api.auditCodexSessionIndex(),
      ]);
      // 后端返回的形状做一次校验再入 state：本页多处直接解引用 row 字段，而 render 抛异常
      // 会让 React 卸载整棵树 → 整窗口白屏（用量页真机反馈过这种事故）。
      setData({
        ...next,
        rows: Array.isArray(next.rows) ? next.rows.filter(isValidRow) : [],
      });
      setTargets(tl);
      setAudit(ia);
      // 选中项优先回填上次选过的，其次是我们自己那个 —— 而不是「当前生效的那个」：
      // 用户来这一页通常正是因为当前生效的与会话记的不一致。
      setTarget((prev) => prev || tl.prefs.lastTarget || tl.ours || tl.current);
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

  const mismatched = data?.stats.mismatched ?? 0;
  const modelGone = data?.stats.modelGone ?? 0;

  /**
   * 筛选是纯前端的：整份列表已经在手里，为一次筛选再跑一趟 IPC + 重扫几百个文件毫无道理。
   *
   * 🔴 **这不是把 `stats.mismatched` 重算一遍**（那一位仍然只来自后端，见
   * `tests/codexSessionPage.test.ts`）—— 这里算的是「当前筛选下显示几行」，
   * 而统计卡永远报**全量**。两个数字回答的是不同问题，所以刻意不共用。
   */
  const shown = useMemo(() => {
    const rows = data?.rows ?? [];
    const q = query.trim().toLowerCase();
    return rows.filter((r) => {
      if (onlyBad && !(data?.currentProvider && r.provider !== data.currentProvider)) return false;
      if (onlyGone && !r.modelUnserviceable) return false;
      if (!q) return true;
      return [r.title, r.cwd, r.provider, r.model, r.project, r.relPath]
        .join("")
        .toLowerCase()
        .includes(q);
    });
  }, [data, query, onlyBad, onlyGone]);

  /**
   * 点「立刻同步」会改到几条 —— 用**用户正在看的这份列表**算，因为确认框要说的就是他看到的
   * 那些（同大脑聚合 Phase2a 那条：让用户确认的必须是他看到的那份）。刻意**不**取
   * `stats.mismatched`：那一位是与 `config.toml` 当前生效的 provider 比，而这里比的是他在
   * 下拉里选的目标，两者未必是同一个 provider。真正权威的数字是同步完那条结果消息。
   */
  const willChange = useMemo(
    () => (data?.rows ?? []).filter((r) => r.provider !== target).length,
    [data, target],
  );

  /** 选中但被当前筛选藏起来的条数 —— 它们照样会被删，所以必须在界面上点出来。 */
  const hiddenPicked = useMemo(() => {
    const visible = new Set(shown.map((r) => r.relPath));
    return [...picked].filter((p) => !visible.has(p)).length;
  }, [picked, shown]);

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

  const doSync = async () => {
    setSyncing(true);
    setConfirmingSync(false);
    try {
      setNote(await api.syncCodexSessions(target));
    } catch (e) {
      setNote(String(e));
    } finally {
      setSyncing(false);
      await load();
    }
  };

  const doPrune = async () => {
    try {
      setNote(await api.pruneCodexSessionIndex());
    } catch (e) {
      setNote(String(e));
    } finally {
      await load();
    }
  };

  const doToggleAuto = async (enabled: boolean) => {
    // 先落乐观更新再写盘：这个开关的写入是一次文件 IO，等它返回会让复选框卡一下。
    setTargets((prev) =>
      prev ? { ...prev, prefs: { ...prev.prefs, autoSyncDisabled: !enabled } } : prev,
    );
    try {
      await api.setCodexSessionAutoSync(enabled);
    } catch (e) {
      setNote(String(e));
      await load(); // 写失败就把界面拉回磁盘上的真值，别留一个假的勾
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

  const srcLabel = useMemo(
    () => (s: string) =>
      s === "config"
        ? t("sessions.srcConfig")
        : s === "rollout"
          ? t("sessions.srcRollout")
          : t("sessions.srcSqlite"),
    [t],
  );

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

      {data && (
        <div className="rounded-md border border-border">
          <dl className="grid grid-cols-2 gap-x-6 gap-y-2 p-4 text-sm sm:grid-cols-4">
            <Stat label={t("sessions.statTotal")} value={String(data.stats.total)} />
            <Stat label={t("sessions.statActive")} value={String(data.stats.active)} />
            <Stat label={t("sessions.statArchived")} value={String(data.stats.archived)} />
            <Stat
              label={t("sessions.statMismatched")}
              value={String(mismatched)}
              danger={mismatched > 0}
            />
          </dl>
          <div className="border-t border-border px-4 py-2 text-xs text-text-muted">
            <span>{t("sessions.statDb")}: </span>
            <code className="text-text">{data.stats.dbPath || t("sessions.dbNone")}</code>
            {data.stats.inDb < data.stats.total && (
              <p className="mt-1">
                {t("sessions.inDbHint", { n: data.stats.inDb, total: data.stats.total })}
              </p>
            )}
          </div>
        </div>
      )}

      {/* 同步区：目标下拉 + 立刻同步 + 自动同步开关 */}
      {targets && (
        <div className="space-y-3 rounded-md border border-border p-4">
          <div className="flex flex-wrap items-end gap-3">
            <label className="flex flex-col gap-1 text-sm">
              <span className="text-text-muted">{t("sessions.targetLabel")}</span>
              <select
                value={target}
                onChange={(e) => setTarget(e.target.value)}
                className="min-w-[18rem] rounded-md border border-border bg-surface px-2 py-1.5 text-sm text-text"
              >
                {targets.targets.map((o) => (
                  <option key={o.id} value={o.id}>
                    {o.id}
                    {"  ·  "}
                    {o.sources.map(srcLabel).join(" / ")}
                    {o.isCurrent ? `  ·  ${t("sessions.current").replace("：", "").replace(":", "")}` : ""}
                    {o.id === targets.ours ? `  ·  ${t("sessions.targetOurs")}` : ""}
                  </option>
                ))}
              </select>
            </label>
            <button
              type="button"
              disabled={syncing || !target}
              onClick={() => setConfirmingSync(true)}
              className="inline-flex items-center gap-1.5 rounded-md bg-primary px-3 py-1.5 text-sm text-primary-foreground disabled:opacity-50"
            >
              <Wand2 className="h-4 w-4" />
              {syncing ? t("sessions.syncing") : t("sessions.syncNow")}
            </button>
          </div>
          <p className="text-xs text-text-muted">{t("sessions.targetHint")}</p>
          <p className="text-xs text-text-muted">{t("sessions.syncHint")}</p>
          {/* 改用户的对话文件之前先让他看清会动几条、动成什么 —— 同大脑聚合落盘前那一屏预览 */}
          {confirmingSync && (
            <div className="rounded-md border border-warning/50 bg-warning/8 p-3">
              <p className="font-medium text-text">{t("sessions.syncConfirmTitle")}</p>
              <p className="mt-1 text-sm text-text-muted">
                {willChange > 0
                  ? t("sessions.syncConfirmBody", { n: willChange, target })
                  : t("sessions.syncConfirmNone", { total: data?.stats.total ?? 0, target })}
              </p>
              <div className="mt-3 flex gap-2">
                <button
                  type="button"
                  onClick={() => void doSync()}
                  className="rounded-md bg-primary px-3 py-1.5 text-sm text-primary-foreground"
                >
                  {t("sessions.syncConfirmOk")}
                </button>
                <button
                  type="button"
                  onClick={() => setConfirmingSync(false)}
                  className="rounded-md border border-border px-3 py-1.5 text-sm text-text"
                >
                  {t("sessions.cancel")}
                </button>
              </div>
            </div>
          )}
          <ToggleRow
            title={t("sessions.autoSync")}
            desc={t("sessions.autoSyncHint")}
            checked={!targets.prefs.autoSyncDisabled}
            onChange={(v) => void doToggleAuto(v)}
          />
        </div>
      )}

      {mismatched > 0 && (
        <div className="flex gap-2 rounded-md border border-warning/40 bg-warning/8 p-3 text-sm text-text">
          <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0 text-warning" />
          <span>{t("sessions.mismatchHint", { n: mismatched })}</span>
        </div>
      )}

      {/* 🔴 只有代理侧能给的那一位：provider 对了、模型却没人服务得了 */}
      {modelGone > 0 && (
        <div className="flex gap-2 rounded-md border border-danger/40 bg-danger/8 p-3 text-sm text-text">
          <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0 text-danger" />
          <span>{t("sessions.modelGoneHint", { n: modelGone })}</span>
        </div>
      )}

      {/* 索引孤儿：只在真有孤儿时才出现整块（恒 0 的一行是噪音） */}
      {audit && audit.orphans > 0 && (
        <div className="space-y-2 rounded-md border border-warning/40 bg-warning/8 p-3 text-sm text-text">
          <p className="font-medium">{t("sessions.indexTitle")}</p>
          <p>{t("sessions.indexOrphans", { n: audit.orphans })}</p>
          <p className="text-xs text-text-muted">{t("sessions.indexBackupNote")}</p>
          <button
            type="button"
            onClick={() => void doPrune()}
            className="rounded-md border border-border px-3 py-1.5 text-xs text-text hover:bg-surface-hover"
          >
            {t("sessions.indexPrune")}
          </button>
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
        <div className="flex flex-wrap items-center gap-3 rounded-md border border-border bg-surface-hover p-3">
          <span className="text-sm text-text">{t("sessions.selected", { n: picked.size })}</span>
          {/* 勾完再筛选会让选中项藏起来 —— 藏着的条目照样会被删，所以必须点出来有几条 */}
          {hiddenPicked > 0 && (
            <span className="text-xs text-warning">
              {t("sessions.pickedHidden", { n: hiddenPicked })}
            </span>
          )}
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
        <>
          <div className="flex flex-wrap items-center gap-3">
            <input
              type="search"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder={t("sessions.filterPlaceholder")}
              aria-label={t("sessions.filter")}
              className="min-w-[16rem] flex-1 rounded-md border border-border bg-surface px-2.5 py-1.5 text-sm text-text"
            />
            <label className="flex items-center gap-1.5 text-sm text-text-muted">
              <input type="checkbox" checked={onlyBad} onChange={(e) => setOnlyBad(e.target.checked)} />
              {t("sessions.onlyMismatched")}
            </label>
            <label className="flex items-center gap-1.5 text-sm text-text-muted">
              <input
                type="checkbox"
                checked={onlyGone}
                onChange={(e) => setOnlyGone(e.target.checked)}
              />
              {t("sessions.onlyModelGone")}
            </label>
            {shown.length !== data.rows.length && (
              <span className="text-xs text-text-muted">
                {t("sessions.filtered", { n: shown.length, total: data.rows.length })}
              </span>
            )}
          </div>
          <SessionTable
            rows={shown}
            currentProvider={data.currentProvider}
            picked={picked}
            onToggle={toggle}
            onExport={(r) => void doExport(r)}
          />
        </>
      )}
    </div>
  );
}

function Stat({ label, value, danger }: { label: string; value: string; danger?: boolean }) {
  return (
    <div>
      <dt className="text-text-muted">{label}</dt>
      <dd className={`text-lg font-semibold ${danger ? "text-danger" : "text-text"}`}>{value}</dd>
    </div>
  );
}

/** 后端返回的行必须有这几个字段才敢渲染（见 `load` 里那段注释）。 */
function isValidRow(r: unknown): r is CodexSessionRow {
  const o = r as Partial<CodexSessionRow> | null;
  return !!o && typeof o.relPath === "string" && typeof o.provider === "string";
}
