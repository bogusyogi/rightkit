import type { ComponentProps } from "react";
import { AppDialogs } from "./AppDialogs.js";
import { BlockDraftViewConfirm } from "@rightkit/document-core/app/chrome";
import { baseName, isUntitled } from "@rightkit/document-core/documents/types";
import type { LoadedDocument } from "@rightkit/document-core/documents/types";
import { addRecentUrl } from "@rightkit/document-core/state/recentUrls";
import { openLinkedDocument } from "@rightkit/document-core/state/tauriBridge";
import { localPathFromClipboardText } from "./clipboardPaths.js";
import type { useDocuments } from "./useDocuments.js";

type DialogProps = ComponentProps<typeof AppDialogs>;
type ConfirmProps = ComponentProps<typeof BlockDraftViewConfirm>;

/** Wires useDocuments' confirm/conflict state + App's overlay flags into AppDialogs, and mounts the
 * block-draft view/fullscreen confirms. Extracted verbatim from App.tsx's JSX tail. */
export function AppDialogsHost({
  docs,
  openDocs,
  quitDirtyDocs,
  paletteItems,
  overlays,
  onHistoryPurged,
  open,
  openLiveUrl,
  viewConfirm,
  fullscreenConfirm,
}: {
  docs: ReturnType<typeof useDocuments>;
  openDocs: LoadedDocument[];
  quitDirtyDocs: LoadedDocument[];
  paletteItems: DialogProps["paletteItems"];
  overlays: {
    settingsOpen: boolean;
    setSettingsOpen: (v: boolean) => void;
    paletteOpen: boolean;
    setPaletteOpen: (v: boolean) => void;
    helpOpen: boolean;
    setHelpOpen: (v: boolean) => void;
    openUrlDialog: boolean;
    setOpenUrlDialog: (v: boolean) => void;
  };
  onHistoryPurged: DialogProps["onHistoryPurged"];
  open: (path: string) => unknown;
  openLiveUrl: (url: string) => unknown;
  viewConfirm: ConfirmProps | null;
  fullscreenConfirm: ConfirmProps | null;
}) {
  const {
    error,
    setError,
    confirmClose,
    setConfirmClose,
    titleFor,
    confirmSave,
    confirmDontSave,
    confirmCloseAll,
    confirmCloseAllSaveAll,
    confirmCloseAllDontSave,
    confirmCloseAllCancel,
    confirmQuit,
    setConfirmQuit,
    quitNow,
    conflict,
    setConflict,
    keepConflict,
    applyReload,
  } = docs;
  return (
    <>
      <AppDialogs
        settingsOpen={overlays.settingsOpen}
        onCloseSettings={() => overlays.setSettingsOpen(false)}
        onHistoryPurged={onHistoryPurged}
        paletteOpen={overlays.paletteOpen}
        paletteItems={paletteItems}
        onClosePalette={() => overlays.setPaletteOpen(false)}
        helpOpen={overlays.helpOpen}
        onCloseHelp={() => overlays.setHelpOpen(false)}
        openUrlDialog={overlays.openUrlDialog}
        onOpenUrlSubmit={(input) => {
          overlays.setOpenUrlDialog(false);
          const localPath = localPathFromClipboardText(input);
          if (localPath) {
            void openLinkedDocument(localPath).then((readablePath) =>
              open(readablePath ?? localPath),
            );
            return;
          }
          addRecentUrl(input);
          void openLiveUrl(input);
        }}
        onCloseOpenUrlDialog={() => overlays.setOpenUrlDialog(false)}
        error={error}
        onCloseError={() => setError(null)}
        confirmClose={confirmClose}
        openDocs={openDocs}
        titleFor={titleFor}
        confirmSave={confirmSave}
        confirmDontSave={confirmDontSave}
        onCancelConfirmClose={() => setConfirmClose(null)}
        confirmCloseAll={confirmCloseAll}
        onCloseAllSaveAll={() => void confirmCloseAllSaveAll()}
        onCloseAllDontSave={() => void confirmCloseAllDontSave()}
        onCloseAllCancel={confirmCloseAllCancel}
        confirmQuit={confirmQuit}
        quitDirtyNames={quitDirtyDocs.map((doc) =>
          isUntitled(doc.path) ? titleFor(doc) : baseName(doc.path),
        )}
        onQuitNow={quitNow}
        onQuitWithoutSaving={() => void docs.quitDiscardAll()}
        onQuitSaveAll={() => void docs.quitSaveAll()}
        onCancelConfirmQuit={() => {
          setConfirmQuit(false);
        }}
        conflict={conflict}
        onReloadConflict={(path) => {
          applyReload(path);
          setConflict(null);
        }}
        onKeepConflict={() => {
          if (conflict) void keepConflict(conflict);
        }}
        htmlConfirm={docs.htmlConfirm}
        htmlDirtyPages={docs.htmlDirtyPages}
        onHtmlSaveCurrent={docs.htmlSaveCurrent}
        onHtmlSaveAll={docs.htmlSaveAll}
        onHtmlDiscardAll={docs.htmlDiscardAll}
        onHtmlConfirmCancel={docs.htmlConfirmCancel}
        htmlConflict={docs.htmlConflict}
        onHtmlConflictReload={docs.htmlConflictReload}
        onHtmlConflictKeep={docs.htmlConflictKeep}
      />
      {viewConfirm && <BlockDraftViewConfirm {...viewConfirm} />}
      {fullscreenConfirm && <BlockDraftViewConfirm {...fullscreenConfirm} />}
    </>
  );
}
