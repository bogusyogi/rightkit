import { useCallback, useEffect, useRef, type MutableRefObject } from "react";
import * as pdfjs from "pdfjs-dist";
import type { PDFDocumentProxy } from "pdfjs-dist";
import { pdfDocumentInit, pdfWasmUrl } from "@rightkit/pdf-ui";
import { prepareAndVerifyPdfSave } from "./pdfSave.js";
import type { PdfCommitResult, PdfEditCommand, PdfPageGraph, PdfSavePlan } from "./pdfEditModel.js";
// New PDF-edit backend commands (docs/PDF-EDITOR-NOW.md §10.1-§10.3), imported by name from
// tauriBridge.ts against the frozen type contract in pdfEditModel.ts.
import { preparePdfSave, commitPdfSave, qaReloadPdfBytes } from "../../state/tauriBridge.js";
import { logError, logEvent } from "../../state/diagnostics.js";
import type { usePdfEditSession } from "./usePdfEditSession.js";
import type { usePdfEditor } from "./usePdfEditor.js";

/**
 * Forces a fresh native stream fetch after a successful same-path commit. Tauri's stream URL can
 * remain byte-cacheable inside WKWebView even when its handler reads current disk bytes; binding
 * URL identity to committed revision prevents a second edit from inspecting stale pre-commit PDF
 * bytes. Browser QA reloads from its byte store instead and must keep its static URL unchanged.
 */
export function withPdfRevisionCacheBust(url: string, revision: string): string {
  const hashIndex = url.indexOf("#");
  const base = hashIndex >= 0 ? url.slice(0, hashIndex) : url;
  const hash = hashIndex >= 0 ? url.slice(hashIndex) : "";
  const separator = base.includes("?") ? "&" : "?";
  return `${base}${separator}vr_revision=${encodeURIComponent(revision)}${hash}`;
}

/** Invalidates published candidate preview state after disk commit, allowing any in-flight
 * recompose to retire itself by sequence before it can publish against the old source revision. */
export function invalidatePdfPreview(
  previewSeqRef: MutableRefObject<number>,
  previewLoadingTaskRef: MutableRefObject<ReturnType<typeof pdfjs.getDocument> | null>,
  previewDocRef: MutableRefObject<PDFDocumentProxy | null>,
  previewPagesRef: MutableRefObject<Set<number>>,
): void {
  previewSeqRef.current += 1;
  const stalePreview = previewLoadingTaskRef.current;
  previewLoadingTaskRef.current = null;
  previewDocRef.current = null;
  previewPagesRef.current = new Set();
  if (stalePreview && !stalePreview.destroyed) void stalePreview.destroy();
}

function sourceCommandSignature(commands: readonly PdfEditCommand[]): string {
  return JSON.stringify(commands);
}

function latestSourceCommands(
  editSession: ReturnType<typeof usePdfEditSession>,
): readonly PdfEditCommand[] {
  return editSession.sessionRef?.current.sourceCommands ?? editSession.session.sourceCommands;
}

function latestEditSignature(editSession: ReturnType<typeof usePdfEditSession>): string {
  const session = editSession.sessionRef?.current ?? editSession.session;
  return `${sourceCommandSignature(session.sourceCommands)}|draft:${JSON.stringify(session.draft)}`;
}

interface PdfSavePlanDeps {
  documentPath: string;
  url?: string;
  editSession: ReturnType<typeof usePdfEditSession>;
  pdfEditor: ReturnType<typeof usePdfEditor>;
  /** Every explicit "commit the active draft" call site routes through this single function. */
  handleCommitDraft: () => PdfEditCommand | null;
  /** Rust source graph cache (per page) — cleared on document switch and after commit. */
  pageGraphsRef: MutableRefObject<Map<number, PdfPageGraph>>;
  /** Bumped whenever pageGraphsRef is cleared; lets in-flight inspections detect the clear. */
  pageGraphsGenRef: MutableRefObject<number>;
  /** Best-known document_sha256 (§5.1), refreshed to the committed revision's sha after Save. */
  sourceShaRef: MutableRefObject<string | null>;
  visibleRef: MutableRefObject<Set<number>>;
  renderedRef: MutableRefObject<Map<number, () => void>>;
  /** Indirection to the per-page renderer so this path never grows renderPage's dep list. */
  renderPageRef: MutableRefObject<(n: number) => Promise<void>>;
  /** Shared preview sequence used by fallback invalidation before caller wires hook callback. */
  previewSeqRef?: MutableRefObject<number>;
  /** Invalidation callback from usePdfPreviewRecompose; handles queued and pending work. */
  invalidatePreview?: () => void;
  previewLoadingTaskRef: MutableRefObject<ReturnType<typeof pdfjs.getDocument> | null>;
  previewDocRef: MutableRefObject<PDFDocumentProxy | null>;
  previewPagesRef: MutableRefObject<Set<number>>;
  /** Post-commit affected pages, drained by the `[pdfEditor.ready, pdfDoc]` catch-up effect. */
  pendingReloadRerenderPagesRef: MutableRefObject<Set<number> | null>;
  /** Deferred document swap that disposes the stale task only after React commits the new pdfDoc. */
  replaceDocument: (doc: PDFDocumentProxy, task: ReturnType<typeof pdfjs.getDocument>) => void;
  /** Register this pane's full two-phase source/OCR edit save plan (§10.1-§10.3). */
  registerPdfSavePlan?: (fn: (() => Promise<PdfSavePlan>) | null) => void;
  /** Optional outer lock: blocks edit/history mutations during prepare/commit/reload. */
  setSaveLocked?: (locked: boolean) => void;
}

/**
 * §10 save orchestration — additive PdfSavePlan registration: commit the active draft, gather
 * source commands + annotation bytes, run the two-phase prepare/verify/commit, then reload the
 * document from disk truth and re-render affected pages. Extracted from PdfViewer verbatim — no
 * behavior change.
 */
export function usePdfSavePlan({
  documentPath,
  url,
  editSession,
  pdfEditor,
  handleCommitDraft,
  pageGraphsRef,
  pageGraphsGenRef,
  sourceShaRef,
  visibleRef,
  renderedRef,
  renderPageRef,
  previewSeqRef,
  invalidatePreview,
  previewLoadingTaskRef,
  previewDocRef,
  previewPagesRef,
  pendingReloadRerenderPagesRef,
  replaceDocument,
  registerPdfSavePlan,
  setSaveLocked,
}: PdfSavePlanDeps) {
  const reloadRequiredRef = useRef(false);
  const reloadPendingNewerEditsRef = useRef(false);
  const preservedUnsavedEditsRef = useRef(false);
  // biome-ignore lint/correctness/useExhaustiveDependencies: refs arrive as stable hook params
  const buildSavePlan = useCallback(async (): Promise<PdfSavePlan> => {
    if (reloadRequiredRef.current) {
      throw new Error(
        reloadPendingNewerEditsRef.current
          ? "PDF was saved while newer edits were active; reopen document before another save"
          : "PDF was saved, but document reload is required before another save",
      );
    }
    // Lock synchronously before any inspect/serialize await so initial save snapshot is stable.
    setSaveLocked?.(true);
    const initialAnnotationState = pdfEditor.annotationStateToken();
    const committed = handleCommitDraft(); // §10.1 step 1: commit active valid draft first
    const latestCommands = latestSourceCommands(editSession);
    const sourceCommands =
      committed && !latestCommands.some((command) => command.id === committed.id)
        ? [...latestCommands, committed]
        : latestCommands;
    const savedSourceSignature = sourceCommandSignature(sourceCommands);
    const savedEditSignature = latestEditSignature(editSession);
    // An annotation-only save (or a save right after Save As, before any page was ever clicked for
    // source editing) never populated sourceShaRef via handlePageClick's inspectPage call — inspect
    // page 1 now so `expectedSourceSha256` isn't sent empty, which the backend rejects as StaleSource.
    if (!sourceShaRef.current) {
      try {
        const graph = await editSession.inspectPage(1);
        pageGraphsRef.current.set(1, graph);
        sourceShaRef.current = graph.documentSha256;
      } catch {
        logError("pdf-editor:inspect");
      }
    }
    // pdf.js warns when saveDocument() is called with empty annotation storage. Native
    // source-command composition does not consume this byte channel, but Tauri still requires a
    // non-empty raw body on WKWebView's successful response path, so source-only saves send one
    // sentinel byte while real annotation saves serialize their actual PDF bytes.
    const hasAnnotationEdits = pdfEditor.dirty;
    const annotationPdfBytes = hasAnnotationEdits
      ? ((await pdfEditor.saveBytes()) ?? new Uint8Array())
      : new Uint8Array([0]);
    // pdf.js may normalize an editor while synchronously building save bytes (FreeText updates
    // its serialized hash without representing a user edit). Use that exact post-serialization
    // token as the save baseline when available. A null token means saveBytes observed a mutation
    // during/after serialization, so initialAnnotationState remains the fail-closed baseline.
    const annotationSaveState =
      (hasAnnotationEdits ? pdfEditor.serializedAnnotationStateToken() : null) ||
      initialAnnotationState;
    logEvent("pdf-save:plan-input");
    // `commit_pdf_save` never returns affected pages (tauriBridge.ts's `commitPdfSave` always
    // returns `[]` — the backend result type carries no such field). Derive them locally instead,
    // same rule tauriBridge's own preview path uses, plus every currently visible page when real
    // annotation edits were serialized (those can touch any visible page, not just source-edited
    // ones).
    const affectedPages = new Set<number>();
    for (const command of sourceCommands) {
      affectedPages.add(command.kind === "replaceSourceText" ? command.locator.page : command.page);
    }
    if (hasAnnotationEdits) {
      for (const page of visibleRef.current) affectedPages.add(page);
    }
    let plan: PdfSavePlan;
    try {
      plan = await prepareAndVerifyPdfSave(
        { preparePdfSave, commitPdfSave },
        {
          sourcePath: documentPath,
          expectedSourceSha256: sourceShaRef.current ?? "",
          annotationPdfBytes,
          sourceCommands,
        },
      );
    } catch (error) {
      setSaveLocked?.(false);
      throw error;
    }
    return {
      commit: async (targetPath?: string): Promise<PdfCommitResult> => {
        setSaveLocked?.(true);
        // Prepare may have awaited inspection/serialization while callers still held a live
        // draft. Refuse to commit a stale plan before it reaches disk; preserving newer edits is
        // safer than committing an older snapshot then forcing a re-open.
        const editsChangedBeforeCommit =
          pdfEditor.annotationStateToken() !== annotationSaveState ||
          latestEditSignature(editSession) !== savedEditSignature;
        if (editsChangedBeforeCommit) {
          setSaveLocked?.(false);
          throw new Error("PDF edits changed while save was preparing; save again");
        }
        let result: PdfCommitResult;
        try {
          result = await plan.commit(targetPath);
        } catch (error) {
          setSaveLocked?.(false);
          throw error;
        }
        const annotationChangedDuringSave =
          pdfEditor.annotationStateToken() !== annotationSaveState;
        // A successful commit is the annotation checkpoint even when PDF reload fails. The hook
        // keeps an epoch/hash captured at serialization, so edits made while prepare/commit was in
        // flight remain dirty instead of being destroyed by reload.
        const annotationsCheckpointed =
          !annotationChangedDuringSave &&
          (hasAnnotationEdits ? pdfEditor.checkpointAnnotationSave() : true);
        const newerSourceEdits =
          sourceCommandSignature(latestSourceCommands(editSession)) !== savedSourceSignature;
        // Invalidate any old in-memory candidate before loading disk truth. In-flight recomposes
        // observe the sequence change and retire their own tasks when they settle.
        if (invalidatePreview) {
          invalidatePreview();
        } else if (previewSeqRef) {
          invalidatePdfPreview(
            previewSeqRef,
            previewLoadingTaskRef,
            previewDocRef,
            previewPagesRef,
          );
        }
        await plan.didCommit(result);
        // §10.3: successful commit refreshes graph/revision. If the user changed source or
        // annotations while the async save was running, keep those edits in memory and fail closed
        // until a fresh document base is available; replacing pdfDoc here would silently destroy
        // them and leave the next annotation save serializing an obsolete base.
        pageGraphsRef.current = new Map();
        pageGraphsGenRef.current++;
        sourceShaRef.current = result.revision.documentSha256;
        if (newerSourceEdits || !annotationsCheckpointed) {
          reloadRequiredRef.current = true;
          reloadPendingNewerEditsRef.current = true;
          preservedUnsavedEditsRef.current = true;
          logError("pdf-editor:reload-required:memory-edits");
          setSaveLocked?.(false);
          return { ...result, preservedUnsavedEdits: true };
        }
        editSession.resetForDocument();
        editSession.checkpoint();
        let reloaded = false;
        if (url) {
          try {
            // The backend stream `url` is a revocable token that re-resolves to the same
            // (now-committed) file (see tauriBridge's stream-token comment) — re-fetching it gives
            // a fresh PDFDocumentProxy reflecting disk truth without needing useDocumentSaver.ts
            // (a sibling lane) to push bytes back into this component. `replaceDocument` defers
            // disposing the stale task until React has committed this fresh `pdfDoc`, so pages
            // still rendering off the old document mid-swap can't crash against a destroyed worker.
            //
            // Browser QA has no such revocable token: `url` there is a static `data:` URL captured
            // once at document-open time, so re-fetching it would always reload the ORIGINAL
            // pre-edit bytes — silently discarding every annotation just committed, and (worse)
            // breaking a second save's incremental-update prefix check against the QA disk's
            // now-updated bytes. `qaReloadPdfBytes` resolves to `null` outside QA mode, so this is a
            // Native reloads bind URL identity to committed revision below so WKWebView cannot
            // hand the next edit session cached pre-commit bytes.
            const qaBytes = await qaReloadPdfBytes(documentPath);
            // `.slice()`: pdf.js's worker transport can transfer (detach) the backing ArrayBuffer
            // of `data` — `qaBytes` is a live reference into the QA disk's stored bytes (never
            // copied on read, so later saves keep seeing the same array), so handing it over
            // directly would corrupt that entry the moment this load transfers it. Same convention
            // `handleRecomposePreview` above already uses for its own `data:` load.
            const freshTask = pdfjs.getDocument(
              qaBytes
                ? pdfDocumentInit(qaBytes.slice(), { wasmUrl: pdfWasmUrl() })
                : pdfDocumentInit(withPdfRevisionCacheBust(url, result.revision.documentSha256), {
                    wasmUrl: pdfWasmUrl(),
                  }),
            );
            const fresh = await freshTask.promise;
            // A user can mutate annotation storage while fresh bytes are loading. Check again
            // immediately before swapping pdfDoc so reload cannot erase that newer state.
            if (
              pdfEditor.annotationStateToken() !== annotationSaveState ||
              (hasAnnotationEdits && !pdfEditor.checkpointAnnotationSave())
            ) {
              if (!freshTask.destroyed) void freshTask.destroy();
              reloadRequiredRef.current = true;
              reloadPendingNewerEditsRef.current = true;
              preservedUnsavedEditsRef.current = true;
              logError("pdf-editor:reload-required:annotation-edits");
              setSaveLocked?.(false);
              return { ...result, preservedUnsavedEdits: true };
            }
            replaceDocument(fresh, freshTask);
            reloaded = true;
          } catch {
            logError("pdf-editor:reload");
          }
        }
        if (!reloaded) {
          reloadRequiredRef.current = true;
          reloadPendingNewerEditsRef.current = false;
          logError("pdf-editor:reload-required:reload-failed");
        } else {
          reloadRequiredRef.current = false;
          reloadPendingNewerEditsRef.current = false;
          preservedUnsavedEditsRef.current = false;
        }
        // Only drop the in-memory candidate preview once the fresh on-disk document is actually
        // showing — if the reload above failed, keep it so previously-edited pages keep rendering
        // from the (still-valid) candidate instead of silently reverting to stale pre-edit content.
        if (reloaded) {
          void previewLoadingTaskRef.current?.destroy();
          previewLoadingTaskRef.current = null;
          previewDocRef.current = null;
          previewPagesRef.current = new Set();
        }
        if (reloaded) {
          // Defer to the `[pdfEditor.ready, pdfDoc]` catch-up effect (below) instead of rendering
          // here: rendering synchronously, right after `replaceDocument`, races React's own
          // processing of the `pdfDoc` swap — see that effect's doc comment.
          pendingReloadRerenderPagesRef.current = new Set(affectedPages);
        } else {
          for (const n of affectedPages) {
            renderedRef.current.get(n)?.();
            void renderPageRef.current(n);
          }
        }
        setSaveLocked?.(false);
        return result;
      },
      didCommit: async () => {
        /* no-op seam — see pdfSave.ts doc comment; app-level bookkeeping owns this. */
      },
    };
  }, [documentPath, editSession, handleCommitDraft, pdfEditor, url, replaceDocument]);

  // biome-ignore lint/correctness/useExhaustiveDependencies: refs arrive as stable hook params
  const retryReload = useCallback(async (): Promise<boolean> => {
    if (!reloadRequiredRef.current || reloadPendingNewerEditsRef.current || !url) return false;
    setSaveLocked?.(true);
    try {
      const qaBytes = await qaReloadPdfBytes(documentPath);
      const freshTask = pdfjs.getDocument(
        qaBytes
          ? pdfDocumentInit(qaBytes.slice(), { wasmUrl: pdfWasmUrl() })
          : pdfDocumentInit(withPdfRevisionCacheBust(url, sourceShaRef.current ?? "retry"), {
              wasmUrl: pdfWasmUrl(),
            }),
      );
      const fresh = await freshTask.promise;
      replaceDocument(fresh, freshTask);
      reloadRequiredRef.current = false;
      reloadPendingNewerEditsRef.current = false;
      preservedUnsavedEditsRef.current = false;
      return true;
    } catch {
      logError("pdf-editor:reload-retry");
      return false;
    } finally {
      setSaveLocked?.(false);
    }
  }, [documentPath, replaceDocument, setSaveLocked, url]);

  useEffect(() => {
    if (!registerPdfSavePlan) return;
    registerPdfSavePlan(buildSavePlan);
    return () => registerPdfSavePlan(null);
  }, [registerPdfSavePlan, buildSavePlan]);

  return {
    retryReload,
    isReloadRequired: () => reloadRequiredRef.current,
    hasPreservedEdits: () => preservedUnsavedEditsRef.current,
  };
}
