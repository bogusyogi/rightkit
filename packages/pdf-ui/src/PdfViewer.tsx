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
import { TextLayer } from "pdfjs-dist";
import { createPdfLinkService, type PdfLinkActions } from "./linkService.js";
import { ensurePdfWorker } from "./pdfJs.js";
import { boundedCanvasDpr, clampScale, viewportRotation, withTimeout } from "./shared.js";
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
}

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
  if (source.kind === "loader") return JSON.stringify(["loader", identityOf(source.load), revision]);
  return JSON.stringify(["bytes", identityOf(source.data.buffer), source.data.byteOffset, source.data.byteLength, revision]);
}

async function loadSource(source: PdfSource): Promise<Uint8Array | string> {
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

function usePdfDocument(source: PdfSource, onError?: (error: Error) => void, onLoadingChange?: (loading: boolean) => void) {
  const [document, setDocument] = useState<PDFDocumentProxy | null>(null);
  const [loading, setLoading] = useState(true);
  const key = sourceKey(source);
  useEffect(() => {
    let cancelled = false;
    let task: ReturnType<typeof pdfjs.getDocument> | null = null;
    setLoading(true);
    onLoadingChange?.(true);
    ensurePdfWorker();
    void (async () => {
      try {
        const data = await loadSource(source);
        if (cancelled) return;
        task = pdfjs.getDocument(typeof data === "string" ? { url: data } : { data });
        const next = await withTimeout(task.promise, PAGE_LOAD_TIMEOUT, "PDF document load timed out");
        if (cancelled) {
          void next.cleanup();
          return;
        }
        setDocument(next);
      } catch (value) {
        if (cancelled) return;
        const error = value instanceof Error ? value : new Error(String(value));
        onError?.(error);
      } finally {
        if (!cancelled) {
          setLoading(false);
          onLoadingChange?.(false);
        }
      }
    })();
    return () => {
      cancelled = true;
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
}: PdfViewerProps) {
  const hostRef = useRef<HTMLDivElement>(null);
  const scrollRef = useRef<HTMLDivElement>(null);
  const slotsRef = useRef(new Map<number, HTMLDivElement>());
  const pageRefs = useRef(new Map<number, PageState>());
  const pdfRef = useRef<PDFDocumentProxy | null>(null);
  const [pdf, setPdf] = useState<PDFDocumentProxy | null>(null);
  const { document: pdfDocument, loading } = usePdfDocument(source, onError, onLoadingChange);
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

  const renderPage = useCallback(async (pageNumber: number, slot: HTMLDivElement) => {
    if (!pdf) return;
    try {
      const pdfPage = await pdf.getPage(pageNumber);
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
      await withTimeout(pdfPage.render({ canvas: canvas as HTMLCanvasElement, canvasContext: context, viewport, transform: dpr === 1 ? undefined : [dpr, 0, 0, dpr, 0, 0] }).promise, PAGE_LOAD_TIMEOUT, `Page ${pageNumber} render timed out`);
      const textLayer = document.createElement("div");
      textLayer.className = "textLayer";
      slot.append(textLayer);
      const text = new TextLayer({ textContentSource: await pdfPage.getTextContent(), container: textLayer, viewport });
      await text.render();
      // AnnotationLayer is optional in pdf.js builds. Text links remain safe through the exported
      // link service; hosts can mount pdf.js's full AnnotationLayer when form widgets are needed.
      setRenderedPages((old) => new Set(old).add(pageNumber));
    } catch (value) {
      const error = value instanceof Error ? value : new Error(String(value));
      pageRefs.current.set(pageNumber, { width: 612, height: 792, rotation, error: error.message });
      slot.replaceChildren();
      const message = document.createElement("p");
      message.className = "rk-pdf-error";
      message.textContent = `Page ${pageNumber} could not render: ${error.message}`;
      slot.append(message);
    }
  }, [linkService, pdf, rotation, scale]);

  useEffect(() => {
    if (!pdf || !scrollRef.current) return;
    const observer = new IntersectionObserver((entries) => {
      for (const entry of entries) {
        if (entry.isIntersecting) {
          const pageNumber = Number((entry.target as HTMLElement).dataset.page);
          if (Number.isFinite(pageNumber) && !pageRefs.current.has(pageNumber)) void renderPage(pageNumber, entry.target as HTMLDivElement);
        }
      }
    }, { root: scrollRef.current, rootMargin: "700px 0px" });
    for (const slot of slotsRef.current.values()) observer.observe(slot);
    return () => observer.disconnect();
  }, [numPages, pdf, renderPage]);

  useEffect(() => {
    if (!pdf) return;
    pageRefs.current.clear();
    setRenderedPages(new Set());
    for (const slot of slotsRef.current.values()) slot.replaceChildren();
  }, [pdf, rotation, scale]);

  const find = useCallback(async (nextQuery = query): Promise<PdfSearchResult> => {
    if (!pdf || !nextQuery) {
      setMatches([]); setActiveMatch(-1); return { count: 0, activeIndex: -1 };
    }
    const found: Array<{ page: number; index: number }> = [];
    const needle = findOptions.matchCase ? nextQuery : nextQuery.toLocaleLowerCase();
    for (let pageNumber = 1; pageNumber <= pdf.numPages; pageNumber++) {
      try {
        const content = await pdf.getPage(pageNumber).then((p) => p.getTextContent());
        let text = content.items.map((item) => "str" in item ? item.str : "").join(" ");
        if (text.length > MAX_TEXT) text = text.slice(0, MAX_TEXT);
        const haystack = findOptions.matchCase ? text : text.toLocaleLowerCase();
        let index = haystack.indexOf(needle);
        while (index >= 0) {
          const end = index + needle.length;
          const isWord = (value: string | undefined) => !!value && /[\p{L}\p{N}_]/u.test(value);
          if (!findOptions.wholeWord || (!isWord(text[index - 1]) && !isWord(text[end]))) found.push({ page: pageNumber, index });
          index = haystack.indexOf(needle, Math.max(index + 1, end));
        }
      } catch { /* one malformed page must not break find in rest */ }
    }
    setMatches(found);
    setActiveMatch(found.length ? 0 : -1);
    if (found[0]) goToPage(found[0].page);
    return { count: found.length, activeIndex: found.length ? 0 : -1 };
  }, [findOptions, goToPage, pdf, query]);

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
      </form>}
      {toolbar}
    </div>}
    <div className="rk-pdf-body">
      {panel === "outline" && <OutlinePanel nodes={outline ?? []} onSelect={(dest) => void goToDest(dest)} />}
      {panel === "thumbs" && pdf && <ThumbnailPanel pdf={pdf} numPages={numPages} current={page} onSelect={goToPage} />}
      <div ref={scrollRef} className="rk-pdf-scroll" role="region" aria-label="PDF pages" tabIndex={0}>
        <div className="rk-pdf-pages">
          {Array.from({ length: numPages }, (_, index) => index + 1).map((pageNumber) => <div
            key={pageNumber} data-page={pageNumber} ref={(element) => { if (element) slotsRef.current.set(pageNumber, element); else slotsRef.current.delete(pageNumber); }}
            className="rk-pdf-page" style={{ width: (pageRefs.current.get(pageNumber)?.width ?? 612) * scale, height: (pageRefs.current.get(pageNumber)?.height ?? 792) * scale }}
          >{pageRefs.current.get(pageNumber)?.error && <p className="rk-pdf-error">{pageRefs.current.get(pageNumber)?.error}</p>}</div>)}
        </div>
      </div>
    </div>
  </div></PdfSearchContext.Provider>;
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
