import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  createContext,
  useContext,
  type CSSProperties,
  type ReactNode,
} from "react";
import * as pdfjs from "pdfjs-dist";
import type { PDFDocumentProxy, PDFPageProxy, PageViewport } from "pdfjs-dist";
import { createPdfLinkService, type PdfLinkActions } from "./linkService.js";
import { ensurePdfWorker } from "./pdfJs.js";
import { boundedCanvasDpr, clampScale, renderPageLimit, viewportRotation, withTimeout } from "./shared.js";
import { attachPasswordHandler, createLoadTimer, type PdfPasswordHandler } from "./password.js";
import { pdfDocumentInit, type PdfDocumentOptions } from "./loader.js";
import { createPdfRangeTransport, pdfRangeDocumentInit, type PdfHttpRangeTransport } from "./rangeTransport.js";
import { PdfPasswordPrompt } from "./PdfPasswordPrompt.js";
import { readPdfPageText, renderPdfTextLayer, type PdfTextContentMode } from "./textLayer.js";
import { createRegexSearcher, findLiteralMatches, findRegexMatches } from "./searchUtils.js";
import { highlightSpan } from "./shared.js";
import type {
  OutlineNode,
  PdfDocumentInfo,
  PdfFitMode,
  PdfPanel,
  PdfSearchOptions,
  PdfSearchResult,
  PdfSource,
  PdfViewState,
  PdfViewerAdapters,
  Size,
} from "./types.js";

const PAGE_LOAD_TIMEOUT = 45_000;
const MAX_TEXT = 2_000_000;

export interface PdfViewerProps {
  source: PdfSource;
  adapters?: PdfViewerAdapters;
  initialView?: Partial<PdfViewState>;
  onViewState?: (state: PdfViewState) => void;
  onDocumentInfo?: (info: PdfDocumentInfo) => void;
  onError?: (error: Error) => void;
  onLoadingChange?: (loading: boolean) => void;
  className?: string;
  toolbar?: ReactNode;
  /** Hide built-in controls while retaining keyboard and programmatic navigation. */
  hideToolbar?: boolean;
  /** Render a find input in the toolbar. Defaults to true. */
  findBar?: boolean;
  /** Called when an encrypted PDF needs a password (`"need"`) or the last one was wrong
   *  (`"incorrect"`). Return the password to retry, or null/undefined to give up: the viewer then
   *  shows no document and reports a `PdfPasswordCancelledError` through `onError`. Absent: an
   *  encrypted PDF fails through `onError` as before. Presence is read when loading starts. */
  onPassword?: PdfPasswordHandler;
  /** Render at most this many pages (from page 1). Later pages stay as empty placeholders, are
   *  never rendered, and are skipped by the thumbnails and find. Absent/invalid: unlimited. */
  maxRenderPages?: number;
  /** Notice shown above the pages while `maxRenderPages` hides some of them (`shown` of `total`).
   *  A node, or a function of `{ shown, total }`. Rendered in `<p class="rk-pdf-notice" role="status">`.
   *  Absent: no notice, and the markup is exactly as before. */
  truncationNotice?: ReactNode | ((info: { shown: number; total: number }) => ReactNode);
  /** pdf.js `getDocument` options passed beside the source, for example `wasmUrl`, `cMapUrl`,
   *  `standardFontDataUrl`. Read when loading starts, like `onPassword`: changing it does not reload
   *  a document already open. Absent: pdf.js defaults, exactly as before. */
  documentOptions?: PdfDocumentOptions;
  /** Show a "Regex" checkbox in the find bar. Off by default, so the markup is unchanged. When
   *  checked, the query is a regular expression, bounded by `regexPatternError`; a refused or invalid
   *  pattern finds nothing. */
  regexFind?: boolean;
  /** Where the text layer and find read page text from: `"getTextContent"` (default) or
   *  `"streamTextContent"` (a stream the pdf.js TextLayer consumes chunk by chunk; some WebKit builds
   *  only finish a stream-backed layer). Find reads the same source. Read when a page renders. */
  textContentMode?: PdfTextContentMode;
  /** Free the rasters of pages that scrolled well out of view (past 150% of the viewport height) and
   *  render again when they come back; pages render within 50%. Off by default: rendered pages stay
   *  until the zoom or rotation changes. Pages with a `pageLayers` layer have it cleaned up on unload. */
  unloadOffscreenPages?: boolean;
  /** Mount pdf.js's AnnotationLayer above the text layer, so links in the document are clickable
   *  (external ones go to `adapters.openExternal` after the http(s)/mailto check; internal ones and
   *  the page actions navigate). Form widgets are not rendered. Off by default. */
  annotationLayer?: boolean;
  /** Highlight find matches in the rendered text layer (`<mark>`, the active match in the stronger
   *  colour). Off by default: find then only counts and scrolls. A match that spans two text-layer
   *  runs is counted but not highlighted. */
  searchLayer?: boolean;
  /** Mount a host-owned layer (for example an editing surface) on every rendered page, after the
   *  text and annotation layers. Called with the page's slot element; return a cleanup function (or a
   *  promise of one) to run when the page is unloaded, re-rendered or the document closes. The
   *  viewer never edits; this is the only extension point for a layer that does. */
  pageLayers?: PdfPageLayerMount;
  /** Show the built-in password dialog when an encrypted PDF needs a password. Ignored when
   *  `onPassword` is set (the host's dialog wins). Cancelling reports a `PdfPasswordCancelledError`
   *  through `onError`. Off by default. */
  passwordPrompt?: boolean;
}

/** Context handed to `PdfViewerProps.pageLayers`. */
export interface PdfPageLayerContext {
  pageNumber: number;
  /** The page's container; the canvas and the viewer's layers are already in it. */
  slot: HTMLElement;
  page: PDFPageProxy;
  /** The viewport the page was rendered with (rotation and scale applied). */
  viewport: PageViewport;
}

export type PdfPageLayerMount = (context: PdfPageLayerContext) => void | (() => void) | Promise<void | (() => void)>;

// pdf.js AnnotationLayer takes a struct-tree layer; this viewer builds no accessibility tree.
const NULL_STRUCT_TREE_LAYER = {
  async getAriaAttributes(): Promise<Map<string, string> | undefined> {
    return undefined;
  },
};

const RENDER_ROOT_MARGIN = "700px 0px";
// With unloading on, the render band is narrower than the unload band: a page renders within 50% of
// the viewport and is freed only after leaving 150%, so reversing near the edge does not churn rasters.
const UNLOAD_RENDER_ROOT_MARGIN = "50% 0px";
const UNLOAD_ROOT_MARGIN = "150% 0px";

interface PageState {
  width: number;
  height: number;
  rotation: number;
  error?: string;
}

const identityIds = new WeakMap<object, number>();
let nextIdentity = 1;
function identityOf(value: object): number {
  let id = identityIds.get(value);
  if (id === undefined) { id = nextIdentity++; identityIds.set(value, id); }
  return id;
}

/** Document identity: URL text, or the identity of the supplied buffer/loader (plus view window and
 *  optional `revision`). Two different same-sized PDFs or two loader closures with identical source
 *  text therefore never collide. */
function sourceKey(source: PdfSource): string {
  const revision = "revision" in source && source.revision !== undefined ? String(source.revision) : "";
  // Structured tuple: arbitrary URL/revision text can never collide through a delimiter.
  if (source.kind === "url") return JSON.stringify(["url", source.url, revision]);
  if (source.kind === "range") return JSON.stringify(["range", source.url, source.chunkSize ?? null, source.headers ? Object.entries(source.headers) : null, revision]);
  if (source.kind === "loader") return JSON.stringify(["loader", identityOf(source.load), revision]);
  return JSON.stringify(["bytes", identityOf(source.data.buffer), source.data.byteOffset, source.data.byteLength, revision]);
}

async function loadSource(source: Exclude<PdfSource, { kind: "range" }>): Promise<Uint8Array | string> {
  if (source.kind === "url") return source.url;
  if (source.kind === "bytes") return new Uint8Array(source.data);
  const bytes = await source.load();
  return new Uint8Array(bytes);
}

function normalizeRotation(rotation: number): number {
  const value = Math.round(rotation / 90) * 90;
  return ((value % 360) + 360) % 360;
}

function fitScale(mode: Exclude<PdfFitMode, null>, host: HTMLElement, size: Size, rotation: number): number {
  const swapped = rotation % 180 !== 0;
  const width = swapped ? size.h : size.w;
  const height = swapped ? size.w : size.h;
  const availableWidth = Math.max(1, host.clientWidth - 32);
  const availableHeight = Math.max(1, host.clientHeight - 32);
  const value = mode === "width" ? availableWidth / width : Math.min(availableWidth / width, availableHeight / height);
  return clampScale(value);
}

function usePdfDocument(source: PdfSource, onError?: (error: Error) => void, onLoadingChange?: (loading: boolean) => void, onPassword?: PdfPasswordHandler, documentOptions?: PdfDocumentOptions) {
  const [document, setDocument] = useState<PDFDocumentProxy | null>(null);
  const [loading, setLoading] = useState(true);
  const unlockRef = useRef(onPassword);
  unlockRef.current = onPassword;
  const optionsRef = useRef(documentOptions);
  optionsRef.current = documentOptions;
  const key = sourceKey(source);
  useEffect(() => {
    let cancelled = false;
    let task: ReturnType<typeof pdfjs.getDocument> | null = null;
    let flow: { failure: Error | null } | null = null;
    let timer: ReturnType<typeof createLoadTimer> | null = null;
    let transport: PdfHttpRangeTransport | null = null;
    const abort = new AbortController();
    setLoading(true);
    onLoadingChange?.(true);
    ensurePdfWorker();
    void (async () => {
      try {
        let init;
        if (source.kind === "range") {
          transport = await createPdfRangeTransport(source.url, { headers: source.headers, chunkSize: source.chunkSize, signal: abort.signal });
          if (cancelled) return;
          init = pdfRangeDocumentInit(transport, optionsRef.current, source.chunkSize);
        } else {
          const data = await loadSource(source);
          if (cancelled) return;
          init = pdfDocumentInit(data, optionsRef.current);
        }
        const loadTask = pdfjs.getDocument(init);
        task = loadTask;
        transport?.setFailureHandler(() => { void loadTask.destroy(); });
        timer = createLoadTimer(PAGE_LOAD_TIMEOUT, "PDF document load timed out");
        if (unlockRef.current) flow = attachPasswordHandler(loadTask, () => unlockRef.current, timer);
        const next = await Promise.race([loadTask.promise, timer.promise]);
        if (cancelled) {
          void next.cleanup();
          return;
        }
        setDocument(next);
      } catch (value) {
        if (cancelled) return;
        const error = flow?.failure ?? transport?.failure ?? (value instanceof Error ? value : new Error(String(value)));
        onError?.(error);
      } finally {
        timer?.clear();
        if (!cancelled) {
          setLoading(false);
          onLoadingChange?.(false);
        }
      }
    })();
    return () => {
      cancelled = true;
      abort.abort();
      transport?.abort();
      setDocument((previous) => {
        if (previous) void previous.cleanup();
        return null;
      });
      task?.destroy();
    };
  }, [key, onError, onLoadingChange]);
  return { document, loading };
}

export function PdfViewer({
  source,
  adapters,
  initialView,
  onViewState,
  onDocumentInfo,
  onError,
  onLoadingChange,
  className,
  toolbar,
  hideToolbar = false,
  findBar = true,
  regexFind = false,
  onPassword,
  maxRenderPages,
  truncationNotice,
  documentOptions,
  textContentMode = "getTextContent",
  unloadOffscreenPages = false,
  annotationLayer = false,
  searchLayer = false,
  pageLayers,
  passwordPrompt = false,
}: PdfViewerProps) {
  const hostRef = useRef<HTMLDivElement>(null);
  const scrollRef = useRef<HTMLDivElement>(null);
  const slotsRef = useRef(new Map<number, HTMLDivElement>());
  const pageRefs = useRef(new Map<number, PageState>());
  const pdfRef = useRef<PDFDocumentProxy | null>(null);
  const [pdf, setPdf] = useState<PDFDocumentProxy | null>(null);
  const [passwordRequest, setPasswordRequest] = useState<{ retry: boolean; resolve: (password: string | null) => void } | null>(null);
  const promptPassword = useCallback<PdfPasswordHandler>(
    (reason) => new Promise<string | null>((resolve) => setPasswordRequest({ retry: reason === "incorrect", resolve })),
    [],
  );
  const { document: pdfDocument, loading } = usePdfDocument(source, onError, onLoadingChange, onPassword ?? (passwordPrompt ? promptPassword : undefined), documentOptions);
  useEffect(() => {
    if (loading || !passwordRequest) return;
    // The load ended (unlocked, failed or was replaced) while the dialog was open.
    passwordRequest.resolve(null);
    setPasswordRequest(null);
  }, [loading, passwordRequest]);
  // Pages that have started rendering (or failed), so one page renders once per raster generation.
  const liveRef = useRef(new Set<number>());
  // Bumped when a page is torn down, so an in-flight render of an older generation stops.
  const generationRef = useRef(new Map<number, number>());
  const layerCleanupRef = useRef(new Map<number, () => void>());
  const optionsRef = useRef({ textContentMode, unloadOffscreenPages, annotationLayer, searchLayer, pageLayers });
  optionsRef.current = { textContentMode, unloadOffscreenPages, annotationLayer, searchLayer, pageLayers };
  const linkServiceRef = useRef<ReturnType<typeof createPdfLinkService> | null>(null);
  const committedFindRef = useRef<{ query: string; options: PdfSearchOptions; active: { page: number; occurrence: number } | null } | null>(null);

  const teardownPage = useCallback((pageNumber: number) => {
    generationRef.current.set(pageNumber, (generationRef.current.get(pageNumber) ?? 0) + 1);
    const cleanup = layerCleanupRef.current.get(pageNumber);
    layerCleanupRef.current.delete(pageNumber);
    try { cleanup?.(); } catch { /* a host layer's cleanup must not stop the viewer */ }
    liveRef.current.delete(pageNumber);
    slotsRef.current.get(pageNumber)?.replaceChildren();
  }, []);
  const teardownAll = useCallback(() => {
    for (const pageNumber of new Set([...slotsRef.current.keys(), ...liveRef.current, ...layerCleanupRef.current.keys()])) teardownPage(pageNumber);
  }, [teardownPage]);
  useEffect(() => teardownAll, [teardownAll]);
  const [numPages, setNumPages] = useState(0);
  const [page, setPage] = useState(Math.max(1, initialView?.page ?? 1));
  const [scale, setScale] = useState(initialView?.scale ?? 1);
  const [fit, setFit] = useState<PdfFitMode>(initialView?.fit ?? "width");
  const [rotation, setRotation] = useState(normalizeRotation(initialView?.rotation ?? 0));
  const [panel, setPanel] = useState<PdfPanel>(initialView?.panel ?? null);
  const [outline, setOutline] = useState<OutlineNode[] | null>(null);
  const [query, setQuery] = useState("");
  const [findOptions, setFindOptions] = useState<PdfSearchOptions>({ matchCase: false, wholeWord: false });
  const [matches, setMatches] = useState<Array<{ page: number; index: number }>>([]);
  const [activeMatch, setActiveMatch] = useState(-1);
  const [pageDraft, setPageDraft] = useState("");
  const [scaleDraft, setScaleDraft] = useState("");
  const [renderedPages, setRenderedPages] = useState<Set<number>>(new Set());
  const renderLimit = renderPageLimit(numPages, maxRenderPages);

  useEffect(() => {
    pdfRef.current = pdfDocument;
    setPdf(pdfDocument);
    pageRefs.current.clear();
    setRenderedPages(new Set());
    if (!pdfDocument) {
      setNumPages(0);
      setOutline(null);
      return;
    }
    setNumPages(pdfDocument.numPages);
    void pdfDocument.getOutline().then((items) => setOutline((items ?? []) as OutlineNode[])).catch(() => setOutline(null));
    void pdfDocument.getOutline().then((items) => onDocumentInfo?.({ numPages: pdfDocument.numPages, hasOutline: Boolean(items?.length) })).catch(() => onDocumentInfo?.({ numPages: pdfDocument.numPages, hasOutline: false }));
  }, [pdfDocument, onDocumentInfo]);

  // A different (or closed) document: nothing rendered for the old one stays, host layers included.
  useEffect(() => { teardownAll(); }, [pdfDocument, teardownAll]);

  const firstSize = useMemo<Size | null>(() => {
    const first = pageRefs.current.get(1);
    return first ? { w: first.width, h: first.height } : null;
  }, [numPages, renderedPages]);

  const emitView = useCallback((next?: Partial<PdfViewState>) => {
    onViewState?.({
      page: next?.page ?? page,
      scale: next?.scale ?? scale,
      fit: next?.fit === undefined ? fit : next.fit,
      rotation: next?.rotation ?? rotation,
      panel: next?.panel === undefined ? panel : next.panel,
    });
  }, [fit, onViewState, page, panel, rotation, scale]);

  useEffect(() => {
    if (!firstSize || !fit || !hostRef.current) return;
    setScale(fitScale(fit, hostRef.current, firstSize, rotation));
  }, [firstSize, fit, rotation]);

  useEffect(() => {
    setScaleDraft(String(Math.round(scale * 100)));
    emitView({ scale });
  }, [emitView, scale]);

  useEffect(() => {
    emitView();
  }, [emitView, fit, panel, page, rotation]);

  const goToPage = useCallback((next: number) => {
    const bounded = Math.max(1, Math.min(numPages || 1, Math.round(next)));
    setPage(bounded);
    slotsRef.current.get(bounded)?.scrollIntoView({ block: "start", behavior: "smooth" });
  }, [numPages]);

  const goToDest = useCallback(async (dest: string | unknown[] | null) => {
    if (!pdf || !dest) return;
    try {
      const resolved = typeof dest === "string" ? await pdf.getDestination(dest) : dest;
      const ref = Array.isArray(resolved) ? resolved[0] : null;
      if (ref) goToPage((await pdf.getPageIndex(ref as Parameters<PDFDocumentProxy["getPageIndex"]>[0])) + 1);
    } catch { /* malformed destination */ }
  }, [goToPage, pdf]);

  const linkService = useMemo(() => {
    const actions: PdfLinkActions = { goToDest, goToPage, currentPage: () => page, pagesCount: () => numPages, openExternal: adapters?.openExternal };
    return createPdfLinkService(actions);
  }, [adapters?.openExternal, goToDest, goToPage, numPages, page]);
  linkServiceRef.current = linkService;

  // Marks the committed find query in one page's text layer. The active match is the page's
  // `occurrence`-th hit in text-layer order (a hit split across two runs is not drawn).
  const highlightPage = useCallback((pageNumber: number, textLayer: Element | null) => {
    if (!textLayer) return;
    const committed = committedFindRef.current;
    const query = committed?.query ?? "";
    const options = committed?.options ?? { matchCase: false, wholeWord: false };
    const active = committed?.active && committed.active.page === pageNumber ? committed.active.occurrence : undefined;
    let running = 0;
    for (const span of textLayer.querySelectorAll<HTMLElement>("span")) {
      const text = span.textContent ?? "";
      const count = !query ? 0 : options.regex ? findRegexMatches(text, query, options).length : findLiteralMatches(text, query, options).length;
      const local = active !== undefined && active >= running && active < running + count ? active - running : undefined;
      highlightSpan(span, query, { ...options, activeOccurrence: local });
      running += count;
    }
  }, []);

  const renderPage = useCallback(async (pageNumber: number, slot: HTMLDivElement) => {
    if (!pdf || pageNumber > renderLimit) return;
    const generation = generationRef.current.get(pageNumber) ?? 0;
    const stale = () => (generationRef.current.get(pageNumber) ?? 0) !== generation;
    const live = optionsRef.current;
    try {
      const pdfPage = await pdf.getPage(pageNumber);
      if (stale()) return;
      const pageRotation = viewportRotation(pdfPage.rotate, rotation);
      const viewport = pdfPage.getViewport({ scale, rotation: pageRotation });
      const cssViewport = pdfPage.getViewport({ scale: 1, rotation: pageRotation });
      pageRefs.current.set(pageNumber, { width: cssViewport.width, height: cssViewport.height, rotation: pageRotation });
      slot.style.width = `${viewport.width}px`;
      slot.style.height = `${viewport.height}px`;
      slot.replaceChildren();
      const canvas = document.createElement("canvas");
      canvas.className = "rk-pdf-canvas";
      const dpr = boundedCanvasDpr(viewport, window.devicePixelRatio || 1);
      canvas.width = Math.ceil(viewport.width * dpr);
      canvas.height = Math.ceil(viewport.height * dpr);
      canvas.style.width = `${viewport.width}px`;
      canvas.style.height = `${viewport.height}px`;
      slot.append(canvas);
      const context = canvas.getContext("2d", { alpha: false });
      if (!context) throw new Error("PDF canvas is unavailable");
      const renderTask = pdfPage.render({ canvas: canvas as HTMLCanvasElement, canvasContext: context, viewport, transform: dpr === 1 ? undefined : [dpr, 0, 0, dpr, 0, 0] });
      // A page torn down mid-render cancels the raster instead of letting it finish unseen.
      layerCleanupRef.current.set(pageNumber, () => renderTask.cancel());
      await withTimeout(renderTask.promise, PAGE_LOAD_TIMEOUT, `Page ${pageNumber} render timed out`);
      if (stale()) return;
      const textLayer = document.createElement("div");
      textLayer.className = "textLayer";
      slot.append(textLayer);
      await renderPdfTextLayer(pdfPage, textLayer, viewport, live.textContentMode);
      if (stale()) return;
      if (live.searchLayer) highlightPage(pageNumber, textLayer);
      if (live.annotationLayer) {
        const annotations = await pdfPage.getAnnotations({ intent: "display" });
        if (stale()) return;
        const service = linkServiceRef.current;
        if (annotations.length > 0 && service) {
          const annotationDiv = document.createElement("div");
          annotationDiv.className = "annotationLayer";
          slot.append(annotationDiv);
          // pdf.js sizes its layers from these variables; the full web viewer sets them on the page.
          slot.style.setProperty("--scale-factor", String(viewport.scale));
          slot.style.setProperty("--total-scale-factor", String(viewport.scale));
          const layerViewport = viewport.clone({ dontFlip: true });
          const layer = new pdfjs.AnnotationLayer({
            div: annotationDiv,
            accessibilityManager: null,
            annotationCanvasMap: null,
            annotationEditorUIManager: null,
            page: pdfPage,
            viewport: layerViewport,
            structTreeLayer: NULL_STRUCT_TREE_LAYER,
            commentManager: null,
            linkService: service as never,
            annotationStorage: pdf.annotationStorage,
          });
          await layer.render({
            annotations,
            div: annotationDiv,
            page: pdfPage,
            viewport: layerViewport,
            linkService: service as never,
            renderForms: false,
            imageResourcesPath: "",
          });
          if (stale()) return;
        }
      }
      let hostCleanup: (() => void) | undefined;
      if (live.pageLayers) {
        const mounted = await live.pageLayers({ pageNumber, slot, page: pdfPage, viewport });
        if (typeof mounted === "function") hostCleanup = mounted;
        if (stale()) {
          try { hostCleanup?.(); } catch { /* host cleanup must not stop the viewer */ }
          return;
        }
      }
      const release = live.unloadOffscreenPages;
      layerCleanupRef.current.set(pageNumber, () => {
        try { hostCleanup?.(); } finally { if (release) pdfPage.cleanup(); }
      });
      setRenderedPages((old) => new Set(old).add(pageNumber));
    } catch (value) {
      if (stale()) return;
      const error = value instanceof Error ? value : new Error(String(value));
      pageRefs.current.set(pageNumber, { width: 612, height: 792, rotation, error: error.message });
      slot.replaceChildren();
      const message = document.createElement("p");
      message.className = "rk-pdf-error";
      message.textContent = `Page ${pageNumber} could not render: ${error.message}`;
      slot.append(message);
    }
  }, [highlightPage, pdf, renderLimit, rotation, scale]);

  useEffect(() => {
    if (!pdf || !scrollRef.current) return;
    const root = scrollRef.current;
    const unload = optionsRef.current.unloadOffscreenPages;
    const observer = new IntersectionObserver((entries) => {
      for (const entry of entries) {
        if (!entry.isIntersecting) continue;
        const pageNumber = Number((entry.target as HTMLElement).dataset.page);
        if (!Number.isFinite(pageNumber) || pageNumber > renderLimit || liveRef.current.has(pageNumber)) continue;
        liveRef.current.add(pageNumber);
        void renderPage(pageNumber, entry.target as HTMLDivElement);
      }
    }, { root, rootMargin: unload ? UNLOAD_RENDER_ROOT_MARGIN : RENDER_ROOT_MARGIN });
    // The unload band is wider than the render band: a page is freed only once it has left it.
    const unloadObserver = unload ? new IntersectionObserver((entries) => {
      for (const entry of entries) {
        if (entry.isIntersecting) continue;
        const pageNumber = Number((entry.target as HTMLElement).dataset.page);
        if (Number.isFinite(pageNumber) && liveRef.current.has(pageNumber)) teardownPage(pageNumber);
      }
    }, { root, rootMargin: UNLOAD_ROOT_MARGIN }) : null;
    for (const slot of slotsRef.current.values()) {
      observer.observe(slot);
      unloadObserver?.observe(slot);
    }
    return () => { observer.disconnect(); unloadObserver?.disconnect(); };
  }, [numPages, pdf, renderLimit, renderPage, teardownPage, unloadOffscreenPages]);

  // A lowered cap un-renders pages that are now past it (they fall back to empty placeholders).
  useEffect(() => {
    for (const pageNumber of slotsRef.current.keys()) {
      if (pageNumber > renderLimit) { teardownPage(pageNumber); pageRefs.current.delete(pageNumber); }
    }
  }, [renderLimit, teardownPage]);

  useEffect(() => {
    if (!pdf) return;
    pageRefs.current.clear();
    setRenderedPages(new Set());
    teardownAll();
  }, [pdf, rotation, scale, teardownAll]);

  // Match marks follow the committed query and the active match on every page that is rendered.
  const [committedFind, setCommittedFind] = useState<{ query: string; options: PdfSearchOptions } | null>(null);
  useEffect(() => {
    if (!searchLayer) return;
    const active = activeMatch >= 0 ? matches[activeMatch] : undefined;
    let occurrence = 0;
    if (active) for (let index = 0; index < activeMatch; index++) if (matches[index]?.page === active.page) occurrence++;
    committedFindRef.current = committedFind ? { ...committedFind, active: active ? { page: active.page, occurrence } : null } : null;
    for (const [pageNumber, slot] of slotsRef.current) {
      if (liveRef.current.has(pageNumber)) highlightPage(pageNumber, slot.querySelector(".textLayer"));
    }
  }, [activeMatch, committedFind, highlightPage, matches, searchLayer]);

  const find = useCallback(async (nextQuery = query): Promise<PdfSearchResult> => {
    if (!pdf || !nextQuery) {
      setMatches([]); setActiveMatch(-1); setCommittedFind(null); return { count: 0, activeIndex: -1 };
    }
    const found: Array<{ page: number; index: number }> = [];
    let timedOut = false;
    const searcher = findOptions.regex ? createRegexSearcher() : null;
    try {
    for (let pageNumber = 1; pageNumber <= Math.min(pdf.numPages, renderLimit); pageNumber++) {
      try {
        let text = await readPdfPageText(await pdf.getPage(pageNumber), optionsRef.current.textContentMode);
        if (text.length > MAX_TEXT) text = text.slice(0, MAX_TEXT);
        let hits;
        if (searcher) {
          const bounded = await searcher.search(text, nextQuery, findOptions);
          if (bounded.timedOut) timedOut = true;
          hits = bounded.matches;
        } else {
          hits = findLiteralMatches(text, nextQuery, findOptions);
        }
        for (const hit of hits) found.push({ page: pageNumber, index: hit.start });
      } catch { /* one malformed page must not break find in rest */ }
    }
    } finally {
      searcher?.dispose();
    }
    setCommittedFind({ query: nextQuery, options: findOptions });
    setMatches(found);
    setActiveMatch(found.length ? 0 : -1);
    if (found[0]) goToPage(found[0].page);
    return { count: found.length, activeIndex: found.length ? 0 : -1, ...(timedOut ? { timedOut: true } : {}) };
  }, [findOptions, goToPage, pdf, query, renderLimit]);

  const stepMatch = useCallback((delta: number): PdfSearchResult => {
    if (!matches.length) return { count: 0, activeIndex: -1 };
    const next = (activeMatch + delta + matches.length) % matches.length;
    setActiveMatch(next);
    goToPage(matches[next]?.page ?? page);
    return { count: matches.length, activeIndex: next };
  }, [activeMatch, goToPage, matches, page]);

  useLayoutEffect(() => {
    for (const slot of slotsRef.current.values()) {
      const pageNumber = Number(slot.dataset.page);
      const state = pageRefs.current.get(pageNumber);
      if (state) {
        slot.style.width = `${state.width * scale}px`;
        slot.style.height = `${state.height * scale}px`;
      }
    }
  }, [scale]);

  const viewState = { page, scale, fit, rotation, panel } satisfies PdfViewState;
  const searchValue: PdfSearchContextValue = { find, next: () => stepMatch(1), prev: () => stepMatch(-1), result: { count: matches.length, activeIndex: activeMatch }, viewState };
  return <PdfSearchContext.Provider value={searchValue}><div ref={hostRef} className={`rk-pdf-viewer ${className ?? ""}`} data-loading={loading || undefined}>
    {!hideToolbar && <div className="rk-pdf-toolbar">
      <button type="button" onClick={() => goToPage(page - 1)} disabled={page <= 1} aria-label="Previous page">‹</button>
      <form onSubmit={(event) => { event.preventDefault(); goToPage(Number(pageDraft)); setPageDraft(""); }}>
        <input aria-label="Page number" value={pageDraft || String(page)} onChange={(event) => setPageDraft(event.target.value)} onFocus={() => setPageDraft(String(page))} />
        <span>/ {numPages || "—"}</span>
      </form>
      <button type="button" onClick={() => goToPage(page + 1)} disabled={page >= numPages} aria-label="Next page">›</button>
      <button type="button" onClick={() => { setFit(null); setScale((value) => clampScale(value - 0.1)); }} aria-label="Zoom out">−</button>
      <form onSubmit={(event) => { event.preventDefault(); const value = Number(scaleDraft) / 100; if (Number.isFinite(value)) { setFit(null); setScale(clampScale(value)); } }}>
        <input aria-label="Zoom percentage" value={scaleDraft || String(Math.round(scale * 100))} onChange={(event) => setScaleDraft(event.target.value)} />%
      </form>
      <button type="button" onClick={() => { setFit(null); setScale((value) => clampScale(value + 0.1)); }} aria-label="Zoom in">+</button>
      <button type="button" onClick={() => { setFit("width"); emitView({ fit: "width" }); }} aria-label="Fit width">Fit</button>
      <button type="button" onClick={() => setRotation((value) => normalizeRotation(value + 90))} aria-label="Rotate">↻</button>
      <button type="button" onClick={() => setPanel(panel === "outline" ? null : "outline")} aria-pressed={panel === "outline"}>Outline</button>
      <button type="button" onClick={() => setPanel(panel === "thumbs" ? null : "thumbs")} aria-pressed={panel === "thumbs"}>Pages</button>
      {findBar && <form className="rk-pdf-find" onSubmit={(event) => { event.preventDefault(); void find(); }}>
        <input aria-label="Find in PDF" placeholder="Find" value={query} onChange={(event) => setQuery(event.target.value)} />
        <button type="submit">Find</button><button type="button" onClick={() => stepMatch(1)} disabled={!matches.length}>Next</button>
        {regexFind && <label><input type="checkbox" checked={findOptions.regex ?? false} onChange={(event) => setFindOptions((value) => ({ ...value, regex: event.target.checked }))} /> Regex</label>}
      </form>}
      {toolbar}
    </div>}
    <div className="rk-pdf-body">
      {panel === "outline" && <OutlinePanel nodes={outline ?? []} onSelect={(dest) => void goToDest(dest)} />}
      {panel === "thumbs" && pdf && <ThumbnailPanel pdf={pdf} numPages={renderLimit} current={page} onSelect={goToPage} />}
      <div ref={scrollRef} className="rk-pdf-scroll" role="region" aria-label="PDF pages" tabIndex={0}>
        <PdfTruncationNotice numPages={numPages} shown={renderLimit} notice={truncationNotice} />
        <div className="rk-pdf-pages">
          {Array.from({ length: numPages }, (_, index) => index + 1).map((pageNumber) => <div
            key={pageNumber} data-page={pageNumber} ref={(element) => { if (element) slotsRef.current.set(pageNumber, element); else slotsRef.current.delete(pageNumber); }}
            className="rk-pdf-page" data-capped={pageNumber > renderLimit || undefined} style={{ width: (pageRefs.current.get(pageNumber)?.width ?? 612) * scale, height: (pageRefs.current.get(pageNumber)?.height ?? 792) * scale }}
          >{pageRefs.current.get(pageNumber)?.error && <p className="rk-pdf-error">{pageRefs.current.get(pageNumber)?.error}</p>}</div>)}
        </div>
      </div>
    </div>
    {passwordRequest && <PdfPasswordPrompt
      retry={passwordRequest.retry}
      onSubmit={(password) => { setPasswordRequest(null); passwordRequest.resolve(password); }}
      onCancel={() => { setPasswordRequest(null); passwordRequest.resolve(null); }}
    />}
  </div></PdfSearchContext.Provider>;
}

/** The "showing first N of M pages" slot. Renders nothing unless a notice is given and pages are hidden. */
export function PdfTruncationNotice({ numPages, shown, notice }: { numPages: number; shown: number; notice: PdfViewerProps["truncationNotice"] }) {
  if (notice === undefined || notice === null || notice === false || shown >= numPages) return null;
  const content = typeof notice === "function" ? notice({ shown, total: numPages }) : notice;
  return <p className="rk-pdf-notice" role="status">{content}</p>;
}

function OutlinePanel({ nodes, onSelect }: { nodes: OutlineNode[]; onSelect: (dest: string | unknown[] | null) => void }) {
  return <aside className="rk-pdf-outline" aria-label="PDF outline"><OutlineList nodes={nodes} onSelect={onSelect} /></aside>;
}

function OutlineList({ nodes, onSelect }: { nodes: OutlineNode[]; onSelect: (dest: string | unknown[] | null) => void }) {
  return <ul>{nodes.map((node, index) => <li key={`${node.title}-${index}`}><button type="button" onClick={() => onSelect(node.dest)}>{node.title}</button>{node.items?.length ? <OutlineList nodes={node.items} onSelect={onSelect} /> : null}</li>)}</ul>;
}

function ThumbnailPanel({ pdf, numPages, current, onSelect }: { pdf: PDFDocumentProxy; numPages: number; current: number; onSelect: (page: number) => void }) {
  const [items, setItems] = useState<Array<{ page: number; url: string; width: number; height: number }>>([]);
  useEffect(() => {
    let cancelled = false;
    void (async () => {
      const next: typeof items = [];
      for (let pageNumber = 1; pageNumber <= numPages; pageNumber++) {
        const page = await pdf.getPage(pageNumber);
        const viewport = page.getViewport({ scale: 1 });
        const scale = 130 / viewport.width;
        const thumb = page.getViewport({ scale });
        const canvas = document.createElement("canvas"); canvas.width = thumb.width; canvas.height = thumb.height;
        const context = canvas.getContext("2d"); if (context) await page.render({ canvas: canvas as HTMLCanvasElement, canvasContext: context, viewport: thumb }).promise;
        next.push({ page: pageNumber, url: canvas.toDataURL("image/png"), width: thumb.width, height: thumb.height });
        if (cancelled) return;
      }
      if (!cancelled) setItems(next);
    })();
    return () => { cancelled = true; };
  }, [numPages, pdf]);
  return <aside className="rk-pdf-thumbs" aria-label="PDF pages"><div>{items.map((item) => <button key={item.page} type="button" aria-label={`Page ${item.page}`} aria-current={item.page === current || undefined} onClick={() => onSelect(item.page)}><img src={item.url} width={item.width} height={item.height} alt="" /><span>{item.page}</span></button>)}</div></aside>;
}

interface PdfSearchContextValue { find: (query?: string) => Promise<PdfSearchResult>; next: () => PdfSearchResult; prev: () => PdfSearchResult; result: PdfSearchResult; viewState: PdfViewState }
export const PdfSearchContext = createContext<PdfSearchContextValue | null>(null);

export function usePdfSearch(): PdfSearchContextValue {
  const value = useContext(PdfSearchContext);
  if (!value) throw new Error("usePdfSearch must be used inside PdfViewer");
  return value;
}

export { createPdfLinkService } from "./linkService.js";
export type { PdfViewerAdapters, PdfSource, PdfViewState } from "./types.js";
