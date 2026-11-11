import {
  memo,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type ComponentProps,
  type RefObject,
} from "react";
import { effectiveReadEdit } from "./shell/readEdit.js";
import { DocumentRouter } from "../documents/DocumentRouter.js";
import { capabilitiesFor } from "../documents/capabilities.js";
import { isUntitled } from "@rightkit/document-core/documents/types";
import type { LoadedDocument, SearchProvider, ViewMeta, ViewMode } from "@rightkit/document-core/documents/types";
import type { DocxMenuApi } from "@rightkit/docx-editor/documents/docx/menuApi";
import type { ImageSaveFunction } from "@rightkit/media-ui";
import type { MarkdownExportApi } from "@rightkit/markdown-editor/documents/markdown/export/markdownExportTypes";
import type { SourceEditorViewState } from "../documents/txt/TxtEditor.js";
import type { PdfSaveFunction } from "../documents/pdf/pdfEditor.js";
import type { PdfSavePlan } from "../documents/pdf/pdfEditModel.js";
import { PaneActiveContext } from "@rightkit/document-core/app/shell/docBar";
import { hostAppName } from "../appName.js";

// Inactive DOCX/PDF panes must not publish toolbar or view-state updates, but their callbacks also
// must not change identity on every App render (their effects depend on these props).
const inactiveDocxToolbar = (_api: DocxMenuApi | null) => {};
const inactivePdfViewState = (_view: ViewMeta) => {};
const inactivePdfPrint = (_fn: (() => Promise<boolean>) | null) => {};
const inactiveMarkdownExport = (_api: MarkdownExportApi | null) => {};

type LiveDocumentRouterProps = Omit<ComponentProps<typeof DocumentRouter>, "text"> & {
  getEditSnapshot: (path: string) => string | undefined;
};

// Keystrokes live inside CodeMirror + editsRef and should not re-render the whole viewer tree.
// External writes still bump App once, so this snapshot re-syncs reload/save/restore paths without
// subscribing the active editor shell to every keypress.
function LiveDocumentRouter({ doc, getEditSnapshot, ...props }: LiveDocumentRouterProps) {
  // Local keystrokes already live inside each editor + edit store. Freeze this input until disk/doc
  // identity changes; otherwise unrelated shell bookkeeping can feed a newer snapshot back through
  // DocumentRouter and repaint a heavy viewer mid-edit. Cold remounts still read latest snapshot.
  const routerText = useMemo(
    () => getEditSnapshot(doc.path) ?? doc.content,
    [doc, getEditSnapshot],
  );
  return <DocumentRouter {...props} doc={doc} text={routerText} />;
}

// T2-3: memoized so an App re-render unrelated to the open-tab set (zoomHud toggle, sidebar collapse,
// settings dialog, etc.) doesn't re-run this component's `.map()` over every mounted doc. The bigger win
// is downstream — each DocumentRouter instance below already bails via its own memo comparator
// (documentRouterPropsEqual) — but that only helps if THIS component doesn't rebuild every child element
// from scratch first. Most handler props here are already useCallback-stable from their owning hooks
// (useDocuments/useDocumentTabs/useViewState); App.tsx now also stabilizes onDocumentWheel and
// onOpenPath, which used to be rebuilt (or an inline arrow) on every render.
function DocumentRouterTableImpl({
  mountedDocs,
  activePath,
  livePaths,
  renderedPaths,
  views,
  contentScrollRef,
  onContentScroll,
  onDocumentWheel,
  subscribeEdits,
  getEditSnapshot,
  getExternalSeq,
  sourceViewStateRef,
  onTxtChange,
  onSave,
  onDocxSaveAs,
  onDocxSave,
  onDocxChange,
  docxSaver,
  imageSaver,
  docxSaversRef,
  pdfSaversRef,
  pdfSavePlansRef,
  imageSaversRef,
  setDocxMenu,
  setPdfPrint,
  setMarkdownExport,
  onImageChange,
  onPdfDirtyChange,
  openImageSibling,
  registerSearchProviderFor,
  onPdfViewState,
  findBarEl,
  findMode,
  canReplaceSearch,
  onChangeView,
  toggleFind,
  closeFind,
  onOpenPath,
  trackMdxGrant,
  htmlPropsFor,
}: {
  mountedDocs: LoadedDocument[];
  activePath: string | null;
  /** Paths currently allowed to own a heavy DocumentRouter tree. */
  livePaths: ReadonlySet<string>;
  renderedPaths: Set<string>;
  views: Record<string, ViewMode>;
  /** Wave 2 ephemeral per-doc Read/Edit modes — absent means "read". */
  contentScrollRef: RefObject<HTMLDivElement | null>;
  onContentScroll: (e: React.UIEvent<HTMLDivElement>) => void;
  onDocumentWheel: (e: React.WheelEvent<HTMLDivElement>) => void;
  subscribeEdits: (path: string, cb: () => void) => () => void;
  getEditSnapshot: (path: string) => string | undefined;
  getExternalSeq: (path: string) => number;
  /** App-owned source selection/scroll survives a dirty router shell + cold remount. */
  sourceViewStateRef: RefObject<Record<string, SourceEditorViewState>>;
  onTxtChange: (path: string, prevContent: string, text: string) => void;
  onSave: (doc: LoadedDocument) => void;
  onDocxSaveAs: (doc: LoadedDocument, buf: ArrayBuffer) => void;
  onDocxSave: (path: string, buf: ArrayBuffer) => void;
  onDocxChange: (path: string) => void;
  docxSaver: RefObject<(() => Promise<ArrayBuffer | null>) | null>;
  imageSaver: RefObject<ImageSaveFunction | null>;
  docxSaversRef: RefObject<Map<string, () => Promise<ArrayBuffer | null>>>;
  pdfSaversRef: RefObject<Map<string, PdfSaveFunction>>;
  pdfSavePlansRef: RefObject<Map<string, () => Promise<PdfSavePlan>>>;
  imageSaversRef: RefObject<Map<string, ImageSaveFunction>>;
  setDocxMenu: (api: DocxMenuApi | null) => void;
  setPdfPrint: (fn: (() => Promise<boolean>) | null) => void;
  setMarkdownExport: (api: MarkdownExportApi | null) => void;
  onImageChange: (path: string) => void;
  onPdfDirtyChange: (path: string, dirty: boolean) => void;
  openImageSibling: (currentPath: string, targetPath: string) => void;
  registerSearchProviderFor: (path: string) => (provider: SearchProvider | null) => void;
  onPdfViewState: (m: ViewMeta) => void;
  findBarEl: React.ReactNode;
  findMode?: "find" | "replace";
  canReplaceSearch: boolean;
  onChangeView: (view: ViewMode) => void;
  toggleFind: (replace: boolean) => void;
  closeFind: () => void;
  onOpenPath: (path: string, opts?: { background?: boolean }) => void;
  trackMdxGrant: (entryPath: string, grantedPath: string) => void;
  htmlPropsFor: (entryPath: string) => ComponentProps<typeof DocumentRouter>["html"] | undefined;
}) {
  const activePathRef = useRef(activePath);
  const [previousActivePath, setPreviousActivePath] = useState<string | null>(null);
  const [resizeActive, setResizeActive] = useState(false);

  useLayoutEffect(() => {
    if (activePathRef.current === activePath) return;
    setPreviousActivePath(activePathRef.current);
    activePathRef.current = activePath;
  }, [activePath]);

  useEffect(() => {
    let timer: number | undefined;
    let frame: number | undefined;
    const onResize = (): void => {
      setResizeActive(true);
      if (timer) window.clearTimeout(timer);
      if (frame) window.cancelAnimationFrame(frame);
      timer = window.setTimeout(() => {
        frame = window.requestAnimationFrame(() => setResizeActive(false));
      }, 120);
    };
    window.addEventListener("resize", onResize);
    return () => {
      window.removeEventListener("resize", onResize);
      if (timer) window.clearTimeout(timer);
      if (frame) window.cancelAnimationFrame(frame);
    };
  }, []);

  // During the activation render, the ref still points at the pane that was active in the previous
  // commit. Keep that router live synchronously so a fast switch cannot unmount it before App's
  // after-paint MRU bookkeeping catches up.
  const transitionPreviousPath =
    activePathRef.current !== activePath ? activePathRef.current : previousActivePath;

  return (
    <>
      {mountedDocs.map((doc) => {
        const isActive = doc.path === activePath;
        const dView = views[doc.path] ?? "preview";
        const markdownOwnsScroll = doc.format === "md";
        const dTextLike = capabilitiesFor(doc.format).canReplace;
        // Background/restored tabs stay as lightweight shells. Active bypasses deferred bookkeeping:
        // its router mounts in this same commit, so first visit and cold revisit never paint an empty
        // shell. App defers only eviction/cleanup after activation has settled.
        const hasRendered = isActive || renderedPaths.has(doc.path);
        const isLive =
          isActive ||
          doc.path === transitionPreviousPath ||
          (hasRendered && livePaths.has(doc.path));
        // Keep only the immediately previous pane paint-warm. Warming every rendered document makes
        // each resize reflow every open Markdown/HTML tree & blocks window IPC for seconds.
        const isPaintWarm = !resizeActive && doc.path === transitionPreviousPath;
        return (
          <div
            key={doc.tabKey ?? doc.path}
            // Hide inactive panes with VISIBILITY, never `display:none`: WebView2/Chromium reloads a
            // srcdoc iframe when its subtree goes display:none→visible (the React component stays
            // mounted, but the iframe content is discarded and re-run — the "reload on switch" bug).
            // visibility:hidden keeps the iframe alive; the active pane sits on top (z-index) and is
            // the only one that takes pointer/scroll/focus.
            aria-hidden={isActive ? undefined : true}
            inert={isActive ? undefined : true}
            data-vr-document-path={doc.path}
            data-vr-pane-state={isActive ? "active" : isPaintWarm ? "warm" : "cold"}
            data-vr-router-state={isLive ? "live" : hasRendered ? "shell" : "deferred"}
            id={isActive ? "vr-main-content" : undefined}
            tabIndex={isActive ? -1 : undefined}
            ref={isActive && !markdownOwnsScroll ? contentScrollRef : undefined}
            onScroll={isActive && !markdownOwnsScroll ? onContentScroll : undefined}
            onWheelCapture={isActive ? onDocumentWheel : undefined}
            {...(isActive && !markdownOwnsScroll ? { "data-document-scroll": "" } : {})}
            style={
              isActive
                ? {
                    contentVisibility: "visible",
                    visibility: "visible",
                    opacity: 1,
                    willChange: "opacity",
                    zIndex: 1,
                  }
                : isPaintWarm
                  ? {
                      contentVisibility: "visible",
                      visibility: "visible",
                      opacity: 0,
                      willChange: "opacity",
                      zIndex: 0,
                      pointerEvents: "none",
                    }
                  : {
                      contentVisibility: "hidden",
                      visibility: "hidden",
                      zIndex: 0,
                      pointerEvents: "none",
                    }
            }
            className={`absolute inset-0 outline-none ${markdownOwnsScroll ? "overflow-hidden" : "overflow-auto"}`}
          >
            {isLive ? (
              <PaneActiveContext.Provider value={isActive}>
                {dTextLike && doc.lossy ? (
                  <div className="mx-4 mt-4 rounded border border-copper/40 bg-copper/5 p-3 font-sans text-xs text-muted">
                    This file couldn’t be decoded cleanly even after detecting its encoding (
                    {doc.encoding}); some bytes were shown as replacement characters. Keep a backup
                    if the exact bytes matter.
                  </div>
                ) : (
                  dTextLike &&
                  doc.encoding &&
                  doc.encoding !== "UTF-8" && (
                    <div className="mx-4 mt-4 rounded border border-line bg-panel/40 p-3 font-sans text-xs text-muted">
                      Detected <span className="text-copper-hi">{doc.encoding}</span> encoding.
                      {hostAppName()} preserves it when you save (it switches to UTF-8 only if you add
                      characters that encoding can’t represent).
                    </div>
                  )
                )}
                <LiveDocumentRouter
                  doc={doc}
                  active={isActive}
                  view={dView}
                  readEdit={effectiveReadEdit(doc)}
                  subscribeEdits={subscribeEdits}
                  getEditSnapshot={getEditSnapshot}
                  getExternalSeq={getExternalSeq}
                  sourceViewStateRef={sourceViewStateRef}
                  onChange={(text) => onTxtChange(doc.path, doc.content, text)}
                  onSave={() => onSave(doc)}
                  onDocxSave={(buf) => {
                    if (isUntitled(doc.path)) {
                      void onDocxSaveAs(doc, buf);
                    } else {
                      void onDocxSave(doc.path, buf);
                    }
                  }}
                  onDocxChange={() => onDocxChange(doc.path)}
                  registerDocxSave={(fn) => {
                    if (fn) docxSaversRef.current?.set(doc.path, fn);
                    else docxSaversRef.current?.delete(doc.path);
                    // Compare by path instead of closure-captured isActive so the active-pointer
                    // update works regardless of when the viewer mounts (deferred by App.tsx).
                    if (doc.path === activePath) docxSaver.current = fn;
                  }}
                  registerImageSave={(fn) => {
                    if (fn) imageSaversRef.current?.set(doc.path, fn);
                    else imageSaversRef.current?.delete(doc.path);
                    if (doc.path === activePath) imageSaver.current = fn;
                  }}
                  onDocxToolbar={isActive ? setDocxMenu : inactiveDocxToolbar}
                  registerPdfPrint={isActive ? setPdfPrint : inactivePdfPrint}
                  registerPdfSave={(fn) => {
                    if (fn) pdfSaversRef.current?.set(doc.path, fn);
                    else pdfSaversRef.current?.delete(doc.path);
                  }}
                  registerPdfSavePlan={(fn) => {
                    if (fn) pdfSavePlansRef.current?.set(doc.path, fn);
                    else pdfSavePlansRef.current?.delete(doc.path);
                  }}
                  onPdfDirtyChange={(dirty) => onPdfDirtyChange(doc.path, dirty)}
                  registerMarkdownExport={isActive ? setMarkdownExport : inactiveMarkdownExport}
                  markdownScrollRef={isActive && markdownOwnsScroll ? contentScrollRef : undefined}
                  onMarkdownScroll={isActive && markdownOwnsScroll ? onContentScroll : undefined}
                  onImageChange={() => onImageChange(doc.path)}
                  onOpenImageSibling={openImageSibling}
                  registerSearchProvider={registerSearchProviderFor(doc.path)}
                  pdfInitialView={doc.viewMeta}
                  onPdfViewState={isActive ? onPdfViewState : inactivePdfViewState}
                  findBar={isActive ? findBarEl : undefined}
                  findMode={isActive ? findMode : undefined}
                  onChangeView={onChangeView}
                  onCloseFind={closeFind}
                  onRequestFind={() => toggleFind(false)}
                  onRequestReplace={
                    isActive && canReplaceSearch ? () => toggleFind(true) : undefined
                  }
                  onOpenPath={(p, opts) => void onOpenPath(p, opts)}
                  onMdxImportGranted={trackMdxGrant}
                  html={doc.format === "html" ? htmlPropsFor(doc.path) : undefined}
                />
              </PaneActiveContext.Provider>
            ) : null}
          </div>
        );
      })}
    </>
  );
}

export const DocumentRouterTable = memo(DocumentRouterTableImpl);
