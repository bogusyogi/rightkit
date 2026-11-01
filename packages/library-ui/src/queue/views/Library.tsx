import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { HistoryRow, Kind } from "../contract";
import { invoke, open as openDialog } from "../lib/ipc";
import { createRequestGate } from "../lib/requestGate";
import { categoryForKind } from "../lib/jobDisplay";
import {
  countLabel,
  HISTORY_PAGE_SIZE,
  loadedRowsNotice,
  mergeHistoryRows,
  nearBottom,
  type HistoryPage,
} from "../lib/historyPage";
import { createShortcutRegistry } from "@rightkit/react-shell";
import { EmptyState } from "@rightkit/react-shell/react";
import { errorTextClass } from "../lib/ui";
import { LibraryList } from "./LibraryList";
import {
  LibraryToolbar,
  type CategoryFilter,
  type SortKey,
  type SourceFilter,
} from "./LibraryToolbar";

interface Props {
  onOpen: (id: string) => void;
  onNavigateToQueue?: () => void;
}

export function Library({ onOpen, onNavigateToQueue }: Props) {
  const [query, setQuery] = useState("");
  const [rows, setRows] = useState<HistoryRow[]>([]);
  const [loading, setLoading] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  // Whether the engine holds pages beyond the loaded rows (issue #228).
  const [hasMore, setHasMore] = useState(false);
  // Matching rows across every page, from the engine's first page (#228).
  const [total, setTotal] = useState<number | null>(null);
  const [deletingId, setDeletingId] = useState<string | null>(null);
  const [confirmDeleteId, setConfirmDeleteId] = useState<string | null>(null);
  const [kindFilter, setKindFilter] = useState<Kind | "all">("all");
  const [sourceFilter, setSourceFilter] = useState<SourceFilter>("all");
  const [catFilter, setCatFilter] = useState<CategoryFilter>("all");
  const [sortKey, setSortKey] = useState<SortKey>("newest");
  const [showFilters, setShowFilters] = useState(false);
  const [selectedIds, setSelectedIds] = useState<Set<string>>(new Set());
  // Bulk delete confirmation binds to the selection at arm time — the
  // user changing the selection afterwards must not widen or redirect the
  // confirmed delete onto rows they never confirmed.
  const [confirmBulkDeleteIds, setConfirmBulkDeleteIds] = useState<Set<string> | null>(null);
  const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const cursorRef = useRef<string | null>(null);
  const queryRef = useRef("");
  // The engine orders the pages, so the sort travels with every request.
  const sortRef = useRef<SortKey>("newest");
  const gateRef = useRef(createRequestGate());
  const loadingMoreRef = useRef(false);

  const search = useCallback((q: string) => {
    const generation = gateRef.current.begin();
    queryRef.current = q;
    cursorRef.current = null;
    loadingMoreRef.current = false;
    setLoading(true);
    setErr(null);
    invoke<HistoryPage>("get_history_page", {
      query: q,
      cursor: null,
      limit: HISTORY_PAGE_SIZE,
      sort: sortRef.current,
    })
      .then((page) => {
        if (!gateRef.current.isCurrent(generation)) return;
        cursorRef.current = page.next_cursor;
        setHasMore(Boolean(page.next_cursor));
        setTotal(typeof page.total === "number" ? page.total : null);
        setRows(page.rows);
        setLoading(false);
      })
      .catch((e) => {
        if (!gateRef.current.isCurrent(generation)) return;
        setErr(String(e));
        setRows([]);
        setHasMore(false);
        setTotal(null);
        setLoading(false);
      });
  }, []);

  // Library pages in on scroll: history is unlimited, responses are not.
  const loadMore = useCallback(() => {
    const cursor = cursorRef.current;
    if (!cursor || loadingMoreRef.current) return;
    const generation = gateRef.current.latest();
    loadingMoreRef.current = true;
    invoke<HistoryPage>("get_history_page", {
      query: queryRef.current,
      cursor,
      limit: HISTORY_PAGE_SIZE,
      sort: sortRef.current,
    })
      .then((page) => {
        if (!gateRef.current.isCurrent(generation)) return;
        cursorRef.current = page.next_cursor;
        setHasMore(Boolean(page.next_cursor));
        setRows((current) => mergeHistoryRows(current, page.rows));
      })
      .catch((e) => {
        if (gateRef.current.isCurrent(generation)) setErr(String(e));
      })
      .finally(() => {
        // A superseded page must not clear the flag a newer search or page request set.
        if (gateRef.current.isCurrent(generation)) loadingMoreRef.current = false;
      });
  }, []);

  // Drop a pending debounced search when the Library unmounts.
  useEffect(
    () => () => {
      if (debounceRef.current) clearTimeout(debounceRef.current);
    },
    [],
  );

  // Initial load — empty query returns all rows
  useEffect(() => {
    search("");
  }, [search]);

  // A new order reloads from the engine's first page.
  const appliedSortRef = useRef<SortKey>("newest");
  useEffect(() => {
    if (appliedSortRef.current === sortKey) return;
    appliedSortRef.current = sortKey;
    sortRef.current = sortKey;
    search(queryRef.current);
  }, [sortKey, search]);

  const handleChange = useCallback(
    (e: React.ChangeEvent<HTMLInputElement>) => {
      const q = e.target.value;
      setQuery(q);
      if (debounceRef.current) clearTimeout(debounceRef.current);
      debounceRef.current = setTimeout(() => search(q), 280);
    },
    [search],
  );

  const handleDelete = useCallback(
    async (row: HistoryRow) => {
      if (confirmDeleteId !== row.id) {
        setConfirmDeleteId(row.id);
        return;
      }
      setConfirmDeleteId(null);
      setDeletingId(row.id);
      setErr(null);
      try {
        await invoke("delete_transcript", { id: row.id });
        setRows((current) => current.filter((item) => item.id !== row.id));
        // Selection changed — any armed bulk confirm is stale and disarms.
        setConfirmBulkDeleteIds(null);
        setSelectedIds((s) => {
          const n = new Set(s);
          n.delete(row.id);
          return n;
        });
      } catch (e) {
        setErr(String(e));
      } finally {
        setDeletingId(null);
      }
    },
    [confirmDeleteId],
  );

  const toggleSelect = useCallback((id: string) => {
    // Any selection change disarms a pending bulk delete: the confirmed set
    // is bound to the selection that armed it, never to a later one.
    setConfirmBulkDeleteIds(null);
    setSelectedIds((s) => {
      const n = new Set(s);
      if (n.has(id)) n.delete(id);
      else n.add(id);
      return n;
    });
  }, []);

  const filteredRows = useMemo(() => {
    let result = rows;
    if (kindFilter !== "all") {
      result = result.filter((r) => r.kind === kindFilter);
    }
    if (sourceFilter === "local") {
      result = result.filter((r) => r.kind === "local_file");
    } else if (sourceFilter === "remote") {
      result = result.filter((r) => r.kind !== "local_file");
    }
    if (catFilter !== "all") {
      result = result.filter((r) => categoryForKind(r.kind) === catFilter);
    }
    // Rows arrive in the engine's order for `sortKey`; filters only narrow them.
    return result;
  }, [rows, kindFilter, sourceFilter, catFilter]);

  // Full category grouping (100% plan) - only when no specific filters
  const grouped = useMemo(() => {
    if (kindFilter !== "all" || sourceFilter !== "all" || catFilter !== "all") return null;
    const byCat: Record<string, HistoryRow[]> = {};
    for (const r of filteredRows) {
      const cat = categoryForKind(r.kind);
      byCat[cat] ??= [];
      byCat[cat].push(r);
    }
    return Object.entries(byCat);
  }, [filteredRows, kindFilter, sourceFilter, catFilter]);

  const selectAll = useCallback(() => {
    setConfirmBulkDeleteIds(null);
    setSelectedIds(new Set(filteredRows.map((r) => r.id)));
  }, [filteredRows]);

  const clearSelection = useCallback(() => {
    setConfirmBulkDeleteIds(null);
    setSelectedIds(new Set());
  }, []);

  const bulkDelete = useCallback(async () => {
    if (!selectedIds.size && !confirmBulkDeleteIds?.size) return;
    // Two-step arming, same as the per-row delete: a bulk delete of N
    // transcripts must not fire on a single click or stray Delete key.
    if (!confirmBulkDeleteIds) {
      setConfirmBulkDeleteIds(new Set(selectedIds));
      return;
    }
    // Only the armed set deletes — a selection changed after arming never
    // redirects the confirmed delete onto unconfirmed rows.
    const armed = confirmBulkDeleteIds;
    setConfirmBulkDeleteIds(null);
    const ids = [...armed];
    setErr(null);
    // Per-item failure retention: only ids whose delete actually succeeded
    // leave the list — failures stay visible AND selected so the user sees
    // exactly which transcripts survived.
    const failed = (
      await Promise.all(
        ids.map((id) =>
          invoke("delete_transcript", { id })
            .then(() => null)
            .catch(() => id),
        ),
      )
    ).filter((id): id is string => id !== null);
    const failedSet = new Set(failed);
    setRows((current) => current.filter((r) => !armed.has(r.id) || failedSet.has(r.id)));
    setSelectedIds((current) => {
      const next = new Set([...current].filter((id) => !armed.has(id)));
      for (const id of failed) next.add(id);
      return next;
    });
    if (failed.length) {
      setErr(`${failed.length} transcript(s) could not be deleted and were kept selected.`);
    }
  }, [selectedIds, confirmBulkDeleteIds]);

  // Delete/Backspace bulk-deletes the selection. Bare keys never fire while a
  // field has focus (the registry's typing guard), so the old editing check is gone.
  useEffect(() => {
    if (!selectedIds.size) return;
    const registry = createShortcutRegistry();
    registry.register({
      id: "library.delete-selected",
      chord: ["delete", "backspace"],
      label: "Delete selected transcripts",
      run: () => {
        void bulkDelete();
      },
    });
    return registry.attach(window);
  }, [bulkDelete, selectedIds.size]);

  const bulkExport = useCallback(async () => {
    if (!selectedIds.size) return;
    const dir = await openDialog({ directory: true, multiple: false });
    if (!dir) return;
    const ids = [...selectedIds];
    setErr(null);
    try {
      await Promise.all(
        ids.map(async (id) => {
          const detail = await invoke<{ title?: string | null; text: string; url: string }>(
            "get_transcript",
            { id },
          );
          const name = (detail.title ?? "transcript").replace(/[^a-zA-Z0-9 _-]/g, "_").slice(0, 60);
          await invoke("save_transcript_as", {
            id,
            dest: `${dir}/${name} [${id}].txt`,
            format: "txt",
          });
        }),
      );
      setSelectedIds(new Set());
    } catch (e) {
      setErr(String(e));
    }
  }, [selectedIds]);

  // Reset confirm state when rows change (e.g. after search).
  // biome-ignore lint/correctness/useExhaustiveDependencies: rows is the intended trigger, not a value read inside the effect
  useEffect(() => {
    setConfirmDeleteId(null);
    setConfirmBulkDeleteIds(null);
  }, [rows]);

  return (
    <main
      className="flex flex-1 flex-col gap-4 overflow-auto p-5"
      onScroll={(event) => {
        if (nearBottom(event.currentTarget)) loadMore();
      }}
    >
      <LibraryToolbar
        query={query}
        loading={loading}
        showFilters={showFilters}
        selectedCount={selectedIds.size}
        visibleCount={filteredRows.length}
        kindFilter={kindFilter}
        sourceFilter={sourceFilter}
        catFilter={catFilter}
        sortKey={sortKey}
        onQueryChange={handleChange}
        onToggleFilters={() => setShowFilters((v) => !v)}
        onSelectAll={selectAll}
        onClearSelection={clearSelection}
        confirmBulkDelete={confirmBulkDeleteIds !== null}
        onBulkDelete={bulkDelete}
        onBulkExport={bulkExport}
        onKindFilter={setKindFilter}
        onSourceFilter={setSourceFilter}
        onCatFilter={setCatFilter}
        onSortKey={setSortKey}
      />

      {err && <div className={errorTextClass}>invoke failed: {err}</div>}

      <p className="m-0 font-[family-name:var(--mono)] text-xs text-[var(--muted)]">
        {countLabel(rows.length, total)} transcripts
      </p>
      {loadedRowsNotice(rows.length, hasMore) && (
        <p className="m-0 font-[family-name:var(--mono)] text-xs text-[var(--muted)]">
          {loadedRowsNotice(rows.length, hasMore)}
        </p>
      )}

      {!err && filteredRows.length === 0 && !loading && (
        <EmptyState
          title={query ? `No results for "${query}".` : "No transcripts yet."}
          action={
            !query && onNavigateToQueue ? (
              <button
                type="button"
                className="mt-3 inline-flex h-9 items-center gap-2 rounded-[6px] border border-[var(--accent)] bg-[color-mix(in_srgb,var(--accent)_15%,var(--panel-2))] px-4 text-sm font-medium text-[var(--accent)] transition-colors hover:bg-[color-mix(in_srgb,var(--accent)_25%,var(--panel-2))]"
                onClick={onNavigateToQueue}
              >
                Go to Queue
              </button>
            ) : undefined
          }
        />
      )}

      {filteredRows.length > 0 && (
        <LibraryList
          rows={filteredRows}
          grouped={grouped}
          selectedIds={selectedIds}
          confirmDeleteId={confirmDeleteId}
          deletingId={deletingId}
          onOpen={onOpen}
          onToggleSelect={toggleSelect}
          onDelete={handleDelete}
        />
      )}
    </main>
  );
}
