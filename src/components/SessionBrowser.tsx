import { useMemo, useState } from "react";
import { Archive, ArchiveRestore, ChevronDown, ChevronLeft, ChevronRight, Trash2 } from "lucide-react";
import type { CodexSessionList, CodexSessionRow } from "@/types";
import { useT } from "@/lib/useT";
import { defaultSessionFilters, filterSessions, groupSessions, type SessionFilters, type SessionGrouping } from "@/lib/codexSessionBrowser";
import { Button } from "@/components/ui/Button";
import { SessionTable } from "@/components/SessionTable";

export function SessionBrowser({ data, picked, disabled, onSelect, onDetails, onExport, onDelete, onArchive }: {
  data: CodexSessionList;
  picked: Set<string>;
  disabled: boolean;
  onSelect: (paths: string[], selected: boolean) => void;
  onDetails: (row: CodexSessionRow) => void;
  onExport: (row: CodexSessionRow) => void;
  onDelete: (paths: string[]) => void;
  onArchive: (paths: string[], archived: boolean) => void;
}) {
  const t = useT();
  const [filters, setFilters] = useState<SessionFilters>(defaultSessionFilters);
  const [mode, setMode] = useState<SessionGrouping>("project");
  const [page, setPage] = useState(0);
  const [pageSize, setPageSize] = useState(50);
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());
  const updateFilter = <Key extends keyof SessionFilters>(key: Key, value: SessionFilters[Key]) => {
    setFilters((previous) => ({ ...previous, [key]: value }));
    setPage(0);
  };
  const filtered = useMemo(() => filterSessions(data.rows, filters, data.currentProvider), [data, filters]);
  const groups = useMemo(() => groupSessions(filtered, mode), [filtered, mode]);
  const ordered = useMemo(() => groups.flatMap((group) => group.rows), [groups]);
  const pageCount = Math.max(1, Math.ceil(ordered.length / pageSize));
  const currentPage = Math.min(page, pageCount - 1);
  const pageRows = ordered.slice(currentPage * pageSize, (currentPage + 1) * pageSize);
  const pageGroups = groupSessions(pageRows, mode);
  const pageSelected = pageRows.length > 0 && pageRows.every((row) => picked.has(row.relPath));
  const visiblePaths = new Set(pageRows.map((row) => row.relPath));
  const hiddenSelected = [...picked].filter((path) => !visiblePaths.has(path)).length;
  const selectClass = "rounded-control border border-border bg-surface px-2 py-1.5 text-sm text-text-primary";

  return <section className="space-y-3" aria-label={t("sessions.browser")}>
    <div className="flex flex-wrap items-center gap-2">
      <input type="search" value={filters.query} onChange={(event) => updateFilter("query", event.target.value)}
        placeholder={t("sessions.filterPlaceholder")} aria-label={t("sessions.filter")}
        className="min-w-0 basis-64 flex-1 rounded-control border border-border bg-surface px-3 py-2 text-sm text-text-primary" />
      <select aria-label={t("sessions.archiveFilter")} value={filters.archive} className={selectClass}
        onChange={(event) => updateFilter("archive", event.target.value as SessionFilters["archive"])}>
        <option value="active">{t("sessions.activeOnly")}</option>
        <option value="archived">{t("sessions.archivedOnly")}</option>
        <option value="all">{t("sessions.allStates")}</option>
      </select>
      <select aria-label={t("sessions.sourceFilter")} value={filters.source} className={selectClass}
        onChange={(event) => updateFilter("source", event.target.value as SessionFilters["source"])}>
        <option value="user">{t("sessions.userOnly")}</option>
        <option value="internal">{t("sessions.internalOnly")}</option>
        <option value="all">{t("sessions.allSources")}</option>
      </select>
      <select aria-label={t("sessions.sort")} value={filters.sort} className={selectClass}
        onChange={(event) => updateFilter("sort", event.target.value as SessionFilters["sort"])}>
        <option value="updated">{t("sessions.sortUpdated")}</option>
        <option value="created">{t("sessions.sortCreated")}</option>
        <option value="title">{t("sessions.sortTitle")}</option>
      </select>
    </div>
    <div className="flex flex-wrap items-center gap-3 text-sm">
      <div className="inline-flex rounded-control border border-border p-0.5" role="group" aria-label={t("sessions.grouping")}>
        {(["project", "time"] as const).map((grouping) => <button key={grouping} type="button" aria-pressed={mode === grouping}
          onClick={() => { setMode(grouping); setPage(0); setCollapsed(new Set()); }}
          className={`rounded-control px-3 py-1 ${mode === grouping ? "bg-primary/12 text-primary" : "text-text-muted hover:bg-surface-hover"}`}>
          {t(`sessions.group.${grouping}`)}
        </button>)}
      </div>
      <label className="flex items-center gap-1.5 text-text-muted">
        <input type="checkbox" checked={filters.onlyMismatched} onChange={(event) => updateFilter("onlyMismatched", event.target.checked)} />
        {t("sessions.onlyMismatched")}
      </label>
      <label className="flex items-center gap-1.5 text-text-muted">
        <input type="checkbox" checked={filters.onlyModelGone} onChange={(event) => updateFilter("onlyModelGone", event.target.checked)} />
        {t("sessions.onlyModelGone")}
      </label>
      <span className="ml-auto text-xs text-text-muted" role="status">{t("sessions.filtered", { n: filtered.length, total: data.rows.length })}</span>
    </div>
    <div className="flex flex-wrap items-center gap-2 rounded-control border border-border bg-surface-hover p-3">
      <Button variant="outline" size="sm" disabled={disabled || !pageRows.length}
        onClick={() => onSelect(pageRows.map((row) => row.relPath), !pageSelected)}>
        {t(pageSelected ? "sessions.deselectPage" : "sessions.selectPage")}
      </Button>
      {picked.size > 0 && <>
        <span className="text-sm text-text-primary">{t("sessions.selected", { n: picked.size })}</span>
        <Button variant="ghost" size="sm" disabled={disabled} onClick={() => onSelect([...picked], false)}>{t("sessions.clearSelection")}</Button>
        <Button variant="outline" size="sm" disabled={disabled} onClick={() => onArchive([...picked], true)}><Archive size={14} />{t("sessions.archiveSelected")}</Button>
        <Button variant="outline" size="sm" disabled={disabled} onClick={() => onArchive([...picked], false)}><ArchiveRestore size={14} />{t("sessions.restoreSelected")}</Button>
        <Button variant="danger-outline" size="sm" disabled={disabled} onClick={() => onDelete([...picked])}><Trash2 size={14} />{t("sessions.deleteSelected")}</Button>
        {hiddenSelected > 0 && <span className="text-xs text-warning">{t("sessions.pickedHidden", { n: hiddenSelected })}</span>}
      </>}
    </div>
    {pageGroups.length === 0 && <div className="rounded-control border border-border p-8 text-center text-sm text-text-muted">
      <p>{t("sessions.noMatches")}</p>
      <Button variant="ghost" size="sm" className="mt-2" onClick={() => { setFilters({ ...defaultSessionFilters, archive: "all", source: "all" }); setPage(0); }}>
        {t("sessions.resetFilters")}
      </Button>
    </div>}
    {pageGroups.map((group, index) => {
      const allRows = groups.find((candidate) => candidate.key === group.key)?.rows ?? group.rows;
      const expanded = !collapsed.has(group.key);
      const region = `session-group-${index}`;
      const label = mode === "time" ? t(`sessions.time.${group.key}`) : group.label || t("sessions.unknownProject");
      const allSelected = allRows.every((row) => picked.has(row.relPath));
      return <div key={group.key} className="space-y-2">
        <div className="flex items-center gap-2 rounded-control bg-surface-hover px-3 py-2">
          <button type="button" className="flex min-w-0 flex-1 items-center gap-2 text-left text-sm font-medium text-text-primary"
            aria-expanded={expanded} aria-controls={region} onClick={() => setCollapsed((previous) => {
              const next = new Set(previous);
              if (!next.delete(group.key)) next.add(group.key);
              return next;
            })}>
            <ChevronDown size={16} className={`shrink-0 transition-transform ${expanded ? "" : "-rotate-90"}`} />
            <span className="truncate" title={label}>{label}</span>
            <span className="shrink-0 text-xs font-normal text-text-muted">{allRows.length}</span>
          </button>
          <Button variant="ghost" size="sm" disabled={disabled}
            onClick={() => onSelect(allRows.map((row) => row.relPath), !allSelected)}>
            {t(allSelected ? "sessions.deselectGroup" : "sessions.selectGroup")}
          </Button>
        </div>
        <div id={region} hidden={!expanded}>
          {expanded && <SessionTable rows={group.rows} currentProvider={data.currentProvider} picked={picked} disabled={disabled}
            onToggle={(path) => onSelect([path], !picked.has(path))} onExport={onExport} onDetails={onDetails}
            onArchive={(row) => onArchive([row.relPath], !row.archived)} onDelete={(row) => onDelete([row.relPath])} />}
        </div>
      </div>;
    })}
    <div className="flex flex-wrap items-center justify-between gap-3 border-t border-border pt-3">
      <span className="text-xs text-text-muted">{t("sessions.page", { page: currentPage + 1, pages: pageCount, total: filtered.length })}</span>
      <div className="flex items-center gap-2">
        <select aria-label={t("sessions.pageSize")} value={pageSize} className={selectClass}
          onChange={(event) => { setPageSize(Number(event.target.value)); setPage(0); }}>
          {[25, 50, 100].map((size) => <option key={size} value={size}>{t("sessions.perPage", { n: size })}</option>)}
        </select>
        <Button variant="outline" size="sm" disabled={currentPage === 0} onClick={() => setPage(currentPage - 1)}><ChevronLeft size={14} />{t("sessions.previous")}</Button>
        <Button variant="outline" size="sm" disabled={currentPage + 1 >= pageCount} onClick={() => setPage(currentPage + 1)}>{t("sessions.next")}<ChevronRight size={14} /></Button>
      </div>
    </div>
  </section>;
}
