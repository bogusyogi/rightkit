import {
  baseName,
  formatFromPath,
  normalizeViewMode,
  type FileFingerprint,
  type LoadedDocument,
  type ViewMeta,
} from "../documents/types.js";
import type { LicenseClient } from "@rightkit/license";
import { isTauri } from "@rightkit/platform-ui";
import { createShell, type AssociationGroupStatus, type Shell } from "@rightkit/tauri-shell";

// Browser-QA fixture modules load on demand. Every call site sits behind `BROWSER_QA_BUILD` (a
// build-time constant), so production builds fold the guard to `false`, drop these dynamic imports,
// and never emit the fixture chunks. Keep static imports of these modules out of this file.
const loadQaFixtures = () => import("./browserQa.js");
const loadQaText = () => import("./browserQaText.js");
const loadQaDocx = () => import("./browserQaDocx.js");
const loadQaPdf = () => import("./browserQaPdf.js");
const loadQaLicense = () => import("./browserQaLicense.js");

/** True when running inside the Tauri webview (vs. plain `vite dev` in a browser). */
const inTauri = isTauri;

let shell: Shell | null = null;
/** The one `@rightkit/tauri-shell` client (menu state, window actions, recovery). Native webview only. */
export function getShell(): Shell {
  shell ??= createShell();
  return shell;
}

/**
 * Build-time switch for the browser-QA fixtures: `true` in `vite dev` and in the
 * `VITE_RIGHTKIT_BROWSER_QA=1` build, a literal `false` in production builds. Every QA branch in
 * this module is guarded with it, so the minifier drops those branches and nothing references the
 * `browserQa*` fixture modules in a production bundle.
 */
const BROWSER_QA_BUILD = import.meta.env?.DEV || import.meta.env?.VITE_RIGHTKIT_BROWSER_QA === "1";

/**
 * Write text to the system clipboard: through the shell's native clipboard in the app, through the
 * browser clipboard in browser QA. It rejects when no clipboard is available, so a caller never
 * reports a copy that did not happen (an absent browser clipboard used to read as success).
 */
export async function writeClipboardText(text: string): Promise<void> {
  if (inTauri()) {
    await getShell().clipboard.write({ text });
    return;
  }
  if (typeof navigator === "undefined" || !navigator.clipboard) {
    throw new Error("The clipboard is unavailable.");
  }
  await navigator.clipboard.writeText(text);
}

export function isBrowserQaMode(): boolean {
  const qaBuild = import.meta.env?.VITE_RIGHTKIT_BROWSER_QA === "1";
  if (!import.meta.env?.DEV && !qaBuild) return false;
  if (typeof window === "undefined" || inTauri()) return false;
  const params = new URLSearchParams(window.location.search);
  return params.has("qa") || qaBuild;
}

async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<T>(cmd, args);
}

async function listen<T>(
  event: string,
  callback: (event: { payload: T }) => void,
): Promise<() => void> {
  const { listen } = await import("@tauri-apps/api/event");
  return listen<T>(event, callback);
}

/**
 * The raw IPC pair handed to the shell-owned clients (`createLicenseClient`,
 * `createNativeUpdateClient`). Native code owns the licence record, tokens, device identity and the
 * update Bearer; the webview only sends commands and receives validated display views.
 */
export const nativeIpc = { invoke, listen };

/**
 * Browser-QA licence client (`@rightkit/license/testing`, in-memory, no network). Returns null
 * outside browser QA, so the caller uses the native client. The `testing` export resolves only
 * under the `development` condition and the call site folds to `false` in a release build.
 */
export async function browserQaLicenseClient(): Promise<LicenseClient | null> {
  if (BROWSER_QA_BUILD && isBrowserQaMode()) return (await loadQaLicense()).qaLicenseClient();
  return null;
}

/** Classified shape every bridge caller can rely on. `code` matches the Rust `HostError`
 *  variant in snake_case (e.g. "broker_denied", "file_not_found"), or "unknown" when the rejection
 *  didn't carry the envelope shape at all (a raw JS Error, a plain string, or something else). */
export interface ClassifiedError {
  code: string;
  message: string;
}

/** Classify an IPC rejection from `invoke()` into `{code, message}`. Tauri serializes a command's
 *  `Err(HostError)` using the envelope's own `#[serde(tag = "code", content = "message")]`
 *  representation, so a migrated command's rejection value IS already `{code, message}` — this
 *  just narrows/validates that shape defensively (a stale/unmigrated command, a thrown JS Error
 *  from the bridge itself, or invoke() rejecting with a bare string, all still need a safe answer).
 *  This does NOT change any caller's UX in this lane — callers keep showing `message`; contextual
 *  recovery UI per `code` is a later lane. */
export function parseHostError(e: unknown): ClassifiedError {
  if (
    e !== null &&
    typeof e === "object" &&
    "code" in e &&
    "message" in e &&
    typeof (e as { code: unknown }).code === "string" &&
    typeof (e as { message: unknown }).message === "string"
  ) {
    return { code: (e as { code: string }).code, message: (e as { message: string }).message };
  }
  if (typeof e === "string") {
    return { code: "unknown", message: e };
  }
  if (e instanceof Error) {
    return { code: "unknown", message: e.message };
  }
  return { code: "unknown", message: String(e) };
}

export async function openFileDialog(): Promise<string | null> {
  if (BROWSER_QA_BUILD && isBrowserQaMode()) {
    return (await loadQaFixtures()).qaDialogPath();
  }
  if (!inTauri()) {
    console.warn("openFileDialog: not in Tauri — open a real document via `pnpm tauri dev`.");
    return null;
  }
  return invoke<string | null>("open_file_dialog");
}

interface RawDocument {
  path: string;
  content: string;
  last_view?: LoadedDocument["lastView"];
  view_meta?: string;
  lossy?: boolean;
  encoding?: string;
  mtime_ms?: number;
  size?: number;
  source_sha256?: string;
  stream_token?: string;
}

export interface WriteDocumentResult {
  encoding: string;
  sourceSha256: string;
}

interface RawWriteDocumentResult {
  encoding: string;
  source_sha256: string;
}

export async function readDocument(path: string): Promise<LoadedDocument> {
  const format = formatFromPath(path);
  if (BROWSER_QA_BUILD && isBrowserQaMode()) {
    const qaDocx = await loadQaDocx();
    const qaText = await loadQaText();
    const savedDocx = qaDocx.readQaDocxDocument({
      path,
      displayName: baseName(path),
      format,
      content: "",
    });
    if (savedDocx) return savedDocx;
    const doc = await (await loadQaFixtures()).qaDocumentForPath(path);
    if (doc) {
      if (format === "pdf") {
        const assetUrl = await (await loadQaPdf()).qaCurrentPdfAssetUrl(path);
        return { ...doc, assetUrl: assetUrl ?? doc.assetUrl };
      }
      return qaText.readQaTextDocument(doc);
    }
    return qaText.readQaTextDocument({ path, displayName: baseName(path), format, content: "" });
  }
  if (!inTauri()) {
    return { path, displayName: baseName(path), format, content: "" };
  }
  const raw = await invoke<RawDocument>("read_document", { path });
  // PDF/DOCX/image viewer URLs use an opaque, revocable backend stream token instead of the file
  // path. Closing the tab revokes the token, and reopening the same file mints a fresh URL.
  let assetUrl: string | undefined;
  if (format === "pdf" || format === "docx" || format === "image") {
    const { convertFileSrc } = await import("@tauri-apps/api/core");
    if (!raw.stream_token) throw new Error("binary stream token missing");
    assetUrl = `${convertFileSrc(raw.stream_token, "viewright-stream")}?t=${Date.now()}`;
  }
  let viewMeta: ViewMeta | undefined;
  if (raw.view_meta) {
    try {
      viewMeta = JSON.parse(raw.view_meta) as ViewMeta;
    } catch {
      /* ignore malformed persisted state */
    }
  }
  return {
    path: raw.path,
    displayName: baseName(raw.path),
    format,
    // Images are opened through assetUrl only. Ignore a stale pre-stream backend payload so an
    // upgraded shell never retains a second base64 copy in the React document state.
    content: format === "image" ? "" : raw.content,
    assetUrl,
    lastView: normalizeViewMode(raw.last_view),
    viewMeta,
    lossy: raw.lossy ?? false,
    encoding: raw.encoding ?? "UTF-8",
    sourceSha256: raw.source_sha256,
    fingerprint: { mtimeMs: raw.mtime_ms ?? 0, size: raw.size ?? 0 },
  };
}

/** Current on-disk fingerprint for an open file, or null outside Tauri / if it can't be stat'd. */
export async function fileFingerprint(path: string): Promise<FileFingerprint | null> {
  if (!inTauri()) return null;
  try {
    const fp = await invoke<{ mtime_ms: number; size: number }>("file_fingerprint", { path });
    return { mtimeMs: fp.mtime_ms, size: fp.size };
  } catch {
    return null;
  }
}

/**
 * What opening the local database had to do. `degraded` is false for a normal open. When true,
 * `quarantinedTo` is where the unreadable database was kept (original bytes intact) before a fresh
 * one replaced it. There is no in-memory fallback: a database that cannot open ends startup.
 */
export async function getPersistenceStatus(): Promise<{ degraded: boolean; quarantinedTo: string | null }> {
  if (!inTauri()) return { degraded: false, quarantinedTo: null };
  const status = await invoke<{ degraded: boolean; quarantined_to?: string | null }>("persistence_status");
  return { degraded: status.degraded === true, quarantinedTo: status.quarantined_to ?? null };
}

export async function getInitialOpenPaths(): Promise<string[]> {
  if (BROWSER_QA_BUILD && isBrowserQaMode()) {
    const path = (await loadQaFixtures()).qaInitialOpenPath();
    return path ? [path] : [];
  }
  if (!inTauri()) return [];
  // The shell's open-paths gate: it grants and drains what argv / a second launch / macOS
  // open-document queued so far (display paths only) and switches to live `rightkit-shell://open-paths`
  // delivery, which `onAppEvent("open-file")` listens for. Subscribe first, then call this.
  return getShell().openPaths.ready();
}

/** Read a stylesheet/image/font beside an opened HTML doc for inlining into the preview. */
export async function readRelativeAsset(
  docPath: string,
  rel: string,
  rootPath?: string,
  siteToken?: string,
): Promise<{ mime: string; base64: string } | null> {
  if (!inTauri()) return null;
  try {
    return await invoke<{ mime: string; base64: string }>("read_relative_asset", {
      docPath,
      rel,
      rootPath,
      siteToken,
    });
  } catch {
    return null;
  }
}

/** Read a same-folder JavaScript file for an explicitly requested interactive local HTML preview.
 *  This is intentionally separate from `readRelativeAsset`: safe HTML preview still cannot inline
 *  scripts, and Rust keeps a script-specific allowlist + size cap. */
export async function readRelativeScript(
  docPath: string,
  rel: string,
  rootPath?: string,
  siteToken?: string,
): Promise<string | null> {
  if (!inTauri()) return null;
  try {
    return await invoke<string>("read_relative_script", { docPath, rel, rootPath, siteToken });
  } catch {
    return null;
  }
}

/** Grant + resolve a document linked relatively from an opened HTML doc (e.g. "chapter-2.html"),
 *  so it can open as a tab. Returns the granted absolute path, or null (skip) outside Tauri / on
 *  rejection (escapes the granted root, not an openable type). The grant is brokered in Rust. */
export async function grantRelativeFile(
  docPath: string,
  rel: string,
  rootPath?: string,
  siteToken?: string,
): Promise<string | null> {
  if (!inTauri()) return null;
  try {
    return await invoke<string>("grant_relative_file", { docPath, rel, rootPath, siteToken });
  } catch {
    return null;
  }
}

/** Infer and grant this HTML tab's site root (nearest enclosing project directory), returning the
 *  opaque token that backs `viewright-site://` subresource URLs plus the granted directory for
 *  display. Returns null outside Tauri, so browser/dev/test builds fall back to inlining. */
export async function grantSiteRoot(
  docPath: string,
): Promise<{ token: string; root: string; markerFound: boolean } | null> {
  if (!inTauri()) return null;
  try {
    return await invoke<{ token: string; root: string; markerFound: boolean }>(
      "grant_site_root",
      { docPath },
    );
  } catch {
    return null;
  }
}

/** Open the native folder picker so the user can choose this document's site root. The directory
 *  must contain the document (enforced in Rust). Returns the new grant, or null when cancelled,
 *  refused, or outside Tauri; the caller revokes its previous grant after a non-null result. */
export async function chooseSiteRoot(
  docPath: string,
): Promise<{ token: string; root: string; markerFound: boolean } | null> {
  if (!inTauri()) return null;
  try {
    return await invoke<{ token: string; root: string; markerFound: boolean } | null>(
      "choose_site_root",
      { docPath },
    );
  } catch {
    return null;
  }
}

/** Release a site-root grant when its tab closes. Best-effort: a failure here only means the grant
 *  outlives the tab until the process exits, never a broken tab. */
export async function revokeSiteRoot(token: string): Promise<void> {
  if (!inTauri()) return;
  try {
    await invoke("revoke_site_root", { token });
  } catch {
    /* best-effort */
  }
}

/** List sibling image files in the same folder as an already-open image (naturally sorted), so the
 *  viewer can page through the folder like Windows Photos. Each returned path is granted in Rust, so
 *  it can be opened without a fresh OS grant. Returns [] outside Tauri / on rejection. */
export async function listSiblingImages(path: string): Promise<string[]> {
  if (!inTauri()) return [];
  try {
    return await invoke<string[]>("list_sibling_images", { path });
  } catch {
    // Non-fatal: outside Tauri, an unauthorized path, or an unreadable folder → no siblings, so
    // folder paging is simply disabled for this image (no chevrons/arrows). Matches the silent-skip
    // pattern of readRelativeAsset/grantRelativeFile above.
    return [];
  }
}

export async function watchFile(path: string): Promise<void> {
  if (!inTauri()) return;
  await invoke("watch_file", { path });
}

/** Watch a SET of files as one context (entry + visited sub-pages of an HTML site session), so an
 *  external edit to any of them prompts. Re-arms the single watcher; supersedes a prior watchFile. */
export async function watchFiles(paths: string[]): Promise<void> {
  if (!inTauri()) return;
  await invoke("watch_files", { paths });
}

/** Revoke a path's broker authorization when its last tab closes (blast-radius reduction). Silent
 *  best-effort — outside Tauri it's a no-op, and the backend treats an unknown path as a no-op. */
export async function revokePath(path: string): Promise<void> {
  if (!inTauri()) return;
  try {
    await invoke("revoke_path", { path });
  } catch {
    /* non-fatal — revocation is opportunistic */
  }
}

/** Write text, preserving the file's `encoding` (default UTF-8). Returns actual encoding plus the
 *  committed source SHA-256; in-place callers send the prior SHA for stale-source protection. */
export async function writeDocument(
  path: string,
  content: string,
  encoding?: string,
  expectedSourceSha256?: string,
): Promise<WriteDocumentResult> {
  if (BROWSER_QA_BUILD && isBrowserQaMode())
    return (await loadQaText()).writeQaTextDocument(path, content, encoding, expectedSourceSha256);
  if (!inTauri()) throw new Error("Saving requires the desktop app.");
  const result = await invoke<RawWriteDocumentResult>("write_document", {
    path,
    content,
    encoding,
    expectedSourceSha256,
  });
  return { encoding: result.encoding, sourceSha256: result.source_sha256 };
}

// PERF-AUDIT T1-3 (2026-07-15): re-checked whether `String.fromCharCode(...slice)` (spread, batched
// per 0x8000-byte chunk) could replace the byte-by-byte loop below to cut call overhead on large
// DOCX saves. Left AS-IS: this function is synchronous with no await point before it returns to
// writeDocumentBytes's invoke() call, so there's no way to yield the UI thread mid-encode without
// changing writeDocumentBytes's contract (out of scope here) — and a 32k-arg spread/apply call is
// exactly the call-stack-pressure risk the loop below was already written to avoid. Not a clear
// improvement, so kept unchanged per the audit's own guidance.
function arrayBufferToBase64(ab: ArrayBuffer): string {
  return byteArrayToBase64(new Uint8Array(ab));
}

function byteArrayToBase64(bytes: Uint8Array): string {
  let bin = "";
  const chunk = 0x8000;
  for (let i = 0; i < bytes.length; i += chunk) {
    // Explicit byte-by-byte build instead of `apply(null, [...slice])`: avoids the 32k-element args
    // array allocation per chunk AND call-stack pressure from apply() on large DOCX saves (>5MB).
    // Modern engines optimize `String.fromCharCode(byte)` in a tight loop well; btoa handles the rest.
    const slice = bytes.subarray(i, i + chunk);
    for (let j = 0; j < slice.length; j++) {
      bin += String.fromCharCode(slice[j]);
    }
  }
  return btoa(bin);
}

export async function writeDocumentBytes(path: string, data: ArrayBuffer): Promise<void> {
  if (BROWSER_QA_BUILD && isBrowserQaMode()) {
    if (formatFromPath(path) === "docx")
      await (await loadQaDocx()).writeQaDocxDocument(path, data, arrayBufferToBase64(data));
    return;
  }
  if (!inTauri()) throw new Error("Saving requires the desktop app.");
  // Raw request body (like ocr_page): the DOCX bytes cross IPC as-is, with no JS base64 encode on
  // the main thread. The target path rides in a hex header so any filesystem path is header-safe.
  const { invoke: tauriInvoke } = await import("@tauri-apps/api/core");
  await tauriInvoke("write_document_base64", new Uint8Array(data), {
    headers: { "x-viewright-path-hex": pathToHex(path) },
  });
}

function pathToHex(path: string): string {
  let hex = "";
  for (const byte of new TextEncoder().encode(path)) hex += byte.toString(16).padStart(2, "0");
  return hex;
}

/** Copy an already-authorized document to a user-selected write target without decoding or
 * reserializing its bytes. The desktop command owns both broker checks and the filesystem copy so
 * PDF Save As preserves signatures, encryption, widgets, and any bytes pdf.js cannot round-trip. */
export async function copyDocument(sourcePath: string, targetPath: string): Promise<void> {
  if (BROWSER_QA_BUILD && isBrowserQaMode()) return;
  if (!inTauri()) throw new Error("Saving requires the desktop app.");
  await invoke("copy_document", { sourcePath, targetPath });
}

// Stored link text is left exactly as the user typed it; only at OPEN time do we assume https:// for
// a schemeless address (like a browser does for "www.x.com"). Anything with an explicit scheme
// (https:, mailto:, and also javascript:/file: which the Rust allowlist then blocks) is untouched.
function toOpenableUrl(url: string): string {
  return /^[a-z][a-z0-9+.-]*:/i.test(url) ? url : `https://${url}`;
}

/** Open a web/mail link in the system browser (never navigate the app webview away). */
export async function openExternal(url: string): Promise<void> {
  const opened = toOpenableUrl(url);
  if (!inTauri()) {
    window.open(opened, "_blank", "noopener");
    return;
  }
  await invoke("rk_shell_open_external", { url: opened });
}

export async function editImageExternal(path: string): Promise<void> {
  if (!inTauri()) return;
  await invoke("edit_image_external", { path });
}

// ── PDF sparse-edit bridge (docs/PDF-EDITOR-NOW.md §5.2/§7.2/§9.4/§10.1/§10.3) ──────────────
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
} from "../documents/pdf/pdfEditModel.js";
import type { OcrLine } from "../documents/pdf/pdfOcr.js";
import { hostAppName } from "../appName.js";

if (BROWSER_QA_BUILD && isBrowserQaMode())
  void loadQaPdf().then((qaPdf) => qaPdf.registerViewrightQaGlobal());

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
 *  show meaningfully. These six PDF-editor commands have no browser-QA fixture to fall back to
 *  (unlike `readDocument`'s `qaDocumentForPath`) — a real Rust backend is unavoidable for source
 *  inspection/composition/save — so the correct graceful behavior is a clean, classified rejection
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
  if (BROWSER_QA_BUILD && isBrowserQaMode()) return (await loadQaFixtures()).qaOcrLines();
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
  if (BROWSER_QA_BUILD && isBrowserQaMode()) return Promise.resolve(true);
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
  if (BROWSER_QA_BUILD && isBrowserQaMode())
    return (await loadQaPdf()).qaInspectPdfPage(path, page);
  requireTauriForPdfEdit("inspect_pdf_page");
  return invoke<PdfPageGraph>("inspect_pdf_page", { path, page });
};

/** §7.2 `list_pdf_fonts`. Returns the bundled + system font catalog available for embedding. */
export const listPdfFonts: ListPdfFonts = async (): Promise<PdfFontCatalogEntry[]> => {
  if (BROWSER_QA_BUILD && isBrowserQaMode()) return (await loadQaPdf()).qaListPdfFonts();
  requireTauriForPdfEdit("list_pdf_fonts");
  return invoke<PdfFontCatalogEntry[]>("list_pdf_fonts");
};

/** §9.4 `compose_pdf_preview`. Memory-only recomposition of staged source commands against fresh
 *  disk bytes, for committed-unsaved-edit preview re-render — no token, no retained candidate, no
 *  writes. Candidate bytes use JSON; affected pages derive from request commands. */
export const composePdfPreview: ComposePdfPreview = async (
  request: ComposePdfPreviewRequest,
): Promise<ComposedPdfPreview> => {
  if (BROWSER_QA_BUILD && isBrowserQaMode())
    return (await loadQaPdf()).qaComposePdfPreview(request);
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
  if (BROWSER_QA_BUILD && isBrowserQaMode()) return (await loadQaPdf()).qaPreparePdfSave(request);
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
  if (BROWSER_QA_BUILD && isBrowserQaMode())
    return (await loadQaPdf()).qaCommitPdfSave(token, targetPath);
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

/** Browser-QA-only: current bytes of the in-memory QA PDF "disk" at `path`, or `null` outside QA
 *  mode / for an untouched path. PdfViewer.tsx's post-commit reload uses this instead of
 *  re-fetching its `url` prop — a real Tauri stream token is a revocable token that re-resolves to
 *  the same (now-committed) file on every fetch, but browser QA's `url` is a static `data:` URL
 *  captured once at document-open time, which can never reflect a later commit on its own. */
export async function qaReloadPdfBytes(path: string): Promise<Uint8Array | null> {
  if (!BROWSER_QA_BUILD || !isBrowserQaMode()) return null;
  return (await loadQaPdf()).qaCurrentPdfBytes(path);
}

/** Token of the interactive HTML preview that currently owns DevTools (the active tab's frame), or
 *  null. Set by the HTML preview frame; DevTools opens exactly that registered document. */
let inspectableHtmlToken: string | null = null;

export function setInspectableHtmlPreview(token: string | null): void {
  inspectableHtmlToken = token;
}

/** Clear the DevTools owner, but only if `token` still owns it (a newer frame may have replaced it). */
export function releaseInspectableHtmlPreview(token: string): void {
  if (inspectableHtmlToken === token) inspectableHtmlToken = null;
}

/** Open DevTools for the opened HTML document: the same registered preview loads in a separate
 *  content window that has no IPC, and the shell opens that window's inspector (decision D2). The
 *  main webview is never inspectable. No-op without an active HTML preview or outside Tauri. */
export async function openDevtools(): Promise<void> {
  const token = inspectableHtmlToken;
  if (!token || (BROWSER_QA_BUILD && isBrowserQaMode()) || !inTauri()) return;
  await invoke("open_html_devtools", { token });
}

/** No-op: the macOS inspect-accelerator scope command was removed with the shell port (viewright#375). */
export async function setDevtoolsShortcutScope(_enabled: boolean): Promise<void> {}

export async function revealInFileExplorer(path: string): Promise<void> {
  if ((BROWSER_QA_BUILD && isBrowserQaMode()) || !inTauri()) return;
  await invoke("reveal_in_file_explorer", { path });
}

/** What the clipboard held when "Open from clipboard" / paste ran. Files win, then a bitmap (written
 *  to a temp `.bmp`), then plain text. Mirrors the Rust `ClipboardContent` enum. */
export type ClipboardContent =
  | { kind: "files"; paths: string[] }
  | { kind: "image"; path: string }
  | { kind: "text"; text: string }
  | { kind: "empty" };

/** Read the clipboard: copied files (granted paths), a bitmap (granted temp `.bmp`), or plain
 *  text. Returns `{ kind: "empty" }` outside Tauri or when nothing openable is present. */
export async function readClipboardContent(): Promise<ClipboardContent> {
  if (!inTauri()) return { kind: "empty" };
  try {
    // The shell's typed item: files and images are already broker-granted; `none` is our `empty`.
    const item = await invoke<ClipboardContent | { kind: "none" }>("rk_shell_clipboard_item");
    return !item || item.kind === "none" ? { kind: "empty" } : item;
  } catch {
    return { kind: "empty" };
  }
}

export async function printCurrentWebview(): Promise<boolean> {
  if (!inTauri()) return false;
  try {
    // The shell's print: WebView2's in-app preview on Windows, the system print panel on macOS.
    await getShell().print();
    return true;
  } catch {
    return false;
  }
}

export async function listSystemFonts(): Promise<string[]> {
  if (BROWSER_QA_BUILD && isBrowserQaMode())
    return ["Inter", "Source Serif 4", "JetBrains Mono", "Geist", "Newsreader"];
  if (!inTauri()) return [];
  return invoke<string[]>("list_system_fonts");
}

// ── Supported-format file associations ─────────────────────────────────────
// The Settings panel keeps this per-format contract, mapped from the shell's association groups
// (`rk_shell_associations_*`, group id = format id; native spec in commands/file_associations.rs).
// Setting a default runs ONLY from the explicit Settings action: never on startup, never on update.
export interface MarkdownUtiHandlerStatus {
  /** UTI (macOS) or `extension:.ext` id (Windows): the shell's `target`. */
  uti: string;
  preferredHandler: string | null;
  hostIsPreferred: boolean;
}

export interface FileAssociationFormatStatus {
  formatId: string;
  extensions: string[];
  handlers: MarkdownUtiHandlerStatus[];
  allSet: boolean;
}

export interface FileAssociationStatus {
  formats: FileAssociationFormatStatus[];
}

export interface SetFileAssociationDefaultsResult {
  formats: FileAssociationFormatStatus[];
  allSet: boolean;
}

const QA_FILE_ASSOCIATION_FORMATS = ["md", "mdx", "txt", "pdf", "html", "docx", "image"];
let qaPreferredAssociationFormats = new Set(["md", "mdx"]);

function qaFileAssociationStatus(): FileAssociationStatus {
  return {
    formats: QA_FILE_ASSOCIATION_FORMATS.map((formatId) => ({
      formatId,
      extensions: [],
      handlers: [],
      allSet: qaPreferredAssociationFormats.has(formatId),
    })),
  };
}

function toFormatStatus(group: AssociationGroupStatus): FileAssociationFormatStatus {
  return {
    formatId: group.id,
    extensions: group.extensions,
    handlers: group.handlers.map((handler) => ({
      uti: handler.target,
      preferredHandler: handler.preferredHandler,
      hostIsPreferred: handler.isDefault,
    })),
    allSet: group.allSet,
  };
}

export async function fileAssociationStatus(): Promise<FileAssociationStatus> {
  if (BROWSER_QA_BUILD && isBrowserQaMode()) return qaFileAssociationStatus();
  const status = await getShell().associations.status();
  return { formats: status.groups.map(toFormatStatus) };
}

export async function setFileAssociationDefaults(
  formatIds: string[],
): Promise<SetFileAssociationDefaultsResult> {
  if (BROWSER_QA_BUILD && isBrowserQaMode()) {
    qaPreferredAssociationFormats = new Set(formatIds);
    const status = qaFileAssociationStatus();
    return { formats: status.formats, allSet: formatIds.length > 0 };
  }
  // macOS: one NSWorkspace request per content type (system consent sheet), one shared 60 s
  // deadline (the shell default; the host clamps a given value to 1 to 120 s).
  const result = await getShell().associations.set(formatIds);
  return { formats: result.groups.map(toFormatStatus), allSet: result.allSet };
}

/** Windows owns protected default-app choices. This opens Windows Default apps; the shell never
 * writes UserChoice registry hashes or claims a default changed. */
export async function openFileAssociationSettings(_formatIds: string[]): Promise<void> {
  if (BROWSER_QA_BUILD && isBrowserQaMode()) return;
  await getShell().associations.openDefaultApps();
}

export async function saveSession(tabs: Array<[string, boolean]>): Promise<void> {
  if (!inTauri()) return;
  await invoke("save_session", { tabs });
}

export async function loadSession(): Promise<Array<[string, boolean]>> {
  if (!inTauri()) return [];
  return invoke<Array<[string, boolean]>>("load_session");
}

export async function clearSession(): Promise<void> {
  if (!inTauri()) return;
  await invoke("clear_session");
}

/** Delete recent files, saved session tabs and pins (not settings) from the native database. */
export async function purgeUserPaths(): Promise<void> {
  if (!inTauri()) return;
  await invoke("purge_user_paths");
}

export async function saveSetupRecovery(snapshot: string): Promise<void> {
  if (!inTauri()) return;
  await getShell().recovery.save(snapshot);
}

export async function loadSetupRecovery(): Promise<string | null> {
  if (!inTauri()) return null;
  const snapshot = await getShell().recovery.load<unknown>();
  return snapshot == null ? null : typeof snapshot === "string" ? snapshot : JSON.stringify(snapshot);
}

export async function clearSetupRecovery(): Promise<void> {
  if (!inTauri()) return;
  await getShell().recovery.clear();
}

/** Key of the settings document inside the native settings commands (`settings_get` / `settings_set`). */
export const SETTINGS_DOCUMENT_KEY = "vr.settings.v1";

/** Read the durable settings document (the `rightkit-app-settings` file behind `settings_get`).
 *  Resolves null outside Tauri and when nothing has ever been saved. A native failure (refused,
 *  unreadable or corrupt store) rejects, so the caller can show it and refuse to overwrite data it
 *  could not read. */
export async function loadSettingsDocument(): Promise<SettingsDocumentRead> {
  if (!inTauri()) return { value: null, revision: 0 };
  return await invoke<SettingsDocumentRead>("settings_get", {
    key: SETTINGS_DOCUMENT_KEY,
    withRevision: true,
  });
}

/** The settings document as the native store holds it: its JSON text (null when empty) and the
 *  committed revision (0 when nothing is stored). */
export interface SettingsDocumentRead {
  value: string | null;
  revision: number;
}

/** Commit through `settings_set`. With `merge`, `value` is a top-level patch applied inside the
 *  store's write lock (`null` removes a key); otherwise it replaces the document. `expectedRevision`
 *  makes the commit a compare-and-swap against the committed revision: a stale one rejects with a
 *  `write_conflict` and nothing is written. Resolves the committed document and revision, or null
 *  outside Tauri (browser QA keeps only the boot cache). Rejects when the native commit fails. */
export async function saveSettingsDocument(
  value: string,
  options: { expectedRevision?: number; merge?: boolean } = {},
): Promise<{ value: string; revision: number } | null> {
  if (!inTauri()) return null;
  return await invoke<{ value: string; revision: number }>("settings_set", {
    key: SETTINGS_DOCUMENT_KEY,
    value,
    expectedRevision: options.expectedRevision ?? null,
    merge: options.merge ?? false,
  });
}

/** Generic key read through the native settings table. Only the explorer record (m10-explorer-keys)
 *  uses it; the settings document goes through `loadSettingsDocument`. Null outside Tauri, for an
 *  unwritten key, or on a bridge failure. */
export async function getSettingsMirror(key: string): Promise<string | null> {
  if (!inTauri()) return null;
  try {
    return await invoke<string | null>("settings_get", { key });
  } catch {
    return null;
  }
}

/** Generic key write for the explorer record (see `getSettingsMirror`). Best effort. */
export async function setSettingsMirror(key: string, value: string): Promise<void> {
  if (!inTauri()) return;
  try {
    await invoke("settings_set", { key, value });
  } catch {
    /* best-effort: the explorer record is a convenience cache */
  }
}

/** Route every window.open(url) through the brokered external-link allowlist. The vendored DOCX
 *  editor opens hyperlinks via raw window.open(href), bypassing openExternal — and the editor runs
 *  from a prebuilt package, so this app-level guard closes the bypass without forking the vendor.
 *  Empty/about:blank opens (the DOCX print path creates a blank print window) pass through. In a
 *  plain browser it falls back to native open; in Tauri it enforces the http/https/mailto allowlist
 *  via the Rust command (rejecting file:/javascript:/data: etc.). Install once at startup. */
export function installExternalLinkGuard(): void {
  if (typeof window === "undefined") return;
  const native = window.open.bind(window);
  window.open = ((url?: string | URL, target?: string, features?: string): Window | null => {
    const href = url == null ? "" : String(url);
    if (href === "" || href === "about:blank") return native(url as string, target, features);
    if (!inTauri()) return native(url as string, target, features);
    void invoke("rk_shell_open_external", { url: toOpenableUrl(href) }).catch(() => {
      // Best-effort guard interception; never make a blocked popup crash the caller.
    });
    return null;
  }) as typeof window.open;
}

export async function saveFileDialog(
  defaultName?: string,
  format?: string,
): Promise<string | null> {
  if (BROWSER_QA_BUILD && isBrowserQaMode())
    return `D:\\${hostAppName()} QA\\${defaultName ?? `untitled.${format ?? "txt"}`}`;
  if (!inTauri()) return null;
  return invoke<string | null>("save_file_dialog", { defaultName, format });
}

export async function grantKnownFileToken(token: string): Promise<string | null> {
  if (BROWSER_QA_BUILD && isBrowserQaMode()) return token;
  if (!inTauri()) return null;
  return invoke<string | null>("grant_known_file_token", { token });
}

// Purpose-scoped replacements for the old generic grant_existing_file_path command (which let the
// renderer mint a grant for any string path). Each caller now states its intent:
//   - openLinkedDocument: the user explicitly referenced `target` from the host app itself (Open URL
//     dialog input, open-from-clipboard paste). `from` anchors the grant to an already-open
//     document when the reference came from one; omit it when the reference is the user's own
//     typed/pasted text, which carries the same trust level as a native open-file dialog result.
//   - regrantSavedTarget: re-grant a path the host app itself just wrote (save-as, duplicate, DOCX
//     save-as) — refused unless the exact path was written in the last few writes.
export async function openLinkedDocument(
  target: string,
  from?: string | null,
): Promise<string | null> {
  if (BROWSER_QA_BUILD && isBrowserQaMode()) return target;
  if (!inTauri()) return null;
  return invoke<string | null>("open_linked_document", { target, from: from ?? null });
}

export async function regrantSavedTarget(path: string): Promise<string | null> {
  if (BROWSER_QA_BUILD && isBrowserQaMode()) return path;
  if (!inTauri()) return null;
  return invoke<string | null>("regrant_saved_target", { path });
}

export {
  onAppEvent,
  onContextOpenLinkExternal,
  onContextOpenLinkTab,
  onContextViewSource,
} from "./bridgeEvents.js";
export { inTauri };
