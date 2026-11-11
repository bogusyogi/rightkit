// PDF sparse-edit bridge: the PDF editor's native commands. Moved from
// `@rightkit/document-core/state/tauriBridge` with the PDF editor (document-core 0.2.0), so the
// shared bridge no longer carries PDF-editor types.
import { inTauri, nativeIpc, byteArrayToBase64 } from "@rightkit/document-core/state/tauriBridge";

const invoke = nativeIpc.invoke;

//
// Typed bridge over the raw-byte PDF-edit IPC channels. Implements the `InspectPdfPage` /
// `ListPdfFonts` / `ComposePdfPreview` / `PreparePdfSave` / `CommitPdfSave` signatures frozen in
// `src/documents/pdf/pdfEditModel.ts` — those types are the contract; the functions below must
// match them exactly since `usePdfEditSession.ts`/`pdfSave.ts` import the types and call these
// exports by name. `editPdfText` and its raw snake_case mirror types are removed in this pass
// (docs/PDF-EDITOR-NOW.md §12: "remove editPdfText" from this file) — the old immediate-write,
// occurrence-based edit path has no successor here; `usePdfEditSession`/`pdfSave` own the new
// staged-command flow.
//
// PDF preview/save candidate bytes cross this boundary as ordinary JSON byte arrays. WKWebView's
// Tauri raw-response stream fails on these commands, so reliability is preferred over wire size.

import type {
  ComposePdfPreview,
  ComposePdfPreviewRequest,
  ComposedPdfPreview,
  CommitPdfSave,
  InspectPdfPage,
  ListPdfFonts,
  PdfCommitResult,
  PdfFontCatalogEntry,
  PdfPageGraph,
  PreparePdfSave,
  PreparePdfSaveRequest,
  PreparedPdfSave,
} from "./pdfEditModel.js";
import type { OcrLine } from "./pdfOcr.js";


/** Normalizes an `invoke()` raw-body result (an `ArrayBuffer`, or occasionally already a
 *  `Uint8Array`/plain byte array depending on Tauri's IPC transport) into a `Uint8Array`. */
function toUint8Array(body: unknown): Uint8Array {
  if (body instanceof Uint8Array) return body;
  if (body instanceof ArrayBuffer) return new Uint8Array(body);
  if (Array.isArray(body)) return new Uint8Array(body as number[]);
  throw new Error("Expected a raw byte response from the PDF bridge.");
}

/** Every other bridge function in this file short-circuits outside a real Tauri webview
 *  (`readDocument`, `fileFingerprint`, `getInitialOpenPaths`, …) rather than letting `invoke()`'s
 *  dynamic `@tauri-apps/api/core` import reach into `window.__TAURI_INTERNALS__`, which is
 *  `undefined` there and throws a raw, unclassified "Cannot read properties of undefined (reading
 *  'invoke')" instead of a `ClassifiedError` a caller's existing catch/`errorMessage` handling can
 *  show meaningfully. These six PDF-editor commands need the real Rust backend for source
 *  inspection/composition/save, so the correct graceful behavior is a clean, classified rejection
 *  rather than either a raw crash or a fabricated result. */
function requireTauriForPdfEdit(command: string): void {
  if (!inTauri()) {
    throw { code: "unsupported_outside_tauri", message: `${command} requires the native app` };
  }
}

/** OCR one rasterized PDF page. The PNG is the raw request body, not JSON metadata. */
export const ocrPage = async (png: ArrayBuffer | Uint8Array): Promise<OcrLine[]> => {
  // Browser QA has no Rust OCR backend, so the IPC-boundary fixture keeps the scanned-page UI
  // exercisable without moving a mock into the document components.
  requireTauriForPdfEdit("ocr_page");
  const { invoke: tauriInvoke } = await import("@tauri-apps/api/core");
  return tauriInvoke<OcrLine[]>("ocr_page", png);
};

// Cached OCR-capability probe. OCR capability cannot change mid-session, so N concurrent
// PdfViewer mounts share ONE `ocr_available` invoke instead of one IPC each. A failed probe is
// NOT cached (reset below) — the next caller retries instead of pinning a transient IPC error.
let ocrAvailableProbe: Promise<boolean> | null = null;

/** Probe whether native OCR can be offered. This deliberately returns false outside Tauri instead
 *  of using requireTauriForPdfEdit: callers use it to decide whether to offer recognition at all.
 *  Non-throwing contract: a backend/IPC failure resolves false. */
export const ocrAvailable = (): Promise<boolean> => {
  // Browser QA has no Rust OCR backend, so the IPC-boundary fixture keeps the scanned-page UI
  // exercisable without moving a mock into the document components.
  if (!inTauri()) return Promise.resolve(false);
  ocrAvailableProbe ??= invoke<boolean>("ocr_available").catch(() => {
    ocrAvailableProbe = null;
    return false;
  });
  return ocrAvailableProbe;
};

/** True only when the backend answered that this machine has no OCR. A timeout, an IPC failure or
 *  running outside Tauri is not an answer: OCR stays offered and the run itself reports the
 *  failure. Only a real answer is cached for the session. */
let ocrBackendAnswer: Promise<boolean> | null = null;
export const ocrBackendMissing = (): Promise<boolean> => {
  if (!inTauri()) return Promise.resolve(false);
  ocrBackendAnswer ??= invoke<boolean>("ocr_available").then(
    (available) => available === false,
    () => {
      ocrBackendAnswer = null;
      return false;
    },
  );
  return ocrBackendAnswer;
};

/** §5.2 `inspect_pdf_page`. Walks one page's content stream and returns its authoritative source
 *  graph (islands, locators, digests). `path` must already be a broker-granted read target. */
export const inspectPdfPage: InspectPdfPage = async (
  path: string,
  page: number,
): Promise<PdfPageGraph> => {
  // Browser QA has no Rust content-stream walker, so the IPC-boundary fixture keeps Edit-text mode
  // reachable/non-crashing (islands always empty — source-text editing stays native-only).
  requireTauriForPdfEdit("inspect_pdf_page");
  return invoke<PdfPageGraph>("inspect_pdf_page", { path, page });
};

/** §7.2 `list_pdf_fonts`. Returns the bundled + system font catalog available for embedding. */
export const listPdfFonts: ListPdfFonts = async (): Promise<PdfFontCatalogEntry[]> => {
  requireTauriForPdfEdit("list_pdf_fonts");
  return invoke<PdfFontCatalogEntry[]>("list_pdf_fonts");
};

/** §9.4 `compose_pdf_preview`. Memory-only recomposition of staged source commands against fresh
 *  disk bytes, for committed-unsaved-edit preview re-render — no token, no retained candidate, no
 *  writes. Candidate bytes use JSON; affected pages derive from request commands. */
export const composePdfPreview: ComposePdfPreview = async (
  request: ComposePdfPreviewRequest,
): Promise<ComposedPdfPreview> => {
  requireTauriForPdfEdit("compose_pdf_preview");
  const body = await invoke<number[]>("compose_pdf_preview", {
    request: {
      sourcePath: request.sourcePath,
      expectedSourceSha256: request.expectedSourceSha256,
      sourceCommands: request.sourceCommands,
    },
  });
  const pages = new Set<number>();
  for (const command of request.sourceCommands) {
    pages.add(command.kind === "replaceSourceText" ? command.locator.page : command.page);
  }
  return {
    candidateBytes: toUint8Array(body),
    affectedPages: Array.from(pages).sort((a, b) => a - b),
  };
};

/** JSON shape `prepare_pdf_save` returns (`PreparedPdfSaveMeta` in `pdf_save.rs`). */
interface RawPreparedPdfSaveMeta {
  token: string;
  candidateSha256: string;
  candidateBytes: number[];
  affectedPages: number[];
  expectedText: Array<{ page: number; text: string }>;
}

/** §10.1 `prepare_pdf_save`. The frozen frontend `PreparePdfSave` type (`pdfEditModel.ts`) expects
 *  one call returning `PreparedPdfSave` (metadata + `candidateBytes` together), but the actual Rust
 *  implementation returns candidate bytes as a numeric JSON array because WKWebView cannot
 *  reliably consume this staged candidate through Tauri's raw-response or large-base64 paths.
 *
 *  Judgment call: this bridge function internally calls both backend commands in sequence and
 *  combines their results into the single `PreparedPdfSave` shape B7's `pdfSave.ts` was written
 *  against, so the frozen frontend type/signature is satisfied without B7 needing to know about the
 *  two-call split. `expectedText` field names (`page`/`text`) come from the backend's
 *  `ExpectedTextCheck { page, text }`; mapped here to `pdfEditModel.ts`'s `PdfExpectedTextCheck`
 *  shape (`page`/`islandId`/`expectedText`) — no `islandId` exists in the backend response, so it is
 *  populated as an empty string (the backend recheck matches by page + text substring only, not by
 *  island id; see `pdf_save.rs`'s `structurally_verify_candidate`). */
export const preparePdfSave: PreparePdfSave = async (
  request: PreparePdfSaveRequest,
): Promise<PreparedPdfSave> => {
  requireTauriForPdfEdit("prepare_pdf_save");
  const meta = await invoke<RawPreparedPdfSaveMeta>("prepare_pdf_save", {
    request: {
      sourcePath: request.sourcePath,
      expectedSourceSha256: request.expectedSourceSha256,
      annotationPdfBase64: byteArrayToBase64(request.annotationPdfBytes),
      sourceCommands: request.sourceCommands,
    },
  });
  return {
    token: meta.token,
    candidateSha256: meta.candidateSha256,
    candidateBytes: Uint8Array.from(meta.candidateBytes),
    affectedPages: meta.affectedPages,
    expectedText: meta.expectedText.map((check) => ({
      page: check.page,
      islandId: "",
      expectedText: check.text,
    })),
  };
};

/** §10.3 `commit_pdf_save`. Consumes the single-use save token and atomically writes the staged
 *  candidate to `targetPath` (or back to the original source path for a same-path Save). The
 *  backend's `PdfCommitResult` (`pdf_save.rs`) carries only `{ targetPath, documentSha256 }` — it
 *  has no `revision`/`affectedPages` fields, unlike the richer shape `pdfEditModel.ts` sketches as
 *  "inferred" (that file's own comment flags this exact field set as not given anywhere in the
 *  contract text). Rather than fabricate an `affectedPages`/`revision` this command never returns,
 *  this bridge function follows the caller-refresh pattern already used elsewhere in this file
 *  (e.g. `readDocument`'s post-write re-read): the caller (`pdfSave.ts`) is expected to re-run
 *  `inspectPdfPage` per page after a successful commit to obtain a fresh revision, rather than this
 *  bridge inventing data. `committedPath`/`committedSha256` map 1:1 from the backend's
 *  `targetPath`/`documentSha256`; `affectedPages` is returned empty and `revision` as an
 *  empty-pages placeholder so the return type still satisfies `PdfCommitResult` structurally. */
export const commitPdfSave: CommitPdfSave = async (
  token: string,
  targetPath?: string,
): Promise<PdfCommitResult> => {
  requireTauriForPdfEdit("commit_pdf_save");
  const result = await invoke<{ targetPath: string; documentSha256: string }>("commit_pdf_save", {
    token,
    targetPath: targetPath ?? null,
  });
  return {
    committedPath: result.targetPath,
    committedSha256: result.documentSha256,
    affectedPages: [],
    revision: { documentSha256: result.documentSha256, pageCount: 0, pages: [] },
  };
};
