import { useCallback, useMemo, type Dispatch, type SetStateAction } from "react";
import { useTabChrome } from "./useTabChrome.js";
import { useDocumentZoom } from "./useDocumentZoom.js";
import { useAppLifecycle } from "./AppLifecycle.js";
import { useOpenFromClipboard } from "./useOpenFromClipboard.js";
import { baseName, isUntitled } from "@rightkit/document-core/documents/types";
import type { LoadedDocument } from "@rightkit/document-core/documents/types";
import type { useDocuments } from "./useDocuments.js";
import type { useDocumentTabs } from "./useDocumentTabs.js";
import type { useAppDocumentCore } from "./useAppDocumentCore.js";
import type { useAppShellSetup } from "./useAppShellSetup.js";
import type { useLibrary } from "./useLibrary.js";

type Docs = ReturnType<typeof useDocuments>;
type Tabs = ReturnType<typeof useDocumentTabs>;
type Core = ReturnType<typeof useAppDocumentCore>;
type Setup = ReturnType<typeof useAppShellSetup>;
type Library = ReturnType<typeof useLibrary>;

export interface AppTabChromeInput {
  docs: Docs;
  tabs: Tabs;
  activePath: Tabs["activePath"];
  sidebarCollapsed: boolean;
  tabOverviewOpen: boolean;
  setTabOverviewOpen: Dispatch<SetStateAction<boolean>>;
  pinnedPaths: Library["pinnedPaths"];
  onTogglePin: Library["onTogglePin"];
  dirtyPaths: Core["dirtyPaths"];
  subscribeDirty: Core["subscribeDirty"];
  active: LoadedDocument | null;
  view: Parameters<typeof useDocumentZoom>[0]["view"];
  settings: Setup["settings"];
  update: Setup["update"];
  autoUpdateCheckRan: Setup["autoUpdateCheckRan"];
}

/**
 * Titlebar title, tab strip, document zoom, the open-file watch set, the lifecycle effects, and the
 * open-from-clipboard action. Extracted verbatim from useAppState; hook call order is unchanged.
 */
export function useAppTabChrome({
  docs,
  tabs,
  activePath,
  sidebarCollapsed,
  tabOverviewOpen,
  setTabOverviewOpen,
  pinnedPaths,
  onTogglePin,
  dirtyPaths,
  subscribeDirty,
  active,
  view,
  settings,
  update,
  autoUpdateCheckRan,
}: AppTabChromeInput) {
  const { titleFor } = docs;
  // Title bar carries the extension (e.g. "notes.md"); sidebar tabs stay stripped (the badge shows extension).
  // As a function so the Toolbar's LiveTitle can recompute it live while typing an untitled doc.
  const titleBarTitleFor = useCallback(
    (d: LoadedDocument) =>
      d.format === "web" || isUntitled(d.path) ? titleFor(d) : baseName(d.path),
    [titleFor],
  );
  const { tabStripProps, openPathsSet, openExplorerPath, cycleTab, jumpToTab } = useTabChrome({
    tabs,
    docs,
    activePath,
    sidebarCollapsed,
    tabOverviewOpen,
    setTabOverviewOpen,
    pinnedPaths,
    onTogglePin,
    titleBarTitleFor,
    dirtyPaths,
    subscribeDirty,
  });

  // Ctrl/Cmd+wheel document zoom + the zoom HUD/reset pill — extracted to useDocumentZoom.
  const zoom = useDocumentZoom({ active, view, settings, update });

  // Watch EVERY open file-backed tab plus each HTML session's visited sub-pages, in the one watcher.
  // An external edit to ANY of them (active or background) arrives via the `file-changed` event and
  // reloads that tab when it happens — so tab activation is a pure state change (no re-stat /
  // reload-on-switch) and a background tab is already fresh by the time you click it. Keyed on the
  // sorted path set so it re-arms only when a tab opens/closes/navigates, not on every keystroke.
  const watchKey = useMemo(() => {
    const files = tabs.openDocs
      .filter((d) => d.format !== "web" && !isUntitled(d.path))
      .map((d) => d.path);
    const subPages = Object.values(tabs.htmlPages).flatMap((session) => Object.keys(session));
    return [...new Set([...files, ...subPages])].sort().join("|");
  }, [tabs.openDocs, tabs.htmlPages]);

  // Ready-show, file watcher re-arm, background update check, and the "open URL as a tab" wiring.
  useAppLifecycle({
    watchKey,
    autoCheckUpdates: settings.autoCheckUpdates,
    autoUpdateCheckRan,
    openLiveUrl: docs.openLiveUrl,
  });

  const openFromClipboard = useOpenFromClipboard({
    open: docs.open,
    openLiveUrl: docs.openLiveUrl,
    duplicateAsDraft: docs.duplicateAsDraft,
    setError: docs.setError,
  });

  return {
    titleBarTitleFor,
    tabStripProps,
    openPathsSet,
    openExplorerPath,
    cycleTab,
    jumpToTab,
    zoom,
    openFromClipboard,
  };
}
