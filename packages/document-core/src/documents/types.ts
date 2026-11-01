import { formatEntryForExtension } from "./documentFormats.js";

export type DocFormat =
  | "md"
  | "mdx"
  | "txt"
  | "pdf"
  | "html"
  | "docx"
  | "image"
  | "web"
  | "unknown";

// Shared across formats. md/mdx use preview+source; html uses `interactive` (scripts + in-page nav,
// default) + `preview` (Safe) — its source is an in-place drawer, not a mode. Legacy persisted
// `visual` degrades to interactive via normalizeViewMode(), so it never reaches this type.
// md/html also override the restored mode on open; other formats ignore view entirely.
export type ViewMode = "preview" | "source" | "interactive";

/** Per-document Read/Edit (PDF: "Markup") mode from the Wave 2 shell. Ephemeral — defaults to
 *  "read" on open, never persisted (unlike ViewMode, which saveViewState restores per tab). */
export type ReadEditMode = "read" | "edit";

/** Coerce a persisted/raw view string to a valid ViewMode (legacy `split`→source, `edit`→preview),
 *  returning undefined for anything unrecognized so the open path falls back to the format default. */
export function normalizeViewMode(raw: string | null | undefined): ViewMode | undefined {
  switch (raw) {
    case "preview":
    case "source":
    case "interactive":
      return raw;
    case "visual":
      return "interactive";
    case "split":
      return "source";
    case "edit":
      return "preview";
    default:
      return undefined;
  }
}

// Virtual browser tab for explicit Open-URL previews. Local .html is owned by the normal html document
// tab (HtmlDocument), which renders the interactive preview as a view mode.
export type WebDocument = { kind: "url"; url: string };

/** Per-format restored view state. scroll = text/markdown/html outer-container scrollTop;
 *  page/scale/fit/rotation/pdfPanel = PDF position + zoom + rotation + side panel. Persisted as
 *  JSON in the DB per recent file. */
export interface ViewMeta {
  scroll?: number;
  page?: number;
  scale?: number;
  fit?: "width" | "page" | null;
  /** PDF page rotation in degrees, normalized to 0 | 90 | 180 | 270. */
  rotation?: number;
  /** PDF side panel; null explicitly records a hidden panel. */
  pdfPanel?: "outline" | "thumbs" | null;
}

export interface LoadedDocument {
  /** Stable identity for the tab slot. The backing `path` can change after Save As or image paging,
   *  but the pane should stay mounted unless the user really opened a different tab. */
  tabKey?: string;
  path: string;
  displayName: string;
  format: DocFormat;
  /** UTF-8 text for md/mdx/txt. Empty for brokered PDF/DOCX/image documents. */
  content: string;
  /** Revocable custom-protocol URL for binary viewers. PDF uses range requests instead of
   *  receiving base64 over IPC; DOCX fetches the buffer from the same tokenized URL. */
  assetUrl?: string;
  lastView?: ViewMode;
  /** Restored per-format view state (scroll / pdf page+zoom+fit). */
  viewMeta?: ViewMeta;
  /** True only when even the detected encoding couldn't decode the bytes cleanly (garbled input). */
  lossy?: boolean;
  /** Encoding the content was decoded from (e.g. "UTF-8", "windows-1252"). Preserved on save. */
  encoding?: string;
  /** SHA-256 of source bytes captured at read/save time. In-place text saves send it to the backend
   * so a same-size, same-mtime replacement cannot be overwritten. */
  sourceSha256?: string;
  /** Cheap on-disk identity (modified-time + size) captured at read/save time. Compared on tab
   *  activation to detect a background-tab file that changed on disk while it wasn't being watched
   *  (the watcher is active-file-only). Undefined for untitled/web/binary-without-metadata. */
  fingerprint?: FileFingerprint;
  /** Virtual browser-like previews. These are session-only tabs, not brokered file documents. */
  web?: WebDocument;
}

/** Modified-time (ms since epoch) + byte size — a cheap "did this file change on disk?" check that
 *  doesn't read or hash the whole file. Either field being unavailable (0) means "can't tell". */
export interface FileFingerprint {
  mtimeMs: number;
  size: number;
}

export function formatFromPath(path: string): DocFormat {
  // Match Rust's Path::extension semantics: a leading dot is part of a dotfile name,
  // not an extension, and dots in parent directories are irrelevant. Extensionless
  // files are text documents on the native read path, so keep the UI classifier aligned.
  const name = baseName(path);
  const dot = name.lastIndexOf(".");
  const hasNoExtension = dot === -1 || dot === 0;
  const ext = dot > 0 ? name.slice(dot + 1).toLowerCase() : "";
  if (hasNoExtension) return "txt";
  return formatEntryForExtension(ext)?.format ?? "unknown";
}

export function baseName(path: string): string {
  return path.split(/[\\/]/).pop() ?? path;
}

/** New, never-saved documents use an `untitled:N` pseudo-path. */
export function isUntitled(path: string): boolean {
  return path.startsWith("untitled:");
}

export function docTitle(path: string): string {
  return isUntitled(path) ? `Untitled ${path.split(":")[1]}` : baseName(path);
}

/** Display name with the recognized document extension stripped (the icon shows the type). */
export function displayTitle(path: string): string {
  const name = baseName(path);
  const dot = name.lastIndexOf(".");
  if (dot <= 0) return name;
  return formatEntryForExtension(name.slice(dot + 1)) ? name.slice(0, dot) : name;
}

/** Name an unsaved doc by its first non-empty line (like Notes / TextEdit), whitespace-collapsed. */
// Called per edit notify by live titles, sidebar rows and the palette, so it must not touch the whole
// buffer: sticky scans read only leading whitespace and at most UNTITLED_TITLE_MAX + 1 chars of the
// first line, so a multi-MB paste costs the same as a short note.
const UNTITLED_TITLE_MAX = 60;
const TITLE_WS = /\s*/y;
const TITLE_LINE = /[^\r\n]{1,61}/y;

export function untitledTitle(text: string): string {
  TITLE_WS.lastIndex = 0;
  TITLE_WS.exec(text);
  TITLE_LINE.lastIndex = TITLE_WS.lastIndex;
  const line = TITLE_LINE.exec(text);
  if (!line) return "Untitled";
  const title = line[0].replace(/\s+/g, " ").trim();
  if (!title) return "Untitled";
  return title.length > UNTITLED_TITLE_MAX
    ? `${title.slice(0, UNTITLED_TITLE_MAX).trimEnd()}…`
    : title;
}

// Windows reserved device names — illegal as a base file name (with or without an extension).
const WINDOWS_RESERVED_NAMES = new Set([
  "CON",
  "PRN",
  "AUX",
  "NUL",
  "COM1",
  "COM2",
  "COM3",
  "COM4",
  "COM5",
  "COM6",
  "COM7",
  "COM8",
  "COM9",
  "LPT1",
  "LPT2",
  "LPT3",
  "LPT4",
  "LPT5",
  "LPT6",
  "LPT7",
  "LPT8",
  "LPT9",
]);

/**
 * Turn an arbitrary title (e.g. an untitled doc's first-line name) into a filesystem-safe filename STEM
 * (no extension), or `null` if nothing usable remains. Used to seed the Save-As dialog; the caller falls
 * back to "untitled" on null. Strips path separators + Windows-illegal characters + control chars,
 * collapses whitespace, trims leading/trailing dots/spaces (which Windows mangles), caps length at 50,
 * and rejects reserved device names (CON, NUL, COM1…). Conservative cross-platform: applies the Windows
 * rules everywhere so the same buffer suggests the same name on every OS.
 */
export function safeFileStem(title: string): string | null {
  let s = "";
  for (const ch of title) {
    const code = ch.codePointAt(0) ?? 0;
    if (code < 0x20) continue; // control characters
    if ('\\/:*?"<>|'.includes(ch)) continue; // path separators + Windows-illegal characters
    s += ch;
  }
  s = s.replace(/\s+/g, " ").replace(/^[.\s]+|[.\s]+$/g, "");
  if (s.length > 50) s = s.slice(0, 50).replace(/[.\s]+$/g, "");
  if (!s) return null;
  if (WINDOWS_RESERVED_NAMES.has(s.toUpperCase())) return null;
  return s;
}

const BINARY_EXTS = new Set([
  "doc",
  "xls",
  "xlsx",
  "ppt",
  "pptx",
  "odt",
  "ods",
  "rtf",
  "pages",
  "zip",
  "rar",
  "7z",
  "tar",
  "gz",
  "exe",
  "dll",
  "bin",
  "tif",
  "tiff",
  "mp3",
  "wav",
  "flac",
  "mp4",
  "mov",
  "avi",
  "mkv",
  "webm",
]);

/** True for known binary formats the host app can't open as text/pdf/html yet. */
export function isUnsupportedBinary(path: string): boolean {
  const ext = path.split(".").pop()?.toLowerCase() ?? "";
  return BINARY_EXTS.has(ext);
}

/** Find options shared across formats. Regex is intentionally not exposed (yet). */
export interface SearchOptions {
  matchCase: boolean;
  wholeWord: boolean;
  /** Treat the query as a regular expression (providers with `supportsRegex`). */
  regex?: boolean;
}

/** Result of any find/navigate/replace call. The PROVIDER owns the active-match index — the
 *  FindBar must read it from here, never track its own (which drifts). `activeIndex` is 0-based,
 *  or -1 when there are no matches. */
export interface SearchResult {
  count: number;
  activeIndex: number;
}

/**
 * One Find/Replace contract for every format. The provider owns match state; find/next/prev/
 * replace/replaceAll all return the updated SearchResult (sync or async — the FindBar awaits).
 * Capability flags let the FindBar enable only the toggles a given format actually supports.
 */
export interface SearchProvider {
  kind: "pdf" | "source" | "markdown-preview" | "html-preview" | "docx";
  canReplace: boolean;
  supportsMatchCase: boolean;
  supportsWholeWord: boolean;
  supportsRegex?: boolean;
  find(query: string, options: SearchOptions): SearchResult | Promise<SearchResult>;
  next(): SearchResult | Promise<SearchResult>;
  prev(): SearchResult | Promise<SearchResult>;
  replace?(replacement: string): SearchResult | Promise<SearchResult>;
  replaceAll?(replacement: string): SearchResult | Promise<SearchResult>;
  /** Push provider-owned result changes caused outside FindBar controls, such as document undo. */
  subscribe?(listener: (query: string, result: SearchResult) => void): () => void;
  seek?(matchIndex: number): void;
  close(): void;
}
