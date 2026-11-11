import { useCloseFindOnSurfaceChange } from "../../app/useCloseFindOnSurfaceChange.js";
import {
  lazy,
  Suspense,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
  type RefObject,
  type UIEventHandler,
} from "react";
import { useEditText } from "../../app/useEditText.js";
import { useMirroredEditText } from "../useMirroredEditText.js";
import { FullscreenIcon } from "../FullscreenIcon.js";
import type { EditorView } from "@codemirror/view";
import type { EditorView as ProseMirrorEditorView } from "prosemirror-view";
import { TextSelection } from "prosemirror-state";
import type { SearchProvider, ViewMode } from "@rightkit/document-core/documents/types";
import type { SourceEditorViewState } from "../txt/TxtEditor.js";
import { scrollPosIntoView } from "../txt/cmScroll.js";
import { MarkdownEditor, type MarkdownToolbarState } from "@rightkit/markdown-editor/documents/markdown/MarkdownEditor";
import type { MarkdownSourceRange } from "@rightkit/markdown-editor/documents/markdown/MarkdownProtectedBlock";
import type { MarkdownTableContext } from "@rightkit/markdown-editor/documents/markdown/tables/markdownTableCommands";
import {
  collectMarkdownHeadings,
  collectSourceMapHeadings,
  type MarkdownHeading,
} from "@rightkit/markdown-editor/documents/markdown/markdownTools";
import { Dropdown, Item, Sep, TBtn } from "@rightkit/document-core/documents/toolbarPrimitives";
import { useMarkdownCoreSubscription } from "@rightkit/markdown-editor/documents/markdown/core/useMarkdownCoreSubscription";
import { useMarkdownCoreOptional } from "@rightkit/markdown-editor/documents/markdown/MarkdownCoreProvider";
import { MarkdownWorkspacePanel } from "@rightkit/markdown-editor/documents/markdown/properties/MarkdownWorkspacePanel";
import type { MarkdownExportApi } from "@rightkit/markdown-editor/documents/markdown/export/markdownExportTypes";
import { useSettings } from "@rightkit/document-core/settings/settings";
import { logicalSourceToPmPosition, postPmSelectionToCore } from "@rightkit/markdown-editor/documents/markdown/projection/pmSelectionBridge";
import { publishMarkdownDocumentActivity } from "@rightkit/markdown-editor/documents/markdown/markdownDocumentActivity";
import { documentEvent } from "@rightkit/document-core/host";

// Source editor (CodeMirror) is lazy so md OPEN stays light — only the preview loads. DocumentRouter
// prefetches this chunk when a md doc opens, so by the time the drawer is opened it's already warm
// (the drawer shows readable source while the chunk resolves). The preview never unmounts, so the
// Suspense fallback only ever covers the drawer.
const TextEditor = lazy(() => import("../txt/TxtEditor.js").then((m) => ({ default: m.TextEditor })));
const noopSubscribeEdits = (_path: string, _cb: () => void) => () => {};
const noopEditSnapshot = (_path: string) => undefined;

export function MarkdownSourceFallback({ text }: { text: string }) {
  return (
    <div className="h-full overflow-auto bg-panel px-8 py-6">
      <pre
        data-vr-markdown-source-fallback=""
        className="min-h-full whitespace-pre-wrap rounded-lg px-8 py-7 font-mono text-sm leading-relaxed"
        style={{ background: "var(--reader-surface)", color: "var(--doc-fg)" }}
      >
        {text}
      </pre>
    </div>
  );
}

export function selectAndScrollSourceLine(view: EditorView, lineNumber: number) {
  const line = view.state.doc.line(Math.max(1, Math.min(lineNumber, view.state.doc.lines)));
  view.dispatch({
    selection: { anchor: line.from, head: line.to },
    scrollIntoView: true,
    userEvent: "select.pointer",
  });
  scrollPosIntoView(view, line.from);
  view.focus();
}

export function selectAndScrollSourceRange(view: EditorView, range: MarkdownSourceRange) {
  const from = Math.max(0, Math.min(range.from, view.state.doc.length));
  const to = Math.max(from, Math.min(range.to, view.state.doc.length));
  view.dispatch({
    selection: { anchor: from, head: to },
    scrollIntoView: true,
    userEvent: "select.pointer",
  });
  scrollPosIntoView(view, from);
  view.focus();
}

interface Props {
  text: string;
  documentPath?: string;
  subscribeEdits?: (path: string, cb: () => void) => () => void;
  getEditSnapshot?: (path: string) => string | undefined;
  devMode?: boolean;
  /** Source drawer open. The preview stays MOUNTED across this toggle (drawer-style, like HTML) — no
   *  remount, so opening source no longer flashes a loading skeleton / re-renders / resets scroll. */
  showSource: boolean;
  /** Wave 2 Read mode: hybrid view becomes non-editable and the source drawer stays closed —
   *  editing is only ever reachable in Edit. */
  readOnly?: boolean;
  /** Close the shared find bar when the rendered/source surface switches. */
  onCloseFind?: () => void;
  onChange: (text: string) => void;
  onSave: () => void;
  registerSearchProvider: (provider: SearchProvider | null) => void;
  findBar?: ReactNode;
  findMode?: "find" | "replace";
  onRequestFind?: () => void;
  onRequestReplace?: () => void;
  onOpenPath?: (path: string) => void;
  onChangeView?: (view: ViewMode) => void;
  active?: boolean;
  registerMarkdownExport?: (api: MarkdownExportApi | null) => void;
  /** App-owned raw-source selection/scroll, retained across a shelled dirty tab. */
  sourceViewStateRef?: RefObject<Record<string, SourceEditorViewState>>;
  scrollContainerRef?: RefObject<HTMLDivElement | null>;
  onScroll?: UIEventHandler<HTMLDivElement>;
  /**
   * Optional: Markdown document core to wire edits through. When
   * supplied, the source editor applies edits via the core instead
   * of the legacy `subscribeEdits`/`onChange` pipeline. Off by
   * default to preserve existing behavior.
   */
  markdownCore?: import("@rightkit/markdown-editor/documents/markdown/core/MarkdownDocumentCore").MarkdownDocumentCore;
}

export function MarkdownSourceDrawer({
  documentPath,
  text,
  onChange,
  onSave,
  registerSourceProvider,
  findBar,
  findMode,
  onRequestFind,
  onRequestReplace,
  headings,
  sourceFocusMode,
  sourceTypewriterMode,
  sourceFullscreen,
  sourceWidth,
  initialViewState,
  onJumpLine,
  onToggleFocus,
  onToggleTypewriter,
  onToggleFullscreen,
  onViewStateChange,
  onEditorReady,
  externalHistory = false,
  markdownCore,
  restoreTitle,
  expandTitle,
  showFullscreenToggle = true,
  className = `relative flex min-h-0 min-w-0 flex-col bg-panel ${sourceFullscreen ? "-ml-px flex-1" : "shrink-0 border-l border-line"}`,
}: {
  documentPath?: string;
  text: string;
  onChange: (text: string) => void;
  onSave: () => void;
  registerSourceProvider: (provider: SearchProvider | null) => void;
  findBar?: ReactNode;
  findMode?: "find" | "replace";
  onRequestFind?: () => void;
  onRequestReplace?: () => void;
  headings: ReturnType<typeof collectMarkdownHeadings>;
  sourceFocusMode: boolean;
  sourceTypewriterMode: boolean;
  sourceFullscreen: boolean;
  sourceWidth: number;
  initialViewState: SourceEditorViewState | null;
  onJumpLine: (line: number) => void;
  onToggleFocus: () => void;
  onToggleTypewriter: () => void;
  onToggleFullscreen: () => void;
  onViewStateChange: (state: SourceEditorViewState) => void;
  onEditorReady: (view: EditorView | null) => void;
  externalHistory?: boolean;
  markdownCore?: import("@rightkit/markdown-editor/documents/markdown/core/MarkdownDocumentCore").MarkdownDocumentCore;
  restoreTitle?: string;
  expandTitle?: string;
  showFullscreenToggle?: boolean;
  className?: string;
}) {
  return (
    <section
      className={className}
      style={sourceFullscreen ? undefined : { width: `${sourceWidth}%` }}
    >
      <div className="min-h-0 flex-1">
        <Suspense fallback={<MarkdownSourceFallback text={text} />}>
          <TextEditor
            documentPath={documentPath}
            mono
            markdownToolbar
            text={text}
            onChange={onChange}
            onSave={onSave}
            registerSearchProvider={registerSourceProvider}
            findBar={findBar}
            findMode={findMode}
            onRequestFind={onRequestFind}
            onRequestReplace={onRequestReplace}
            sourceToolbarActions={
              <MarkdownSourceActions
                headings={headings}
                focusMode={sourceFocusMode}
                typewriterMode={sourceTypewriterMode}
                sourceFullscreen={sourceFullscreen}
                onJumpLine={onJumpLine}
                onToggleFocus={onToggleFocus}
                onToggleTypewriter={onToggleTypewriter}
                onToggleFullscreen={onToggleFullscreen}
                restoreTitle={restoreTitle}
                expandTitle={expandTitle}
                showFullscreenToggle={showFullscreenToggle}
              />
            }
            initialViewState={initialViewState}
            onViewStateChange={onViewStateChange}
            onEditorReady={onEditorReady}
            autoFocus
            sourceFocusMode={sourceFocusMode}
            sourceTypewriterMode={sourceTypewriterMode}
            externalHistory={externalHistory}
            markdownCore={markdownCore}
          />
        </Suspense>
      </div>
    </section>
  );
}

export function useMarkdownSourceDrawer(
  text: string,
  onEditorReady?: (
    view: EditorView | null,
    jumpSourceToLine: (lineNumber: number) => void,
    jumpSourceToRange: (range: MarkdownSourceRange) => void,
  ) => void,
  onCaretOffsetChange?: (offset: number) => void,
  mappedHeadings?: MarkdownHeading[],
  documentPath?: string,
  externalSourceViewStateRef?: RefObject<Record<string, SourceEditorViewState>>,
) {
  const localSourceViewStateRef = useRef<SourceEditorViewState | null>(null);
  const sourceEditorRef = useRef<EditorView | null>(null);
  const [sourceWidth, setSourceWidth] = useState(42); // % — wide enough for Markdown tools while keeping preview visible
  const [sourceFullscreen, setSourceFullscreen] = useState(false);
  const [sourceFocusMode, setSourceFocusMode] = useState(false);
  const [sourceTypewriterMode, setSourceTypewriterMode] = useState(false);
  const headings = useMemo(
    () => mappedHeadings ?? collectMarkdownHeadings(text),
    [mappedHeadings, text],
  );

  const jumpSourceToLine = useCallback((lineNumber: number) => {
    const view = sourceEditorRef.current;
    if (!view) return;
    selectAndScrollSourceLine(view, lineNumber);
    requestAnimationFrame(() => selectAndScrollSourceLine(view, lineNumber));
    window.setTimeout(() => selectAndScrollSourceLine(view, lineNumber), 80);
    window.setTimeout(() => selectAndScrollSourceLine(view, lineNumber), 250);
  }, []);
  const jumpSourceToRange = useCallback((range: MarkdownSourceRange) => {
    const jump = () => {
      const view = sourceEditorRef.current;
      if (view) selectAndScrollSourceRange(view, range);
    };
    jump();
    requestAnimationFrame(jump);
    window.setTimeout(jump, 80);
    window.setTimeout(jump, 250);
  }, []);

  const drawerProps = {
    headings,
    sourceFocusMode,
    sourceTypewriterMode,
    sourceFullscreen,
    sourceWidth,
    initialViewState:
      (documentPath ? externalSourceViewStateRef?.current[documentPath] : undefined) ??
      localSourceViewStateRef.current,
    onJumpLine: jumpSourceToLine,
    onToggleFocus: () => setSourceFocusMode((value) => !value),
    onToggleTypewriter: () => setSourceTypewriterMode((value) => !value),
    onToggleFullscreen: () => setSourceFullscreen((value) => !value),
    onViewStateChange: (state: SourceEditorViewState) => {
      if (documentPath && externalSourceViewStateRef)
        externalSourceViewStateRef.current[documentPath] = state;
      else localSourceViewStateRef.current = state;
      onCaretOffsetChange?.(state.head);
    },
    onEditorReady: (view: EditorView | null) => {
      sourceEditorRef.current = view;
      onEditorReady?.(view, jumpSourceToLine, jumpSourceToRange);
    },
  };

  return {
    sourceWidth,
    setSourceWidth,
    sourceFullscreen,
    sourceEditorRef,
    jumpSourceToLine,
    jumpSourceToRange,
    drawerProps,
  };
}

export function MarkdownSplitPane({
  text,
  showSource,
  readOnly = false,
  onCloseFind,
  onChange,
  onSave,
  registerSearchProvider,
  findBar,
  findMode,
  onRequestFind,
  onRequestReplace,
  documentPath,
  subscribeEdits,
  getEditSnapshot,
  onOpenPath,
  onChangeView,
  active = true,
  registerMarkdownExport,
  sourceViewStateRef,
  scrollContainerRef,
  onScroll,
  markdownCore,
}: Props) {
  const { settings } = useSettings();
  // When the core is supplied, mirror its snapshot into local state
  // so the rest of the component works without further changes. The
  // source editor's own updates route back through the core via the
  // legacy `onChange` path; the core is the source of truth.
  const contextCore = useMarkdownCoreOptional();
  const resolvedCore = markdownCore ?? contextCore;
  const coreSnapshot = useMarkdownCoreSubscription(resolvedCore ?? undefined);
  const liveText = useEditText(
    subscribeEdits ?? noopSubscribeEdits,
    getEditSnapshot ?? noopEditSnapshot,
    documentPath ?? "",
  );
  // Core mode: MarkdownCoreProvider owns external ingress (origin-tagged); no mirror into the core.
  const [legacyText, setLegacyText] = useMirroredEditText(liveText, text);
  // Legacy fallback for non-core mode. In core mode the hot path is
  // direct: CodeMirror feeds update.changes and ProseMirror feeds
  // local block patches without whole-string LCS. This callback is
  // retained only for external reconciliation (bounded diff).
  const commitSurfaceChange = useCallback(
    (next: string, origin: "source" | "hybrid") => {
      if (!resolvedCore) {
        setLegacyText(next);
        onChange(next);
        return;
      }
      const current = resolvedCore.snapshot();
      if (current.logicalSource === next) return;
      // Complete-source callbacks are reconciliation-only. Keep this path
      // bounded and deterministic; local core surfaces feed native ranges.
      const edits: readonly import("@rightkit/markdown-editor/documents/markdown/core/types").MarkdownTextEdit[] = [
        {
          from: 0,
          to: current.logicalSource.length,
          insert: next,
          inheritMode: "preserve" as const,
        },
      ];
      resolvedCore.applyCommand({
        edits,
        expectedRevision: current.revision,
        origin,
        label: "Typing",
        coalesceKey: `${origin}:typing`,
      });
    },
    [onChange, resolvedCore, setLegacyText],
  );
  const handleHybridChange = useCallback(
    (next: string) => commitSurfaceChange(next, "hybrid"),
    [commitSurfaceChange],
  );
  const handleSourceChange = useCallback(
    (next: string) => commitSurfaceChange(next, "source"),
    [commitSurfaceChange],
  );
  const renderedText = coreSnapshot?.logicalSource ?? legacyText;
  const mappedHeadings = useMemo(
    () =>
      coreSnapshot
        ? collectSourceMapHeadings(coreSnapshot.logicalSource, coreSnapshot.sourceMap)
        : undefined,
    [coreSnapshot],
  );
  const [caretOffset, setCaretOffset] = useState(0);
  const [tableContext, setTableContext] = useState<MarkdownTableContext | null>(null);
  const [toolbarState, setToolbarState] = useState<MarkdownToolbarState>({
    headingLevel: null,
    bold: false,
    italic: false,
    strike: false,
    code: false,
    highlight: false,
  });
  const [imageDialogRequest, setImageDialogRequest] = useState(0);
  const hybridEditorRef = useRef<ProseMirrorEditorView | null>(null);
  const selectionRangeRef = useRef({ anchor: 0, head: 0 });
  const captureHybridSelection = useCallback(() => {
    const view = hybridEditorRef.current;
    if (!view || !resolvedCore) return undefined;
    // Keyboard focus may leave the browser range in this editor before ProseMirror's
    // selection observer catches up. Reconcile that owned range before formatting.
    const domSelection = view.dom.ownerDocument.getSelection();
    if (
      domSelection?.anchorNode &&
      domSelection.focusNode &&
      view.dom.contains(domSelection.anchorNode) &&
      view.dom.contains(domSelection.focusNode)
    ) {
      try {
        const anchor = view.posAtDOM(domSelection.anchorNode, domSelection.anchorOffset);
        const head = view.posAtDOM(domSelection.focusNode, domSelection.focusOffset);
        const selection = TextSelection.between(
          view.state.doc.resolve(anchor),
          view.state.doc.resolve(head),
        );
        if (!selection.eq(view.state.selection))
          view.dispatch(view.state.tr.setSelection(selection));
      } catch {
        // Detached ranges use the editor's last owned selection.
      }
    }
    const snapshot = resolvedCore.snapshot();
    const hint = postPmSelectionToCore(
      resolvedCore,
      view.state.doc,
      view.state.selection,
      snapshot,
    );
    if (!hint) return undefined;
    const range = { anchor: hint.anchor, head: hint.head };
    selectionRangeRef.current = range;
    return range;
  }, [resolvedCore]);

  useEffect(() => {
    if (!active || !resolvedCore || !registerMarkdownExport) return;
    const fallbackTitle = (documentPath?.split(/[\\/]/).pop() ?? "Untitled").replace(
      /\.(md|markdown)$/i,
      "",
    );
    const api: MarkdownExportApi = {
      documentPath,
      getRevision: () => resolvedCore.snapshot().revision,
      // Export pipeline (DOM render, standalone HTML) is loaded on first export, not on open.
      createSnapshot: async (options) => {
        const { renderMarkdownExport } = await import("@rightkit/markdown-editor/documents/markdown/export/renderMarkdownExport");
        return renderMarkdownExport({
          snapshot: resolvedCore.snapshot(),
          documentPath,
          fallbackTitle,
          theme: options?.theme ?? "current",
          remoteImages: options?.remoteImages ?? "preserve",
          safeHtmlEnabled: settings.markdownSafeHtml,
        });
      },
    };
    registerMarkdownExport(api);
    return () => registerMarkdownExport(null);
  }, [active, documentPath, registerMarkdownExport, resolvedCore, settings.markdownSafeHtml]);
  const pendingSourceRangeRef = useRef<MarkdownSourceRange | null>(null);
  const { sourceEditorRef, jumpSourceToRange, drawerProps } = useMarkdownSourceDrawer(
    renderedText,
    (view, _jumpLine, jumpRange) => {
      if (view && pendingSourceRangeRef.current) {
        const range = pendingSourceRangeRef.current;
        pendingSourceRangeRef.current = null;
        jumpRange(range);
      }
    },
    setCaretOffset,
    mappedHeadings,
    documentPath,
    sourceViewStateRef,
  );

  // The active surface owns find: Hybrid when rendered, Source while the drawer is open.
  const hybridProviderRef = useRef<SearchProvider | null>(null);
  const sourceProviderRef = useRef<SearchProvider | null>(null);
  // Read mode keeps the drawer closed even if a persisted view asks for "source" — the drawer is
  // the editing surface, and Read never exposes one.
  const drawer = showSource && !readOnly;
  useCloseFindOnSurfaceChange(drawer, !!findMode, onCloseFind);
  const showSourceRef = useRef(drawer);
  showSourceRef.current = drawer;
  const publish = useCallback(() => {
    registerSearchProvider(
      showSourceRef.current ? sourceProviderRef.current : hybridProviderRef.current,
    );
  }, [registerSearchProvider]);
  const registerHybridProvider = useCallback(
    (p: SearchProvider | null) => {
      hybridProviderRef.current = p;
      publish();
    },
    [publish],
  );
  const registerSourceProvider = useCallback(
    (p: SearchProvider | null) => {
      sourceProviderRef.current = p;
      publish();
    },
    [publish],
  );
  useEffect(() => {
    publish();
  }, [publish]); // re-expose the active surface when the drawer toggles
  useEffect(() => () => registerSearchProvider(null), [registerSearchProvider]);

  const openSourceAtRange = useCallback(
    (range: { from: number; to: number }) => {
      const from = Math.max(0, Math.min(range.from, renderedText.length));
      const to = Math.max(from, Math.min(range.to, renderedText.length));
      pendingSourceRangeRef.current = { from, to };
      if (!showSourceRef.current) {
        onChangeView?.("source");
        return;
      }
      jumpSourceToRange({ from, to });
      pendingSourceRangeRef.current = null;
    },
    [jumpSourceToRange, onChangeView, renderedText],
  );

  // Read-mode outline/issue jump: the source drawer is unavailable, so select the range inside the
  // hybrid view instead — same "jump to the thing" intent, never an edit surface.
  const revealInHybrid = useCallback(
    (range: { from: number; to: number }) => {
      const view = hybridEditorRef.current;
      if (!view || view.isDestroyed || !resolvedCore) return;
      const pos = logicalSourceToPmPosition(view.state.doc, range.from, resolvedCore.snapshot());
      const clamped = Math.max(0, Math.min(pos, view.state.doc.content.size));
      try {
        const sel = TextSelection.near(view.state.doc.resolve(clamped));
        view.dispatch(view.state.tr.setSelection(sel).scrollIntoView());
        view.focus();
      } catch {
        // Positions inside protected/atom blocks have no text cursor — leave the doc as-is.
      }
    },
    [resolvedCore],
  );

  useEffect(() => {
    publishMarkdownDocumentActivity(documentPath, active ?? true);
    return () => publishMarkdownDocumentActivity(documentPath, false);
  }, [active, documentPath]);

  // Read mode: undo/redo mutate the doc — the listeners stay off entirely (the Edit menu that
  // fires these events is hidden too, so nothing is lost).
  useEffect(() => {
    if (!active || !resolvedCore || readOnly) return;
    const onUndo = () => {
      if (!drawer) captureHybridSelection();
      resolvedCore.undo();
    };
    const onRedo = () => {
      if (!drawer) captureHybridSelection();
      resolvedCore.redo();
    };
    window.addEventListener(documentEvent("undo"), onUndo);
    window.addEventListener(documentEvent("redo"), onRedo);
    return () => {
      window.removeEventListener(documentEvent("undo"), onUndo);
      window.removeEventListener(documentEvent("redo"), onRedo);
    };
  }, [active, resolvedCore, readOnly, drawer, captureHybridSelection]);

  useEffect(() => {
    if (!drawer || !sourceEditorRef.current) return;
    if (pendingSourceRangeRef.current) {
      const range = pendingSourceRangeRef.current;
      pendingSourceRangeRef.current = null;
      jumpSourceToRange(range);
      return;
    }
  }, [drawer, jumpSourceToRange, sourceEditorRef.current]);

  const getInsertAt = useCallback(() => {
    return (
      sourceEditorRef.current?.state.selection.main.head ??
      selectionRangeRef.current.head ??
      caretOffset
    );
  }, [caretOffset, sourceEditorRef]);

  return (
    <div className="flex h-full min-h-0 min-w-0 bg-ink">
      {/* Hybrid render is ALWAYS mounted; source hides it via CSS
          so the ProseMirror view stays mounted and pointer-movement caching
          remains free. The section's parent chain never changes shape. */}
      <section
        className={drawer ? "hidden" : "min-h-0 min-w-0 flex-1"}
        aria-hidden={drawer ? true : undefined}
      >
        {/* resolvedCore is fixed for a mounted tab, and chromeHidden only hides the panel's own
              chrome — the editor's parent chain never changes shape, so toggling the source drawer
              can NEVER remount the hybrid editor (the DocumentRouter md invariant, one level down). */}
        {resolvedCore ? (
          <MarkdownWorkspacePanel
            core={resolvedCore}
            caretOffset={caretOffset}
            documentPath={documentPath}
            safeHtml={settings.markdownSafeHtml}
            onJumpToRange={readOnly ? revealInHybrid : openSourceAtRange}
            getInsertAt={getInsertAt}
            selection={selectionRangeRef.current}
            getEditorView={() => hybridEditorRef.current}
            tableContext={tableContext}
            toolbarState={toolbarState}
            onInsertImage={() => setImageDialogRequest((request) => request + 1)}
            chromeHidden={drawer}
            readOnly={readOnly}
            onToggleSource={() => onChangeView?.("source")}
            captureSelection={captureHybridSelection}
            scrollContainerRef={scrollContainerRef}
            onScroll={onScroll}
            findBar={findBar}
            findMode={findMode}
            onRequestFind={onRequestFind}
            onRequestReplace={onRequestReplace}
          >
            <MarkdownEditor
              text={renderedText}
              onChange={handleHybridChange}
              onSave={onSave}
              documentPath={documentPath}
              onOpenPath={onOpenPath}
              onSourceJump={readOnly ? revealInHybrid : openSourceAtRange}
              onCaretOffsetChange={setCaretOffset}
              onSelectionRangeChange={(selection) => {
                selectionRangeRef.current = selection;
              }}
              onEditorReady={(view) => {
                hybridEditorRef.current = view;
              }}
              onTableContextChange={setTableContext}
              onToolbarStateChange={(next) =>
                setToolbarState((current) =>
                  current.headingLevel === next.headingLevel &&
                  current.bold === next.bold &&
                  current.italic === next.italic &&
                  current.strike === next.strike &&
                  current.code === next.code &&
                  current.highlight === next.highlight
                    ? current
                    : next,
                )
              }
              registerSearchProvider={registerHybridProvider}
              core={resolvedCore}
              mappedHeadings={mappedHeadings}
              requestImageDialog={imageDialogRequest}
              surfaceHidden={drawer || !active}
              active={active}
              readOnly={readOnly}
            />
          </MarkdownWorkspacePanel>
        ) : (
          <MarkdownEditor
            text={renderedText}
            onChange={handleHybridChange}
            onSave={onSave}
            documentPath={documentPath}
            onOpenPath={onOpenPath}
            onSourceJump={readOnly ? revealInHybrid : openSourceAtRange}
            onCaretOffsetChange={setCaretOffset}
            onSelectionRangeChange={(selection) => {
              selectionRangeRef.current = selection;
            }}
            onEditorReady={(view) => {
              hybridEditorRef.current = view;
            }}
            registerSearchProvider={registerHybridProvider}
            core={resolvedCore}
            surfaceHidden={!active}
            active={active}
            readOnly={readOnly}
          />
        )}
      </section>

      {drawer && (
        <MarkdownSourceDrawer
          documentPath={documentPath}
          text={renderedText}
          onChange={handleSourceChange}
          externalHistory={Boolean(resolvedCore)}
          onSave={onSave}
          registerSourceProvider={registerSourceProvider}
          findBar={findBar}
          findMode={findMode}
          onRequestFind={onRequestFind}
          onRequestReplace={onRequestReplace}
          {...drawerProps}
          sourceFullscreen
          showFullscreenToggle={false}
          markdownCore={resolvedCore ?? undefined}
        />
      )}
    </div>
  );
}

export function MarkdownSourceActions({
  headings,
  focusMode,
  typewriterMode,
  sourceFullscreen,
  onJumpLine,
  onToggleFocus,
  onToggleTypewriter,
  onToggleFullscreen,
  restoreTitle = "Restore markdown split",
  expandTitle = "Expand markdown source",
  showFullscreenToggle = true,
}: {
  headings: ReturnType<typeof collectMarkdownHeadings>;
  focusMode: boolean;
  typewriterMode: boolean;
  sourceFullscreen: boolean;
  onJumpLine: (line: number) => void;
  onToggleFocus: () => void;
  onToggleTypewriter: () => void;
  onToggleFullscreen: () => void;
  restoreTitle?: string;
  expandTitle?: string;
  showFullscreenToggle?: boolean;
}) {
  return (
    <>
      {headings.length > 0 && (
        <Dropdown label="outline" title="Jump to heading" minWidth={72} panelWidth={220}>
          {(close) =>
            headings.map((heading) => (
              <Item
                key={`${heading.line}-${heading.text}`}
                onRun={() => {
                  onJumpLine(heading.line);
                  close();
                }}
              >
                <span style={{ paddingLeft: (heading.depth - 1) * 10 }}>
                  {heading.text || `Line ${heading.line}`}
                </span>
              </Item>
            ))
          }
        </Dropdown>
      )}
      <Sep />
      <TBtn
        label="focus"
        title="Focus current source line"
        active={focusMode}
        onRun={onToggleFocus}
        width={50}
      />
      <TBtn
        label="type"
        title="Typewriter source mode"
        active={typewriterMode}
        onRun={onToggleTypewriter}
        width={42}
      />
      {showFullscreenToggle && (
        <TBtn
          label={<FullscreenIcon restore={sourceFullscreen} />}
          title={sourceFullscreen ? restoreTitle : expandTitle}
          onRun={onToggleFullscreen}
        />
      )}
    </>
  );
}
