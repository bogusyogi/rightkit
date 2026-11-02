import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type Dispatch,
  type MutableRefObject,
  type SetStateAction,
} from "react";
import {
  isUntitled,
  type LoadedDocument,
  type ViewMeta,
  type ViewMode,
} from "@rightkit/document-core/documents/types";
import { isWebTabKey } from "@rightkit/document-core/documents/web/webUrl";
import {
  readDocument,
  revokePath,
} from "@rightkit/document-core/state/tauriBridge";
import type { DocumentTabs } from "./useDocumentTabs.js";
import {
  clearFileDraft,
  flushDraft,
} from "./documentDrafts.js";
import { errorMessage } from "@rightkit/document-core/state/errors";
import { useDocumentRuntimeEffects } from "./useDocumentRuntimeEffects.js";
import { useFormatDirtyState } from "./useFormatDirtyState.js";
import { useHtmlSession, type HtmlConfirmState, type HtmlConflictState } from "./useHtmlSession.js";
import { useDocumentLoader } from "./useDocumentLoader.js";
import { useDocumentSaver } from "./useDocumentSaver.js";
import { useDocumentWatcher, pickNextActive } from "./useDocumentWatcher.js";
import { useSettings } from "@rightkit/document-core/settings/settings";
import { createBlockDraftRegistry } from "@rightkit/document-core/app/blockDrafts";
import { useAutoSave } from "./useAutoSave.js";
import { useCloseAll } from "./useCloseAll.js";
import { useDraftConfirmGate } from "./useDraftConfirmGate.js";
import { usePendingSavePaths } from "./usePendingSavePaths.js";
import { useSetupRecovery } from "./useSetupRecovery.js";
import { useDocumentDirty } from "./useDocumentDirty.js";
import { useDocumentCreation } from "./useDocumentCreation.js";
import { useDocumentOutputActions } from "./useDocumentOutputActions.js";

const EMPTY_PENDING_SAVE_PATHS: ReadonlySet<string> = new Set();

// Re-exported so the existing unit test (useDocuments.pickNextActive.test.ts) keeps importing this
// module without a signature change; the implementation now lives in useDocumentWatcher.ts.
export { pickNextActive };

interface ViewStateDeps {
  setViews: Dispatch<SetStateAction<Record<string, ViewMode>>>;
  viewsRef: MutableRefObject<Record<string, ViewMode>>;
  viewMetaRef: MutableRefObject<Record<string, ViewMeta>>;
  dropViewState: (path: string) => void;
}

interface LibraryDeps {
  refreshLibrary: () => Promise<void>;
  syncPinned: (path: string) => Promise<void>;
}

/**
 * The document lifecycle orchestrator: composes useDocumentLoader (open/read), useDocumentSaver
 * (save/save-as), and useDocumentWatcher (close/teardown/reopen + external-change reload) around the
 * shared `tabs` state and view-state/library setters (passed in to avoid a hook-ordering cycle).
 * Bodies are unchanged from when everything lived in one 862-line hook — this file now just wires the
 * pieces together and owns the state/refs genuinely shared across all three (fingerprints, the
 * open-docs/activation-order mirrors, and the dialog state the HTML session drives).
 */
export function useDocuments(deps: {
  tabs: DocumentTabs;
  viewState: ViewStateDeps;
  library: LibraryDeps;
  blockDrafts?: import("@rightkit/document-core/app/blockDrafts").BlockDraftRegistry;
}) {
  const [internalDrafts] = useState(() => createBlockDraftRegistry());
  const blockDrafts = deps.blockDrafts ?? internalDrafts;
  const { settings } = useSettings();
  const {
    openDocs,
    setOpenDocs,
    activePath,
    setActivePath,
    editsRef,
    writeEdits,
    writeEditsLive,
    docxDirtyRef,
    setDocxDirty,
    pdfDirtyRef,
    setPdfDirty,
    imageDirtyRef,
    setImageDirty,
    htmlPagesRef,
    writeHtmlPages,
    htmlActivePage,
    setHtmlActivePage,
    mdxGrantsRef,
    trackMdxGrant,
  } = deps.tabs;
  const { setViews, viewsRef, viewMetaRef } = deps.viewState;
  const { refreshLibrary, syncPinned } = deps.library;

  const [error, setError] = useState<string | null>(null);
  const [openingPath, setOpeningPath] = useState<string | null>(null);
  // True while the launch effect's session restore is in flight (file-backed tabs loading,
  // active tab being chosen). App.tsx renders a small "Restoring session…" indicator in the
  // active-pane slot while true so the user sees feedback instead of a blank pane that reads
  // as "the app isn't loaded". Starts false so the first render shows the EmptyState (or the
  // active doc) — the launch effect flips it true on entry, false in `finally` on exit.
  const [restoringSession, setRestoringSession] = useState(false);
  const untitledSeq = useRef(1);
  const hydrated = useRef(false); // becomes true once the launch restore has read the saved session
  const [confirmClose, setConfirmClose] = useState<string | null>(null);
  const [conflict, setConflict] = useState<string | null>(null); // file changed on disk while dirty
  // HTML session dialogs: the page-aware close/save chooser, and a sub-page on-disk conflict. State
  // lives here (not in useHtmlSession) because requestClose opens the chooser before that hook runs.
  const [htmlConfirm, setHtmlConfirm] = useState<HtmlConfirmState>(null);
  const [htmlConflict, setHtmlConflict] = useState<HtmlConflictState>(null);
  // Last-seen on-disk fingerprint per open file, captured at read/save time and compared on
  // activation to catch background-tab edits the active-file-only watcher can't see.
  const fingerprints = useRef<Map<string, { mtimeMs: number; size: number }>>(new Map());
  const tabSeq = useRef(1);
  const openDocsRef = useRef(openDocs);
  const activePathRef = useRef(activePath);
  // queueAutoSave must exist before useAutoSave's deps (docxSaver/onSave/htmlSession) do — stable
  // forwarder into the scheduler slot useAutoSave populates each render.
  const autoSaveSchedulerRef = useRef<(path: string) => void>(() => {});
  const queueAutoSave = useCallback((path: string) => autoSaveSchedulerRef.current(path), []);

  useEffect(() => {
    openDocsRef.current = openDocs;
  }, [openDocs]);
  activePathRef.current = activePath;

  // MRU activation history (most-recent LAST). Closing the active tab focuses the most-recently-used
  // still-open tab — VS Code's `focusRecentEditorAfterClose` default — not nothing. `doClose` falls
  // back to the spatial neighbour if there's no usable history (shouldn't happen once a tab has ever
  // been active). Stale entries are harmless: `pickNextActive` filters against the remaining set.
  const activationOrderRef = useRef<string[]>([]);
  useEffect(() => {
    if (!activePath) return;
    const order = activationOrderRef.current;
    const at = order.indexOf(activePath);
    if (at !== -1) order.splice(at, 1);
    order.push(activePath);
  }, [activePath]);

  const withTabKey = useCallback(
    <T extends LoadedDocument>(doc: T, existingKey?: string): T => ({
      ...doc,
      tabKey: existingKey ?? doc.tabKey ?? `tab:${tabSeq.current++}`,
    }),
    [],
  );

  const releaseReplacedBackingPath = useCallback((oldPath: string, newPath: string) => {
    if (oldPath === newPath || isUntitled(oldPath) || isWebTabKey(oldPath)) return;
    fingerprints.current.delete(oldPath);
    clearFileDraft(oldPath);
    void revokePath(oldPath);
  }, []);

  // markSelfSaved/refreshFingerprint are owned by useDocumentSaver, constructed below — but
  // useFormatDirtyState (docx/image dirty-set + onDocxSave/writeImagePayload) needs them and must be
  // constructed BEFORE useDocumentSaver (the saver takes writeImagePayload as a dep). Break the cycle
  // with a stable ref indirection: the ref is populated once the saver exists, and these thin
  // forwarding callbacks never change identity, so downstream memoization is unaffected.
  const saverHandlesRef = useRef<{
    markSelfSaved: (path: string) => void;
    refreshFingerprint: (path: string) => void;
  }>({
    markSelfSaved: () => {},
    refreshFingerprint: () => {},
  });
  const markSelfSavedForward = useCallback(
    (path: string) => saverHandlesRef.current.markSelfSaved(path),
    [],
  );
  const refreshFingerprintForward = useCallback(
    (path: string) => saverHandlesRef.current.refreshFingerprint(path),
    [],
  );

  const {
    docxSaver,
    imageSaver,
    docxSaversRef,
    pdfSaversRef,
    pdfSavePlansRef,
    imageSaversRef,
    onDocxSave: persistDocx,
    onDocxChange,
    onPdfDirtyChange,
    onImageChange,
    clearImageDirty,
    restoreImageDirty,
    writeImagePayload,
  } = useFormatDirtyState({
    setError,
    setDocxDirty,
    setPdfDirty,
    setImageDirty,
    markSelfSaved: markSelfSavedForward,
    refreshFingerprint: refreshFingerprintForward,
    onDirty: queueAutoSave,
  });

  const { hasModifiedDraft, snapshotModified, isDirty, titleFor } = useDocumentDirty({
    blockDrafts,
    pdfDirtyRef,
    docxDirtyRef,
    imageDirtyRef,
    editsRef,
    htmlPagesRef,
  });

  // Setup-replacement recovery (snapshot before window destroy + rehydrate on next launch) —
  // extracted to useSetupRecovery; the pure (de)serialization stays in setupRecovery.ts.
  const { prepareSetupRecovery, restoreSetupRecovery } = useSetupRecovery({
    blockDrafts,
    setError,
    openDocsRef,
    activePathRef,
    viewsRef,
    isDirty,
    editsRef,
    htmlPagesRef,
    docxSaversRef,
    imageSaversRef,
    setOpenDocs,
    writeEdits,
    writeHtmlPages,
    setViews,
    setDocxDirty,
    restoreImageDirty,
    fingerprints,
    withTabKey,
  });

  const loader = useDocumentLoader({
    openDocs,
    setOpenDocs,
    setViews,
    viewsRef,
    viewMetaRef,
    writeEdits,
    setError,
    setOpeningPath,
    setActivePath,
    fingerprints,
    refreshLibrary,
    syncPinned,
    isDirty,
    clearImageDirty,
    withTabKey,
  });
  const { activate, open, openLiveUrl, openRecent, onOpenDialog, openImageSibling } = loader;

  const saver = useDocumentSaver({
    editsRef,
    writeEdits,
    setDocxDirty,
    setOpenDocs,
    setActivePath,
    setViews,
    setError,
    setConflict,
    fingerprints,
    refreshLibrary,
    imageSaver,
    imageSaversRef,
    docxSaver,
    docxSaversRef,
    pdfSaversRef,
    pdfSavePlansRef,
    setPdfDirty,
    pdfDirtyRef,
    clearImageDirty,
    writeImagePayload,
    releaseReplacedBackingPath,
    withTabKey,
    blockDrafts,
  });
  const {
    onSave,
    onSaveAs,
    onDocxSaveAs,
    markSelfSaved,
    refreshFingerprint,
    selfSaved,
    pendingSavePaths: saverPendingSavePaths = EMPTY_PENDING_SAVE_PATHS,
    pendingSavePathsRef: saverPendingSavePathsRef,
  } = saver;
  // In-flight save paths across all three save surfaces (saver's own set + tracked docx writes +
  // per-page HTML session writes) — extracted to usePendingSavePaths.
  const {
    onDocxSave,
    onHtmlSaveActivity,
    pendingSavePaths,
    pendingSavePathsRef: formatPendingSavePathsRef,
  } = usePendingSavePaths({
    persistDocx,
    saverPendingSavePaths,
  });
  saverHandlesRef.current.markSelfSaved = markSelfSaved;
  saverHandlesRef.current.refreshFingerprint = refreshFingerprint;

  // T2-1 data-loss guard, explicit save path: flush any pending debounced draft write for this doc
  // before saving. Save itself reads the live editsRef (always current, unaffected by the draft
  // debounce), so this isn't required for save correctness — it just guarantees the recovery draft
  // in localStorage can't hold text older than what's about to hit disk if something inspects it
  // mid-save. clearDraft/clearFileDraft (called after a successful save) already cancel the pending
  // write outright, so this and that together fully close the window.
  const onSaveFlushed = useCallback(
    (doc: LoadedDocument, options?: { silent?: boolean }) => {
      flushDraft(doc.path);
      return onSave(doc, options);
    },
    [onSave],
  );
  const onSaveAsFlushed = useCallback(
    (doc: LoadedDocument) => {
      flushDraft(doc.path);
      return onSaveAs(doc);
    },
    [onSaveAs],
  );

  const { newDocument, duplicateAsDraft, onTxtChange } = useDocumentCreation({
    untitledSeq,
    withTabKey,
    setOpenDocs,
    setActivePath,
    setViews,
    writeEdits,
    writeEditsLive,
    queueAutoSave,
  });

  // doClose (from useDocumentWatcher, constructed below) and reloadHtmlSubpage (from useHtmlSession,
  // constructed here) are mutually dependent, exactly as in the original single-hook version — broken
  // with the same stable-ref-forwarding pattern used above.
  const doCloseRef = useRef<(path: string) => void>(() => {});
  const doCloseForward = useCallback((path: string) => doCloseRef.current(path), []);

  // ── HTML site sessions ──────────────────────────────────────────────────────────────────────
  // The navigated SUB-page buffers, their save/discard/dirty model, the page-aware close chooser, the
  // sub-page conflict flow, and sub-page reload reconciliation all live in useHtmlSession. The ENTRY
  // page still uses the shared text path above (edits + onSave). Dialog state stays here (above) so
  // requestClose can open the chooser before this hook is constructed.
  const htmlSession = useHtmlSession({
    openDocs,
    editsRef,
    writeEdits,
    htmlPagesRef,
    writeHtmlPages,
    htmlActivePage,
    setHtmlActivePage,
    htmlConfirm,
    setHtmlConfirm,
    htmlConflict,
    setHtmlConflict,
    onSave,
    doClose: doCloseForward,
    markSelfSaved,
    setError,
    onDirty: queueAutoSave,
    onSaveActivity: onHtmlSaveActivity,
    blockDrafts,
  });
  const { reloadHtmlSubpage } = htmlSession;

  // Idle-delay autosave for file-backed tabs + the T2-1 draft-flush guards on unload/hide —
  // extracted to useAutoSave. `autoSaveSchedulerRef` stays here: `queueAutoSave` (above, consumed by
  // useFormatDirtyState/useHtmlSession/onTxtChange) must exist before this hook's deps do.
  useAutoSave({
    autoSave: settings.autoSave,
    openDocs,
    openDocsRef,
    activePath,
    activePathRef,
    isDirty,
    docxSaver,
    onDocxSave,
    onSave,
    saveAllHtmlPages: htmlSession.saveAllHtmlPages,
    autoSaveSchedulerRef,
  });

  const watcher = useDocumentWatcher({
    openDocs,
    openDocsRef,
    activationOrderRef,
    setOpenDocs,
    setActivePath,
    activePath,
    writeEdits,
    setDocxDirty,
    setPdfDirty,
    htmlPagesRef,
    writeHtmlPages,
    setHtmlActivePage,
    mdxGrantsRef,
    clearImageDirty,
    isDirty,
    openLiveUrl,
    activate,
    open,
    fingerprints,
    selfSaved,
    pendingSavePaths,
    reloadHtmlSubpage,
    setHtmlConfirm,
    setConfirmClose,
    confirmClose,
    setConflict,
    onSave,
    onDocxSave,
    docxSaver,
    docxSaversRef,
    onDocxSaveAs,
    withTabKey,
    blockDrafts,
    dropViewState: deps.viewState.dropViewState,
    synchronousPendingSavePathsRef: saverPendingSavePathsRef,
    synchronousPendingSavePathsRefs: [saverPendingSavePathsRef, formatPendingSavePathsRef],
  });
  const {
    doClose,
    reopenLastClosed,
    requestClose,
    confirmSave,
    confirmDontSave,
    applyReload,
    reload,
    teardownDoc,
  } = watcher;
  doCloseRef.current = doClose;

  // Keeping local edits after an external change must refresh the backend's expected source hash.
  // The next save then overwrites only the exact bytes just acknowledged, while the backend guard
  // still catches a second external write that races that save.
  const keepConflict = useCallback(
    async (path: string) => {
      const doc = openDocs.find((candidate) => candidate.path === path);
      if (doc && !["txt", "md", "mdx", "html"].includes(doc.format)) {
        // Binary/PDF saves have no source hash guard; preserve their existing Keep behavior.
        setConflict(null);
        return;
      }
      try {
        const loaded = await readDocument(path);
        if (!loaded.sourceSha256) {
          setError("Couldn't refresh file identity before overwrite.");
          return;
        }
        if (loaded.fingerprint) fingerprints.current.set(path, loaded.fingerprint);
        setOpenDocs((prev) =>
          prev.map((doc) =>
            doc.path === path
              ? {
                  ...doc,
                  sourceSha256: loaded.sourceSha256,
                  fingerprint: loaded.fingerprint ?? doc.fingerprint,
                }
              : doc,
          ),
        );
        setConflict(null);
      } catch (error) {
        setError(errorMessage(error));
      }
    },
    [openDocs, setOpenDocs],
  );

  // Bulk Close All + the Save-All/Discard-All quit flow — extracted to useCloseAll. The hook also
  // owns the confirmQuit/confirmCloseAll dialog state (only consumed after requestClose exists, so
  // unlike the other dialog state it doesn't need to stay at the top of this hook).
  const {
    confirmQuit,
    setConfirmQuit,
    confirmCloseAll,
    setConfirmCloseAll,
    closeAll,
    closeMany,
    confirmCloseAllSaveAll,
    confirmCloseAllDontSave,
    confirmCloseAllCancel,
    quitNow,
    quitSaveAll,
    quitDiscardAll,
  } = useCloseAll({
    openDocsRef,
    isDirty,
    setOpenDocs,
    setActivePath,
    teardownDoc,
    blockDrafts,
    htmlSession,
    onSaveFlushed,
    docxSaver,
    docxSaversRef,
    onDocxSave,
    onDocxSaveAs,
  });

  const { onEditImageExternal, onPrint } = useDocumentOutputActions(setError);

  useDocumentRuntimeEffects({
    openDocs,
    activePath,
    hydrated,
    untitledSeq,
    refreshLibrary,
    setOpenDocs,
    writeEdits,
    setViews,
    activate,
    open,
    reload,
    isDirty,
    setConfirmQuit,
    prepareSetupRecovery,
    restoreSetupRecovery,
    restoreSession: settings.restoreSession,
    setRestoringSession,
    blockDrafts,
  });

  // Draft-confirm dialog (view changes with a modified draft): Done commits, Discard drops,
  // Cancel stays on the current view with the editor open — extracted to useDraftConfirmGate.
  const {
    draftConfirm,
    setDraftConfirm,
    confirmDraftDone,
    confirmDraftDiscard,
    confirmDraftCancel,
    requestDraftAwareViewChange,
  } = useDraftConfirmGate({ blockDrafts });

  return {
    blockDrafts,
    activate,
    open,
    openLiveUrl,
    openRecent,
    onOpenDialog,
    newDocument,
    duplicateAsDraft,
    requestClose,
    closeAll,
    closeMany,
    reopenLastClosed,
    openImageSibling,
    onTxtChange,
    onSave: onSaveFlushed,
    onSaveAs: onSaveAsFlushed,
    onDocxSaveAs,
    onPrint,
    onDocxSave,
    onDocxChange,
    onPdfDirtyChange,
    docxSaver,
    docxSaversRef,
    pdfSaversRef,
    pdfSavePlansRef,
    onImageChange,
    imageSaver,
    imageSaversRef,
    onEditImageExternal,
    isDirty,
    hasModifiedDraft,
    snapshotModified,
    snapshotAll: (path?: string) => blockDrafts.snapshotAll(path),
    pendingSavePaths,
    titleFor,
    error,
    openingPath,
    restoringSession,
    setError,
    confirmClose,
    setConfirmClose,
    confirmCloseAll,
    setConfirmCloseAll,
    confirmCloseAllSaveAll,
    confirmCloseAllDontSave,
    confirmCloseAllCancel,
    confirmQuit,
    setConfirmQuit,
    conflict,
    setConflict,
    keepConflict,
    reload,
    confirmSave,
    confirmDontSave,
    applyReload,
    quitNow,
    quitSaveAll,
    quitDiscardAll,
    draftConfirm,
    setDraftConfirm,
    confirmDraftDone,
    confirmDraftDiscard,
    confirmDraftCancel,
    requestDraftAwareViewChange,
    // HTML site sessions — dialog state owned here; behavior from useHtmlSession.
    htmlConfirm,
    htmlConflict,
    ...htmlSession,
    // MDX component-import grant tracking — reported by the resolver/preview frame, revoked in
    // teardownDoc above.
    trackMdxGrant,
  };
}
