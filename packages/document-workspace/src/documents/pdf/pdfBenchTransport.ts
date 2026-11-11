import * as pdfjs from "pdfjs-dist";
import type { PDFDocumentProxy } from "pdfjs-dist";
import {
  createPdfRangeTransport,
  pdfDocumentInit,
  pdfRangeDocumentInit,
  PDF_RANGE_CHUNK_SIZE,
} from "@rightkit/pdf-ui";
import { isPdfBenchEnabled, type PdfBenchLoadMode } from "./pdfBench.js";

const RANGE_CHUNK_SIZE = PDF_RANGE_CHUNK_SIZE;
/** Above this size the whole-file buffer fallback is skipped in favour of range loading. */
export const MAX_BUFFERED_PDF_BYTES = 150 * 1024 * 1024;

export function shouldRangeLoadInsteadOfBuffer(contentLength: string | null): boolean {
  const n = Number(contentLength);
  return Number.isFinite(n) && n > MAX_BUFFERED_PDF_BYTES;
}

export interface PreparedPdfLoading {
  readonly task: ReturnType<typeof pdfjs.getDocument>;
  readonly promise: Promise<PDFDocumentProxy>;
  dispose(): void;
}

interface PreparePdfLoadingArgs {
  readonly url: string;
  readonly mode: PdfBenchLoadMode;
  readonly signal: AbortSignal;
  readonly wasmUrl: string;
}

function commonOptions(wasmUrl: string) {
  return {
    wasmUrl,
    useWorkerFetch: false,
    // Only the deterministic benchmark pins OffscreenCanvas off. This transport also serves every
    // ordinary PDF load, and pdf.js's save path REQUIRES OffscreenCanvas to encode Stamp (image)
    // annotations: with it forced off, `saveDocument()` hands the stamp writer a null image and
    // throws ("Cannot destructure property 'imageRef'"), so every image-stamp save failed with
    // "annotations could not be serialized". Let pdf.js detect support in the real app.
    isOffscreenCanvasSupported: isPdfBenchEnabled()
      ? false
      : typeof OffscreenCanvas !== "undefined",
    isImageDecoderSupported: false,
  } as const;
}

export async function preparePdfLoading({
  url,
  mode,
  signal,
  wasmUrl,
}: PreparePdfLoadingArgs): Promise<PreparedPdfLoading> {
  let bufferResponse: Response | null = null;
  if (mode === "buffer") {
    const response = await fetch(url, { signal });
    if (!response.ok) {
      throw new Error(`PDF fetch failed: ${response.status} ${response.statusText}`.trim());
    }
    if (shouldRangeLoadInsteadOfBuffer(response.headers.get("content-length"))) {
      void response.body?.cancel().catch(() => {});
    } else {
      bufferResponse = response;
    }
  }
  if (bufferResponse) {
    const response = bufferResponse;
    const data = new Uint8Array(await response.arrayBuffer());
    const task = pdfjs.getDocument(pdfDocumentInit(data, commonOptions(wasmUrl)));
    let disposed = false;
    const dispose = () => {
      if (disposed) return;
      disposed = true;
      void task.destroy();
    };
    signal.addEventListener("abort", dispose, { once: true });
    return { task, promise: task.promise, dispose };
  }

  if (mode === "url-native") {
    // Deliberately use PDF.js's own Fetch/XHR network stream with default stream/range behavior.
    const task = pdfjs.getDocument(
      pdfDocumentInit(url, { rangeChunkSize: RANGE_CHUNK_SIZE, ...commonOptions(wasmUrl) }),
    );
    let disposed = false;
    const dispose = () => {
      if (disposed) return;
      disposed = true;
      void task.destroy();
    };
    signal.addEventListener("abort", dispose, { once: true });
    return { task, promise: task.promise, dispose };
  }

  // Range loading goes through @rightkit/pdf-ui's transport (the one copy; this file used to carry
  // its own HostPdfRangeTransport): HEAD or a one-byte probe, then explicit 206-checked ranges.
  const transport = await createPdfRangeTransport(url, { signal });
  const task = pdfjs.getDocument(pdfRangeDocumentInit(transport, commonOptions(wasmUrl), RANGE_CHUNK_SIZE));

  transport.setFailureHandler((error) => {
    console.error("[pdf-bench] custom range transport failed", error);
    void task.destroy();
  });

  const promise = task.promise.catch((error) => {
    throw transport.failure ?? error;
  });

  let disposed = false;
  const dispose = () => {
    if (disposed) return;
    disposed = true;
    transport.abort();
    void task.destroy();
  };
  signal.addEventListener("abort", dispose, { once: true });

  return {
    task,
    promise,
    dispose,
  };
}
