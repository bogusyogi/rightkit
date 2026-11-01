/**
 * Deterministic PDF-edit-command mocks for browser QA mode (`VITE_RIGHTKIT_BROWSER_QA=1`,
 * `?qa=1&doc=pdf`). Never active in production builds — every export here is only ever reached
 * through `tauriBridge.ts`'s `isBrowserQaMode()` gate, exactly like `qaOcrLines` already is for
 * OCR (see `browserQa.ts`).
 *
 * Before this module, `requireTauriForPdfEdit` threw `unsupported_outside_tauri` for every
 * PDF-editor command (`inspect_pdf_page`, `compose_pdf_preview`, `prepare_pdf_save`,
 * `commit_pdf_save`, `list_pdf_fonts`), so Save and Edit-text had zero coverage in the automated
 * `qa:functional:browser` harness — every PDF save regression had to be found by hand in the
 * native app. This module gives the harness a real, in-memory PDF "disk" and mirrors the Rust
 * save-protocol rules (`src-tauri/crates/pdf/src/save.rs`'s `select_compose_base`, the stale-sha
 * check, single-use tokens) closely enough that ANNOTATION editing + save (stamp/ink/highlight, via
 * pdf.js's own `AnnotationEditorLayer`/`saveDocument()`) is now exercised end to end in the
 * headless browser harness. Source-TEXT editing (`inspect_pdf_page`'s `islands`) stays native-only:
 * browser QA has no Rust content-stream walker, so `qaInspectPdfPage` always reports zero editable
 * islands — good enough to keep the Edit-text UI reachable/non-crashing, not to exercise a real
 * text-replace round trip.
 */

import type {
  ComposePdfPreviewRequest,
  ComposedPdfPreview,
  PdfCommitResult,
  PdfEditCommand,
  PdfFontCatalogEntry,
  PdfPageGraph,
  PdfTextIsland,
  PreparePdfSaveRequest,
  PreparedPdfSave,
} from "../documents/pdf/pdfEditModel.js";
import { qaDocumentForPath } from "./browserQa.js";

// -------------------------------------------------------------------------------------------
// byte/base64/sha helpers
// -------------------------------------------------------------------------------------------

/** One JS UTF-16 code unit per byte value (0-255) — the "binary string" `btoa` expects, and
 *  exactly the "latin1 string of bytes" `window.__viewrightQa.pdfDiskLatin1` exposes so QA
 *  assertions can `.includes("/Subtype /Stamp")` etc. against raw (uncompressed) PDF syntax. */
function bytesToBinaryString(bytes: Uint8Array): string {
  let out = "";
  const chunk = 0x8000;
  for (let i = 0; i < bytes.length; i += chunk) {
    const slice = bytes.subarray(i, i + chunk);
    for (let j = 0; j < slice.length; j += 1) out += String.fromCharCode(slice[j]);
  }
  return out;
}

function bytesToBase64(bytes: Uint8Array): string {
  return btoa(bytesToBinaryString(bytes));
}

function base64ToBytes(base64: string): Uint8Array {
  const binary = atob(base64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) bytes[i] = binary.charCodeAt(i);
  return bytes;
}

/** `qaDocumentForPath`'s `assetUrl` is always a `data:...;base64,<payload>` URL (the static QA
 *  fixture constant, or a corpus file re-encoded by `bytesToBase64PdfUrl`) — decode straight to
 *  bytes rather than going through a real `fetch(dataUrl)` round trip. */
function dataUrlToBytes(dataUrl: string): Uint8Array {
  const comma = dataUrl.indexOf(",");
  if (comma === -1) throw new Error("qa pdf fixture: not a data: URL");
  return base64ToBytes(dataUrl.slice(comma + 1));
}

/** Matches `document_sha256` (`src-tauri/crates/pdf/src/model.rs`): plain SHA-256 of the whole
 *  byte sequence, lowercase hex. */
export async function sha256Hex(bytes: Uint8Array): Promise<string> {
  // `crypto.subtle.digest` hashes exactly a `BufferSource` view's byteOffset..byteLength range
  // (not its whole backing buffer), and never transfers/detaches it — safe to pass a live reference
  // into `qaDisk`. The `new Uint8Array(bytes)` copy is only here to satisfy `BufferSource`'s
  // plain-`ArrayBuffer` (vs. `ArrayBufferLike`/`SharedArrayBuffer`) typing, not for safety.
  const digest = await crypto.subtle.digest("SHA-256", new Uint8Array(bytes));
  return Array.from(new Uint8Array(digest))
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
}

function startsWithBytes(haystack: Uint8Array, prefix: Uint8Array): boolean {
  if (haystack.length < prefix.length) return false;
  for (let i = 0; i < prefix.length; i += 1) {
    if (haystack[i] !== prefix[i]) return false;
  }
  return true;
}

function affectedPagesFor(commands: readonly PdfEditCommand[]): number[] {
  const pages = new Set<number>();
  for (const command of commands) {
    pages.add(command.kind === "replaceSourceText" ? command.locator.page : command.page);
  }
  return Array.from(pages).sort((a, b) => a - b);
}

// -------------------------------------------------------------------------------------------
// In-memory QA "disk"
// -------------------------------------------------------------------------------------------

const qaDisk = new Map<string, Uint8Array>();

/** Lazily seeds `qaDisk` for `path` from `qaDocumentForPath`'s fixture/corpus data URL on first
 *  touch, then always returns the (possibly since-committed) current bytes from the map. */
async function currentDiskBytes(path: string): Promise<Uint8Array | null> {
  const existing = qaDisk.get(path);
  if (existing) return existing;
  const doc = await qaDocumentForPath(path);
  if (!doc?.assetUrl) return null;
  const bytes = dataUrlToBytes(doc.assetUrl);
  qaDisk.set(path, bytes);
  return bytes;
}

/** Exported for PdfViewer.tsx's post-commit reload only (see its own call site's comment): the
 *  `url` prop it otherwise reloads from is a static `data:` URL captured once at document-open
 *  time — unlike a real Tauri stream token, it can never reflect a later commit on its own, so the
 *  reload needs to ask this in-memory disk for the CURRENT bytes directly. Returns `null` outside
 *  QA mode or for a path never touched through the PDF-edit bridge. */
export async function qaCurrentPdfBytes(path: string): Promise<Uint8Array | null> {
  return currentDiskBytes(path);
}

export async function qaCurrentPdfAssetUrl(path: string): Promise<string | null> {
  const bytes = await qaCurrentPdfBytes(path);
  return bytes ? `data:application/pdf;base64,${bytesToBase64(bytes)}` : null;
}

/** Test-only: clears the in-memory QA disk and save-token registry so each test starts from a
 *  fresh fixture seed, independent of what earlier tests in the same file committed. Mirrors
 *  `resourceCensus.ts`'s `resetResourceCensusForTests` convention. */
export function resetQaPdfStateForTests(): void {
  qaDisk.clear();
  qaSaveTokens.clear();
}

// -------------------------------------------------------------------------------------------
// inspect_pdf_page / list_pdf_fonts
// -------------------------------------------------------------------------------------------

export async function qaInspectPdfPage(path: string, page: number): Promise<PdfPageGraph> {
  const bytes = await currentDiskBytes(path);
  if (!bytes) throw { code: "malformed_content", message: `qa disk has no PDF at ${path}` };
  const documentSha256 = await sha256Hex(bytes);
  return {
    documentSha256,
    page,
    // Matches the QA fixture's own `/MediaBox [0 0 612 792]` (both pages, US Letter) — a corpus
    // PDF's real box would differ, but source-text editing (the only consumer of these bounds)
    // never runs against a corpus fixture in the automated harness.
    mediaBox: [0, 0, 612, 792],
    cropBox: [0, 0, 612, 792],
    rotation: 0,
    islands: page === 1 ? [qaFixtureIsland(documentSha256)] : [],
  };
}

/**
 * One hand-built editable island for the QA fixture's page 1 title run `BT /F1 24 Tf 72 700 Td
 * (Page 1: external link below) Tj ET` (Helvetica 24pt, baseline at y=700). The title run is
 * deliberately chosen over the "Visit example.com" / "Go to page 2" runs below it because those
 * two carry Link annotations whose `<a>` overlays would intercept the click before it reaches the
 * page hit-test; the title has no annotation over it. Geometry uses Helvetica's nominal metrics
 * (ascender 0.718em, descender 0.207em, ~0.5em average advance) so the inline editor's on-screen
 * box lines up with the painted glyphs closely enough to dogfood click-to-edit UX (font face,
 * size at zoom, glyph masking). The locator carries no spans: browser QA has no content-stream
 * walker or composer, so a committed edit is staged but never composed into the bytes — the round
 * trip that IS covered natively.
 */
function qaFixtureIsland(documentSha256: string): PdfTextIsland {
  const x0 = 72;
  const baseline = 700;
  const sizePt = 24;
  const text = "Page 1: external link below";
  const width = text.length * sizePt * 0.5;
  const top = baseline + sizePt * 0.718;
  const bottom = baseline - sizePt * 0.207;
  return {
    id: "qa-island-visit-example",
    text,
    quads: [[x0, bottom, x0 + width, bottom, x0 + width, top, x0, top]],
    baseline: [x0, baseline, x0 + width, baseline],
    appearance: {
      fontId: "base14:helvetica",
      family: "Helvetica",
      sizePt,
      color: { r: 0, g: 0, b: 0, alpha: 1 },
    },
    locator: { documentSha256, page: 1, spans: [], islandDigest: "qa-fixture" },
    editable: true,
    rejection: null,
  };
}

export async function qaListPdfFonts(): Promise<PdfFontCatalogEntry[]> {
  return [];
}

// -------------------------------------------------------------------------------------------
// compose_pdf_preview (§9.4 memory-only preview)
// -------------------------------------------------------------------------------------------

export async function qaComposePdfPreview(
  request: ComposePdfPreviewRequest,
): Promise<ComposedPdfPreview> {
  const bytes = await currentDiskBytes(request.sourcePath);
  if (!bytes) {
    throw { code: "malformed_content", message: `qa disk has no PDF at ${request.sourcePath}` };
  }
  const actualSha = await sha256Hex(bytes);
  if (request.expectedSourceSha256 !== actualSha) {
    throw { code: "stale_source", message: "PDF changed since edit started" };
  }
  // Source-text commands are never applied here (native-only, per module doc) — an empty command
  // list is exactly what the real composer also does with no-op input (see `compose_candidate`'s
  // own `if commands.is_empty() { return source.to_vec() }` fast path), so this only diverges from
  // real behavior if a caller stages a non-empty `sourceCommands` list, which QA scenarios don't.
  return { candidateBytes: bytes, affectedPages: affectedPagesFor(request.sourceCommands) };
}

// -------------------------------------------------------------------------------------------
// prepare_pdf_save / commit_pdf_save (§10.1/§10.3 two-phase save)
// -------------------------------------------------------------------------------------------

/** Mirrors `src-tauri/crates/pdf/src/save.rs`'s `select_compose_base` exactly: a `<=1`-byte
 *  annotation payload is the frontend's "no annotation edits" sentinel (base = source); a real
 *  pdf.js incremental update is the original bytes verbatim plus appended annotation objects/xref
 *  (base = those bytes); anything else is rejected rather than silently guessed at. */
function selectComposeBase(source: Uint8Array, annotationPdf: Uint8Array): Uint8Array {
  if (annotationPdf.length <= 1) return source;
  if (annotationPdf.length > source.length && startsWithBytes(annotationPdf, source)) {
    return annotationPdf;
  }
  throw {
    code: "annotation_serialization_failed",
    message: "annotations could not be serialized",
  };
}

interface QaSaveToken {
  sourcePath: string;
  sourceSha256: string;
  candidateBytes: Uint8Array;
}

const qaSaveTokens = new Map<string, QaSaveToken>();

export async function qaPreparePdfSave(request: PreparePdfSaveRequest): Promise<PreparedPdfSave> {
  // Matches `pdf_adapter.rs`'s own explicit empty check, which runs before `select_compose_base`
  // even sees the bytes (its own `<=1` branch would otherwise also accept a genuinely-empty
  // payload as "no annotation edits", which is not what an empty/failed serialization means).
  if (request.annotationPdfBytes.length === 0) {
    throw {
      code: "annotation_serialization_failed",
      message: "annotations could not be serialized",
    };
  }
  const source = await currentDiskBytes(request.sourcePath);
  if (!source) {
    throw { code: "malformed_content", message: `qa disk has no PDF at ${request.sourcePath}` };
  }
  const actualSha = await sha256Hex(source);
  if (request.expectedSourceSha256 !== actualSha) {
    throw { code: "stale_source", message: "PDF changed since edit started" };
  }
  const candidateBytes = selectComposeBase(source, request.annotationPdfBytes);
  const candidateSha256 = await sha256Hex(candidateBytes);
  const token = crypto.randomUUID();
  qaSaveTokens.set(token, {
    sourcePath: request.sourcePath,
    sourceSha256: actualSha,
    candidateBytes,
  });
  return {
    token,
    candidateSha256,
    candidateBytes,
    affectedPages: affectedPagesFor(request.sourceCommands),
    expectedText: [],
  };
}

export async function qaCommitPdfSave(
  token: string,
  targetPath?: string,
): Promise<PdfCommitResult> {
  // Single-use regardless of outcome — mirrors `PdfSaveTokenStore::consume`'s "removes the token
  // record on the very first commit_pdf_save call for it, success or failure."
  const record = qaSaveTokens.get(token);
  qaSaveTokens.delete(token);
  if (!record) {
    throw { code: "write_conflict", message: "save token invalid or already used" };
  }
  const target = targetPath ?? record.sourcePath;
  if (target === record.sourcePath) {
    // Same-path Save rechecks source sha immediately before the "atomic" write, same as
    // `resolve_commit`'s same-path recheck.
    const current = await currentDiskBytes(record.sourcePath);
    const currentSha = current ? await sha256Hex(current) : "";
    if (currentSha !== record.sourceSha256) {
      throw { code: "write_conflict", message: "target changed before save" };
    }
  }
  qaDisk.set(target, record.candidateBytes);
  const committedSha256 = await sha256Hex(record.candidateBytes);
  return {
    committedPath: target,
    committedSha256,
    affectedPages: [],
    revision: { documentSha256: committedSha256, pageCount: 0, pages: [] },
  };
}

// -------------------------------------------------------------------------------------------
// window.__viewrightQa — assertion surface for scripts/qa-browser-functional.mjs
// -------------------------------------------------------------------------------------------

export interface ViewrightQaGlobal {
  pdfDiskBase64(path: string): string | null;
  pdfDiskLatin1(path: string): string | null;
  /** The PRISTINE fixture/corpus bytes for `path` (always freshly re-derived from
   *  `qaDocumentForPath`, never from the mutable `qaDisk` — so this stays correct even after a
   *  commit has already overwritten `qaDisk`'s entry). For asserting a save produced a real
   *  incremental update ("committed bytes start with the ORIGINAL bytes"), since by the time a
   *  script can read `pdfDiskLatin1` after a "Saved" toast, `qaDisk` already holds the new,
   *  post-commit bytes. */
  originalPdfDiskLatin1(path: string): Promise<string | null>;
}

declare global {
  interface Window {
    __viewrightQa?: ViewrightQaGlobal;
  }
}

let registered = false;

/** Registers `window.__viewrightQa` once. Callers (`tauriBridge.ts`) must gate this behind their
 *  own `isBrowserQaMode()` check — kept unconditional here (idempotent via `registered`) rather
 *  than duplicating that check locally, since importing `isBrowserQaMode` from `tauriBridge.ts`
 *  would cycle back into this module's own `./browserQa` import. */
export function registerViewrightQaGlobal(): void {
  if (registered || typeof window === "undefined") return;
  registered = true;
  window.__viewrightQa = {
    pdfDiskBase64(path: string): string | null {
      const bytes = qaDisk.get(path);
      return bytes ? bytesToBase64(bytes) : null;
    },
    pdfDiskLatin1(path: string): string | null {
      const bytes = qaDisk.get(path);
      return bytes ? bytesToBinaryString(bytes) : null;
    },
    async originalPdfDiskLatin1(path: string): Promise<string | null> {
      const doc = await qaDocumentForPath(path);
      if (!doc?.assetUrl) return null;
      return bytesToBinaryString(dataUrlToBytes(doc.assetUrl));
    },
  };
}
