import type { MutableRefObject, RefObject, WheelEventHandler } from "react";
import type { PDFDocumentProxy } from "pdfjs-dist";
import type { OutlineNode, Size } from "./pdfShared.js";
import { PdfOutline } from "./PdfOutline.js";
import { PdfThumbnails } from "./PdfThumbnails.js";
import { PdfOcrLayer } from "./PdfOcrLayer.js";
import type { OcrLine } from "./pdfOcr.js";

export function PdfViewerBody({
  panel,
  outline,
  pdfDoc,
  numPages,
  current,
  rotation,
  base,
  pageSizes,
  scale,
  renderErrors,
  ocrLinesByPage,
  scrollRef,
  slotRefs,
  onSelectDestination,
  onSelectPage,
  onWheel,
}: {
  panel: null | "outline" | "thumbs";
  outline: OutlineNode[] | null;
  pdfDoc: PDFDocumentProxy | null;
  numPages: number;
  current: number;
  rotation: number;
  base: Size | null;
  pageSizes: Map<number, Size>;
  scale: number;
  renderErrors: Map<number, string>;
  ocrLinesByPage: ReadonlyMap<number, OcrLine[]>;
  scrollRef: RefObject<HTMLDivElement | null>;
  slotRefs: MutableRefObject<Map<number, HTMLDivElement>>;
  onSelectDestination: (dest: string | unknown[] | null) => void;
  onSelectPage: (page: number) => void;
  onWheel?: WheelEventHandler<HTMLDivElement>;
}) {
  return (
    <div className="relative flex min-h-0 flex-1">
      {panel && (
        <aside
          className={`no-print shrink-0 overflow-hidden border-r border-line bg-panel ${panel === "outline" ? "w-64" : "w-44"}`}
        >
          {panel === "outline" && outline && outline.length > 0 && (
            <div className="h-full overflow-auto py-2 text-xs">
              <PdfOutline nodes={outline} onSelect={onSelectDestination} />
            </div>
          )}
          {panel === "thumbs" && pdfDoc && (
            <PdfThumbnails
              pdf={pdfDoc}
              numPages={numPages}
              current={current}
              rotation={rotation}
              onSelect={onSelectPage}
            />
          )}
        </aside>
      )}

      {/* The tool bar is docked in its own row above this region, so the scroll box starts below
          it and no top clearance is needed. */}
      <div
        ref={scrollRef}
        role="region"
        aria-label="PDF pages"
        onWheel={onWheel}
        className="min-h-0 flex-1 overflow-auto bg-ink/40 outline-none"
      >
        <div className="flex min-w-full flex-col items-start gap-4 py-6">
          {base &&
            Array.from({ length: numPages }, (_, i) => i + 1).map((n) => {
              const fallback = rotation % 180 !== 0 ? { w: base.h, h: base.w } : base;
              const sz = pageSizes.get(n) ?? fallback;
              return (
                <div
                  key={n}
                  data-page={n}
                  ref={(el) => {
                    if (el) slotRefs.current.set(n, el);
                    else slotRefs.current.delete(n);
                  }}
                  className="relative mx-auto bg-white shadow"
                  // T4-5 (conservative fix — see docs/PERF-AUDIT-2026-07-15.md): rather than
                  // mounting/unmounting slot DOM nodes as the user scrolls (true windowing), keep
                  // every slot mounted but tell the engine off-screen ones don't need layout/paint
                  // work. `content-visibility: auto` skips style/layout/paint for subtrees the
                  // browser judges "not relevant to the user" while still keeping the element (and
                  // its box, since we already set an explicit width/height) in normal flow — so
                  // `slotRefs`, `goToPage`'s `scrollIntoView`, the initial-view restore effect, and
                  // the canvas IntersectionObserver in PdfViewer.tsx all keep working unchanged, with
                  // zero risk to scroll-to-page/current-page tracking. True remove-from-DOM
                  // windowing was not implemented: page heights are per-page-measured (mixed
                  // orientation docs — see the "D1" comment in PdfViewer.tsx's renderPage) rather
                  // than uniform, so an accurate windowed slot list needs an offset-aware version of
                  // `thumbnailWindow()` (see @rightkit/pdf-ui) PLUS teaching
                  // `goToPage`/the restore effect/the IntersectionObserver's `observe()` calls to
                  // handle a target page whose slot isn't currently mounted — doable, but risky to
                  // ship unverified (no way to interactively confirm scroll-to-page/search-jump/
                  // outline-nav still land correctly in this pass). `content-visibility` is a no-op
                  // in engines that don't support it (graceful degradation), so this is safe on both
                  // WebView2 and WKWebView regardless of version.
                  style={{
                    width: sz.w * scale,
                    height: sz.h * scale,
                    contentVisibility: "auto",
                    containIntrinsicSize: `${sz.w * scale}px ${sz.h * scale}px`,
                  }}
                >
                  {ocrLinesByPage.has(n) && <PdfOcrLayer lines={ocrLinesByPage.get(n) ?? []} />}
                  {renderErrors.has(n) && (
                    <div className="absolute inset-0 flex items-center justify-center bg-[var(--reader-surface)] p-4 text-center text-xs text-danger">
                      Page {n} render error: {renderErrors.get(n)}
                    </div>
                  )}
                </div>
              );
            })}
        </div>
      </div>
    </div>
  );
}
