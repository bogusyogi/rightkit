import { useCallback, useMemo, useState } from "react";
import { createBlockDraftRegistry } from "@rightkit/document-core/app/blockDrafts";
import { useDocumentTabs } from "./useDocumentTabs";
import { useViewState } from "./useViewState";
import { useLibrary } from "./useLibrary";
import { useDocuments } from "./useDocuments";

/**
 * Document core: shared tab/edit state, view state, the recents/pins library, and the open/save/dirty/close
 * owner. Extracted verbatim from useAppState; these are the first hook calls of the app shell.
 */
export function useAppDocumentCore() {
  const [blockDrafts] = useState(() => createBlockDraftRegistry());
  const tabs = useDocumentTabs();
  // App's docked-bar centre slot; formats portal their tool capsule into it (shell/docBar).
  const [docBarSlot, setDocBarSlot] = useState<HTMLElement | null>(null);
  const { openDocs, activePath, subscribeEdits } = tabs;
  const dirtyPaths = useMemo(
    () => new Set([...tabs.docxDirty, ...tabs.pdfDirty, ...tabs.imageDirty]),
    [tabs.docxDirty, tabs.pdfDirty, tabs.imageDirty],
  );
  const subscribeDirty = useCallback(
    (path: string, listener: () => void) => {
      const unsubscribeEdits = subscribeEdits(path, listener);
      const unsubscribeDrafts = blockDrafts.subscribe(listener);
      return () => {
        unsubscribeEdits();
        unsubscribeDrafts();
      };
    },
    [blockDrafts, subscribeEdits],
  );
  const viewState = useViewState(activePath, openDocs, blockDrafts);
  const library = useLibrary(openDocs);
  const docs = useDocuments({ tabs, viewState, library, blockDrafts });
  return {
    blockDrafts,
    tabs,
    docBarSlot,
    setDocBarSlot,
    dirtyPaths,
    subscribeDirty,
    viewState,
    library,
    docs,
  };
}
