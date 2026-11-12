import { useDismiss } from "@rightkit/platform-ui/headless";
import { Notice, PageShell } from "@rightkit/react-shell/react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { LibraryGroup, LibraryRow, LibrarySource, LibraryVariant } from "../contract.js";
import { libraryAppName, readStored, writeStored } from "../host.js";
import {
  DEFAULT_LIBRARY_LABELS,
  LibraryActionsLine,
  LibraryConfirmDialog,
  LibraryContext,
  type LibraryContextValue,
  type LibraryFilterOption,
  type LibraryLabels,
  LibraryNotice,
  LibraryToolbar,
} from "./LibraryChrome.js";
import { LibraryRows } from "./LibraryRows.js";
import { type DateFilter, dateFilterRange, formatRowsPlainText, friendlyAppName, type PendingConfirm } from "./format.js";
import { collectAllRows, type LibraryFilter, useLibraryQuery } from "./useLibraryQuery.js";
import { RecoveryBanner } from "./RecoveryBanner.js";
import { type GroupSelection, useGroupSelection } from "./useGroupSelection.js";
import { useLibrarySelection } from "./useLibrarySelection.js";

const DEFAULT_VARIANTS: readonly LibraryVariant[] = [{ key: "all", label: "All" }];
const NO_GROUPS: readonly LibraryGroup[] = [];

export interface LibraryScreenProps {
  source: LibrarySource;
  /** Search text. The host may render the field elsewhere (e.g. a title bar). */
  search?: string;
  /** Tabs over the list; the first is the default. Default: one "All" variant (no tabs). */
  variants?: readonly LibraryVariant[];
  /** Groups rows belong to (e.g. devices). Default: none. */
  groups?: readonly LibraryGroup[];
  /** Controlled group visibility; without it the screen keeps its own (`useGroupSelection`). */
  groupSelection?: GroupSelection;
  labels?: Partial<LibraryLabels>;
  /** File-name stem for "Export selected". Default: the area label, lower case. */
  exportName?: string;
  /** Page width cap. Default 1320. */
  maxWidth?: number;
}

/**
 * The library list: filters (variants, date, origin), retention, export, selection, per-row edit,
 * copy and delete with confirmation, over the host's `LibrarySource`. HeardRight's History page is
 * the reference behaviour.
 */
export function LibraryScreen({
  source,
  search = "",
  variants = DEFAULT_VARIANTS,
  groups = NO_GROUPS,
  groupSelection: controlledSelection,
  labels: labelOverrides,
  exportName,
  maxWidth = 1320,
}: LibraryScreenProps) {
  const labels = useMemo<LibraryLabels>(() => ({ ...DEFAULT_LIBRARY_LABELS, ...labelOverrides }), [labelOverrides]);
  const context = useMemo<LibraryContextValue>(() => ({ source, groups, variants, labels }), [source, groups, variants, labels]);
  const own = useGroupSelection(groups);
  const sourceSelection = controlledSelection ?? own.groupSelection;
  const [dateFilter, setDateFilter] = useState<DateFilter>("all");
  const [appFilter, setAppFilter] = useState("all");
  const [outcomeFilter, setOutcomeFilter] = useState<string>(variants[0]?.key ?? "all");
  const [error, setError] = useState<string>("");
  const [copiedId, setCopiedId] = useState("");
  const [expandedId, setExpandedId] = useState("");
  const [showRaw, setShowRaw] = useState<Record<string, boolean>>({});
  const [notice, setNotice] = useState("");
  const [pendingConfirm, setPendingConfirm] = useState<PendingConfirm>(null);
  const [dontAskAgain, setDontAskAgain] = useState(false);
  const [editingId, setEditingId] = useState("");
  const [draft, setDraft] = useState("");
  const [retention, setRetentionState] = useState<string>(source.retention?.options[0]?.value ?? "");
  const [retentionSaving, setRetentionSaving] = useState(false);
  const cancelConfirmRef = useRef<HTMLButtonElement | null>(null);
  // Copied-flash timer: tracked so an unmount clears it instead of writing
  // state on a dead component.
  const copiedTimer = useRef<number | undefined>(undefined);
  // Exact id list for a pending "Delete all" confirmation, captured at
  // request time (a full walk of every matching page — see
  // `collectAllMatching`) so the confirm step deletes precisely what the
  // dialog told the user it would, instead of re-deriving from whatever
  // happens to still be loaded when they click through.
  const pendingDeleteAllIds = useRef<string[]>([]);

  const isAllSourcesSelected = groups.every((group) => sourceSelection[group.key] !== false);

  // `variant` is an exclusive source-side scope: each tab returns exactly its rows, so the count and
  // pagination reflect what is shown (no client re-filtering).
  const filter = useMemo<LibraryFilter>(() => {
    const range = dateFilterRange(dateFilter);
    return {
      search: search.trim(),
      fromMs: range.fromMs,
      untilMs: range.untilMs,
      origin: appFilter === "all" ? "" : appFilter,
      variant: outcomeFilter,
    };
  }, [search, dateFilter, appFilter, outcomeFilter]);

  // Query/pagination lifecycle (fetch, page drain, load-more, refresh
  // triggers) lives in useHistoryQuery. `exhaustive` makes refresh walk every
  // matching page whenever a source checkbox is off, so the source scope
  // applied below sees the complete matching set.
  const { rows: items, total, hasMore, loaded, loadingMore, refresh, loadMore } = useLibraryQuery({
    source,
    filter,
    exhaustive: !isAllSourcesSelected,
    setError,
  });

  // The backend `cancelled_only` scope returns exactly the rows for the active
  // tab (Dictations excludes cancelled+draft; Cancelled returns only cancelled),
  // so there is no outcome re-filtering — the only client-side scope left is
  // the inclusive source checkboxes, applied over the complete fetched set.
  // `visibleItems` IS the rendered list: `total`/pagination match it exactly.
  const visibleItems = useMemo(
    () =>
      isAllSourcesSelected
        ? items
        : items.filter((item) => sourceSelection[item.group ?? ""] !== false),
    [items, isAllSourcesSelected, sourceSelection],
  );

  // When scoped, the count the UI reports is the visible (locally scoped)
  // count; when all sources are on, the native `total` is exact already.
  const displayTotal = isAllSourcesSelected ? total : visibleItems.length;

  // Selection mode (Export/Delete selected, Select-all, per-row checkboxes)
  // lives in useHistorySelection — including the reset when the titlebar
  // source visibility changes.
  const {
    isSelecting,
    selectedIds,
    deletableCount,
    allVisibleSelected,
    toggleSelectAll,
    toggleSelecting,
    toggleSelect,
    clearSelection,
  } = useLibrarySelection({ visibleRows: visibleItems, groupSelection: sourceSelection });

  /** Every row matching the current search/date/origin/variant filter, ignoring group visibility: the
   *  exact owned set "Delete all" removes, never understated by a partly loaded list. */
  const collectAllMatching = useCallback(() => collectAllRows(source, filter), [source, filter]);

  useEffect(() => {
    if (!source.retention) return;
    source.retention
      .get()
      .then(setRetentionState)
      .catch((e) => setError(String(e)));
  }, [source]);

  useEffect(() => {
    if (!notice) return;
    const timeout = window.setTimeout(() => setNotice(""), 1800);
    return () => window.clearTimeout(timeout);
  }, [notice]);

  // Clear the pending copied-flash timer on unmount so it can't write state
  // on an unmounted component.
  useEffect(
    () => () => {
      if (copiedTimer.current !== undefined) window.clearTimeout(copiedTimer.current);
    },
    [],
  );

  useEffect(() => {
    if (!pendingConfirm) return;
    cancelConfirmRef.current?.focus();
  }, [pendingConfirm]);
  useDismiss(() => setPendingConfirm(null), {
    enabled: pendingConfirm !== null,
    outside: false,
    preventDefault: false,
    stopPropagation: false,
  });

  // The filter sends an origin's `filterKey` (the source's own key, e.g. a raw process name) and shows its
  // `label`; a source that matches on the key never sees the display form. Labels seen once stay known, so
  // a picked origin keeps its label while the filtered list is empty.
  const originLabels = useRef(new Map<string, string>());
  const appOptions = useMemo<LibraryFilterOption[]>(() => {
    const options = new Map<string, string>();
    for (const item of visibleItems) {
      if (!item.origin) continue;
      const value = item.origin.filterKey ?? item.origin.label;
      originLabels.current.set(value, item.origin.label);
      if (!options.has(value)) options.set(value, item.origin.label);
    }
    if (appFilter !== "all" && !options.has(appFilter)) {
      options.set(appFilter, originLabels.current.get(appFilter) ?? friendlyAppName(appFilter));
    }
    return Array.from(options, ([value, label]) => ({ value, label })).sort(
      (a, b) => a.label.localeCompare(b.label) || a.value.localeCompare(b.value),
    );
  }, [visibleItems, appFilter]);

  const hasActiveFilters =
    search.trim() !== "" || dateFilter !== "all" || appFilter !== "all" || !isAllSourcesSelected;

  const shownText = useCallback(
    (it: LibraryRow) => (showRaw[it.id] && it.altText ? it.altText : it.text),
    [showRaw],
  );

  const flashCopied = useCallback(async (item: LibraryRow, text: string, notice: string) => {
    try {
      await source.copyText(text);
      setCopiedId(item.id);
      setNotice(notice);
      if (copiedTimer.current !== undefined) window.clearTimeout(copiedTimer.current);
      copiedTimer.current = window.setTimeout(
        () => setCopiedId((current) => (current === item.id ? "" : current)),
        1400,
      );
    } catch (e) {
      setError(`Clipboard copy failed: ${String(e)}`);
    }
  }, [source]);

  const copyItem = useCallback(
    async (item: LibraryRow) => {
      await flashCopied(item, shownText(item), `${cap(labels.item)} copied.`);
    },
    [shownText, flashCopied],
  );

  const cancelCorrection = useCallback(() => {
    setEditingId("");
    setDraft("");
  }, []);

  const deleteItem = useCallback(
    async (item: LibraryRow) => {
      try {
        if (!source.remove) return;
        await source.remove(item.id);
        setNotice(`${cap(labels.item)} deleted.`);
        setEditingId((current) => {
          if (current === item.id) {
            setDraft("");
            return "";
          }
          return current;
        });
        await refresh();
      } catch (e) {
        setError(String(e));
      }
    },
    [refresh, source, labels],
  );

  // Every bulk-delete path deletes one confirmed, owned id at a time (never a "delete matching"
  // call, which could not tell owned rows from read-only ones); the deletes run in parallel.
  const deleteMany = useCallback(
    async (ids: string[]) => {
      const remove = source.remove;
      if (ids.length === 0 || !remove) return;
      try {
        const results = await Promise.allSettled(ids.map((id) => remove(id)));
        const failed = results.filter((r) => r.status === "rejected").length;
        if (failed > 0) {
          setError(`${failed} of ${ids.length} deletes failed.`);
        } else {
          setNotice(`${ids.length} ${ids.length === 1 ? labels.item : labels.items} deleted.`);
          clearSelection();
        }
        await refresh();
      } catch (e) {
        setError(String(e));
      }
    },
    [clearSelection, refresh, source, labels],
  );

  const changeRetention = useCallback(
    async (next: string) => {
      if (!source.retention) return;
      const previous = retention;
      setRetentionState(next);
      setRetentionSaving(true);
      try {
        const deleted = await source.retention.set(next);
        setNotice(
          deleted > 0
            ? `${deleted} expired ${deleted === 1 ? labels.item : labels.items} deleted.`
            : `${labels.area} retention updated.`,
        );
        await refresh();
      } catch (e) {
        setRetentionState(previous);
        setError(String(e));
      } finally {
        setRetentionSaving(false);
      }
    },
    [refresh, retention, source, labels],
  );

  const exportAll = source.exportAll;
  const exportHistory = useCallback(async () => {
    if (!exportAll) return;
    try {
      const count = await exportAll();
      if (count !== null) setNotice(`${count} ${count === 1 ? labels.item : labels.items} exported.`);
    } catch (e) {
      setError(String(e));
    }
  }, [exportAll, labels]);

  // "Export selected": the selected rows as plain text, written through the source's save dialog.
  const exportSelected = useCallback(async () => {
    const selected = visibleItems.filter((item) => selectedIds.has(item.id));
    if (selected.length === 0 || !source.saveText) return;
    try {
      const stem = exportName ?? labels.area.toLowerCase();
      const saved = await source.saveText(
        `${stem}-selected-${new Date().toISOString().slice(0, 10)}.txt`,
        formatRowsPlainText(selected, `${libraryAppName()} ${labels.area}`),
      );
      if (!saved) return;
      setNotice(`${selected.length} ${selected.length === 1 ? labels.item : labels.items} exported.`);
      clearSelection();
    } catch (e) {
      setError(String(e));
    }
  }, [visibleItems, selectedIds, clearSelection, source, exportName, labels]);

  const startCorrection = useCallback((item: LibraryRow) => {
    setEditingId(item.id);
    setDraft(item.text);
  }, []);

  const saveCorrection = useCallback(
    async (item: LibraryRow) => {
      const text = draft.trim();
      if (!text) return;
      try {
        if (!source.update) return;
        const saved = await source.update(item.id, text);
        if (!saved) throw new Error(`${cap(labels.item)} was not found.`);
        setNotice("Fix saved.");
        cancelCorrection();
        await refresh();
      } catch (e) {
        setError(String(e));
      }
    },
    [draft, cancelCorrection, refresh, source, labels],
  );

  const copyCorrection = useCallback(
    async (item: LibraryRow) => {
      const text = draft.trim();
      if (!text) return;
      await flashCopied(item, text, "Correction copied.");
    },
    [draft, flashCopied],
  );

  const requestDelete = useCallback(
    (item: LibraryRow) => {
      if (readStored("skipDeleteConfirm") === "1") void deleteItem(item);
      else {
        setDontAskAgain(false);
        setPendingConfirm({ kind: "delete", row: item });
      }
    },
    [deleteItem],
  );

  // HR-HISTORY-011: "Delete all" means every LOCALLY-OWNED (Mac-origin)
  // record in the current search/date/app/outcome scope — never foreign
  // rows, and never rows hidden by an unchecked source box (those are
  // simply not part of "the current source scope"). Walks every matching
  // page (not just what happens to be loaded) so the confirmation count —
  // and what actually gets deleted — is exact even when history exceeds one
  // page.
  const requestDeleteAll = useCallback(async () => {
    try {
      const matching = await collectAllMatching();
      const targets = matching.filter((row) => !row.readOnly).map((row) => row.id);
      if (targets.length === 0) return;
      pendingDeleteAllIds.current = targets;
      setDontAskAgain(false);
      setPendingConfirm({ kind: "clear", count: targets.length });
    } catch (e) {
      setError(String(e));
    }
  }, [collectAllMatching]);

  const requestDeleteSelected = useCallback(() => {
    const ids = visibleItems
      .filter((item) => selectedIds.has(item.id) && !item.readOnly)
      .map((item) => item.id);
    if (ids.length === 0) return;
    setDontAskAgain(false);
    setPendingConfirm({ kind: "selected", ids });
  }, [visibleItems, selectedIds]);

  const confirmPending = useCallback(async () => {
    const pending = pendingConfirm;
    if (!pending) return;
    setPendingConfirm(null);
    if (pending.kind === "delete") {
      if (dontAskAgain) writeStored("skipDeleteConfirm", "1");
      await deleteItem(pending.row);
    } else if (pending.kind === "clear") {
      const ids = pendingDeleteAllIds.current;
      pendingDeleteAllIds.current = [];
      await deleteMany(ids);
    } else {
      await deleteMany(pending.ids);
    }
  }, [pendingConfirm, dontAskAgain, deleteItem, deleteMany]);

  const toggleExpand = useCallback((id: string) => {
    setExpandedId((cur) => (cur === id ? "" : id));
  }, []);

  const toggleShowRaw = useCallback((id: string) => {
    setShowRaw((m) => ({ ...m, [id]: !m[id] }));
  }, []);

  const setDraftValue = useCallback((value: string) => setDraft(value), []);

  return (
    <LibraryContext.Provider value={context}>
    <PageShell maxWidth={maxWidth}>
      <div data-page="library" className="w-full">
        {/* Filters & selection actions share one fixed-height toolbar. */}
        <div className="flex h-11 w-full min-w-0 items-center gap-[var(--space-2)] overflow-x-auto border-b border-[color-mix(in_srgb,var(--line),transparent_40%)] py-[var(--space-2)]">
          <LibraryToolbar
            dateFilter={dateFilter}
            appFilter={appFilter}
            appOptions={appOptions}
            outcomeFilter={outcomeFilter}
            setDateFilter={setDateFilter}
            setAppFilter={setAppFilter}
            setOutcomeFilter={setOutcomeFilter}
          />
          <LibraryActionsLine
            retention={retention}
            retentionSaving={retentionSaving}
            setRetention={(value) => void changeRetention(value)}
            exportHistory={exportAll ? () => void exportHistory() : undefined}
            requestDeleteAll={() => void requestDeleteAll()}
            deletableCount={source.remove ? deletableCount : 0}
            isSelecting={isSelecting}
            selectedCount={selectedIds.size}
            allVisibleSelected={allVisibleSelected}
            toggleSelectAll={toggleSelectAll}
            exportSelected={() => void exportSelected()}
            requestDeleteSelected={requestDeleteSelected}
            toggleSelecting={toggleSelecting}
          />
        </div>
        {source.recovery ? <RecoveryBanner recovery={source.recovery} /> : null}
        {error ? (
          <Notice tone="bad" action={<button type="button" className="rk-btn rk-btn--ghost rk-btn--sm" onClick={() => setError("")}>Dismiss</button>}>
            {error}
          </Notice>
        ) : null}

        <LibraryNotice notice={notice} />

        <LibraryRows
          loaded={loaded}
          items={visibleItems}
          outcomeFilter={outcomeFilter}
          hasActiveFilters={hasActiveFilters}
          total={displayTotal}
          hasMore={hasMore}
          loadingMore={loadingMore}
          onLoadMore={loadMore}
          editingId={editingId}
          draft={draft}
          expandedId={expandedId}
          copiedId={copiedId}
          showRaw={showRaw}
          isSelecting={isSelecting}
          selectedIds={selectedIds}
          onSetDraft={setDraftValue}
          onToggleExpand={toggleExpand}
          onStartCorrection={startCorrection}
          onCancelCorrection={cancelCorrection}
          onSaveCorrection={saveCorrection}
          onCopyCorrection={copyCorrection}
          onCopyItem={copyItem}
          onToggleShowRaw={toggleShowRaw}
          onRequestDelete={requestDelete}
          onToggleSelect={toggleSelect}
        />
      </div>
      {pendingConfirm ? (
        <LibraryConfirmDialog
          pendingConfirm={pendingConfirm}
          dontAskAgain={dontAskAgain}
          cancelRef={cancelConfirmRef}
          setDontAskAgain={setDontAskAgain}
          onCancel={() => setPendingConfirm(null)}
          onConfirm={() => void confirmPending()}
        />
      ) : null}
    </PageShell>
    </LibraryContext.Provider>
  );
}

const cap = (text: string) => text.charAt(0).toUpperCase() + text.slice(1);

export default LibraryScreen;
