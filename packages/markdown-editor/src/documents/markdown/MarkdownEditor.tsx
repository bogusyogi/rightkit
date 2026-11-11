import type * as React from "react";
import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { useDismiss } from "@rightkit/platform-ui/headless";
import { useSlashMenuEscape } from "./useSlashMenuEscape.js";
import { createPortal } from "react-dom";
import { baseKeymap } from "prosemirror-commands";
import { history, redo, undo } from "prosemirror-history";
import { keymap } from "prosemirror-keymap";
import type { Node as ProseMirrorNode } from "prosemirror-model";
import { EditorState, TextSelection, type Transaction } from "prosemirror-state";
import { EditorView } from "prosemirror-view";
import type { SearchProvider } from "@rightkit/document-core/documents/types";
import { clearHighlights } from "@rightkit/document-core/documents/searchUtils";
import { logEvent, logPerf } from "@rightkit/document-core/state/diagnostics";
import { scheduleAfterPaint } from "@rightkit/document-core/app/deferredMount";
import { markdownToPmDoc } from "./projection/pmFromMarkdown.js";
import { pmDocToMarkdown } from "./projection/pmToMarkdown.js";
import {
  collectSourceMapHeadings,
  outlineHeadingsFor,
  shouldParseMarkdownPaste,
  type MappedMarkdownHeading,
} from "./markdownTools.js";
import {
  RenderedRawBlockView,
  type MarkdownSourceRange,
  type ProtectedRenderContext,
} from "./MarkdownProtectedBlock.js";
import type { MarkdownDocumentCore } from "./core/MarkdownDocumentCore.js";
import { MarkdownSurfaceId, type MarkdownCommitSummary } from "./core/MarkdownDocumentTypes.js";
import { commitPmTransactionToCore } from "./projection/pmProjectionSync.js";
import {
  classifyMarkdownTransaction,
  commitOptionsForIntent,
} from "./projection/markdownTransactionIntent.js";
import { pmSelectionToLogicalRange } from "./projection/pmSelectionBridge.js";
import { projectMarkdownToPm } from "./projection/MarkdownProjection.js";
import {
  captureTopLevelSelection,
  createStateFromCore,
  createStateFromMergedDoc,
  createSuspendedState,
  getTableContext,
  isBlankDoc,
  markdownOffsetForSelection,
  mergeDocsPreservingIdentities,
  sameTableContext,
  selectAllMarkdown,
} from "./projection/pmEditorState.js";
import { markdownHybridSchema } from "./markdownHybridSchema.js";
import { markdownTablePlugins } from "./tables/markdownTableCommands.js";
import type { MarkdownTableContext } from "./tables/markdownTableCommands.js";
import { MarkdownImageNodeView } from "./inlineEditing/MarkdownImageNodeView.js";
import { CodeFenceNodeView } from "./codeFence/CodeFenceNodeView.js";
import { codeFenceHighlightPlugin } from "./codeFence/codeFenceHighlight.js";
import { isImageMime, mimeFromFileName } from "./assets/markdownImage.js";
import { MarkdownImageDialog } from "./commands/MarkdownImageDialog.js";
import { MarkdownInsertMenu } from "./insert/MarkdownInsertMenu.js";
import { useOptionalBlockDrafts } from "@rightkit/document-core/app/BlockDraftContext";
import { replaceWholeDocumentWithMarkdown, selectBlockFromLeftEdge } from "./markdownHybridDom.js";
import {
  acquireMarkdownResource,
  createMarkdownWork,
  getMarkdownColdTabState,
  persistMarkdownTabState,
  rememberMarkdownColdTabState,
} from "./markdownResourceLifecycle.js";
import { FIND_REPLACE_META, useMarkdownSearchProvider } from "./useMarkdownSearchProvider.js";
import { useMarkdownImageIngest } from "./assets/useMarkdownImageIngest.js";
import { documentEvent } from "@rightkit/document-core/host";

interface Props {
  text: string;
  onChange: (text: string) => void;
  onSave?: () => void;
  /** Reconcile parent document path after image insertion performs Save As. */
  onSaveAsSuccess?: (newPath: string) => Promise<void> | void;
  onEditorReady?: (view: EditorView | null) => void;
  registerSearchProvider?: (provider: SearchProvider | null) => void;
  documentPath?: string;
  onOpenPath?: (path: string) => void;
  onSourceJump?: (range: MarkdownSourceRange) => void;
  onCaretOffsetChange?: (offset: number) => void;
  onSelectionRangeChange?: (selection: { anchor: number; head: number }) => void;
  /** Fires whenever the caret's table context changes (including null on leaving a table), so an
   *  owning toolbar outside this component's own render tree can show contextual table controls. */
  onTableContextChange?: (context: MarkdownTableContext | null) => void;
  onToolbarStateChange?: (state: MarkdownToolbarState) => void;
  requestImageDialog?: number;
  core?: MarkdownDocumentCore | null;
  /** Source-map headings already collected by the owning split pane. Supplying them keeps the scan
   *  to ONE pass per revision — both surfaces need the same list, and it is recomputed per keystroke. */
  mappedHeadings?: readonly MappedMarkdownHeading[];
  /** True while this rendered surface is hidden (e.g. behind the Source drawer). Core commits then
   *  only mark the projection stale; the view re-projects once when it becomes visible again,
   *  instead of re-parsing the whole document on every Source keystroke. */
  surfaceHidden?: boolean;
  /** Active tab ownership. Core-backed inactive surfaces suspend their PM projection. */
  active?: boolean;
  /** Wave 2 Read mode: the PM view is non-editable and doc-changing transactions are dropped.
   *  Selection, scrolling, and find still work; the Source drawer owns the editable fallback. */
  readOnly?: boolean;
}

export interface MarkdownToolbarState {
  readonly headingLevel: 1 | 2 | 3 | 4 | 5 | 6 | null;
  readonly bold: boolean;
  readonly italic: boolean;
  readonly strike: boolean;
  readonly code: boolean;
  readonly highlight: boolean;
}

/**
 * Carries the selection a hidden surface should restore on reveal across one core commit: the
 * commit's own selection hint wins; an external replacement (no history commands) drops it so the
 * structural selection is preserved as before; otherwise the pending hint is mapped through the
 * commit's `oldToNew` changes.
 */
function logHybridCommitRejected(
  expectedRevision: number,
  core: { readonly state: { readonly revision: number } } | null | undefined,
  error: unknown,
): void {
  console.warn(
    JSON.stringify({
      event: "md_hybrid_commit_rejected",
      expectedRevision,
      coreRevision: core?.state.revision ?? null,
      message: error instanceof Error ? error.message : String(error),
    }),
  );
}

function deferredSelectionAfter(
  pending: { anchor: number; head: number } | null,
  summary: MarkdownCommitSummary,
): { anchor: number; head: number } | null {
  if (summary.selectionHint) {
    return { anchor: summary.selectionHint.anchor, head: summary.selectionHint.head };
  }
  if (!pending || summary.historyGroup.commands.length === 0) return null;
  return {
    anchor: mapOffsetThroughChanges(pending.anchor, summary.oldToNew),
    head: mapOffsetThroughChanges(pending.head, summary.oldToNew),
  };
}

function mapOffsetThroughChanges(
  offset: number,
  changes: MarkdownCommitSummary["oldToNew"],
): number {
  // Changes are sorted and their B-side offsets already include every earlier change.
  let mapped = offset;
  for (const change of changes) {
    if (change.toA <= offset) mapped = offset + (change.toB - change.toA);
    else if (change.fromA < offset) return change.toB;
    else break;
  }
  return mapped;
}

export function MarkdownEditor({
  text,
  onChange,
  onSaveAsSuccess,
  onEditorReady,
  registerSearchProvider,
  documentPath,
  onOpenPath,
  onSourceJump,
  onCaretOffsetChange,
  onSelectionRangeChange,
  onTableContextChange,
  onToolbarStateChange,
  requestImageDialog,
  core,
  mappedHeadings,
  surfaceHidden = false,
  active = true,
  readOnly = false,
}: Props) {
  // Core-backed Markdown is always mounted under App's canonical provider.
  const blockDrafts = useOptionalBlockDrafts();
  const sourceHeadings = useMemo(() => {
    if (!core) return outlineHeadingsFor(text);
    const collected =
      mappedHeadings ??
      (() => {
        const snapshot = core.snapshot();
        return collectSourceMapHeadings(snapshot.logicalSource, snapshot.sourceMap);
      })();
    return collected.filter((heading) => heading.depth <= 3);
  }, [core, text, mappedHeadings]);
  const hostRef = useRef<HTMLDivElement>(null);
  const viewRef = useRef<EditorView | null>(null);
  const suspendedRef = useRef(false);
  const resourceReleaseRef = useRef<(() => void) | null>(null);
  const latestTextRef = useRef(text);
  latestTextRef.current = text;
  const composingRef = useRef(false);
  const compositionBaseDocRef = useRef<ProseMirrorNode | null>(null);
  const compositionBaseStateRef = useRef<EditorState | null>(null);
  const compositionTransactionsRef = useRef<Transaction[]>([]);
  const projectionRevisionRef = useRef(core?.state.revision ?? 0);
  const committingHybridProjectionRef = useRef(false);
  // Hidden-surface projection deferral. `surfaceHiddenRef` is read by the core subscriber;
  // `staleProjectionRef` records that core commits arrived while hidden; `deferredSelectionRef`
  // carries the latest surface selection hint (mapped through later commits) to the reveal.
  const surfaceHiddenRef = useRef(surfaceHidden);
  const staleProjectionRef = useRef(false);
  const deferredSelectionRef = useRef<{ anchor: number; head: number } | null>(null);
  const onChangeRef = useRef(onChange);
  const onCaretOffsetChangeRef = useRef(onCaretOffsetChange);
  const onSelectionRangeChangeRef = useRef(onSelectionRangeChange);
  const onTableContextChangeRef = useRef(onTableContextChange);
  const onToolbarStateChangeRef = useRef(onToolbarStateChange);
  // Last table context reported upward. Used to dedupe equivalent contexts so ordinary
  // transactions (typing, caret moves inside an unchanged table) do not emit fresh objects
  // that would rerender the whole Markdown tree. `undefined` marks "nothing emitted yet".
  const lastTableContextRef = useRef<MarkdownTableContext | null | undefined>(undefined);
  const onEditorReadyRef = useRef(onEditorReady);
  const onSaveAsSuccessRef = useRef(onSaveAsSuccess);
  // Read-mode flag read by PM's `editable` prop + the dispatchTransaction guard, so both stay
  // live without rebuilding the EditorView.
  const readOnlyRef = useRef(readOnly);
  readOnlyRef.current = readOnly;
  const renderContextRef = useRef<ProtectedRenderContext>({
    documentPath,
    onOpenPath,
    onSourceJump,
    sourceHeadings,
    registry: blockDrafts ?? undefined,
  });
  const [imageDialogOpen, setImageDialogOpen] = useState(false);
  const imageSelectionRef = useRef<{ from: number; to: number } | undefined>(undefined);
  const seenImageDialogRequestRef = useRef(requestImageDialog ?? 0);
  // "/"-while-typing inserter: opens the same MarkdownInsertMenu used by the toolbar's Insert
  // surface, positioned at the caret. `pmPos` is the ProseMirror position the trigger "/" was
  // typed at (never actually inserted); `insertAt` is that position's logical-source offset.
  const [slashMenu, setSlashMenu] = useState<{
    left: number;
    top: number;
    pmPos: number;
    insertAt: number;
    query: string;
  } | null>(null);
  const slashMenuHostRef = useRef<HTMLDivElement>(null);
  const slashMenuOpenRef = useRef(false);

  useEffect(() => {
    onChangeRef.current = onChange;
    onCaretOffsetChangeRef.current = onCaretOffsetChange;
    onSelectionRangeChangeRef.current = onSelectionRangeChange;
    onTableContextChangeRef.current = onTableContextChange;
    onToolbarStateChangeRef.current = onToolbarStateChange;
    onEditorReadyRef.current = onEditorReady;
    onSaveAsSuccessRef.current = onSaveAsSuccess;
    renderContextRef.current = {
      documentPath,
      onOpenPath,
      onSourceJump,
      sourceHeadings,
      registry: blockDrafts ?? undefined,
    };
  }, [
    onCaretOffsetChange,
    onSelectionRangeChange,
    onTableContextChange,
    onToolbarStateChange,
    onChange,
    onEditorReady,
    onSaveAsSuccess,
    documentPath,
    onOpenPath,
    onSourceJump,
    sourceHeadings,
    blockDrafts,
  ]);

  const emitChange = useCallback((doc: ProseMirrorNode) => {
    onChangeRef.current(pmDocToMarkdown(doc));
  }, []);

  const { refreshFind } = useMarkdownSearchProvider({
    viewRef,
    registerSearchProvider,
    readOnly,
  });

  // Reports the current table context upward, but only when it is semantically different from
  // the last reported value. This keeps equivalent contexts (same rows/columns/applicability,
  // or repeated `null`) from being re-emitted on every transaction.
  const emitTableContext = useCallback((view: EditorView | null) => {
    const next = getTableContext(view);
    if (sameTableContext(lastTableContextRef.current, next)) return;
    lastTableContextRef.current = next;
    onTableContextChangeRef.current?.(next);
  }, []);

  const emitToolbarState = useCallback((view: EditorView | null) => {
    if (!view) return;
    const { state } = view;
    const parent = state.selection.$from.parent;
    const level = parent.type.name === "heading" ? Number(parent.attrs.level) : 0;
    const headingLevel = level >= 1 && level <= 6 ? (level as 1 | 2 | 3 | 4 | 5 | 6) : null;
    const marks = state.selection.empty
      ? (state.storedMarks ?? state.selection.$from.marks())
      : null;
    const has = (name: string) =>
      marks
        ? marks.some((mark) => mark.type.name === name)
        : state.doc.rangeHasMark(
            state.selection.from,
            state.selection.to,
            state.schema.marks[name],
          );
    onToolbarStateChangeRef.current?.({
      headingLevel,
      bold: has("strong"),
      italic: has("em"),
      strike: has("delete"),
      code: has("code"),
      highlight: has("mark"),
    });
  }, []);

  const syncFromCore = useCallback(
    (
      view: EditorView,
      documentCore: MarkdownDocumentCore,
      selectionHint?: { anchor: number; head: number } | null,
    ) => {
      if (view.isDestroyed) return;
      staleProjectionRef.current = false;
      deferredSelectionRef.current = null;
      projectionRevisionRef.current = documentCore.state.revision;
      const preservedSelection = captureTopLevelSelection(view.state);
      const freshDoc = projectMarkdownToPm(documentCore, markdownHybridSchema);
      const mergedDoc = mergeDocsPreservingIdentities(view.state.doc, freshDoc);
      view.updateState(
        createStateFromMergedDoc(mergedDoc, documentCore, preservedSelection, selectionHint),
      );
      emitTableContext(view);
      emitToolbarState(view);
      queueMicrotask(refreshFind);
    },
    [refreshFind, emitTableContext, emitToolbarState],
  );

  const commitHybridProjection = useCallback(
    (
      before: ProseMirrorNode,
      after: ProseMirrorNode,
      options: Parameters<typeof commitPmTransactionToCore>[3],
    ) => {
      if (!core) return;
      committingHybridProjectionRef.current = true;
      try {
        commitPmTransactionToCore(core, before, after, options);
      } finally {
        committingHybridProjectionRef.current = false;
      }
    },
    [core],
  );

  // --- Image: Paste / Drop / Toolbar pipeline (M3) ---
  const { pasteThroughCore, insertImagesFromFiles, handleImagePaste, handleImageDrop } =
    useMarkdownImageIngest({
      core,
      documentPath,
      hostRef,
      viewRef,
      onSaveAsSuccessRef,
    });

  const focusFromShell = useCallback((event: React.MouseEvent<HTMLDivElement>) => {
    const view = viewRef.current;
    const target = event.target as HTMLElement | null;
    if (!view || target?.closest(".ProseMirror")) return;
    if (selectBlockFromLeftEdge(view.dom, event.nativeEvent, event.currentTarget)) return;
    event.preventDefault();
    view.focus();
  }, []);

  const openImageDialog = useCallback(() => {
    if (readOnlyRef.current) return;
    const view = viewRef.current;
    if (core && view) {
      const selection = pmSelectionToLogicalRange(
        view.state.doc,
        view.state.selection,
        core.snapshot(),
      );
      imageSelectionRef.current = {
        from: Math.min(selection.anchor, selection.head),
        to: Math.max(selection.anchor, selection.head),
      };
    }
    setImageDialogOpen(true);
  }, [core]);

  useEffect(() => {
    if (
      requestImageDialog === undefined ||
      requestImageDialog === seenImageDialogRequestRef.current
    )
      return;
    seenImageDialogRequestRef.current = requestImageDialog;
    openImageDialog();
  }, [openImageDialog, requestImageDialog]);

  const closeSlashMenu = useCallback((refocus: boolean) => {
    slashMenuOpenRef.current = false;
    setSlashMenu(null);
    if (refocus) viewRef.current?.focus();
  }, []);
  useEffect(() => {
    if (!active || surfaceHidden) closeSlashMenu(false);
  }, [active, surfaceHidden, closeSlashMenu]);

  const openSlashMenu = useCallback(
    (view: EditorView, pmPos: number) => {
      if (!core) return false;
      const coords = view.coordsAtPos(pmPos);
      const logical = pmSelectionToLogicalRange(
        view.state.doc,
        TextSelection.create(view.state.doc, pmPos),
        core.snapshot(),
      );
      slashMenuOpenRef.current = true;
      setSlashMenu({
        left: coords.left,
        top: coords.bottom,
        pmPos,
        insertAt: logical.head,
        query: "",
      });
      return true;
    },
    [core],
  );

  useEffect(() => {
    if (!slashMenu) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Enter") {
        // The table size picker owns Enter while focus is inside it.
        if (
          event.target instanceof Element &&
          event.target.closest('[data-testid="md-table-picker"]')
        )
          return;
        const first = slashMenuHostRef.current?.querySelector<HTMLButtonElement>(
          '[role="menuitem"]:not([disabled])',
        );
        event.preventDefault();
        event.stopPropagation();
        if (first) first.click();
        else closeSlashMenu(true);
      } else if (event.key === "Backspace" && event.target !== document.body) {
        if (!slashMenuHostRef.current?.contains(event.target as Node)) {
          event.preventDefault();
          event.stopPropagation();
          if (slashMenu.query) setSlashMenu({ ...slashMenu, query: slashMenu.query.slice(0, -1) });
          else closeSlashMenu(true);
        }
      }
    };
    document.addEventListener("keydown", onKeyDown, true);
    return () => document.removeEventListener("keydown", onKeyDown, true);
  }, [slashMenu, closeSlashMenu]);

  // A press outside closes the menu without returning focus to the editor.
  useDismiss(() => closeSlashMenu(false), {
    enabled: slashMenu !== null,
    escape: false,
    container: slashMenuHostRef,
    outside: "mousedown",
  });

  // Escape closes the menu and returns focus to the editor. It is read on the document in the capture
  // phase (so a modal that takes it in the window capture phase wins, and it runs above element
  // handlers): the capture listener delegates to the public `registry.handle`, which matches Escape with
  // any modifiers, in a text field, during IME composition and when already prevented.
  useSlashMenuEscape(slashMenu != null, closeSlashMenu);

  const createState = useCallback(
    (source: string) =>
      EditorState.create({
        doc: markdownToPmDoc(source),
        plugins: [
          ...(core
            ? []
            : [
                history(),
                keymap({
                  "Mod-a": selectAllMarkdown,
                  "Mod-z": undo,
                  "Mod-y": redo,
                  "Mod-Shift-z": redo,
                }),
              ]),
          keymap(baseKeymap),
          ...markdownTablePlugins(),
          codeFenceHighlightPlugin(),
        ],
      }),
    [core],
  );

  // Build EditorView before browser paint so a newly activated Markdown tab never exposes an empty host.
  // biome-ignore lint/correctness/useExhaustiveDependencies: hostRef and insertImagesFromFiles are stable refs, core is sufficient
  useLayoutEffect(() => {
    const host = hostRef.current;
    if (!host) return;
    suspendedRef.current = false;
    const resourceKey = documentPath ?? core?.state.documentId;
    const priorState = getMarkdownColdTabState(resourceKey);
    let restoreWork: ReturnType<typeof createMarkdownWork> | null = null;
    const releaseView = acquireMarkdownResource("prosemirror", resourceKey);
    resourceReleaseRef.current = releaseView;
    // Cold-mount instrumentation. Naming follows the cold-mount-cost plan
    // (docs/plans/2026-07-10-viewright-cold-mount-architecture.md): parse vs EditorView split.
    const mountStart = performance.now();
    performance.mark("md:parse:start");
    const viewState = core
      ? createStateFromCore(core, undefined, priorState?.selection)
      : createState(latestTextRef.current);
    performance.mark("md:editorView:start");
    const view = new EditorView(host, {
      state: viewState,
      editable: () => !readOnlyRef.current,
      nodeViews: {
        code_fence: (node, view, getPos) =>
          new CodeFenceNodeView(node, view, getPos as () => number | undefined, {
            stampIntent: true,
          }),
        raw_block: (node, view, getPos) =>
          new RenderedRawBlockView(
            node,
            view,
            getPos as () => number | undefined,
            renderContextRef,
            core ?? null,
          ),
        image: (node, view, getPos) =>
          new MarkdownImageNodeView(node, view, getPos as () => number | undefined, {
            core: core ?? null,
            documentPath,
          }),
      },
      attributes: {
        role: "textbox",
        "aria-label": "Markdown Hybrid editor",
        "aria-multiline": "true",
        class: "vr-pm-markdown-doc vr-markdown-document-geometry",
      },
      handleTextInput: (nextView, from, to, text) => {
        if (slashMenuOpenRef.current && !nextView.composing) {
          // Typing while the slash menu is open filters it; the text never reaches the document.
          setSlashMenu((current) => (current ? { ...current, query: current.query + text } : current));
          return true;
        }
        if (text === "/" && from === to && core && !nextView.composing) {
          const $from = nextView.state.doc.resolve(from);
          const parent = $from.parent;
          if (
            parent.type.name === "paragraph" &&
            parent.textContent === "" &&
            $from.parentOffset === 0
          ) {
            return openSlashMenu(nextView, from);
          }
        }
        return false;
      },
      handleDOMEvents: {
        // Native contenteditable undo/redo would mutate the DOM behind the core and PM would
        // then commit that as a user edit (empty image attrs). Route to the core instead.
        beforeinput: (_view, event) => {
          const inputType = (event as InputEvent).inputType;
          if (inputType !== "historyUndo" && inputType !== "historyRedo") return false;
          event.preventDefault();
          if (readOnlyRef.current || !core) return true;
          window.dispatchEvent(
            new Event(inputType === "historyUndo" ? documentEvent("undo") : documentEvent("redo"), {
              cancelable: true,
            }),
          );
          return true;
        },
        mousedown: (nextView, event) => selectBlockFromLeftEdge(nextView.dom, event as MouseEvent),
        // Contract 14 (spatial continuity): a supported table has exactly one editing
        // surface — the native ProseMirror cells at their document position. Double-click
        // falls through to PM's own word/cell selection; no duplicate grid editor opens.
        compositionstart: () => {
          composingRef.current = true;
          const composingView = viewRef.current;
          compositionBaseDocRef.current = composingView?.state.doc ?? null;
          compositionBaseStateRef.current = composingView?.state ?? null;
          compositionTransactionsRef.current = [];
          return false;
        },
        compositionend: (nextView) => {
          composingRef.current = false;
          queueMicrotask(() => {
            if (viewRef.current !== nextView) return;
            const before = compositionBaseDocRef.current;
            const baseState = compositionBaseStateRef.current;
            const compositionTransactions = compositionTransactionsRef.current;
            compositionBaseDocRef.current = null;
            compositionBaseStateRef.current = null;
            compositionTransactionsRef.current = [];
            if (!core || !before) {
              emitChange(nextView.state.doc);
              return;
            }
            let compositionTransaction: Transaction | undefined;
            if (baseState && compositionTransactions.length > 0) {
              try {
                const aggregate = baseState.tr;
                for (const transaction of compositionTransactions) {
                  for (const step of transaction.steps) aggregate.step(step);
                }
                if (aggregate.doc.eq(nextView.state.doc)) compositionTransaction = aggregate;
              } catch {
                // A browser may expose composition steps against a transient
                // DOM state; retain existing bounded source fallback then.
              }
            }
            try {
              commitHybridProjection(before, nextView.state.doc, {
                origin: "hybrid",
                label: "Typing",
                coalesceKey: "hybrid:ime",
                expectedRevision: projectionRevisionRef.current,
                fallbackSource: () => pmDocToMarkdown(nextView.state.doc),
                transaction: compositionTransaction,
              });
            } catch (error) {
              logHybridCommitRejected(projectionRevisionRef.current, core, error);
              syncFromCore(nextView, core);
            }
          });
          return false;
        },
        dragover: (_nextView, event) => {
          const dt = (event as DragEvent).dataTransfer;
          if (!dt) return false;
          const hasImage =
            Array.from(dt.types ?? []).some((t) => t === "Files") &&
            Array.from(dt.files ?? []).some(
              (f) => isImageMime(f.type) || !!mimeFromFileName(f.name),
            );
          if (hasImage || Array.from(dt.types ?? []).includes("Files")) {
            // Allow drop; set effect
            if (dt) dt.dropEffect = "copy";
            event.preventDefault();
            return true;
          }
          return false;
        },
        drop: (nextView, event) => {
          if (readOnlyRef.current) return false;
          if (handleImageDrop(nextView, event as unknown as DragEvent)) return true;
          return false;
        },
        paste: (nextView, event) => {
          if (readOnlyRef.current) return false;
          const clipboardEvent = event as ClipboardEvent;
          if (handleImagePaste(nextView, clipboardEvent)) return true;
          const pastedText = clipboardEvent.clipboardData?.getData("text/plain") ?? "";
          if (pasteThroughCore(nextView, clipboardEvent)) return true;
          if (!pastedText || !shouldParseMarkdownPaste(pastedText)) return false;

          event.preventDefault();
          if (isBlankDoc(nextView.state.doc)) {
            replaceWholeDocumentWithMarkdown(nextView, pastedText);
            return true;
          }

          const pastedDoc = markdownToPmDoc(pastedText);
          nextView.dispatch(
            nextView.state.tr
              .replaceSelection(pastedDoc.slice(0, pastedDoc.content.size))
              .scrollIntoView(),
          );
          return true;
        },
      },
      dispatchTransaction(transaction) {
        if (view.isDestroyed) return;
        // Read mode: drop every doc-changing transaction (node-view chips, image dialogs, table
        // plugins, drag handles — anything that slipped past editable=false). Selection-only
        // transactions still flow so find/scroll/selection hints keep working.
        if (readOnlyRef.current && transaction.docChanged) return;
        if (slashMenuOpenRef.current && (transaction.docChanged || transaction.selectionSet)) {
          closeSlashMenu(false);
        }
        const before = view.state.doc;
        const nextState = view.state.apply(transaction);
        view.updateState(nextState);
        if (composingRef.current && transaction.docChanged) {
          compositionTransactionsRef.current.push(transaction);
        }
        emitTableContext(view);
        emitToolbarState(view);
        if (transaction.selectionSet) {
          if (core) {
            const mapped = pmSelectionToLogicalRange(nextState.doc, nextState.selection);
            core.postSelectionHint({
              surface: "prosemirror",
              nodeId: mapped.nodeId as import("./core/MarkdownNodeIdFactory.js").MarkdownNodeId | null,
              anchor: mapped.anchor,
              head: mapped.head,
              assoc: mapped.assoc,
            });
            onCaretOffsetChangeRef.current?.(mapped.head);
            onSelectionRangeChangeRef.current?.({ anchor: mapped.anchor, head: mapped.head });
          } else {
            onCaretOffsetChangeRef.current?.(markdownOffsetForSelection(nextState));
          }
        }
        if (transaction.docChanged && !composingRef.current && !view.composing) {
          if (!core) {
            emitChange(nextState.doc);
          } else {
            try {
              const findReplace = transaction.getMeta(FIND_REPLACE_META) as
                | { label: "Replace" }
                | undefined;
              // CE-2A: the transaction's user intent drives core history grouping —
              // one user action lands as one history entry; typing/delete coalesce.
              const intent = classifyMarkdownTransaction(transaction);
              const options = commitOptionsForIntent(intent);
              commitHybridProjection(before, nextState.doc, {
                origin: "hybrid",
                label: findReplace?.label ?? options.label,
                coalesceKey: findReplace ? undefined : options.coalesceKey,
                isolateHistory: findReplace ? true : options.isolateHistory,
                expectedRevision: projectionRevisionRef.current,
                fallbackSource: () => pmDocToMarkdown(nextState.doc),
                transaction,
              });
            } catch (error) {
              logHybridCommitRejected(projectionRevisionRef.current, core, error);
              syncFromCore(view, core);
            }
          }
          queueMicrotask(refreshFind);
        }
      },
    });
    viewRef.current = view;
    let stopQaView: (() => void) | undefined;
    let qaViewCancelled = false;
    if (
      import.meta.env?.VITE_RIGHTKIT_BROWSER_QA === "1" &&
      resourceKey &&
      new URLSearchParams(window.location.search).get("qa") === "1"
    ) {
      void import("./qaCoreObservation.js").then(({ observeQaMarkdownView }) => {
        if (!qaViewCancelled) stopQaView = observeQaMarkdownView(resourceKey, view);
      });
    }
    // The new view was projected from the core's current revision.
    staleProjectionRef.current = false;
    if (core) projectionRevisionRef.current = core.state.revision;
    deferredSelectionRef.current = null;
    const scrollHost = host.closest<HTMLElement>("[data-document-scroll]");
    if (scrollHost && priorState) {
      restoreWork = createMarkdownWork();
      requestAnimationFrame(() => {
        if (!restoreWork?.isCancelled()) {
          scrollHost.scrollTop = priorState.scrollTop;
          scrollHost.scrollLeft = priorState.scrollLeft;
          restoreWork?.complete();
        }
      });
    }
    emitTableContext(view);
    emitToolbarState(view);
    const unsubscribeCore = core?.subscribe(MarkdownSurfaceId.ProseMirror, (summary) => {
      const commands = summary.historyGroup.commands;
      const origin = commands[commands.length - 1]?.origin;
      // This view's own commit: its doc already reflects the change.
      if (committingHybridProjectionRef.current && origin === "hybrid") {
        projectionRevisionRef.current = summary.revision;
        return;
      }
      if (viewRef.current !== view) {
        projectionRevisionRef.current = summary.revision;
        return;
      }
      if (surfaceHiddenRef.current) {
        // Hidden behind the Source drawer: a full re-projection per keystroke is pure waste.
        // Mark the view stale and re-project exactly once on reveal. `projectionRevisionRef`
        // stays at the projected revision, so a commit built from the stale doc is rejected
        // by the core as stale and falls back to `syncFromCore`.
        staleProjectionRef.current = true;
        deferredSelectionRef.current = deferredSelectionAfter(
          deferredSelectionRef.current,
          summary,
        );
        return;
      }
      syncFromCore(view, core, summary.selectionHint);
    });
    onEditorReadyRef.current?.(view);
    performance.mark("md:editorView:end");
    performance.measure("md:parse", "md:parse:start", "md:editorView:start");
    performance.measure("md:editorView", "md:editorView:start", "md:editorView:end");
    logPerf("md:cold-mount", mountStart);
    // viewer:content:paint — true post-paint marker. rAF×2 ensures the browser has actually
    // committed the EditorView's first DOM to the screen, not just the React tree.
    const cancelContentPaint = scheduleAfterPaint(() => {
      logEvent("viewer:content:paint");
      // Tab-switch latency: warm = this tab had remembered state, cold = first mount.
      try {
        const kind = priorState ? "warm" : "cold";
        performance.measure(`md:tab-switch:${kind}`, {
          start: mountStart,
          end: performance.now(),
          detail: { path: documentPath ?? null, chars: latestTextRef.current.length },
        });
      } catch {
        // Measurement is diagnostic only.
      }
    });
    const persistNow = () => {
      try {
        const snap = core?.snapshot();
        if (!snap || viewRef.current !== view) return;
        const sel = staleProjectionRef.current
          ? deferredSelectionRef.current
          : pmSelectionToLogicalRange(view.state.doc, view.state.selection, snap);
        persistMarkdownTabState(resourceKey, {
          scrollTop: scrollHost?.scrollTop ?? 0,
          scrollLeft: scrollHost?.scrollLeft ?? 0,
          selection: sel ? { anchor: sel.anchor, head: sel.head } : undefined,
        });
      } catch {
        // Persistence is best effort.
      }
    };
    window.addEventListener("pagehide", persistNow);
    const persistOnHide = () => {
      if (document.visibilityState === "hidden") persistNow();
    };
    document.addEventListener("visibilitychange", persistOnHide);
    let scrollPersistTimer: ReturnType<typeof setTimeout> | undefined;
    const onScroll = () => {
      clearTimeout(scrollPersistTimer);
      scrollPersistTimer = setTimeout(persistNow, 400);
    };
    scrollHost?.addEventListener("scroll", onScroll, { passive: true });
    return () => {
      window.removeEventListener("pagehide", persistNow);
      document.removeEventListener("visibilitychange", persistOnHide);
      scrollHost?.removeEventListener("scroll", onScroll);
      clearTimeout(scrollPersistTimer);
      qaViewCancelled = true;
      stopQaView?.();
      let snapshot: ReturnType<MarkdownDocumentCore["snapshot"]> | undefined;
      try {
        snapshot = core?.snapshot();
      } catch {
        // Parent teardown can dispose the core before this mounted surface cleans up.
      }
      // A stale (hidden, not yet re-projected) doc cannot map its selection against the
      // current snapshot; use the deferred surface hint instead.
      const selection =
        core && snapshot && viewRef.current === view
          ? staleProjectionRef.current
            ? deferredSelectionRef.current
            : pmSelectionToLogicalRange(view.state.doc, view.state.selection, snapshot)
          : null;
      rememberMarkdownColdTabState(resourceKey, {
        scrollTop: scrollHost?.scrollTop ?? 0,
        scrollLeft: scrollHost?.scrollLeft ?? 0,
        selection: selection ? { anchor: selection.anchor, head: selection.head } : undefined,
        renderedRevision: snapshot?.revision,
      });
      restoreWork?.cancel();
      resourceReleaseRef.current?.();
      resourceReleaseRef.current = null;
      cancelContentPaint();
      unsubscribeCore?.();
      if (viewRef.current === view) viewRef.current = null;
      emitTableContext(null);
      onToolbarStateChangeRef.current?.({
        headingLevel: null,
        bold: false,
        italic: false,
        strike: false,
        code: false,
        highlight: false,
      });
      onEditorReadyRef.current?.(null);
      clearHighlights();
      view.destroy();
    };
  }, [
    createState,
    emitChange,
    emitTableContext,
    emitToolbarState,
    refreshFind,
    documentPath,
    core,
    syncFromCore,
    pasteThroughCore,
    commitHybridProjection,
    handleImageDrop,
    handleImagePaste,
    insertImagesFromFiles,
    openSlashMenu,
    closeSlashMenu,
  ]);

  // Read/Edit flips re-evaluate the `editable` prop: an empty setProps forces PM to recompute
  // view.editable and rewrite contenteditable on the DOM without touching state or the doc.
  // biome-ignore lint/correctness/useExhaustiveDependencies: readOnly is the trigger; editable reads readOnlyRef.
  useLayoutEffect(() => {
    const view = viewRef.current;
    if (!view || view.isDestroyed) return;
    view.setProps({});
  }, [readOnly]);

  // Warm tab switch: any inactive-to-active transition of this mounted surface, whether or not it
  // was suspended. Declared before the reveal effect so the measure includes re-projection.
  const wasInactiveRef = useRef(false);
  useLayoutEffect(() => {
    if (!active) {
      wasInactiveRef.current = true;
      return;
    }
    if (!wasInactiveRef.current) return;
    wasInactiveRef.current = false;
    const start = performance.now();
    return scheduleAfterPaint(() => {
      try {
        performance.measure("md:tab-switch:warm", {
          start,
          end: performance.now(),
          detail: { path: documentPath ?? null },
        });
      } catch {
        // Measurement is diagnostic only.
      }
    });
  }, [active, documentPath]);

  // Reveal: re-project a view that went stale while hidden, exactly once and before paint.
  // Declared after the view-creating layout effect so a fresh mount never double-projects.
  useLayoutEffect(() => {
    const hidden = surfaceHidden || !active;
    surfaceHiddenRef.current = hidden;
    if (hidden || !core || !staleProjectionRef.current) return;
    const view = viewRef.current;
    if (!view || view.isDestroyed) return;
    syncFromCore(view, core, deferredSelectionRef.current);
  }, [active, surfaceHidden, core, syncFromCore]);

  // Core-backed inactive surfaces keep their authoritative core mounted but replace their projected
  // document with one empty PM paragraph. This releases large DOM/node-view trees while preserving
  // source, history, and logical caret for reveal projection.
  useLayoutEffect(() => {
    const view = viewRef.current;
    if (!core || !view || view.isDestroyed) return;
    if (active && suspendedRef.current) {
      suspendedRef.current = false;
      resourceReleaseRef.current = acquireMarkdownResource(
        "prosemirror",
        documentPath ?? core.state.documentId,
      );
      onEditorReadyRef.current?.(view);
      return;
    }
    if (active) return;
    if (suspendedRef.current) return;
    let selectionHint: { anchor: number; head: number } | null = null;
    try {
      const snapshot = core.snapshot();
      const logical = pmSelectionToLogicalRange(view.state.doc, view.state.selection, snapshot);
      selectionHint = { anchor: logical.anchor, head: logical.head };
    } catch {
      // Core teardown owns final cleanup; retain any previously deferred selection.
    }
    if (selectionHint) deferredSelectionRef.current = selectionHint;
    staleProjectionRef.current = true;
    view.updateState(createSuspendedState(core));
    suspendedRef.current = true;
    resourceReleaseRef.current?.();
    resourceReleaseRef.current = null;
    emitTableContext(view);
    emitToolbarState(view);
    onEditorReadyRef.current?.(null);
  }, [active, core, documentPath, emitTableContext, emitToolbarState]);

  useEffect(() => {
    const view = viewRef.current;
    if (core) return;
    if (!view || view.isDestroyed || pmDocToMarkdown(view.state.doc) === text) return;
    view.updateState(createState(text));
    refreshFind();
  }, [core, createState, refreshFind, text]);

  return (
    <div
      className="relative h-full min-h-0 min-w-0 bg-ink"
      {...(readOnly ? { "data-vr-md-readonly": "" } : {})}
    >
      <div
        ref={hostRef}
        className="vr-reading-container min-h-full py-6"
        onMouseDown={focusFromShell}
      />
      {active &&
        !surfaceHidden &&
        slashMenu &&
        core &&
        createPortal(
          <div
            ref={slashMenuHostRef}
            role="presentation"
            style={{ position: "fixed", left: slashMenu.left, top: slashMenu.top }}
            className="vr-chrome z-[100] w-72 max-h-[70vh] overflow-y-auto rounded-xl border border-line bg-panel/95 shadow-xl backdrop-blur-sm"
          >
            <MarkdownInsertMenu
              core={core}
              insertAt={slashMenu.insertAt}
              filter={slashMenu.query}
              onAfterInsert={() => closeSlashMenu(true)}
              onClose={() => closeSlashMenu(true)}
              onInsertImage={() => {
                closeSlashMenu(false);
                openImageDialog();
              }}
              getEditorView={() => viewRef.current}
            />
          </div>,
          document.body,
        )}
      {imageDialogOpen && core && (
        <MarkdownImageDialog
          core={core}
          view={viewRef.current}
          documentPath={documentPath}
          initialSelection={imageSelectionRef.current}
          onClose={() => setImageDialogOpen(false)}
          onSaveAsSuccess={onSaveAsSuccess}
          getDocumentText={() => core.snapshot().logicalSource}
        />
      )}
    </div>
  );
}
