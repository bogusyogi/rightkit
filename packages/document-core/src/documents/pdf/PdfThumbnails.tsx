import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { PDFDocumentProxy } from "pdfjs-dist";
import { boundedCanvasDpr, THUMB_W, viewportRotation } from "./pdfShared.js";
import { thumbnailWindow } from "@rightkit/pdf-ui";

const THUMB_ITEM_HEIGHT = Math.round(THUMB_W * 1.3) + 30;
const THUMB_OVERSCAN = 2;

// Page-thumbnail rail. Lazily renders each page to a small canvas as it scrolls into the rail
// (IntersectionObserver), caches the result, and re-renders when rotation changes. Clicking a
// thumbnail navigates the main view; the current page is highlighted and scrolled into view.
export function PdfThumbnails({
  pdf,
  numPages,
  current,
  rotation,
  onSelect,
}: {
  pdf: PDFDocumentProxy;
  numPages: number;
  current: number;
  rotation: number;
  onSelect: (n: number) => void;
}) {
  const rootRef = useRef<HTMLDivElement>(null);
  const observerRef = useRef<IntersectionObserver | null>(null);
  const slotRefs = useRef<Map<number, HTMLDivElement>>(new Map());
  const renderedRef = useRef<Set<number>>(new Set());
  const visibleRef = useRef<Set<number>>(new Set());
  const generationRef = useRef(0);
  const renderTasksRef = useRef<Map<number, { cancel?: () => void }>>(new Map());
  const renderAttemptsRef = useRef<Map<number, object>>(new Map());
  const [viewportHeight, setViewportHeight] = useState(600);
  const [scrollTop, setScrollTop] = useState(0);
  const windowed = thumbnailWindow({
    numPages,
    scrollTop,
    viewportHeight,
    itemHeight: THUMB_ITEM_HEIGHT,
    overscan: THUMB_OVERSCAN,
  });
  const pages = useMemo(
    () =>
      Array.from(
        { length: Math.max(0, windowed.end - windowed.start + 1) },
        (_, index) => windowed.start + index,
      ),
    [windowed.end, windowed.start],
  );

  const cancelThumb = useCallback((n: number) => {
    const task = renderTasksRef.current.get(n);
    renderTasksRef.current.delete(n);
    renderAttemptsRef.current.delete(n);
    try {
      task?.cancel?.();
    } catch {
      // pdf.js cancellation is best-effort during slot reuse and unmount.
    }
    renderedRef.current.delete(n);
  }, []);

  useEffect(() => {
    const root = rootRef.current;
    if (!root) return;
    const updateHeight = () => setViewportHeight(Math.max(1, root.clientHeight));
    updateHeight();
    const observer = new ResizeObserver(updateHeight);
    observer.observe(root);
    return () => observer.disconnect();
  }, []);

  const renderThumb = useCallback(
    async (n: number) => {
      if (renderedRef.current.has(n)) return;
      const generation = generationRef.current;
      const attempt = {};
      renderAttemptsRef.current.set(n, attempt);
      renderedRef.current.add(n);
      const slot = slotRefs.current.get(n);
      if (!slot) {
        renderAttemptsRef.current.delete(n);
        renderedRef.current.delete(n);
        return;
      }
      try {
        const page = await pdf.getPage(n);
        if (
          renderAttemptsRef.current.get(n) !== attempt ||
          generationRef.current !== generation ||
          slotRefs.current.get(n) !== slot
        ) {
          if (renderAttemptsRef.current.get(n) === attempt) {
            renderAttemptsRef.current.delete(n);
            renderedRef.current.delete(n);
          }
          return;
        }
        const pageRotation = viewportRotation(page.rotate, rotation);
        const unit = page.getViewport({ scale: 1, rotation: pageRotation });
        const scale = THUMB_W / unit.width;
        const vp = page.getViewport({ scale, rotation: pageRotation });
        const dpr = boundedCanvasDpr(vp, window.devicePixelRatio || 1);
        const canvas = document.createElement("canvas");
        canvas.width = Math.floor(vp.width * dpr);
        canvas.height = Math.floor(vp.height * dpr);
        canvas.style.width = `${vp.width}px`;
        canvas.style.height = `${vp.height}px`;
        const ctx = canvas.getContext("2d");
        if (!ctx) {
          renderAttemptsRef.current.delete(n);
          renderedRef.current.delete(n);
          return;
        }
        // intent: "print" is NOT about printing — it's the cheapest way to give this render its own
        // pdf.js-internal render slot. PdfViewer's main pane calls page.render() on the SAME cached
        // PDFPageProxy (pdf.js caches proxies per document) for the same page, under the default
        // "display" intent. pdf.js keys concurrent render/operator-list state by rendering intent
        // (_intentStates, keyed by a cacheKey derived from the intent flags — see pdf.mjs
        // getRenderingIntent/render), so two "display" renders on one page proxy collide and one
        // silently produces a blank canvas. A distinct intent gets its own intentState entry instead
        // of a second parsed document, which the print path uses for the same underlying collision
        // (documentPrinting.ts printPdfBytes).
        const renderTask = page.render({
          canvas,
          canvasContext: ctx,
          viewport: vp,
          intent: "print",
          transform: dpr !== 1 ? [dpr, 0, 0, dpr, 0, 0] : undefined,
        });
        renderTasksRef.current.set(n, renderTask);
        try {
          await renderTask.promise;
        } finally {
          if (renderTasksRef.current.get(n) === renderTask) renderTasksRef.current.delete(n);
        }
        // Only discard on a genuine slot swap (this numbered DOM slot got reused for a different
        // page — real virtualization reuse), NOT on `!visibleRef.current.has(n)`. That extra check
        // used to race: `rotation` settles one commit after `pdf` (usePdfDocumentLoader sets them in
        // two separate setState calls either side of an `await`), so the observer-setup effect below
        // (deps: [renderThumb], which depends on `rotation`) can tear down and rebuild while THIS
        // render is still in flight — its cleanup clears visibleRef out from under a render that was
        // legitimately visible when it started and still is when it finishes, so the completed,
        // correctly-rasterized canvas got thrown away every time. If the page truly scrolled out of
        // view in the meantime, the intersection observer's own "not intersecting" branch already
        // removes the canvas — that's the correct place for that decision, not a second check here.
        if (
          renderAttemptsRef.current.get(n) !== attempt ||
          generationRef.current !== generation ||
          slotRefs.current.get(n) !== slot
        ) {
          if (renderAttemptsRef.current.get(n) === attempt) {
            renderAttemptsRef.current.delete(n);
            renderedRef.current.delete(n);
          }
          return;
        }
        slot.querySelector("canvas")?.remove();
        slot.appendChild(canvas);
      } catch {
        if (renderAttemptsRef.current.get(n) === attempt) {
          renderAttemptsRef.current.delete(n);
          renderedRef.current.delete(n); // allow a retry on the next intersection
        }
      }
    },
    [pdf, rotation],
  );

  // (Re)render visible thumbnails. Re-runs when rotation changes: clear the cache + existing
  // canvases so each page re-rasterizes at the new orientation.
  useEffect(() => {
    generationRef.current += 1;
    renderTasksRef.current.forEach((_, n) => cancelThumb(n));
    renderedRef.current.clear();
    visibleRef.current.clear();
    slotRefs.current.forEach((el) => el.querySelector("canvas")?.remove());
    const root = rootRef.current;
    if (!root) return;
    const obs = new IntersectionObserver(
      (entries) => {
        for (const e of entries) {
          const n = Number((e.target as HTMLElement).dataset.page);
          if (e.isIntersecting) {
            visibleRef.current.add(n);
            void renderThumb(n);
          } else {
            visibleRef.current.delete(n);
            cancelThumb(n);
            e.target.querySelector("canvas")?.remove();
          }
        }
      },
      { root, rootMargin: "200px 0px" },
    );
    slotRefs.current.forEach((el) => obs.observe(el));
    observerRef.current = obs;
    return () => {
      visibleRef.current.clear();
      renderTasksRef.current.forEach((_, n) => cancelThumb(n));
      observerRef.current = null;
      obs.disconnect();
    };
  }, [cancelThumb, renderThumb]);

  // Keep the current page's thumbnail visible as the main view scrolls.
  useEffect(() => {
    const root = rootRef.current;
    if (!root) return;
    const top = (current - 1) * THUMB_ITEM_HEIGHT;
    const bottom = top + THUMB_ITEM_HEIGHT;
    if (top < root.scrollTop) root.scrollTop = top;
    else if (bottom > root.scrollTop + root.clientHeight)
      root.scrollTop = bottom - root.clientHeight;
  }, [current]);

  return (
    <div
      ref={rootRef}
      className="h-full overflow-auto py-2 text-chrome-xs"
      onScroll={(event) => setScrollTop(event.currentTarget.scrollTop)}
    >
      {windowed.before > 0 && <div aria-hidden="true" style={{ height: windowed.before }} />}
      {pages.map((n) => (
        <button
          type="button"
          key={n}
          onClick={() => onSelect(n)}
          className="flex w-full flex-col items-center gap-1 py-1.5"
          style={{ height: THUMB_ITEM_HEIGHT }}
          title={`Page ${n}`}
        >
          <div
            data-page={n}
            ref={(el) => {
              if (el) {
                slotRefs.current.set(n, el);
                observerRef.current?.observe(el);
              } else {
                cancelThumb(n);
                slotRefs.current.delete(n);
                visibleRef.current.delete(n);
              }
            }}
            className={`flex items-center justify-center overflow-hidden border bg-white ${n === current ? "border-copper" : "border-line"}`}
            style={{ width: THUMB_W, minHeight: Math.round(THUMB_W * 1.3) }}
          />
          <span className={`font-mono ${n === current ? "text-copper-hi" : "text-muted-2"}`}>
            {n}
          </span>
        </button>
      ))}
      {windowed.after > 0 && <div aria-hidden="true" style={{ height: windowed.after }} />}
    </div>
  );
}
