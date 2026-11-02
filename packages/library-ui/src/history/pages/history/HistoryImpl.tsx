import { useDismiss } from "@rightkit/platform-ui/headless";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import ErrorBanner from "@rightkit/dictation-ui/components/ErrorBanner";
import PageShell from "@rightkit/dictation-ui/components/PageShell";
import { useHistorySourceSelection } from "@rightkit/dictation-ui/hooks/useHistorySourceSelection";
import { historySearch, useField } from "@rightkit/dictation-ui/lib/fieldStore";
import type { HistoryItem } from "@rightkit/dictation-ui/lib/homeMetrics";
import { invoke } from "@rightkit/dictation-ui/lib/tauri";
import {
  HistoryConfirmDialog,
  HistoryNotice,
  HistoryActionsLine,
  HistoryToolbar,
  type HistoryRetention,
} from "./HistoryChrome.js";
import { HistoryList } from "./HistoryList.js";
import {
  SKIP_DELETE_CONFIRM,
  type DateFilter,
  dateFilterRange,
  formatSelectedPlainText,
  historyDevice,
  isLocallyDeletable,
  type OutcomeFilter,
  type PendingConfirm,
  targetLabel,
} from "@rightkit/dictation-ui/pages/history/historyUtils";
import {
  HISTORY_PAGE_SIZE,
  type HistoryPageResult,
  type HistoryQueryArgs,
  type NativeHistoryFilter,
  useHistoryQuery,
} from "./useHistoryQuery.js";
import HistoryRecoveryBanner from "../../components/HistoryRecoveryBanner.js";
import { useHistorySelection } from "./useHistorySelection.js";

export default function HistoryPage() {
  // The search field lives in the title bar; this page consumes the shared
  // store so both stay in sync (see fieldStore.ts).
  const [search] = useField(historySearch);
  const [dateFilter, setDateFilter] = useState<DateFilter>("all");
  // HR-HISTORY-003/004/005: inclusive Mac/iPhone/Watch checkboxes, sole
  // visibility control (replaces the earlier exclusive single-select
  // picker). Desktop defaults to Mac only; loaded once from this device's
  // persisted choice, falling back to that default on first run.
  const { sourceSelection } = useHistorySourceSelection();
  const [appFilter, setAppFilter] = useState("all");
  const [outcomeFilter, setOutcomeFilter] = useState<OutcomeFilter>("dictations");
  const [error, setError] = useState<string>("");
  const [copiedId, setCopiedId] = useState("");
  const [expandedId, setExpandedId] = useState("");
  const [showRaw, setShowRaw] = useState<Record<string, boolean>>({});
  const [notice, setNotice] = useState("");
  const [pendingConfirm, setPendingConfirm] = useState<PendingConfirm>(null);
  const [dontAskAgain, setDontAskAgain] = useState(false);
  const [editingId, setEditingId] = useState("");
  const [draft, setDraft] = useState("");
  const [retention, setRetentionState] = useState<HistoryRetention>("forever");
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

  const isAllSourcesSelected =
    sourceSelection.mac && sourceSelection.iphone && sourceSelection.watch;

  const nativeFilter = useMemo<NativeHistoryFilter>(() => {
    const range = dateFilterRange(dateFilter);
    return {
      search: search.trim(),
      from_ms: range.from_ms,
      until_ms: range.until_ms,
      app: appFilter === "all" ? "" : appFilter,
    };
  }, [search, dateFilter, appFilter]);

  // Separate from nativeFilter (used by delete_history_matching, which doesn't
  // take the outcome scope) so the fetch-only query args don't leak an
  // unsupported field into unrelated commands. `cancelled_only` is an EXCLUSIVE
  // server-side scope: the Cancelled tab returns only cancelled rows, so the
  // count and pagination reflect exactly what's shown (no client re-filtering).
  const queryArgs = useMemo<HistoryQueryArgs>(
    () => ({ ...nativeFilter, cancelled_only: outcomeFilter === "cancelled" }),
    [nativeFilter, outcomeFilter],
  );

  // Query/pagination lifecycle (fetch, page drain, load-more, refresh
  // triggers) lives in useHistoryQuery. `exhaustive` makes refresh walk every
  // matching page whenever a source checkbox is off, so the source scope
  // applied below sees the complete matching set.
  const { items, total, hasMore, loaded, loadingMore, refresh, loadMore } = useHistoryQuery({
    queryArgs,
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
        : items.filter((item) => sourceSelection[historyDevice(item)]),
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
  } = useHistorySelection({ visibleItems, sourceSelection });

  /** Full walk of every page matching the current search/date/app/outcome
   *  filter (ignores the source checkboxes — used to compute an EXACT
   *  locally-owned set for "Delete all", which must never understate what a
   *  partially-loaded page would suggest). Bounded by MAX_HISTORY_ITEMS.
   *  Stays page-side because it feeds only the delete-all confirmation below,
   *  not the displayed list (which the query hook owns). */
  const collectAllMatching = useCallback(async (): Promise<HistoryItem[]> => {
    const collected: HistoryItem[] = [];
    let offset = 0;
    let page = await invoke<HistoryPageResult>("query_history", {
      query: { ...queryArgs, offset, limit: HISTORY_PAGE_SIZE },
    });
    collected.push(...page.items);
    while (page.has_more && page.items.length > 0) {
      offset += page.items.length;
      page = await invoke<HistoryPageResult>("query_history", {
        query: { ...queryArgs, offset, limit: HISTORY_PAGE_SIZE },
      });
      collected.push(...page.items);
    }
    return collected;
  }, [queryArgs]);

  useEffect(() => {
    invoke<HistoryRetention>("get_history_retention")
      .then(setRetentionState)
      .catch((e) => setError(String(e)));
  }, []);

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

  const appOptions = useMemo(() => {
    const apps = new Set(visibleItems.map(targetLabel));
    if (appFilter !== "all") apps.add(appFilter);
    return Array.from(apps).sort((a, b) => a.localeCompare(b));
  }, [visibleItems, appFilter]);

  const hasActiveFilters =
    search.trim() !== "" || dateFilter !== "all" || appFilter !== "all" || !isAllSourcesSelected;

  const shownText = useCallback(
    (it: HistoryItem) =>
      showRaw[it.delivery_id] && it.raw_transcript ? it.raw_transcript : it.transcript,
    [showRaw],
  );

  const flashCopied = useCallback(async (item: HistoryItem, text: string, notice: string) => {
    try {
      await invoke<void>("copy_text_to_clipboard", { text });
      setCopiedId(item.delivery_id);
      setNotice(notice);
      if (copiedTimer.current !== undefined) window.clearTimeout(copiedTimer.current);
      copiedTimer.current = window.setTimeout(
        () => setCopiedId((current) => (current === item.delivery_id ? "" : current)),
        1400,
      );
    } catch (e) {
      setError(`Clipboard copy failed: ${String(e)}`);
    }
  }, []);

  const copyItem = useCallback(
    async (item: HistoryItem) => {
      await flashCopied(item, shownText(item), "Transcript copied.");
    },
    [shownText, flashCopied],
  );

  const cancelCorrection = useCallback(() => {
    setEditingId("");
    setDraft("");
  }, []);

  const deleteItem = useCallback(
    async (item: HistoryItem) => {
      try {
        await invoke("delete_history_item", { deliveryId: item.delivery_id });
        setNotice("Transcript deleted.");
        setEditingId((current) => {
          if (current === item.delivery_id) {
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
    [refresh],
  );

  // HR-HISTORY-006/007/011: every bulk-delete path goes through this same
  // per-id loop instead of the native `delete_history_matching` command —
  // that command has no source-origin awareness at all, so calling it
  // directly (even when every source checkbox is on) would delete
  // iPhone/Watch-origin rows synced in via CloudKit right along with this
  // Mac's own rows. Deleting one confirmed-local id at a time is the
  // frontend-side half of the origin enforcement; the data-layer half lives
  // in `heardright_core::history::{delete_scoped, delete_matching_scoped}`
  // (see docs/dispatch/evidence/history-inclusive-sources.md — wiring those
  // into this command is an escalation, since the Tauri command itself
  // lives outside this lane's exclusive-ownership scope).
  const deleteMany = useCallback(
    async (ids: string[]) => {
      if (ids.length === 0) return;
      try {
        // Per-id deletes (not delete_history_matching) keep source-origin
        // enforcement — but they are independent, so they run in parallel.
        const results = await Promise.allSettled(
          ids.map((id) => invoke("delete_history_item", { deliveryId: id })),
        );
        const failed = results.filter((r) => r.status === "rejected").length;
        if (failed > 0) {
          setError(`${failed} of ${ids.length} transcript deletes failed.`);
        } else {
          setNotice(`${ids.length} transcript${ids.length === 1 ? "" : "s"} deleted.`);
          clearSelection();
        }
        await refresh();
      } catch (e) {
        setError(String(e));
      }
    },
    [clearSelection, refresh],
  );

  const changeRetention = useCallback(
    async (next: HistoryRetention) => {
      const previous = retention;
      setRetentionState(next);
      setRetentionSaving(true);
      try {
        const deleted = await invoke<number>("set_history_retention", { retention: next });
        setNotice(
          deleted > 0
            ? `${deleted} expired transcript${deleted === 1 ? "" : "s"} deleted.`
            : "History retention updated.",
        );
        await refresh();
      } catch (e) {
        setRetentionState(previous);
        setError(String(e));
      } finally {
        setRetentionSaving(false);
      }
    },
    [refresh, retention],
  );

  const exportHistory = useCallback(async () => {
    try {
      const { save } = await import("@tauri-apps/plugin-dialog");
      const destination = await save({
        defaultPath: `heardright-history-${new Date().toISOString().slice(0, 10)}.json`,
        filters: [
          { name: "JSON (portable)", extensions: ["json"] },
          { name: "Markdown", extensions: ["md"] },
          { name: "Plain text", extensions: ["txt"] },
          { name: "CSV", extensions: ["csv"] },
        ],
      });
      if (!destination) return;
      const count = await invoke<number>("export_history", { path: destination });
      setNotice(`${count} transcript${count === 1 ? "" : "s"} exported.`);
    } catch (e) {
      setError(String(e));
    }
  }, []);

  // HR-HISTORY-009 "Export selected". The native `export_history` command
  // exports the entire history unconditionally with no id list/filter
  // parameter (see the comment on `deleteMany` above — same ownership
  // boundary), so this builds the selected rows' text client-side and
  // writes it out through the existing generic `write_text_file` command
  // (`src-tauri/src/commands/files.rs`, already registered and
  // locked to .txt/.srt/.vtt) instead of requiring a new backend command.
  const exportSelected = useCallback(async () => {
    const selected = visibleItems.filter((item) => selectedIds.has(item.delivery_id));
    if (selected.length === 0) return;
    try {
      const { save } = await import("@tauri-apps/plugin-dialog");
      const destination = await save({
        defaultPath: `heardright-history-selected-${new Date().toISOString().slice(0, 10)}.txt`,
        filters: [{ name: "Plain text", extensions: ["txt"] }],
      });
      if (!destination) return;
      await invoke("write_text_file", {
        path: destination,
        contents: formatSelectedPlainText(selected),
      });
      setNotice(`${selected.length} transcript${selected.length === 1 ? "" : "s"} exported.`);
      clearSelection();
    } catch (e) {
      setError(String(e));
    }
  }, [visibleItems, selectedIds, clearSelection]);

  const startCorrection = useCallback((item: HistoryItem) => {
    setEditingId(item.delivery_id);
    setDraft(item.transcript);
  }, []);

  const saveCorrection = useCallback(
    async (item: HistoryItem) => {
      const text = draft.trim();
      if (!text) return;
      try {
        const saved = await invoke<boolean>("update_history_item", {
          deliveryId: item.delivery_id,
          transcript: text,
        });
        if (!saved) throw new Error("Transcript was not found.");
        setNotice("Fix saved.");
        cancelCorrection();
        await refresh();
      } catch (e) {
        setError(String(e));
      }
    },
    [draft, cancelCorrection, refresh],
  );

  const copyCorrection = useCallback(
    async (item: HistoryItem) => {
      const text = draft.trim();
      if (!text) return;
      await flashCopied(item, text, "Correction copied.");
    },
    [draft, flashCopied],
  );

  const requestDelete = useCallback(
    (item: HistoryItem) => {
      if (localStorage.getItem(SKIP_DELETE_CONFIRM) === "1") void deleteItem(item);
      else {
        setDontAskAgain(false);
        setPendingConfirm({ kind: "delete", item });
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
      const targets = matching.filter(isLocallyDeletable).map((item) => item.delivery_id);
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
      .filter((item) => selectedIds.has(item.delivery_id) && isLocallyDeletable(item))
      .map((item) => item.delivery_id);
    if (ids.length === 0) return;
    setDontAskAgain(false);
    setPendingConfirm({ kind: "selected", ids });
  }, [visibleItems, selectedIds]);

  const confirmPending = useCallback(async () => {
    const pending = pendingConfirm;
    if (!pending) return;
    setPendingConfirm(null);
    if (pending.kind === "delete") {
      if (dontAskAgain) localStorage.setItem(SKIP_DELETE_CONFIRM, "1");
      await deleteItem(pending.item);
    } else if (pending.kind === "clear") {
      const ids = pendingDeleteAllIds.current;
      pendingDeleteAllIds.current = [];
      await deleteMany(ids);
    } else {
      await deleteMany(pending.ids);
    }
  }, [pendingConfirm, dontAskAgain, deleteItem, deleteMany]);

  const toggleExpand = useCallback((deliveryId: string) => {
    setExpandedId((cur) => (cur === deliveryId ? "" : deliveryId));
  }, []);

  const toggleShowRaw = useCallback((deliveryId: string) => {
    setShowRaw((m) => ({ ...m, [deliveryId]: !m[deliveryId] }));
  }, []);

  const setDraftValue = useCallback((value: string) => setDraft(value), []);

  return (
    <PageShell
      tab="history"
      fixedHeight={false}
      headerInContent
      header={
        // Filters & selection actions share one fixed-height toolbar.
        <div className="flex h-11 w-full min-w-0 items-center gap-[var(--space-2)] overflow-x-auto">
          <HistoryToolbar
            dateFilter={dateFilter}
            appFilter={appFilter}
            appOptions={appOptions}
            outcomeFilter={outcomeFilter}
            setDateFilter={setDateFilter}
            setAppFilter={setAppFilter}
            setOutcomeFilter={setOutcomeFilter}
          />
          <HistoryActionsLine
            retention={retention}
            retentionSaving={retentionSaving}
            setRetention={(value) => void changeRetention(value)}
            exportHistory={() => void exportHistory()}
            requestDeleteAll={() => void requestDeleteAll()}
            deletableCount={deletableCount}
            isSelecting={isSelecting}
            selectedCount={selectedIds.size}
            allVisibleSelected={allVisibleSelected}
            toggleSelectAll={toggleSelectAll}
            exportSelected={() => void exportSelected()}
            requestDeleteSelected={requestDeleteSelected}
            toggleSelecting={toggleSelecting}
          />
        </div>
      }
    >
      <div className="w-full">
        <HistoryRecoveryBanner />
        {error ? <ErrorBanner message={error} onDismiss={() => setError("")} /> : null}

        <HistoryNotice notice={notice} />

        <HistoryList
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
        <HistoryConfirmDialog
          pendingConfirm={pendingConfirm}
          dontAskAgain={dontAskAgain}
          cancelRef={cancelConfirmRef}
          setDontAskAgain={setDontAskAgain}
          onCancel={() => setPendingConfirm(null)}
          onConfirm={() => void confirmPending()}
        />
      ) : null}
    </PageShell>
  );
}
