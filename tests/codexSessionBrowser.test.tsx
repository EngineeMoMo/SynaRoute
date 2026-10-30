import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { CodexSessionRow } from "@/types";
import { defaultSessionFilters, displayProjectPath, filterSessions, groupSessions, isInternalSession, normalizeProjectPath, normalizeSessionRow, retainSessionSelection, selectSessionPaths } from "@/lib/codexSessionBrowser";
import { SessionBrowser } from "@/components/SessionBrowser";
import { SessionDetailDialog } from "@/components/SessionDetailDialog";
import { mockCodexSessions } from "@/lib/mockData.sessions";
import { sessionsEn, sessionsZh } from "@/lib/i18n.sessions";

function session(id: string, changes: Partial<CodexSessionRow> = {}): CodexSessionRow {
  return {
    relPath: `sessions/${id}.jsonl`, threadId: id, provider: "synaroute", archived: false,
    cwd: "D:\\projects\\app", timestamp: "2026-10-01T10:00:00Z", updatedAt: "2026-10-30T10:00:00Z",
    bytes: 1024, threadSource: "user", forked: false, title: `Conversation ${id}`, model: "test-model",
    effort: "high", tokens: 100, modelUnserviceable: false, project: "app", ...changes,
  };
}

describe("Codex session browsing", () => {
  it("keeps user forks and unknown sources visible while separating known internal sessions", () => {
    const rows = [session("user"), session("fork", { forked: true, internal: true }),
      session("internal", { threadSource: "guardian_review", internal: true }),
      session("unknown", { threadSource: "new-source" }), session("archived", { archived: true })];
    expect(filterSessions(rows, defaultSessionFilters, "synaroute").map((row) => row.threadId).sort())
      .toEqual(["fork", "unknown", "user"]);
    expect(isInternalSession(session("veto", { internal: true }))).toBe(false);
    expect(filterSessions(rows, { ...defaultSessionFilters, source: "internal" }, "synaroute").map((row) => row.threadId))
      .toEqual(["internal"]);
    expect(filterSessions(rows, { ...defaultSessionFilters, archive: "archived" }, "synaroute").map((row) => row.threadId))
      .toEqual(["archived"]);
  });

  it("searches IDs with multiple terms and combines diagnostics with archive filters", () => {
    const rows = [session("needle-id", { title: "Fix routing", provider: "openai", archived: true, modelUnserviceable: true }), session("other")];
    const filters = { ...defaultSessionFilters, query: "needle-ID Routing", archive: "all" as const, onlyMismatched: true, onlyModelGone: true };
    expect(filterSessions(rows, filters, "synaroute").map((row) => row.threadId)).toEqual(["needle-id"]);
    expect(filterSessions(rows, filters, "")).toEqual([]);
    expect(filterSessions(rows, { ...filters, query: "needle missing" }, "synaroute")).toEqual([]);
  });

  it("uses last activity rather than creation time and sorts ties deterministically", () => {
    const rows = [session("old", { updatedAt: "2026-10-20T10:00:00Z", timestamp: "2026-10-20T10:00:00Z" }),
      session("resumed", { timestamp: "2026-01-01T00:00:00Z" }), session("missing", { updatedAt: "", timestamp: "2026-10-25T10:00:00Z" })];
    expect(filterSessions(rows, defaultSessionFilters, "synaroute").map((row) => row.threadId)).toEqual(["resumed", "missing", "old"]);
    expect(filterSessions(rows, { ...defaultSessionFilters, sort: "created" }, "synaroute").map((row) => row.threadId)).toEqual(["missing", "old", "resumed"]);
  });

  it("groups Windows path aliases but keeps different projects and Unix case distinct", () => {
    const rows = [session("long", { cwd: "\\\\?\\D:\\Work\\App" }), session("plain", { cwd: "d:/work/app/" }),
      session("different", { cwd: "D:\\other\\app" }), session("unix-upper", { cwd: "/Work/app" }), session("unix-lower", { cwd: "/work/app" })];
    const groups = groupSessions(rows, "project");
    expect(groups).toHaveLength(4);
    expect(groups[0].rows.map((row) => row.threadId)).toEqual(["long", "plain"]);
    expect(groups[0].label).toBe("D:\\Work\\App");
    expect(normalizeProjectPath("\\\\?\\UNC\\server\\Share\\App\\")).toBe("//server/share/app");
    expect(displayProjectPath("\\\\?\\UNC\\server\\share")).toBe("\\\\server\\share");
    expect(normalizeProjectPath("/")).toBe("/");
  });

  it("groups by calendar day using last activity", () => {
    const now = new Date(2026, 9, 30, 12);
    const localTime = (day: number) => new Date(2026, 9, day, 8).toISOString();
    const rows = [session("today", { updatedAt: localTime(30) }), session("yesterday", { updatedAt: localTime(29) }),
      session("week", { updatedAt: localTime(25) }), session("older", { updatedAt: localTime(20) })];
    expect(groupSessions(rows, "time", now).map((group) => group.key)).toEqual(["today", "yesterday", "week", "older"]);
  });

  it("preserves off-page selections, deselects only the requested group, and removes stale paths on refresh", () => {
    const firstPage = Array.from({ length: 50 }, (_, index) => session(`page-${index}`));
    const offPage = session("off-page");
    const previous = new Set([offPage.relPath]);
    const selected = selectSessionPaths(previous, firstPage.map((row) => row.relPath), true);
    expect(selected.size).toBe(51);
    expect(previous.size).toBe(1);
    const cleared = selectSessionPaths(selected, firstPage.map((row) => row.relPath), false);
    expect([...cleared]).toEqual([offPage.relPath]);
    expect([...retainSessionSelection(selected, [firstPage[0], offPage])]).toEqual([offPage.relPath, firstPage[0].relPath]);
  });

  it("normalizes incomplete IPC rows without letting invalid title or numeric fields break rendering", () => {
    expect(normalizeSessionRow(null)).toBeNull();
    expect(normalizeSessionRow({ provider: "test" })).toBeNull();
    const normalized = normalizeSessionRow({ relPath: "sessions/example.jsonl", provider: "test", title: {}, tokens: NaN, bytes: -5 });
    expect(normalized).toMatchObject({ title: "", cwd: "", threadSource: "", tokens: 0, bytes: 0 });
    expect(filterSessions([normalized!], { ...defaultSessionFilters, query: "example" }, "test")).toHaveLength(1);
  });
});

describe("Session management rendering", () => {
  const noop = () => {};

  it("renders at most one page of rows while keeping group selection and pagination accessible", () => {
    const data = mockCodexSessions();
    data.rows = Array.from({ length: 123 }, (_, index) => session(`session-${index}`));
    const markup = renderToStaticMarkup(createElement(SessionBrowser, { data, picked: new Set<string>(), disabled: false,
      onSelect: noop, onDetails: noop, onExport: noop, onDelete: noop, onArchive: noop }));
    expect(markup.match(/type="checkbox"/g)).toHaveLength(52);
    expect(markup).toContain('aria-controls="session-group-0"');
    expect(markup).toContain('aria-expanded="true"');
    expect(markup).toContain('aria-pressed="true"');
    expect(markup).not.toContain("Conversation session-99");
    expect(markup).toContain("123");
  });

  it("exposes the exact session ID and a resume command only for valid IDs", () => {
    const id = "01a05d14-4e5b-7773-b425-25ae029f078f";
    const props = { row: session(id), disabled: false, onClose: noop, onExport: noop, onArchive: noop, onDelete: noop };
    const markup = renderToStaticMarkup(createElement(SessionDetailDialog, props));
    expect(markup).toContain('aria-labelledby="session-detail-title"');
    expect(markup).toContain(`codex resume ${id}`);
    const malformed = renderToStaticMarkup(createElement(SessionDetailDialog, { ...props, row: session("invalid; command") }));
    expect(malformed).not.toContain("codex resume");
  });

  it("keeps all session translations and placeholders aligned", () => {
    const keys = Object.keys(sessionsZh).filter((key) => key.startsWith("sessions."));
    expect(Object.keys(sessionsEn).filter((key) => key.startsWith("sessions.")).sort()).toEqual([...keys].sort());
    for (const key of keys) {
      expect(sessionsEn[key], key).toBeTruthy();
      expect(sessionsEn[key].match(/\{\w+\}/g)?.sort(), key).toEqual(sessionsZh[key].match(/\{\w+\}/g)?.sort());
    }
  });
});
