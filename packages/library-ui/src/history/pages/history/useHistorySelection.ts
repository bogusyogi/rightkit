import { useCallback, useEffect, useMemo, useState } from "react";
import type { HistoryItem } from "@rightkit/dictation-ui/lib/homeMetrics";
import { isLocallyDeletable, type SourceSelection } from "@rightkit/dictation-ui/pages/history/historyUtils";

/**
 * HR-HISTORY-009: owns History's selection mode — the `isSelecting` toggle,
 * the `selectedIds` set behind Export selected / Delete selected, and the
 * derived deletable/Select-all counts. Only locally-owned (Mac-origin) rows
 * are ever added to `selectedIds`; foreign rows have no selection affordance
 * in the UI (HistoryRow renders a lock glyph instead of a checkbox for them).
 * Mutation itself (delete/export commands and their confirmations) stays
 * with the page — this hook owns only the selection state machine.
 */
export function useHistorySelection({
  visibleItems,
  sourceSelection,
}: {
  visibleItems: HistoryItem[];
  sourceSelection: SourceSelection;
}) {
  const [isSelecting, setIsSelecting] = useState(false);
  const [selectedIds, setSelectedIds] = useState<Set<string>>(new Set());

  // Reset bulk-selection state whenever titlebar source visibility changes.
  useEffect(() => {
    void sourceSelection;
    setIsSelecting(false);
    setSelectedIds(new Set());
  }, [sourceSelection]);

  // Informational only (the exact "Delete all" set is recomputed via a full
  // page walk in the page's requestDeleteAll) — used to disable the control
  // when nothing currently loaded is deletable, and to drive Select-all.
  const deletableVisibleIds = useMemo(
    () => visibleItems.filter(isLocallyDeletable).map((item) => item.delivery_id),
    [visibleItems],
  );

  const deletableCount = deletableVisibleIds.length;

  const allVisibleSelected =
    deletableVisibleIds.length > 0 && deletableVisibleIds.every((id) => selectedIds.has(id));

  const toggleSelectAll = useCallback(() => {
    setSelectedIds((current) => {
      if (allVisibleSelected) {
        const next = new Set(current);
        for (const id of deletableVisibleIds) next.delete(id);
        return next;
      }
      return new Set([...current, ...deletableVisibleIds]);
    });
  }, [allVisibleSelected, deletableVisibleIds]);

  const toggleSelecting = useCallback(() => {
    setIsSelecting((current) => !current);
    setSelectedIds(new Set());
  }, []);

  const toggleSelect = useCallback((deliveryId: string) => {
    setSelectedIds((current) => {
      const next = new Set(current);
      if (next.has(deliveryId)) next.delete(deliveryId);
      else next.add(deliveryId);
      return next;
    });
  }, []);

  /** Drop the whole selection and leave selection mode — used after a
   *  successful bulk delete/export. */
  const clearSelection = useCallback(() => {
    setSelectedIds(new Set());
    setIsSelecting(false);
  }, []);

  return {
    isSelecting,
    selectedIds,
    deletableCount,
    allVisibleSelected,
    toggleSelectAll,
    toggleSelecting,
    toggleSelect,
    clearSelection,
  };
}
