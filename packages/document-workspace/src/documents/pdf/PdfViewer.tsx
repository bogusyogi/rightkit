import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { createShortcutRegistry } from "@rightkit/react-shell";
import * as pdfjs from "pdfjs-dist";
import type { PDFDocumentProxy, PageViewport } from "pdfjs-dist";
import type { SearchProvider, ViewMeta } from "@rightkit/document-core/documents/types";
import { boundedCanvasDpr, highlightSpan, viewportRotation } from "./pdfShared.js";
import { PdfToolbar } from "./PdfToolbar.js";
import { PdfViewerBody } from "./PdfViewerBody.js";
import { PdfPasswordPrompt } from "@rightkit/pdf-ui";
import { usePdfDocumentLoader } from "./usePdfDocumentLoader.js";
import { usePdfPreviewRecompose } from "./usePdfPreviewRecompose.js";
import { usePdfSavePlan } from "./usePdfSavePlan.js";
import { usePdfFit } from "./usePdfFit.js";
import {
  acquirePdfRenderKeepalive,
  createPdfLinkService,
  type PdfLinkActions,
} from "@rightkit/pdf-ui";
import { openExternal } from "@rightkit/document-core/state/tauriBridge";
import { usePdfSearch } from "./usePdfSearch.js";
import { usePdfOcr } from "./usePdfOcr.js";
import type { NativePdfEditingAction } from "./macPdfNativeBridge.js";
import type { OcrLine } from "./pdfOcr.js";
import { applyOcrCorrections, findOcrLineAt, sampleCanvasRectBackground } from "./pdfOcrEdit.js";
import { usePdfViewControls } from "./usePdfViewControls.js";
import { usePdfEditor } from "./usePdfEditor.js";
import { createPdfEditorTextLayerAdapter, SOURCE_TEXT_COMMAND } from "./pdfEditor.js";
import { usePdfEditSession, type PdfEditSessionBridge } from "./usePdfEditSession.js";
import { PdfInlineTextEditor } from "./PdfInlineTextEditor.js";
import type {
  PdfColor,
  PdfEditCommand,
  PdfPageGraph,
  PdfQuad8,
  PdfRect,
  PdfSavePlan,
  PdfTextAppearance,
  PdfTextIsland,
} from "./pdfEditModel.js";
// New PDF-edit backend commands (docs/PDF-EDITOR-NOW.md §5.2/§7.2/§9.4/§10.1/§10.3). These land in
// tauriBridge.ts at Dispatch D2 — this lane imports them by name against the frozen type contract
// in pdfEditModel.ts, per the dispatch brief ("D2 is responsible for making those exports real").
import { inspectPdfPage, listPdfFonts } from "./pdfBridge.js";
// Worker-src setup lives in pdfPrinting.ts (module side effect), the one shared resolution point
// for both the viewer and the print path — see its comment for why the worker URL must never be
// resolved twice through different chunks.
import "./pdfPrinting.js";
import { printPdfDocument } from "./pdfPrinting.js";
import { acquireResource } from "@rightkit/document-core/app/resourceCensus";
import { logError, logPerf } from "@rightkit/document-core/state/diagnostics";
import { errorMessage } from "@rightkit/document-core/state/errors";
import {
  isPdfBenchEnabled,
  markPdfBenchError,
  markPdfBenchFirstCanvasPaint,
  registerPdfBenchLatePageNavigator,
} from "./pdfBench.js";
import { nearestVisiblePdfPage } from "./nearestVisiblePdfPage.js";
import { documentEvent } from "@rightkit/document-core/host";

// pdf.js's own bundled code (build/pdf.mjs) has at least one call site — converting an existing
// Stamp annotation into an editable `StampEditor` object, which `AnnotationEditorLayer`'s
// `#enableAll()`/`updateMode()` does for every existing annotation on entering an editor mode —
// that dereferences `structTreeLayer` (`parent._structTree.getAriaAttributes(id)`) with NO
// null-guard, unlike its sibling call sites (which correctly use `?.`). This app deliberately has
// no struct-tree implementation (Stage 1 doesn't build one — see the comment at its use below), so
// `null` — the value pdf.js's own `AnnotationEditorLayerBuilder` itself passes through at runtime,
// despite its generated types (wrongly) declaring the field non-nullable — throws
// `TypeError: Cannot read properties of null (reading 'getAriaAttributes')` from inside
// `AnnotationEditorUIManager.updateMode()`'s promise chain the instant a page whose annotations
// include a Stamp tries to enter ANY editor mode. Since `updateMode()` is invoked fire-and-forget
// (`void uiManager.updateMode(...)`, matching pdf.js's own viewer), that rejection was entirely
// invisible: the editor mode's own state (aria-pressed, etc.) still flipped, but every registered
// page's `AnnotationEditorLayer.updateMode()` — the call that un-hides its div — was never reached,
// leaving the layer permanently inert (a real DOM node, 0×0, `hidden`) with no visible error. This
// is why it only reproduces once a document HAS a Stamp annotation already on disk — i.e. after the
// very first save that includes one, never on a document's first, pre-save edit session. This
// minimal stub satisfies the one method pdf.js actually calls on it without depending on any deeper
// pdf.js internals or a real accessibility struct tree.
const NULL_STRUCT_TREE_LAYER = {
  async getAriaAttributes(): Promise<Map<string, string> | undefined> {
    return undefined;
  },
};

// Pages whose canvas render exceeds this are exempt from scroll-out unload. The
// unload-on-scroll-out strategy assumes re-rendering is ~20 ms; a page with a
// huge embedded image (measured: one 6352×3068 image = ~4.7 s per render,
// re-paid on EVERY re-entry because it exceeds pdf.js's decoded-image cache cap)
// breaks that assumption, so those keep their canvas alive across scroll-out.
const COSTLY_RENDER_MS = 250;
// Byte budget for expensive off-screen page canvases kept alive, evicted stalest-first (LRU by
// last render/scroll-out). Each page is costed at canvas.width * canvas.height * 4 bytes; canvas
// dimensions already include devicePixelRatio, so this is the device-pixel backing-store size.
// There is deliberately no per-count cap: the byte budget is the only bound.
const MAX_KEPT_CANVAS_BYTES = 32 * 1024 * 1024;
// Virtualization hysteresis (#138): pages render when within RENDER_ROOT_MARGIN of the viewport
// and unload only after leaving the wider UNLOAD_ROOT_MARGIN, so direction reversals near the
// render edge do not churn rasters.
const RENDER_ROOT_MARGIN = "50% 0px";
const UNLOAD_ROOT_MARGIN = "150% 0px";
/** Fallback mask behind the inline source-text editor when the canvas under it isn't uniform. */
const WHITE_COVER: PdfColor = { r: 1, g: 1, b: 1, alpha: 1 };

// ---------------------------------------------------------------------------------------------
// §4/§5.3 source-graph <-> viewport correlation helpers
// ---------------------------------------------------------------------------------------------
//
// The Rust inspector reports islands/quads in PDF user-space (pre-viewport-transform, per
// pdfEditModel.ts's PdfRect doc comment). pdf.js's DOM never identifies persistent source content
// (§4), so every hit-test and every on-screen placement goes through pdf.js's own PageViewport
// transform, computed once per page in renderPage below and cached in viewportsRef.

/** Point-in-polygon (ray casting) test against one glyph-run quad's 4 corner pairs. Quads may be
 *  non-axis-aligned (rotated pages, skewed text matrices), so a plain bounding-box test would
 *  over-select; this matches the exact painted quad instead. */
function quadContainsPoint(quad: PdfQuad8, x: number, y: number): boolean {
  const pts: Array<[number, number]> = [
    [quad[0], quad[1]],
    [quad[2], quad[3]],
    [quad[4], quad[5]],
    [quad[6], quad[7]],
  ];
  let inside = false;
  for (let i = 0, j = pts.length - 1; i < pts.length; j = i++) {
    const [xi, yi] = pts[i];
    const [xj, yj] = pts[j];
    const intersects = yi > y !== yj > y && x < ((xj - xi) * (y - yi)) / (yj - yi) + xi;
    if (intersects) inside = !inside;
  }
  return inside;
}

/** Axis-aligned PDF-space bounding box over every quad in an island — used both as the hit target's
 *  selection rect and (conservatively, see judgment-call note on startSourceEditDraft below) as the
 *  draft's available-bounds until the backend's own layout validation runs at Save time (§8). */
function islandPdfBounds(island: PdfTextIsland): PdfRect {
  let minX = Number.POSITIVE_INFINITY;
  let minY = Number.POSITIVE_INFINITY;
  let maxX = Number.NEGATIVE_INFINITY;
  let maxY = Number.NEGATIVE_INFINITY;
  for (const quad of island.quads) {
    for (let i = 0; i < 8; i += 2) {
      minX = Math.min(minX, quad[i]);
      maxX = Math.max(maxX, quad[i]);
      minY = Math.min(minY, quad[i + 1]);
      maxY = Math.max(maxY, quad[i + 1]);
    }
  }
  if (!Number.isFinite(minX)) return { x: 0, y: 0, width: 0, height: 0 };
  return { x: minX, y: minY, width: maxX - minX, height: maxY - minY };
}

/** Converts a PDF-space rect to a screen-space (CSS px) rect via the page's current pdf.js
 *  viewport, per §6.1's `pdfRectToViewport(session.draft.rect, viewport)`. Converts all four
 *  corners (not just two) and takes the bounding box, so rotation is handled correctly. */
function pdfRectToViewport(rect: PdfRect, viewport: PageViewport): PdfRect {
  const corners: Array<[number, number]> = [
    [rect.x, rect.y],
    [rect.x + rect.width, rect.y],
    [rect.x, rect.y + rect.height],
    [rect.x + rect.width, rect.y + rect.height],
  ];
  const converted = corners.map(
    ([x, y]) => viewport.convertToViewportPoint(x, y) as [number, number],
  );
  const xs = converted.map((c) => c[0]);
  const ys = converted.map((c) => c[1]);
  const minX = Math.min(...xs);
  const maxX = Math.max(...xs);
  const minY = Math.min(...ys);
  const maxY = Math.max(...ys);
  return { x: minX, y: minY, width: maxX - minX, height: maxY - minY };
}

/** Default appearance for a fresh Add Text draft. The contract's font/size/color toolbar (§3.2) is
 *  owned by the frozen PdfToolbar.tsx (Dispatch B), which does not yet expose those controls in
 *  this dispatch — see the file-level judgment-call note near `handlePageClick` below. */
const DEFAULT_TEXT_APPEARANCE: PdfTextAppearance = {
  fontId: "source-serif-4",
  family: "Source Serif 4",
  sizePt: 12,
  color: { r: 0, g: 0, b: 0, alpha: 1 },
};

export function PdfViewer({
  documentPath,
  url,
  active = true,
  initialView,
  initialEditingAction,
  onViewState,
  registerPrint,
  registerSearchProvider,
  findBar,
  findMode,
  onRequestFind,
  onRequestReplace,
  registerPdfSave,
  registerPdfSavePlan,
  onDirtyChange,
  readOnly = false,
}: {
  documentPath: string;
  url?: string;
  active?: boolean;
  initialView?: ViewMeta;
  /** Explicit action carried across the native-reader → editor transition. */
  initialEditingAction?: NativePdfEditingAction;
  onViewState?: (m: ViewMeta) => void;
  /** Register a print function that uses THIS pane's already-loaded PDFDocumentProxy — see
   *  documents/pdf/pdfPrinting.ts printPdfDocument. */
  registerPrint?: (fn: (() => Promise<boolean>) | null) => void;
  registerSearchProvider?: (provider: SearchProvider | null) => void;
  /** Shared FindBar element, floated over content below toolbar triggers. */
  findBar?: ReactNode;
  findMode?: "find" | "replace";
  onRequestFind?: () => void;
  onRequestReplace?: () => void;
  /** Register this pane's ANNOTATION-ONLY save function (pdf.js `saveDocument()` → PDF bytes),
   *  unchanged from Stage 1. `src/app/useDocumentSaver.ts` (a sibling lane, Dispatch B8) still calls
   *  this exact shape today, so it stays wired to `pdfEditor.saveBytes()` verbatim — changing its
   *  contract here would silently break the current Save/Save As/Ctrl+S path for annotation edits. */
  registerPdfSave?: (fn: (() => Promise<Uint8Array | null>) | null) => void;
  /** Register this pane's full two-phase source/OCR edit save plan (§10.1-§10.3). */
  registerPdfSavePlan?: (fn: (() => Promise<PdfSavePlan>) | null) => void;
  /** Reflects whether unsaved annotation-editor edits exist (pdf.js annotation storage is dirty).
   *  The app can use this the same way it uses `docxDirty` for the tab's dirty indicator. */
  onDirtyChange?: (dirty: boolean) => void;
  /** Wave 2 Read mode: annotation tools, source-text edit, and OCR stay off — the viewer is a
   *  pure reader. Entering Read commits any in-flight draft and drops active edit modes. */
  readOnly?: boolean;
}) {
  const scrollRef = useRef<HTMLDivElement>(null);
  const benchmarkEnabled = isPdfBenchEnabled();
  const effectiveInitialView = benchmarkEnabled ? undefined : initialView;
  const restoredRef = useRef(false); // applied initialView page once per document
  const reportReadyRef = useRef(false); // gate reporting until after the initial restore
  const slotRefs = useRef<Map<number, HTMLDivElement>>(new Map());
  const renderedRef = useRef<Map<number, () => void>>(new Map()); // page → cleanup
  const rasterRenderRef = useRef<Map<number, () => Promise<void>>>(new Map());
  const rasterDropRef = useRef<Map<number, () => void>>(new Map());
  const rasterPresentRef = useRef<Set<number>>(new Set());
  const rasterPendingRef = useRef<Set<number>>(new Set());
  const visibleRef = useRef<Set<number>>(new Set());
  const benchmarkRenderRef = useRef<Set<number>>(new Set());
  // Pages whose last canvas render exceeded COSTLY_RENDER_MS, → last-touched
  // timestamp. Used to exempt them from scroll-out unload (see the observer).
  const costlyRef = useRef<Map<number, number>>(new Map());
  const pdfRef = useRef<PDFDocumentProxy | null>(null);
  // Pane activation is intentionally separate from PDF document lifetime. Pure-view panes may
  // release page rasters while hidden, but an initialized editor keeps its layers/history alive.
  const activeRef = useRef(active);
  activeRef.current = active;
  const previousActiveRef = useRef(active);
  // Census attribution only; renderPage must not re-create itself when the URL changes.
  const documentPathRef = useRef(documentPath);
  documentPathRef.current = documentPath;

  // §4/§9.4: per-page pdf.js viewport (for hit-testing + on-screen placement), the Rust source
  // graph (cached per page so repeated clicks on an already-inspected page skip the round trip),
  // and the in-memory candidate document/pages committed-but-unsaved edits render from.
  const viewportsRef = useRef<Map<number, PageViewport>>(new Map());
  const pageGraphsRef = useRef<Map<number, PdfPageGraph>>(new Map());
  // Bumped wherever pageGraphsRef is cleared, so an inspection requested before that clear cannot
  // write a pre-save/pre-switch graph back into the cache or into a draft (handlePageClick).
  const pageGraphsGenRef = useRef(0);
  const previewDocRef = useRef<PDFDocumentProxy | null>(null);
  // `PDFDocumentProxy` itself has no `destroy()` in this pdfjs-dist version — disposal goes through
  // the `PDFDocumentLoadingTask` that produced it, same as `usePdfDocumentLoader`'s own task
  // ownership (`replaceDocument`) and `pdfPrinting.ts`/`pdfBenchTransport.ts`. The preview document
  // is created ad hoc via `pdfjs.getDocument()`, so its disposal likewise goes through this task.
  const previewLoadingTaskRef = useRef<ReturnType<typeof pdfjs.getDocument> | null>(null);
  const previewPagesRef = useRef<Set<number>>(new Set());
  // Bumped on every `handleRecomposePreview` call (same pattern as usePdfSearch.ts's
  // `searchSeqRef`) — guards against concurrent publishes (rapid undo/redo) racing: whichever call
  // is no longer the latest by the time its own compose/load finishes discards its own task instead
  // of leaking it or overwriting the refs above with a stale candidate.
  const previewSeqRef = useRef(0);
  // Serialize page decoding within this pane so adjacent scanned pages do not allocate their
  // full mask/image working sets simultaneously. Cancelled queue entries never start decoding.
  const renderTailRef = useRef<Promise<void>>(Promise.resolve());
  const canvasBytesRef = useRef(new Map<number, number>());
  const ocrSourceLinesRef = useRef<ReadonlyMap<number, OcrLine[]>>(new Map());
  // Best-known document_sha256 (§5.1) — populated from every inspected page graph, including an
  // empty scanned page, then refreshed to the committed revision's sha after Save.
  const sourceShaRef = useRef<string | null>(null);
  // Pages a post-commit reload still needs re-rendered, drained by the `[pdfEditor.ready, pdfDoc]`
  // catch-up effect below rather than rendered synchronously right after `replaceDocument` — see
  // that effect's doc comment for why: rendering synchronously there races React's own processing
  // of the `pdfDoc` swap (which is what tears down the OLD AnnotationEditorUIManager and builds the
  // new one), and can leave a page's editor layer registered against a soon-destroyed manager.
  const pendingReloadRerenderPagesRef = useRef<Set<number> | null>(null);
  const saveLockedRef = useRef(false);
  const [saveLocked, setSaveLockedState] = useState(false);
  const setSaveLocked = useCallback((locked: boolean) => {
    saveLockedRef.current = locked;
    setSaveLockedState(locked);
  }, []);

  const [scale, setScale] = useState(1);
  const [rotation, setRotation] = useState(0); // 0 | 90 | 180 | 270, applied to every page
  const [current, setCurrent] = useState(1);
  const currentRef = useRef(current);
  currentRef.current = current;
  const [pageDraft, setPageDraft] = useState<string | null>(null); // in-progress page-box edit
  const [pageShake, setPageShake] = useState(false); // brief shake on an invalid page-box entry
  const [fitMode, setFitMode] = useState<"width" | "page" | null>("width");
  const [scaleDraft, setScaleDraft] = useState(() => String(Math.round(100 * 100)));
  const [ocrCorrectionError, setOcrCorrectionError] = useState<string | null>(null);
  const currentRaf = useRef<number | null>(null);

  const [panel, setPanel] = useState<null | "outline" | "thumbs">(() => {
    const saved = typeof localStorage !== "undefined" ? localStorage.getItem("vr.pdf.panel") : null;
    return saved === "outline" || saved === "thumbs" ? saved : null;
  });
  useEffect(() => {
    try {
      if (panel) localStorage.setItem("vr.pdf.panel", panel);
      else localStorage.removeItem("vr.pdf.panel");
    } catch {
      /* ignore */
    }
  }, [panel]);

  const {
    pdfDoc,
    numPages,
    base,
    pageSizes,
    setPageSizes,
    error,
    renderErrors,
    setRenderErrors,
    pwPrompt,
    outline,
    replaceDocument,
  } = usePdfDocumentLoader({
    documentPath,
    url,
    initialView: effectiveInitialView,
    pdfRef,
    renderedRef,
    slotRefs,
    visibleRef,
    restoredRef,
    reportReadyRef,
    setCurrent,
    setScale,
    setRotation,
    setFitMode,
  });

  useEffect(() => {
    if (!registerPrint) return;
    registerPrint(async () => {
      const doc = pdfRef.current;
      if (!doc) return false;
      return printPdfDocument(doc);
    });
    return () => registerPrint(null);
  }, [registerPrint]);

  // Stage 1 editing: FreeText / Ink / Highlight / Stamp via pdf.js's own AnnotationEditorLayer
  // stack. `mode` defaults to "none" (view-first invariant) — see usePdfEditor.ts for the
  // UIManager lifecycle and docs/ENGINEERING-FINDINGS-2026-08.md §4 for why this needs its own
  // hook rather than a flag flip.
  const pdfEditor = usePdfEditor({ pdfDoc, containerRef: scrollRef });

  // §5-§10: source-text edit session (mode/tool/selection/draft/history, checkpointing). Bridge
  // functions are the §5.2/§7.2 backend commands, imported by name from tauriBridge.ts per the
  // dispatch brief (D2 lands the real implementations there). The bridge object is memoized with an
  // empty dep array — `inspectPdfPage`/`listPdfFonts` are stable module-level imports, and
  // usePdfEditSession recreates its own callbacks whenever this reference changes.
  const editBridgeRef = useRef<PdfEditSessionBridge>({ inspectPdfPage, listPdfFonts });
  const editSession = usePdfEditSession({
    bridge: editBridgeRef.current,
    sourcePath: documentPath,
  });

  // Reset every per-document cache when the document identity changes (new tab/path). Intentionally
  // keyed on `documentPath` only — this must reset once per document switch, not whenever
  // `editSession.resetForDocument`'s identity happens to change; `editSession`'s own identity is
  // stable across renders (its setters are useCallback-memoized) so it is safely omitted.
  // biome-ignore lint/correctness/useExhaustiveDependencies: see comment above.
  useEffect(() => {
    viewportsRef.current = new Map();
    pageGraphsRef.current = new Map();
    pageGraphsGenRef.current++;
    void previewLoadingTaskRef.current?.destroy();
    previewLoadingTaskRef.current = null;
    previewDocRef.current = null;
    previewPagesRef.current = new Set();
    // Invalidate any still-in-flight `handleRecomposePreview` call from the previous document —
    // its `seq` capture can no longer match, so it discards its own task instead of writing a
    // stale preview into the refs just reset above once it resolves after this switch.
    previewSeqRef.current++;
    sourceShaRef.current = null;
    costlyRef.current = new Map();
    editSession.resetForDocument();
  }, [documentPath]);

  // ---------------------------------------------------------------------------------------------
  // §6.2 unified pdf.js command history for source-text commits
  // ---------------------------------------------------------------------------------------------
  //
  // `usePdfEditSession.commitDraft()` already applies the resulting command to `sourceCommands`
  // and calls `publish()` itself (so §9.4's preview recompose and dirty-state tracking work with
  // zero extra wiring here) — see its doc comment. That means the "commit" has already happened by
  // the time we get the command back; `pushSourceCommand`'s helper (pdfEditor.ts) assumes the
  // opposite order (apply happens lazily, driven by `addCommands({ mustExec: true })`), so using it
  // verbatim here would double-apply. Instead we register a *retroactive* history entry directly:
  // `mustExec: false` skips the immediate `cmd()` call (state is already correct), and only
  // undo/redo from here on route through `editSession.revert`/`editSession.apply` so source edits
  // and annotation edits undo/redo in one chronological pdf.js stack (§6.2).
  const registerCommittedSourceCommand = useCallback(
    (command: PdfEditCommand) => {
      const uiManager = pdfEditor.uiManagerRef.current;
      if (!uiManager) return; // degrade to sourceDigest-only undo if the UI manager never built
      uiManager.addCommands({
        cmd: () => editSession.apply(command),
        undo: () => editSession.revert(command),
        post: () => editSession.publish(),
        mustExec: false,
        type: SOURCE_TEXT_COMMAND,
        overwriteIfSameType: false,
        keepUndo: false,
      });
    },
    [pdfEditor.uiManagerRef, editSession],
  );

  // Every explicit "commit the active draft" call site (blur/Enter from the inline editor, click
  // another target, Done) routes through this single function so the history registration above
  // and the no-op-on-overflow/no-op-on-empty behavior stay in one place. Selecting a new island or
  // exiting edit mode both call this FIRST (before usePdfEditSession's own selectIsland/
  // exitEditMode, which also commit internally) so that by the time those run, the draft is already
  // null and their internal commit is a no-op — avoiding a double-commit of the same draft id.
  const handleCommitDraft = useCallback((): PdfEditCommand | null => {
    const command = editSession.commitDraft();
    if (command) registerCommittedSourceCommand(command);
    return command;
  }, [editSession, registerCommittedSourceCommand]);

  // ---------------------------------------------------------------------------------------------
  // §9.4 in-memory candidate preview: recompose + re-render affected pages on every source-history
  // mutation (commit, undo, redo — anything that calls `editSession.publish()`).
  // ---------------------------------------------------------------------------------------------

  const renderPageRef = useRef<(n: number) => Promise<void>>(async () => {});
  const { invalidatePreview } = usePdfPreviewRecompose({
    documentPath,
    editSession,
    sourceShaRef,
    previewSeqRef,
    previewLoadingTaskRef,
    previewDocRef,
    previewPagesRef,
    renderedRef,
    renderPageRef,
  });

  // ---------------------------------------------------------------------------------------------
  // §10 save orchestration — additive PdfSavePlan registration (see registerPdfSavePlan doc above)
  // ---------------------------------------------------------------------------------------------

  const savePlan = usePdfSavePlan({
    documentPath,
    url,
    editSession,
    pdfEditor,
    handleCommitDraft,
    pageGraphsRef,
    pageGraphsGenRef,
    sourceShaRef,
    visibleRef,
    renderedRef,
    renderPageRef,
    previewSeqRef,
    invalidatePreview,
    previewLoadingTaskRef,
    previewDocRef,
    previewPagesRef,
    pendingReloadRerenderPagesRef,
    replaceDocument,
    registerPdfSavePlan,
    setSaveLocked,
  });

  // ---------------------------------------------------------------------------------------------
  // §3.2/§4 Edit-text mode: click-to-select-or-add over the whole scroll container. The frozen
  // PdfToolbar.tsx (Dispatch B) exposes one "Edit existing PDF text" toggle (`textEditActive` /
  // `onToggleTextEdit`) and no separate "Add text" tool button, so — judgment call — this single
  // mode handles both: clicking an editable island opens a `sourceEdit` draft on it; clicking empty
  // page space opens an `addText` draft anchored at the click (§1's "add text at a clicked page
  // position"); clicking an uneditable island is a no-op (viewer stays usable, §15).
  // ---------------------------------------------------------------------------------------------

  const handlePageClick = useCallback(
    async (page: number, pdfX: number, pdfY: number) => {
      setOcrCorrectionError(null);
      handleCommitDraft(); // §3.2 "Clicking another page target commits active valid draft"
      let graph = pageGraphsRef.current.get(page);
      if (!graph) {
        // A save commit or document switch may clear the cache while this inspect is in flight; its
        // result then describes the old file. Re-request once against the current file in that case.
        let generation = pageGraphsGenRef.current;
        try {
          graph = await editSession.inspectPage(page);
          if (generation !== pageGraphsGenRef.current) {
            generation = pageGraphsGenRef.current;
            graph = await editSession.inspectPage(page);
          }
        } catch (e) {
          logError("pdf-editor:inspect");
          setOcrCorrectionError(`Couldn't read this page for editing: ${errorMessage(e)}`);
          return;
        }
        if (generation !== pageGraphsGenRef.current) return; // cleared again: fail closed, no draft
        pageGraphsRef.current.set(page, graph);
        sourceShaRef.current = graph.documentSha256;
      }
      const viewport = viewportsRef.current.get(page);
      const ocrLines = ocrSourceLinesRef.current.get(page);
      if (viewport && ocrLines && ocrLines.length > 0) {
        const [viewX, viewY] = viewport.convertToViewportPoint(pdfX, pdfY);
        const hit = findOcrLineAt(ocrLines, viewX / viewport.width, viewY / viewport.height);
        if (hit) {
          setOcrCorrectionError(
            "Recognized text is available for search & copy. Editing scanned text is not supported.",
          );
          return;
        }
      }
      const hitEditable = graph.islands.find(
        (island) => island.editable && island.quads.some((q) => quadContainsPoint(q, pdfX, pdfY)),
      );
      if (hitEditable) {
        const bounds = islandPdfBounds(hitEditable);
        editSession.selectIsland({
          page,
          islandId: hitEditable.id,
          locator: hitEditable.locator,
          rect: bounds,
        });
        editSession.startSourceEditDraft({
          id: crypto.randomUUID(),
          rect: bounds,
          text: hitEditable.text,
          appearance: hitEditable.appearance,
          // Conservative default: no expansion beyond the island's own painted box. True available
          // space (including verified empty neighboring page area, §8) is backend-validated at
          // Save time regardless — this only affects the live overflow hint, not correctness.
          availableBounds: bounds,
        });
        return;
      }
      const hitUneditable = graph.islands.some(
        (island) => !island.editable && island.quads.some((q) => quadContainsPoint(q, pdfX, pdfY)),
      );
      if (hitUneditable) return; // "Unsupported content remains viewable & unchanged" (§3.2)
      editSession.startAddTextDraft({
        id: crypto.randomUUID(),
        page,
        anchor: { x: pdfX, y: pdfY },
        rect: {
          x: pdfX,
          y: pdfY,
          width: 160,
          height: DEFAULT_TEXT_APPEARANCE.sizePt * 1.4,
        },
        appearance: DEFAULT_TEXT_APPEARANCE,
      });
    },
    [editSession, handleCommitDraft],
  );

  useEffect(() => {
    const root = scrollRef.current;
    if (!root || editSession.session.mode !== "edit" || editSession.session.tool !== "selectText") {
      return;
    }
    const onClick = (e: MouseEvent) => {
      const slot = (e.target as HTMLElement | null)?.closest<HTMLElement>("[data-page]");
      if (!slot) return;
      // The inline editor and toolbar controls sit "inside edit interaction scope" (§3.2) — a click
      // on the editor's own contenteditable must not be reinterpreted as a page hit-test.
      if ((e.target as HTMLElement | null)?.closest("[data-pdf-inline-text-editor]")) return;
      const page = Number(slot.dataset.page);
      const viewport = viewportsRef.current.get(page);
      if (!viewport) return;
      const rect = slot.getBoundingClientRect();
      const [pdfX, pdfY] = viewport.convertToPdfPoint(e.clientX - rect.left, e.clientY - rect.top);
      e.preventDefault();
      e.stopPropagation();
      void handlePageClick(page, pdfX, pdfY);
    };
    root.addEventListener("click", onClick, true);
    return () => root.removeEventListener("click", onClick, true);
  }, [editSession.session.mode, editSession.session.tool, handlePageClick]);

  const setTextEditActive = useCallback(
    (next: boolean) => {
      if (next) {
        if (!pdfEditor.ensureReady()) return;
        editSession.enterEditMode();
        editSession.setTool("selectText");
        // Mutually exclusive with the annotation-editor tools so a click unambiguously means
        // "select/add text" rather than also placing a FreeText/Ink/Highlight/Stamp editor.
        if (pdfEditor.mode !== "none") pdfEditor.setMode("none");
      } else {
        handleCommitDraft(); // §3.2 "Done": commit active valid draft, then return to view mode.
        editSession.exitEditMode();
      }
    },
    [editSession, handleCommitDraft, pdfEditor],
  );

  // Leaving edit-text mode is the reverse exclusion: picking an annotation tool while mid source
  // edit commits & exits, matching the old pdfTextEdit <-> pdfEditor mutual-exclusion contract.
  const handleSetAnnotationMode = useCallback(
    (mode: Parameters<typeof pdfEditor.setMode>[0]) => {
      if (mode !== "none" && editSession.session.mode === "edit") {
        handleCommitDraft();
        editSession.exitEditMode();
      }
      pdfEditor.setMode(mode);
    },
    [editSession, handleCommitDraft, pdfEditor],
  );

  // Wave 2 Read mode: drop every active edit surface — annotation mode back to "none", and the
  // source-text session exits through setTextEditActive(false) so an in-flight draft COMMITS
  // rather than being silently discarded.
  useEffect(() => {
    if (!readOnly) return;
    if (pdfEditor.mode !== "none") pdfEditor.setMode("none");
    if (editSession.session.mode === "edit") setTextEditActive(false);
    else editSession.cancelDraft();
  }, [readOnly, pdfEditor, editSession, setTextEditActive]);

  // §6.2: Cmd/Ctrl+Z outside text composition drives pdf.js's unified command history (annotations
  // + source edits together); Cmd/Ctrl+Z while a native text input/contenteditable is focused keeps
  // that element's own native undo (we simply don't intercept the key in that case).
  useEffect(() => {
    if (!active) return;
    // Matched by the shared registry (Ctrl or Meta + Z, Ctrl + Y; any extra modifiers, IME state and
    // already-prevented events are accepted as before) and never prevents the default action.
    const editableHistory = createShortcutRegistry();
    editableHistory.register({
      id: "pdf.editable-history",
      label: "Keep native undo in editable targets",
      chord: ["ctrlormeta+z", "ctrl+y"],
      hidden: true,
      preventDefault: false,
      ignoreExtraModifiers: true,
      allowComposing: true,
      allowDefaultPrevented: true,
      allowInInput: true,
      run: (event) => {
        // This listener runs after the editable target has handled its native undo, but before any
        // window-bubble listener (including pdf.js's). Keep default action untouched while suppressing
        // pdf.js's second history command for contenteditable/input targets.
        const target = event.target as HTMLElement | null;
        if (!target?.closest?.("input, textarea, [contenteditable='true']")) return false;
        event.stopImmediatePropagation();
      },
    });
    const onEditableHistoryKeyDown = (event: KeyboardEvent) => {
      editableHistory.handle(event);
    };
    const undo = () => {
      if (readOnly) return;
      if (!saveLockedRef.current && !savePlan.isReloadRequired())
        pdfEditor.uiManagerRef.current?.undo();
    };
    const redo = () => {
      if (readOnly) return;
      if (!saveLockedRef.current && !savePlan.isReloadRequired())
        pdfEditor.uiManagerRef.current?.redo();
    };
    // The chords are matched by a registry that this capture-phase listener delegates to, so the
    // listener still runs before pdf.js's own window listener. The old handler accepted Ctrl, Meta
    // or both with any Shift or Alt, skipped an already-prevented event and ignored IME state.
    const historyShortcuts = createShortcutRegistry();
    historyShortcuts.register({
      id: "pdf.history",
      label: "Undo or redo",
      chord: ["ctrlormeta+z", "ctrl+y"],
      hidden: true,
      ignoreExtraModifiers: true,
      allowComposing: true,
      run: (event) => {
        const target = event.target as HTMLElement | null;
        if (target?.closest?.("input, textarea, [contenteditable='true']")) return false;
        // Read mode: the UIManager from a prior Markup session is still mounted — swallow the
        // shortcut here so neither this pane nor pdf.js's own window listener mutates history.
        // pdf.js's AnnotationEditorUIManager also installs a window keydown listener. This pane
        // owns history routing, so stop its listener after handling the shortcut; otherwise one
        // keystroke can enter a stale/destroyed CommandManager during tab/document transitions.
        event.stopImmediatePropagation();
        if (readOnly) return;
        if (event.shiftKey || (event.ctrlKey && event.key.toLowerCase() === "y")) redo();
        else undo();
      },
    });
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.defaultPrevented) return;
      if (saveLockedRef.current || savePlan.isReloadRequired()) {
        event.preventDefault();
        event.stopImmediatePropagation();
        return;
      }
      historyShortcuts.handle(event);
    };
    window.addEventListener(documentEvent("undo"), undo);
    window.addEventListener(documentEvent("redo"), redo);
    window.addEventListener("keydown", onKeyDown, true);
    document.addEventListener("keydown", onEditableHistoryKeyDown);
    return () => {
      window.removeEventListener(documentEvent("undo"), undo);
      window.removeEventListener(documentEvent("redo"), redo);
      window.removeEventListener("keydown", onKeyDown, true);
      document.removeEventListener("keydown", onEditableHistoryKeyDown);
    };
  }, [active, readOnly, pdfEditor.uiManagerRef, savePlan]);

  // A viewport transform rebuilds text-layer DOM & invalidates cached geometry. Discard (not
  // commit — matches the prior pdfTextEdit behavior) any transient draft instead of leaving a
  // detached inline editor over stale coordinates.
  const textEditViewportRef = useRef({ scale, rotation });
  useEffect(() => {
    const previous = textEditViewportRef.current;
    textEditViewportRef.current = { scale, rotation };
    if (previous.scale !== scale || previous.rotation !== rotation) {
      editSession.cancelDraft();
    }
  }, [scale, rotation, editSession]);

  useEffect(() => {
    onDirtyChange?.(pdfEditor.dirty || editSession.dirty);
  }, [editSession.dirty, pdfEditor.dirty, onDirtyChange]);

  useEffect(() => {
    if (!registerPdfSave) return;
    registerPdfSave(() => pdfEditor.saveBytes());
    return () => registerPdfSave(null);
  }, [registerPdfSave, pdfEditor.saveBytes]);

  // Keep the UI manager's viewParameters (used to place/scale new + existing editors) in step
  // with the viewer's own scale/rotation/page state.
  useEffect(() => {
    pdfEditor.syncViewState({ scale, rotation, page: current });
  }, [scale, rotation, current, pdfEditor.syncViewState]);

  const goToPage = useCallback(
    (n: number) => {
      const p = Math.max(1, Math.min(numPages, n));
      slotRefs.current.get(p)?.scrollIntoView({ block: "start" });
      setCurrent(p);
    },
    [numPages],
  );

  // Link service for the per-page annotation layer. The service object must stay stable (it is
  // captured by renderPage, whose dependency list must not grow), so it delegates through a ref
  // that an effect below keeps pointed at the live navigation callbacks.
  const linkActionsRef = useRef<PdfLinkActions>({
    goToDest: () => {},
    goToPage: () => {},
    currentPage: () => 1,
    pagesCount: () => 1,
  });
  const linkServiceRef = useRef<ReturnType<typeof createPdfLinkService> | null>(null);
  if (!linkServiceRef.current) {
    linkServiceRef.current = createPdfLinkService({
      goToDest: (dest) => linkActionsRef.current.goToDest(dest),
      goToPage: (n) => linkActionsRef.current.goToPage(n),
      currentPage: () => linkActionsRef.current.currentPage(),
      pagesCount: () => linkActionsRef.current.pagesCount(),
      openExternal,
    });
  }

  // Find lives in its own hook (owns the match model + committed query). It returns the committed
  // query (highlight re-render dependency) and a ref the per-page render reads to apply <mark>s.
  const { queryRef, optionsRef, setPageText, invalidatePageText, rehighlight } = usePdfSearch({
    url,
    pdfRef,
    slotRefs,
    scrollRef,
    goToPage,
    registerSearchProvider,
  });

  const pdfOcr = usePdfOcr({
    documentId: documentPath,
    pdfDoc,
    pdfRef,
    current,
    rotation,
    setPageText,
    invalidatePageText,
    rehighlight,
  });
  const appliedInitialActionRef = useRef(false);
  const [initialActionError, setInitialActionError] = useState<string | null>(null);
  useEffect(() => {
    if (!initialEditingAction || appliedInitialActionRef.current || !active || !pdfDoc || !base) {
      return;
    }
    // Restore the native reader's page before tools/OCR use the editor's current page.
    const target = Math.max(1, Math.min(numPages, effectiveInitialView?.page ?? 1));
    if (current !== target) return;
    if (
      initialEditingAction.kind !== "ocr" &&
      initialEditingAction.kind !== "ocr-document" &&
      !pdfEditor.ensureReady()
    )
      return;
    appliedInitialActionRef.current = true;
    switch (initialEditingAction.kind) {
      case "annotation":
        handleSetAnnotationMode(initialEditingAction.mode);
        break;
      case "text":
        setTextEditActive(true);
        break;
      case "stamp":
        // A cold editor may still be rendering this page when its image picker completes.
        void pdfEditor
          .addImageStamp(initialEditingAction.file, 15000)
          .then((added) => {
            if (!added) {
              const message =
                "Couldn't add this image. Wait for the page to load & select the image again.";
              logError("pdf-editor:initial-stamp");
              setInitialActionError(message);
            }
          })
          .catch((error: unknown) => {
            logError("pdf-editor:initial-stamp");
            setInitialActionError(`Couldn't add this image: ${errorMessage(error)}`);
          });
        break;
      case "ocr":
        // Explicit native request precedes the editor's text-only eligibility check.
        void pdfOcr.recognizeCurrentPage(true);
        break;
      case "ocr-document":
        void pdfOcr.recognizeDocument();
        break;
    }
  }, [
    active,
    base,
    current,
    effectiveInitialView?.page,
    handleSetAnnotationMode,
    initialEditingAction,
    numPages,
    pdfDoc,
    pdfEditor,
    pdfOcr,
    setTextEditActive,
  ]);
  const effectiveOcrLinesByPage = useMemo(
    () => applyOcrCorrections(pdfOcr.linesByPage, editSession.session.sourceCommands),
    [pdfOcr.linesByPage, editSession.session.sourceCommands],
  );
  ocrSourceLinesRef.current = pdfOcr.linesByPage;
  useEffect(() => {
    for (const [page, lines] of effectiveOcrLinesByPage) {
      setPageText(page, lines.map((line) => line.text).join(" "));
    }
  }, [effectiveOcrLinesByPage, setPageText]);

  const ocrErrorMessage =
    ocrCorrectionError ??
    (pdfOcr.currentError
      ? pdfOcr.currentErrorIsBackendUnavailable
        ? "This machine cannot recognize text in PDFs."
        : `Text recognition failed: ${pdfOcr.currentError}`
      : undefined);

  // Fit-to-width/page scale math + container-resize re-fit (rAF-coalesced).
  usePdfFit({ scrollRef, base, rotation, fitMode, setScale });

  // Measured page sizes are recorded at render time for the *current* rotation. When rotation
  // changes, those are stale — drop them so every slot falls back to the rotation-aware base
  // until its page re-renders. (Safe on initial load too: pages aren't rendered yet.)
  useEffect(() => {
    setPageSizes(new Map());
  }, [setPageSizes]);

  // --- Render a single page (canvas @ DPR + positioned text layer) ----------------
  // `queryRef.current` / `optionsRef.current` are read fresh at call time and are deliberately NOT
  // dependencies: listing them gave renderPage a new identity on every committed find query, which
  // made the IntersectionObserver effect below tear down and re-rasterize every visible page
  // (visible flicker + CPU spike on each search). usePdfSearch applies highlights in place instead.
  // biome-ignore lint/correctness/useExhaustiveDependencies: see comment above.
  const renderPage = useCallback(
    async (n: number) => {
      if (!activeRef.current) return;
      // §9.4: pages in the committed-but-unsaved candidate set render from the in-memory preview
      // document instead of the on-disk one, so unsaved source edits show as edited truth rather
      // than a DOM overlay (§3.2's "Committed, unsaved source edits display as edited").
      const pdf = (previewPagesRef.current.has(n) ? previewDocRef.current : null) ?? pdfRef.current;
      const slot = slotRefs.current.get(n);
      if (!pdf || !slot) return;
      if (renderedRef.current.has(n)) {
        if (!rasterPresentRef.current.has(n) && !rasterPendingRef.current.has(n)) {
          void rasterRenderRef.current.get(n)?.();
        }
        return;
      }

      let disposed = false;
      let renderedPage: Awaited<ReturnType<PDFDocumentProxy["getPage"]>> | null = null;
      let pageCanvas: HTMLCanvasElement | null = null;
      let renderTask: ReturnType<
        Awaited<ReturnType<PDFDocumentProxy["getPage"]>>["render"]
      > | null = null;
      let textLayer: pdfjs.TextLayer | null = null;
      // Page rasters are the dominant PDF allocation, so the census tracks each live canvas
      // and its estimated bytes rather than only counting rendered slots.
      let releaseCanvasCensus: (() => void) | null = null;
      let editorLayer: pdfjs.AnnotationEditorLayer | null = null;
      let initialRasterGeneration = 0;
      let initialRenderSettled = false;
      // `slot` (PdfViewerBody.tsx's `<div data-page>`) can also hold React-owned children —
      // `<PdfOcrLayer>` and the render-error `<div>`. EVERY node this render appends imperatively
      // (canvas, text/annotation/editor layers, and any `DrawLayer` SVG pdf.js appends later during
      // an active ink-drawing gesture, via `drawLayer.setParent` below) goes inside this one wrapper
      // instead of directly into `slot`, and cleanup removes only the wrapper itself. Previously,
      // cleanup called `slot.replaceChildren()` (wiping React's children too, then re-appending the
      // OCR layer node by hand) — mutating a React-owned child's parent behind React's back, which
      // made React's next reconciliation of that child throw `NotFoundError` from `removeChild`
      // (crashing to ErrorBoundary; reproduced live on scanned PDFs, whose slots have an OCR layer).
      // Appended once, up front (before any `await`), so cleanup can always safely remove it even if
      // a page is scrolled out mid-render, before any pdf.js layer exists yet.
      //
      // Stacking is unaffected by this extra nesting level: `layerRoot` itself is `position: static`
      // (no position/z-index/opacity/transform), so it establishes no stacking context of its own —
      // its absolutely-positioned children (canvas/text/annotation/editor layers, all z-index:auto/0
      // per pdf_viewer.css) paint exactly as if they were still direct children of `slot`. `.pdfOcrLayer`
      // (PdfOcrLayer.tsx) sets `zIndex: 2` specifically so it paints above all of those regardless of
      // DOM order/nesting, so no `insertBefore` is needed to keep it visually on top.
      //
      // `canvasWrapper`: pdf.js's Ink/Highlight `DrawLayer` only paints its `<svg class="draw">`/
      // `<svg class="highlight">` strokes when styled by pdf_viewer.css's `.canvasWrapper svg{...}`
      // rules (position:absolute + mix-blend-mode; unstyled, an svg defaults to `position: static`
      // and paints nothing visible at the stroke's coordinates). That block is entirely
      // `.canvasWrapper svg{...}` descendant rules — no box-model properties on `.canvasWrapper`
      // itself here — so this class is safe to add with zero effect on this wrapper's own layout.
      // (A second, `.pdfViewer .canvasWrapper{ overflow/width/height:100% }` rule exists in
      // pdf_viewer.css, but only under a `.pdfViewer` ancestor class this app's DOM never uses, so
      // it never applies here.)
      const layerRoot = document.createElement("div");
      layerRoot.className = "canvasWrapper";
      slot.appendChild(layerRoot);
      const rasterRoot = document.createElement("div");
      rasterRoot.className = "pdfRasterLayer";
      layerRoot.appendChild(rasterRoot);
      let rasterPainted = false;
      let rasterRepaint: Promise<void> | null = null;
      let rasterGeneration = 0;
      const dropRaster = () => {
        rasterGeneration += 1;
        rasterPainted = false;
        renderTask?.cancel();
        if (pageCanvas) {
          pageCanvas.width = pageCanvas.height = 0;
          pageCanvas.remove();
          pageCanvas = null;
        }
        releaseCanvasCensus?.();
        releaseCanvasCensus = null;
        canvasBytesRef.current.delete(n);
        rasterPresentRef.current.delete(n);
        renderedPage?.cleanup();
        renderedPage = null;
      };
      const cleanup = () => {
        if (disposed) return;
        disposed = true;
        rasterPendingRef.current.delete(n);
        rasterRenderRef.current.delete(n);
        rasterDropRef.current.delete(n);
        textLayer?.cancel();
        editorLayer?.destroy();
        editorLayer = null;
        dropRaster();
        layerRoot.remove();
        if (renderedRef.current.get(n) === cleanup) renderedRef.current.delete(n);
      };
      renderedRef.current.set(n, cleanup);
      rasterPendingRef.current.add(n);
      const requestRaster = async (): Promise<void> => {
        if (disposed || !activeRef.current || rasterPainted || !initialRenderSettled) return;
        if (rasterRepaint) {
          const pending = rasterRepaint;
          await pending;
          if (!disposed && activeRef.current && !rasterPresentRef.current.has(n)) {
            await requestRaster();
          }
          return;
        }
        rasterPendingRef.current.add(n);
        const currentPdf =
          (previewPagesRef.current.has(n) ? previewDocRef.current : null) ?? pdfRef.current;
        const viewport = viewportsRef.current.get(n);
        if (!currentPdf || !viewport) {
          rasterPendingRef.current.delete(n);
          return;
        }
        const previousRender = renderTailRef.current;
        let releaseRender!: () => void;
        renderTailRef.current = new Promise<void>((resolve) => {
          releaseRender = resolve;
        });
        const startedAt = performance.now();
        const repaint = (async () => {
          const generation = rasterGeneration;
          let page: Awaited<ReturnType<PDFDocumentProxy["getPage"]>> | null = null;
          let keepaliveRelease: (() => void) | null = null;
          try {
            await previousRender;
            if (disposed || !activeRef.current || generation !== rasterGeneration) return;
            page = await currentPdf.getPage(n);
            if (disposed || !activeRef.current || generation !== rasterGeneration) return;
            const dpr = boundedCanvasDpr(viewport, window.devicePixelRatio || 1);
            const canvas = document.createElement("canvas");
            pageCanvas = canvas;
            canvas.width = Math.floor(viewport.width * dpr);
            canvas.height = Math.floor(viewport.height * dpr);
            canvas.style.width = `${viewport.width}px`;
            canvas.style.height = `${viewport.height}px`;
            canvas.className = "absolute left-0 top-0";
            rasterRoot.appendChild(canvas);
            canvasBytesRef.current.set(n, canvas.width * canvas.height * 4);
            releaseCanvasCensus = acquireResource({
              kind: "pdf-page-canvas",
              documentPath: documentPathRef.current,
              format: "pdf",
              estimatedBytes: canvas.width * canvas.height * 4,
            });
            const ctx = canvas.getContext("2d");
            if (!ctx) return;
            const renderIntent = isPdfBenchEnabled() ? "print" : "display";
            keepaliveRelease = renderIntent === "display" ? acquirePdfRenderKeepalive() : null;
            renderTask = page.render({
              canvas,
              canvasContext: ctx,
              viewport,
              transform: dpr !== 1 ? [dpr, 0, 0, dpr, 0, 0] : undefined,
              intent: renderIntent,
            });
            await renderTask.promise;
            if (disposed || !activeRef.current || generation !== rasterGeneration) return;
            rasterPainted = true;
            rasterPresentRef.current.add(n);
            logPerf("pdf:page-render", startedAt);
          } catch (error) {
            if (!disposed && generation === rasterGeneration) {
              const message = String((error as { message?: unknown })?.message ?? error);
              setRenderErrors((prev) => {
                const next = new Map(prev);
                next.set(n, message);
                return next;
              });
            }
          } finally {
            rasterPendingRef.current.delete(n);
            keepaliveRelease?.();
            renderTask = null;
            page?.cleanup();
            releaseRender();
            if (!rasterPainted && generation === rasterGeneration) dropRaster();
          }
        })();
        rasterRepaint = repaint;
        try {
          await repaint;
        } finally {
          if (rasterRepaint === repaint) rasterRepaint = null;
        }
      };
      rasterRenderRef.current.set(n, requestRaster);
      rasterDropRef.current.set(n, () => {
        // A partially built page has no complete editor layer to preserve. Remove its queued
        // work/DOM so activation builds every layer again; only settled pages keep editor state.
        if (!initialRenderSettled) cleanup();
        else dropRaster();
      });

      const previousRender = renderTailRef.current;
      let releaseRender!: () => void;
      renderTailRef.current = new Promise<void>((resolve) => {
        releaseRender = resolve;
      });
      try {
        await previousRender;
        if (disposed) return;
        if (!activeRef.current) {
          cleanup();
          return;
        }
        initialRasterGeneration = rasterGeneration;
        const page = await pdf.getPage(n);
        renderedPage = page;
        if (disposed || rasterGeneration !== initialRasterGeneration) {
          page.cleanup();
          return;
        }
        if (!activeRef.current) {
          page.cleanup();
          cleanup();
          return;
        }
        const pageRotation = viewportRotation(page.rotate, rotation);
        const viewport = page.getViewport({ scale, rotation: pageRotation });
        viewportsRef.current.set(n, viewport);
        slot.style.width = `${viewport.width}px`;
        slot.style.height = `${viewport.height}px`;
        slot.style.setProperty("--scale-factor", String(scale));
        // pdf.js 6.x sizes the annotation layer with these three vars, normally defined on the
        // library's own `.pdfViewer .page` selector — our slots aren't inside one, so set them
        // here or the layer computes 0x0 and every link collapses to an unclickable point.
        slot.style.setProperty("--total-scale-factor", String(scale * (page.userUnit ?? 1)));
        slot.style.setProperty("--scale-round-x", "1px");
        slot.style.setProperty("--scale-round-y", "1px");

        // Record the page's intrinsic size so its slot reserves correct space even before render —
        // this is what stops mixed-orientation PDFs from jumping as you scroll (D1).
        const iw = viewport.width / scale;
        const ih = viewport.height / scale;
        setPageSizes((prev) => {
          const ex = prev.get(n);
          if (ex && Math.abs(ex.w - iw) < 0.5 && Math.abs(ex.h - ih) < 0.5) return prev;
          const next = new Map(prev);
          next.set(n, { w: iw, h: ih });
          return next;
        });

        const dpr = boundedCanvasDpr(viewport, window.devicePixelRatio || 1);
        const canvas = document.createElement("canvas");
        pageCanvas = canvas;
        canvas.width = Math.floor(viewport.width * dpr);
        canvas.height = Math.floor(viewport.height * dpr);
        canvas.style.width = `${viewport.width}px`;
        canvas.style.height = `${viewport.height}px`;
        canvas.className = "absolute left-0 top-0";
        rasterRoot.appendChild(canvas);
        canvasBytesRef.current.set(n, canvas.width * canvas.height * 4);
        releaseCanvasCensus = acquireResource({
          kind: "pdf-page-canvas",
          documentPath: documentPathRef.current,
          format: "pdf",
          estimatedBytes: canvas.width * canvas.height * 4,
        });

        const ctx = canvas.getContext("2d");
        if (!ctx) return;
        const renderStart = performance.now();
        const renderIntent = isPdfBenchEnabled() ? "print" : "display";
        const releaseKeepalive = renderIntent === "display" ? acquirePdfRenderKeepalive() : null;
        try {
          renderTask = page.render({
            canvas,
            canvasContext: ctx,
            viewport,
            transform: dpr !== 1 ? [dpr, 0, 0, dpr, 0, 0] : undefined,
            // Bench renders use print intent for deterministic headless timing. Installed display
            // renders keep display semantics; scoped keepalive advances pdf.js only when WKWebView
            // pauses animation frames behind another app.
            intent: renderIntent,
          });
          await renderTask.promise;
        } finally {
          releaseKeepalive?.();
        }
        if (disposed || rasterGeneration !== initialRasterGeneration) return;
        rasterPainted = true;
        rasterPresentRef.current.add(n);
        rasterPendingRef.current.delete(n);
        markPdfBenchFirstCanvasPaint(documentPathRef.current, n);
        logPerf("pdf:page-render", renderStart);
        // An expensive render marks the page for scroll-out keep-alive; a page
        // that later renders fast again (e.g. after a zoom-out shrank the raster
        // work) demotes itself so the slot goes back to normal unloading.
        if (performance.now() - renderStart > COSTLY_RENDER_MS) {
          costlyRef.current.set(n, performance.now());
        } else {
          costlyRef.current.delete(n);
        }

        const textDiv = document.createElement("div");
        textDiv.className = "textLayer";
        layerRoot.appendChild(textDiv);
        textLayer = new pdfjs.TextLayer({
          textContentSource: page.streamTextContent(),
          container: textDiv,
          viewport,
        });
        await textLayer.render();
        if (disposed || rasterGeneration !== initialRasterGeneration) return;

        // Apply find highlights to this freshly-rendered text layer.
        const q = queryRef.current;
        if (q)
          textDiv
            .querySelectorAll<HTMLElement>("span")
            .forEach((s) => highlightSpan(s, q, optionsRef.current));

        // Annotation layer: interactive DOM (clickable links, note popups) on top of the canvas.
        // Visual appearances are already painted into the canvas by page.render; this layer only
        // adds the hit targets. `annotationEditorUIManager`/`annotationStorage` are wired (Stage 1)
        // so existing annotations interoperate with the editor layer below; `renderForms` stays off
        // — form-field editing is out of Stage 1's scope.
        const annotations = await page.getAnnotations({ intent: "display" });
        if (disposed || rasterGeneration !== initialRasterGeneration) return;
        const layerViewport = viewport.clone({ dontFlip: true });
        const uiManager = pdfEditor.uiManagerRef.current;
        let annotationLayer: pdfjs.AnnotationLayer | null = null;
        if (annotations.length > 0) {
          const annotationDiv = document.createElement("div");
          annotationDiv.className = "annotationLayer";
          layerRoot.appendChild(annotationDiv);
          annotationLayer = new pdfjs.AnnotationLayer({
            div: annotationDiv,
            accessibilityManager: null,
            annotationCanvasMap: null,
            annotationEditorUIManager: uiManager,
            page,
            viewport: layerViewport,
            structTreeLayer: NULL_STRUCT_TREE_LAYER,
            commentManager: null,
            linkService: linkServiceRef.current,
            annotationStorage: pdf.annotationStorage,
          });
          await annotationLayer.render({
            annotations,
            div: annotationDiv,
            page,
            viewport: layerViewport,
            // The declared param type is pdf.js's own web-viewer class; the layer only calls the
            // IPDFLinkService surface our structural service implements (see @rightkit/pdf-ui linkService).
            linkService: linkServiceRef.current as unknown as Parameters<
              pdfjs.AnnotationLayer["render"]
            >[0]["linkService"],
            renderForms: false,
            imageResourcesPath: "",
          });
          if (disposed || rasterGeneration !== initialRasterGeneration) return;
        }

        // Editor layer: the interactive surface for FreeText/Ink/Highlight/Stamp editors (Stage 1).
        // Built even on pages with zero existing annotations — a new editor can be placed on any
        // page. `uiManager` is null until usePdfEditor finishes constructing it for this document
        // (one render tick after pdfDoc loads), so an early page render just skips it; it re-renders
        // on the next scroll-driven visibility pass once the manager exists (editor layers are
        // otherwise cheap/idempotent to add — the IntersectionObserver re-invokes renderPage for
        // any page whose renderedRef entry was cleared, and a fresh document load always clears it).
        if (uiManager) {
          // The interactive editor layer is optional: it must never be able to blank the base page.
          // If its (viewer-internal, environment-sensitive) construction/render throws, degrade to a
          // page with no editor layer rather than failing the whole page render in the outer catch.
          try {
            const editorDiv = document.createElement("div");
            editorDiv.className = "annotationEditorLayer";
            layerRoot.appendChild(editorDiv);
            const drawLayer = new pdfjs.DrawLayer({ pageIndex: n - 1 });
            // Ink-drawing SVGs pdf.js appends later (mid-gesture, well after this render call
            // returns) must land inside `layerRoot` too, not directly in `slot` — see `layerRoot`'s
            // doc comment above.
            drawLayer.setParent(layerRoot);
            // `NULL_STRUCT_TREE_LAYER`, not `null` — see its own doc comment (top of file) for the
            // exact crash a bare `null` here causes the instant a page's annotations include a
            // Stamp. structTreeLayer is declared non-nullable in pdfjs-dist's generated types
            // regardless, so cast this one boundary rather than fight the (viewer-internal-only)
            // types.
            editorLayer = new pdfjs.AnnotationEditorLayer({
              uiManager,
              pageIndex: n - 1,
              div: editorDiv,
              structTreeLayer: NULL_STRUCT_TREE_LAYER,
              accessibilityManager: undefined,
              annotationLayer: annotationLayer ?? undefined,
              drawLayer,
              textLayer: createPdfEditorTextLayerAdapter(textDiv),
              viewport: layerViewport,
              l10n: uiManager.l10n,
            } as unknown as ConstructorParameters<typeof pdfjs.AnnotationEditorLayer>[0]);
            await editorLayer.render({ viewport: layerViewport });
            if (disposed || rasterGeneration !== initialRasterGeneration) return;
          } catch (editorError) {
            if (benchmarkEnabled) markPdfBenchError(documentPathRef.current, editorError);
            // Previously silent (no console/diagnostics trail at all) — degrading to "no editor
            // layer" is the right recovery, but a page silently losing its editor layer (ink/
            // highlight/stamp become uninteractable, with no visible error) still needs to be
            // diagnosable after the fact.
            logError("pdf-editor:layer-init");
            editorLayer = null;
          }
        }
      } catch (e) {
        if (!disposed && rasterGeneration === initialRasterGeneration) {
          if (benchmarkEnabled) markPdfBenchError(documentPathRef.current, e);
          const message = String((e as { message?: unknown })?.message ?? e);
          setRenderErrors((prev) => {
            const next = new Map(prev);
            next.set(n, message);
            return next;
          });
        }
      } finally {
        // Settled page rasters & editor DOM/history have separate ownership from pdf.js's
        // operator lists/decoded image objects. Release those caches after editing renders too;
        // cleanup defers until pending render tasks finish & leaves editor layers intact.
        rasterPendingRef.current.delete(n);
        renderedPage?.cleanup();
        renderedPage = null;
        releaseRender();
        if (!rasterPainted && rasterGeneration === initialRasterGeneration) dropRaster();
        initialRenderSettled = true;
        if (!disposed && activeRef.current && !rasterPresentRef.current.has(n)) {
          void requestRaster();
        }
      }
    },
    [benchmarkEnabled, scale, rotation, setPageSizes, setRenderErrors, pdfEditor.uiManagerRef],
  );
  renderPageRef.current = renderPage;

  // Hidden pure-view panes keep their PDFDocumentProxy/worker/search/OCR state, but their page
  // rasters are disposable. Existing cleanup cancels queued/in-flight renders, zeroes detached
  // canvases, releases census bytes & calls page.cleanup(). Initialized editor panes retain their
  // layers/history/UIManager, but drop only base canvases and repaint them after activation.
  useEffect(() => {
    if (active) {
      if (pdfEditor.uiManagerRef.current) {
        renderedRef.current.forEach((_cleanup, n) => {
          if (!rasterPresentRef.current.has(n)) {
            void rasterRenderRef.current.get(n)?.();
          }
        });
      }
      return;
    }
    visibleRef.current.clear();
    costlyRef.current.clear();
    if (pdfEditor.uiManagerRef.current) {
      rasterDropRef.current.forEach((drop) => drop());
    } else {
      renderedRef.current.forEach((cleanup) => cleanup());
    }
  }, [active, pdfEditor.uiManagerRef]);

  // The UIManager finishes constructing one tick after pdfDoc loads (see usePdfEditor.ts), which
  // can land after the first visible pages already rendered without an editor layer. Re-render
  // whatever is currently on-screen once it's ready so those pages pick one up — pure pages
  // (already covered) are unaffected; this is a one-time catch-up per document load. Also drains
  // `pendingReloadRerenderPagesRef` (set by `buildSavePlan`'s `commit`, see its own comment) — a
  // post-commit reload's affected pages, deliberately NOT rendered synchronously right after
  // `replaceDocument`, precisely so their `AnnotationEditorLayer`s are only ever built here, after
  // `usePdfEditor.ts` has already rebuilt the UIManager for the fresh `pdfDoc`.
  //
  // Also keyed on `pdfDoc` itself (not just `pdfEditor.ready`): a post-commit reload
  // (`replaceDocument`) can swap `pdfDoc` while `ready` stays `true` the whole time (usePdfEditor.ts
  // batches `setReady(false)` then `setReady(true)` within its OWN single `[pdfDoc]` effect, so
  // `ready`'s rendered value never observably changes across the swap, and a boolean-only dependency
  // never re-fires this effect). This matters even with the synchronous render removed from
  // `commit`: without re-firing here too, a reload would leave affected pages permanently
  // unrendered whenever `ready` happens to already be `true` across the swap.
  //
  // History: an earlier version of this fix still called `renderPageRef.current(n)` synchronously
  // from `commit` right after `replaceDocument`, racing THIS effect. Both executions could complete
  // their own async `AnnotationEditorLayer` construction in either order, and whichever called the
  // UIManager's `addLayer()` LAST won that page's slot in the UIManager's registry — even when it
  // was the earlier (by-then-detached) attempt. The result: a real, correctly-styled `.disabled`/
  // `.nonEditing` class removal (from `AnnotationEditorLayer.enable()`, called by `addLayer()`
  // itself) with NO further update ever reaching the DOM-attached layer — a plausible-looking but
  // permanently `hidden` (0×0) editor layer, with no console error at all. `commit` no longer
  // renders synchronously (see its own comment), which removes this race by construction.
  // biome-ignore lint/correctness/useExhaustiveDependencies: catch-up runs only when manager becomes ready or the document identity changes; renderPage changes with ordinary view state.
  useEffect(() => {
    const pendingReloadPages = pendingReloadRerenderPagesRef.current;
    // React's ready value can still describe the old manager during a save reload. Check the
    // live ref, and initialize the replacement before consuming its pending pages.
    if (pendingReloadPages && !pdfEditor.ensureReady()) return;
    if (!pdfEditor.uiManagerRef.current) return;
    pendingReloadRerenderPagesRef.current = null;
    if (pendingReloadPages) {
      for (const n of pendingReloadPages) {
        if (visibleRef.current.has(n)) continue; // covered by the loop below already
        renderedRef.current.get(n)?.();
        void renderPage(n);
      }
    }
    for (const n of visibleRef.current) {
      renderedRef.current.get(n)?.();
      void renderPage(n);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pdfEditor.ready, pdfDoc]);

  // Native transport measurements must not depend on WKWebView's occlusion-throttled observer.
  // Render page 1 explicitly; renderedRef keeps this idempotent with normal visible-page delivery.
  useEffect(() => {
    if (!benchmarkEnabled || !base || numPages <= 0) return;
    benchmarkRenderRef.current.add(1);
    void renderPage(1).finally(() => benchmarkRenderRef.current.delete(1));
  }, [base, benchmarkEnabled, numPages, renderPage]);

  // The dev-only PDF benchmark asks the live viewer to visit a late page after first paint. Force
  // that exact page through the same renderer as a visible intersection: WKWebView may throttle
  // IntersectionObserver while an unattended benchmark window is occluded, which otherwise turns
  // transport timing into a foreground-focus measurement. Keep ownership on this mounted document
  // so a stale pane cannot navigate or render a later tab.
  useEffect(() => {
    registerPdfBenchLatePageNavigator((requested) => {
      if (numPages <= 0 || !Number.isFinite(requested)) return null;
      const page = Math.max(1, Math.min(numPages, Math.trunc(requested)));
      if (page !== current) goToPage(page);
      benchmarkRenderRef.current.add(page);
      void renderPage(page).finally(() => benchmarkRenderRef.current.delete(page));
      return page;
    });
    return () => registerPdfBenchLatePageNavigator(null);
  }, [current, goToPage, numPages, renderPage]);

  // --- Virtualization: render pages near the viewport, drop the rest --------------
  // NB: committedQuery is intentionally NOT a dependency. Tearing down + re-rasterizing every
  // rendered page on each query change caused a visible page jitter while typing in find. Highlights
  // are now applied in place by usePdfSearch (and by renderPage for pages that scroll in fresh).
  useEffect(() => {
    const reactivated = active && !previousActiveRef.current;
    previousActiveRef.current = active;
    const root = scrollRef.current;
    if (!root || !base) return;
    if (!active) return;
    // Activation must not tear down initialized editor layers: their DOM owns live annotation
    // interaction while UIManager owns pending edits/history. Pure-view panes were already cleaned
    // by the inactive effect above, so only those need a fresh raster pass here.
    const preserveEditorLayers = reactivated && pdfEditor.ready;
    if (!preserveEditorLayers) renderedRef.current.forEach((cleanup) => cleanup());

    // Render zone: drives visibility and renders pages entering the near-viewport band.
    const renderObs = new IntersectionObserver(
      (entries) => {
        for (const e of entries) {
          const n = Number((e.target as HTMLElement).dataset.page);
          if (e.isIntersecting) {
            visibleRef.current.add(n);
            void renderPage(n);
          } else {
            visibleRef.current.delete(n);
          }
        }
      },
      { root, rootMargin: RENDER_ROOT_MARGIN },
    );
    // Unload zone (wider than the render zone): a page is only unloaded once it leaves this band.
    const unloadObs = new IntersectionObserver(
      (entries) => {
        for (const e of entries) {
          if (e.isIntersecting) continue;
          const n = Number((e.target as HTMLElement).dataset.page);
          if (benchmarkRenderRef.current.has(n)) continue;
          if (costlyRef.current.has(n)) {
            // Keep expensive pages rendered across scroll-out — re-rendering
            // one costs seconds, not the ~20 ms the unload strategy assumes.
            // Bound memory by evicting the stalest kept pages beyond the byte budget.
            costlyRef.current.set(n, performance.now());
            const kept = [...costlyRef.current.keys()].filter(
              (p) => renderedRef.current.has(p) && !visibleRef.current.has(p),
            );
            kept.sort((a, b) => (costlyRef.current.get(a) ?? 0) - (costlyRef.current.get(b) ?? 0));
            let bytes = kept.reduce((total, p) => total + (canvasBytesRef.current.get(p) ?? 0), 0);
            while (bytes > MAX_KEPT_CANVAS_BYTES) {
              const oldest = kept.shift();
              if (oldest === undefined) break;
              bytes -= canvasBytesRef.current.get(oldest) ?? 0;
              renderedRef.current.get(oldest)?.();
            }
          } else {
            renderedRef.current.get(n)?.(); // unload off-screen page
          }
        }
      },
      { root, rootMargin: UNLOAD_ROOT_MARGIN },
    );
    slotRefs.current.forEach((el) => {
      renderObs.observe(el);
      unloadObs.observe(el);
    });
    if (reactivated) void renderPage(currentRef.current);
    return () => {
      renderObs.disconnect();
      unloadObs.disconnect();
    };
  }, [active, base, pdfEditor.ready, renderPage]);

  const updateCurrentPage = useCallback(() => {
    const root = scrollRef.current;
    if (!root || slotRefs.current.size === 0) return;
    const bestPage = nearestVisiblePdfPage(root, slotRefs.current, visibleRef.current, currentRef.current);
    setCurrent((prev) => (prev === bestPage ? prev : bestPage));
  }, []);

  useEffect(() => {
    const root = scrollRef.current;
    if (!root || !base) return;
    const onScroll = () => {
      if (currentRaf.current != null) return;
      currentRaf.current = requestAnimationFrame(() => {
        currentRaf.current = null;
        updateCurrentPage();
      });
    };
    root.addEventListener("scroll", onScroll, { passive: true });
    updateCurrentPage();
    return () => {
      root.removeEventListener("scroll", onScroll);
      if (currentRaf.current != null) {
        cancelAnimationFrame(currentRaf.current);
        currentRaf.current = null;
      }
    };
  }, [base, updateCurrentPage]);

  const { goToDest, commitPageDraft, zoom, commitScaleDraft, onWheelZoom, rotate } =
    usePdfViewControls({
      active,
      numPages,
      pageDraft,
      scale,
      scaleDraft,
      scrollRef,
      pdfRef,
      goToPage,
      setPageDraft,
      setPageShake,
      setScale,
      setScaleDraft,
      setFitMode,
      setRotation,
    });

  // Keep the link service's delegate pointed at the live navigation callbacks and page state.
  useEffect(() => {
    linkActionsRef.current = {
      goToDest,
      goToPage,
      currentPage: () => current,
      pagesCount: () => numPages,
    };
  }, [goToDest, goToPage, current, numPages]);

  // Restore the saved page once the document is loaded and its slots exist (run once per doc).
  useEffect(() => {
    if (restoredRef.current || !base || numPages === 0) return;
    restoredRef.current = true;
    const page = effectiveInitialView?.page;
    if (!page || page <= 1) {
      reportReadyRef.current = true;
      return;
    }
    let raf = 0;
    let tries = 0;
    const target = Math.min(page, numPages);
    const tick = () => {
      const slot = slotRefs.current.get(target);
      if (slot) {
        slot.scrollIntoView({ block: "start" });
        setCurrent(target);
        reportReadyRef.current = true;
        return;
      }
      if (tries++ < 30) raf = requestAnimationFrame(tick);
      else reportReadyRef.current = true;
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  }, [base, numPages, effectiveInitialView?.page]);

  // Report page/zoom/fit/rotation upward so the app can persist it (debounced there). Gated until
  // the initial restore ran so we don't immediately overwrite the saved position with the default.
  useEffect(() => {
    if (benchmarkEnabled || !reportReadyRef.current || !onViewState) return;
    onViewState({ page: current, scale, fit: fitMode, rotation });
  }, [benchmarkEnabled, current, scale, fitMode, rotation, onViewState]);

  if (!url) {
    return (
      <div className="flex h-full items-center justify-center text-sm text-muted-2">
        Open via the desktop app.
      </div>
    );
  }
  if (error) return <div className="m-4 text-sm text-copper-hi">PDF error: {error}</div>;

  const draft = editSession.session.draft;
  const draftSlot = draft ? slotRefs.current.get(draft.page) : null;
  const draftViewport = draft ? viewportsRef.current.get(draft.page) : null;
  const draftScreenRect =
    draft && draftViewport ? pdfRectToViewport(draft.rect, draftViewport) : null;
  // Existing-text edits type over glyphs that are still painted on the canvas underneath, so the
  // editor paints a background sampled from that canvas (white when the area isn't uniform) to hide
  // them; fresh add-text drafts sit on empty page space and stay transparent.
  let draftCoverColor: PdfColor | null = null;
  if (draft?.kind === "sourceEdit" && draftSlot && draftScreenRect) {
    const canvas = draftSlot.querySelector("canvas");
    draftCoverColor =
      (canvas && sampleCanvasRectBackground(canvas, draftScreenRect, draftSlot.clientWidth)) ??
      WHITE_COVER;
  }

  return (
    <div className="relative flex h-full flex-col">
      {saveLocked && (
        <div role="status" className="px-3 py-2 text-sm">
          Saving PDF…
        </div>
      )}
      {savePlan.isReloadRequired() && (
        <div role="alert" className="px-3 py-2 text-sm">
          {savePlan.hasPreservedEdits()
            ? "PDF saved. Newer edits remain unsaved; automatic reload paused."
            : "PDF saved. Reload saved revision before editing."}
          <button
            type="button"
            disabled={saveLocked || savePlan.hasPreservedEdits()}
            onClick={() => void savePlan.retryReload()}
          >
            Retry reload
          </button>
        </div>
      )}
      <div className="contents" inert={saveLocked || savePlan.isReloadRequired()}>
        {initialActionError && (
          <div role="alert" className="px-3 py-2 text-sm text-copper-hi">
            {initialActionError}
          </div>
        )}
        <PdfToolbar
          panel={panel}
          hasOutline={!!outline && outline.length > 0}
          current={current}
          numPages={numPages}
          pageDraft={pageDraft}
          pageShake={pageShake}
          scaleDraft={scaleDraft}
          fitMode={fitMode}
          onTogglePanel={(p) => setPanel((cur) => (cur === p ? null : p))}
          onPageDraftChange={setPageDraft}
          onCommitPageDraft={commitPageDraft}
          onZoom={zoom}
          onScaleDraftChange={setScaleDraft}
          onCommitScaleDraft={commitScaleDraft}
          onSetFit={setFitMode}
          onRotate={rotate}
          ocrAvailable={pdfOcr.ocrAvailable}
          currentPageScanned={pdfOcr.currentPageScanned}
          ocrState={pdfOcr.currentPageState.status}
          ocrErrorMessage={ocrErrorMessage}
          // OCR adds searchable overlays without changing the file, so Live Text stays in Read.
          onRecognize={() => void pdfOcr.recognizeCurrentPage()}
          documentOcrState={pdfOcr.documentOcrState}
          onRecognizeDocument={() => void pdfOcr.recognizeDocument()}
          findBar={findBar}
          findMode={findMode}
          onRequestFind={onRequestFind}
          onRequestReplace={onRequestReplace}
          editorMode={pdfEditor.mode}
          editorReady={!!pdfDoc}
          editorDirty={pdfEditor.dirty}
          onSetEditorMode={readOnly ? undefined : handleSetAnnotationMode}
          freeTextSize={pdfEditor.freeTextSize}
          onFreeTextSizeChange={pdfEditor.setFreeTextSize}
          onAddImageStamp={pdfEditor.addImageStamp}
          textEditActive={editSession.session.mode === "edit"}
          onToggleTextEdit={
            readOnly ? undefined : () => setTextEditActive(editSession.session.mode !== "edit")
          }
          readOnly={readOnly}
        />

        <PdfViewerBody
          panel={panel}
          outline={outline}
          pdfDoc={pdfDoc}
          numPages={numPages}
          current={current}
          rotation={rotation}
          base={base}
          pageSizes={pageSizes}
          scale={scale}
          renderErrors={renderErrors}
          ocrLinesByPage={effectiveOcrLinesByPage}
          scrollRef={scrollRef}
          slotRefs={slotRefs}
          onSelectDestination={goToDest}
          onSelectPage={goToPage}
          onWheel={onWheelZoom}
        />

        {editSession.session.mode === "edit" && !draft && (
          <div className="no-print pointer-events-none absolute bottom-4 left-1/2 z-30 -translate-x-1/2 rounded-full border border-line bg-panel/95 px-3 py-1.5 text-xs text-paper shadow-lg">
            Click text to edit it, or click empty space to add text.
          </div>
        )}

        {/* §6.1: the inline editor is portaled into the same page slot pdf.js's own canvas/text/
          annotation layers live in, so it follows zoom/rotation/scroll (PDF coordinates stay
          authoritative — see pdfRectToViewport above) exactly like the contract's PdfPageSlot
          sketch, without restructuring PdfViewerBody's imperative slot-management model. */}
        {draft &&
          draftSlot &&
          draftScreenRect &&
          createPortal(
            <>
              <PdfInlineTextEditor
                key={draft.id}
                draft={draft}
                rect={draftScreenRect}
                cssPerPdfUnit={draftViewport?.scale}
                coverColor={draftCoverColor}
                onTextChange={editSession.updateDraftText}
                onCommit={handleCommitDraft}
                onCancel={editSession.cancelDraft}
                onOverflowChange={editSession.setDraftOverflow}
              />
              {draft.overflow && (
                <div
                  className="no-print pointer-events-none absolute rounded bg-panel/95 px-1.5 py-0.5 text-chrome-xs text-danger shadow"
                  style={{
                    left: draftScreenRect.x,
                    top: draftScreenRect.y + draftScreenRect.height + 2,
                  }}
                >
                  Text exceeds available space — Save is disabled for this edit
                </div>
              )}
            </>,
            draftSlot,
          )}

        {pwPrompt && (
          <PdfPasswordPrompt
            retry={pwPrompt.retry}
            onSubmit={pwPrompt.onSubmit}
            onCancel={pwPrompt.onCancel}
          />
        )}
      </div>
    </div>
  );
}
