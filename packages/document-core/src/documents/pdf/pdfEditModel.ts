/**
 * Canonical frontend model for sparse PDF edits.
 *
 * This file is the frozen cross-lane seam described in `docs/PDF-EDITOR-DISPATCH.md`
 * ("Frozen cross-lane seams"): it owns every shared frontend type — the Rust wire-model mirrors
 * (`docs/PDF-EDITOR-NOW.md` §5.1), the session/command types (§6), `PdfSavePlan` (§10.3), the typed
 * `PdfEditError` union (§11), and the *injected-bridge* function signatures other lanes accept by
 * injection ahead of the real `tauriBridge.ts` implementations landing at Dispatch D2.
 *
 * PDF pages are fixed-layout scenes, not word-processing documents. Existing content therefore
 * enters the editor as small, source-backed "islands" reported by the Rust inspector
 * (`inspect_pdf_page`, §5.2). Every edit command retains a stable source locator (§5.1) so
 * persistence (§9) can patch only changed content instead of clearing and reconstructing a page.
 *
 * This is a contract-definition file: types plus small pure helpers. Session lifecycle, draft
 * commit/cancel orchestration, and the real bridge live elsewhere (`usePdfEditSession.ts`,
 * `pdfSave.ts`, `tauriBridge.ts` — see `docs/PDF-EDITOR-NOW.md` §12).
 */

// ---------------------------------------------------------------------------------------------
// Geometry primitives (§3.2 / §6.1 / §8)
// ---------------------------------------------------------------------------------------------

/** Axis-aligned rectangle in PDF user-space units (origin/orientation follow the page's own
 *  coordinate system, pre-viewport-transform). Used for available-bounds checks (§8's
 *  `available.contains(run.bounds)`) and for the draft's page-slot placement (§6.1's
 *  `pdfRectToViewport(session.draft.rect, viewport)`). */
export interface PdfRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

/** A single PDF user-space point, e.g. the clicked anchor for Add Text (§6 `PdfAddText.anchor`). */
export interface PdfPoint {
  x: number;
  y: number;
}

/** Four-number tuple shape shared by `mediaBox`, `cropBox` (§5.1), and an island's `baseline`
 *  segment — all serialize from Rust `[f64; 4]`. */
export type PdfQuad4 = [number, number, number, number];

/** Eight-number tuple shape for one glyph-run quadrilateral (`PdfTextIsland.quads`, §5.1),
 *  serialized from Rust `[f64; 8]` as four (x, y) corner pairs. */
export type PdfQuad8 = [number, number, number, number, number, number, number, number];

// ---------------------------------------------------------------------------------------------
// Authoritative PDF source graph — Rust wire-model mirrors (§5.1)
// ---------------------------------------------------------------------------------------------
//
// These interfaces mirror the `#[serde(rename_all = "camelCase")]` Rust structs verbatim in field
// set and camelCase naming. Byte payloads never cross IPC as JSON arrays (§4): fields shown here
// as `Uint8Array` travel as the raw invoke body / `tauri::ipc::Response`, with the rest of the
// struct as JSON metadata beside them (§4, §9.4, §10.1).

export interface PdfDocumentRevision {
  documentSha256: string;
  pageCount: number;
  pages: PdfPageGraph[];
}

export interface PdfPageGraph {
  documentSha256: string;
  page: number;
  mediaBox: PdfQuad4;
  cropBox: PdfQuad4;
  rotation: number;
  islands: PdfTextIsland[];
}

/** One editable (or explicitly-rejected) same-line visual text run (§5.3). Named identically to
 *  the Rust wire struct it mirrors — this supersedes the prior DOM/occurrence-based
 *  `PdfTextIsland` from the superseded attempt; there is no backward-compat alias. */
export interface PdfTextIsland {
  id: string;
  text: string;
  quads: PdfQuad8[];
  baseline: PdfQuad4;
  appearance: PdfTextAppearance;
  locator: PdfSourceLocator;
  editable: boolean;
  rejection: PdfEditError | null;
}

/** Stable identity for a piece of source content, independent of any DOM node or viewport state
 *  (§4: "pdf.js DOM nodes never identify persistent source content"). */
export interface PdfSourceLocator {
  documentSha256: string;
  page: number;
  spans: PdfSourceSpan[];
  islandDigest: string;
}

/** One exact source-byte range an edit command may patch (§9.1/§9.2). `streamRef` mirrors Rust's
 *  `(u32, u16)` object/generation tuple. */
export interface PdfSourceSpan {
  streamRef: [number, number];
  contentsIndex: number;
  operatorIndex: number;
  operandIndex: number;
  glyphStart: number;
  glyphEnd: number;
  expectedText: string;
  expectedDigest: string;
}

// ---------------------------------------------------------------------------------------------
// Font catalog (§7.2) — needed by the injected `ListPdfFonts` bridge signature below.
// ---------------------------------------------------------------------------------------------

/** Not specified as a concrete Rust type in §7.2 — inferred as the two catalog origins the section
 *  narrative distinguishes ("bundled fonts such as Source Serif 4 plus eligible system fonts"). */
export type PdfFontSource = "bundled" | "system";

export interface PdfFontCatalogEntry {
  id: string;
  family: string;
  postscriptName: string | null;
  styles: string[];
  embeddable: boolean;
  subsettable: boolean;
  source: PdfFontSource;
}

// ---------------------------------------------------------------------------------------------
// Frontend session & commands (§6)
// ---------------------------------------------------------------------------------------------

export type PdfColor = { r: number; g: number; b: number; alpha: number };

export type PdfTextAppearance = {
  fontId: string;
  family: string;
  sizePt: number;
  color: PdfColor;
};

export type PdfSourceEdit = {
  kind: "replaceSourceText";
  id: string;
  locator: PdfSourceLocator;
  before: { text: string; appearance: PdfTextAppearance };
  after: { text: string; appearance: PdfTextAppearance };
  availableBounds: PdfRect;
};

export type PdfAddText = {
  kind: "addText";
  id: string;
  page: number;
  anchor: PdfPoint;
  text: string;
  appearance: PdfTextAppearance;
};

export type PdfOcrCorrection = {
  kind: "correctOcrText";
  id: string;
  page: number;
  lineId: string;
  rect: PdfRect;
  text: string;
  appearance: PdfTextAppearance;
  coverColor: PdfColor;
  backgroundProof: "nearUniform";
};

export type PdfEditCommand = PdfSourceEdit | PdfAddText | PdfOcrCorrection;

/** Narrows `command` to `PdfSourceEdit`. Trivial given the `kind` discriminant, but kept as a named
 *  helper so call sites read intent rather than re-deriving the literal each time. */
export function isPdfSourceEdit(command: PdfEditCommand): command is PdfSourceEdit {
  return command.kind === "replaceSourceText";
}

/** Narrows `command` to `PdfAddText`. */
export function isPdfAddText(command: PdfEditCommand): command is PdfAddText {
  return command.kind === "addText";
}

export function isPdfOcrCorrection(command: PdfEditCommand): command is PdfOcrCorrection {
  return command.kind === "correctOcrText";
}

export type PdfEditSession = {
  mode: "view" | "edit";
  tool: "selectText" | "addText" | "highlight" | "ink" | "image";
  selection: PdfSelection | null;
  draft: PdfTextDraft | null;
  sourceCommands: readonly PdfEditCommand[];
  sourceDigest: string;
  savedSourceDigest: string;
  annotationDirty: boolean;
};

/** The currently selected existing text island (§3.2). Not specified as a concrete shape in the
 *  contract text beyond "selection"; inferred as enough identity + geometry to drive highlight
 *  rendering and to seed a draft when the user starts editing it. */
export interface PdfSelection {
  page: number;
  islandId: string;
  locator: PdfSourceLocator;
  rect: PdfRect;
}

/** The in-progress edit surfaced by the page-slot editor (§6.1). Covers both existing-text edits
 *  (`kind: "sourceEdit"`, seeded from a `PdfSelection`/island) and Add Text (`kind: "addText"`,
 *  seeded from a clicked `PdfPoint`). Not given as a concrete shape in the contract text; inferred
 *  from §6.1's `session.draft.rect` / `session.draft.page` usage and §3.3's overflow behavior
 *  ("editor shows overflow on page & Save is disabled for that command"). Commit-time logic builds
 *  a `PdfSourceEdit` or `PdfAddText` from these fields. */
export type PdfTextDraft = {
  kind: "sourceEdit" | "addText" | "ocrEdit";
  id: string;
  page: number;
  rect: PdfRect;
  text: string;
  appearance: PdfTextAppearance;
  /** `sourceEdit` only: locator identifying the island being replaced. */
  locator: PdfSourceLocator | null;
  /** `sourceEdit` only: text/appearance snapshot at draft start, used to build the command's
   *  `before` half and to detect a no-op commit. */
  originalText: string | null;
  originalAppearance: PdfTextAppearance | null;
  /** `sourceEdit` only: verified bounds the replacement run must stay inside (§8). */
  availableBounds: PdfRect | null;
  /** `addText` only: the clicked PDF-space anchor point. */
  anchor: PdfPoint | null;
  /** `ocrEdit` only: stable recognized-line identity & locally sampled cover color. */
  lineId?: string;
  coverColor?: PdfColor;
  backgroundProof?: "nearUniform";
  /** True once the current draft geometry has been measured to exceed `availableBounds` (§3.3).
   *  UI disables Save for this command while true; it never auto-shrinks/clips/wraps. */
  overflow: boolean;
};

/** Narrows `draft` to a `sourceEdit` draft (editing an existing island). */
export function isSourceEditDraft(
  draft: PdfTextDraft,
): draft is PdfTextDraft & { kind: "sourceEdit"; locator: PdfSourceLocator } {
  return draft.kind === "sourceEdit";
}

/** Narrows `draft` to an `addText` draft (new text at a clicked point). */
export function isAddTextDraft(
  draft: PdfTextDraft,
): draft is PdfTextDraft & { kind: "addText"; anchor: PdfPoint } {
  return draft.kind === "addText";
}

// ---------------------------------------------------------------------------------------------
// Digest / dirty-state helpers (§6)
// ---------------------------------------------------------------------------------------------
//
// Pure, deterministic, order-sensitive. `computeSourceDigest` is the canonical way session logic
// (owned by usePdfEditSession.ts) derives `PdfEditSession.sourceDigest` from the current ordered
// `sourceCommands`; order sensitivity matters because undo/redo changes the array's tail and must
// therefore change the digest even when only the newest command differs.

function stableStringify(value: unknown): string {
  if (value === null || typeof value !== "object") {
    return JSON.stringify(value);
  }
  if (Array.isArray(value)) {
    return `[${value.map(stableStringify).join(",")}]`;
  }
  const record = value as Record<string, unknown>;
  const keys = Object.keys(record).sort();
  const entries = keys.map((key) => `${JSON.stringify(key)}:${stableStringify(record[key])}`);
  return `{${entries.join(",")}}`;
}

/** FNV-1a over a UTF-16-code-unit string. Not cryptographic; only used to derive a short, stable,
 *  collision-resistant-enough-for-UI digest of the in-memory command list. Structurally identical
 *  to the hashing approach the superseded file used for island ids, generalized to hash any
 *  stable-stringified value. */
function fnv1a(text: string): string {
  let hash = 0x811c9dc5;
  for (let i = 0; i < text.length; i += 1) {
    hash ^= text.charCodeAt(i);
    hash = Math.imul(hash, 0x01000193);
  }
  return (hash >>> 0).toString(16).padStart(8, "0");
}

/** Deterministic digest of an ordered command list. Two lists produce the same digest iff they
 *  have the same commands in the same order (object key order inside each command does not
 *  matter — `stableStringify` sorts keys). Empty history has a fixed, distinguishable digest. */
export function computeSourceDigest(commands: readonly PdfEditCommand[]): string {
  if (commands.length === 0) return "empty";
  return fnv1a(stableStringify(commands));
}

/** Dirty-state derivation per §6: source edits are dirty when the live digest has diverged from
 *  the last-saved checkpoint, OR annotations changed. This is what "survives undo across save
 *  checkpoint" — a boolean "was written" flag would not, because undoing back to a *different*
 *  command list than what was saved is still dirty even though something was undone. */
export function isSessionDirty(
  session: Pick<PdfEditSession, "sourceDigest" | "savedSourceDigest" | "annotationDirty">,
): boolean {
  return session.sourceDigest !== session.savedSourceDigest || session.annotationDirty;
}

// ---------------------------------------------------------------------------------------------
// Typed failures (§11)
// ---------------------------------------------------------------------------------------------
//
// `docs/PDF-EDITOR-NOW.md` §11 declares `PdfEditError` as a `thiserror` enum with
// `#[derive(Debug, Serialize, thiserror::Error)]` but does not show its serde tagging attribute
// (unlike the wire structs in §5.1, which all carry an explicit `#[serde(rename_all =
// "camelCase")]`). This codebase's existing typed-error convention — `HostError` in
// `src-tauri/src/error.rs`, mirrored by `ClassifiedError` in `src/state/tauriBridge.ts` — tags
// with `#[serde(tag = "code", content = "message", rename_all = "snake_case")]`, producing a JSON
// envelope of `{ code: "<snake_case_variant>", message: "<context>" }` on the wire. This file
// mirrors that same convention for `PdfEditError` (snake_case `code` literals matching the §11
// variant names 1:1, plus a `message` string for contextual detail) rather than inventing a
// differently-shaped error surface for just this feature. `pdf_model.rs` (Dispatch A1) owns the
// actual Rust attribute; if it tags differently, this union's `code` values still line up 1:1 with
// its variant names and only the envelope wrapping would need reconciling in tauriBridge.ts (D2).

export type PdfEditErrorCode =
  | "stale_source"
  | "source_digest_mismatch"
  | "no_unique_match"
  | "unsupported_transform"
  | "unsupported_encoding"
  | "missing_glyph"
  | "font_embedding_denied"
  | "text_overflow"
  | "malformed_content"
  | "encrypted_document"
  | "annotation_serialization_failed"
  | "verification_failed"
  | "write_conflict";

export const PDF_EDIT_ERROR_CODES: readonly PdfEditErrorCode[] = [
  "stale_source",
  "source_digest_mismatch",
  "no_unique_match",
  "unsupported_transform",
  "unsupported_encoding",
  "missing_glyph",
  "font_embedding_denied",
  "text_overflow",
  "malformed_content",
  "encrypted_document",
  "annotation_serialization_failed",
  "verification_failed",
  "write_conflict",
];

/** Typed IPC error for every fallible PDF-edit command in this contract (§5.2, §7.2, §9.4, §10.1,
 *  §10.3). `message` carries the contextual, non-document-text detail §11 requires ("Logs include
 *  code/page/strategy but no document text"). */
export interface PdfEditError {
  code: PdfEditErrorCode;
  message: string;
}

/** Runtime guard for `code`, primarily useful when classifying an unknown IPC rejection (mirrors
 *  the narrowing `parseHostError` does for `ClassifiedError` in tauriBridge.ts). */
export function isPdfEditErrorCode(code: unknown): code is PdfEditErrorCode {
  return typeof code === "string" && (PDF_EDIT_ERROR_CODES as readonly string[]).includes(code);
}

// ---------------------------------------------------------------------------------------------
// Save protocol (§10)
// ---------------------------------------------------------------------------------------------

/** Not given as a concrete Rust type in §10.1/§10.2 despite being referenced by
 *  `PreparedPdfSave.expectedText` and consumed by `verifyPdfCandidate` to check extracted text
 *  against the composed candidate. Inferred minimal shape: enough to locate which island on which
 *  page must contain which text after the isolated re-render (§10.2). */
export interface PdfExpectedTextCheck {
  page: number;
  islandId: string;
  expectedText: string;
}

export interface PreparePdfSaveRequest {
  sourcePath: string;
  expectedSourceSha256: string;
  /** Raw invoke body per §4 — never a JSON number array. */
  annotationPdfBytes: Uint8Array;
  sourceCommands: readonly PdfEditCommand[];
}

export interface PreparedPdfSave {
  token: string;
  candidateSha256: string;
  /** Raw `tauri::ipc::Response` body per §4. */
  candidateBytes: Uint8Array;
  affectedPages: readonly number[];
  expectedText: readonly PdfExpectedTextCheck[];
}

/** Not given as a concrete Rust type anywhere in the contract — §10.3 explicitly requires "Save
 *  protocol... return a commit plan, not bare bytes" and states a successful commit "reloads
 *  active pdf.js document from committed bytes, refreshes graph/revision, clears annotation
 *  storage dirty state, & checkpoints sourceDigest." Inferred shape carries what the frontend needs
 *  to do all of that without a second round trip: the committed path/sha, which pages changed, and
 *  the fresh `PdfDocumentRevision` to refresh the graph from. */
export interface PdfCommitResult {
  committedPath: string;
  committedSha256: string;
  affectedPages: readonly number[];
  revision: PdfDocumentRevision;
  /** Disk commit succeeded while newer in-memory edits remain unsaved in active document. */
  preservedUnsavedEdits?: boolean;
}

/** Exact shape from §10.3. Generic document saver (`useDocumentSaver.ts`, B8) updates
 *  recents/fingerprints/checkpoints only after `commit` succeeds and `didCommit` has run. */
export type PdfSavePlan = {
  commit(targetPath?: string): Promise<PdfCommitResult>;
  didCommit(result: PdfCommitResult): Promise<void>;
};

// ---------------------------------------------------------------------------------------------
// Injected-bridge function types
// ---------------------------------------------------------------------------------------------
//
// Type-only signatures for the Tauri commands in §5.2/§7.2/§9.4/§10.1/§10.3. No implementation
// lives here — the real bridge lands in `src/state/tauriBridge.ts` at Dispatch D2. Lanes B3
// (`usePdfEditSession.ts`), B7 (`pdfSave.ts`), and D2 import these exact types so session logic and
// save orchestration can build and test against injected fakes before the bridge exists
// (`docs/PDF-EDITOR-DISPATCH.md`, "Frozen cross-lane seams").

/** §5.2 `inspect_pdf_page`. */
export type InspectPdfPage = (path: string, page: number) => Promise<PdfPageGraph>;

/** §7.2 `list_pdf_fonts`. */
export type ListPdfFonts = () => Promise<PdfFontCatalogEntry[]>;

export interface ComposePdfPreviewRequest {
  sourcePath: string;
  expectedSourceSha256: string;
  sourceCommands: readonly PdfEditCommand[];
}

export interface ComposedPdfPreview {
  /** Raw `tauri::ipc::Response` body per §4 — memory-only candidate bytes, never persisted. */
  candidateBytes: Uint8Array;
  affectedPages: readonly number[];
}

/** §9.4 `compose_pdf_preview`. Memory-only: no token, no retained candidate, no writes. */
export type ComposePdfPreview = (request: ComposePdfPreviewRequest) => Promise<ComposedPdfPreview>;

/** §10.1 `prepare_pdf_save`. */
export type PreparePdfSave = (request: PreparePdfSaveRequest) => Promise<PreparedPdfSave>;

/** §10.3 `commit_pdf_save`. */
export type CommitPdfSave = (token: string, targetPath?: string) => Promise<PdfCommitResult>;
