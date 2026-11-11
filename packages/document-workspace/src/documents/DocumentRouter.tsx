import {
  lazy,
  memo,
  Suspense,
  useEffect,
  type ReactNode,
  type RefObject,
  type UIEventHandler,
} from "react";
import { capabilitiesFor } from "./capabilities.js";
import { ErrorBoundary } from "../app/ErrorBoundary.js";
import type { LoadedDocument, ReadEditMode, ViewMode, ViewMeta, SearchProvider } from "@rightkit/document-core/documents/types";
import type { DocxMenuApi } from "@rightkit/docx-editor/documents/docx/menuApi";
import type { ImageSaveFunction } from "@rightkit/media-ui";
import type { HtmlPageBuffer } from "@rightkit/html-editor/documents/html/sessionTypes";
import type { MarkdownExportApi } from "@rightkit/markdown-editor/documents/markdown/export/markdownExportTypes";
import type { SourceEditorViewState } from "./txt/TxtEditor.js";
import type { PdfSaveFunction } from "./pdf/pdfEditor.js";
import type { PdfSavePlan } from "./pdf/pdfEditModel.js";
import { type DiagnosticEvent, logEvent } from "@rightkit/document-core/state/diagnostics";

// Heavy viewers — load only when their format is actually opened (keeps cold launch fast).
// MarkdownSplitPane pulls the unified/remark engine (its preview) + lazies the CodeMirror source;
// HtmlViewer pulls DOMPurify — all lazy so a cold launch or opening a plain TXT never pays for them.
const MarkdownSplitPane = lazy(() =>
  import("./markdown/MarkdownSplitPane.js").then((m) => ({ default: m.MarkdownSplitPane })),
);
const MarkdownCoreProvider = lazy(() =>
  import("@rightkit/markdown-editor/documents/markdown/MarkdownCoreProvider").then((m) => ({ default: m.MarkdownCoreProvider })),
);
const MdxSplitPane = lazy(() =>
  import("./mdx/MdxSplitPane.js").then((m) => ({ default: m.MdxSplitPane })),
);
const HtmlDocument = lazy(() =>
  import("./html/HtmlDocument.js").then((m) => ({ default: m.HtmlDocument })),
);
const DocxPane = lazy(() => import("@rightkit/docx-editor/documents/docx/DocxPane").then((m) => ({ default: m.DocxPane })));
const PdfViewer = lazy(() =>
  import("./pdf/NativePdfDocument.js").then((m) => ({ default: m.NativePdfDocument })),
);
const LivePreviewPane = lazy(() =>
  import("./web/LivePreviewPane.js").then((m) => ({ default: m.LivePreviewPane })),
);
const ImageViewer = lazy(() => import("./ImageDocument.js").then((m) => ({ default: m.ImageViewer })));
const TextEditor = lazy(() => import("./txt/TxtEditor.js").then((m) => ({ default: m.TextEditor })));

// The image viewer's React key: the containing folder, so same-folder paging reuses one instance
// (seamless swap) while a different folder remounts. Falls back to the full path if it has no parent.
const VIEWER_ROUTE_EVENTS: Record<ViewMode, DiagnosticEvent> = {
  preview: "viewer:route:preview",
  source: "viewer:route:source",
  interactive: "viewer:route:interactive",
};

const imageInstanceKey = (path: string) => path.replace(/[\\/][^\\/]*$/, "") || path;

const Loading = () => (
  <div className="h-full bg-ink p-4">
    <div className="h-full animate-pulse rounded border border-line bg-panel/40 p-5">
      <div className="mb-5 h-6 w-40 rounded bg-panel-2" />
      <div className="space-y-3">
        <div className="h-4 w-3/4 rounded bg-panel-2" />
        <div className="h-4 w-2/3 rounded bg-panel-2" />
        <div className="h-4 w-5/6 rounded bg-panel-2" />
      </div>
    </div>
  </div>
);

export const MarkdownTextFallback = ({ text }: { text: string }) => (
  <div data-vr-markdown-first-paint="" className="h-full overflow-auto bg-ink px-8 py-6">
    <pre
      className="vr-markdown-document-geometry min-h-full whitespace-pre-wrap rounded-lg px-8 py-7 font-serif text-[length:var(--doc-size)] leading-[var(--doc-leading)]"
      style={{ background: "var(--reader-surface)", color: "var(--doc-fg)" }}
    >
      {text}
    </pre>
  </div>
);

function ViewerShell({ doc, children }: { doc: LoadedDocument; children: ReactNode }) {
  return (
    <ErrorBoundary
      label={`${doc.displayName || doc.format} viewer`}
      resetKey={`${doc.path}:${doc.format}`}
      closeTabPath={doc.path}
    >
      <Suspense fallback={<Loading />}>{children}</Suspense>
    </ErrorBoundary>
  );
}

interface Props {
  doc: LoadedDocument;
  view: ViewMode;
  /** Wave 2 per-document Read/Edit (PDF: Markup) mode — "read" keeps viewers view-first,
   *  "edit" exposes the format's existing editing surface as the floating format bar. */
  readEdit?: ReadEditMode;
  devMode?: boolean;
  /** Live editable text (edits ?? content) + handlers, owned by App. */
  text: string;
  /** Per-document live edit subscription used by split panes without repainting App. */
  subscribeEdits?: (path: string, cb: () => void) => () => void;
  getEditSnapshot?: (path: string) => string | undefined;
  getExternalSeq?: (path: string) => number;
  /** App-level source selection/scroll store, retained when a dirty source pane shells. */
  sourceViewStateRef?: RefObject<Record<string, SourceEditorViewState>>;
  onChange: (text: string) => void;
  onSave: () => void;
  /** DOCX editor writes its bytes back to disk via its own File→Save. */
  onDocxSave: (buffer: ArrayBuffer) => void;
  /** DOCX edited (for unsaved-close warning). */
  onDocxChange: () => void;
  /** Register the DOCX editor's programmatic save (for the close dialog). */
  registerDocxSave: (fn: (() => Promise<ArrayBuffer | null>) | null) => void;
  /** Register the image editor export function for app-level Save / Save As / Ctrl+S. */
  registerImageSave: (fn: ImageSaveFunction | null) => void;
  /** Lift the DOCX command surface for the app's File/Format/Insert/Help menus. */
  onDocxToolbar: (api: DocxMenuApi | null) => void;
  onImageChange: () => void;
  /** Page to a neighbouring image in the same folder, replacing this tab in place (image branch). */
  onOpenImageSibling: (currentPath: string, targetPath: string) => void;
  registerSearchProvider: (provider: SearchProvider | null) => void;
  /** Keep-alive: whether this is the visible (active) pane. The real renderer doesn't branch on it
   *  (inactive panes stay mounted, just CSS-hidden by the parent); it's a hook for test harnesses. */
  active?: boolean;
  /** Restored PDF view state (page/zoom/fit) and a callback to persist changes. */
  pdfInitialView?: ViewMeta;
  onPdfViewState?: (m: ViewMeta) => void;
  /** Register the active PDF pane's print function (uses its own already-loaded document — see
   *  documents/pdf/pdfPrinting.ts printPdfDocument for why re-fetching the stream URL is unreliable). */
  registerPdfPrint?: (fn: (() => Promise<boolean>) | null) => void;
  /** Register PDF annotation serialization for app-level Save / Save As / Ctrl+S. */
  registerPdfSave?: (fn: PdfSaveFunction | null) => void;
  /** Register PDF two-phase save plan for source/OCR edits plus annotations. */
  registerPdfSavePlan?: (fn: (() => Promise<PdfSavePlan>) | null) => void;
  /** Publish PDF annotation dirtiness to app-level close, quit, & autosave handling. */
  onPdfDirtyChange?: (dirty: boolean) => void;
  /** Register canonical rendered Markdown export for app-level print, export, & clipboard commands. */
  registerMarkdownExport?: (api: MarkdownExportApi | null) => void;
  /** Markdown live view owns its scroll below fixed authoring chrome. */
  markdownScrollRef?: RefObject<HTMLDivElement | null>;
  onMarkdownScroll?: UIEventHandler<HTMLDivElement>;
  /** Shared FindBar element, placed by each searchable document toolbar. */
  findBar?: ReactNode;
  findMode?: "find" | "replace";
  /** Shell-level preview/source toggle. */
  onChangeView?: (view: ViewMode) => void;
  /** Open the shared find UI — used by the HTML preview to intercept Ctrl+F inside its iframe. */
  onRequestFind?: () => void;
  /** Close the shared find bar — used when a surface that owns its own search goes away. */
  onCloseFind?: () => void;
  onRequestReplace?: () => void;
  /** Open a brokered absolute path as a tab — used for HTML internal relative-link navigation.
   *  `background` (new-tab gesture) adds the tab without switching to it. */
  onOpenPath?: (path: string, opts?: { background?: boolean }) => void;
  /** MDX branch only: reports every grant minted for the document's compile-time imports + in-preview
   *  link clicks, so the owning tab's close can revoke them. */
  onMdxImportGranted?: (entryPath: string, grantedPath: string) => void;
  /** HTML site-session state + handlers (consumed only by the html branch). */
  html?: {
    pageBuffers: Record<string, HtmlPageBuffer>;
    dirtyPagePaths: string[];
    onEnsurePage: (entryPath: string, pagePath: string) => void;
    onPageChange: (entryPath: string, pagePath: string, text: string) => void;
    onSetActivePage: (entryPath: string, pagePath: string) => void;
    onSavePage: (entryPath: string, pagePath: string) => void;
    onDiscardPage: (entryPath: string, pagePath: string) => void;
    onReloadFromDisk: (path: string, options?: { force?: boolean }) => void;
    initialPreviewScrollY: number;
    onPreviewScroll: (y: number) => void;
  };
}

function DocumentRouterImpl({
  doc,
  view,
  readEdit = "read",
  devMode,
  text,
  subscribeEdits,
  getEditSnapshot,
  getExternalSeq,
  sourceViewStateRef,
  onChange,
  onSave,
  onDocxSave,
  onDocxChange,
  registerDocxSave,
  registerImageSave,
  onDocxToolbar,
  onImageChange,
  onOpenImageSibling,
  registerSearchProvider,
  active = false,
  pdfInitialView,
  onPdfViewState,
  registerPdfPrint,
  registerPdfSave,
  registerPdfSavePlan,
  onPdfDirtyChange,
  registerMarkdownExport,
  markdownScrollRef,
  onMarkdownScroll,
  findBar,
  findMode,
  onChangeView,
  onRequestFind,
  onCloseFind,
  onRequestReplace,
  onOpenPath,
  onMdxImportGranted,
  html,
}: Props) {
  useEffect(() => {
    logEvent(VIEWER_ROUTE_EVENTS[view]);
  }, [view]);

  useEffect(() => {
    if (doc.format === "md") {
      void import("./markdown/MarkdownSplitPane.js");
      void import("@rightkit/markdown-editor/documents/markdown/MarkdownCoreProvider");
      // The source drawer (CodeMirror) is not needed for first paint: warm it after the preview
      // has painted so it stays off the cold-mount critical path but is ready when opened.
      const warmSource = () => {
        void import("./txt/TxtEditor.js");
      };
      if (typeof window.requestIdleCallback === "function") {
        window.requestIdleCallback(warmSource, { timeout: 2000 });
      } else {
        window.setTimeout(warmSource, 300);
      }
    } else if (doc.format === "mdx") {
      // MDX preview is a separate subtree (sandboxed iframe). The compile path lives in mdxCompiler
      // and the iframe runtime is bundled in mdxBridgeScript; the splitter + source editor are
      // shared with the markdown side, so we prefetch both formats' chunks on first open.
      void import("./mdx/MdxSplitPane.js");
      void import("@rightkit/markdown-editor/documents/mdx/mdxCompiler");
      void import("./markdown/MarkdownSplitPane.js"); // shared TxtEditor + outline code
      void import("./txt/TxtEditor.js");
    } else if (doc.format === "html") {
      void import("./txt/TxtEditor.js");
    }
  }, [doc.format]);

  // Every viewer EXCEPT image is keyed by `doc.path` so switching documents REMOUNTS it. That makes
  // the search provider register on mount and unregister on unmount through the normal lifecycle — no
  // separate reset effect (which raced the lazy child's registration), and no "re-register on doc
  // change" hacks inside the viewers. Same-format switches (html→html, pdf→pdf, txt→txt) previously
  // reused one instance whose stable-deps registration effect never re-ran, so a leaving-doc reset
  // left the provider null and Ctrl+F couldn't open. Keying fixes that whole class of bug. The key is
  // stable while editing one doc (only switches remount), so the caret/scroll within a doc is intact.
  // The IMAGE viewer is the deliberate exception: it's keyed by its FOLDER (imageInstanceKey), not the
  // file, so same-folder paging reuses one mounted instance (seamless swap). That's safe because the
  // image viewer registers NO search provider and resets its per-image edit/view state internally —
  // do NOT "fix" it back to doc.path or you'll reintroduce the remount → blank → resize on paging.
  switch (doc.format) {
    case "web":
      return (
        <ViewerShell doc={doc}>
          <LivePreviewPane key={doc.path} doc={doc} />
        </ViewerShell>
      );
    case "pdf":
      return (
        <ViewerShell doc={doc}>
          <PdfViewer
            key={doc.path}
            documentPath={doc.path}
            url={doc.assetUrl}
            active={active}
            readOnly={readEdit !== "edit"}
            initialView={pdfInitialView}
            onViewState={onPdfViewState}
            registerPrint={registerPdfPrint}
            registerPdfSave={registerPdfSave}
            registerPdfSavePlan={registerPdfSavePlan}
            onDirtyChange={onPdfDirtyChange}
            registerSearchProvider={registerSearchProvider}
            findBar={findBar}
            findMode={findMode}
            onRequestFind={onRequestFind}
            onRequestReplace={onRequestReplace}
          />
        </ViewerShell>
      );
    case "docx":
      return (
        <ViewerShell doc={doc}>
          <DocxPane
            key={doc.path}
            documentPath={doc.path}
            url={doc.assetUrl}
            readOnly={readEdit !== "edit"}
            initialBase64={doc.assetUrl ? undefined : doc.content}
            onSave={onDocxSave}
            onChange={onDocxChange}
            registerSave={registerDocxSave}
            registerMenu={onDocxToolbar}
            registerSearchProvider={registerSearchProvider}
            findBar={findBar}
            findMode={findMode}
            onRequestFind={onRequestFind}
            onRequestReplace={onRequestReplace}
          />
        </ViewerShell>
      );
    case "image":
      return (
        <ViewerShell doc={doc}>
          {/* Keyed by FOLDER, not file: paging to a sibling image keeps this instance mounted (the
              viewport stays measured and the new image is decoded before it's swapped in), so folder
              paging is seamless instead of a remount → blank → resize. Cross-folder images remount. */}
          <ImageViewer
            key={imageInstanceKey(doc.path)}
            doc={doc}
            active={active}
            readOnly={readEdit !== "edit"}
            onChange={onImageChange}
            registerSave={registerImageSave}
            onOpenSibling={onOpenImageSibling}
          />
        </ViewerShell>
      );
    case "txt":
      return (
        <ViewerShell doc={doc}>
          <TextEditor
            key={doc.path}
            documentPath={doc.path}
            language="plain"
            text={text}
            onChange={onChange}
            onSave={onSave}
            registerSearchProvider={registerSearchProvider}
            findBar={findBar}
            findMode={findMode}
            onRequestFind={onRequestFind}
            onRequestReplace={onRequestReplace}
            active={active}
            readOnly={readEdit !== "edit"}
            initialViewState={sourceViewStateRef?.current[doc.path] ?? null}
            onViewStateChange={(state) => {
              if (sourceViewStateRef) sourceViewStateRef.current[doc.path] = state;
            }}
          />
        </ViewerShell>
      );
    case "html":
      if (!html) throw new Error("HTML router props are missing");
      return (
        <ViewerShell doc={doc}>
          <HtmlDocument
            key={doc.path}
            entryPath={doc.path}
            view={view}
            text={text}
            onChange={onChange}
            registerSearchProvider={registerSearchProvider}
            findBar={findBar}
            findMode={findMode}
            onRequestFind={onRequestFind}
            onRequestReplace={onRequestReplace}
            onCloseFind={onCloseFind}
            onOpenPath={onOpenPath}
            onChangeView={onChangeView}
            active={active}
            readOnly={readEdit !== "edit"}
            pageBuffers={html.pageBuffers}
            dirtyPagePaths={html.dirtyPagePaths}
            onEnsurePage={html.onEnsurePage}
            onPageChange={html.onPageChange}
            onSetActivePage={html.onSetActivePage}
            onSavePage={html.onSavePage}
            onDiscardPage={html.onDiscardPage}
            onReloadFromDisk={html.onReloadFromDisk}
            initialPreviewScrollY={html.initialPreviewScrollY}
            onPreviewScroll={html.onPreviewScroll}
            sourceViewStateRef={sourceViewStateRef}
          />
        </ViewerShell>
      );
    case "mdx":
      // MDX is JSX-execution content — it can NEVER mount in the app's React tree (it would
      // share window/Tauri/localStorage with the shell). The MdxSplitPane owns a sandboxed
      // iframe (`sandbox="allow-scripts"`, never `allow-same-origin`) and keeps the markdown
      // source drawer verbatim. Compile + postMessage live in MdxPreviewFrame.
      return (
        <ViewerShell doc={doc}>
          <MdxSplitPane
            key={doc.path}
            text={text}
            documentPath={doc.path}
            subscribeEdits={subscribeEdits}
            getEditSnapshot={getEditSnapshot}
            showSource={view === "source"}
            onCloseFind={onCloseFind}
            devMode={devMode}
            readOnly={readEdit !== "edit"}
            onChange={onChange}
            onSave={onSave}
            registerSearchProvider={registerSearchProvider}
            findBar={findBar}
            findMode={findMode}
            onRequestFind={onRequestFind}
            onRequestReplace={onRequestReplace}
            onOpenPath={onOpenPath}
            onImportGranted={onMdxImportGranted}
            active={active}
          />
        </ViewerShell>
      );
    default:
      // md — preview is ALWAYS mounted; the source drawer toggles via `showSource` (no
      // component swap, so opening source no longer remounts/refreshes the preview, like HTML's drawer).
      return (
        <ViewerShell doc={doc}>
          <Suspense fallback={<MarkdownTextFallback text={text} />}>
            <MarkdownCoreProvider
              documentId={doc.path}
              initialText={text}
              onChange={onChange}
              subscribeEdits={subscribeEdits}
              getEditSnapshot={getEditSnapshot}
              getExternalSeq={getExternalSeq}
            >
              <MarkdownSplitPane
                key={doc.path}
                text={text}
                documentPath={doc.path}
                subscribeEdits={subscribeEdits}
                getEditSnapshot={getEditSnapshot}
                showSource={view === "source"}
                onCloseFind={onCloseFind}
                devMode={devMode}
                readOnly={readEdit !== "edit"}
                onChange={onChange}
                onSave={onSave}
                registerSearchProvider={registerSearchProvider}
                findBar={findBar}
                findMode={findMode}
                onRequestFind={onRequestFind}
                onRequestReplace={onRequestReplace}
                onOpenPath={onOpenPath}
                onChangeView={onChangeView}
                active={active}
                registerMarkdownExport={registerMarkdownExport}
                scrollContainerRef={markdownScrollRef}
                onScroll={onMarkdownScroll}
                sourceViewStateRef={sourceViewStateRef}
              />
            </MarkdownCoreProvider>
          </Suspense>
        </ViewerShell>
      );
  }
}

// Active + MRU + protected tabs may stay mounted (see App.tsx). Without this memo, shell updates
// cascade into every live pane; with many tabs open that can make the main thread drop keystrokes.
// Format-aware: PDF, Image, HTML, and DOCX viewers consume active ownership for side effects
// (shortcut/scroll state, save state, active print ownership, or the global DOCX toolbar). Web can
// stay isolated from an active-only wrapper change. Find placement is viewer-owned, so it is compared too.
const formatUsesActive = (doc: LoadedDocument | undefined) =>
  !!doc && capabilitiesFor(doc.format).canEdit;

const formatUsesFindBar = (doc: LoadedDocument | undefined) =>
  doc?.format !== "image" && doc?.format !== "web";

// T5-1: `html` used to be entirely omitted from the memo comparison, so an HTML tab's session props
// (pageBuffers/dirtyPagePaths) could change underneath the memo without ever re-rendering
// the pane. Compare the fields
// that actually drive its rendering. App.tsx's htmlPropsFor (T2-3) keeps this prop referentially
// STABLE per entryPath unless that path's own slice changed, so the `prev === next` fast path is the
// common case for every OTHER open html tab — this only forces a render when html truly changed for
// THIS doc, it never widens re-renders beyond that.
function htmlPropsEqual(prev: Props["html"], next: Props["html"]): boolean {
  if (prev === next) return true;
  if (!prev || !next) return false;
  if (prev.pageBuffers !== next.pageBuffers) return false;
  if (prev.dirtyPagePaths !== next.dirtyPagePaths) {
    if (prev.dirtyPagePaths.length !== next.dirtyPagePaths.length) return false;
    for (let i = 0; i < prev.dirtyPagePaths.length; i++) {
      if (prev.dirtyPagePaths[i] !== next.dirtyPagePaths[i]) return false;
    }
  }
  return true;
}

type MemoProps = Pick<
  Props,
  | "doc"
  | "active"
  | "view"
  | "readEdit"
  | "text"
  | "findBar"
  | "findMode"
  | "onRequestReplace"
  | "onChangeView"
  | "html"
>;

export function documentRouterPropsEqual(prev: MemoProps, next: MemoProps) {
  if ((formatUsesActive(prev.doc) || formatUsesActive(next.doc)) && prev.active !== next.active) {
    return false;
  }
  if (
    (prev.doc.format === "html" || next.doc.format === "html") &&
    !htmlPropsEqual(prev.html, next.html)
  ) {
    return false;
  }
  return (
    prev.doc === next.doc &&
    prev.view === next.view &&
    prev.readEdit === next.readEdit &&
    prev.text === next.text &&
    prev.findMode === next.findMode &&
    prev.onChangeView === next.onChangeView &&
    !!prev.onRequestReplace === !!next.onRequestReplace &&
    (!(formatUsesFindBar(prev.doc) || formatUsesFindBar(next.doc)) || prev.findBar === next.findBar)
  );
}

export const DocumentRouter = memo(DocumentRouterImpl, documentRouterPropsEqual);
