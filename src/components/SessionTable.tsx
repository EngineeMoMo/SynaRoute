// 会话页那张表。从 CodexSessionsPage 抽出来的：那一页现在还要放统计卡、同步区、索引孤儿区，
// 表格本身又长了标题/模型两列 —— 两者混在一个文件里会让「改一列」变成在三百行里找。
//
// 🔴 **标题这一列是本轮借鉴 CodexPlusPlus 时补的最大缺口。** 在它之前这张表只有时间、
// provider、工作目录 —— 用户面对的是一串时间戳和同一个目录，压根认不出哪条是哪条对话。
// 标题的取值顺序在后端（`codex_session_view.rs`）：会话库的 `name`（Codex 自己生成的短标题）
// → `title` / `first_user_message` → 从正文读第一条用户消息。
//
// `forked` / `threadSource` 两个徽标**只标注、不隐藏**：CodexPlusPlus 的列表直接读 sqlite，
// 于是本机 5 个会话文件里它只列出 3 条（fork 出来的分支在库里没有记录）。隐藏用户的数据比
// 多显示一行更糟，而不标注的话「我明明只开了几个对话，这里怎么多出两条」无从解释。

import type { CodexSessionRow } from "@/types";
import { Badge } from "@/components/ui/Badge";
import { Download } from "lucide-react";
import { useT } from "@/lib/useT";

export function SessionTable({
  rows,
  currentProvider,
  picked,
  onToggle,
  onExport,
}: {
  rows: CodexSessionRow[];
  currentProvider: string;
  picked: Set<string>;
  onToggle: (relPath: string) => void;
  onExport: (row: CodexSessionRow) => void;
}) {
  const t = useT();
  return (
    <div className="overflow-x-auto rounded-md border border-border">
      <table className="w-full text-sm">
        <thead className="bg-surface-hover/60 text-left text-text-muted">
          <tr>
            <th className="w-8 px-3 py-2" />
            <th className="px-3 py-2">{t("sessions.colTitle")}</th>
            <th className="px-3 py-2">{t("sessions.colProvider")}</th>
            <th className="px-3 py-2">{t("sessions.colModel")}</th>
            <th className="px-3 py-2">{t("sessions.colTime")}</th>
            <th className="px-3 py-2">{t("sessions.colCwd")}</th>
            <th className="px-3 py-2 text-right">{t("sessions.colSize")}</th>
            <th className="px-3 py-2 text-right">{t("sessions.colOps")}</th>
          </tr>
        </thead>
        <tbody>
          {rows.map((r) => {
            const bad = !!currentProvider && r.provider !== currentProvider;
            return (
              <tr key={r.relPath} className="border-t border-border align-top">
                <td className="px-3 py-2">
                  <input
                    type="checkbox"
                    aria-label={r.relPath}
                    checked={picked.has(r.relPath)}
                    onChange={() => onToggle(r.relPath)}
                  />
                </td>
                <td className="max-w-[24rem] px-3 py-2">
                  <div className="truncate text-text" title={r.title || r.relPath}>
                    {r.title || t("sessions.noTitle")}
                  </div>
                  <div className="mt-0.5 flex flex-wrap items-center gap-1">
                    {r.archived && <Badge variant="neutral">{t("sessions.archived")}</Badge>}
                    {r.forked && <Badge variant="neutral">{t("sessions.forked")}</Badge>}
                    {/* `user` 是绝大多数，只标注例外 —— 每行都挂一个「user」是纯噪音 */}
                    {r.threadSource !== "" && r.threadSource !== "user" && (
                      <Badge variant="neutral">{t("sessions.derived")}</Badge>
                    )}
                  </div>
                </td>
                <td className="px-3 py-2">
                  <code className={bad ? "text-danger" : "text-text"}>{r.provider || "—"}</code>
                  {bad && (
                    <Badge variant="danger" className="ml-2">
                      {t("sessions.mismatch")}
                    </Badge>
                  )}
                </td>
                <td className="whitespace-nowrap px-3 py-2 text-text-muted">
                  <span className={r.modelUnserviceable ? "text-danger" : undefined}>
                    {r.model || "—"}
                  </span>
                  {r.effort && <span className="ml-1 text-xs">({r.effort})</span>}
                  {/* 🔴 只有代理侧能给的那一位：provider 是绿的、打开却仍然不对，成因在这 */}
                  {r.modelUnserviceable && (
                    <Badge variant="danger" className="ml-1">
                      {t("sessions.modelGone")}
                    </Badge>
                  )}
                  {r.tokens > 0 && (
                    <div className="text-xs">{formatTokens(r.tokens)} tokens</div>
                  )}
                </td>
                <td className="whitespace-nowrap px-3 py-2 text-text">{formatTime(r.timestamp)}</td>
                <td className="max-w-[16rem] truncate px-3 py-2 text-text-muted" title={r.cwd}>
                  {r.cwd || "—"}
                  {/* Desktop 项目侧栏的归属。空 = 未归入任何项目，那正是「这条对话为什么
                      不在我的项目侧栏里」的答案 —— 与 provider 正交，那一列看不出来。 */}
                  <div className="text-xs">
                    {r.project ? (
                      <span className="opacity-80">{t("sessions.project", { name: r.project })}</span>
                    ) : (
                      <span className="opacity-60">{t("sessions.noProject")}</span>
                    )}
                  </div>
                </td>
                <td className="whitespace-nowrap px-3 py-2 text-right text-text-muted">
                  {formatBytes(r.bytes)}
                </td>
                <td className="whitespace-nowrap px-3 py-2 text-right">
                  <button
                    type="button"
                    onClick={() => onExport(r)}
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
  );
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

/** token 数动辄上百万（本机实测一条会话 2 283 034），原样打印读不出量级。 */
function formatTokens(n: number): string {
  if (!Number.isFinite(n) || n <= 0) return "0";
  if (n < 1000) return String(n);
  if (n < 1_000_000) return `${(n / 1000).toFixed(1)}K`;
  return `${(n / 1_000_000).toFixed(2)}M`;
}
