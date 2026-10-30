import type { CodexSessionRow } from "@/types";

export type SessionGrouping = "project" | "time";
export type SessionSort = "updated" | "created" | "title";
export type SessionFilters = {
  query: string;
  archive: "all" | "active" | "archived";
  source: "all" | "user" | "internal";
  onlyMismatched: boolean;
  onlyModelGone: boolean;
  sort: SessionSort;
};

export const defaultSessionFilters: SessionFilters = {
  query: "", archive: "active", source: "user",
  onlyMismatched: false, onlyModelGone: false, sort: "updated",
};

export function displayProjectPath(path: string): string {
  return path.replace(/^\\\\\?\\UNC\\/i, "\\\\").replace(/^\\\\\?\\/, "");
}

export function normalizeProjectPath(path: string): string {
  const displayed = displayProjectPath(path.trim()).replace(/\\/g, "/");
  const normalized = displayed.replace(/\/+$/, "") || (displayed.startsWith("/") ? "/" : "");
  return /^[a-z]:/i.test(normalized) || normalized.startsWith("//")
    ? normalized.toLowerCase() : normalized;
}

export function isInternalSession(row: CodexSessionRow): boolean {
  if (row.forked || row.threadSource?.trim().toLowerCase() === "user") return false;
  return row.internal ?? ["guardian_review", "subagent", "memory_consolidation"]
    .includes(row.threadSource?.trim().toLowerCase());
}

export function sessionTime(row: CodexSessionRow, sort: "updated" | "created" = "updated"): number {
  const created = Date.parse(row.timestamp) || 0;
  return sort === "created" ? created : Date.parse(row.updatedAt ?? "") || created;
}

export function filterSessions(rows: CodexSessionRow[], filters: SessionFilters, currentProvider: string): CodexSessionRow[] {
  const terms = filters.query.trim().toLowerCase().split(/\s+/).filter(Boolean);
  return rows.filter((row) => {
    if (filters.archive === "active" && row.archived) return false;
    if (filters.archive === "archived" && !row.archived) return false;
    if (filters.source === "user" && isInternalSession(row)) return false;
    if (filters.source === "internal" && !isInternalSession(row)) return false;
    if (filters.onlyMismatched && (!currentProvider || row.provider === currentProvider)) return false;
    if (filters.onlyModelGone && !row.modelUnserviceable) return false;
    const searchable = [row.title, row.cwd, row.provider, row.model, row.project, row.threadId, row.relPath]
      .join(" ").toLowerCase();
    return terms.every((term) => searchable.includes(term));
  }).sort((left, right) => {
    const compared = filters.sort === "title"
      ? left.title.localeCompare(right.title)
      : sessionTime(right, filters.sort) - sessionTime(left, filters.sort);
    return compared || left.relPath.localeCompare(right.relPath);
  });
}

export type SessionGroup = { key: string; label: string; rows: CodexSessionRow[] };

function timeGroup(timestamp: number, now: Date): string {
  if (!timestamp) return "unknown";
  const today = new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime();
  const yesterday = new Date(now.getFullYear(), now.getMonth(), now.getDate() - 1).getTime();
  const week = new Date(now.getFullYear(), now.getMonth(), now.getDate() - 6).getTime();
  return timestamp >= today ? "today" : timestamp >= yesterday ? "yesterday" : timestamp >= week ? "week" : "older";
}

export function groupSessions(rows: CodexSessionRow[], mode: SessionGrouping, now = new Date()): SessionGroup[] {
  const groups = new Map<string, SessionGroup>();
  for (const row of rows) {
    const path = normalizeProjectPath(row.cwd);
    const key = mode === "time" ? timeGroup(sessionTime(row), now) : path || `project:${row.project || ""}`;
    const label = mode === "time" ? key : displayProjectPath(row.cwd) || row.project;
    const group = groups.get(key) ?? { key, label, rows: [] };
    group.rows.push(row);
    groups.set(key, group);
  }
  return [...groups.values()];
}

export function selectSessionPaths(previous: Set<string>, paths: string[], selected: boolean): Set<string> {
  const next = new Set(previous);
  for (const path of paths) {
    if (selected) next.add(path); else next.delete(path);
  }
  return next;
}

export function retainSessionSelection(previous: Set<string>, rows: CodexSessionRow[]): Set<string> {
  const existing = new Set(rows.map((row) => row.relPath));
  return new Set([...previous].filter((path) => existing.has(path)));
}

export function normalizeSessionRow(value: unknown): CodexSessionRow | null {
  if (!value || typeof value !== "object") return null;
  const row = value as Record<string, unknown>;
  if (typeof row.relPath !== "string" || typeof row.provider !== "string") return null;
  const text = (key: string) => typeof row[key] === "string" ? row[key] as string : "";
  return {
    relPath: row.relPath, provider: row.provider, threadId: text("threadId"),
    title: text("title"), cwd: text("cwd"), project: text("project"), model: text("model"),
    timestamp: text("timestamp"), updatedAt: text("updatedAt"), effort: text("effort"),
    threadSource: text("threadSource"), source: text("source"),
    archived: row.archived === true, forked: row.forked === true,
    internal: typeof row.internal === "boolean" ? row.internal : undefined,
    modelUnserviceable: row.modelUnserviceable === true,
    bytes: typeof row.bytes === "number" && Number.isFinite(row.bytes) ? Math.max(0, row.bytes) : 0,
    tokens: typeof row.tokens === "number" && Number.isFinite(row.tokens) ? Math.max(0, row.tokens) : 0,
  };
}
