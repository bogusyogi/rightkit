import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { PDFDocumentProxy } from "pdfjs-dist";
import type { SearchOptions, SearchProvider, SearchResult } from "../types";
import { findLiteralMatches } from "../searchUtils";
import { MARK_ACTIVE_BG, MARK_BG, highlightSpan, type Match } from "./pdfShared";

const NO_RESULT: SearchResult = { count: 0, activeIndex: -1 };
export const PDF_SEARCH_CACHE_MAX_BYTES = 8 * 1024 * 1024;

function textBytes(text: string): number {
  // UTF-16 storage is a safe upper bound for this in-memory cache and avoids allocating a second
  // encoded copy merely to account for it.
  return text.length * 2;
}

/**
 * Read page text through same streaming API used by rendered text layers. WKWebView's PDF.js worker
 * can finish a stream-backed text layer while a parallel getTextContent() request remains empty;
 * search must consume stream chunks so native search sees text already visible on page.
 */
export async function readPdfPageText(
  page: Awaited<ReturnType<PDFDocumentProxy["getPage"]>>,
  signal?: AbortSignal,
): Promise<string> {
  const reader = page.streamTextContent().getReader();
  const items: string[] = [];
  let canceled = false;
  const cancelRead = () => {
    if (canceled) return;
    canceled = true;
    void reader.cancel().catch(() => undefined);
  };
  signal?.addEventListener("abort", cancelRead, { once: true });
  try {
    while (true) {
      if (signal?.aborted) throw new DOMException("Search canceled", "AbortError");
      const { done, value } = await reader.read();
      if (done) break;
      for (const item of value.items) {
        if ("str" in item) items.push(item.str);
      }
    }
  } finally {
    signal?.removeEventListener("abort", cancelRead);
    if (signal?.aborted) cancelRead();
    reader.releaseLock();
  }
  return items.join(" ");
}

/**
 * PDF find, exposed as the app's shared `SearchProvider`. Owns the match model + committed query +
 * options (read by the viewer's render path via the returned refs to draw <mark>s, and as effect
 * deps to re-highlight already-rendered pages). Page text is extracted once per page and cached
 * (original case — case folding happens at match time); rapid typing supersedes in-flight runs via a
 * sequence guard. Supports match-case + whole-word; read-only (no replace).
 */
export function usePdfSearch({
  url,
  pdfRef,
  slotRefs,
  scrollRef,
  goToPage,
  registerSearchProvider,
}: {
  url?: string;
  pdfRef: React.MutableRefObject<PDFDocumentProxy | null>;
  slotRefs: React.MutableRefObject<Map<number, HTMLDivElement>>;
  scrollRef: React.RefObject<HTMLDivElement | null>;
  goToPage: (n: number) => void;
  registerSearchProvider?: (provider: SearchProvider | null) => void;
}) {
  const [committedQuery, setCommittedQuery] = useState("");
  const [opts, setOpts] = useState<SearchOptions>({ matchCase: false, wholeWord: false });
  const [matches, setMatches] = useState<Match[]>([]);
  const [activeIdx, setActiveIdx] = useState(0);

  const pageTextRef = useRef<Map<number, string>>(new Map()); // page → original-case text (find cache)
  const pageTextBytesRef = useRef(0);
  const searchAbortRef = useRef<AbortController | null>(null);
  const queryRef = useRef(""); // committed query, read by the viewer's renderPage to highlight
  const optionsRef = useRef<SearchOptions>(opts); // current options, read by renderPage's highlight
  const activeMarkRef = useRef<HTMLElement | null>(null);
  const searchSeqRef = useRef(0); // bumped per search; guards stale overlapping runSearch results
  const matchesLenRef = useRef(0); // live match count for stable next()/prev() provider methods
  const activeIdxRef = useRef(0);
  const searchResultListenersRef = useRef<Set<(query: string, result: SearchResult) => void>>(
    new Set(),
  );

  queryRef.current = committedQuery;
  optionsRef.current = opts;
  matchesLenRef.current = matches.length;
  activeIdxRef.current = activeIdx;

  const cachePageText = useCallback((page: number, text: string) => {
    const previous = pageTextRef.current.get(page);
    if (previous !== undefined) pageTextBytesRef.current -= textBytes(previous);
    pageTextRef.current.delete(page);
    const bytes = textBytes(text);
    if (bytes <= PDF_SEARCH_CACHE_MAX_BYTES) {
      pageTextRef.current.set(page, text);
      pageTextBytesRef.current += bytes;
    }
    while (pageTextBytesRef.current > PDF_SEARCH_CACHE_MAX_BYTES) {
      const oldest = pageTextRef.current.keys().next().value as number | undefined;
      if (oldest === undefined) break;
      const oldText = pageTextRef.current.get(oldest) ?? "";
      pageTextRef.current.delete(oldest);
      pageTextBytesRef.current -= textBytes(oldText);
    }
  }, []);

  // --- Find: extract text, count matches, navigate -------------------------------
  // NOTE (approximate by design): the match COUNT comes from each page's getTextContent() items
  // joined with a space, while the highlight <mark>s are applied per rendered text-layer span. A
  // query that straddles a text-item/span boundary can be counted but not visibly highlighted (and
  // vice versa). Accepted as approximate PDF search; the page-nav fallback always lands right.
  const runSearch = useCallback(
    async (raw: string, options: SearchOptions, notifyListeners = false): Promise<Match[]> => {
      const pdf = pdfRef.current;
      const q = raw.trim();
      searchAbortRef.current?.abort();
      const controller = new AbortController();
      searchAbortRef.current = controller;
      const seq = ++searchSeqRef.current; // supersede any in-flight search (rapid typing on big docs)
      if (!pdf || !q) {
        setMatches([]);
        setActiveIdx(0);
        return [];
      }
      // Extract uncached pages in parallel chunks instead of one page at a time — a first search on
      // a big PDF used to freeze with zero feedback until every page had round-tripped through
      // getTextContent(). Chunking keeps memory/IPC fan-out bounded while still overlapping the work.
      const CHUNK = 8;
      const pending: number[] = [];
      for (let n = 1; n <= pdf.numPages; n++) {
        if (!pageTextRef.current.has(n)) pending.push(n);
      }
      // Each page's text is scanned for matches exactly ONCE per search (counts keyed by page), and
      // publishes just flatten the counts in page order. Rescanning every page's text on every
      // chunk publish was quadratic on big PDFs (125 chunks × 1000 pages of regex work).
      const counts = new Map<number, number>();
      const scanPage = (n: number, text = pageTextRef.current.get(n) ?? "") => {
        counts.set(n, findLiteralMatches(text, q, options).length);
      };
      for (let n = 1; n <= pdf.numPages; n++) {
        if (pageTextRef.current.has(n)) scanPage(n);
      }
      const flatten = (): Match[] => {
        const found: Match[] = [];
        for (let n = 1; n <= pdf.numPages; n++) {
          const c = counts.get(n) ?? 0;
          for (let occ = 0; occ < c; occ++) found.push({ page: n, occ });
        }
        return found;
      };
      for (let i = 0; i < pending.length; i += CHUNK) {
        if (searchSeqRef.current !== seq || controller.signal.aborted) return [];
        const chunk = pending.slice(i, i + CHUNK);
        await Promise.all(
          chunk.map(async (n) => {
            try {
              const page = await pdf.getPage(n);
              const text = await readPdfPageText(page, controller.signal);
              if (controller.signal.aborted) return;
              cachePageText(n, text);
              scanPage(n, text);
            } catch {
              if (!controller.signal.aborted) {
                cachePageText(n, "");
                scanPage(n, "");
              }
            }
          }),
        );
        if (searchSeqRef.current !== seq || controller.signal.aborted) return [];
        // Publish a growing count as each chunk lands instead of staying blank until the whole
        // document is scanned; the active index is only committed once, after the final chunk.
        if (i + CHUNK < pending.length) setMatches(flatten());
      }
      if (searchSeqRef.current !== seq || controller.signal.aborted) return [];
      const found = flatten();
      const nextResult: SearchResult = {
        count: found.length,
        activeIndex: found.length ? 0 : -1,
      };
      const previousCount = matchesLenRef.current;
      const previousResult: SearchResult = {
        count: previousCount,
        activeIndex: previousCount ? activeIdxRef.current : -1,
      };
      setMatches(found);
      setActiveIdx(0);
      if (
        notifyListeners &&
        queryRef.current === q &&
        (previousResult.count !== nextResult.count ||
          previousResult.activeIndex !== nextResult.activeIndex)
      ) {
        for (const listener of searchResultListenersRef.current) listener(q, nextResult);
      }
      return found;
    },
    [cachePageText, pdfRef],
  );

  // A scanned page's empty text layer caches "" forever, leaving later OCR text invisible to find.
  // Keep the cache private while letting recognition inject authoritative original-case text and
  // recount an already-committed query through the same guarded search path.
  const setPageText = useCallback(
    (page: number, text: string): void => {
      cachePageText(page, text);
      if (queryRef.current) void runSearch(queryRef.current, optionsRef.current, true);
    },
    [cachePageText, runSearch],
  );
  const invalidatePageText = useCallback((page: number): void => {
    const previous = pageTextRef.current.get(page);
    if (previous !== undefined) pageTextBytesRef.current -= textBytes(previous);
    pageTextRef.current.delete(page);
  }, []);

  // SearchProvider for the shared FindBar. find() commits the query + options (drives the <mark>
  // highlight render) and runs the search; next/prev step the active match (live count via a ref so
  // the methods stay referentially stable). Returns the provider-owned {count, activeIndex}.
  const providerFind = useCallback(
    async (raw: string, options: SearchOptions): Promise<SearchResult> => {
      setCommittedQuery(raw.trim());
      setOpts(options);
      // FindBar-initiated searches return their result directly; pushing it too would duplicate
      // the update and can create a render loop.
      const found = await runSearch(raw, options, false);
      return { count: found.length, activeIndex: found.length ? 0 : -1 };
    },
    [runSearch],
  );
  const providerNext = useCallback((): SearchResult => {
    const n = matchesLenRef.current;
    if (!n) return NO_RESULT;
    let idx = 0;
    setActiveIdx((i) => {
      idx = (i + 1) % n;
      return idx;
    });
    return { count: n, activeIndex: idx };
  }, []);
  const providerPrev = useCallback((): SearchResult => {
    const n = matchesLenRef.current;
    if (!n) return NO_RESULT;
    let idx = 0;
    setActiveIdx((i) => {
      idx = (i - 1 + n) % n;
      return idx;
    });
    return { count: n, activeIndex: idx };
  }, []);
  const providerSubscribe = useCallback(
    (listener: (query: string, result: SearchResult) => void) => {
      searchResultListenersRef.current.add(listener);
      return () => searchResultListenersRef.current.delete(listener);
    },
    [],
  );
  const providerClose = useCallback(() => {
    searchResultListenersRef.current.clear();
    searchSeqRef.current++;
    searchAbortRef.current?.abort();
    searchAbortRef.current = null;
    setCommittedQuery("");
    setMatches([]);
    setActiveIdx(0);
    activeMarkRef.current = null;
  }, []);
  const searchProvider = useMemo<SearchProvider>(
    () => ({
      kind: "pdf",
      canReplace: false,
      supportsMatchCase: true,
      supportsWholeWord: true,
      supportsRegex: true,
      find: providerFind,
      next: providerNext,
      prev: providerPrev,
      subscribe: providerSubscribe,
      close: providerClose,
    }),
    [providerFind, providerNext, providerPrev, providerSubscribe, providerClose],
  );
  // Register on mount, unregister on unmount. DocumentRouter keys the viewer by doc.path, so a
  // document switch remounts this hook — registration rides the normal lifecycle.
  useEffect(() => {
    if (!registerSearchProvider) return;
    registerSearchProvider(searchProvider);
    return () => registerSearchProvider(null);
  }, [registerSearchProvider, searchProvider]);

  // Reset all search state when the document changes. `url` is deliberately a lifecycle trigger:
  // the state belongs to the mounted PDF, rather than a value read by the reset body.
  // biome-ignore lint/correctness/useExhaustiveDependencies: reset state when the document URL changes.
  useEffect(() => {
    pageTextRef.current.clear();
    pageTextBytesRef.current = 0;
    searchAbortRef.current?.abort();
    searchAbortRef.current = null;
    searchSeqRef.current++;
    activeMarkRef.current = null;
    setCommittedQuery("");
    setMatches([]);
    setActiveIdx(0);
    return () => {
      searchSeqRef.current++;
      searchAbortRef.current?.abort();
      searchAbortRef.current = null;
      pageTextRef.current.clear();
      pageTextBytesRef.current = 0;
    };
  }, [url]);

  // Re-apply highlights to all *currently-rendered* text layers when the query OR options change —
  // in place, without tearing pages down (that teardown was the typing jitter). Pages that scroll in
  // afterward are highlighted by renderPage via queryRef/optionsRef. Empty query flattens marks away.
  // The callback is also used after OCR mounts a recognized-text layer without changing the query.
  // biome-ignore lint/correctness/useExhaustiveDependencies: slotRefs is a stable mutable render registry.
  const rehighlight = useCallback(() => {
    const q = queryRef.current;
    const options = optionsRef.current;
    slotRefs.current.forEach((slot) => {
      for (const selector of [".textLayer", ".pdfOcrLayer"]) {
        const layer = slot.querySelector<HTMLElement>(selector);
        if (!layer) continue;
        layer.querySelectorAll<HTMLElement>("span").forEach((span) => {
          if (q) highlightSpan(span, q, options);
          else if (span.childElementCount > 0)
            span.replaceChildren(document.createTextNode(span.textContent ?? ""));
        });
      }
    });
  }, []);

  // biome-ignore lint/correctness/useExhaustiveDependencies: query/options intentionally trigger ref-driven DOM repaint.
  useEffect(() => {
    rehighlight();
  }, [committedQuery, opts, rehighlight]);

  // Emphasize the active match and bring it into view. Only scrolls when the match isn't already
  // visible. `goToPage` is the fallback when the match's page hasn't rendered yet.
  useEffect(() => {
    if (matches.length === 0) return;
    const m = matches[activeIdx];
    if (!m) return;
    let raf = 0;
    let tries = 0;
    let navigated = false;
    const tick = () => {
      const slot = slotRefs.current.get(m.page);
      const marks = slot?.querySelectorAll<HTMLElement>("mark");
      if (marks && marks.length > m.occ) {
        if (activeMarkRef.current && activeMarkRef.current !== marks[m.occ]) {
          activeMarkRef.current.style.background = MARK_BG;
        }
        const el = marks[m.occ];
        el.style.background = MARK_ACTIVE_BG;
        activeMarkRef.current = el;
        const root = scrollRef.current;
        if (root) {
          const er = el.getBoundingClientRect();
          const cr = root.getBoundingClientRect();
          if (er.top < cr.top || er.bottom > cr.bottom) el.scrollIntoView({ block: "center" });
        }
        return;
      }
      if (!navigated) {
        goToPage(m.page);
        navigated = true;
      }
      if (tries++ < 60) raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  }, [activeIdx, matches, goToPage, scrollRef, slotRefs]);

  // committedQuery drives the viewer's highlight re-render dependency; queryRef/optionsRef are read
  // by its per-page render to apply <mark>s as pages render in.
  return {
    committedQuery,
    queryRef,
    optionsRef,
    setPageText,
    invalidatePageText,
    rehighlight,
  };
}
