import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api } from "@/lib/bridge";
import type { SessionMaintenanceProgress as Progress } from "@/lib/sessionMaintenance";
import { SessionMaintenanceProgress } from "@/components/SessionMaintenanceProgress";
import { SessionIdRepairPanel } from "@/components/SessionIdRepairPanel";
import { useT } from "@/lib/useT";
import type {
  CodexProviderTargetList,
  CodexSessionIndexAudit,
  CodexSessionList,
  CodexSessionRow,
} from "@/types";
import { ToggleRow } from "@/components/ToggleRow";
import { FolderOpen, Wand2 } from "lucide-react";
import { SessionBrowser } from "@/components/SessionBrowser";
import { SessionDetailDialog } from "@/components/SessionDetailDialog";
import { ConfirmDialog } from "@/components/ui/ConfirmDialog";
import { normalizeSessionRow, retainSessionSelection, selectSessionPaths } from "@/lib/codexSessionBrowser";
import { Button } from "@/components/ui/Button";
import { RefreshButton } from "@/components/ui/RefreshButton";
import { InlineAlert } from "@/components/ui/InlineAlert";
import { PageHeader } from "@/components/ui/PageHeader";

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
export function chooseKnownTarget(
  options: CodexProviderTargetList,
  previous: string,
): string {
  const known = new Set(options.targets.filter((option) => option.available).map((o) => o.id));
  return [previous, options.prefs.lastTarget, options.ours, options.current].find((v) => known.has(v))
    ?? options.targets.find((option) => option.available)?.id
    ?? "";
}

export function CodexSessionsPage() {
  const t = useT();
  const [data, setData] = useState<CodexSessionList | null>(null);
  const [targets, setTargets] = useState<CodexProviderTargetList | null>(null);
  const [audit, setAudit] = useState<CodexSessionIndexAudit | null>(null);
  const [target, setTarget] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [picked, setPicked] = useState<Set<string>>(new Set());
  const [deleteRequest, setDeleteRequest] = useState<string[] | null>(null);
  const [archiveRequest, setArchiveRequest] = useState<{ paths: string[]; archived: boolean } | null>(null);
  const [detail, setDetail] = useState<CodexSessionRow | null>(null);
  const [note, setNote] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [syncing, setSyncing] = useState(false);
  const [repairing, setRepairing] = useState(false);
  const [syncProgress, setSyncProgress] = useState<Progress | null>(null);
  const [confirmingSync, setConfirmingSync] = useState(false);
  const [maintenanceError, setMaintenanceError] = useState("");
  const loadGeneration = useRef(0);
  const operationInFlight = useRef(false);

  const load = useCallback(async () => {
    const generation = ++loadGeneration.current;
    setError(null);
    try {
      const [sessionsResult, targetsResult, auditResult] = await Promise.allSettled([
        api.listCodexSessions(),
        api.listCodexProviderTargets(),
        api.auditCodexSessionIndex(),
      ]);
      if (generation !== loadGeneration.current) return;
      if (sessionsResult.status === "fulfilled") {
        const next = sessionsResult.value;
        const rows = Array.isArray(next.rows)
          ? next.rows.map(normalizeSessionRow).filter((row): row is CodexSessionRow => row !== null) : [];
        setData({ ...next, rows });
        setPicked((previous) => retainSessionSelection(previous, rows));
        setDetail((previous) => previous ? rows.find((row) => row.relPath === previous.relPath) ?? null : null);
      } else {
        setError(String(sessionsResult.reason));
      }
      if (targetsResult.status === "fulfilled") {
        setTargets(targetsResult.value);
        setTarget((previous) => chooseKnownTarget(targetsResult.value, previous));
      }
      if (auditResult.status === "fulfilled") setAudit(auditResult.value);
      setMaintenanceError([targetsResult, auditResult]
        .filter((result): result is PromiseRejectedResult => result.status === "rejected")
        .map((result) => String(result.reason)).join("\n"));
    } catch (e) {
      if (generation !== loadGeneration.current) return;
      setError(String(e));
    }
  }, []);

  useEffect(() => {
    void load();
    return () => { loadGeneration.current += 1; };
  }, [load]);

  const mismatched = data?.stats.mismatched ?? 0;
  const modelGone = data?.stats.modelGone ?? 0;

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

  const doDelete = async () => {
    if (busy || syncing || repairing || operationInFlight.current || !deleteRequest?.length) return;
    operationInFlight.current = true;
    const paths = deleteRequest;
    setBusy(true);
    setDeleteRequest(null);
    try {
      setNote(await api.deleteCodexSessions(paths));
      setPicked((previous) => selectSessionPaths(previous, paths, false));
    } catch (e) {
      // 部分成功时后端返回 Err 并列出没删掉的那些 —— 原样展示，不要压成一句「失败」。
      setNote(String(e));
    } finally {
      await load();
      setBusy(false);
      operationInFlight.current = false;
    }
  };

  const doArchive = async () => {
    if (busy || syncing || repairing || operationInFlight.current || !archiveRequest) return;
    operationInFlight.current = true;
    const request = archiveRequest;
    setArchiveRequest(null);
    setBusy(true);
    try {
      const result = await api.setCodexSessionsArchived(request.paths, request.archived);
      setPicked((previous) => selectSessionPaths(previous, [...result.updated, ...result.unchanged], false));
      setNote([
        t(request.archived ? "sessions.archiveResult" : "sessions.restoreResult", {
          n: result.updated.length, unchanged: result.unchanged.length, failed: result.failed.length,
        }),
        result.updated.length > 0 && result.backupDir ? t("sessions.archiveBackup", { path: result.backupDir }) : "",
        ...result.failed.map((failure) => `${failure.relPath}: ${failure.error}`),
        ...result.warnings,
      ].filter(Boolean).join("\n"));
    } catch (error) {
      setNote(String(error));
    } finally {
      await load();
      setBusy(false);
      operationInFlight.current = false;
    }
  };

  const doSync = async () => {
    if (busy || syncing || repairing || operationInFlight.current || !targets?.targets.some((option) => option.id === target && option.available)) return;
    operationInFlight.current = true;
    setSyncing(true);
    setSyncProgress({ phase: "scanning", completed: 0, total: 0 });
    setConfirmingSync(false);
    try {
      setNote(await api.syncCodexSessions(target, setSyncProgress));
      setSyncProgress({ phase: "complete", completed: 0, total: 0 });
    } catch (e) {
      setSyncProgress(null);
      setNote(String(e));
    } finally {
      await load();
      setSyncing(false);
      operationInFlight.current = false;
    }
  };

  const doPrune = async () => {
    if (busy || syncing || repairing || operationInFlight.current) return;
    operationInFlight.current = true;
    setBusy(true);
    try {
      setNote(await api.pruneCodexSessionIndex());
    } catch (e) {
      setNote(String(e));
    } finally {
      await load();
      setBusy(false);
      operationInFlight.current = false;
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

  // 打开导出目录。回显实际打开的绝对路径：MSIX 虚拟化下打开的可能是包内私有副本，
  // 用户必须能核对自己看的是哪一份（同 `openLogDir` 那条平行宇宙防线）。
  //
  // ⚠️ **必须带一句说明，不能只塞裸路径**：这块 note 区域与「已导出到 X」「已删除 N 条」
  // 共用，一个没有前缀的绝对路径读不出是什么意思（审查时发现，同 `settings.logOpened` 的做法）。
  const doOpenExports = async () => {
    try {
      setNote(t("sessions.openExportsDone", { dir: await api.openCodexExportsDir() }));
    } catch (e) {
      setNote(`${t("sessions.openExportsFailed")}: ${e}`);
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

  const disabled = busy || syncing || repairing || deleteRequest !== null || archiveRequest !== null;
  const closeDetail = useCallback(() => setDetail(null), []);
  const requestDelete = (paths: string[]) => {
    if (disabled || !paths.length) return;
    setDetail(null);
    setDeleteRequest([...paths]);
  };
  const requestArchive = (paths: string[], archived: boolean) => {
    if (disabled || !paths.length) return;
    setDetail(null);
    setArchiveRequest({ paths: [...paths], archived });
  };

  return (
    // 🔴 **`h-full overflow-y-auto`：整页一起滚。** `App` 的 `<main>` 是
    // `flex-1 overflow-hidden`，子页面自己不滚动的话内容一长就被裁掉 ——
    // 用户 2026-09-10 实报的「没有滚动条、页面不自适应」就是这个（此前本页外壳只有
    // `space-y-4`，一个高度约束都没有）。
    //
    // ⚠️ **刻意不用日志页那套「固定头部 + 内层滚动」**（我第一版抄错了）：那套要求头部
    // 恒定矮，而本页头部有 6 段（统计卡 + 同步区 + 两条横幅 + 索引审计），
    // 实测 **801px** —— 比 520px 高的窗口整个视口都装不下，于是表格区被压到 16px、
    // 一行都看不见。头部可能超高的页（设置 / 大脑 / 厂商 / 关于）全是本页这套。
    <div className="h-full overflow-y-auto">
      <div className="space-y-4 px-6 py-4">
      <PageHeader
        icon={Wand2}
        title={t("sessions.title")}
        description={t("sessions.subtitle")}
        actions={
          <>
            <Button variant="outline" size="sm" onClick={() => void doOpenExports()}>
              <FolderOpen className="h-4 w-4" /> {t("sessions.openExports")}
            </Button>
            <RefreshButton
              onRefresh={load}
              idleLabel={t("sessions.refresh")}
              busyLabel={t("sessions.refreshing")}
            />
          </>
        }
      />

      {data && (
        <div className="rounded-control border border-border">
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
            <code className="text-text-primary">{data.stats.dbPath || t("sessions.dbNone")}</code>
            {data.stats.inDb < data.stats.total && (
              <p className="mt-1">
                {t("sessions.inDbHint", { n: data.stats.inDb, total: data.stats.total })}
              </p>
            )}
          </div>
        </div>
      )}

      <details className="space-y-3 rounded-control border border-border p-4">
      <summary className="cursor-pointer text-sm font-medium text-text-primary">{t("sessions.maintenance")}</summary>
      {maintenanceError && <p className="whitespace-pre-wrap text-sm text-warning">{maintenanceError}</p>}
      {targets && (
        <div className="space-y-3 rounded-control border border-border p-4">
          <div className="flex flex-wrap items-end gap-3">
            <label className="flex flex-col gap-1 text-sm">
              <span className="text-text-muted">{t("sessions.targetLabel")}</span>
              <select
                value={target}
                disabled={busy || syncing || repairing}
                onChange={(e) => setTarget(e.target.value)}
                className="min-w-[18rem] rounded-control border border-border bg-surface px-2 py-1.5 text-sm text-text-primary"
              >
                {!target && <option value="" disabled>{t("sessions.noAvailableTarget")}</option>}
                {targets.targets.map((o) => (
                  <option key={o.id} value={o.id} disabled={!o.available}>
                    {o.id}
                    {"  ·  "}
                    {o.sources.map(srcLabel).join(" / ")}
                    {o.isCurrent ? `  ·  ${t("sessions.current").replace("：", "").replace(":", "")}` : ""}
                    {o.id === targets.ours ? `  ·  ${t("sessions.targetOurs")}` : ""}
                    {!o.available ? `  ·  ${t("sessions.unavailableTarget")}` : ""}
                  </option>
                ))}
              </select>
            </label>
            <Button
              size="sm"
              disabled={busy || syncing || repairing || !target}
              onClick={() => setConfirmingSync(true)}
            >
              <Wand2 className="h-4 w-4" />
              {syncing ? t("sessions.syncing") : t("sessions.syncNow")}
            </Button>
          </div>
          <p className="text-xs text-text-muted">{t("sessions.targetHint")}</p>
          <p className="text-xs text-text-muted">{t("sessions.syncHint")}</p>
          <SessionMaintenanceProgress value={syncProgress} />
          {/* 改用户的对话文件之前先让他看清会动几条、动成什么 —— 同大脑聚合落盘前那一屏预览 */}
          {confirmingSync && (
            <div className="rounded-control border border-warning/50 bg-warning/8 p-3">
              <p className="font-medium text-text-primary">{t("sessions.syncConfirmTitle")}</p>
              <p className="mt-1 text-sm text-text-muted">
                {willChange > 0
                  ? t("sessions.syncConfirmBody", { n: willChange, target })
                  : t("sessions.syncConfirmNone", { total: data?.stats.total ?? 0, target })}
              </p>
              <div className="mt-3 flex gap-2">
                <Button size="sm" disabled={busy || syncing || repairing || !target} onClick={() => void doSync()}>
                  {t("sessions.syncConfirmOk")}
                </Button>
                <Button size="sm" variant="outline" onClick={() => setConfirmingSync(false)}>
                  {t("sessions.cancel")}
                </Button>
              </div>
            </div>
          )}
          <ToggleRow
            title={t("sessions.autoSync")}
            disabled={busy || syncing || repairing}
            desc={t("sessions.autoSyncHint")}
            checked={!targets.prefs.autoSyncDisabled}
            onChange={(v) => void doToggleAuto(v)}
          />
        </div>
      )}

      <SessionIdRepairPanel disabled={busy || syncing} onBusyChange={setRepairing} onDone={load} />

      {mismatched > 0 && (
        <InlineAlert tone="warning">{t("sessions.mismatchHint", { n: mismatched })}</InlineAlert>
      )}

      {/* 🔴 只有代理侧能给的那一位：provider 对了、模型却没人服务得了 */}
      {modelGone > 0 && (
        <InlineAlert tone="danger">{t("sessions.modelGoneHint", { n: modelGone })}</InlineAlert>
      )}

      {/* 索引孤儿：只在真有孤儿时才出现整块（恒 0 的一行是噪音） */}
      {audit && audit.orphans > 0 && (
        <div className="space-y-2 rounded-control border border-warning/40 bg-warning/8 p-3 text-sm text-text-primary">
          <p className="font-medium">{t("sessions.indexTitle")}</p>
          <p>{t("sessions.indexOrphans", { n: audit.orphans })}</p>
          <p className="text-xs text-text-muted">{t("sessions.indexBackupNote")}</p>
          <Button size="sm" variant="outline" disabled={busy || syncing || repairing} onClick={() => void doPrune()}>
            {t("sessions.indexPrune")}
          </Button>
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

      </details>
      {note && <p role="status" className="whitespace-pre-wrap break-words rounded-control bg-surface-hover p-3 text-sm text-text-primary">{note}</p>}
      {error && (
        <p className="rounded-control border border-danger/40 bg-danger/8 p-3 text-sm text-text-primary">
          {t("sessions.loadFailed")}: {error}
        </p>
      )}

      {data === null && !error ? (
        <p className="text-sm text-text-muted">{t("sessions.loading")}</p>
      ) : data?.rows.length === 0 ? (
        <p className="text-sm text-text-muted">{t("sessions.empty")}</p>
      ) : data ? <SessionBrowser data={data} picked={picked} disabled={disabled}
        onSelect={(paths, selected) => setPicked((previous) => selectSessionPaths(previous, paths, selected))}
        onDetails={setDetail} onExport={(row) => void doExport(row)} onDelete={requestDelete} onArchive={requestArchive} /> : null}
      {deleteRequest && <ConfirmDialog title={t("sessions.confirmTitle")}
        description={t("sessions.confirmBody", { n: deleteRequest.length })}
        confirmLabel={t("sessions.confirmOk")} cancelLabel={t("sessions.cancel")}
        busy={busy || syncing || repairing} onConfirm={() => void doDelete()} onCancel={() => setDeleteRequest(null)} />}
      {archiveRequest && <ConfirmDialog tone="primary"
        title={t(archiveRequest.archived ? "sessions.archiveConfirmTitle" : "sessions.restoreConfirmTitle")}
        description={t("sessions.archiveConfirmBody", { n: archiveRequest.paths.length })}
        confirmLabel={t(archiveRequest.archived ? "sessions.archive" : "sessions.restore")} cancelLabel={t("sessions.cancel")}
        busy={busy || syncing || repairing} onConfirm={() => void doArchive()} onCancel={() => setArchiveRequest(null)} />}
      {detail && <SessionDetailDialog row={detail} disabled={disabled} onClose={closeDetail}
        onExport={(row) => { setDetail(null); void doExport(row); }}
        onArchive={(row) => requestArchive([row.relPath], !row.archived)} onDelete={(row) => requestDelete([row.relPath])} />}
      </div>
    </div>
  );
}

function Stat({ label, value, danger }: { label: string; value: string; danger?: boolean }) {
  return (
    <div>
      <dt className="text-text-muted">{label}</dt>
      <dd className={`text-lg font-semibold ${danger ? "text-danger" : "text-text-primary"}`}>{value}</dd>
    </div>
  );
}

