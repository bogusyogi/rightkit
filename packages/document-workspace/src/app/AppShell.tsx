import { AppFrame, Plane } from "@rightkit/react-shell/react";
import { Sidebar } from "./Sidebar";
import { Toolbar } from "./Toolbar";
import { DocumentContextMenu } from "./DocumentContextMenu";
import { ToastViewport } from "./ToastViewport";
import { AppDialogsHost } from "./AppDialogsHost";
import { DocumentPane } from "./DocumentPane";
import { DropOverlay } from "./useFileDragOverlay";
import { useAppContext } from "./AppProvider";

/** Layout shell: renders the sidebar, toolbar, document pane, and dialogs from the composed app state. */
export function AppShell() {
  const {
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
  fullscreenConfirm
  } = useAppContext();
  return (
    <AppFrame>
      <div
        className="vr-app-root flex h-full w-full bg-[var(--chrome)] text-paper"
        {...rootDragProps}
      >
        {dragOver && <DropOverlay />}
        {/* Keyboard/screen-reader users can jump straight past the sidebar + toolbars to the document. */}
        <a href="#vr-doc-pane" className="vr-skip-link">
          Skip to content
        </a>
        {!distractionFree && (
          <Sidebar
            activePath={activePath}
            openPaths={openPathsSet}
            recents={recents}
            pinnedPaths={pinnedPaths}
            pinnedEntries={pinnedEntries}
            onActivate={activate}
            onOpenRecent={openRecent}
            onOpenPath={openExplorerPath}
            onTogglePin={onTogglePin}
            onTogglePinDir={library.onTogglePinDir}
            onClearRecents={library.onClearRecents}
            onOpenDialog={onOpenDialog}
            onOpenUrl={openUrl}
            onOpenClipboard={openFromClipboard}
            onNew={newDocument}
            onOpenSettings={handleOpenSettings}
            onCollapsedChange={setSidebarCollapsed}
            titlebarSlot={menuSlot}
          />
        )}
        <Plane className="min-w-0 flex-1 overflow-visible">
          <main className="flex min-h-0 min-w-0 flex-1 flex-col">
            {!distractionFree && (
              <Toolbar
                doc={active}
                title={titleBarTitle}
                compute={titleBarTitleFor}
                subscribeEdits={subscribeEdits}
                getEditSnapshot={getEditSnapshot}
                sidebarCollapsed={sidebarCollapsed}
                menuSlot={menuSlot}
                isDirty={isDirty}
                dirtyPaths={dirtyPaths}
                subscribeDirty={subscribeDirty}
                tabs={tabStripProps}
              />
            )}
            {/* The Info inspector is a flex SIBLING of the content plane — the plane shrinks so the
                panel never overlaps the floating format bar or find bar inside it. */}
            <DocumentPane
              active={active}
              activeIndex={activeIndex}
              activeEditable={activeEditable}
              view={view}
              distractionFree={distractionFree}
              docBarSlot={docBarSlot}
              setDocBarSlot={setDocBarSlot}
              modeSlot={modeSlot}
              inspectorOpen={inspectorOpen}
              toggleInspector={toggleInspector}
              closeInspector={closeInspector}
              findBarEl={findBarEl}
              zoom={zoom}
              openingPath={docs.openingPath}
              restoringSession={restoringSession}
              openDocs={openDocs}
              startPage={{
                onNew: newDocument,
                onOpenDialog,
                onOpenUrl: openUrl,
                onOpenClipboard: openFromClipboard,
                recents,
                onOpenRecent: openRecent,
              }}
              blockDrafts={blockDrafts}
              router={{
                mountedDocs: openDocs,
                activePath,
                livePaths,
                renderedPaths,
                views,
                contentScrollRef,
                onContentScroll: viewState.onContentScroll,
                onDocumentWheel: zoom.onDocumentWheel,
                subscribeEdits,
                getEditSnapshot,
                getExternalSeq,
                sourceViewStateRef,
                onTxtChange: docs.onTxtChange,
                onSave,
                onDocxSaveAs: docs.onDocxSaveAs,
                onDocxSave: docs.onDocxSave,
                onDocxChange: docs.onDocxChange,
                docxSaver: docs.docxSaver,
                imageSaver: docs.imageSaver,
                docxSaversRef: docs.docxSaversRef,
                pdfSaversRef: docs.pdfSaversRef,
                pdfSavePlansRef: docs.pdfSavePlansRef,
                imageSaversRef: docs.imageSaversRef,
                setDocxMenu,
                setPdfPrint: registerPdfPrint,
                setMarkdownExport: registerMarkdownExport,
                onImageChange: docs.onImageChange,
                onPdfDirtyChange: docs.onPdfDirtyChange,
                openImageSibling: docs.openImageSibling,
                registerSearchProviderFor,
                onPdfViewState: viewState.onPdfViewState,
                findBarEl: null,
                findMode,
                canReplaceSearch: !!searchProvider?.canReplace,
                onChangeView,
                toggleFind,
                closeFind,
                onOpenPath: onOpenPathForRouter,
                trackMdxGrant: docs.trackMdxGrant,
                htmlPropsFor,
              }}
              subscribeEdits={subscribeEdits}
              getEditSnapshot={getEditSnapshot}
              dirtyPaths={dirtyPaths}
              isDirty={isDirty}
              subscribeDirty={subscribeDirty}
              docxMenu={docxMenu}
              onHtmlSave={docs.onHtmlSaveShortcut}
              onSave={onSave}
              settings={settings}
              update={update}
            />
          </main>
        </Plane>
        {docContextMenu && (
          <DocumentContextMenu
            menu={docContextMenu}
            onClose={closeDocContextMenu}
            onCopyContents={copyContents}
          />
        )}
        <ToastViewport />
        <AppDialogsHost
          docs={docs}
          openDocs={openDocs}
          quitDirtyDocs={quitDirtyDocs}
          paletteItems={paletteItems}
          overlays={{
            settingsOpen,
            setSettingsOpen,
            paletteOpen,
            setPaletteOpen,
            helpOpen,
            setHelpOpen,
            openUrlDialog,
            setOpenUrlDialog,
          }}
          onHistoryPurged={library.onHistoryPurged}
          open={open}
          openLiveUrl={openLiveUrl}
          viewConfirm={
            viewState.pendingViewChange
              ? {
                  onDone: () => void viewState.confirmViewChange("done"),
                  onDiscard: () => void viewState.confirmViewChange("discard"),
                  onStay: () => void viewState.confirmViewChange("stay"),
                  error:
                    viewState.pendingViewChange.drafts.find((d) => d.error)?.error ??
                    viewState.pendingViewChange.drafts.find((d) => d.reason)?.reason ??
                    null,
                }
              : null
          }
          fullscreenConfirm={fullscreenConfirm}
        />
      </div>
    </AppFrame>
  );
}
