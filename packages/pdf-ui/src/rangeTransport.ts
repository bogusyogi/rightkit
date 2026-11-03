// HTTP range transport for pdf.js (rightkit#396, ported from ViewRight's pdfBenchTransport.ts).
// pdf.js asks a `PDFDataRangeTransport` for the byte ranges it needs, so a large PDF opens without
// downloading the whole file. Every response is checked against its `Content-Range` and the
// requested length: a server (or a custom URL scheme) that ignores `Range` fails the load with a
// clear error instead of handing pdf.js the wrong bytes.
import * as pdfjs from "pdfjs-dist";
import type { PdfDocumentInit, PdfDocumentOptions } from "./loader.js";

/** Default pdf.js `rangeChunkSize` for a range-loaded document. */
export const PDF_RANGE_CHUNK_SIZE = 64 * 1024;

/** Options of a range-transport load. */
export interface PdfRangeOptions {
  /** Extra request headers for every probe and range request (for example `Authorization`). */
  headers?: Record<string, string>;
  /** pdf.js `rangeChunkSize`. Default `PDF_RANGE_CHUNK_SIZE`. */
  chunkSize?: number;
  /** Aborts the probe and every outstanding range request. */
  signal?: AbortSignal;
}

interface ContentRange {
  readonly begin: number;
  readonly end: number;
  readonly total: number;
}

/** `Range` header for the half-open byte interval `[begin, end)`. */
export function formatPdfRangeHeader(begin: number, end: number): string {
  if (!Number.isInteger(begin) || !Number.isInteger(end) || begin < 0 || end <= begin) {
    throw new Error(`Invalid PDF byte range [${begin}, ${end})`);
  }
  return `bytes=${begin}-${end - 1}`;
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

/** The total length a `Content-Range` header reports, or null when it is absent or malformed. */
export function parseContentRangeTotal(value: string | null): number | null {
  return parseContentRange(value)?.total ?? null;
}

function parseContentLength(value: string | null): number | null {
  if (!value) return null;
  const length = Number(value);
  return Number.isSafeInteger(length) && length > 0 ? length : null;
}

function isAbort(error: unknown): boolean {
  return (error as { name?: string } | null)?.name === "AbortError";
}

interface LengthProbe {
  readonly length: number;
  readonly initialData: Uint8Array | null;
}

/** The document's total length: `HEAD` when the server answers it, otherwise a one-byte range
 *  request (whose byte is returned as `initialData`). Rejects when the server offers neither. */
async function probePdfLength(url: string, options: PdfRangeOptions): Promise<LengthProbe> {
  const { headers, signal } = options;
  try {
    const head = await fetch(url, { method: "HEAD", signal, cache: "no-store", headers });
    const length = head.ok ? parseContentLength(head.headers.get("content-length")) : null;
    if (length !== null) return { length, initialData: null };
  } catch (error) {
    if (isAbort(error)) throw error;
    // HEAD is not supported everywhere (custom URL schemes in a webview): fall through.
  }
  const probe = await fetch(url, {
    signal,
    cache: "no-store",
    headers: { ...headers, Range: "bytes=0-0" },
  });
  if (probe.status !== 206) {
    throw new Error(`PDF range probe failed: expected 206, got ${probe.status}`);
  }
  const range = parseContentRange(probe.headers.get("content-range"));
  if (!range) throw new Error("PDF range probe did not expose a valid total length");
  const initialData = new Uint8Array(await probe.arrayBuffer());
  if (range.begin !== 0 || range.end !== initialData.byteLength - 1) {
    throw new Error("PDF range probe returned bytes that do not match Content-Range");
  }
  return { length: range.total, initialData };
}

/** A pdf.js `PDFDataRangeTransport` that serves explicit ranges over `fetch`. */
export class PdfHttpRangeTransport extends pdfjs.PDFDataRangeTransport {
  readonly #url: string;
  readonly #length: number;
  readonly #headers: Record<string, string> | undefined;
  readonly #controllers = new Set<AbortController>();
  #aborted = false;
  #failure: Error | null = null;
  #onFailure: ((error: Error) => void) | null = null;

  constructor(url: string, length: number, initialData: Uint8Array | null, headers?: Record<string, string>) {
    // progressiveDone = true: this transport supplies explicit ranges only, never a stream.
    super(length, initialData, true);
    this.#url = url;
    this.#length = length;
    this.#headers = headers;
  }

  /** The error that ended a range request, if one did. */
  get failure(): Error | null {
    return this.#failure;
  }

  /** Called once for the first failed range request (the viewer destroys the load task). */
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
      headers: { ...this.#headers, Range: formatPdfRangeHeader(begin, end) },
    })
      .then(async (response) => {
        if (response.status !== 206) {
          throw new Error(`PDF range [${begin}, ${end}) failed: expected 206, got ${response.status}`);
        }
        const bytes = new Uint8Array(await response.arrayBuffer());
        const range = parseContentRange(response.headers.get("content-range"));
        if (!range || range.begin !== begin || range.end !== end - 1 || range.total !== this.#length) {
          throw new Error(`PDF range [${begin}, ${end}) returned an invalid Content-Range`);
        }
        if (bytes.byteLength !== end - begin) {
          throw new Error(`PDF range [${begin}, ${end}) returned ${bytes.byteLength} bytes; expected ${end - begin}`);
        }
        if (!this.#aborted) this.onDataRange(begin, bytes);
      })
      .catch((error: unknown) => {
        if (this.#aborted || isAbort(error)) return;
        if (this.#failure) return;
        const failure = error instanceof Error ? error : new Error(String(error));
        this.#failure = failure;
        this.#onFailure?.(failure);
      })
      .finally(() => {
        this.#controllers.delete(controller);
      });
  }

  /** Cancels every outstanding request and ignores later data. */
  abort(): void {
    if (this.#aborted) return;
    this.#aborted = true;
    for (const controller of this.#controllers) controller.abort();
    this.#controllers.clear();
  }
}

/** Probes `url` and returns a range transport for it. */
export async function createPdfRangeTransport(url: string, options: PdfRangeOptions = {}): Promise<PdfHttpRangeTransport> {
  const probe = await probePdfLength(url, options);
  const transport = new PdfHttpRangeTransport(url, probe.length, probe.initialData, options.headers);
  options.signal?.addEventListener("abort", () => transport.abort(), { once: true });
  return transport;
}

/** The `getDocument` init for a range transport, merged over `documentOptions`. As with
 *  `pdfDocumentInit`, `url`, `data` and `range` never come from the options. Streaming is off so
 *  pdf.js reads only through the transport. */
export function pdfRangeDocumentInit(
  transport: pdfjs.PDFDataRangeTransport,
  documentOptions?: PdfDocumentOptions,
  chunkSize: number = PDF_RANGE_CHUNK_SIZE,
): PdfDocumentInit {
  const { url: _url, data: _data, range: _range, ...options } = (documentOptions ?? {}) as PdfDocumentOptions & { url?: unknown; data?: unknown; range?: unknown };
  return { rangeChunkSize: chunkSize, ...options, range: transport, disableRange: false, disableStream: true };
}
