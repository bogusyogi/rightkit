import * as pdfjs from "pdfjs-dist";
import type { PDFDocumentProxy } from "pdfjs-dist";
import { pdfDocumentInit } from "@rightkit/pdf-ui";
import { mountPrintImage, printHost } from "../../app/printPipelineShared.js";
import { withTimeout } from "./pdfShared.js";

// pdf.js's getDocument() spins up a Web Worker to parse the PDF off the main thread, and if that
// worker never starts (silently, e.g. resolving `?url` through a fresh dynamic import chunk rather
// than the already-loaded static module), `getDocument().promise` just hangs forever — no error, no
// rejection, nothing to catch. That hang was indistinguishable from a printer dialog never opening.
// The worker URL must therefore be resolved exactly ONCE, by the host app at startup, through
// @rightkit/pdf-ui's configurePdfJs({ workerSrc }) (a Vite host: `import workerUrl from
// "pdfjs-dist/build/pdf.worker.min.mjs?url"`): a published package cannot resolve a bundler URL. A hard
// timeout below converts any hang into a visible failure instead of silence.

/** Rasterize PDF bytes into the print host as flat images, then trigger native print.
 *
 *  Two deliberate choices, both learned the hard way on macOS:
 *
 *  1. A SEPARATE pdf.js document is loaded from these bytes, never the on-screen viewer's live
 *     `PDFDocumentProxy`. pdf.js caches page proxies per document, so printing off the viewer's copy
 *     issues a second `render()` against a `PDFPageProxy` the viewer may already be rendering — a
 *     documented failure class whose symptom is a render promise that never settles, i.e. print that
 *     silently does nothing. An isolated document shares no page state.
 *  2. Each page is rasterized to a canvas and then flattened to a `data:` image. The canvas itself is
 *     NOT appended: WKWebView's print pass re-renders the page into a print context and canvas
 *     backing stores do not reliably survive it (the same reason the image viewer had to print a flat
 *     `<img src="data:...">` instead of its live stage). Images do survive.
 *
 *  Every await is bounded, so a hang surfaces as a rejected promise the caller can toast rather than
 *  as silence. */
export async function printPdfBytes(bytes: ArrayBuffer | Uint8Array): Promise<boolean> {
  const data = bytes instanceof Uint8Array ? bytes.slice() : new Uint8Array(bytes.slice(0));
  const loadingTask = pdfjs.getDocument(pdfDocumentInit(data));
  try {
    const doc = await withTimeout(loadingTask.promise, 20_000, "pdf.js getDocument timed out");
    let mountedPages = 0;
    return await printHost(async (host) => {
      for (let i = 1; i <= doc.numPages; i++) {
        const page = await withTimeout(doc.getPage(i), 20_000, `pdf.js getPage(${i}) timed out`);
        try {
          const viewport = page.getViewport({ scale: 2 });
          const canvas = document.createElement("canvas");
          canvas.width = viewport.width;
          canvas.height = viewport.height;
          try {
            const ctx = canvas.getContext("2d");
            if (!ctx) throw new Error(`PDF print page ${i} has no 2D canvas context`);
            const renderTask = page.render({ canvas, canvasContext: ctx, viewport });
            try {
              await withTimeout(renderTask.promise, 30_000, `pdf.js render(page ${i}) timed out`);
            } catch (error) {
              // A timeout only races the promise; cancel the underlying render before cleanup and
              // backing-store release so a late worker completion cannot paint a detached canvas.
              renderTask.cancel?.();
              throw error;
            }

            if (mountedPages === 0) {
              const pageSize = document.createElement("style");
              // PDF.js viewport units at scale 1 are PDF points. Matching @page to those dimensions
              // makes each raster image exactly one printer sheet; width-only fitting spilled an A4
              // PDF onto Letter's next sheet, which looked like alternating blank pages.
              pageSize.textContent = `@page { size: ${viewport.width / 2}pt ${viewport.height / 2}pt; margin: 0; }`;
              host.appendChild(pageSize);
            }

            // Prepare one page at a time. The host must retain every decoded image for native print,
            // but no second all-pages `pageImages` array or canvas backing stores are retained while
            // later pages render. Data URLs remain the portable representation required by the
            // Windows preview webview and by WKWebView's raster print path.
            const src = canvas.toDataURL("image/png");
            await mountPrintImage(
              host,
              src,
              "box-sizing:border-box;display:block;width:99.5%;height:auto;margin:0 auto;break-inside:avoid;page-break-inside:avoid;",
            );
            mountedPages += 1;
          } finally {
            // Explicitly release temporary raster backing store before preparing next page.
            canvas.width = 0;
            canvas.height = 0;
          }
        } finally {
          page.cleanup();
        }
      }
      return mountedPages > 0;
    });
  } finally {
    try {
      await withTimeout(loadingTask.destroy(), 5_000, "pdf.js print cleanup timed out");
    } catch {
      // Cleanup failure must not become an unhandled rejection or replace the print error.
    }
  }
}

/** Print the PDF a viewer pane already has open, WITHOUT re-fetching it. `getData()` hands back the
 *  bytes pdf.js already holds in memory, so this never re-opens the revocable viewright-stream:// URL
 *  that the on-screen viewer is still streaming from. */
export async function printPdfDocument(doc: PDFDocumentProxy): Promise<boolean> {
  const bytes = await withTimeout(doc.getData(), 15_000, "pdf.js getData timed out");
  return printPdfBytes(bytes);
}

/** Fallback PDF print path for when no viewer pane is registered: fetch the bytes from the stream URL
 *  and rasterize them. Prefer printPdfDocument whenever a pane is mounted — it skips the fetch. NOT a
 *  plain `<iframe src=blobURL>`: WKWebView does not reliably render a PDF blob inline via a bare
 *  iframe navigation, so `iframe.onload` never fires and print() never runs — the original silent
 *  no-op on macOS that this replaced. */
export async function printPdf(assetUrl: string): Promise<boolean> {
  const bytes = await withTimeout(
    fetch(assetUrl).then((r) => r.arrayBuffer()),
    15_000,
    "pdf byte fetch timed out",
  );
  return printPdfBytes(bytes);
}
