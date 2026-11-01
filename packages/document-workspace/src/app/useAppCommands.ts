import type { Dispatch, SetStateAction } from "react";
import { useCommandPalette } from "./useCommandPalette";
import { useDistractionFree } from "./useDistractionFree";
import { useGlobalShortcuts } from "./useGlobalShortcuts";
import { buildCopyAsMarkdown, buildCopyRendered, buildExportMarkdown } from "./markdownPaletteActions";
import type { LoadedDocument } from "@rightkit/document-core/documents/types";
import type { createBlockDraftRegistry } from "@rightkit/document-core/app/blockDrafts";
import type { useDocuments } from "./useDocuments";
import type { useFormatApis } from "./useFormatApis";
import type { useLibrary } from "./useLibrary";
import type { useOpenFromClipboard } from "./useOpenFromClipboard";
import type { useSettings } from "@rightkit/document-core/settings/settings";
import type { useViewState } from "./useViewState";
import type { useAppTabChrome } from "./useAppTabChrome";
import type { useAppPaneSearch } from "./useAppPaneSearch";

type Docs = ReturnType<typeof useDocuments>;
type Formats = ReturnType<typeof useFormatApis>;
type Setting = ReturnType<typeof useSettings>;
type Tab = ReturnType<typeof useAppTabChrome>;
type Pane = ReturnType<typeof useAppPaneSearch>;
type Setter = Dispatch<SetStateAction<boolean>>;

export interface AppCommandsInput {
  docs: Docs;
  openDocs: LoadedDocument[];
  recents: ReturnType<typeof useLibrary>["recents"];
  active: LoadedDocument | null;
  activeEditable: boolean;
  activeTextLike: boolean;
  activeText: string;
  copyContents: (() => void) | null;
  settings: Setting["settings"];
  update: Setting["update"];
  onChangeView: ReturnType<typeof useViewState>["requestViewChange"];
  docxMenu: Formats["docxMenu"];
  markdownExport: Formats["markdownExport"];
  handlePrint: Formats["handlePrint"];
  openFromClipboard: ReturnType<typeof useOpenFromClipboard>;
  cycleTab: Tab["cycleTab"];
  jumpToTab: Tab["jumpToTab"];
  blockDrafts: ReturnType<typeof createBlockDraftRegistry>;
  searchProvider: Pane["searchProvider"];
  canOpenFind: Pane["canOpenFind"];
  findOpen: Pane["findOpen"];
  openFind: Pane["openFind"];
  settingsOpen: boolean;
  setSettingsOpen: Setter;
  paletteOpen: boolean;
  setPaletteOpen: Setter;
  helpOpen: boolean;
  setHelpOpen: Setter;
  openUrlDialog: boolean;
  setOpenUrlDialog: Setter;
  setTabOverviewOpen: Setter;
}

/**
 * Command palette, distraction-free mode, and global keyboard shortcuts. Extracted verbatim from
 * useAppState; the three hooks run in the original order (palette → distraction-free → shortcuts).
 */
export function useAppCommands({
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
}: AppCommandsInput) {
  const paletteItems = useCommandPalette({
    openDocs,
    recents,
    active,
    activeEditable,
    titleFor: docs.titleFor,
    activate: docs.activate,
    openRecent: docs.openRecent,
    newDocument: docs.newDocument,
    onOpenDialog: docs.onOpenDialog,
    onOpenUrl: () => setOpenUrlDialog(true),
    onSave: docs.onSave,
    onSaveAs: docs.onSaveAs,
    onPrint: handlePrint,
    docxMenu,
    onChangeView,
    settings,
    update,
    openSettings: () => setSettingsOpen(true),
    reopenClosed: docs.reopenLastClosed,
    showHelp: () => setHelpOpen(true),
    copyContents,
    duplicate: activeTextLike && active ? () => docs.duplicateAsDraft(activeText) : null,
    exportMarkdown: buildExportMarkdown(active, markdownExport, activeText),
    copyRendered: buildCopyRendered(active, markdownExport, activeText),
    copyAsMarkdown: buildCopyAsMarkdown(active),
  });

  const { distractionFree, toggleDistractionFree, fullscreenConfirm } = useDistractionFree({
    active,
    blockDrafts,
    overlayOpen: settingsOpen || paletteOpen || helpOpen || openUrlDialog,
  });

  useGlobalShortcuts({
    active,
    activeEditable,
    searchProvider,
    canOpenFind,
    docxMenu,
    newDocument: docs.newDocument,
    onOpenDialog: docs.onOpenDialog,
    onPrint: handlePrint,
    onSave: docs.onSave,
    onSaveAs: docs.onSaveAs,
    requestClose: docs.requestClose,
    togglePalette: () => setPaletteOpen((o) => !o),
    openFind,
    onHtmlSaveShortcut: docs.onHtmlSaveShortcut,
    reloadActiveFromDisk: (doc: LoadedDocument) => docs.reload(doc.path),
    openFromClipboard,
    cycleTab,
    jumpToTab,
    reopenClosed: docs.reopenLastClosed,
    resetZoom: () => update({ fontSizePx: 16, sourceFontSizePx: 12 }),
    toggleWordWrap: () => update({ wordWrap: !settings.wordWrap }),
    toggleSidebar: () => window.dispatchEvent(new CustomEvent("viewright:toggle-sidebar")),
    toggleTabOverview: () => setTabOverviewOpen((o) => !o),
    showHelp: () => setHelpOpen(true),
    findOpen,
    toggleDistractionFree,
  });

  return { paletteItems, distractionFree, toggleDistractionFree, fullscreenConfirm };
}
