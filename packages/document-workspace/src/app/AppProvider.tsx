import { createContext, useContext, type ReactNode } from "react";
import { capabilitiesFor } from "../documents/capabilities";
import { effectiveReadEdit } from "./shell/readEdit";
import { toast } from "@rightkit/platform-ui/headless";
import type { ReadEditMode } from "@rightkit/document-core/documents/types";
import { useAppDocumentCore } from "./useAppDocumentCore";
import { useAppChromeState } from "./useAppChromeState";
import { useAppShellSetup } from "./useAppShellSetup";
import { useAppPaneSearch } from "./useAppPaneSearch";
import { useAppTabChrome } from "./useAppTabChrome";
import { useAppCommands } from "./useAppCommands";
import { useAppSlots } from "./useAppSlots";
import { writeClipboardText } from "@rightkit/document-core/state/tauriBridge";
import { hostAppName } from "../appName";

/**
 * Hook composition for the whole app shell. Hook call order matches the original useAppState exactly:
 * document core → chrome state → shell setup → pane/search → tab chrome/zoom/lifecycle → commands &
 * shortcuts → slots. Plain (non-hook) derivations stay between the same hook calls they originally did.
 */
function useAppState() {
  const {
    blockDrafts,
    tabs,
    docBarSlot,
    setDocBarSlot,
    dirtyPaths,
    subscribeDirty,
    viewState,
    library,
    docs,
  } = useAppDocumentCore();
  const { openDocs, activePath, editsRef, subscribeEdits, getEditSnapshot, getExternalSeq } = tabs;
  const { views, contentScrollRef, requestViewChange: onChangeView } = viewState;
  const { recents, pinnedPaths, pinnedEntries, onTogglePin } = library;
  const { activate, open, openLiveUrl, openRecent, onOpenDialog, newDocument, onSave } = docs;
  const { isDirty, restoringSession } = docs;

  const {
    settingsOpen,
    setSettingsOpen,
    paletteOpen,
    setPaletteOpen,
    helpOpen,
    setHelpOpen,
    openUrlDialog,
    setOpenUrlDialog,
    tabOverviewOpen,
    setTabOverviewOpen,
    sidebarCollapsed,
    setSidebarCollapsed,
    inspectorOpen,
    toggleInspector,
    closeInspector,
  } = useAppChromeState();

  const {
    settings,
    update,
    handleOpenSettings,
    dragOver,
    rootDragProps,
    docxMenu,
    setDocxMenu,
    markdownExport,
    registerPdfPrint,
    registerMarkdownExport,
    handlePrint,
    docContextMenu,
    closeDocContextMenu,
    autoUpdateCheckRan,
    previewScrollStore,
    sourceViewStateRef,
  } = useAppShellSetup({ docs, activePath, setSettingsOpen });

  const active = openDocs.find((d) => d.path === activePath) ?? null;
  // Tab order == openDocs order (titlebar tabs use index-keyed ids for aria-controls/labelledby).
  const activeIndex = openDocs.findIndex((d) => d.path === activePath);
  const quitDirtyDocs = openDocs.filter((doc) => isDirty(doc));
  const view = active ? (views[active.path] ?? "preview") : "preview";

  const {
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
  } = useAppPaneSearch({
    docs,
    tabs,
    openDocs,
    activePath,
    active,
    blockDrafts,
    previewScrollStore,
    sourceViewStateRef,
  });

  // Read the live editsRef (not the debounced `edits` state) so the controlled editor + command-palette
  // text actions see the latest keystroke without App re-rendering on every keystroke.
  const activeText = active ? (editsRef.current[active.path] ?? active.content) : "";
  const activeEditable = active ? capabilitiesFor(active.format).canEdit : false;
  const activeReadEdit: ReadEditMode = active ? effectiveReadEdit(active) : "read";
  const activeTextLike = active ? capabilitiesFor(active.format).canReplace : false;
  // Whole-document copy — shared by the command palette, the bottom-bar button, and the document
  // context menu's "Copy contents". Null for non-text formats (pdf/image/docx binary surfaces).
  const copyContents =
    activeTextLike && active
      ? () =>
          void writeClipboardText(activeText)
            .then(() => toast("Document contents copied", "success"))
            .catch(() => toast("Couldn't copy to clipboard", "error"))
      : null;

  const {
    titleBarTitleFor,
    tabStripProps,
    openPathsSet,
    openExplorerPath,
    cycleTab,
    jumpToTab,
    zoom,
    openFromClipboard,
  } = useAppTabChrome({
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
  });
  const titleBarTitle = active ? titleBarTitleFor(active) : hostAppName();

  const { paletteItems, distractionFree, fullscreenConfirm } =
    useAppCommands({
      docs,
      openDocs,
      recents,
      active,
      activeEditable,
      activeTextLike,
      activeText,
      copyContents,
      settings,
      update,
      onChangeView,
      docxMenu,
      markdownExport,
      handlePrint,
      openFromClipboard,
      cycleTab,
      jumpToTab,
      blockDrafts,
      searchProvider,
      canOpenFind,
      findOpen,
      openFind,
      settingsOpen,
      setSettingsOpen,
      paletteOpen,
      setPaletteOpen,
      helpOpen,
      setHelpOpen,
      openUrlDialog,
      setOpenUrlDialog,
      setTabOverviewOpen,
    });

  const { onOpenPathForRouter, menuSlot, modeSlot } = useAppSlots({
    docs,
    active,
    view,
    onChangeView,
    handlePrint,
    docxMenu,
    activeReadEdit,
  });

  const openUrl = () => setOpenUrlDialog(true);
  return {
    dragOver,
    rootDragProps,
    distractionFree,
    activePath,
    openPathsSet,
    recents,
    pinnedPaths,
    pinnedEntries,
    activate,
    openRecent,
    openExplorerPath,
    onTogglePin,
    library,
    onOpenDialog,
    openUrl,
    openFromClipboard,
    newDocument,
    handleOpenSettings,
    setSidebarCollapsed,
    menuSlot,
    modeSlot,
    active,
    activeIndex,
    activeEditable,
    titleBarTitle,
    titleBarTitleFor,
    subscribeEdits,
    getEditSnapshot,
    getExternalSeq,
    sidebarCollapsed,
    isDirty,
    dirtyPaths,
    subscribeDirty,
    tabStripProps,
    view,
    docBarSlot,
    setDocBarSlot,
    inspectorOpen,
    toggleInspector,
    closeInspector,
    findBarEl,
    zoom,
    docs,
    restoringSession,
    openDocs,
    blockDrafts,
    livePaths,
    renderedPaths,
    views,
    contentScrollRef,
    viewState,
    onSave,
    setDocxMenu,
    registerPdfPrint,
    registerMarkdownExport,
    findMode,
    searchProvider,
    toggleFind,
    closeFind,
    onChangeView,
    registerSearchProviderFor,
    onOpenPathForRouter,
    htmlPropsFor,
    sourceViewStateRef,
    tabs,
    docxMenu,
    settings,
    update,
    docContextMenu,
    closeDocContextMenu,
    copyContents,
    paletteItems,
    settingsOpen,
    setSettingsOpen,
    paletteOpen,
    setPaletteOpen,
    helpOpen,
    setHelpOpen,
    openUrlDialog,
    setOpenUrlDialog,
    quitDirtyDocs,
    open,
    openLiveUrl,
    fullscreenConfirm,
  };
}

export type AppState = ReturnType<typeof useAppState>;

const AppContext = createContext<AppState | null>(null);

/** Read the composed app state inside AppProvider. */
export function useAppContext(): AppState {
  const app = useContext(AppContext);
  if (!app) throw new Error("useAppContext must be used inside AppProvider");
  return app;
}

/** Mounts the composed app state (all top-level hooks) for AppShell and its children. */
export function AppProvider({ children }: { children: ReactNode }) {
  const app = useAppState();
  return <AppContext.Provider value={app}>{children}</AppContext.Provider>;
}
