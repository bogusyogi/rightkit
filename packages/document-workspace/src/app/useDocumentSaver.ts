import {
  useCallback,
  useRef,
  useState,
  type Dispatch,
  type MutableRefObject,
  type SetStateAction,
} from "react";
import {
  baseName,
  formatFromPath,
  isUntitled,
  safeFileStem,
  untitledTitle,
  type LoadedDocument,
  type ViewMode,
} from "@rightkit/document-core/documents/types";
import { isWebDocument } from "@rightkit/document-core/documents/web/webUrl";
import {
  readDocument,
  writeDocument,
  writeDocumentBytes,
  copyDocument,
  saveFileDialog,
  regrantSavedTarget,
  fileFingerprint,
  parseHostError,
  type WriteDocumentResult,
} from "@rightkit/document-core/state/tauriBridge";
import { recordRecent } from "../state/library.js";
import type { DocumentTabs } from "./useDocumentTabs.js";
import { defaultImageSaveName, editableImageOutputForPath } from "@rightkit/media-ui";
import { clearDraft, clearFileDraft } from "./documentDrafts.js";
import { errorMessage } from "@rightkit/document-core/state/errors";
import { logEvent, logPerf, type DiagnosticEvent } from "@rightkit/document-core/state/diagnostics";
import { toast } from "@rightkit/platform-ui/headless";
import { incDiskWrites } from "@rightkit/document-core/documents/perfEventCounters";
import type { ImageSaveFunction, ImageSavePayload } from "@rightkit/media-ui";
import { resolveDocumentDrafts } from "./draftResolution.js";
import type { BlockDraftRegistry } from "@rightkit/document-core/app/blockDrafts";
import type { PdfSaveFunction } from "../documents/pdf/pdfEditor.js";
import type { PdfCommitResult, PdfSavePlan } from "../documents/pdf/pdfEditModel.js";

// Save-As default name for an untitled buffer: seed the dialog with the same first-line title shown in
// the tab/titlebar (filesystem-sanitized), or "untitled.<ext>" when the buffer is empty/unusable.
function untitledSaveName(text: string, ext: string): string {
  const stem = safeFileStem(untitledTitle(text));
  return stem && stem !== "Untitled" ? `${stem}.${ext}` : `untitled.${ext}`;
}

/** Drop `path` from a dirty-flag set only when present (identity-preserving for React). */
function dropPathFromSet(setter: Dispatch<SetStateAction<Set<string>>>, path: string): void {
  setter((current) => {
    if (!current.has(path)) return current;
    const next = new Set(current);
    next.delete(path);
    return next;
  });
}

/** Re-add `path` to a dirty-flag set after a failed save — but only when it was already dirty
 *  before the attempt, so a failed save on a clean document doesn't fabricate dirty state. */
function restorePathToSet(
  setter: Dispatch<SetStateAction<Set<string>>>,
  path: string,
  wasDirty: boolean,
): void {
  if (!wasDirty) return;
  setter((current) => (current.has(path) ? current : new Set(current).add(path)));
}

/** Keep a PDF dirty when disk commit succeeded but active memory still has newer edits. */
function retainPathAsDirty(setter: Dispatch<SetStateAction<Set<string>>>, path: string): void {
  setter((current) => (current.has(path) ? current : new Set(current).add(path)));
}

/** Shared performSave success tail: perf log + success toast (unless silent), returning the
 *  saved path. */
function saveSucceeded(
  perfLabel: DiagnosticEvent,
  startedAt: number,
  savedPath: string,
  silent: boolean | undefined,
): string {
  logPerf(perfLabel, startedAt);
  if (!silent) toast(`Saved ${baseName(savedPath)}`, "success");
  return savedPath;
}

/** Shared performSave failure tail: surface the error and log it, returning null. */
function saveFailed(
  setError: Dispatch<SetStateAction<string | null>>,
  startedAt: number,
  error: unknown,
): null {
  const message = errorMessage(error);
  setError(message);
  logPerf("save:error", startedAt);
  return null;
}

function writeResult(result: WriteDocumentResult | string): {
  encoding: string;
  sourceSha256?: string;
} {
  if (typeof result === "string") return { encoding: result };
  return result;
}

/** Shared "build → commit → didCommit" sequence for the PDF save-plan path (Save and Save As). */
async function runPdfSavePlan(
  buildPlan: () => Promise<PdfSavePlan>,
  target?: string,
): Promise<PdfCommitResult> {
  const plan = await buildPlan();
  // Preserve the exact call shape: Save commits with no argument, Save As with a target.
  const result = target === undefined ? await plan.commit() : await plan.commit(target);
  await plan.didCommit(result);
  incDiskWrites(1);
  return result;
}

interface DocumentSaverDeps {
  editsRef: DocumentTabs["editsRef"];
  writeEdits: DocumentTabs["writeEdits"];
  setDocxDirty: DocumentTabs["setDocxDirty"];
  setPdfDirty: DocumentTabs["setPdfDirty"];
  pdfDirtyRef: DocumentTabs["pdfDirtyRef"];
  setOpenDocs: DocumentTabs["setOpenDocs"];
  setActivePath: DocumentTabs["setActivePath"];
  setViews: Dispatch<SetStateAction<Record<string, ViewMode>>>;
  setError: Dispatch<SetStateAction<string | null>>;
  setConflict?: Dispatch<SetStateAction<string | null>>;
  fingerprints: MutableRefObject<Map<string, { mtimeMs: number; size: number }>>;
  refreshLibrary: () => Promise<void>;
  imageSaver: MutableRefObject<ImageSaveFunction | null>;
  imageSaversRef: MutableRefObject<Map<string, ImageSaveFunction>>;
  docxSaver: MutableRefObject<(() => Promise<ArrayBuffer | null>) | null>;
  docxSaversRef: MutableRefObject<Map<string, () => Promise<ArrayBuffer | null>>>;
  pdfSaversRef: MutableRefObject<Map<string, PdfSaveFunction>>;
  pdfSavePlansRef: MutableRefObject<Map<string, () => Promise<PdfSavePlan>>>;
  clearImageDirty: (path: string) => void;
  writeImagePayload: (path: string, payload: ImageSavePayload) => Promise<void>;
  releaseReplacedBackingPath: (oldPath: string, newPath: string) => void;
  withTabKey: <T extends LoadedDocument>(doc: T, existingKey?: string) => T;
  blockDrafts?: BlockDraftRegistry;
}

/**
 * Save / Save-As across every document format: text/markdown/mdx (writeDocument), DOCX (bytes +
 * Save-As tab replacement), and images (payload write + Save-As tab replacement). Extracted verbatim
 * from useDocuments — bodies, error handling, toasts, and fingerprint/dirty bookkeeping are unchanged.
 * `markSelfSaved` + `refreshFingerprint` are owned here (this hook is the only writer of self-saved
 * echo suppression and post-write fingerprints for save operations); `refreshFingerprint` is also
 * exposed so the orchestrator's reload/reload-adjacent code can reuse the exact same fingerprint map.
 */
export function useDocumentSaver({
  editsRef,
  writeEdits,
  setDocxDirty,
  setPdfDirty,
  pdfDirtyRef,
  setOpenDocs,
  setActivePath,
  setViews,
  setError,
  setConflict,
  fingerprints,
  refreshLibrary,
  imageSaver,
  imageSaversRef,
  docxSaver,
  docxSaversRef,
  pdfSaversRef,
  pdfSavePlansRef,
  clearImageDirty,
  writeImagePayload,
  releaseReplacedBackingPath,
  withTabKey,
  blockDrafts,
}: DocumentSaverDeps) {
  const selfSaved = useRef<Set<string>>(new Set());
  const inFlightSaves = useRef<Map<string, Promise<string | null>>>(new Map());
  const pendingSaveCounts = useRef<Map<string, number>>(new Map());
  // Synchronous mirror used by the file watcher before React can render the pending-path state.
  // Update this before invoking any awaited writer, then clear it only after the operation settles.
  const pendingSavePathsRef = useRef<ReadonlySet<string>>(new Set());
  const pendingFingerprintRefreshes = useRef<Map<string, Promise<void>>>(new Map());
  const [pendingSavePaths, setPendingSavePaths] = useState<Set<string>>(() => new Set());

  const waitForFingerprint = useCallback(async (path: string) => {
    await pendingFingerprintRefreshes.current.get(path);
  }, []);

  const runSaveOnce = useCallback(
    (path: string, key: string, operation: () => Promise<string | null>) => {
      const existing = inFlightSaves.current.get(key);
      if (existing) return existing;
      pendingSaveCounts.current.set(path, (pendingSaveCounts.current.get(path) ?? 0) + 1);
      const pendingNow = new Set(pendingSavePathsRef.current);
      pendingNow.add(path);
      pendingSavePathsRef.current = pendingNow;
      setPendingSavePaths((prev) => (prev.has(path) ? prev : new Set(prev).add(path)));
      const pending = operation()
        .then(
          async (value) => {
            await waitForFingerprint(path);
            return value;
          },
          async (error) => {
            await waitForFingerprint(path);
            throw error;
          },
        )
        .finally(() => {
          if (inFlightSaves.current.get(key) === pending) inFlightSaves.current.delete(key);
          const remaining = (pendingSaveCounts.current.get(path) ?? 1) - 1;
          if (remaining > 0) return pendingSaveCounts.current.set(path, remaining);
          pendingSaveCounts.current.delete(path);
          const pendingNow = new Set(pendingSavePathsRef.current);
          pendingNow.delete(path);
          pendingSavePathsRef.current = pendingNow;
          setPendingSavePaths((prev) => {
            if (!prev.has(path)) return prev;
            const next = new Set(prev);
            next.delete(path);
            return next;
          });
        });
      inFlightSaves.current.set(key, pending);
      return pending;
    },
    [waitForFingerprint],
  );

  const markSelfSaved = useCallback((path: string) => {
    selfSaved.current.add(path);
    setTimeout(() => selfSaved.current.delete(path), 500);
  }, []);

  // Capture a file's current fingerprint after our own write so a later activation doesn't mistake
  // our save for an external change. A miss just means the next activation reloads.
  const refreshFingerprint = useCallback(
    (path: string) => {
      const refresh = fileFingerprint(path)
        .then((fp) => {
          // A miss must not leave the pre-write fingerprint behind: the watcher would read it as
          // "differs from ours" and treat our own echo as external (or, worse, trust a stale base).
          if (fp) fingerprints.current.set(path, fp);
          else fingerprints.current.delete(path);
        })
        .catch(() => {
          fingerprints.current.delete(path);
          logEvent("fingerprint:error");
        });
      pendingFingerprintRefreshes.current.set(path, refresh);
      void refresh.finally(() => {
        if (pendingFingerprintRefreshes.current.get(path) === refresh)
          pendingFingerprintRefreshes.current.delete(path);
      });
    },
    [fingerprints],
  );

  // Post-write bookkeeping for an in-place save: suppress the watcher echo and refresh the stored
  // fingerprint so a later activation doesn't mistake our write for an external change.
  const markSavedInPlace = useCallback(
    (path: string) => {
      markSelfSaved(path);
      refreshFingerprint(path);
    },
    [markSelfSaved, refreshFingerprint],
  );

  // Shared Save-As epilogue for the byte/payload writes (image, docx, pdf): re-grant the target so
  // its stream stays readable, re-read it (fresh revocable token + fingerprint), swap the tab to
  // the saved copy, clear the format's dirty flag, activate it, then update recents + library.
  // Per-format differences come in through `opts`: whether the echo suppression marks the target,
  // which dirty flag to clear, and whether to toast. `regrantPath` overrides the path re-granted
  // when the writer reports a committed path that differs from `target` (PDF save plans).
  const finishSaveAsSwap = useCallback(
    async (
      doc: LoadedDocument,
      target: string,
      opts: {
        regrantPath?: string;
        markSaved?: boolean;
        clearDirty?: () => void;
        toastSaved?: boolean;
      } = {},
    ): Promise<string | null> => {
      const readableTarget = (await regrantSavedTarget(opts.regrantPath ?? target)) ?? target;
      if (opts.markSaved) markSelfSaved(readableTarget);
      const loaded = await readDocument(readableTarget);
      if (loaded.fingerprint) fingerprints.current.set(target, loaded.fingerprint);
      releaseReplacedBackingPath(doc.path, target);
      setOpenDocs((prev) =>
        prev.map((d) => (d.path === doc.path ? withTabKey(loaded, d.tabKey) : d)),
      );
      setViews((v) => ({ ...v, [target]: v[doc.path] ?? "preview" }));
      opts.clearDirty?.();
      setActivePath(target);
      await recordRecent(loaded);
      await refreshLibrary();
      if (opts.toastSaved) toast(`Saved ${baseName(target)}`, "success");
      return target;
    },
    [
      fingerprints,
      markSelfSaved,
      refreshLibrary,
      releaseReplacedBackingPath,
      setActivePath,
      setOpenDocs,
      setViews,
      withTabKey,
    ],
  );

  const onImageSaveAs = useCallback(
    async (doc: LoadedDocument): Promise<string | null> => {
      const requested = await saveFileDialog(defaultImageSaveName(doc.path), "image");
      if (!requested) return null;
      const output = editableImageOutputForPath(requested);
      if (!output) {
        setError("Save image copies as .png, .jpg, .jpeg, or .webp.");
        return null;
      }
      const target = requested;
      const payload = await imageSaver.current?.(output);
      if (!payload) return null;
      try {
        await writeImagePayload(target, payload);
        incDiskWrites(1);
        return await finishSaveAsSwap(doc, target, {
          clearDirty: () => clearImageDirty(doc.path),
        });
      } catch (e) {
        setError(errorMessage(e));
        return null;
      }
    },
    [clearImageDirty, finishSaveAsSwap, imageSaver, setError, writeImagePayload],
  );

  const onSaveAsRef = useRef<((doc: LoadedDocument) => Promise<string | null>) | null>(null);

  // Shared PDF save skeleton for Save (in-place) and Save As (target): resolve the save plan or
  // the live bytes, write them, then run the matching epilogue. `options.target` selects it —
  // undefined = in-place (markSavedInPlace + saveSucceeded), defined = tab swap (finishSaveAsSwap).
  // Any failure restores the pre-attempt dirty flag and logs the perf-tagged error.
  const performPdfSave = useCallback(
    async (
      doc: LoadedDocument,
      startedAt: number,
      options: { target?: string; silent?: boolean } = {},
    ): Promise<string | null> => {
      const { target, silent } = options;
      // A failed save must not fabricate a dirty state: only re-add `doc.path` to `pdfDirty` on
      // failure if it was ALREADY dirty before this attempt started (there are real unsaved
      // edits to preserve). Pressing Save on an already-clean PDF that fails for unrelated
      // reasons (e.g. a disk/permission error) must not flip its tab to "unsaved".
      const wasDirty = pdfDirtyRef.current.has(doc.path);
      const succeed = async (savedPath: string, regrantPath?: string): Promise<string | null> => {
        if (target === undefined) {
          markSavedInPlace(savedPath);
          dropPathFromSet(setPdfDirty, doc.path);
          return saveSucceeded("save:pdf", startedAt, savedPath, silent);
        }
        return await finishSaveAsSwap(doc, target, {
          regrantPath,
          markSaved: true,
          clearDirty: () => dropPathFromSet(setPdfDirty, doc.path),
          toastSaved: true,
        });
      };
      const buildPlan = pdfSavePlansRef.current.get(doc.path);
      if (buildPlan) {
        try {
          const result = await runPdfSavePlan(buildPlan, target);
          if (result.preservedUnsavedEdits) {
            if (target === undefined || result.committedPath === doc.path)
              markSavedInPlace(result.committedPath);
            retainPathAsDirty(setPdfDirty, doc.path);
            logPerf("save:pdf", startedAt);
            if (!silent)
              toast(`Saved ${baseName(result.committedPath)}; newer edits remain`, "success");
            return null;
          }
          return await succeed(result.committedPath, result.committedPath);
        } catch (e) {
          restorePathToSet(setPdfDirty, doc.path, wasDirty);
          return saveFailed(setError, startedAt, e);
        }
      }
      const pdfSaver = pdfSaversRef.current.get(doc.path);
      // Native PDFKit reading deliberately does not mount pdf.js, so a clean native PDF has no
      // save plan or byte serializer. Save is still a successful no-op; only a dirty document needs
      // a serializer. Save As uses the brokered byte-for-byte copy command so signatures,
      // encryption, widgets, and other PDF features survive untouched.
      if (!pdfSaver && !wasDirty) {
        if (target === undefined) {
          return saveSucceeded("save:pdf", startedAt, doc.path, silent);
        }
        try {
          if (target === doc.path) {
            return saveSucceeded("save:pdf", startedAt, target, undefined);
          }
          await copyDocument(doc.path, target);
          incDiskWrites(1);
          return await succeed(target);
        } catch (e) {
          return saveFailed(setError, startedAt, e);
        }
      }
      let bytes: Uint8Array | null | undefined;
      try {
        bytes = await pdfSaver?.();
      } catch (e) {
        restorePathToSet(setPdfDirty, doc.path, wasDirty);
        return saveFailed(setError, startedAt, e);
      }
      if (!bytes) {
        restorePathToSet(setPdfDirty, doc.path, wasDirty);
        setError("PDF edits could not be serialized.");
        return null;
      }
      try {
        const savedPath = target ?? doc.path;
        await writeDocumentBytes(savedPath, bytes.slice().buffer);
        incDiskWrites(1);
        return await succeed(savedPath);
      } catch (e) {
        restorePathToSet(setPdfDirty, doc.path, wasDirty);
        return saveFailed(setError, startedAt, e);
      }
    },
    [
      finishSaveAsSwap,
      markSavedInPlace,
      pdfDirtyRef,
      pdfSaversRef,
      pdfSavePlansRef,
      setError,
      setPdfDirty,
    ],
  );

  const performSave = useCallback(
    async (
      doc: LoadedDocument,
      options: { silent?: boolean; resolveDrafts?: boolean } = {},
    ): Promise<string | null> => {
      const startedAt = performance.now();
      logEvent("save:start");
      if (isWebDocument(doc)) return null;
      // A manual save resolves the target document's block drafts first (Done → commit → then the
      // save writes the committed source). Autosave passes resolveDrafts:false — it must never
      // commit a draft. Failed/cancelled resolution stops the write entirely.
      if (options.resolveDrafts !== false && blockDrafts) {
        const outcome = await resolveDocumentDrafts(blockDrafts, doc.path, "manual-save", "done");
        if (outcome !== "resolved") return null;
      }
      if (doc.format === "image") {
        const output = editableImageOutputForPath(doc.path);
        if (!output) return onImageSaveAs(doc);
        // Lifecycle routes can save a background tab. Its mounted saver remains registered by path,
        // while imageSaver only mirrors the active tab.
        const payload = await (imageSaversRef.current.get(doc.path) ?? imageSaver.current)?.(
          output,
        );
        if (!payload) return null;
        try {
          await writeImagePayload(doc.path, payload);
          incDiskWrites(1);
          return saveSucceeded("save:image", startedAt, doc.path, options.silent);
        } catch (e) {
          return saveFailed(setError, startedAt, e);
        }
      }
      if (doc.format === "pdf") {
        return performPdfSave(doc, startedAt, { silent: options.silent });
      }
      const text = editsRef.current[doc.path] ?? doc.content;
      if (isUntitled(doc.path)) return onSaveAsRef.current?.(doc) ?? null;
      try {
        const result = writeResult(
          await writeDocument(doc.path, text, doc.encoding, doc.sourceSha256),
        );
        const written = result.encoding;
        incDiskWrites(1);
        // The write is awaited, and the user can keep typing while it runs (a slow or network
        // disk). Text typed in that window is newer than what was written: it must stay in the
        // edit buffer and keep its recovery draft, never be replaced by the saved text.
        const typedDuringSave = (editsRef.current[doc.path] ?? text) !== text;
        markSavedInPlace(doc.path);
        if (!typedDuringSave) {
          clearDraft(doc.path);
          clearFileDraft(doc.path);
        }
        setOpenDocs((prev) =>
          prev.map((d) =>
            d.path === doc.path
              ? {
                  ...d,
                  content: text,
                  encoding: written,
                  sourceSha256: result.sourceSha256 ?? d.sourceSha256,
                }
              : d,
          ),
        );
        if (!typedDuringSave) writeEdits((e) => ({ ...e, [doc.path]: text }));
        return saveSucceeded("save:text", startedAt, doc.path, options.silent);
      } catch (e) {
        if (parseHostError(e).code === "write_conflict") {
          setConflict?.(doc.path);
          logPerf("save:conflict", startedAt);
          return null;
        }
        return saveFailed(setError, startedAt, e);
      }
    },
    [
      writeEdits,
      editsRef,
      setOpenDocs,
      writeImagePayload,
      onImageSaveAs,
      imageSaver,
      imageSaversRef,
      markSavedInPlace,
      setError,
      setConflict,
      blockDrafts,
      performPdfSave,
    ],
  );

  const onSave = useCallback(
    (doc: LoadedDocument, options: { silent?: boolean; resolveDrafts?: boolean } = {}) =>
      runSaveOnce(doc.path, `save:${doc.path}`, () => performSave(doc, options)),
    [performSave, runSaveOnce],
  );

  // DOCX Save As: grab the live bytes from the editor, write them to a chosen path, then REPLACE the
  // active tab with the saved copy (mirrors text Save As — Word-style "you're now editing the new
  // file", not two tabs). Re-reading the saved file mints a fresh revocable stream token.
  const performDocxSaveAs = useCallback(
    async (doc: LoadedDocument, buffer?: ArrayBuffer): Promise<string | null> => {
      // Save As may be reached from Close All/quit for a background DOCX tab. Read its path-owned
      // saver instead of whichever tab is active.
      const ab = buffer ?? (await (docxSaversRef.current.get(doc.path) ?? docxSaver.current)?.());
      if (!ab) return null;
      const target = await saveFileDialog(
        isUntitled(doc.path)
          ? untitledSaveName(editsRef.current[doc.path] ?? "", "docx")
          : baseName(doc.path),
        "docx",
      );
      if (!target) return null;
      try {
        await writeDocumentBytes(target, ab);
        incDiskWrites(1);
        return await finishSaveAsSwap(doc, target, {
          markSaved: true,
          clearDirty: () => dropPathFromSet(setDocxDirty, doc.path),
        });
      } catch (e) {
        setError(errorMessage(e));
        return null;
      }
    },
    [finishSaveAsSwap, setDocxDirty, docxSaver, docxSaversRef, editsRef, setError],
  );

  const onDocxSaveAs = useCallback(
    (doc: LoadedDocument, buffer?: ArrayBuffer) =>
      runSaveOnce(doc.path, `docx-save-as:${doc.path}`, () => performDocxSaveAs(doc, buffer)),
    [performDocxSaveAs, runSaveOnce],
  );

  const performSaveAs = useCallback(
    async (doc: LoadedDocument): Promise<string | null> => {
      if (isWebDocument(doc)) return null;
      if (doc.format === "docx") return onDocxSaveAs(doc);
      if (doc.format === "image") return onImageSaveAs(doc);
      if (doc.format === "pdf") {
        const target = await saveFileDialog(baseName(doc.path), "pdf");
        if (!target) return null;
        return performPdfSave(doc, performance.now(), { target });
      }
      const text = editsRef.current[doc.path] ?? doc.content;
      const target = await saveFileDialog(
        isUntitled(doc.path)
          ? untitledSaveName(
              editsRef.current[doc.path] ?? "",
              doc.format === "mdx" ? "mdx" : doc.format,
            )
          : baseName(doc.path),
        doc.format,
      );
      if (!target) return null;
      try {
        const result = writeResult(await writeDocument(target, text, doc.encoding));
        const written = result.encoding;
        incDiskWrites(1);
        markSelfSaved(target); // suppress the watcher echo from our own write on the newly-watched target
        refreshFingerprint(target);
        releaseReplacedBackingPath(doc.path, target);
        const newDoc: LoadedDocument = {
          path: target,
          displayName: baseName(target),
          format: formatFromPath(target),
          content: text,
          encoding: written,
          sourceSha256: result.sourceSha256,
        };
        setOpenDocs((prev) =>
          prev.map((d) => (d.path === doc.path ? withTabKey(newDoc, d.tabKey) : d)),
        );
        setViews((v) => ({ ...v, [target]: v[doc.path] ?? "preview" }));
        writeEdits((e) => {
          const n = { ...e };
          delete n[doc.path];
          n[target] = text;
          return n;
        });
        clearDraft(doc.path);
        clearFileDraft(doc.path);
        setActivePath(target);
        await recordRecent(newDoc);
        await refreshLibrary();
        toast(`Saved ${baseName(target)}`, "success");
        return target;
      } catch (e) {
        setError(errorMessage(e));
        return null;
      }
    },
    [
      refreshLibrary,
      markSelfSaved,
      releaseReplacedBackingPath,
      writeEdits,
      onDocxSaveAs,
      onImageSaveAs,
      editsRef,
      setOpenDocs,
      setViews,
      setActivePath,
      refreshFingerprint,
      withTabKey,
      setError,
      performPdfSave,
    ],
  );
  const onSaveAs = useCallback(
    (doc: LoadedDocument) => runSaveOnce(doc.path, `save-as:${doc.path}`, () => performSaveAs(doc)),
    [performSaveAs, runSaveOnce],
  );
  onSaveAsRef.current = onSaveAs;

  return {
    onSave,
    onSaveAs,
    onDocxSaveAs,
    onImageSaveAs,
    markSelfSaved,
    refreshFingerprint,
    selfSaved,
    pendingSavePaths,
    pendingSavePathsRef,
  };
}
