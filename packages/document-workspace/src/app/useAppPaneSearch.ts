import { useCallback, useRef } from "react";
import { usePaneLiveness } from "./usePaneLiveness";
import { useSearchController } from "./useSearchController";
import { useHtmlSessionProps } from "./useHtmlSessionProps";
import { useClosedTabEviction } from "./useClosedTabEviction";
import type { LoadedDocument, SearchProvider } from "@rightkit/document-core/documents/types";
import type { createBlockDraftRegistry } from "@rightkit/document-core/app/blockDrafts";
import type { useDocuments } from "./useDocuments";
import type { useDocumentTabs } from "./useDocumentTabs";
import type { useAppShellSetup } from "./useAppShellSetup";

type Docs = ReturnType<typeof useDocuments>;
type Tabs = ReturnType<typeof useDocumentTabs>;
type Setup = ReturnType<typeof useAppShellSetup>;

export interface AppPaneSearchInput {
  docs: Docs;
  tabs: Tabs;
  openDocs: LoadedDocument[];
  activePath: Tabs["activePath"];
  active: LoadedDocument | null;
  blockDrafts: ReturnType<typeof createBlockDraftRegistry>;
  previewScrollStore: Setup["previewScrollStore"];
  sourceViewStateRef: Setup["sourceViewStateRef"];
}

/**
 * Warm-MRU pane liveness, search controller, stable per-path search registration, HTML session props,
 * and closed-tab eviction. Extracted verbatim from useAppState; hook call order is unchanged.
 */
export function useAppPaneSearch({
  docs,
  tabs,
  openDocs,
  activePath,
  active,
  blockDrafts,
  previewScrollStore,
  sourceViewStateRef,
}: AppPaneSearchInput) {
  // Warm-MRU pane liveness (see usePaneLiveness); `pruneClosedPaths` drops a closed tab's markers.
  const { livePaths, renderedPaths, pruneClosedPaths } = usePaneLiveness({
    openDocs,
    activePath,
    isDirty: docs.isDirty,
    pendingSavePaths: docs.pendingSavePaths,
    restoringSession: docs.restoringSession,
    blockDrafts,
  });
  const {
    searchProvider,
    registerSearchProvider,
    openFind,
    toggleFind,
    closeFind,
    findBarEl,
    canOpenFind,
    findOpen,
    findMode,
  } = useSearchController(activePath, active, openDocs, docs.activate);
  // Per-path, STABLE binding of the controller's `registerSearchProvider`. Each doc pane is rendered
  // inside a `.map`, so an inline `(p) => registerSearchProvider(doc.path, p)` would be a fresh
  // function every render — and viewers (MarkdownView/MarkdownSplitPane, HTML, PDF) list their register
  // callback in effect deps, so an unstable identity re-fires those effects every render, each minting a
  // new provider object → setSearchProvider → re-render → infinite "Maximum update depth" loop. Caching
  // one bound fn per path keeps the identity stable (the controller's registerSearchProvider already is).
  const boundRegisterRef = useRef<Map<string, (provider: SearchProvider | null) => void>>(
    new Map(),
  );
  const registerSearchProviderFor = useCallback(
    (path: string) => {
      const cache = boundRegisterRef.current;
      let fn = cache.get(path);
      if (!fn) {
        fn = (provider) => registerSearchProvider(path, provider);
        cache.set(path, fn);
      }
      return fn;
    },
    [registerSearchProvider],
  );
  // HTML site-session props for the router — extracted to useHtmlSessionProps. dropHtmlProps is
  // what the shared closed-tab eviction effect below calls per closed path.
  const { htmlPropsFor, dropHtmlProps } = useHtmlSessionProps({
    htmlPages: tabs.htmlPages,
    previewScrollStore,
    ensureHtmlPage: docs.ensureHtmlPage,
    setHtmlPageEdited: docs.setHtmlPageEdited,
    setHtmlActive: docs.setHtmlActive,
    saveHtmlPage: docs.saveHtmlPage,
    discardHtmlPage: docs.discardHtmlPage,
    htmlDirtyPages: docs.htmlDirtyPages,
    reload: docs.reload,
  });
  const evictBoundRegister = useCallback((path: string) => {
    boundRegisterRef.current.delete(path);
  }, []);
  useClosedTabEviction({
    openDocs,
    onEvictPath: evictBoundRegister,
    dropHtmlProps,
    sourceViewStateRef,
    pruneClosedPaths,
  });
  return {
    livePaths,
    renderedPaths,
    searchProvider,
    openFind,
    toggleFind,
    closeFind,
    findBarEl,
    canOpenFind,
    findOpen,
    findMode,
    registerSearchProviderFor,
    htmlPropsFor,
  };
}
