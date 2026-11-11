import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { EditorView } from "@codemirror/view";
import type { ViewMode } from "@rightkit/document-core/documents/types";
import type { SourceEditorViewState } from "../txt/TxtEditor.js";
import { scrollPosIntoView } from "../txt/cmScroll.js";
import { previewBootHtml } from "@rightkit/html-editor/documents/html/htmlInline";
import { useSiteRootGrant } from "@rightkit/html-editor/documents/html/useSiteRootGrant";
import { logEvent } from "@rightkit/document-core/state/diagnostics";
import { toast } from "@rightkit/platform-ui/headless";
import { useSettings } from "@rightkit/document-core/settings/settings";
import { viewportWidthPx } from "@rightkit/document-core/documents/previewViewport";
import { useHtmlNavigation } from "@rightkit/html-editor/documents/html/useHtmlNavigation";
import { useHtmlPreviewFind } from "@rightkit/html-editor/documents/html/useHtmlPreviewFind";
import { useHtmlSubPages } from "@rightkit/html-editor/documents/html/useHtmlSubPages";
import { useHtmlPreviewLifecycle, type HtmlBlockHistoryEntry } from "@rightkit/html-editor/documents/html/useHtmlPreviewLifecycle";
import { useHtmlLiveCssBridge } from "@rightkit/html-editor/documents/html/useHtmlLiveCssBridge";
import { useHtmlColdState } from "@rightkit/html-editor/documents/html/useHtmlColdState";
import { useHtmlBlockUndoRedo } from "@rightkit/html-editor/documents/html/useHtmlBlockUndoRedo";
import { useHtmlReload } from "@rightkit/html-editor/documents/html/useHtmlReload";
import { useHtmlInframeTarget } from "@rightkit/html-editor/documents/html/useHtmlInframeTarget";
import { resolveSafeBlockTarget, type HtmlInspectTarget, type SourceRange } from "@rightkit/html-editor/documents/html/sourceMap";
import { useHtmlDocumentBridge } from "@rightkit/html-editor/documents/html/HtmlDocumentBridge";
import { HtmlDocumentLayout } from "./HtmlDocumentLayout.js";
import { useCloseFindOnSurfaceChange } from "../../app/useCloseFindOnSurfaceChange.js";
import { trackHtmlDocumentMount, trackHtmlUnmount } from "@rightkit/html-editor/documents/html/htmlResourceLifecycle";
import { useOptionalBlockDrafts } from "@rightkit/document-core/app/BlockDraftContext";
import { createBlockDraftRegistry } from "@rightkit/document-core/app/blockDrafts";
import { useHtmlPageBuffer } from "@rightkit/html-editor/documents/html/useHtmlPageBuffer";
import { useHtmlSafeBlockEntry } from "@rightkit/html-editor/documents/html/useHtmlSafeBlockEntry";
import { useHtmlBlockOps, useRecordBlockCommit } from "@rightkit/html-editor/documents/html/useHtmlBlockOps";
import { useHtmlSourceDrawerResize } from "@rightkit/html-editor/documents/html/useHtmlSourceDrawerResize";
import { useHtmlAutoViewport } from "@rightkit/html-editor/documents/html/useHtmlAutoViewport";
import { useHtmlPreviewControls } from "@rightkit/html-editor/documents/html/useHtmlPreviewControls";
import { useHtmlInspect } from "@rightkit/html-editor/documents/html/useHtmlInspect";
import { useHtmlBlockDrafts } from "@rightkit/html-editor/documents/html/useHtmlBlockDrafts";
import { useHtmlRichEditState } from "@rightkit/html-editor/documents/html/useHtmlRichEditState";
import { useHtmlSrcDocLifecycle } from "@rightkit/html-editor/documents/html/useHtmlSrcDocLifecycle";
import { initialSourceDrawerOpen, type ViewportMode } from "@rightkit/html-editor/documents/html/htmlDocumentShared";
import type { HtmlDocumentProps } from "./HtmlDocumentProps.js";
import { documentEvent } from "@rightkit/document-core/host";

export type { HtmlDocumentProps } from "./HtmlDocumentProps.js";

/**
 * One tab = one local-site session. The tab is identified by `entryPath`; `activePath` is the page
 * currently shown and may roam to sub-pages as the user navigates (always under the entry folder —
 * Rust enforces containment). Switching modes/opening the source drawer keeps this component mounted,
 * so the nav stack and every page's edit buffer survive.
 *
 * This file is the mode-router shell: it owns the preview-build lifecycle (applied snapshot → srcDoc),
 * the source drawer state, and layout. Behavior lives in focused hooks (useHtmlPreviewControls,
 * useHtmlInspect, useHtmlBlockDrafts, useHtmlAutoViewport, plus the existing navigation/bridge/block
 * hooks); rendering is delegated to HtmlDocumentView (preview surface), HtmlDocumentSource (source
 * drawer), and HtmlDocumentEdit (inspect context menu).
 */
export function HtmlDocument({
  entryPath,
  view,
  text,
  onChange,
  registerSearchProvider,
  findBar,
  findMode,
  onRequestFind,
  onRequestReplace,
  onCloseFind,
  onOpenPath,
  onChangeView,
  readOnly = false,
  active = true,
  pageBuffers,
  dirtyPagePaths,
  onEnsurePage,
  onPageChange,
  onSetActivePage,
  onSavePage,
  onDiscardPage,
  onReloadFromDisk,
  initialPreviewScrollY,
  onPreviewScroll,
  sourceViewStateRef: externalSourceViewStateRef,
}: HtmlDocumentProps) {
  const optionalDrafts = useOptionalBlockDrafts();
  const fallbackDrafts = useRef(createBlockDraftRegistry()).current;
  const blockDrafts = optionalDrafts ?? fallbackDrafts;
  const mountedRef = useRef(true);
  useEffect(() => {
    mountedRef.current = true;
    trackHtmlDocumentMount(entryPath);
    return () => {
      mountedRef.current = false;
      trackHtmlUnmount();
    };
  }, [entryPath]);
  const { settings } = useSettings();
  // Changes when the palette changes; theme CSS is hot-applied into the iframe without rebuilding it.
  const themeKey = `${settings.themeId}:${settings.primaryAccentId ?? ""}:${settings.customPrimary ?? ""}`;
  const sessionId = useMemo(
    () =>
      typeof crypto !== "undefined" && "randomUUID" in crypto
        ? crypto.randomUUID()
        : `${Date.now()}-${Math.random().toString(36).slice(2)}`,
    [],
  );
  const iframeRef = useRef<HTMLIFrameElement>(null);
  const safeIframeRef = useRef<HTMLIFrameElement>(null);
  // The element under the last right-click in the interactive iframe (recorded by the bridge without
  // suppressing the native menu). Used when the user picks the grafted native "View source" item.
  const lastContextTargetRef = useRef<HtmlInspectTarget | null>(null);
  const [srcDoc, setSrcDoc] = useState(() => previewBootHtml());
  // One site-root grant per tab, inferred in Rust from `entryPath` (nearest enclosing project
  // directory). It is what lets the preview load assets that live above the document's own folder —
  // the way a browser does — and it is revoked when the tab unmounts so the grant never outlives it.
  // undefined = grant still resolving; null = resolved fallback without protocol grant. Never build
  // user HTML against tauri:// while resolution is pending or root-relative assets leak into app CSP.
  const { siteRoot, pickingRoot, chooseRoot: handleChooseSiteRoot } = useSiteRootGrant(entryPath);
  const [nonce, setNonce] = useState(0); // bumped by Reload to force a rebuild
  const [error, setError] = useState<string | null>(null);
  const [drawerOpen, setDrawerOpen] = useState(initialSourceDrawerOpen);
  // Read mode keeps the source drawer (the editing surface) closed even if its state says open.
  const drawer = drawerOpen && !readOnly;
  // Done used to lift the inline editor the instant it committed, but rebuilding the preview is
  // async — it reparses the document and re-inlines scripts before the new srcDoc commits. So the
  // overlay disappeared over the STILL-OLD rendering, the pre-edit text flashed back, and only then
  // did the edit appear. Hold the overlay until the rebuilt srcDoc lands, with a timeout so a failed
  // or superseded build can never strand it on screen.
  const srcDocRef = useRef("");
  const pendingEditorCloseRef = useRef<{ srcDocAtCommit: string; timer: number } | null>(null);

  useCloseFindOnSurfaceChange(drawer, !!findMode, onCloseFind);
  // The content the preview currently renders. Updated only on navigation,
  // a sub-page's disk load completing, and Reload — NOT on every keystroke, so the running page keeps
  // its state while you edit. Ownership is renderedRevision (sourceChecksum): preview reloads only when
  // checksum !== renderedRevisionRef, not on save/apply nonce bumps. Live CSS hot-applies via bridge.
  const [applied, setApplied] = useState("");
  // Identity of the revision the preview currently renders. Sole owner of reload decisions.
  const renderedRevisionRef = useRef<string | null>(null);
  // Page-scoped inverse history for block Done (one entry per committed plan). Cleared/rebased on
  // external full-source replacement or explicit full-source save. Also drives Ctrl/Cmd+Z undo.
  const blockHistoryRef = useRef<HtmlBlockHistoryEntry[]>([]);
  const redoHistoryRef = useRef<HtmlBlockHistoryEntry[]>([]);
  // The canonical source the draft-commit path last committed. The buffer state update is async
  // (setCommittedSource / onPageChange re-render later), so runBlockStructuralOp must read the
  // FRESH committed bytes here or its plan resolves against the pre-edit source and silently
  // drops the text edit the draft just committed.
  const lastBlockCommitSourceRef = useRef<string | null>(null);
  // The first snapshot is committed in an effect. Keep the builder dormant until that effect has run:
  // otherwise React runs the builder once with the empty boot value and again with the real document.
  const [initialAppliedReady, setInitialAppliedReady] = useState(false);
  const [sourceFullscreen, setSourceFullscreen] = useState(false);
  const [viewportMode, setViewportMode] = useState<ViewportMode>("auto");
  const splitRef = useRef<HTMLDivElement>(null);
  const autoViewport = useHtmlAutoViewport(splitRef);
  const [previewZoomHud, setPreviewZoomHud] = useState<string | null>(null);
  // Browser-style status strip: the URL of the link currently under the pointer in the preview.
  const [hoveredLink, setHoveredLink] = useState<string | null>(null);
  const previewZoomTimer = useRef<number | null>(null);
  const inspectScrollCancelRef = useRef<(() => void) | null>(null);
  // Seed from App's durable store so the scroll survives a tab-switch unmount; also restored
  // across live rebuilds within a session.
  const scrollYRef = useRef(initialPreviewScrollY ?? 0);
  // Ref so the bridge message effect can persist scroll without resubscribing on every render.
  const onPreviewScrollRef = useRef(onPreviewScroll);
  onPreviewScrollRef.current = onPreviewScroll;

  // Keep-alive diagnostic: this fires ONCE per real mount. If it fires again for the same tab after a
  // tab switch, the component is being remounted (keep-alive broken) — visible in Settings → Diagnostics.
  useEffect(() => {
    logEvent("html:mount");
  }, []);
  const localSourceViewStateRef = useRef<Record<string, SourceEditorViewState>>({});
  const sourceViewStateRef = externalSourceViewStateRef ?? localSourceViewStateRef;
  const sourceEditorRef = useRef<EditorView | null>(null);
  const pendingInspectRangeRef = useRef<SourceRange | null>(null);

  // In-tab navigation/history, local-link resolution, and the blocked-link prompt.
  const {
    activePath,
    pendingFragment,
    blocked,
    setBlocked,
    navigate: rawNavigate,
    handleHref: rawHandleHref,
    goHistory: rawGoHistory,
    locateBlocked,
    copyBlocked,
    canGoBack,
    canGoForward,
  } = useHtmlNavigation({ entryPath, onOpenPath, scrollYRef });

  // A modified rich draft must be committed (Done) before the page changes; it is never dropped.
  // Returns false (navigation cancelled, draft kept) when the commit does not succeed.
  const commitDraftBeforeLeaving = useCallback(async (): Promise<boolean> => {
    const identity = { documentPath: entryPath, pagePath: activePath };
    if (!blockDrafts.hasModifiedDraft(identity)) return true;
    const result = await blockDrafts.resolve(identity, "view-change", "done");
    return result.outcome === "done" || result.outcome === "missing";
  }, [activePath, blockDrafts, entryPath]);
  const navigate = useCallback(
    (path: string, fragment: string | null = null) => {
      void commitDraftBeforeLeaving().then((ok) => {
        if (ok) rawNavigate(path, fragment);
      });
    },
    [commitDraftBeforeLeaving, rawNavigate],
  );
  const handleHref = useCallback(
    async (href: string, newTab = false) => {
      if (!newTab && !(await commitDraftBeforeLeaving())) return;
      await rawHandleHref(href, newTab);
    },
    [commitDraftBeforeLeaving, rawHandleHref],
  );
  const goHistory = useCallback(
    (delta: number) => {
      void commitDraftBeforeLeaving().then((ok) => {
        if (ok) rawGoHistory(delta);
      });
    },
    [commitDraftBeforeLeaving, rawGoHistory],
  );

  const revealSourceRange = useCallback((range: SourceRange): boolean => {
    const view = sourceEditorRef.current;
    if (!view) return false;
    const start = Math.max(0, Math.min(view.state.doc.length, range.start));
    const end = Math.max(start, Math.min(view.state.doc.length, range.end));
    view.dispatch({ selection: { anchor: start, head: end } });
    view.focus();
    scrollPosIntoView(view, start); // CM's own scrollIntoView doesn't move the editor here — see cmScroll
    return true;
  }, []);

  const { drawerWidth, startResize } = useHtmlSourceDrawerResize(splitRef);

  // Legacy persisted visual values degrade to Interactive; both rendered modes use scoped editing.
  const [fallbackView, setFallbackView] = useState<ViewMode | null>(null);
  const effectiveView = onChangeView ? view : (fallbackView ?? view);
  const mode = (effectiveView === "preview" ? "preview" : "interactive") as
    | "interactive"
    | "preview";
  // Rich-edit session state (safe target, transient view handoff, toolbar formatting, deferred
  // inline-editor close). See useHtmlRichEditState.ts.
  const {
    safeTarget,
    setSafeTarget,
    pendingSafeTargetRef,
    restoreInteractiveAfterEditRef,
    richFormatting,
    setRichFormatting,
    richEditRef,
    restoreInteractiveAfterTransientEdit,
    closeInlineEditor,
    clearPendingEditorClose,
  } = useHtmlRichEditState({ onChangeView, setFallbackView, srcDocRef, pendingEditorCloseRef });

  // Print routing, the Interactive<->Safe swap mask, and select-all / zoom-reset / scroll-to-top.
  const { swapping, scrollInteractivePreviewToTop, resetInteractivePreviewZoom } =
    useHtmlPreviewControls({
      active,
      drawer,
      error,
      mode,
      sessionId,
      iframeRef,
      previewZoomTimer,
      inspectScrollCancelRef,
      scrollYRef,
    });

  // Find in the rendered interactive preview (drawer closed): registers an html-preview provider that
  // drives the in-frame find engine; resolveFindResult settles each async postMessage round-trip.
  const { resolveFindResult } = useHtmlPreviewFind({
    iframeRef,
    sessionId,
    mode,
    drawerOpen: drawer,
    error,
    registerSearchProvider,
  });

  const {
    onEntry,
    activeBuffer,
    activeBufferRef,
    activeDirty,
    subLoaded,
    setCommittedSource,
    commitSource,
    revisionForSource,
    commitPageMutation,
  } = useHtmlPageBuffer({
    entryPath,
    activePath,
    text,
    pageBuffers,
    dirtyPagePaths,
    onChange,
    onPageChange,
  });
  // Cold-tab handoff: restore/park coherent rendered bytes + revision on remount/unmount.
  useHtmlColdState({
    entryPath,
    activePath,
    activeBuffer,
    applied,
    renderedRevisionRef,
    setApplied,
    setInitialAppliedReady,
  });
  const recordBlockCommit = useRecordBlockCommit({
    activePath,
    blockHistoryRef,
    redoHistoryRef,
    renderedRevisionRef,
    commitSource,
    setApplied,
  });
  const { handleSafeBlockTarget, handleEditRefused } = useHtmlSafeBlockEntry({
    readOnly,
    mode,
    drawer,
    entryPath,
    activePath,
    blockDrafts,
    mountedRef,
    activeBufferRef,
    sourceEditorRef,
    pendingInspectRangeRef,
    richEditRef,
    pendingEditorCloseRef,
    pendingSafeTargetRef,
    restoreInteractiveAfterEditRef,
    lastBlockCommitSourceRef,
    onChangeView,
    setFallbackView,
    setDrawerOpen,
    setSafeTarget,
    revealSourceRange,
    restoreInteractiveAfterTransientEdit,
    revisionForSource,
    commitPageMutation,
    recordBlockCommit,
  });
  // Contextual structural/rich operations from the block editor (one atomic transaction each).
  const { runBlockStructuralOp, runBlockRichOp, canRunRichOp, canRunStructuralOp, propertyAttributes } =
    useHtmlBlockOps({
      entryPath,
      activePath,
      activeBuffer,
      activeBufferRef,
      mountedRef,
      blockDrafts,
      safeTarget,
      lastBlockCommitSourceRef,
      closeInlineEditor,
      revisionForSource,
      commitPageMutation,
      recordBlockCommit,
    });

  // Draft lifecycle for the active page (navigation/source-change resolution, read-mode release,
  // block snapshot for the authoring row and overlay).
  const { blockIdentity, blockSnapshot } = useHtmlBlockDrafts({
    readOnly,
    drawerOpen,
    setDrawerOpen,
    entryPath,
    activePath,
    activeBuffer,
    blockDrafts,
    safeTarget,
    setSafeTarget,
    restoreInteractiveAfterTransientEdit,
    revisionForSource,
    blockHistoryRef,
    redoHistoryRef,
    pendingEditorCloseRef,
    pendingSafeTargetRef,
  });

  // Page-scoped block undo/redo + its global event/shortcut wiring.
  useHtmlBlockUndoRedo({
    active,
    readOnly,
    blockDrafts,
    entryPath,
    activePath,
    blockHistoryRef,
    redoHistoryRef,
    activeBufferRef,
    commitSource,
    commitPageMutation,
    renderedRevisionRef,
    setApplied,
    setSafeTarget,
  });

  // Inspect-to-source (View source) and its element context menu.
  const {
    inspectMenu,
    setInspectMenu,
    handleInspectTarget,
    openInspectMenu,
    inspectFromMenu,
    copyMenuValue,
  } = useHtmlInspect({
    readOnly,
    activeBuffer,
    mode,
    sessionId,
    iframeRef,
    sourceEditorRef,
    pendingInspectRangeRef,
    inspectScrollCancelRef,
    revealSourceRange,
    setDrawerOpen,
  });

  // Seed the sub-page buffer on navigation, and report the active page up to App.
  useHtmlSubPages({ entryPath, activePath, onEntry, onEnsurePage, onSetActivePage });

  // Preview lifecycle: applied-snapshot gating + interactive srcDoc rebuild.
  useHtmlPreviewLifecycle({
    active,
    mode,
    entryPath,
    activePath,
    onEntry,
    subLoaded,
    drawerOpen: drawer,
    applied,
    activeBuffer,
    nonce,
    sessionId,
    siteRoot,
    initialAppliedReady,
    pendingFragment,
    pageBuffers,
    activeBufferRef,
    blockHistoryRef,
    redoHistoryRef,
    renderedRevisionRef,
    scrollYRef,
    mountedRef,
    setApplied,
    setSrcDoc,
    setError,
    setInitialAppliedReady,
  });

  // Live CSS/theme hot-apply bridge while the drawer is open.
  useHtmlLiveCssBridge({
    mode,
    sessionId,
    drawerOpen: drawer,
    activeBuffer,
    iframeRef,
  });

  // srcDoc lifecycle: debounced live rebuild, srcDoc ref sync, and pending inline-editor close.
  // See useHtmlSrcDocLifecycle.ts.
  useHtmlSrcDocLifecycle({
    drawer,
    mode,
    activeBuffer,
    applied,
    setApplied,
    srcDoc,
    srcDocRef,
    pendingEditorCloseRef,
    clearPendingEditorClose,
  });

  // Interactive-preview postMessage bridge + native context-menu Tauri event listeners (View source /
  // Open link in new tab / Open link externally). See HtmlDocumentBridge.tsx.
  useHtmlDocumentBridge({
    iframeRef,
    sessionId,
    lastContextTargetRef,
    handleHref,
    onRequestFind,
    resolveFindResult,
    handleInspectTarget,
    openInspectMenu,
    onPreviewScrollRef,
    scrollYRef,
    setPreviewZoomHud,
    previewZoomTimer,
    onEnterVisual: handleSafeBlockTarget,
    onEditRefused: handleEditRefused,
    onLinkHover: setHoveredLink,
  });

  useEffect(() => {
    if (!drawer) setSourceFullscreen(false);
  }, [drawer]);

  const toggleSourceDrawer = useCallback(() => {
    // Read mode: the source drawer is the editing surface — the toggle is hidden in chrome and
    // any programmatic call (the html-toggle-source event) must also be a no-op.
    if (readOnly) return;
    const identity = { documentPath: entryPath, pagePath: activePath };
    if (blockDrafts.get(identity)) {
      toast("Finish the current HTML edit before opening Source.", "error");
      richEditRef.current?.focus();
      return;
    }
    setDrawerOpen((open) => !open);
  }, [activePath, blockDrafts, entryPath, readOnly, richEditRef]);

  // Source drawer remains an explicit HTML control; F12 and inspect chords open HTML DevTools.
  useEffect(() => {
    if (!active) return;
    const onToggleSource = toggleSourceDrawer;
    window.addEventListener(documentEvent("html-toggle-source"), onToggleSource);
    return () => window.removeEventListener(documentEvent("html-toggle-source"), onToggleSource);
  }, [active, toggleSourceDrawer]);

  // Browser-style reload (the html-reload event) + the reload-pending buffer snapshot.
  const { doReload } = useHtmlReload({
    active,
    activePath,
    activeBuffer,
    activeBufferRef,
    renderedRevisionRef,
    setApplied,
    setNonce,
    onReloadFromDisk,
  });

  // Source-drawer edit/save/discard for the active page (entry vs sub routed by App).
  // Same ownership problem as a rendered-pane commit: typing here writes through a ref that does not
  // re-render App, so this pane's `text` prop lags. Route it through the identical path so the drawer
  // and the rendered pane can never disagree about which bytes are current.
  const onActiveSourceChange = commitSource;
  // Save the active page through App's session save (handles entry vs sub) so the preview reloads
  // when the saved bytes differ — consistent for the entry, sub-pages, and Ctrl+S.
  const onActiveSave = useCallback(() => {
    onSavePage(entryPath, activePath);
  }, [onSavePage, entryPath, activePath]);

  const activeName = activePath.split(/[\\/]/).pop() ?? activePath;
  const effectiveViewport = viewportMode === "auto" ? autoViewport : viewportMode;

  const viewportWidth = viewportWidthPx(effectiveViewport);

  const editRichSource = useCallback(async () => {
    const identity = { documentPath: entryPath, pagePath: activePath };
    // A modified draft is committed (Done) first so typed text survives; if the commit fails the
    // draft stays open and Source is not opened.
    const modified = blockDrafts.hasModifiedDraft(identity);
    const current = activeBufferRef.current;
    const exactRange =
      safeTarget && !modified ? (resolveSafeBlockTarget(current, safeTarget)?.range ?? null) : null;
    const result = await blockDrafts.resolve(
      identity,
      "view-change",
      modified ? "done" : "discard",
    );
    if (
      result.outcome === "discarded" ||
      result.outcome === "missing" ||
      result.outcome === "done"
    ) {
      setSafeTarget(null);
      restoreInteractiveAfterTransientEdit();
      if (exactRange) pendingInspectRangeRef.current = exactRange;
      setDrawerOpen(true);
      if (exactRange && sourceEditorRef.current) {
        window.requestAnimationFrame(() => {
          const pending = pendingInspectRangeRef.current;
          if (pending && revealSourceRange(pending)) pendingInspectRangeRef.current = null;
        });
      }
    }
  }, [
    activeBufferRef,
    activePath,
    blockDrafts,
    entryPath,
    restoreInteractiveAfterTransientEdit,
    revealSourceRange,
    safeTarget,
    setSafeTarget,
  ]);
  // In-frame ProseMirror mount target: resolve safeTarget.childPath inside the Safe iframe's DOM.
  const { inframeTarget, setInframeTarget, iframeGeneration } = useHtmlInframeTarget({
    safeIframeRef,
    safeTarget,
    mode,
    blockSnapshot,
    activePath,
    applied,
  });
  return (
    <HtmlDocumentLayout
      flags={{ readOnly, active, drawer, sourceFullscreen, mode }}
      address={{
        activePath,
        activeDirty,
        subpageStale: !onEntry && !!pageBuffers[activePath]?.stale,
        viewport: effectiveViewport,
        canGoBack,
        canGoForward,
        goHistory,
        setViewportMode,
        onReload: doReload,
        findBar,
        findMode,
        onRequestFind,
        onRequestReplace,
        onToggleSource: toggleSourceDrawer,
      }}
      rich={{
        safeTarget,
        blockSnapshot: blockSnapshot ?? undefined,
        blockIdentity,
        hasChangeView: !!onChangeView,
        richEditRef,
        richFormatting,
        setRichFormatting,
        runBlockRichOp,
        runBlockStructuralOp,
        canRunRichOp,
        canRunStructuralOp,
        propertyAttributes,
        editRichSource,
        closeInlineEditor,
        inframeTarget,
        setInframeTarget,
        iframeGeneration,
      }}
      preview={{
        splitRef,
        hoveredLink,
        pickingRoot,
        onChooseSiteRoot: () => void handleChooseSiteRoot(),
        showSiteRootNotice: !!siteRoot && siteRoot.markerFound === false,
        error,
        nonce,
        srcDoc,
        iframeRef,
        safeIframeRef,
        activePath,
        entryPath,
        siteRoot: siteRoot ?? null,
        resourcesReady: siteRoot !== undefined,
        applied,
        themeKey,
        registerSearchProvider,
        onInspectTarget: handleInspectTarget,
        onInspectMenu: openInspectMenu,
        onRequestFind,
        navigate,
        onOpenPath,
        viewportWidth,
        previewZoomHud,
        swapping,
        onBackToTop: scrollInteractivePreviewToTop,
        onResetPreviewZoom: resetInteractivePreviewZoom,
        onSafeBlockTarget: handleSafeBlockTarget,
        onLinkHover: setHoveredLink,
      }}
      source={{
        subLoaded,
        activePath,
        activeBuffer,
        activeName,
        activeDirty,
        drawerWidth,
        onStartResize: startResize,
        onToggleFullscreen: () => setSourceFullscreen((value) => !value),
        onActiveSourceChange,
        onActiveSave,
        onDiscardPage: () => {
          // An entry discard reverts App's store without re-rendering this pane's `text`
          // prop, so the parked local commit must be dropped here or it stays on screen.
          if (onEntry) setCommittedSource(null);
          onDiscardPage(entryPath, activePath);
        },
        registerSearchProvider,
        findBar,
        findMode,
        onRequestFind,
        onRequestReplace,
        sourceViewStateRef,
        sourceEditorRef,
        pendingInspectRangeRef,
        revealSourceRange,
      }}
      overlays={{
        inspectMenu,
        setInspectMenu,
        handleHref,
        copyMenuValue,
        inspectFromMenu,
        blocked,
        setBlocked,
        locateBlocked,
        copyBlocked,
      }}
    />
  );
}
