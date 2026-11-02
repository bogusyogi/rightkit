import * as pdfjs from "pdfjs-dist";
import type { PDFDocumentProxy } from "pdfjs-dist";
import { pdfDocumentInit } from "@rightkit/pdf-ui";
import { isPdfBenchEnabled, type PdfBenchLoadMode } from "./pdfBench.js";

const RANGE_CHUNK_SIZE = 64 * 1024;
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

interface LengthProbe {
  readonly length: number;
  readonly initialData: Uint8Array | null;
}

interface ContentRange {
  readonly begin: number;
  readonly end: number;
  readonly total: number;
}

export function formatPdfRangeHeader(begin: number, end: number): string {
  if (!Number.isInteger(begin) || !Number.isInteger(end) || begin < 0 || end <= begin) {
    throw new Error(`Invalid PDF byte range [${begin}, ${end})`);
  }
  return `bytes=${begin}-${end - 1}`;
}

export function parseContentRangeTotal(value: string | null): number | null {
  return parseContentRange(value)?.total ?? null;
}

function parseContentRange(value: string | null): ContentRange | null {
  if (!value) return null;
  const match = value.match(/^bytes\s+(\d+)-(\d+)\/(\d+)$/i);
  if (!match) return null;
  const begin = Number(match[1]);
  const end = Number(match[2]);
  const total = Number(match[3]);
  if (
    !Number.isSafeInteger(begin) ||
    !Number.isSafeInteger(end) ||
    !Number.isSafeInteger(total) ||
    begin < 0 ||
    end < begin ||
    total <= end
  ) {
    return null;
  }
  return { begin, end, total };
}

function parseContentLength(value: string | null): number | null {
  if (!value) return null;
  const length = Number(value);
  return Number.isSafeInteger(length) && length > 0 ? length : null;
}

async function probePdfLength(url: string, signal: AbortSignal): Promise<LengthProbe> {
  // HEAD is cheapest when the webview/custom protocol handles it correctly.
  try {
    const head = await fetch(url, { method: "HEAD", signal, cache: "no-store" });
    const length = head.ok ? parseContentLength(head.headers.get("content-length")) : null;
    if (length !== null) return { length, initialData: null };
  } catch (error) {
    if ((error as { name?: string })?.name === "AbortError") throw error;
    // Fall through to a 1-byte range probe. Benchmarking exists specifically because custom-scheme
    // behavior differs across webviews; HEAD support must not be assumed.
  }

  const probe = await fetch(url, {
    signal,
    cache: "no-store",
    headers: { Range: "bytes=0-0" },
  });
  if (probe.status !== 206) {
    throw new Error(`PDF range probe failed: expected 206, got ${probe.status}`);
  }
  const length = parseContentRangeTotal(probe.headers.get("content-range"));
  if (length === null) throw new Error("PDF range probe did not expose a valid total length");
  const initialData = new Uint8Array(await probe.arrayBuffer());
  const range = parseContentRange(probe.headers.get("content-range"));
  if (!range || range.begin !== 0 || range.end !== initialData.byteLength - 1) {
    throw new Error("PDF range probe returned bytes that do not match Content-Range");
  }
  return { length, initialData };
}

class HostPdfRangeTransport extends pdfjs.PDFDataRangeTransport {
  readonly #url: string;
  readonly #length: number;
  readonly #controllers = new Set<AbortController>();
  #aborted = false;
  #failure: Error | null = null;
  #onFailure: ((error: Error) => void) | null = null;

  constructor(url: string, length: number, initialData: Uint8Array | null) {
    // progressiveDone=true: this transport intentionally supplies explicit ranges only.
    super(length, initialData, true);
    this.#url = url;
    this.#length = length;
  }

  get failure(): Error | null {
    return this.#failure;
  }

  setFailureHandler(handler: (error: Error) => void): void {
    this.#onFailure = handler;
  }

  requestDataRange(begin: number, end: number): void {
    if (this.#aborted) return;

    const controller = new AbortController();
    this.#controllers.add(controller);

    void fetch(this.#url, {
      signal: controller.signal,
      cache: "no-store",
      headers: { Range: formatPdfRangeHeader(begin, end) },
    })
      .then(async (response) => {
        if (response.status !== 206) {
          throw new Error(
            `PDF custom range failed for [${begin}, ${end}): expected 206, got ${response.status}`,
          );
        }

        const bytes = new Uint8Array(await response.arrayBuffer());
        const expected = end - begin;
        const range = parseContentRange(response.headers.get("content-range"));
        if (
          !range ||
          range.begin !== begin ||
          range.end !== end - 1 ||
          range.total !== this.#length
        ) {
          throw new Error(`PDF custom range returned invalid Content-Range for [${begin}, ${end})`);
        }
        if (bytes.byteLength !== expected) {
          throw new Error(
            `PDF custom range returned ${bytes.byteLength} bytes for [${begin}, ${end}); expected ${expected}`,
          );
        }

        if (!this.#aborted) this.onDataRange(begin, bytes);
      })
      .catch((error) => {
        if (this.#aborted || (error as { name?: string })?.name === "AbortError") return;
        const failure =
          error instanceof Error
            ? error
            : new Error(String((error as { message?: unknown })?.message ?? error));
        this.#failure = failure;
        this.#onFailure?.(failure);
      })
      .finally(() => {
        this.#controllers.delete(controller);
      });
  }

  abort(): void {
    if (this.#aborted) return;
    this.#aborted = true;
    for (const controller of this.#controllers) controller.abort();
    this.#controllers.clear();
  }
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

  const probe = await probePdfLength(url, signal);
  const transport = new HostPdfRangeTransport(url, probe.length, probe.initialData);
  const task = pdfjs.getDocument({
    range: transport,
    rangeChunkSize: RANGE_CHUNK_SIZE,
    disableRange: false,
    disableStream: true,
    ...commonOptions(wasmUrl),
  });

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
