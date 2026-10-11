// Document init for pdf.js `getDocument`. The viewer builds its init here; hosts that drive pdf.js
// themselves use the same helper so both paths send the same source and options.
import type * as pdfjs from "pdfjs-dist";

/** Options a host may pass beside the source: any pdf.js `getDocument` parameter (for example
 *  `wasmUrl`, `cMapUrl`, `cMapPacked`, `standardFontDataUrl`, `httpHeaders`, `withCredentials`). The
 *  source itself (`url`, `data`, and a `range` transport, which pdf.js would prefer over `url`) is never
 *  taken from options; password flow is attached separately
 *  by `attachPasswordHandler`. */
export type PdfDocumentOptions = Omit<NonNullable<Parameters<typeof pdfjs.getDocument>[0]>, "url" | "data" | "range">;

export type PdfDocumentInit = NonNullable<Parameters<typeof pdfjs.getDocument>[0]>;

/** The `getDocument` init for a source: `{ url }` or `{ data }`, merged over `documentOptions`. With
 *  no options it is exactly the init the viewer has always sent. */
export function pdfDocumentInit(data: Uint8Array | string, documentOptions?: PdfDocumentOptions): PdfDocumentInit {
  const source = typeof data === "string" ? { url: data } : { data };
  if (!documentOptions) return source;
  const { url: _url, data: _data, range: _range, ...options } = documentOptions as PdfDocumentOptions & { url?: unknown; data?: unknown; range?: unknown };
  return { ...options, ...source };
}
