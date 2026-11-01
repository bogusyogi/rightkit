import { useCallback, useEffect, useRef, useState, type MutableRefObject } from "react";
import * as pdfjs from "pdfjs-dist";
import type { PDFDocumentProxy } from "pdfjs-dist";
// Must precede the "pdfjs-dist/web/pdf_viewer.mjs" import below — see its comment.
import "./pdfjsLibGlobal";
// pdf.js does not expose AnnotationEditorUIManager's dependencies (EventBus, GenericL10n) from the
// main `pdfjs-dist` entry — only from its bundled "web" viewer components. This is exactly the
// coupling ENGINEERING-FINDINGS-2026-08.md §4 flagged: "Mozilla declined a generic third-party
// embedding API." We take only the two viewer-component classes the UIManager constructor requires
// and build everything else (container/viewer elements, per-page layers) ourselves.
import { EventBus, GenericL10n } from "pdfjs-dist/web/pdf_viewer.mjs";
import { logError, logEvent } from "../../state/diagnostics";
import { modeToAnnotationEditorType, type PdfEditorMode } from "./pdfEditor";

// pdfjs-dist's own type defs mark AnnotationEditorUIManager's constructor args as `any` (it's
// treated as viewer-internal, not a public API) — so we keep this hook's construction call and the
// per-page wiring in PdfViewer.tsx typed loosely at that one boundary.
// biome-ignore lint/suspicious/noExplicitAny: pdfjs-dist's AnnotationEditorUIManager ctor args are untyped viewer-internals.
type AnyUIManager = any;
const IMAGE_STAMP_LAYER_WAIT_MS = 2000;
const IMAGE_STAMP_LAYER_POLL_MS = 16;

export interface PdfEditorState {
  /** Current tool. "none" = pure viewer, no AnnotationEditorLayer is interactive. */
  mode: PdfEditorMode;
  setMode: (mode: PdfEditorMode) => void;
  /** Construct the editor manager for an explicit edit request. Returns false when no PDF or
   *  container is available yet. Construction is synchronous so callers can immediately render
   *  editor layers against the returned manager. */
  ensureReady: () => boolean;
  freeTextSize: number;
  setFreeTextSize: (size: number) => void;
  /** True once any annotation-editor edit exists in the document's annotation storage. */
  dirty: boolean;
  /** Serialize current edits (editor annotations + form values) to PDF bytes via pdf.js
   *  `saveDocument()`. Returns null if no document is loaded or serialization fails. */
  saveBytes: () => Promise<Uint8Array | null>;
  /** Checkpoint serialized annotation bytes after successful disk commit, retaining newer edits. */
  checkpointAnnotationSave: () => boolean;
  /** Stable token for detecting annotation mutations across async save/reload work. */
  annotationStateToken: () => string;
  /** Token captured immediately after pdf.js synchronously serializes current annotations. */
  serializedAnnotationStateToken: () => string | null;
  /** Add an image stamp editor on the current page from a picked file (drag-drop or file picker).
   *  No-op if no editor layer is currently registered for the visible page (e.g. before first
   *  render). Returns false when the add could not be performed. */
  addImageStamp: (file: File, layerWaitMs?: number) => Promise<boolean>;
  /** True once the UI manager is constructed for this document. */
  ready: boolean;
  uiManagerRef: MutableRefObject<AnyUIManager | null>;
  /** Forward the viewer's own scale/rotation/page state into the UI manager so newly-placed
   *  editors and existing ones stay correctly positioned as the user zooms/rotates/scrolls. Cheap
   *  no-op when no document/editor is loaded. */
  syncViewState: (state: { scale: number; rotation: number; page: number }) => void;
}

/**
 * Stage 1 PDF editing: owns the pdf.js `AnnotationEditorUIManager` + its `EventBus` for one open
 * document, and exposes a small React-facing surface (mode, dirty, save, add-image) to PdfViewer.
 * Per-page `AnnotationEditorLayer`s are built in PdfViewer's renderPage (they need the same
 * per-page viewport/canvas/text-layer context renderPage already computes) and register themselves
 * with `uiManagerRef.current` via their own constructor (`uiManager.addLayer(this)`).
 *
 * View-first invariant: `mode` defaults to "none" on every new document, so opening a PDF is
 * unchanged — editing is an explicit opt-in via the toolbar.
 */
export function usePdfEditor({
  pdfDoc,
  containerRef,
}: {
  pdfDoc: PDFDocumentProxy | null;
  /** The scrollable pages container — doubles as both the UI manager's focus-containment root and
   *  its "viewer" element (only used for `--scale-factor` bookkeeping and `dir`, not for indexing
   *  page children, so our virtualized/unloaded slots don't confuse it). */
  containerRef: MutableRefObject<HTMLElement | null>;
}): PdfEditorState {
  const [mode, setModeState] = useState<PdfEditorMode>("none");
  const [dirty, setDirty] = useState(false);
  const [freeTextSize, setFreeTextSizeState] = useState(10);
  const [ready, setReady] = useState(false);
  const uiManagerRef = useRef<AnyUIManager | null>(null);
  const eventBusRef = useRef<InstanceType<typeof EventBus> | null>(null);
  const storageRef = useRef<{
    onSetModified: (() => void) | null;
    onResetModified: (() => void) | null;
  } | null>(null);
  const pdfDocRef = useRef<PDFDocumentProxy | null>(null);
  pdfDocRef.current = pdfDoc;
  // True while `saveBytes` is inside pdf.js `saveDocument()` — see `onReset` below.
  const serializingRef = useRef(false);
  const annotationEpochRef = useRef(0);
  const serializedEpochRef = useRef<number | null>(null);
  const serializedAnnotationHashRef = useRef<string | null>(null);
  const serializedStateTokenRef = useRef<string | null>(null);
  const committedAnnotationHashRef = useRef<string | null>(null);
  const annotationCheckpointSafeRef = useRef(true);

  const viewStateRef = useRef<{ scale: number; rotation: number; page: number } | null>(null);

  const syncViewState = useCallback(
    ({ scale, rotation, page }: { scale: number; rotation: number; page: number }) => {
      viewStateRef.current = { scale, rotation, page };
      const eventBus = eventBusRef.current;
      if (!eventBus) return;
      // UIManager multiplies viewer scale by PDF_TO_CSS_UNITS. Our scale already measures CSS
      // pixels per PDF unit, so normalize it first to keep annotation placement at the pointer.
      eventBus.dispatch("scalechanging", {
        source: eventBus,
        scale: scale / pdfjs.PixelsPerInch.PDF_TO_CSS_UNITS,
      });
      eventBus.dispatch("rotationchanging", { source: eventBus, pagesRotation: rotation });
      eventBus.dispatch("pagechanging", { source: eventBus, pageNumber: page });
    },
    [],
  );

  const destroyManager = useCallback(() => {
    const storage = storageRef.current;
    if (storage) {
      storage.onSetModified = null;
      storage.onResetModified = null;
    }
    storageRef.current = null;
    const uiManager = uiManagerRef.current;
    uiManagerRef.current = null;
    eventBusRef.current = null;
    if (uiManager) {
      // pdf.js owns a window keydown listener for editor history. Detach it before destroying
      // CommandManager: its destroy() intentionally nulls command storage, so a queued Cmd-Z or
      // Cmd-Shift-Z must not enter undo/redo after this document's manager has gone away.
      uiManager.removeEditListeners?.();
      uiManager.destroy();
    }
    setReady(false);
  }, []);

  // pdf.js's AnnotationEditorUIManager is a viewer-internal class with no stability contract on
  // its constructor signature (see docs/ENGINEERING-FINDINGS-2026-08.md §4 — Mozilla declined a
  // generic embedding API). Guard construction so a pdfjs-dist upgrade that shifts this shape
  // degrades to "editing controls stay disabled" rather than breaking the pure viewer.
  const ensureReady = useCallback((): boolean => {
    if (uiManagerRef.current) return true;
    const doc = pdfDocRef.current;
    const container = containerRef.current;
    if (!doc || !container) return false;

    let eventBus: InstanceType<typeof EventBus> | null = null;
    let uiManager: AnyUIManager | null = null;
    try {
      eventBus = new EventBus();
      const l10n = new GenericL10n(navigator.language || "en-US");
      uiManager = new (
        pdfjs as unknown as { AnnotationEditorUIManager: new (...a: unknown[]) => AnyUIManager }
      ).AnnotationEditorUIManager(
        container, // container: focus/keyboard scoping root
        container, // viewer: only read for --scale-factor / dir bookkeeping here
        null, // viewerAlert
        null, // altTextManager
        null, // commentManager
        null, // signatureManager
        eventBus,
        doc,
        null, // pageColors — no forced high-contrast palette
        undefined, // highlightColors — pdf.js defaults
        false, // enableHighlightFloatingButton
        false, // enableUpdatedAddImage
        false, // enableNewAltTextWhenAddingImage
        null, // mlManager — no ML alt-text guesses
        null, // editorUndoBar — toolbar has no undo-bar UI in Stage 1
        false, // supportsPinchToZoom
      );
      uiManager.l10n = l10n;
    } catch {
      logError("pdf-editor:init");
      eventBus = null;
      uiManager = null;
    }
    if (!eventBus || !uiManager) return false;
    eventBusRef.current = eventBus;
    uiManagerRef.current = uiManager;
    // A save reload can replace the manager without ready ever rendering false.
    // Seed its scale immediately, before any annotation layer can accept input.
    if (viewStateRef.current) syncViewState(viewStateRef.current);

    // pdfjs-dist's generated types declare these callback slots as literal `null` (they're meant
    // to be assigned a function at runtime, same as pdf.js's own viewer does) — cast the boundary.
    // `annotationStorage` is a lazily-created getter on every real PDFDocumentProxy, but stays
    // undefined on the minimal PDFDocumentProxy stand-ins other PdfViewer tests construct (they
    // predate Stage 1 and only implement what the pure viewer touches) — guard rather than crash
    // the whole component (and, with it, unrelated pure-viewer behavior) from an editor-only hook.
    const storage = doc.annotationStorage as unknown as
      | { onSetModified: (() => void) | null; onResetModified: (() => void) | null }
      | undefined;
    const onModified = () => {
      annotationEpochRef.current += 1;
      annotationCheckpointSafeRef.current = false;
      setDirty(true);
    };
    // pdf.js calls `annotationStorage.resetModified()` inside `saveDocument()` itself, i.e. the
    // moment bytes are SERIALIZED — long before the app has written anything to disk. Honouring
    // that reset here flipped the tab to "clean" during every save attempt, and a save that then
    // failed (prepare/commit rejected) left real unsaved annotations showing as saved. Dirty state
    // is cleared by the post-commit document reload instead (new `pdfDoc` re-runs this effect), so
    // resets raised while `saveBytes` is serializing are ignored.
    const onReset = () => {
      if (serializingRef.current) return;
      annotationCheckpointSafeRef.current = true;
      setDirty(false);
    };
    if (storage) {
      storage.onSetModified = onModified;
      storage.onResetModified = onReset;
    }
    storageRef.current = storage ?? null;
    setReady(true);
    return true;
  }, [containerRef, syncViewState]);

  // Build/tear down one UIManager per loaded document. The manager stays absent while a PDF is
  // merely being viewed, then survives mode="none" so pdf.js's unified undo history remains intact.
  // A document identity change always destroys it and resets mode/dirty state.
  // biome-ignore lint/correctness/useExhaustiveDependencies: pdfDoc identity defines the manager lifetime.
  useEffect(() => {
    setModeState("none");
    setDirty(false);
    annotationEpochRef.current = 0;
    serializedEpochRef.current = null;
    serializedAnnotationHashRef.current = null;
    serializedStateTokenRef.current = null;
    committedAnnotationHashRef.current = null;
    annotationCheckpointSafeRef.current = true;
    setReady(false);

    return () => {
      destroyManager();
    };
  }, [destroyManager, pdfDoc]);

  const setMode = useCallback(
    (next: PdfEditorMode) => {
      const uiManager =
        next === "none" ? uiManagerRef.current : ensureReady() ? uiManagerRef.current : null;
      setModeState(next);
      if (!uiManager) return;
      // A rejection here previously vanished completely: pdf.js's own viewer also calls
      // `updateMode()` fire-and-forget, and every registered page's `AnnotationEditorLayer.
      // updateMode()` — the call that un-hides its div — only runs once THIS promise resolves. A
      // page whose annotations include a Stamp used to reject this unconditionally (see
      // `NULL_STRUCT_TREE_LAYER`'s doc comment in PdfViewer.tsx) with zero visible error: the mode
      // still LOOKED like it switched (this hook's own `mode` state flips regardless), but every
      // editor layer stayed permanently inert. Logging a rejection here keeps any future regression
      // in this class diagnosable instead of silently inert again.
      void uiManager
        .updateMode(modeToAnnotationEditorType(next))
        .catch(() => logError("pdf-editor:mode-switch"));
    },
    [ensureReady],
  );

  const setFreeTextSize = useCallback((size: number) => {
    if (!Number.isFinite(size)) return;
    const normalized = Math.max(6, Math.min(144, Math.round(size)));
    setFreeTextSizeState(normalized);
    uiManagerRef.current?.updateParams(pdfjs.AnnotationEditorParamsType.FREETEXT_SIZE, normalized);
  }, []);

  const annotationStateToken = useCallback((): string => {
    const hash = pdfDocRef.current?.annotationStorage?.serializable?.hash ?? "";
    return `${annotationEpochRef.current}:${hash}`;
  }, []);

  const saveBytes = useCallback(async (): Promise<Uint8Array | null> => {
    const doc = pdfDocRef.current;
    if (!doc) return null;
    const serializedEpoch = annotationEpochRef.current;
    serializedEpochRef.current = serializedEpoch;
    serializedAnnotationHashRef.current = doc.annotationStorage?.serializable?.hash ?? null;
    serializedStateTokenRef.current = null;
    annotationCheckpointSafeRef.current = true;
    serializingRef.current = true;
    logEvent("pdf-editor:serialize:start");
    try {
      // saveDocument() invokes annotationStorage.serializable synchronously before returning its
      // worker promise. Capture token immediately after that call: FreeText serialization can
      // normalize editor state/hash without a real edit, while any later mutation must remain
      // visible to save-plan stale checks.
      const savePromise = doc.saveDocument();
      const serializedStateToken = annotationStateToken();
      const serializedEpochAfterCall = annotationEpochRef.current;
      if (serializedEpochAfterCall === serializedEpoch) {
        // Checkpoint against normalized serialization state. Keep the pre-call hash when an
        // epoch changed during save so newer edits remain visible to checkpointAnnotationSave.
        serializedAnnotationHashRef.current =
          doc.annotationStorage?.serializable?.hash ?? serializedAnnotationHashRef.current;
      }
      const bytes = await savePromise;
      const settledStateToken = annotationStateToken();
      if (
        serializedEpochAfterCall === serializedEpoch &&
        settledStateToken === serializedStateToken
      ) {
        serializedStateTokenRef.current = serializedStateToken;
      }
      logEvent("pdf-editor:serialize:done");
      return bytes;
    } catch {
      logError("pdf-editor:serialize");
      return null;
    } finally {
      serializingRef.current = false;
    }
  }, [annotationStateToken]);

  const serializedAnnotationStateToken = useCallback(
    (): string | null => serializedStateTokenRef.current,
    [],
  );

  const checkpointAnnotationSave = useCallback((): boolean => {
    const serializedEpoch = serializedEpochRef.current;
    const serializedHash = serializedAnnotationHashRef.current;
    const storage = pdfDocRef.current?.annotationStorage;
    if (!storage || !annotationCheckpointSafeRef.current) return false;
    const currentHash = storage.serializable.hash;
    if (serializedEpoch === null || serializedHash === null) {
      return (
        committedAnnotationHashRef.current === null ||
        currentHash === committedAnnotationHashRef.current
      );
    }
    if (serializedEpoch !== annotationEpochRef.current || currentHash !== serializedHash) {
      return false;
    }
    storage.resetModified();
    setDirty(false);
    committedAnnotationHashRef.current = serializedHash;
    serializedEpochRef.current = null;
    serializedAnnotationHashRef.current = null;
    annotationCheckpointSafeRef.current = true;
    return true;
  }, []);

  const addImageStamp = useCallback(
    async (file: File, layerWaitMs = IMAGE_STAMP_LAYER_WAIT_MS): Promise<boolean> => {
      if (!ensureReady()) return false;
      const uiManager = uiManagerRef.current;
      if (!uiManager) return false;
      const deadline = Date.now() + Math.min(15000, Math.max(0, layerWaitMs));
      while (!uiManager.currentLayer && Date.now() < deadline) {
        await new Promise<void>((resolve) => setTimeout(resolve, IMAGE_STAMP_LAYER_POLL_MS));
        if (uiManagerRef.current !== uiManager) return false;
      }
      const layer = uiManager.currentLayer ?? null;
      if (!layer) return false;
      await uiManager.updateMode(modeToAnnotationEditorType("stamp"));
      await layer.pasteEditor({ mode: modeToAnnotationEditorType("stamp") }, { bitmapFile: file });
      setModeState("stamp");
      return true;
    },
    [ensureReady],
  );

  return {
    mode,
    setMode,
    ensureReady,
    freeTextSize,
    setFreeTextSize,
    dirty,
    saveBytes,
    checkpointAnnotationSave,
    annotationStateToken,
    serializedAnnotationStateToken,
    addImageStamp,
    ready,
    uiManagerRef,
    syncViewState,
  };
}
