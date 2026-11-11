import { useCallback, useSyncExternalStore } from "react";

/**
 * Subscribe a component to ONE document's live unsaved-edit text from the edits store (useDocumentTabs).
 * Re-renders only this component when that path's edits change — App and the rest of the chrome stay put.
 * Consumers: the bottom bar (word-count/dirty) and each tab's live title (LiveTitle).
 */
export function useEditText(
  subscribeEdits: (path: string, cb: () => void) => () => void,
  getEditSnapshot: (path: string) => string | undefined,
  path: string,
): string | undefined {
  const subscribe = useCallback(
    (cb: () => void) => subscribeEdits(path, cb),
    [subscribeEdits, path],
  );
  return useSyncExternalStore(subscribe, () => getEditSnapshot(path));
}
