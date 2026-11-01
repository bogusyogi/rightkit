import { useCallback, useEffect, useState } from "react";
import { usePlatform } from "./usePlatform";
import {
  DEFAULT_SOURCE_SELECTION,
  loadSourceSelection,
  saveSourceSelection,
  type SourceSelection,
} from "../pages/history/historyUtils";

const SOURCE_SELECTION_CHANGED = "heardright-history-source-selection-changed";

/** Windows has no Apple sync: every row is this PC's, and no stored choice may hide them. */
const WINDOWS_SOURCE_SELECTION: SourceSelection = { ...DEFAULT_SOURCE_SELECTION };

/** Shared source filter for History body + titlebar controls. */
export function useHistorySourceSelection() {
  const windows = usePlatform() === "windows";
  const [storedSelection, setSourceSelectionState] = useState<SourceSelection>(() =>
    loadSourceSelection(),
  );
  const sourceSelection = windows ? WINDOWS_SOURCE_SELECTION : storedSelection;

  useEffect(() => {
    const syncSelection = (event: Event) => {
      const next = (event as CustomEvent<SourceSelection>).detail;
      setSourceSelectionState(next ?? loadSourceSelection());
    };
    window.addEventListener(SOURCE_SELECTION_CHANGED, syncSelection);
    return () => window.removeEventListener(SOURCE_SELECTION_CHANGED, syncSelection);
  }, []);

  const setSourceSelection = useCallback(
    (next: SourceSelection) => {
      if (windows) return;
      saveSourceSelection(next);
      setSourceSelectionState(next);
      window.dispatchEvent(new CustomEvent(SOURCE_SELECTION_CHANGED, { detail: next }));
    },
    [windows],
  );

  return { sourceSelection, setSourceSelection };
}
