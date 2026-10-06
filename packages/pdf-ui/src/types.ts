// Public contracts for @rightkit/pdf-ui. No app, Tauri, or file-system imports: an app supplies
// bytes (or a URL/loader) and the viewer renders them read-only.

/** Where a PDF comes from. Apps adapt their own file access (for example @rightkit/shell reads)
 *  into one of these shapes; this package never reads files itself.
 *
 *  Keep the object referentially stable (useMemo / state): a new `source` object loads a new
 *  document. `bytes` are copied before they reach the pdf.js worker, so the caller's buffer is
 *  never detached. */
export type PdfSource =
  | { kind: "bytes"; data: Uint8Array }
  | { kind: "url"; url: string }
  | { kind: "loader"; load(): Promise<Uint8Array> };

export type PdfFitMode = "width" | "page" | null;
export type PdfPanel = null | "outline" | "thumbs";

/** Persistable view state. All fields optional on input; all present on output. */
export interface PdfViewState {
  /** 1-based page the view is on. */
  page: number;
  scale: number;
  fit: PdfFitMode;
  /** 0 | 90 | 180 | 270, applied to every page. */
  rotation: number;
  panel: PdfPanel;
}

export interface PdfDocumentInfo {
  numPages: number;
  hasOutline: boolean;
}

export interface PdfSearchOptions {
  matchCase: boolean;
  wholeWord: boolean;
}

/** `activeIndex` is 0-based, or -1 when there are no matches. */
export interface PdfSearchResult {
  count: number;
  activeIndex: number;
}

/** Find contract. Hosts that own their find UI can register this provider through
 *  `PdfViewer.onSearchProvider` and hide the built-in bar with `findBar={false}`. */
export interface PdfSearchProvider {
  find(query: string, options: PdfSearchOptions): Promise<PdfSearchResult>;
  next(): PdfSearchResult;
  prev(): PdfSearchResult;
  /** Push provider-owned result changes (for example a late re-count). */
  subscribe(listener: (query: string, result: PdfSearchResult) => void): () => void;
  close(): void;
}

/** Optional host callbacks. Everything degrades cleanly when absent. */
export interface PdfViewerAdapters {
  /** Open an http(s)/mailto link found in the document. Absent: external links are inert. The
   *  viewer already restricts URLs to http(s)/mailto before calling this. */
  openExternal?(url: string): void | Promise<void>;
}

export interface Size {
  w: number;
  h: number;
}

/** A node in the PDF outline (bookmarks); mirrors pdf.js `getOutline()`. */
export interface OutlineNode {
  title: string;
  bold: boolean;
  italic: boolean;
  dest: string | unknown[] | null;
  url: string | null;
  items: OutlineNode[];
}

export interface Match {
  page: number;
  /** 0-based occurrence index within that page (text order). */
  occ: number;
}
