import {
  QuitConfirm,
  CloseConfirm,
  CloseAllConfirm,
  HtmlCloseConfirm,
  ConflictReload,
  OpenUrlDialog,
  AppErrorNotice,
} from "@rightkit/document-core/app/chrome";
import { CommandPalette, type PaletteItem } from "./CommandPalette.js";
import { ShortcutHelp } from "./ShortcutHelp.js";
import { SettingsPanel } from "../settings/SettingsPanel.js";
import { baseName } from "@rightkit/document-core/documents/types";
import type { LoadedDocument } from "@rightkit/document-core/documents/types";
import type { HtmlConfirmState, HtmlConflictState } from "./useHtmlSession.js";

// The modal/overlay cluster that used to live inline at the bottom of App.tsx: settings, command
// palette, shortcut help, the open-url dialog, the error notice, and every confirm/reload dialog.
// Each dialog owns its own visibility gate via the prop it's passed (mirrors the original inline
// `{cond && <Dialog .../>}` pattern) so behavior is identical to before the extraction.
export function AppDialogs({
  settingsOpen,
  onCloseSettings,
  onHistoryPurged,
  paletteOpen,
  paletteItems,
  onClosePalette,
  helpOpen,
  onCloseHelp,
  openUrlDialog,
  onOpenUrlSubmit,
  onCloseOpenUrlDialog,
  error,
  onCloseError,
  confirmClose,
  openDocs,
  titleFor,
  confirmSave,
  confirmDontSave,
  onCancelConfirmClose,
  confirmCloseAll,
  onCloseAllSaveAll,
  onCloseAllDontSave,
  onCloseAllCancel,
  confirmQuit,
  quitDirtyNames,
  onQuitNow,
  onQuitWithoutSaving,
  onQuitSaveAll,
  onCancelConfirmQuit,
  conflict,
  onReloadConflict,
  onKeepConflict,
  htmlConfirm,
  htmlDirtyPages,
  onHtmlSaveCurrent,
  onHtmlSaveAll,
  onHtmlDiscardAll,
  onHtmlConfirmCancel,
  htmlConflict,
  onHtmlConflictReload,
  onHtmlConflictKeep,
}: {
  settingsOpen: boolean;
  onCloseSettings: () => void;
  onHistoryPurged: () => void;
  paletteOpen: boolean;
  paletteItems: PaletteItem[];
  onClosePalette: () => void;
  helpOpen: boolean;
  onCloseHelp: () => void;
  openUrlDialog: boolean;
  onOpenUrlSubmit: (input: string) => void;
  onCloseOpenUrlDialog: () => void;
  error: string | null;
  onCloseError: () => void;
  confirmClose: string | null;
  openDocs: LoadedDocument[];
  titleFor: (doc: LoadedDocument) => string;
  confirmSave: () => void;
  confirmDontSave: () => void;
  onCancelConfirmClose: () => void;
  confirmCloseAll: { orderedPaths: string[]; affectedPaths: string[] } | null;
  onCloseAllSaveAll: () => void;
  onCloseAllDontSave: () => void;
  onCloseAllCancel: () => void;
  confirmQuit: boolean;
  quitDirtyNames: string[];
  onQuitNow: () => void;
  onQuitWithoutSaving: () => void;
  onQuitSaveAll: () => void;
  onCancelConfirmQuit: () => void;
  conflict: string | null;
  onReloadConflict: (path: string) => void;
  onKeepConflict: () => void;
  htmlConfirm: HtmlConfirmState;
  htmlDirtyPages: (entryPath: string) => Array<{ path: string; name: string }>;
  onHtmlSaveCurrent: () => void;
  onHtmlSaveAll: () => void;
  onHtmlDiscardAll: () => void;
  onHtmlConfirmCancel: () => void;
  htmlConflict: HtmlConflictState;
  onHtmlConflictReload: () => void;
  onHtmlConflictKeep: () => void;
}) {
  return (
    <>
      {settingsOpen && <SettingsPanel onClose={onCloseSettings} onHistoryPurged={onHistoryPurged} />}
      {paletteOpen && <CommandPalette items={paletteItems} onClose={onClosePalette} />}
      {helpOpen && <ShortcutHelp onClose={onCloseHelp} />}
      {openUrlDialog && <OpenUrlDialog onOpen={onOpenUrlSubmit} onCancel={onCloseOpenUrlDialog} />}
      {error && <AppErrorNotice message={error} onClose={onCloseError} />}
      {confirmClose && (
        <CloseConfirm
          title={titleFor(
            openDocs.find((d) => d.path === confirmClose) ?? {
              path: confirmClose,
              displayName: "",
              format: "txt",
              content: "",
            },
          )}
          onSave={confirmSave}
          onDontSave={confirmDontSave}
          onCancel={onCancelConfirmClose}
        />
      )}
      {confirmCloseAll && (
        <CloseAllConfirm
          dirtyNames={confirmCloseAll.affectedPaths.map(
            (p) =>
              titleFor(
                openDocs.find((d) => d.path === p) ?? {
                  path: p,
                  displayName: baseName(p),
                  format: "txt",
                  content: "",
                },
              ) || baseName(p),
          )}
          onSaveAll={onCloseAllSaveAll}
          onDontSave={onCloseAllDontSave}
          onCancel={onCloseAllCancel}
        />
      )}
      {confirmQuit && (
        <QuitConfirm
          dirtyNames={quitDirtyNames}
          onQuit={onQuitNow}
          onQuitWithoutSaving={onQuitWithoutSaving}
          onSaveAll={onQuitSaveAll}
          onCancel={onCancelConfirmQuit}
        />
      )}
      {conflict && (
        <ConflictReload
          name={baseName(conflict)}
          onReload={() => onReloadConflict(conflict)}
          onKeep={onKeepConflict}
        />
      )}
      {htmlConfirm && (
        <HtmlCloseConfirm
          title={baseName(htmlConfirm.entryPath)}
          mode={htmlConfirm.mode}
          dirtyNames={htmlDirtyPages(htmlConfirm.entryPath).map((d) => d.name)}
          onSaveCurrent={onHtmlSaveCurrent}
          onSaveAll={onHtmlSaveAll}
          onDiscardAll={onHtmlDiscardAll}
          onCancel={onHtmlConfirmCancel}
        />
      )}
      {htmlConflict && (
        <ConflictReload
          name={baseName(htmlConflict.pagePath)}
          onReload={onHtmlConflictReload}
          onKeep={onHtmlConflictKeep}
        />
      )}
    </>
  );
}
