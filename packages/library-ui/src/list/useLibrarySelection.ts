import { useCallback, useEffect, useMemo, useState } from "react";
import type { LibraryRow } from "../contract.js";
import type { GroupSelection } from "./useGroupSelection.js";

/**
 * Owns the list's selection mode: the `isSelecting` toggle, the `selectedIds`
 * set behind Export selected / Delete selected, and the derived
 * deletable/Select-all counts. Read-only rows are never added to
 * `selectedIds` (the row renders a lock glyph instead of a checkbox).
 * Mutation itself (delete/export and their confirmations) stays with the
 * screen; this hook owns only the selection state machine.
 */
export function useLibrarySelection({
  visibleRows,
  groupSelection,
}: {
  visibleRows: LibraryRow[];
  groupSelection: GroupSelection;
}) {
  const [isSelecting, setIsSelecting] = useState(false);
  const [selectedIds, setSelectedIds] = useState<Set<string>>(new Set());

  // Reset bulk-selection state whenever titlebar source visibility changes.
  useEffect(() => {
    void groupSelection;
    setIsSelecting(false);
    setSelectedIds(new Set());
  }, [groupSelection]);

  // Informational only (the exact "Delete all" set is recomputed via a full
  // page walk in the page's requestDeleteAll) — used to disable the control
  // when nothing currently loaded is deletable, and to drive Select-all.
  const deletableVisibleIds = useMemo(
    () => visibleRows.filter((row) => !row.readOnly).map((row) => row.id),
    [visibleRows],
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

  const toggleSelect = useCallback((id: string) => {
    setSelectedIds((current) => {
      const next = new Set(current);
      if (next.has(id)) next.delete(id);
      else next.add(id);
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
