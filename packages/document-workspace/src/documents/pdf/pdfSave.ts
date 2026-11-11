/**
 * PDF save orchestration (`docs/PDF-EDITOR-NOW.md` §9.4, §10).
 *
 * Two independent flows live here, both built against the *injected-bridge* function types frozen
 * in `pdfEditModel.ts` (§10.3's "Frozen cross-lane seams" — the real `tauriBridge.ts` Tauri-backed
 * implementations land later at Dispatch D2, so every backend call here is a parameter, never an
 * import):
 *
 *  1. `recomposePdfPreview` (§9.4) — calls an injected `ComposePdfPreview`-shaped function to get
 *     memory-only candidate bytes for the current source-command history, so the on-page display of
 *     committed-but-unsaved edits always renders from the same composer Save itself will use.
 *
 *  2. `createPdfSavePlan` (§10.1/§10.2/§10.3) — the two-phase Save protocol: prepare (Rust composes
 *     + structurally validates a candidate and returns it plus a short-lived token), isolated
 *     pdf.js render-verify (catch corruption client-side before committing), then commit (Rust
 *     atomically replaces bytes). Returns a `PdfSavePlan` (§10.3's exact shape) rather than bare
 *     bytes, so the generic document saver only updates recents/fingerprints/checkpoints after a
 *     real commit succeeds.
 *
 * `verifyPdfCandidate` (§10.2) loads candidate bytes into a FRESH, isolated pdf.js document — never
 * the active on-screen `PDFDocumentProxy` — and renders affected pages to an offscreen canvas before
 * extracting text for the expected-text checks. This mirrors `pdfPrinting.ts`'s isolated-document
 * pattern (pdf.js caches page proxies per document; reusing the live viewer's document risks a
 * second concurrent `render()` against a `PDFPageProxy` it may already be rendering — a hang, not an
 * error). Every await is time-bounded so a stuck pdf.js worker surfaces as a rejected promise
 * (`VerificationFailed`) instead of silence.
 */

import * as pdfjs from "pdfjs-dist";
import type { PDFDocumentProxy, PDFPageProxy } from "pdfjs-dist";
import type {
  CommitPdfSave,
  ComposePdfPreview,
  ComposePdfPreviewRequest,
  ComposedPdfPreview,
  PdfCommitResult,
  PdfEditCommand,
  PdfExpectedTextCheck,
  PdfSavePlan,
  PreparePdfSave,
  PreparePdfSaveRequest,
  PreparedPdfSave,
} from "./pdfEditModel.js";
import { pdfDocumentInit, pdfWasmUrl } from "@rightkit/pdf-ui";
import { withTimeout } from "./pdfShared.js";
import { logEvent } from "@rightkit/document-core/state/diagnostics";

// ---------------------------------------------------------------------------------------------
// §9.4 in-memory preview composition
// ---------------------------------------------------------------------------------------------

/** Compose final replacements against original bytes without deleting undo history. A failed
 * preview leaves its original locator visible; correcting it supersedes earlier replacements
 * of exactly those bytes, rather than retrying an invalid intermediate edit first. */
function effectiveSourceCommands(commands: readonly PdfEditCommand[]): readonly PdfEditCommand[] {
  const seen = new Set<string>();
  const effective: PdfEditCommand[] = [];
  for (const command of [...commands].reverse()) {
    if (command.kind === "replaceSourceText") {
      const { documentSha256, page, islandDigest, spans } = command.locator;
      const key = JSON.stringify([
        documentSha256,
        page,
        islandDigest,
        spans.map((span) => [
          span.streamRef,
          span.contentsIndex,
          span.operatorIndex,
          span.operandIndex,
          span.glyphStart,
          span.glyphEnd,
          span.expectedText,
          span.expectedDigest,
        ]),
      ]);
      if (seen.has(key)) continue;
      seen.add(key);
    }
    effective.push(command);
  }
  return effective.reverse();
}

/** Deps for `recomposePdfPreview` — just the injected bridge call, kept as an object so call sites
 *  read as named fields rather than positional args (matches this module's other `*Deps` shapes). */
export interface RecomposePdfPreviewDeps {
  composePdfPreview: ComposePdfPreview;
}

/** Recompose the memory-only preview candidate for the given source-command history (§9.4). No
 *  token, no retained candidate, no write — callers re-render affected pages from the returned
 *  bytes inside their existing page slots. Thin by design: the composer decides validity; this
 *  function only shapes the request and forwards the injected call. */
export async function recomposePdfPreview(
  deps: RecomposePdfPreviewDeps,
  sourcePath: string,
  expectedSourceSha256: string,
  sourceCommands: readonly PdfEditCommand[],
): Promise<ComposedPdfPreview> {
  const request: ComposePdfPreviewRequest = {
    sourcePath,
    expectedSourceSha256,
    sourceCommands: effectiveSourceCommands(sourceCommands),
  };
  return deps.composePdfPreview(request);
}

// ---------------------------------------------------------------------------------------------
// §10.2 isolated pdf.js render-verify
// ---------------------------------------------------------------------------------------------

/** Thrown by `verifyPdfCandidate` on any renderability/content failure. Callers map this to
 *  `PdfEditError` code `"verification_failed"` (§11) rather than letting a raw pdf.js error surface,
 *  since §11 requires logs to carry "code/page/strategy but no document text" — this type carries a
 *  `page` when the failure is page-scoped and never embeds extracted document text in its message. */
export class PdfCandidateVerificationError extends Error {
  readonly page: number | null;
  constructor(message: string, page: number | null = null) {
    super(message);
    this.name = "PdfCandidateVerificationError";
    this.page = page;
  }
}

export interface VerifyPdfCandidateArgs {
  bytes: Uint8Array;
  pages: readonly number[];
  expectedText: readonly PdfExpectedTextCheck[];
}

const RENDER_TIMEOUT_MS = 20_000;

/** WKWebView's ReadableStream lacks async iteration, which makes pdf.js `getTextContent()` throw.
 * Read its public stream with a reader instead; this works in WKWebView & standards browsers. */
async function extractPageText(page: PDFPageProxy): Promise<string> {
  const reader = page.streamTextContent().getReader();
  let extracted = "";
  try {
    while (true) {
      const chunk = await withTimeout(
        reader.read(),
        RENDER_TIMEOUT_MS,
        "pdf.js text stream timed out",
      );
      if (chunk.done) break;
      extracted += chunk.value.items.map((item: { str?: string }) => item.str ?? "").join("");
    }
    return extracted;
  } finally {
    reader.releaseLock();
  }
}

/** Render every page in `pages` from a FRESH, isolated pdf.js document built from `bytes` (never the
 *  active on-screen document — see module doc), waiting for each render to fully settle, then check
 *  that every `expectedText` entry's text is present somewhere in that page's extracted text.
 *  Resolves on success; rejects with `PdfCandidateVerificationError` on any load/render/text-check
 *  failure. Always disposes the isolated document via `doc.destroy()`, success or failure, so a
 *  failed verification never leaks a pdf.js worker/document. */
export async function verifyPdfCandidate({
  bytes,
  pages,
  expectedText,
}: VerifyPdfCandidateArgs): Promise<void> {
  const data = bytes.slice();
  const loadingTask = pdfjs.getDocument(pdfDocumentInit(data, { wasmUrl: pdfWasmUrl() }));
  let doc: PDFDocumentProxy;
  try {
    doc = await withTimeout(loadingTask.promise, RENDER_TIMEOUT_MS, "pdf.js getDocument timed out");
  } catch (e) {
    throw new PdfCandidateVerificationError(
      e instanceof Error ? e.message : "candidate could not be opened",
    );
  }
  try {
    const expectedByPage = new Map<number, PdfExpectedTextCheck[]>();
    for (const check of expectedText) {
      const list = expectedByPage.get(check.page);
      if (list) list.push(check);
      else expectedByPage.set(check.page, [check]);
    }

    for (const pageNumber of pages) {
      let page: PDFPageProxy;
      try {
        page = await withTimeout(
          doc.getPage(pageNumber),
          RENDER_TIMEOUT_MS,
          `pdf.js getPage(${pageNumber}) timed out`,
        );
      } catch (e) {
        throw new PdfCandidateVerificationError(
          e instanceof Error ? e.message : "candidate page could not be opened",
          pageNumber,
        );
      }

      const viewport = page.getViewport({ scale: 1 });
      const canvas = document.createElement("canvas");
      canvas.width = Math.max(1, Math.ceil(viewport.width));
      canvas.height = Math.max(1, Math.ceil(viewport.height));
      const ctx = canvas.getContext("2d");
      if (!ctx) {
        throw new PdfCandidateVerificationError("candidate render context unavailable", pageNumber);
      }
      try {
        await withTimeout(
          page.render({ canvas, canvasContext: ctx, viewport }).promise,
          RENDER_TIMEOUT_MS,
          `pdf.js render(page ${pageNumber}) timed out`,
        );
      } catch (e) {
        throw new PdfCandidateVerificationError(
          e instanceof Error ? e.message : "candidate page failed to render",
          pageNumber,
        );
      }

      const checks = expectedByPage.get(pageNumber);
      if (checks && checks.length > 0) {
        let extracted: string;
        try {
          extracted = await extractPageText(page);
        } catch (e) {
          throw new PdfCandidateVerificationError(
            e instanceof Error ? e.message : "candidate text could not be extracted",
            pageNumber,
          );
        }
        for (const check of checks) {
          if (!extracted.includes(check.expectedText)) {
            throw new PdfCandidateVerificationError(
              `expected text missing after candidate render (island ${check.islandId})`,
              pageNumber,
            );
          }
        }
      }
    }
  } finally {
    // `PDFDocumentProxy` has no `destroy()` of its own in this pdfjs-dist version — disposal goes
    // through the `PDFDocumentLoadingTask` that produced it (same pattern as `pdfPrinting.ts`/
    // `pdfBenchTransport.ts`).
    await loadingTask.destroy();
  }
}

// ---------------------------------------------------------------------------------------------
// §10.1/§10.3 prepare / render-verify / commit save orchestration
// ---------------------------------------------------------------------------------------------

export interface PdfSaveDeps {
  preparePdfSave: PreparePdfSave;
  commitPdfSave: CommitPdfSave;
  /** Injectable so tests can substitute a fake without mocking pdf.js. Defaults to the real
   *  `verifyPdfCandidate` above. */
  verifyCandidate?: (args: VerifyPdfCandidateArgs) => Promise<void>;
}

export interface PdfSaveInput {
  sourcePath: string;
  expectedSourceSha256: string;
  annotationPdfBytes: Uint8Array;
  sourceCommands: readonly PdfEditCommand[];
}

/** Builds `PreparePdfSaveRequest` from `PdfSaveInput` — kept as a named step so both the happy path
 *  and tests can assert on the exact shape sent to `preparePdfSave` without duplicating field
 *  wiring. */
function toPrepareRequest(input: PdfSaveInput): PreparePdfSaveRequest {
  return {
    sourcePath: input.sourcePath,
    expectedSourceSha256: input.expectedSourceSha256,
    annotationPdfBytes: input.annotationPdfBytes,
    sourceCommands: effectiveSourceCommands(input.sourceCommands),
  };
}

/**
 * Runs §10.1 prepare against the injected `preparePdfSave`, then §10.2 isolated pdf.js
 * render-verify against the returned candidate. Returns a `PdfSavePlan` (§10.3's exact shape) only
 * once both steps succeed — `commit` is the only thing that talks to `commitPdfSave`, and it does so
 * lazily so a caller that decides not to commit (e.g. user cancels a Save As target picker between
 * prepare and commit) never invalidates the token by calling it.
 *
 * Token lifecycle (§10.3): the token this returns is single-use, expires after five minutes, and is
 * invalidated by any source revision in between — this module does not retry or re-prepare on a
 * stale-token rejection from `commit_pdf_save`; it surfaces the typed `PdfEditError` (expected code
 * `"write_conflict"` or `"stale_source"` per §11) to the caller, which owns the "start over" UX.
 * `didCommit` intentionally does nothing beyond what `commitPdfSave` already returned — §10.3 gives
 * the generic document saver, not this module, ownership of when recents/fingerprints/checkpoints
 * update, so this hook exists only as the seam that saver calls after using `result`.
 */
export async function prepareAndVerifyPdfSave(
  deps: PdfSaveDeps,
  input: PdfSaveInput,
): Promise<PdfSavePlan> {
  let prepared: PreparedPdfSave;
  try {
    prepared = await deps.preparePdfSave(toPrepareRequest(input));
    logEvent("pdf-save:prepare:done");
  } catch (error) {
    logEvent("pdf-save:prepare:error");
    throw error;
  }

  const verify = deps.verifyCandidate ?? verifyPdfCandidate;
  try {
    await verify({
      bytes: prepared.candidateBytes,
      pages: prepared.affectedPages,
      expectedText: prepared.expectedText,
    });
    logEvent("pdf-save:verify:done");
  } catch (error) {
    logEvent("pdf-save:verify:error");
    throw error;
  }

  let committed = false;
  return {
    async commit(targetPath?: string): Promise<PdfCommitResult> {
      if (committed) {
        throw new PdfCandidateVerificationError(
          "save token already used — prepare a new candidate before committing again",
        );
      }
      committed = true;
      // Any rejection (typed `PdfEditError` per §11, e.g. "write_conflict"/"stale_source", or
      // otherwise) is surfaced as-is — this module does not retry or reinterpret it (see doc
      // comment above); the caller owns the "start over" UX.
      return deps.commitPdfSave(prepared.token, targetPath);
    },
    async didCommit(_result: PdfCommitResult): Promise<void> {
      // Intentionally a no-op seam (see doc comment above): the generic document saver
      // (`useDocumentSaver.ts`) is the sole owner of post-commit bookkeeping.
    },
  };
}
