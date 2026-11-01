import { useCallback, useRef, type Dispatch, type SetStateAction } from "react";
import { useAppEffects } from "./useAppEffects";
import { useFileDragOverlay } from "./useFileDragOverlay";
import { useFormatApis } from "./useFormatApis";
import { useDocumentContextMenu } from "./DocumentContextMenu";
import { useSettings } from "@rightkit/document-core/settings/settings";
import type { useDocuments } from "./useDocuments";
import type { useDocumentTabs } from "./useDocumentTabs";
import type { SourceEditorViewState } from "../documents/txt/TxtEditor";

type Docs = ReturnType<typeof useDocuments>;
type Tabs = ReturnType<typeof useDocumentTabs>;

export interface AppShellSetupInput {
  docs: Docs;
  activePath: Tabs["activePath"];
  setSettingsOpen: Dispatch<SetStateAction<boolean>>;
}

/**
 * Settings, drag/drop overlay, format APIs, the document context menu, app-level effects, and the
 * per-app refs (auto-update guard, preview scroll store, source view state). Extracted verbatim from
 * useAppState; hook call order is unchanged.
 */
export function useAppShellSetup({ docs, activePath, setSettingsOpen }: AppShellSetupInput) {
  const { settings, update } = useSettings();
  const handleOpenSettings = useCallback(() => setSettingsOpen(true), [setSettingsOpen]);
  useAppEffects({
    diagnosticsSetting: settings.diagnostics,
    activePath,
    docxSaver: docs.docxSaver,
    imageSaver: docs.imageSaver,
    docxSaversRef: docs.docxSaversRef,
    imageSaversRef: docs.imageSaversRef,
    openSettings: handleOpenSettings,
  });
  const { dragOver, rootDragProps } = useFileDragOverlay();
  const {
    docxMenu,
    setDocxMenu,
    markdownExport,
    registerPdfPrint,
    registerMarkdownExport,
    handlePrint,
  } = useFormatApis(docs.onPrint);
  const { menu: docContextMenu, close: closeDocContextMenu } = useDocumentContextMenu();
  const autoUpdateCheckRan = useRef(false);
  // Durable interactive-HTML preview scroll, keyed by the html tab's entry path. Lives here (not in
  // HtmlDocument) so it survives the unmount on tab switch and is restored when you return.
  const previewScrollStore = useRef<Map<string, number>>(new Map());
  // Source selection + scroll are view state, but must survive an eligible dirty-router eviction just
  // like the source itself. One path-keyed owner covers TXT, Markdown's source drawer, and HTML pages.
  const sourceViewStateRef = useRef<Record<string, SourceEditorViewState>>({});
  return {
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
  };
}
