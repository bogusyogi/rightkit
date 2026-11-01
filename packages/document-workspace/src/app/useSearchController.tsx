import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { FindBar } from "./FindBar.js";
import type { LoadedDocument, SearchProvider } from "@rightkit/document-core/documents/types";
import { buildCrossDocProvider } from "./useCrossDocSearch.js";

/** Formats whose find surface registers asynchronously, so Find may open before its provider exists. */
const EARLY_FIND_FORMATS = new Set(["html", "docx", "md", "mdx"]);

export type SearchScope = "current" | "all";

/**
 * Shared find/replace controller. With keep-alive, several viewers can be mounted at once, so search
 * providers are registered PATH-SCOPED (each viewer registers its own path's provider on mount /
 * null on unmount); the LIVE provider that Find uses is the active tab's, re-selected on tab switch
 * (NOT on remount). Also owns find-bar open/replace state and the single toolbar FindBar element.
 */
export function useSearchController(
  activePath: string | null,
  active: LoadedDocument | null,
  openDocs?: LoadedDocument[],
  activate?: (path: string) => Promise<void>,
) {
  const [searchProvider, setSearchProvider] = useState<SearchProvider | null>(null);
  const [findState, setFindState] = useState({ open: false, replace: false });
  const [scope, setScope] = useState<SearchScope>("current");
  const providers = useRef<Map<string, SearchProvider>>(new Map());
  const activePathRef = useRef(activePath);
  activePathRef.current = activePath;
  const crossDocRef = useRef<SearchProvider | null>(null);
  // Latest state mirrors for callbacks that must keep a stable identity across a find session.
  const searchProviderRef = useRef<SearchProvider | null>(null);
  searchProviderRef.current = searchProvider;
  const findStateRef = useRef(findState);
  findStateRef.current = findState;

  // A viewer registers (provider) / unregisters (null) its OWN path. If it's the active tab, it also
  // becomes the live provider immediately.
  const registerSearchProvider = useCallback((path: string, provider: SearchProvider | null) => {
    if (provider) providers.current.set(path, provider);
    else providers.current.delete(path);
    if (path === activePathRef.current) setSearchProvider(provider);
  }, []);

  // On tab switch, swap to that tab's already-registered provider (warm panes don't remount).
  // In "all" scope, build a cross-doc provider aggregating all warm tabs. It is rebuilt only when
  // the set of open tabs changes: stepping to a match in another tab activates that tab, and
  // rebuilding then would reset the match position to the first tab.
  const openDocsRef = useRef(openDocs);
  openDocsRef.current = openDocs;
  const activateRef = useRef(activate);
  activateRef.current = activate;
  const crossDocScope = scope === "all" && !!openDocs && !!activate;
  const openPathsKey = openDocs?.map((doc) => doc.path).join("\0") ?? "";
  const followedPath = crossDocScope ? null : activePath;
  useEffect(() => {
    const docs = openDocsRef.current;
    if (crossDocScope && docs && openPathsKey && providers.current.size >= 2) {
      // Tear down previous cross-doc provider.
      crossDocRef.current?.close();
      const crossDoc = buildCrossDocProvider(providers.current, docs, (path) =>
        activateRef.current ? activateRef.current(path) : Promise.resolve(),
      );
      crossDocRef.current = crossDoc;
      setSearchProvider(crossDoc);
    } else {
      crossDocRef.current?.close();
      crossDocRef.current = null;
      setSearchProvider(providers.current.get(followedPath ?? activePathRef.current ?? "") ?? null);
    }
  }, [crossDocScope, openPathsKey, followedPath]);

  const openFind = useCallback((replace: boolean) => {
    setFindState({ open: true, replace });
  }, []);
  const openFindAll = useCallback(() => {
    setScope("all");
    setFindState({ open: true, replace: false });
  }, []);

  // Open even if the provider is still racing into place (HTML Safe render registers on iframe load;
  // Interactive registers a postMessage-driven preview provider; DOCX registers when its editor view
  // is ready; Markdown/MDX register once the ProseMirror view mounts). When the HTML source drawer is
  // open it registers a "source" provider instead.
  //
  // Every format that owns a find surface must be listed in EARLY_FIND_FORMATS. Markdown was
  // missing, so Cmd/Ctrl+F was a silent no-op on a .md tab until its editor happened to win the
  // race — the failure mode is invisible (no error, nothing opens), which is why it went unnoticed.
  const canOpenFind =
    !!searchProvider || (!!active?.format && EARLY_FIND_FORMATS.has(active.format));
  const canCrossDoc = !!openDocs && openDocs.length >= 2;
  // closeFind/toggleFind read live state through refs, so their identities stay stable for the
  // whole find session instead of changing on every provider or open/replace transition.
  const closeFind = useCallback(() => {
    setFindState((current) => ({ ...current, open: false }));
    crossDocRef.current?.close();
    crossDocRef.current = null;
    searchProviderRef.current?.close();
  }, []);
  const toggleFind = useCallback(
    (replace: boolean) => {
      const current = findStateRef.current;
      if (current.open && current.replace === replace) {
        closeFind();
        return;
      }
      setFindState({ open: true, replace });
    },
    [closeFind],
  );

  const findBarEl: ReactNode =
    active && findState.open && searchProvider ? (
      <FindBar
        provider={searchProvider}
        placement={active.format === "txt" ? "floating" : "toolbar"}
        initialReplaceMode={findState.replace}
        scope={scope}
        onScopeChange={setScope}
        canCrossDoc={canCrossDoc}
        onClose={closeFind}
      />
    ) : null;

  return {
    searchProvider,
    registerSearchProvider,
    openFind,
    closeFind,
    toggleFind,
    openFindAll,
    findBarEl,
    canOpenFind,
    findOpen: findState.open,
    findMode: findState.open
      ? findState.replace
        ? ("replace" as const)
        : ("find" as const)
      : undefined,
    scope,
    setScope,
  };
}
