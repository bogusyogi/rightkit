import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { cx } from "@rightkit/react-shell/react";
import type { LibraryRow } from "../contract.js";
import { LibraryRowView, useLibraryButton, useLibraryContext } from "./LibraryChrome.js";
import { dayLabel, emptyClass, startOfDay, surfaceClass } from "./format.js";

// Render window: at most a viewport's worth of rows (plus overscan)
// stay mounted at once. Everything that consumes history data — selection,
// Export selected, Delete all, the "Load more" count — operates on the loaded
// `items` array or a fresh backend page walk, never on mounted rows, so
// unmounting offscreen rows changes nothing semantically. Offscreen spans
// become estimated-height spacers, which is what actually bounds the DOM
// node count and the shared transcript-measure subscriptions as "Load more"
// pages accumulate.
const RENDER_OVERSCAN = 24;
// Below this many rows the list renders in full — no spacers, no scroll
// listener, byte-for-byte the old behavior for the common one-page case.
const FULL_RENDER_LIMIT = 200;
// First-paint guess for a collapsed row's height (px); corrected from the
// live rendered rows on every layout pass.
const ROW_HEIGHT_FALLBACK = 72;

function findScrollParent(el: HTMLElement): HTMLElement | null {
  let node = el.parentElement;
  while (node) {
    const overflowY = getComputedStyle(node).overflowY;
    if (overflowY === "auto" || overflowY === "scroll") return node;
    node = node.parentElement;
  }
  return null;
}

/**
 * Pure list surface: start-of-day grouping + day labels, the empty state, a
 * windowed row per item, and the "Load more"
 * paginator. Every piece of interaction state arrives via props — fetching,
 * selection, and mutation behavior all live in the page/hooks above this
 * component.
 */
export function LibraryRows({
  loaded,
  items,
  outcomeFilter,
  hasActiveFilters,
  total,
  hasMore,
  loadingMore,
  onLoadMore,
  editingId,
  draft,
  expandedId,
  copiedId,
  showRaw,
  isSelecting,
  selectedIds,
  onSetDraft,
  onToggleExpand,
  onStartCorrection,
  onCancelCorrection,
  onSaveCorrection,
  onCopyCorrection,
  onCopyItem,
  onToggleShowRaw,
  onRequestDelete,
  onToggleSelect,
}: {
  loaded: boolean;
  /** The already-scoped rows to render (visibleItems from the page). */
  items: LibraryRow[];
  outcomeFilter: string;
  hasActiveFilters: boolean;
  total: number;
  hasMore: boolean;
  loadingMore: boolean;
  onLoadMore: () => Promise<void>;
  editingId: string;
  draft: string;
  expandedId: string;
  copiedId: string;
  showRaw: Record<string, boolean>;
  isSelecting: boolean;
  selectedIds: Set<string>;
  onSetDraft: (value: string) => void;
  onToggleExpand: (id: string) => void;
  onStartCorrection: (item: LibraryRow) => void;
  onCancelCorrection: () => void;
  onSaveCorrection: (item: LibraryRow) => void;
  onCopyCorrection: (item: LibraryRow) => void;
  onCopyItem: (item: LibraryRow) => void;
  onToggleShowRaw: (id: string) => void;
  onRequestDelete: (item: LibraryRow) => void;
  onToggleSelect: (id: string) => void;
}) {
  const grouped = useMemo(() => {
    const map = new Map<number, LibraryRow[]>();
    for (const it of items) {
      const key = startOfDay(it.timestampMs);
      const bucket = map.get(key) ?? [];
      bucket.push(it);
      map.set(key, bucket);
    }
    return Array.from(map.entries()).sort((a, b) => b[0] - a[0]);
  }, [items]);

  // ---- render window state ---------------------------------------------
  const listRef = useRef<HTMLDivElement>(null);
  const renderedRef = useRef<HTMLDivElement>(null);
  const scrollerRef = useRef<HTMLElement | null>(null);
  const rowHeightRef = useRef(ROW_HEIGHT_FALLBACK);
  const renderedCountRef = useRef(0);
  const windowed = items.length > FULL_RENDER_LIMIT;
  // [start, end) item-index span that is mounted. Indexing follows the
  // grouped display order (same order `grouped` emits).
  const [range, setRange] = useState<[number, number]>([0, FULL_RENDER_LIMIT]);

  const updateWindow = useCallback(() => {
    const el = listRef.current;
    const sc = scrollerRef.current;
    if (!el || !sc) return;
    const rowH = rowHeightRef.current;
    // List top in scroll-content coordinates (stable across scrolling).
    const listTop = el.getBoundingClientRect().top - sc.getBoundingClientRect().top + sc.scrollTop;
    const viewTop = sc.scrollTop - listTop;
    const viewBot = viewTop + sc.clientHeight;
    const count = items.length;
    const start = Math.max(0, Math.min(count, Math.floor(viewTop / rowH) - RENDER_OVERSCAN));
    const end = Math.max(start, Math.min(count, Math.ceil(viewBot / rowH) + RENDER_OVERSCAN));
    setRange((prev) => (prev[0] === start && prev[1] === end ? prev : [start, end]));
  }, [items.length]);

  // Attach the scroll listener (rAF-throttled) + window resize to the page's
  // scroll region. One pair of listeners for the whole list.
  useEffect(() => {
    if (!windowed) return;
    const el = listRef.current;
    if (!el) return;
    const sc = findScrollParent(el);
    scrollerRef.current = sc;
    if (!sc) {
      // No scrollable ancestor (non-standard host, headless tests): render
      // everything rather than leave rows permanently unmounted.
      setRange([0, items.length]);
      return;
    }
    let raf = 0;
    const onScroll = () => {
      if (raf) return;
      raf = window.requestAnimationFrame(() => {
        raf = 0;
        updateWindow();
      });
    };
    sc.addEventListener("scroll", onScroll, { passive: true });
    window.addEventListener("resize", onScroll);
    updateWindow();
    return () => {
      scrollerRef.current = null;
      sc.removeEventListener("scroll", onScroll);
      window.removeEventListener("resize", onScroll);
      if (raf) window.cancelAnimationFrame(raf);
    };
  }, [windowed, updateWindow, items.length]);

  // Recalibrate when content or row controls change, then update the window.
  // A range-only render must retain its estimate: measuring the new slice
  // can choose a different range, whose height chooses the previous range
  // again. That synchronous layout feedback crashes long, mixed-height lists.
  // biome-ignore lint/correctness/useExhaustiveDependencies: Row controls change measured DOM height; range-only renders must not recalibrate.
  useLayoutEffect(() => {
    if (!windowed) return;
    const rendered = renderedRef.current;
    if (rendered && renderedCountRef.current > 0) {
      const measured = rendered.offsetHeight / renderedCountRef.current;
      if (measured > 8 && Math.abs(measured - rowHeightRef.current) > 2) {
        rowHeightRef.current = measured;
      }
    }
    updateWindow();
  }, [windowed, items, expandedId, editingId, draft, isSelecting, showRaw, updateWindow]);

  const { variants, labels } = useLibraryContext();
  const Btn = useLibraryButton();
  if (!loaded) return null;
  const variant = variants.find((v) => v.key === outcomeFilter);
  // A variant's own empty copy shows when nothing is stored yet, or always for a variant past the first
  // (e.g. "Cancelled"); otherwise the empty list is a filter miss.
  const ownEmpty = !!variant && (variant !== variants[0] || (!hasActiveFilters && total === 0));
  const emptyTitle = ownEmpty && variant?.emptyTitle ? variant.emptyTitle : !hasActiveFilters && total === 0 ? `No ${labels.items} yet.` : (labels.noMatch ?? `No ${labels.items} match your search.`);
  const emptyBody = ownEmpty && variant?.emptyBody ? variant.emptyBody : !hasActiveFilters && total === 0 ? "" : "Try a shorter phrase or clear the search field.";

  return (
    <>
      {items.length === 0 ? (
        <div
          id="history-list"
          role="tabpanel"
          aria-labelledby={`history-tab-${outcomeFilter}`}
          className={emptyClass}
        >
          <p className="m-0 text-[var(--paper)]">
            {emptyTitle}
          </p>
          <p className="m-0 mt-[var(--space-1)] text-[length:var(--size-sm)] text-[var(--muted)]">
            {emptyBody}
          </p>
        </div>
      ) : (
        <div
          id="history-list"
          role="tabpanel"
          ref={listRef}
          aria-labelledby={`history-tab-${outcomeFilter}`}
        >
          {(() => {
            // Per-group render; when windowed, only rows inside `range` mount
            // and the rest collapse into measured-height spacers.
            const [winStart, winEnd] = windowed ? range : [0, items.length];
            const rowH = rowHeightRef.current;
            let renderedCount = 0;
            let cursor = 0; // global item index at the start of each group
            const dayGroups = grouped.map(([dayKey, rows]) => {
              const groupStart = cursor;
              cursor += rows.length;
              const dayLabelText = dayLabel(dayKey);
              // Indices of this group's rows that fall inside the window.
              const sliceStart = Math.max(0, winStart - groupStart);
              const sliceEnd = Math.min(rows.length, winEnd - groupStart);
              if (sliceEnd <= sliceStart) return null;
              renderedCount += sliceEnd - sliceStart;
              const slice = rows.slice(sliceStart, sliceEnd);
              return (
                <div key={dayKey}>
                  {/* Section header in the mockup's `.sh` grammar: day label
                      left, a quiet item count right. */}
                  <h3
                    className={cx(
                      "mb-[var(--space-2)] flex items-baseline justify-between px-1 text-[length:var(--size-base)] font-semibold text-[var(--muted-2)]",
                      groupStart === 0 ? "mt-0" : "mt-[var(--space-4)]",
                    )}
                  >
                    {dayLabelText}
                    <span className="text-[length:var(--size-xs)] font-normal text-[var(--muted)]">
                      {rows.length} {rows.length === 1 ? "item" : "items"}
                    </span>
                  </h3>
                  {/* Rows for this day sit inside one inset group card; the
                      rows own their own hairline separators. */}
                  <div className={cx(surfaceClass, "overflow-hidden")}>
                    {slice.map((it, i) => (
                      <LibraryRowView
                        key={it.id}
                        item={it}
                        actionContext={`${dayLabelText} item ${sliceStart + i + 1}`}
                        isEditing={editingId === it.id}
                        isExpanded={expandedId === it.id}
                        isCopied={copiedId === it.id}
                        showRaw={!!showRaw[it.id]}
                        draft={editingId === it.id ? draft : ""}
                        isSelecting={isSelecting}
                        isSelected={selectedIds.has(it.id)}
                        isDeletable={!it.readOnly}
                        onSetDraft={onSetDraft}
                        onToggleExpand={onToggleExpand}
                        onStartCorrection={onStartCorrection}
                        onCancelCorrection={onCancelCorrection}
                        onSaveCorrection={onSaveCorrection}
                        onCopyCorrection={onCopyCorrection}
                        onCopyItem={onCopyItem}
                        onToggleShowRaw={onToggleShowRaw}
                        onRequestDelete={onRequestDelete}
                        onToggleSelect={onToggleSelect}
                      />
                    ))}
                  </div>
                </div>
              );
            });
            renderedCountRef.current = renderedCount;
            if (!windowed) return dayGroups;
            return (
              <>
                {winStart > 0 ? (
                  <div aria-hidden="true" style={{ height: winStart * rowH }} />
                ) : null}
                <div ref={renderedRef}>{dayGroups}</div>
                {winEnd < items.length ? (
                  <div
                    aria-hidden="true"
                    style={{ height: (items.length - winEnd) * rowH }}
                  />
                ) : null}
              </>
            );
          })()}
        </div>
      )}
      {hasMore ? (
        <div className="mt-[var(--space-4)] flex justify-center">
          <Btn disabled={loadingMore} onClick={() => void onLoadMore()}>
            {loadingMore ? "Loading…" : `Load more (${items.length} of ${total})`}
          </Btn>
        </div>
      ) : null}
    </>
  );
}
